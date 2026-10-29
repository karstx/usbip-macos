//! Finds what macOS built on top of one of our virtual controllers: the attached device's
//! product name, and the disks of a storage device so they can be ejected before detaching.

use std::{
    ffi::{c_char, c_void, CStr, CString},
    ptr::{self, NonNull},
    sync::mpsc,
    time::Instant,
};

use dispatch2::DispatchQueue;
use log::info;
use objc2_core_foundation::{CFBoolean, CFRetained, CFString, CFType};
use objc2_disk_arbitration::{
    kDADiskEjectOptionDefault, kDADiskUnmountOptionWhole, DADisk, DADissenter, DASession,
};

type IoObject = u32;

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    fn IOServiceMatching(name: *const c_char) -> *mut c_void;
    fn IOServiceGetMatchingServices(
        main_port: u32,
        matching: *mut c_void,
        iterator: *mut IoObject,
    ) -> i32;
    fn IOIteratorNext(iterator: IoObject) -> IoObject;
    fn IOObjectRelease(object: IoObject) -> i32;
    fn IOObjectConformsTo(object: IoObject, class_name: *const c_char) -> u32;
    fn IORegistryEntryCreateIterator(
        entry: IoObject,
        plane: *const c_char,
        options: u32,
        iterator: *mut IoObject,
    ) -> i32;
    fn IORegistryEntryCreateCFProperty(
        entry: IoObject,
        key: &CFString,
        allocator: *const c_void,
        options: u32,
    ) -> Option<NonNull<CFType>>;
}

const ITERATE_RECURSIVELY: u32 = 1;

struct Object(IoObject);

impl Drop for Object {
    fn drop(&mut self) {
        unsafe { IOObjectRelease(self.0) };
    }
}

impl Object {
    fn conforms_to(&self, class: &CStr) -> bool {
        unsafe { IOObjectConformsTo(self.0, class.as_ptr()) != 0 }
    }

    fn property(&self, key: &str) -> Option<CFRetained<CFType>> {
        let key = CFString::from_str(key);
        let value = unsafe { IORegistryEntryCreateCFProperty(self.0, &key, ptr::null(), 0) }?;
        Some(unsafe { CFRetained::from_raw(value) })
    }

    fn string(&self, key: &str) -> Option<String> {
        let value = self.property(key)?;
        Some(value.downcast_ref::<CFString>()?.to_string())
    }

    fn flag(&self, key: &str) -> bool {
        self.property(key)
            .is_some_and(|v| v.downcast_ref::<CFBoolean>().is_some_and(|b| b.as_bool()))
    }

    fn descendants(&self) -> impl Iterator<Item = Object> {
        let mut iterator = 0;
        let status = unsafe {
            IORegistryEntryCreateIterator(
                self.0,
                c"IOService".as_ptr(),
                ITERATE_RECURSIVELY,
                &mut iterator,
            )
        };
        (status == 0)
            .then(|| Iter(Object(iterator)))
            .into_iter()
            .flatten()
    }
}

struct Iter(Object);

impl Iterator for Iter {
    type Item = Object;

    fn next(&mut self) -> Option<Object> {
        let next = unsafe { IOIteratorNext(self.0 .0) };
        (next != 0).then_some(Object(next))
    }
}

/// The kernel side of the controller: its user client carries the controller's UUID.
fn controller(uuid: &str) -> Option<Object> {
    let mut iterator = 0;
    let status = unsafe {
        let matching = IOServiceMatching(c"AppleUSBUserHCIUserClient".as_ptr());
        IOServiceGetMatchingServices(0, matching, &mut iterator)
    };
    if status != 0 {
        return None;
    }
    Iter(Object(iterator))
        .find(|o| o.string("IOUSBHostControllerInterfaceUUID").as_deref() == Some(uuid))
}

/// The product string of the device attached to the controller, once macOS has read it.
pub fn product_name(uuid: &str) -> Option<String> {
    controller(uuid)?
        .descendants()
        .find(|o| o.conforms_to(c"IOUSBHostDevice"))?
        .string("USB Product Name")
        .filter(|name| !name.trim().is_empty())
}

fn whole_disks(uuid: &str) -> Vec<String> {
    let Some(controller) = controller(uuid) else {
        return Vec::new();
    };
    controller
        .descendants()
        .filter(|o| o.conforms_to(c"IOMedia") && o.flag("Whole"))
        .filter_map(|o| o.string("BSD Name"))
        .collect()
}

type Reply = mpsc::Sender<Result<(), String>>;

unsafe extern "C-unwind" fn finished(
    _disk: NonNull<DADisk>,
    dissenter: *const DADissenter,
    context: *mut c_void,
) {
    let reply = unsafe { Box::from_raw(context.cast::<Reply>()) };
    let result = match unsafe { dissenter.as_ref() } {
        None => Ok(()),
        Some(dissenter) => Err(unsafe { dissenter.status_string() }
            .map(|s| s.to_string())
            .unwrap_or_else(|| "it is in use".into())),
    };
    let _ = reply.send(result);
}

fn wait(deadline: Instant, start: impl FnOnce(*mut c_void)) -> Result<(), String> {
    let (tx, rx) = mpsc::channel();
    start(Box::into_raw(Box::new(tx)).cast());
    rx.recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .unwrap_or_else(|_| Err("the disk did not respond in time".into()))
}

/// Unmounts and ejects every disk on the controller, the way Finder's Eject does. Blocks until
/// DiskArbitration answers or `deadline` passes, so call it off the main thread. An unmount can
/// take several seconds while DiskArbitration asks other apps whether they object.
pub fn eject(uuid: &str, deadline: Instant) -> Result<(), String> {
    let disks = whole_disks(uuid);
    if disks.is_empty() {
        return Ok(());
    }
    let session = unsafe { DASession::new(None) }.ok_or("DiskArbitration is unavailable")?;
    let queue = DispatchQueue::new("usbip.disks", None);
    unsafe { session.set_dispatch_queue(Some(&queue)) };
    let result = disks.iter().try_for_each(|name| {
        let bsd_name = CString::new(name.as_str()).map_err(|e| e.to_string())?;
        let disk = unsafe {
            DADisk::from_bsd_name(None, &session, NonNull::from(bsd_name.as_c_str()).cast())
        }
        .ok_or_else(|| format!("{name} has disappeared"))?;
        wait(deadline, |context| unsafe {
            disk.unmount(kDADiskUnmountOptionWhole, Some(finished), context)
        })
        .map_err(|e| format!("{name} could not be unmounted: {e}"))?;
        wait(deadline, |context| unsafe {
            disk.eject(kDADiskEjectOptionDefault, Some(finished), context)
        })
        .map_err(|e| format!("{name} could not be ejected: {e}"))?;
        info!("ejected {name}");
        Ok(())
    });
    unsafe { session.set_dispatch_queue(None) };
    result
}
