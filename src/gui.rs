//! Menu bar front end, used when the app bundle is opened. Lists the devices a server
//! exports and attaches or detaches one when its menu item is clicked.

use std::{
    cell::{OnceCell, RefCell},
    collections::HashMap,
    mem,
    process::ExitCode,
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};

use dispatch2::DispatchQueue;
use objc2::{
    define_class, msg_send,
    rc::Retained,
    runtime::{AnyObject, ProtocolObject, Sel},
    sel, DefinedClass, MainThreadMarker, MainThreadOnly,
};
use objc2_app_kit::{
    NSAlert, NSAlertFirstButtonReturn, NSApplication, NSApplicationActivationPolicy,
    NSControlStateValueOn, NSImage, NSMenu, NSMenuDelegate, NSMenuItem, NSStatusBar, NSStatusItem,
    NSTextField, NSVariableStatusItemLength,
};
use objc2_foundation::{
    ns_string, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString, NSUserDefaults,
};
use tokio::{runtime::Runtime, sync::oneshot};
use usbip_macos::{attach::Attachment, cli, client, protocol::ExportedDevice};

const SERVER_KEY: &str = "server";
const LIST_TIMEOUT: Duration = Duration::from_secs(5);
const QUIT_TIMEOUT: Duration = Duration::from_secs(3);

enum Link {
    Attaching,
    Attached(oneshot::Sender<()>),
    Detaching,
}

#[derive(Default)]
struct Model {
    /// What the user typed, and the address it resolves to.
    server: Option<(String, String)>,
    /// `None` until the first listing finishes.
    devices: Option<Result<Vec<ExportedDevice>, String>>,
    links: HashMap<String, Link>,
    error: Option<String>,
}

type SharedModel = Arc<Mutex<Model>>;

fn lock(model: &Mutex<Model>) -> MutexGuard<'_, Model> {
    model.lock().unwrap_or_else(|e| e.into_inner())
}

thread_local! {
    static APP: OnceCell<Retained<App>> = const { OnceCell::new() };
}

/// Redraws the menu from the model. Safe to call from any thread.
fn notify() {
    DispatchQueue::main().exec_async(|| {
        APP.with(|app| {
            if let Some(app) = app.get() {
                app.rebuild();
            }
        })
    });
}

struct Ivars {
    runtime: Runtime,
    model: SharedModel,
    menu: Retained<NSMenu>,
    _status_item: Retained<NSStatusItem>,
    /// Bus ids of the device items, indexed by their tags.
    shown: RefCell<Vec<String>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = Ivars]
    struct App;

    impl App {
        #[unsafe(method(toggle:))]
        fn toggle(&self, item: &NSMenuItem) {
            let Some(busid) = self.ivars().shown.borrow().get(item.tag() as usize).cloned() else {
                return;
            };
            self.toggle_device(busid);
        }

        #[unsafe(method(chooseServer:))]
        fn choose_server(&self, _sender: Option<&AnyObject>) {
            self.ask_for_server();
        }

        #[unsafe(method(quit:))]
        fn quit(&self, _sender: Option<&AnyObject>) {
            self.detach_all();
            NSApplication::sharedApplication(self.mtm()).terminate(None);
        }
    }

    unsafe impl NSObjectProtocol for App {}

    unsafe impl NSMenuDelegate for App {
        #[unsafe(method(menuWillOpen:))]
        fn menu_will_open(&self, _menu: &NSMenu) {
            self.refresh();
        }
    }
);

