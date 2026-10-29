//! The Settings window: a Servers tab to add, rename and remove servers, a General tab, and a
//! Log tab with the app's recent log lines.

use std::{
    cell::RefCell,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::UNIX_EPOCH,
};

use dispatch2::DispatchQueue;
use objc2::{
    define_class, msg_send,
    rc::Retained,
    runtime::{AnyObject, ProtocolObject},
    sel, AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly,
};
use objc2_app_kit::{
    NSAutoresizingMaskOptions, NSBackingStoreType, NSBezelStyle, NSBorderType, NSButton, NSColor,
    NSControlStateValueOff, NSControlStateValueOn, NSControlTextEditingDelegate, NSFont,
    NSFontWeightRegular, NSLineBreakMode, NSPasteboard, NSPasteboardTypeString, NSScrollView,
    NSTabView, NSTabViewItem, NSTableCellView, NSTableColumn, NSTableView, NSTableViewDataSource,
    NSTableViewDelegate, NSTextField, NSTextView, NSUserInterfaceItemIdentification, NSView,
    NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{
    ns_string, NSDate, NSDateFormatter, NSInteger, NSObject, NSObjectProtocol, NSPoint, NSRange,
    NSRect, NSSize, NSString,
};

use super::{logger, state::Shared, system, with_app};

const WIDTH: f64 = 560.0;
const HEIGHT: f64 = 380.0;
const MARGIN: f64 = 12.0;
const SERVER_COLUMN: &str = "server";
const STATUS_COLUMN: &str = "status";

pub struct Ivars {
    shared: Arc<Shared>,
    window: Retained<NSWindow>,
    table: Retained<NSTableView>,
    login: Retained<NSButton>,
    log_scroll: Retained<NSScrollView>,
    log: Retained<NSTextView>,
    times: Retained<NSDateFormatter>,
    /// Server input and status for each table row, as of the last reload.
    rows: RefCell<Vec<(String, String)>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = Ivars]
    pub struct Settings;

    impl Settings {
        #[unsafe(method(addServer:))]
        fn add_server(&self, _sender: Option<&AnyObject>) {
            with_app(|app| app.ask_for_server());
        }

        #[unsafe(method(removeServer:))]
        fn remove_server(&self, _sender: Option<&AnyObject>) {
            let row = self.ivars().table.selectedRow();
            let input = usize::try_from(row)
                .ok()
                .and_then(|row| self.ivars().rows.borrow().get(row).map(|r| r.0.clone()));
            if let Some(input) = input {
                with_app(|app| app.remove_server(&input));
            }
        }

        #[unsafe(method(toggleLoginItem:))]
        fn toggle_login_item(&self, sender: &NSButton) {
            let enabled = sender.state() == NSControlStateValueOn;
            if let Err(e) = system::set_opens_at_login(enabled) {
                with_app(|app| app.show_alert("Open at Login didn't change", &e));
            }
            self.update_login_item();
        }

        #[unsafe(method(copyLog:))]
        fn copy_log(&self, _sender: Option<&AnyObject>) {
            let pasteboard = NSPasteboard::generalPasteboard();
            pasteboard.clearContents();
            pasteboard.setString_forType(&self.log_text(), unsafe { NSPasteboardTypeString });
        }

        #[unsafe(method(clearLog:))]
        fn clear_log(&self, _sender: Option<&AnyObject>) {
            logger::clear();
            self.refresh_log();
        }
    }

    unsafe impl NSObjectProtocol for Settings {}

    impl Settings {
        /// Sent by a server name's text field when editing it ends.
        #[unsafe(method(renameServer:))]
        fn rename_server(&self, sender: &NSTextField) {
            let row = self.ivars().table.rowForView(sender);
            let old = usize::try_from(row)
                .ok()
                .and_then(|row| self.ivars().rows.borrow().get(row).map(|r| r.0.clone()));
            let Some(old) = old else {
                return;
            };
            let new = sender.stringValue().to_string().trim().to_string();
            if new == old {
                return;
            }
            let renamed = with_app_result(|app| app.rename_server(&old, &new));
            if let Some(Err(e)) = renamed {
                sender.setStringValue(&NSString::from_str(&old));
                with_app(|app| app.show_alert(&format!("Couldn't rename {old}"), &e));
            }
        }
    }

    unsafe impl NSControlTextEditingDelegate for Settings {}

    unsafe impl NSTableViewDelegate for Settings {
        #[unsafe(method_id(tableView:viewForTableColumn:row:))]
        fn view_for_cell(
            &self,
            table: &NSTableView,
            column: Option<&NSTableColumn>,
            row: NSInteger,
        ) -> Option<Retained<NSView>> {
            let server = is_column(column, SERVER_COLUMN);
            let cell = self.cell(table, server);
            if let (Some(field), Some(text)) = (unsafe { cell.textField() }, self.cell_text(column, row)) {
                field.setStringValue(&NSString::from_str(&text));
            }
            Some(Retained::into_super(cell))
        }
    }

    unsafe impl NSTableViewDataSource for Settings {
        #[unsafe(method(numberOfRowsInTableView:))]
        fn number_of_rows(&self, _table: &NSTableView) -> NSInteger {
            self.ivars().rows.borrow().len() as NSInteger
        }
    }
);

