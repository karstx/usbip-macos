//! The parts of the Mac app that don't need AppKit, so they are tested on every platform.

use std::time::Duration;

use crate::protocol::ExportedDevice;

/// A short description of what the device is, from its interface classes.
pub fn device_kind(device: &ExportedDevice) -> &'static str {
    let interface = device
        .interfaces
        .iter()
        .find(|i| i.class != 0x0a)
        .or(device.interfaces.first());
    let Some(i) = interface else {
        return "USB device";
    };
    match (i.class, i.subclass, i.protocol) {
        (0x03, _, 1) => "Keyboard",
        (0x03, _, 2) => "Mouse",
        (0x03, _, _) => "Input device",
        (0x01, _, _) => "Audio device",
        (0x02, 0x02, _) => "Serial port",
        (0x02, 0x06 | 0x0d, _) => "Network adapter",
        (0x02, _, _) => "Communications device",
        (0x07, _, _) => "Printer",
        (0x08, _, _) => "Storage",
        (0x09, _, _) => "Hub",
        (0x0b, _, _) => "Smart card reader",
        (0x0e, _, _) => "Camera",
        (0xe0, 0x01, 0x01) => "Bluetooth adapter",
        (0xe0, _, _) => "Wireless controller",
        (0xfe, 0x01, _) => "Firmware updater",
        (0xff, _, _) => "Vendor-specific device",
        _ => "USB device",
    }
}

/// How long to wait before reconnection attempt `attempt` (counting from 0).
pub fn retry_delay(attempt: u32) -> Duration {
    Duration::from_secs(1u64 << attempt.min(5)).min(Duration::from_secs(30))
}

/// Whether release `latest` (a tag like `v1.2.0`) is newer than version `current`.
pub fn is_newer(latest: &str, current: &str) -> bool {
    fn parse(v: &str) -> Option<Vec<u64>> {
        let v = v.trim().trim_start_matches('v');
        v.split('.').map(|p| p.parse().ok()).collect()
    }
    match (parse(latest), parse(current)) {
        (Some(mut latest), Some(mut current)) => {
            let len = latest.len().max(current.len());
            latest.resize(len, 0);
            current.resize(len, 0);
            latest > current
        }
        _ => false,
    }
}

/// A device to keep attached, stored as `busid@server` in the preferences.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DeviceKey {
    pub server: String,
    pub busid: String,
}

impl DeviceKey {
    pub fn new(server: &str, busid: &str) -> Self {
        Self {
            server: server.into(),
            busid: busid.into(),
        }
    }

    pub fn encode(&self) -> String {
        format!("{}@{}", self.busid, self.server)
    }

    pub fn decode(s: &str) -> Option<Self> {
        let (busid, server) = s.split_once('@')?;
        (!busid.is_empty() && !server.is_empty()).then(|| Self::new(server, busid))
    }
}