impl App {
    fn new(mtm: MainThreadMarker, runtime: Runtime) -> Retained<Self> {
        let status_item =
            NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
        if let Some(button) = status_item.button(mtm) {
            match NSImage::imageWithSystemSymbolName_accessibilityDescription(
                ns_string!("cable.connector"),
                Some(ns_string!("USB/IP")),
            ) {
                Some(image) => button.setImage(Some(&image)),
                None => button.setTitle(ns_string!("USB/IP")),
            }
        }
        let menu = NSMenu::new(mtm);
        status_item.setMenu(Some(&menu));

        let model = Model {
            server: saved_server().and_then(|input| {
                let address = cli::server_address(&input)?;
                Some((input, address))
            }),
            ..Model::default()
        };
        let this = Self::alloc(mtm).set_ivars(Ivars {
            runtime,
            model: Arc::new(Mutex::new(model)),
            menu: menu.clone(),
            _status_item: status_item,
            shown: RefCell::default(),
        });
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };
        menu.setDelegate(Some(ProtocolObject::from_ref(&*this)));
        this.rebuild();
        this
    }

    fn model(&self) -> MutexGuard<'_, Model> {
        lock(&self.ivars().model)
    }

    fn item(&self, title: &str, action: Option<Sel>, key: &str) -> Retained<NSMenuItem> {
        let item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(self.mtm()),
                &NSString::from_str(title),
                action,
                &NSString::from_str(key),
            )
        };
        if action.is_some() {
            unsafe { item.setTarget(Some(self)) };
        }
        item
    }

    /// Items without an action are shown disabled, which is how status lines are drawn.
    fn rebuild(&self) {
        let menu = &self.ivars().menu;
        menu.removeAllItems();
        let model = self.model();
        let mut shown = Vec::new();

        match &model.server {
            None => menu.addItem(&self.item("No server set", None, "")),
            Some((input, _)) => {
                menu.addItem(&self.item(&format!("Server: {input}"), None, ""));
                match &model.devices {
                    None => menu.addItem(&self.item("Loading…", None, "")),
                    Some(Err(e)) => menu.addItem(&self.item(e, None, "")),
                    Some(Ok(devices)) if devices.is_empty() => {
                        menu.addItem(&self.item("No exported devices", None, ""))
                    }
                    Some(Ok(devices)) => {
                        for device in devices {
                            let title = format!(
                                "{}  {:04x}:{:04x}",
                                device.busid, device.id_vendor, device.id_product
                            );
                            self.add_device(&model, &device.busid, &title, &mut shown);
                        }
                    }
                }
            }
        }
        // Attached devices the server no longer lists still need a way to be detached.
        let listed: Vec<String> = match &model.devices {
            Some(Ok(devices)) => devices.iter().map(|d| d.busid.clone()).collect(),
            _ => Vec::new(),
        };
        let mut orphans: Vec<&String> = model
            .links
            .keys()
            .filter(|busid| !listed.contains(busid))
            .collect();
        orphans.sort();
        for busid in orphans {
            self.add_device(&model, busid, busid, &mut shown);
        }

        if let Some(error) = &model.error {
            menu.addItem(&NSMenuItem::separatorItem(self.mtm()));
            menu.addItem(&self.item(error, None, ""));
        }
        menu.addItem(&NSMenuItem::separatorItem(self.mtm()));
        menu.addItem(&self.item("Set Server…", Some(sel!(chooseServer:)), ","));
        menu.addItem(&self.item("Quit", Some(sel!(quit:)), "q"));
        drop(model);
        *self.ivars().shown.borrow_mut() = shown;
    }

    fn add_device(&self, model: &Model, busid: &str, title: &str, shown: &mut Vec<String>) {
        let item = match model.links.get(busid) {
            Some(Link::Attaching) => self.item(&format!("{title}  (attaching…)"), None, ""),
            Some(Link::Detaching) => self.item(&format!("{title}  (detaching…)"), None, ""),
            Some(Link::Attached(_)) => {
                let item = self.item(title, Some(sel!(toggle:)), "");
                item.setState(NSControlStateValueOn);
                item
            }
            None => self.item(title, Some(sel!(toggle:)), ""),
        };
        item.setTag(shown.len() as isize);
        shown.push(busid.to_string());
        self.ivars().menu.addItem(&item);
    }

    fn refresh(&self) {
        let Some((_, address)) = self.model().server.clone() else {
            return;
        };
        let model = Arc::clone(&self.ivars().model);
        self.ivars().runtime.spawn(async move {
            let listing = async {
                let mut stream = client::connect(&address).await?;
                anyhow::Ok(client::list_devices(&mut stream).await?)
            };
            let result = match tokio::time::timeout(LIST_TIMEOUT, listing).await {
                Ok(Ok(devices)) => Ok(devices),
                Ok(Err(e)) => Err(format!("Cannot list devices: {e}")),
                Err(_) => Err(format!("{address} did not answer")),
            };
            let mut model = lock(&model);
            if model.server.as_ref().is_some_and(|(_, a)| *a == address) {
                model.devices = Some(result);
            }
            drop(model);
            notify();
        });
    }

    fn toggle_device(&self, busid: String) {
        let mut model = self.model();
        model.error = None;
        match model.links.get_mut(&busid) {
            None => {
                let Some((_, address)) = model.server.clone() else {
                    return;
                };
                model.links.insert(busid.clone(), Link::Attaching);
                self.spawn_attach(address, busid);
            }
            Some(link @ Link::Attached(_)) => {
                if let Link::Attached(stop) = mem::replace(link, Link::Detaching) {
                    let _ = stop.send(());
                }
            }
            Some(Link::Attaching | Link::Detaching) => {}
        }
        drop(model);
        self.rebuild();
    }

    fn spawn_attach(&self, address: String, busid: String) {
        let model = Arc::clone(&self.ivars().model);
        self.ivars().runtime.spawn(async move {
            let attachment = match Attachment::import(&address, &busid).await {
                Ok(attachment) => attachment,
                Err(e) => {
                    finish(&model, &busid, Some(format!("{busid}: {e}")));
                    return;
                }
            };
            attachment.connect();
            let (stop, stopped) = oneshot::channel();
            lock(&model)
                .links
                .insert(busid.clone(), Link::Attached(stop));
            notify();

            let error = tokio::select! {
                _ = stopped => None,
                reason = attachment.closed() => Some(format!("{busid}: connection lost: {reason}")),
            };
            attachment.detach().await;
            finish(&model, &busid, error);
        });
    }

    /// Detaches everything before quitting, so devices leave the system cleanly.
    fn detach_all(&self) {
        for link in self.model().links.values_mut() {
            if let Link::Attached(_) = link {
                if let Link::Attached(stop) = mem::replace(link, Link::Detaching) {
                    let _ = stop.send(());
                }
            }
        }
        let model = Arc::clone(&self.ivars().model);
        self.ivars().runtime.block_on(async {
            let _ = tokio::time::timeout(QUIT_TIMEOUT, async {
                while !lock(&model).links.is_empty() {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await;
        });
    }

    fn ask_for_server(&self) {
        let mtm = self.mtm();
        let current = self
            .model()
            .server
            .as_ref()
            .map(|(input, _)| input.clone())
            .unwrap_or_default();

        let alert = NSAlert::new(mtm);
        alert.setMessageText(ns_string!("USB/IP Server"));
        alert.setInformativeText(ns_string!(
            "Host name or address of the server, optionally followed by :port."
        ));
        alert.addButtonWithTitle(ns_string!("OK"));
        alert.addButtonWithTitle(ns_string!("Cancel"));
        let field = NSTextField::textFieldWithString(&NSString::from_str(&current), mtm);
        field.setFrame(NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(260.0, 24.0),
        ));
        alert.setAccessoryView(Some(&field));
        alert.window().setInitialFirstResponder(Some(&field));

        // An accessory app has to come forward itself, or the alert opens behind other windows.
        NSApplication::sharedApplication(mtm).activate();
        if alert.runModal() != NSAlertFirstButtonReturn {
            return;
        }

        let input = field.stringValue().to_string().trim().to_string();
        let server = cli::server_address(&input).map(|address| (input.clone(), address));
        save_server(server.as_ref().map(|(input, _)| input.as_str()));
        let mut model = self.model();
        model.server = server;
        model.devices = None;
        model.error = None;
        drop(model);
        self.rebuild();
        self.refresh();
    }
}

fn finish(model: &Mutex<Model>, busid: &str, error: Option<String>) {
    let mut model = lock(model);
    model.links.remove(busid);
    if error.is_some() {
        model.error = error;
    }
    drop(model);
    notify();
}

fn saved_server() -> Option<String> {
    NSUserDefaults::standardUserDefaults()
        .stringForKey(&NSString::from_str(SERVER_KEY))
        .map(|s| s.to_string())
}

fn save_server(input: Option<&str>) {
    let defaults = NSUserDefaults::standardUserDefaults();
    let key = NSString::from_str(SERVER_KEY);
    match input {
        Some(input) => unsafe { defaults.setObject_forKey(Some(&NSString::from_str(input)), &key) },
        None => defaults.removeObjectForKey(&key),
    }
}

pub fn run() -> ExitCode {
    let Some(mtm) = MainThreadMarker::new() else {
        eprintln!("error: the menu bar app must run on the main thread");
        return ExitCode::FAILURE;
    };
    let runtime = match crate::runtime() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("error: failed to start runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    let delegate = App::new(mtm, runtime);
    APP.with(|cell| {
        let _ = cell.set(delegate.clone());
    });
    delegate.refresh();
    app.run();
    ExitCode::SUCCESS
}