fn is_column(column: Option<&NSTableColumn>, identifier: &str) -> bool {
    column.is_some_and(|c| c.identifier().to_string() == identifier)
}

fn with_app_result<T>(f: impl FnOnce(&super::App) -> T) -> Option<T> {
    let mut result = None;
    with_app(|app| result = Some(f(app)));
    result
}

fn button(
    mtm: MainThreadMarker,
    title: &NSString,
    target: &Settings,
    action: objc2::runtime::Sel,
) -> Retained<NSButton> {
    unsafe { NSButton::buttonWithTitle_target_action(title, Some(target), Some(action), mtm) }
}

fn place(view: &NSView, x: f64, y: f64, width: f64, height: f64, mask: NSAutoresizingMaskOptions) {
    view.setFrame(NSRect::new(NSPoint::new(x, y), NSSize::new(width, height)));
    view.setAutoresizingMask(mask);
}

/// Logging happens on any thread and can come in bursts, so redraws are coalesced into one
/// main queue block at a time.
static LOG_REFRESH_PENDING: AtomicBool = AtomicBool::new(false);

fn log_changed() {
    if LOG_REFRESH_PENDING.swap(true, Ordering::AcqRel) {
        return;
    }
    DispatchQueue::main().exec_async(|| {
        LOG_REFRESH_PENDING.store(false, Ordering::Release);
        with_app(|app| {
            if let Some(settings) = app.settings() {
                settings.refresh_log();
            }
        });
    });
}

impl Settings {
    pub fn new(mtm: MainThreadMarker, shared: Arc<Shared>) -> Retained<Self> {
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(WIDTH, HEIGHT)),
                NSWindowStyleMask::Titled
                    | NSWindowStyleMask::Closable
                    | NSWindowStyleMask::Miniaturizable
                    | NSWindowStyleMask::Resizable,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        unsafe { window.setReleasedWhenClosed(false) };
        window.setTitle(ns_string!("usbip Settings"));
        window.setContentMinSize(NSSize::new(420.0, 280.0));

        let table = NSTableView::new(mtm);
        for (identifier, title, width, editable) in [
            (SERVER_COLUMN, "Server", 200.0, true),
            (STATUS_COLUMN, "Status", 260.0, false),
        ] {
            let column = NSTableColumn::initWithIdentifier(
                NSTableColumn::alloc(mtm),
                &NSString::from_str(identifier),
            );
            column.setTitle(&NSString::from_str(title));
            column.setWidth(width);
            column.setEditable(editable);
            table.addTableColumn(&column);
        }
        table.setUsesAlternatingRowBackgroundColors(true);

        let login = unsafe {
            NSButton::checkboxWithTitle_target_action(
                ns_string!("Open at Login"),
                None,
                Some(sel!(toggleLoginItem:)),
                mtm,
            )
        };
        let log_scroll = NSTextView::scrollableTextView(mtm);
        let log = log_scroll
            .documentView()
            .and_then(|v| v.downcast::<NSTextView>().ok())
            .expect("a scrollable text view holds a text view");
        log.setEditable(false);
        let times = NSDateFormatter::new();
        times.setDateFormat(Some(ns_string!("HH:mm:ss")));

