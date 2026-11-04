//! Serves the devices shown in the app's screenshots: `cargo run --example screenshot_server --
//! 127.0.0.1:3240`. They carry the ids and interface classes of devices commonly shared over
//! USB/IP, but only the flash drive and the receiver work when attached; the others just list.

#[path = "../tests/support/mod.rs"]
mod support;

use support::{
    classes::{self, Fifo, Mouse, Storage, MOUSE_REPORT},
    server::Export,
    usb::*,
};

fn descriptors(vid: u16, pid: u16, mps0: u8, body: &[u8], strings: [&str; 3]) -> Descriptors {
    Descriptors {
        device: device_descriptor(0x0200, [0, 0, 0], mps0, vid, pid, 1),
        configurations: vec![configuration(1, body)],
        strings: strings.map(String::from).to_vec(),
        ..Default::default()
    }
}

fn listed(busid: &str, vid: u16, pid: u16, body: Vec<u8>, strings: [&str; 3]) -> Export {
    StdDevice::new(descriptors(vid, pid, 64, &body, strings), Fifo::default())
        .export(busid, classes::FULL)
}

fn bulk_pair(number: u8, class: [u8; 3], out: u8, r#in: u8) -> Vec<u8> {
    let mut body = interface(number, 0, 2, class);
    body.extend(endpoint(out, 2, 64, 0));
    body.extend(endpoint(r#in, 2, 64, 0));
    body
}

fn cdc_acm(first: u8, notify: u8, out: u8, r#in: u8) -> Vec<u8> {
    let mut body = interface(first, 0, 1, [2, 2, 1]);
    body.extend(endpoint(notify, 3, 8, 16));
    body.extend(bulk_pair(first + 1, [0x0a, 0, 0], out, r#in));
    body
}

#[tokio::main]
async fn main() {
    support::init_logging();
    let addr = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "127.0.0.1:3240".into());

    let mut storage = interface(0, 0, 2, [8, 6, 0x50]);
    storage.extend(endpoint(0x01, 2, 512, 0));
    storage.extend(endpoint(0x81, 2, 512, 0));
    let drive = StdDevice::new(
        descriptors(
            0x0781,
            0x5581,
            64,
            &storage,
            ["SanDisk", "SanDisk Ultra", "4C530001"],
        ),
        Storage::new(classes::fat16(64 << 20, "FLASH DRIVE")),
    )
    .export("1-1", classes::HIGH);

    let mut receiver = interface(0, 0, 1, [3, 1, 2]);
    let len = (MOUSE_REPORT.len() as u16).to_le_bytes();
    receiver.extend([9, 0x21, 0x11, 0x01, 0, 1, 0x22, len[0], len[1]]);
    receiver.extend(endpoint(0x81, 3, 4, 10));
    let mut receiver = descriptors(
        0x046d,
        0xc52b,
        8,
        &receiver,
        ["Logitech", "USB Receiver", ""],
    );
    receiver.hid_reports = [(0, MOUSE_REPORT.to_vec())].into();
    let receiver = StdDevice::new(receiver, Mouse::default()).export("1-2", classes::LOW);

    let mut yubikey = interface(0, 0, 1, [3, 1, 1]);
    yubikey.extend(endpoint(0x81, 3, 8, 10));
    yubikey.extend(interface(1, 0, 2, [3, 0, 0]));
    yubikey.extend(endpoint(0x04, 3, 64, 2));
    yubikey.extend(endpoint(0x84, 3, 64, 2));
    yubikey.extend(bulk_pair(2, [0x0b, 0, 0], 0x02, 0x82));

    let mut stlink = bulk_pair(0, [0xff, 0xff, 0xff], 0x01, 0x81);
    stlink.extend(bulk_pair(1, [8, 6, 0x50], 0x03, 0x83));
    stlink.extend(cdc_acm(2, 0x84, 0x05, 0x85));

    let exports = vec![
        drive,
        receiver,
        listed(
            "1-3",
            0x10c4,
            0xea60,
            bulk_pair(0, [0xff, 0, 0], 0x01, 0x81),
            ["ITead", "Sonoff Zigbee 3.0 USB Dongle Plus", "20220801"],
        ),
        listed(
            "1-4",
            0x2e8a,
            0x000a,
            cdc_acm(0, 0x81, 0x02, 0x82),
            ["Raspberry Pi", "Pico", "E6614103E7"],
        ),
        listed(
            "1-5",
            0x1050,
            0x0407,
            yubikey,
            ["Yubico", "YubiKey OTP+FIDO+CCID", ""],
        ),
        listed(
            "1-6",
            0x0483,
            0x374b,
            stlink,
            ["STMicroelectronics", "STM32 STLink", "066DFF"],
        ),
        listed(
            "1-7",
            0x04f9,
            0x2042,
            bulk_pair(0, [7, 1, 2], 0x02, 0x81),
            ["Brother", "QL-700", "000F1Z"],
        ),
    ];
    let server = support::server::Server::bind(&addr, exports).await;
    eprintln!("serving on {}", server.addr);
    tokio::signal::ctrl_c().await.unwrap();
}
