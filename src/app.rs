//! The parts of the Mac app that don't need AppKit, so they are tested on every platform.

use std::time::Duration;

use crate::protocol::ExportedDevice;

/// What a device is, judged from its interface classes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Keyboard,
    Mouse,
    Input,
    Audio,
    Serial,
    Network,
    Communications,
    Printer,
    Storage,
    Hub,
    SmartCard,
    Camera,
    Bluetooth,
    Wireless,
    Firmware,
    VendorSpecific,
    Other,
}

impl Kind {
    pub fn of(device: &ExportedDevice) -> Self {
        // CDC data interfaces say nothing about the device, so the control interface decides.
        let interface = device
            .interfaces
            .iter()
            .find(|i| i.class != 0x0a)
            .or(device.interfaces.first());
        let Some(i) = interface else {
            return Self::Other;
        };
        match (i.class, i.subclass, i.protocol) {
            (0x03, _, 1) => Self::Keyboard,
            (0x03, _, 2) => Self::Mouse,
            (0x03, _, _) => Self::Input,
            (0x01, _, _) => Self::Audio,
            (0x02, 0x02, _) => Self::Serial,
            (0x02, 0x06 | 0x0d, _) => Self::Network,
            (0x02, _, _) => Self::Communications,
            (0x07, _, _) => Self::Printer,
            (0x08, _, _) => Self::Storage,
            (0x09, _, _) => Self::Hub,
            (0x0b, _, _) => Self::SmartCard,
            (0x0e, _, _) => Self::Camera,
            (0xe0, 0x01, 0x01) => Self::Bluetooth,
            (0xe0, _, _) => Self::Wireless,
            (0xfe, 0x01, _) => Self::Firmware,
            (0xff, _, _) => Self::VendorSpecific,
            _ => Self::Other,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Keyboard => "Keyboard",
            Self::Mouse => "Mouse",
            Self::Input => "Input device",
            Self::Audio => "Audio device",
            Self::Serial => "Serial port",
            Self::Network => "Network adapter",
            Self::Communications => "Communications device",
            Self::Printer => "Printer",
            Self::Storage => "Storage",
            Self::Hub => "Hub",
            Self::SmartCard => "Smart card reader",
            Self::Camera => "Camera",
            Self::Bluetooth => "Bluetooth adapter",
            Self::Wireless => "Wireless controller",
            Self::Firmware => "Firmware updater",
            Self::VendorSpecific => "Vendor-specific device",
            Self::Other => "USB device",
        }
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
