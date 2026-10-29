//! Menu bar front end, used when the app bundle is opened. Lists the devices that servers
//! export and attaches or detaches one when its menu item is clicked.

mod links;
pub mod logger;
mod prefs;
mod registry;
mod settings;
mod state;
mod system;
mod update;

use std::{
    cell::{Cell, OnceCell, RefCell},
    process::ExitCode,
    sync::Arc,
    time::Duration,
};

use block2::RcBlock;
use dispatch2::DispatchQueue;
use log::{info, warn};
use objc2::{
    define_class, msg_send,
    rc::Retained,
    runtime::{AnyObject, ProtocolObject, Sel},
    sel, AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly,
};
use objc2_app_kit::{
    NSAlert, NSAlertFirstButtonReturn, NSApplication, NSApplicationActivationPolicy,
    NSApplicationDelegate, NSApplicationTerminateReply, NSControlStateValueMixed,
    NSControlStateValueOn, NSImage, NSMenu, NSMenuDelegate, NSMenuItem, NSStatusBar, NSStatusItem,
    NSTextField, NSVariableStatusItemLength, NSWorkspace, NSWorkspaceDidWakeNotification,
    NSWorkspaceWillSleepNotification,
};
use objc2_foundation::{
    ns_string, NSData, NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect, NSRunLoop,
    NSSize, NSString, NSURL,
};
use tokio::{
    runtime::Runtime,
    signal::unix::{signal, SignalKind},
};
use usbip_macos::{
    app::{DeviceKey, Kind},
    client,
};

use settings::Settings;
use state::{Model, Server, Shared, State};

const LIST_TIMEOUT: Duration = Duration::from_secs(5);
const QUIT_TIMEOUT: Duration = Duration::from_secs(5);
const SLEEP_TIMEOUT: Duration = Duration::from_secs(5);
const UPDATE_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
const MESSAGE_WIDTH: usize = 60;

fn kind_symbol(kind: Kind) -> &'static str {
    match kind {
        Kind::Keyboard => "keyboard",
        Kind::Mouse => "computermouse",
        Kind::Input => "hand.point.up.left",
        Kind::Audio => "hifispeaker",
        Kind::Serial => "cable.connector.horizontal",
        Kind::Network => "network",
        Kind::Communications | Kind::Wireless => "antenna.radiowaves.left.and.right",
        Kind::Printer => "printer",
        Kind::Storage => "externaldrive",
        Kind::Hub => "point.3.filled.connected.trianglepath.dotted",
        Kind::SmartCard => "creditcard",
        Kind::Camera => "web.camera",
        Kind::Bluetooth => "wave.3.right",
        Kind::Firmware => "arrow.down.circle",
        Kind::VendorSpecific => "cpu",
        Kind::Other => "cable.connector",
    }
}

fn save_servers(model: &Model) {
    let inputs: Vec<String> = model.servers.iter().map(|s| s.input.clone()).collect();
    prefs::save_servers(&inputs);
}

thread_local! {
    static APP: OnceCell<Retained<App>> = const { OnceCell::new() };
}

fn with_app(f: impl FnOnce(&App)) {
    APP.with(|app| {
        if let Some(app) = app.get() {
            f(app);
        }
    })
}

/// Redraws the menu from the model. Safe to call from any thread.
fn redraw() {
    DispatchQueue::main().exec_async(|| with_app(App::rebuild));
}

/// An accessory app has to bring itself forward before showing a window, or the window opens
/// behind other apps without keyboard focus.
fn come_forward(mtm: MainThreadMarker) {
    NSApplication::sharedApplication(mtm).activate();
}

/// Runs `f` on the main thread from the run loop rather than from a main queue block. Anything
/// that spins a nested run loop (a modal alert, terminating with a TerminateLater reply) needs
/// this: the main queue stays blocked while it runs, so redraws and replies posted to it
/// would never arrive.
fn outside_main_queue(f: impl FnOnce() + Send + 'static) {
    DispatchQueue::main().exec_async(move || {
        let f = Cell::new(Some(f));
        let block = RcBlock::new(move || {
            if let Some(f) = f.take() {
                f();
            }
        });
        unsafe { NSRunLoop::mainRunLoop().performBlock(&block) };
    });
}

