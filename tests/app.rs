use std::time::Duration;

use usbip_macos::app::{is_newer, retry_delay, DeviceKey, Kind};
use usbip_macos::protocol::{ExportedDevice, Interface, Speed};

fn device(interfaces: &[(u8, u8, u8)]) -> ExportedDevice {
    ExportedDevice {
        path: String::new(),
        busid: "1-1".into(),
        busnum: 1,
        devnum: 2,
        speed: Speed::High,
        id_vendor: 0x1209,
        id_product: 0x0001,
        bcd_device: 0,
        device_class: 0,
        device_subclass: 0,
        device_protocol: 0,
        configuration_value: 1,
        num_configurations: 1,
        num_interfaces: interfaces.len() as u8,
        interfaces: interfaces
            .iter()
            .map(|&(class, subclass, protocol)| Interface {
                class,
                subclass,
                protocol,
            })
            .collect(),
    }
}

#[test]
fn kinds() {
    let kind = |interfaces: &[(u8, u8, u8)]| Kind::of(&device(interfaces));
    assert_eq!(kind(&[(3, 1, 1)]), Kind::Keyboard);
    assert_eq!(kind(&[(3, 1, 2)]), Kind::Mouse);
    assert_eq!(kind(&[(8, 6, 0x50)]), Kind::Storage);
    assert_eq!(kind(&[(0xff, 0, 0)]), Kind::VendorSpecific);
    assert_eq!(kind(&[]), Kind::Other);
    assert_eq!(kind(&[(0x0a, 0, 0), (2, 2, 1)]), Kind::Serial);
    assert_eq!(kind(&[(0x0a, 0, 0)]), Kind::Other);
    assert_eq!(Kind::Storage.label(), "Storage");
}

#[test]
fn retry_delays() {
    assert_eq!(retry_delay(0), Duration::from_secs(1));
    assert_eq!(retry_delay(3), Duration::from_secs(8));
    assert_eq!(retry_delay(5), Duration::from_secs(30));
    assert_eq!(retry_delay(100), Duration::from_secs(30));
}

#[test]
fn versions() {
    assert!(is_newer("v0.1.0", "0.0.1"));
    assert!(is_newer("1.0", "0.9.9"));
    assert!(is_newer("v0.0.2", "0.0.1"));
    assert!(!is_newer("v0.0.1", "0.0.1"));
    assert!(!is_newer("0.0.1.0", "0.0.1"));
    assert!(!is_newer("v0.0.1", "0.1.0"));
    assert!(!is_newer("nightly", "0.0.1"));
}

#[test]
fn device_keys() {
    let key = DeviceKey::new("prue:3241", "1-1.4");
    assert_eq!(key.encode(), "1-1.4@prue:3241");
    assert_eq!(DeviceKey::decode("1-1.4@prue:3241"), Some(key));
    assert_eq!(
        DeviceKey::decode("1-1@[::1]:3240"),
        Some(DeviceKey::new("[::1]:3240", "1-1"))
    );
    assert_eq!(DeviceKey::decode("1-1"), None);
    assert_eq!(DeviceKey::decode("@host"), None);
    assert_eq!(DeviceKey::decode("1-1@"), None);
}
