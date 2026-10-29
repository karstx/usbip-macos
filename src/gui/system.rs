//! The macOS services the app plugs into: login items, notifications, network changes and the
//! code signature.

use std::{ffi::c_void, ptr};

use block2::{Block, RcBlock};
use dispatch2::DispatchQueue;
use log::{debug, warn};
use objc2::{runtime::Bool, sel, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSApplication, NSMenu, NSMenuItem, NSRunningApplication};
use objc2_core_foundation::{CFBoolean, CFString};
use objc2_foundation::{ns_string, NSBundle, NSError, NSString, NSUUID};
use objc2_security::SecTask;
use objc2_service_management::{SMAppService, SMAppServiceStatus};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNMutableNotificationContent, UNNotificationRequest,
    UNUserNotificationCenter,
};

const HCI_ENTITLEMENT: &str = "com.apple.developer.usb.host-controller-interface";

extern "C" {
    fn geteuid() -> u32;
}

/// Whether this process may create virtual USB controllers: it either carries the entitlement
/// or runs as root on a system with the signature checks turned off.
pub fn can_host_controllers() -> bool {
    if unsafe { geteuid() } == 0 {
        return true;
    }
    let Some(task) = (unsafe { SecTask::from_self(None) }) else {
        return false;
    };
    let key = CFString::from_str(HCI_ENTITLEMENT);
    unsafe { task.value_for_entitlement(&key, ptr::null_mut()) }
        .is_some_and(|v| v.downcast_ref::<CFBoolean>().is_some_and(|b| b.as_bool()))
}

pub fn other_instance_running() -> bool {
    let Some(id) = NSBundle::mainBundle().bundleIdentifier() else {
        return false;
    };
    let me = NSRunningApplication::currentApplication().processIdentifier();
    NSRunningApplication::runningApplicationsWithBundleIdentifier(&id)
        .iter()
        .any(|app| app.processIdentifier() != me)
}

pub fn opens_at_login() -> bool {
    let status = unsafe { SMAppService::mainAppService().status() };
    status == SMAppServiceStatus::Enabled
}

pub fn set_opens_at_login(enabled: bool) -> Result<(), String> {
    let service = unsafe { SMAppService::mainAppService() };
    let result = unsafe {
        if enabled {
            service.registerAndReturnError()
        } else {
            service.unregisterAndReturnError()
        }
    };
    result.map_err(|e| e.localizedDescription().to_string())?;
    if unsafe { service.status() } == SMAppServiceStatus::RequiresApproval {
        unsafe { SMAppService::openSystemSettingsLoginItems() };
    }
    Ok(())
}

pub fn request_notifications() {
    let done = RcBlock::new(
        |granted: Bool, error: *mut NSError| match unsafe { error.as_ref() } {
            Some(e) => warn!("notifications unavailable: {}", e.localizedDescription()),
            None => debug!("notifications allowed: {}", granted.as_bool()),
        },
    );
    UNUserNotificationCenter::currentNotificationCenter()
        .requestAuthorizationWithOptions_completionHandler(UNAuthorizationOptions::Alert, &done);
}

/// Shows a notification. Safe to call from any thread.
pub fn post(title: String, body: String) {
    DispatchQueue::main().exec_async(move || {
        let content = UNMutableNotificationContent::new();
        content.setTitle(&NSString::from_str(&title));
        content.setBody(&NSString::from_str(&body));
        let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
            &NSUUID::new().UUIDString(),
            &content,
            None,
        );
        UNUserNotificationCenter::currentNotificationCenter()
            .addNotificationRequest_withCompletionHandler(&request, None);
    });
}

#[link(name = "Network", kind = "framework")]
extern "C" {
    fn nw_path_monitor_create() -> *mut c_void;
    fn nw_path_monitor_set_update_handler(
        monitor: *mut c_void,
        handler: &Block<dyn Fn(*mut c_void)>,
    );
    fn nw_path_monitor_set_queue(monitor: *mut c_void, queue: &DispatchQueue);
    fn nw_path_monitor_start(monitor: *mut c_void);
    fn nw_path_get_status(path: *mut c_void) -> i32;
}

const NW_PATH_STATUS_SATISFIED: i32 = 1;

/// Calls `changed` on the main queue whenever the network comes up or changes route. The
/// monitor lives as long as the app.
pub fn watch_network(changed: impl Fn() + 'static) {
    let handler = RcBlock::new(move |path: *mut c_void| {
        if unsafe { nw_path_get_status(path) } == NW_PATH_STATUS_SATISFIED {
            changed();
        }
    });
    unsafe {
        let monitor = nw_path_monitor_create();
        nw_path_monitor_set_update_handler(monitor, &handler);
        nw_path_monitor_set_queue(monitor, DispatchQueue::main());
        nw_path_monitor_start(monitor);
    }
}

fn plain_item(
    mtm: MainThreadMarker,
    title: &NSString,
    action: objc2::runtime::Sel,
    key: &NSString,
) -> objc2::rc::Retained<NSMenuItem> {
    unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            title,
            Some(action),
            key,
        )
    }
}

fn submenu(
    mtm: MainThreadMarker,
    title: &NSString,
    items: &[Option<(&NSString, objc2::runtime::Sel, &NSString)>],
) -> objc2::rc::Retained<NSMenuItem> {
    let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), title);
    for item in items {
        match item {
            Some((title, action, key)) => menu.addItem(&plain_item(mtm, title, *action, key)),
            None => menu.addItem(&NSMenuItem::separatorItem(mtm)),
        }
    }
    let item = NSMenuItem::new(mtm);
    item.setSubmenu(Some(&menu));
    item
}

/// Never shown, since the app has no menu bar of its own, but windows and dialogs take their
/// keyboard shortcuts (Copy, Paste, Close) from its key equivalents. ⌘Q closes the focused
/// window rather than quitting, since quitting would detach every device; the status menu's Quit
/// does that. The items have no target, so they go to the focused window.
pub fn install_main_menu(mtm: MainThreadMarker) {
    let main = NSMenu::new(mtm);
    // AppKit treats the first item as the app menu.
    main.addItem(&submenu(
        mtm,
        ns_string!("usbip"),
        &[Some((
            ns_string!("Close Window"),
            sel!(performClose:),
            ns_string!("q"),
        ))],
    ));
    main.addItem(&submenu(
        mtm,
        ns_string!("Edit"),
        &[
            Some((ns_string!("Undo"), sel!(undo:), ns_string!("z"))),
            Some((ns_string!("Redo"), sel!(redo:), ns_string!("Z"))),
            None,
            Some((ns_string!("Cut"), sel!(cut:), ns_string!("x"))),
            Some((ns_string!("Copy"), sel!(copy:), ns_string!("c"))),
            Some((ns_string!("Paste"), sel!(paste:), ns_string!("v"))),
            Some((ns_string!("Select All"), sel!(selectAll:), ns_string!("a"))),
        ],
    ));
    main.addItem(&submenu(
        mtm,
        ns_string!("Window"),
        &[
            Some((
                ns_string!("Minimize"),
                sel!(performMiniaturize:),
                ns_string!("m"),
            )),
            Some((ns_string!("Close"), sel!(performClose:), ns_string!("w"))),
        ],
    ));
    NSApplication::sharedApplication(mtm).setMainMenu(Some(&main));
}