        let this = Self::alloc(mtm).set_ivars(Ivars {
            shared,
            window,
            table,
            login,
            log_scroll,
            log,
            times,
            rows: RefCell::default(),
        });
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };
        this.build(mtm);
        logger::set_listener(log_changed);
        this
    }

    fn build(&self, mtm: MainThreadMarker) {
        let ivars = self.ivars();
        let fill = NSAutoresizingMaskOptions::ViewWidthSizable
            | NSAutoresizingMaskOptions::ViewHeightSizable;
        let bottom = NSAutoresizingMaskOptions::ViewMaxYMargin;

        // The tab views are laid out in the tab view's content area, so it needs its real frame
        // before anything goes into it.
        let tabs = NSTabView::initWithFrame(
            NSTabView::alloc(mtm),
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(WIDTH, HEIGHT)),
        );
        tabs.setAutoresizingMask(fill);
        let NSSize { width, height } = tabs.contentRect().size;
        let tab_view = || {
            let view = NSView::new(mtm);
            place(&view, 0.0, 0.0, width, height, fill);
            view
        };

        let servers = tab_view();
        let table_scroll = NSScrollView::new(mtm);
        table_scroll.setHasVerticalScroller(true);
        table_scroll.setBorderType(NSBorderType::BezelBorder);
        table_scroll.setDocumentView(Some(&ivars.table));
        place(
            &table_scroll,
            MARGIN,
            42.0,
            width - 2.0 * MARGIN,
            height - 42.0 - MARGIN,
            fill,
        );
        servers.addSubview(&table_scroll);
        unsafe {
            ivars
                .table
                .setDataSource(Some(ProtocolObject::from_ref(self)));
            ivars
                .table
                .setDelegate(Some(ProtocolObject::from_ref(self)));
        }

        let add = button(mtm, ns_string!("+"), self, sel!(addServer:));
        let remove = button(mtm, ns_string!("−"), self, sel!(removeServer:));
        for (i, b) in [&add, &remove].into_iter().enumerate() {
            b.setBezelStyle(NSBezelStyle::SmallSquare);
            place(b, MARGIN + i as f64 * 26.0, 14.0, 26.0, 24.0, bottom);
            servers.addSubview(b);
        }

        let general = tab_view();
        unsafe { ivars.login.setTarget(Some(self)) };
        place(
            &ivars.login,
            MARGIN,
            height - MARGIN - 20.0,
            width - 2.0 * MARGIN,
            20.0,
            NSAutoresizingMaskOptions::ViewMinYMargin,
        );
        general.addSubview(&ivars.login);

        let log_tab = tab_view();
        ivars.log_scroll.setBorderType(NSBorderType::BezelBorder);
        place(
            &ivars.log_scroll,
            MARGIN,
            52.0,
            width - 2.0 * MARGIN,
            height - 52.0 - MARGIN,
            fill,
        );
        log_tab.addSubview(&ivars.log_scroll);
        let copy = button(mtm, ns_string!("Copy"), self, sel!(copyLog:));
        let clear = button(mtm, ns_string!("Clear"), self, sel!(clearLog:));
        for (i, b) in [&copy, &clear].into_iter().enumerate() {
            place(b, MARGIN + i as f64 * 90.0, 14.0, 84.0, 28.0, bottom);
            log_tab.addSubview(b);
        }

        for (label, view) in [
            ("Servers", &servers),
            ("General", &general),
            ("Log", &log_tab),
        ] {
            let item = unsafe {
                NSTabViewItem::initWithIdentifier(
                    NSTabViewItem::alloc(),
                    Some(&NSString::from_str(label)),
                )
            };
            item.setLabel(&NSString::from_str(label));
            item.setView(Some(view));
            tabs.addTabViewItem(&item);
        }
        ivars.window.setContentView(Some(&tabs));
    }

    pub fn show(&self) {
        let mtm = self.mtm();
        let window = &self.ivars().window;
        if !window.isVisible() {
            window.center();
        }
        super::come_forward(mtm);
        window.makeKeyAndOrderFront(None);
        self.reload();
        self.update_login_item();
        self.refresh_log();
    }

    /// A table cell whose label is centred in the row. The server column's label is editable,
    /// and editing it renames the server.
    fn cell(&self, table: &NSTableView, server: bool) -> Retained<NSTableCellView> {
        let identifier = NSString::from_str(if server { SERVER_COLUMN } else { STATUS_COLUMN });
        if let Some(cell) = unsafe { table.makeViewWithIdentifier_owner(&identifier, Some(self)) }
            .and_then(|view| view.downcast::<NSTableCellView>().ok())
        {
            return cell;
        }
        let mtm = self.mtm();
        let width = table
            .tableColumnWithIdentifier(&identifier)
            .map_or(200.0, |c| c.width());
        let row = table.rowHeight();
        let cell = NSTableCellView::new(mtm);
        cell.setIdentifier(Some(&identifier));
        cell.setFrame(NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(width, row)));

        let label = NSTextField::labelWithString(ns_string!(""), mtm);
        label.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
        if server {
            label.setEditable(true);
            unsafe {
                label.setTarget(Some(self));
                label.setAction(Some(sel!(renameServer:)));
            }
        } else {
            label.setTextColor(Some(&NSColor::secondaryLabelColor()));
        }
        let height = label.fittingSize().height;
        place(
            &label,
            2.0,
            ((row - height) / 2.0).round(),
            width - 4.0,
            height,
            NSAutoresizingMaskOptions::ViewWidthSizable
                | NSAutoresizingMaskOptions::ViewMinYMargin
                | NSAutoresizingMaskOptions::ViewMaxYMargin,
        );
        cell.addSubview(&label);
        unsafe { cell.setTextField(Some(&label)) };
        cell
    }

    /// Whether a server name is being edited, which a reload would throw away.
    fn editing(&self) -> bool {
        self.ivars()
            .window
            .firstResponder()
            .and_then(|r| r.downcast::<NSTextView>().ok())
            .is_some_and(|text| text.isFieldEditor())
    }

    fn cell_text(&self, column: Option<&NSTableColumn>, row: NSInteger) -> Option<String> {
        let rows = self.ivars().rows.borrow();
        let (input, status) = rows.get(usize::try_from(row).ok()?)?;
        Some(
            if is_column(column, SERVER_COLUMN) {
                input
            } else {
                status
            }
            .clone(),
        )
    }

    fn update_login_item(&self) {
        self.ivars().login.setState(if system::opens_at_login() {
            NSControlStateValueOn
        } else {
            NSControlStateValueOff
        });
    }

    /// Reloads the server table from the model, unless a name is being edited.
    pub fn reload(&self) {
        let ivars = self.ivars();
        if !ivars.window.isVisible() || self.editing() {
            return;
        }
        let rows = {
            let model = ivars.shared.lock();
            model
                .servers
                .iter()
                .map(|server| {
                    let attached = model
                        .links
                        .keys()
                        .filter(|k| k.server == server.input)
                        .count();
                    let status = match &server.devices {
                        None => "Checking…".to_string(),
                        Some(Err(e)) => e.clone(),
                        Some(Ok(devices)) => {
                            format!("{} exported, {attached} attached", devices.len())
                        }
                    };
                    (server.input.clone(), status)
                })
                .collect()
        };
        *ivars.rows.borrow_mut() = rows;
        ivars.table.reloadData();
    }

    fn log_text(&self) -> Retained<NSString> {
        let times = &self.ivars().times;
        let text = logger::with_recent(|entries| {
            let mut text = String::new();
            for e in entries {
                let seconds = e
                    .time
                    .duration_since(UNIX_EPOCH)
                    .map_or(0.0, |d| d.as_secs_f64());
                let time = times.stringFromDate(&NSDate::dateWithTimeIntervalSince1970(seconds));
                text += &format!("{time}  {:<5}  {}  {}\n", e.level, e.target, e.message);
            }
            text
        });
        NSString::from_str(&text)
    }

    pub fn refresh_log(&self) {
        let ivars = self.ivars();
        if !ivars.window.isVisible() {
            return;
        }
        let text = self.log_text();
        ivars.log.setString(&text);
        let font = NSFont::monospacedSystemFontOfSize_weight(11.0, unsafe { NSFontWeightRegular });
        ivars.log.setFont(Some(&font));
        ivars
            .log
            .scrollRangeToVisible(NSRange::new(text.length(), 0));
    }
}