struct Ivars {
    runtime: Runtime,
    shared: Arc<Shared>,
    menu: Retained<NSMenu>,
    status_item: Retained<NSStatusItem>,
    /// The device each clicked item toggles, indexed by its tag.
    devices: RefCell<Vec<DeviceKey>>,
    settings: OnceCell<Retained<Settings>>,
    /// Status item images: normal, and crossed out for a device that is reconnecting.
    icons: (Option<Retained<NSImage>>, Option<Retained<NSImage>>),
}

const STATUS_ICON: &[u8] = include_bytes!("../../packaging/StatusIcon.svg");
const STATUS_ICON_DISCONNECTED: &[u8] =
    include_bytes!("../../packaging/StatusIconDisconnected.svg");
/// A little under the menu bar's height, like the system's own status items.
const STATUS_ICON_HEIGHT: f64 = 17.0;

/// The app icon's glyph as a template image, so the menu bar tints it for light and dark mode.
/// NSImage reads SVG since macOS 14; anything older falls back to an SF Symbol.
fn status_icon(svg: &[u8], fallback: &NSString) -> Option<Retained<NSImage>> {
    let image = NSImage::initWithData(NSImage::alloc(), &NSData::with_bytes(svg))
        .filter(|image| image.size().height > 0.0);
    let Some(image) = image else {
        warn!("can't read the status icon, using {fallback}");
        return NSImage::imageWithSystemSymbolName_accessibilityDescription(
            fallback,
            Some(ns_string!("usbip")),
        );
    };
    let size = image.size();
    image.setSize(NSSize::new(
        size.width * STATUS_ICON_HEIGHT / size.height,
        STATUS_ICON_HEIGHT,
    ));
    image.setTemplate(true);
    image.setAccessibilityDescription(Some(ns_string!("usbip")));
    Some(image)
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = Ivars]
    struct App;

    impl App {
        #[unsafe(method(toggleDevice:))]
        fn toggle_device(&self, item: &NSMenuItem) {
            let key = self.ivars().devices.borrow().get(item.tag() as usize).cloned();
            if let Some(key) = key {
                self.toggle(key);
            }
        }

        #[unsafe(method(addServer:))]
        fn add_server_action(&self, _sender: Option<&AnyObject>) {
            self.ask_for_server();
        }

        #[unsafe(method(showSettings:))]
        fn show_settings(&self, _sender: Option<&AnyObject>) {
            let shared = Arc::clone(self.shared());
            self.ivars()
                .settings
                .get_or_init(|| Settings::new(self.mtm(), shared))
                .show();
        }

        /// Lists every server again and cuts short any reconnection that is waiting to retry.
        #[unsafe(method(tryAgain:))]
        fn try_again(&self, _sender: Option<&AnyObject>) {
            info!("trying again");
            self.shared().retry_now.send_modify(|n| *n += 1);
            self.refresh();
        }

        #[unsafe(method(openUpdate:))]
        fn open_update(&self, _sender: Option<&AnyObject>) {
            let url = self.ivars().shared.lock().update.as_ref().map(|(_, url)| url.clone());
            if let Some(url) = url.and_then(|u| NSURL::URLWithString(&NSString::from_str(&u))) {
                NSWorkspace::sharedWorkspace().openURL(&url);
            }
        }

        #[unsafe(method(about:))]
        fn about(&self, _sender: Option<&AnyObject>) {
            come_forward(self.mtm());
            NSApplication::sharedApplication(self.mtm()).orderFrontStandardAboutPanel(None);
        }

        #[unsafe(method(quit:))]
        fn quit(&self, _sender: Option<&AnyObject>) {
            NSApplication::sharedApplication(self.mtm()).terminate(None);
        }

        #[unsafe(method(willSleep:))]
        fn will_sleep(&self, _notification: &NSNotification) {
            self.sleep();
        }

        #[unsafe(method(didWake:))]
        fn did_wake(&self, _notification: &NSNotification) {
            info!("woke up");
            self.ivars().shared.awake.send_replace(true);
            self.refresh();
        }
    }

    unsafe impl NSObjectProtocol for App {}

    unsafe impl NSApplicationDelegate for App {
        #[unsafe(method(applicationShouldTerminate:))]
        fn should_terminate(&self, _sender: &NSApplication) -> NSApplicationTerminateReply {
            self.detach_all_and_quit()
        }
    }

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
        let menu = NSMenu::new(mtm);
        // Automatic enabling leaves items without an action enabled, so `item` decides instead.
        menu.setAutoenablesItems(false);
        status_item.setMenu(Some(&menu));

        let mut servers = prefs::servers();
        servers.dedup();
        let model = Model {
            servers: servers.iter().filter_map(|s| Server::new(s)).collect(),
            wanted: prefs::attached(),
            names: prefs::names(),
            can_host: system::can_host_controllers(),
            ..Model::default()
        };
        let this = Self::alloc(mtm).set_ivars(Ivars {
            runtime,
            shared: Shared::new(model, redraw),
            menu: menu.clone(),
            status_item,
            devices: RefCell::default(),
            settings: OnceCell::new(),
            icons: (
                status_icon(STATUS_ICON, ns_string!("cable.connector")),
                status_icon(
                    STATUS_ICON_DISCONNECTED,
                    ns_string!("cable.connector.slash"),
                ),
            ),
        });
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };
        menu.setDelegate(Some(ProtocolObject::from_ref(&*this)));
        this.rebuild();
        this
    }

    fn shared(&self) -> &Arc<Shared> {
        &self.ivars().shared
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
        item.setEnabled(action.is_some());
        item
    }

    fn settings(&self) -> Option<&Settings> {
        self.ivars().settings.get().map(|s| &**s)
    }

    /// An item that toggles `key`, found again through its tag when clicked.
    fn toggle_item(&self, title: &str, key: &DeviceKey) -> Retained<NSMenuItem> {
        let item = self.item(title, Some(sel!(toggleDevice:)), "");
        let mut devices = self.ivars().devices.borrow_mut();
        item.setTag(devices.len() as isize);
        devices.push(key.clone());
        item
    }

    /// Items without an action are drawn disabled, which is how status lines are shown.
    fn status_line(&self, text: &str) -> Retained<NSMenuItem> {
        let item = if text.chars().count() > MESSAGE_WIDTH {
            let short: String = text.chars().take(MESSAGE_WIDTH - 1).collect();
            let item = self.item(&format!("{short}…"), None, "");
            item.setToolTip(Some(&NSString::from_str(text)));
            item
        } else {
            self.item(text, None, "")
        };
        item
    }

    fn rebuild(&self) {
        let mtm = self.mtm();
        let menu = &self.ivars().menu;
        menu.removeAllItems();
        self.ivars().devices.borrow_mut().clear();
        let model = self.shared().lock();

        let mut notices = false;
        if !model.can_host {
            let item = self.status_line("Attaching devices needs root");
            item.setToolTip(Some(ns_string!(
                "This copy of usbip isn't signed with the USB host controller entitlement, so \
                 macOS only lets it attach devices when it runs as root."
            )));
            menu.addItem(&item);
            notices = true;
        }
        if let Some((version, _)) = &model.update {
            menu.addItem(&self.item(
                &format!("Update Available: {version}…"),
                Some(sel!(openUpdate:)),
                "",
            ));
            notices = true;
        }
        if notices {
            menu.addItem(&NSMenuItem::separatorItem(mtm));
        }

        if model.servers.is_empty() {
            menu.addItem(&self.status_line("No servers"));
            menu.addItem(&self.item("Add Server…", Some(sel!(addServer:)), ""));
        }
        for server in &model.servers {
            menu.addItem(&NSMenuItem::sectionHeaderWithTitle(
                &NSString::from_str(&server.input),
                mtm,
            ));
            let mut busids: Vec<&str> = Vec::new();
            match &server.devices {
                None => menu.addItem(&self.status_line("Loading…")),
                Some(Err(e)) => menu.addItem(&self.status_line(e)),
                Some(Ok(devices)) if devices.is_empty() => {
                    menu.addItem(&self.status_line("No exported devices"))
                }
                Some(Ok(devices)) => busids.extend(devices.iter().map(|d| d.busid.as_str())),
            }
            // Attached devices the server no longer lists still need a way to be detached.
            for key in model.links.keys().filter(|k| k.server == server.input) {
                if !busids.contains(&key.busid.as_str()) {
                    busids.push(&key.busid);
                }
            }
            for busid in busids {
                menu.addItem(&self.device_item(&model, &DeviceKey::new(&server.input, busid)));
            }
            let reconnecting = model.links.iter().any(|(k, l)| {
                k.server == server.input && matches!(l.state, State::Reconnecting(_))
            });
            if matches!(server.devices, Some(Err(_))) || reconnecting {
                menu.addItem(&self.item("Try Again", Some(sel!(tryAgain:)), ""));
            }
        }

        menu.addItem(&NSMenuItem::separatorItem(mtm));
        menu.addItem(&self.item("Settings…", Some(sel!(showSettings:)), ","));
        menu.addItem(&self.item("About usbip", Some(sel!(about:)), ""));
        menu.addItem(&self.item("Quit usbip", Some(sel!(quit:)), "q"));

        self.update_icon(&model);
        drop(model);
        if let Some(settings) = self.settings() {
            settings.reload();
        }
    }

    /// Titled with the product name learned from an earlier attach, with the vendor and product
    /// ids below it. USB/IP carries no names, so a device never attached is titled with its ids,
    /// with its class below.
    fn device_item(&self, model: &Model, key: &DeviceKey) -> Retained<NSMenuItem> {
        let device = model.device(key);
        let ids = device.map(|d| format!("{:04x}:{:04x}", d.id_vendor, d.id_product));
        let name = device.and_then(|d| model.names.get(&(d.id_vendor, d.id_product)));
        let kind = device.map_or(Kind::Other, Kind::of);
        let (title, detail) = match (name, ids) {
            (Some(name), Some(ids)) => (name.clone(), Some(ids)),
            (None, Some(ids)) => (ids, Some(kind.label().to_string())),
            _ => (key.busid.clone(), None),
        };
        let link = model.links.get(key);
        let item = match link.map(|l| &l.state) {
            Some(State::Ejecting | State::Detaching) => self.item(&title, None, ""),
            _ => self.toggle_item(&title, key),
        };
        if let Some(image) = NSImage::imageWithSystemSymbolName_accessibilityDescription(
            &NSString::from_str(kind_symbol(kind)),
            Some(&NSString::from_str(kind.label())),
        ) {
            item.setImage(Some(&image));
        }
        let mut subtitle = match detail {
            Some(detail) => format!("{detail} · {}", key.busid),
            None => key.busid.clone(),
        };
        let state = match link.map(|l| &l.state) {
            None => None,
            Some(State::Attached) => {
                item.setState(NSControlStateValueOn);
                None
            }
            Some(State::Attaching) => Some("Attaching…".to_string()),
            Some(State::Reconnecting(e)) => {
                item.setState(NSControlStateValueMixed);
                Some(format!("Reconnecting: {e}"))
            }
            Some(State::Asleep) => {
                item.setState(NSControlStateValueMixed);
                Some("Reattaches after wake".to_string())
            }
            Some(State::Ejecting) => Some("Ejecting…".to_string()),
            Some(State::Detaching) => Some("Detaching…".to_string()),
        };
        if let Some(state) = state {
            item.setToolTip(Some(&NSString::from_str(&state)));
            subtitle += &format!("\n{state}");
        }
        item.setSubtitle(Some(&NSString::from_str(&subtitle)));
        item
    }

    /// Dimmed with nothing attached, crossed out while a device is reconnecting.
    fn update_icon(&self, model: &Model) {
        let Some(button) = self.ivars().status_item.button(self.mtm()) else {
            return;
        };
        let states = || model.links.values().map(|l| &l.state);
        let reconnecting = states().any(|s| matches!(s, State::Reconnecting(_)));
        let attached = states().filter(|s| **s == State::Attached).count();
        let (connected, disconnected) = &self.ivars().icons;
        match if reconnecting {
            disconnected
        } else {
            connected
        } {
            Some(image) => button.setImage(Some(image)),
            None => button.setTitle(ns_string!("USB/IP")),
        }
        button.setAppearsDisabled(model.links.is_empty());
        let tooltip = match attached {
            0 => "usbip".to_string(),
            1 => "usbip: 1 device attached".to_string(),
            n => format!("usbip: {n} devices attached"),
        };
        button.setToolTip(Some(&NSString::from_str(&tooltip)));
    }

    fn refresh(&self) {
        let servers: Vec<(String, String)> = self
            .shared()
            .lock()
            .servers
            .iter()
            .map(|s| (s.input.clone(), s.address.clone()))
            .collect();
        for (input, address) in servers {
            let shared = Arc::clone(self.shared());
            self.ivars().runtime.spawn(async move {
                let listing = async {
                    let mut stream = client::connect(&address).await?;
                    anyhow::Ok(client::list_devices(&mut stream).await?)
                };
                let result = match tokio::time::timeout(LIST_TIMEOUT, listing).await {
                    Ok(Ok(devices)) => Ok(devices),
                    Ok(Err(e)) => Err(format!("Cannot list devices: {e}")),
                    Err(_) => Err(format!("{input} did not answer")),
                };
                shared.update(|m| {
                    if let Some(server) = m.server_mut(&input) {
                        server.devices = Some(result);
                    }
                });
            });
        }
    }

    /// Attaches the devices that were attached when the app last quit.
    fn restore(&self) {
        let model = self.shared().lock();
        let keys: Vec<(DeviceKey, String)> = model
            .wanted
            .iter()
            .filter_map(|k| Some((k.clone(), model.server(&k.server)?.address.clone())))
            .collect();
        drop(model);
        for (key, address) in keys {
            links::start(self.shared(), &self.ivars().runtime, key, address, false);
        }
    }

    fn toggle(&self, key: DeviceKey) {
        let shared = self.shared();
        let mut model = shared.lock();
        let Some(link) = model.links.get(&key) else {
            let Some(address) = model.server(&key.server).map(|s| s.address.clone()) else {
                return;
            };
            drop(model);
            links::start(shared, &self.ivars().runtime, key, address, true);
            return;
        };
        match &link.state {
            State::Attached => {
                drop(model);
                self.detach(key);
            }
            State::Attaching | State::Reconnecting(_) | State::Asleep => {
                link.stop.send_replace(true);
                model.forget(&key);
                drop(model);
                (shared.redraw)();
            }
            State::Ejecting | State::Detaching => {}
        }
    }

    /// Detaches an attached device for good, ejecting its disks first. If they can't be
    /// ejected the device stays attached and the user is told why.
    fn detach(&self, key: DeviceKey) {
        let shared = Arc::clone(self.shared());
        let uuid = {
            let mut model = shared.lock();
            let Some(link) = model.links.get_mut(&key) else {
                return;
            };
            match &link.controller {
                Some(uuid) => {
                    link.state = State::Ejecting;
                    uuid.clone()
                }
                None => {
                    link.stop.send_replace(true);
                    model.forget(&key);
                    drop(model);
                    (shared.redraw)();
                    return;
                }
            }
        };
        (shared.redraw)();
        self.ivars().runtime.spawn(async move {
            let result = links::eject(uuid, links::EJECT_TIMEOUT).await;
            let failure = shared.update(|m| {
                let label = m.label(&key);
                let link = m.links.get_mut(&key)?;
                match result {
                    Ok(()) => {
                        link.stop.send_replace(true);
                        m.forget(&key);
                        None
                    }
                    Err(e) => {
                        if link.state == State::Ejecting {
                            link.state = State::Attached;
                        }
                        Some((label, e))
                    }
                }
            });
            if let Some((label, e)) = failure {
                outside_main_queue(move || {
                    with_app(|app| {
                        app.show_alert(
                            &format!("{label} is still in use"),
                            &format!("{e}\n\nQuit the apps using it and try again."),
                        )
                    })
                });
            }
        });
    }

    fn show_alert(&self, message: &str, detail: &str) {
        let alert = NSAlert::new(self.mtm());
        alert.setMessageText(&NSString::from_str(message));
        alert.setInformativeText(&NSString::from_str(detail));
        come_forward(self.mtm());
        alert.runModal();
    }

    fn remove_server(&self, input: &str) {
        let keys: Vec<DeviceKey> = {
            let model = self.shared().lock();
            model
                .links
                .keys()
                .chain(model.wanted.iter())
                .filter(|k| k.server == input)
                .cloned()
                .collect()
        };
        for key in keys {
            let attached = self
                .shared()
                .lock()
                .links
                .get(&key)
                .is_some_and(|l| l.state == State::Attached);
            if attached {
                self.detach(key);
            } else {
                self.shared().update(|m| {
                    if let Some(link) = m.links.get(&key) {
                        link.stop.send_replace(true);
                    }
                    m.forget(&key);
                });
            }
        }
        self.shared().update(|m| {
            m.servers.retain(|s| s.input != input);
            save_servers(m);
        });
        info!("removed server {input}");
    }

    fn ask_for_server(&self) {
        let mtm = self.mtm();
        let alert = NSAlert::new(mtm);
        alert.setMessageText(ns_string!("Add USB/IP Server"));
        alert.setInformativeText(ns_string!(
            "Host name or address of the server, optionally followed by :port."
        ));
        alert.addButtonWithTitle(ns_string!("Add"));
        alert.addButtonWithTitle(ns_string!("Cancel"));
        let field = NSTextField::textFieldWithString(ns_string!(""), mtm);
        field.setFrame(NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(260.0, 24.0),
        ));
        field.setPlaceholderString(Some(ns_string!("raspberrypi.local")));
        alert.setAccessoryView(Some(&field));
        alert.window().setInitialFirstResponder(Some(&field));

        come_forward(mtm);
        if alert.runModal() != NSAlertFirstButtonReturn {
            return;
        }

        let input = field.stringValue().to_string().trim().to_string();
        if input.is_empty() {
            return;
        }
        if let Err(e) = self.add_server(&input) {
            self.show_alert(&format!("Couldn't add {input}"), &e);
        }
    }

    fn add_server(&self, input: &str) -> Result<(), String> {
        let server = Server::new(input).ok_or("That isn't a server address.")?;
        self.shared().update(|m| {
            if m.server(input).is_some() {
                return Err("It is already in the list.".to_string());
            }
            m.servers.push(server);
            save_servers(m);
            Ok(())
        })?;
        info!("added server {input}");
        self.refresh();
        Ok(())
    }

    /// Renames a server in place. Refused while any of its devices are attached, since they
    /// would have to move to the new address.
    fn rename_server(&self, old: &str, new: &str) -> Result<(), String> {
        let server = Server::new(new).ok_or("That isn't a server address.")?;
        self.shared().update(|m| {
            if m.server(new).is_some() {
                return Err(format!("{new} is already in the list."));
            }
            if m.links.keys().any(|k| k.server == old) {
                return Err("Detach its devices first.".to_string());
            }
            let Some(slot) = m.servers.iter_mut().find(|s| s.input == old) else {
                return Ok(());
            };
            *slot = server;
            let renamed: Vec<DeviceKey> = m
                .wanted
                .iter()
                .filter(|k| k.server == old)
                .cloned()
                .collect();
            for key in renamed {
                m.forget(&key);
                m.want(&DeviceKey::new(new, &key.busid));
            }
            save_servers(m);
            Ok(())
        })?;
        info!("renamed server {old} to {new}");
        self.refresh();
        Ok(())
    }

    /// Detaches everything before the system sleeps; the links attach again on wake. Blocks
    /// the notification for a few seconds so the devices leave before the network goes down.
    fn sleep(&self) {
        info!("going to sleep");
        let shared = Arc::clone(self.shared());
        shared.awake.send_replace(false);
        self.ivars().runtime.block_on(async {
            let _ = tokio::time::timeout(SLEEP_TIMEOUT, async {
                while shared
                    .lock()
                    .links
                    .values()
                    .any(|l| !matches!(l.state, State::Asleep | State::Reconnecting(_)))
                {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await;
        });
    }

    /// Ejects disks and detaches every device before quitting, so devices leave the system
    /// cleanly. They are still remembered and attach again on the next launch.
    fn detach_all_and_quit(&self) -> NSApplicationTerminateReply {
        let shared = Arc::clone(self.shared());
        let controllers: Vec<String> = {
            let model = shared.lock();
            if model.links.is_empty() {
                return NSApplicationTerminateReply::TerminateNow;
            }
            model
                .links
                .values()
                .filter_map(|l| l.controller.clone())
                .collect()
        };
        info!("detaching everything before quitting");
        self.ivars().runtime.spawn(async move {
            for uuid in controllers {
                if let Err(e) = links::eject(uuid, links::EJECT_BEFORE_QUIT).await {
                    warn!("ejecting before quitting: {e}");
                }
            }
            for link in shared.lock().links.values() {
                link.stop.send_replace(true);
            }
            let _ = tokio::time::timeout(QUIT_TIMEOUT, async {
                while !shared.lock().links.is_empty() {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await;
            DispatchQueue::main().exec_async(|| {
                let mtm = MainThreadMarker::new().expect("main queue");
                NSApplication::sharedApplication(mtm).replyToApplicationShouldTerminate(true);
            });
        });
        NSApplicationTerminateReply::TerminateLater
    }

    fn observe_system(&self) {
        let center = NSWorkspace::sharedWorkspace().notificationCenter();
        unsafe {
            center.addObserver_selector_name_object(
                self,
                sel!(willSleep:),
                Some(NSWorkspaceWillSleepNotification),
                None,
            );
            center.addObserver_selector_name_object(
                self,
                sel!(didWake:),
                Some(NSWorkspaceDidWakeNotification),
                None,
            );
        }
        let shared = Arc::clone(self.shared());
        system::watch_network(move || {
            shared.retry_now.send_modify(|n| *n += 1);
        });

        // `kill` and launchd stop the app with a signal; quit through AppKit so devices detach.
        self.ivars().runtime.spawn(async {
            let (Ok(mut term), Ok(mut int)) = (
                signal(SignalKind::terminate()),
                signal(SignalKind::interrupt()),
            ) else {
                return;
            };
            tokio::select! {
                _ = term.recv() => {}
                _ = int.recv() => {}
            }
            outside_main_queue(|| {
                let mtm = MainThreadMarker::new().expect("main thread");
                NSApplication::sharedApplication(mtm).terminate(None);
            });
        });
    }

    fn check_for_updates(&self) {
        self.ivars().runtime.spawn(async {
            let mut interval = tokio::time::interval(UPDATE_INTERVAL);
            loop {
                interval.tick().await;
                DispatchQueue::main().exec_async(|| {
                    update::check(|release| {
                        with_app(|app| {
                            app.shared()
                                .update(|m| m.update = Some((release.version, release.url)))
                        })
                    })
                });
            }
        });
    }
}

pub fn run() -> ExitCode {
    let Some(mtm) = MainThreadMarker::new() else {
        eprintln!("error: the menu bar app must run on the main thread");
        return ExitCode::FAILURE;
    };
    if system::other_instance_running() {
        info!("already running");
        return ExitCode::SUCCESS;
    }
    let runtime = match crate::runtime() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("error: failed to start runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    system::install_main_menu(mtm);
    let delegate = App::new(mtm, runtime);
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    APP.with(|cell| {
        let _ = cell.set(delegate.clone());
    });
    delegate.observe_system();
    system::request_notifications();
    delegate.restore();
    delegate.refresh();
    delegate.check_for_updates();
    app.run();
    ExitCode::SUCCESS
}
