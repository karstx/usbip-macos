//! Serves the devices shown in the app's screenshots: `cargo run --example screenshot_server --
//! 127.0.0.1:3240`. Only the flash drive works when attached; the others just list.

#[path = "../tests/support/mod.rs"]
mod support;

use support::{
    classes::{self, Fifo, Storage},
    server::Export,
    usb::*,
};

fn device(busid: &str, pid: u16, body: Vec<u8>, product: &str) -> Export {
    StdDevice::new(
        Descriptors {
            device: device_descriptor(0x0200, [0, 0, 0], 64, classes::VID, pid, 1),
            configurations: vec![configuration(1, &body)],
            strings: vec!["usbip".into(), product.into(), busid.into()],
            ..Default::default()
        },
        Fifo::default(),
    )
    .export(busid, classes::FULL)
}

fn with_endpoints(mut body: Vec<u8>, endpoints: &[(u8, u8)]) -> Vec<u8> {
    for &(address, attributes) in endpoints {
        body.extend(endpoint(address, attributes, 64, if attributes == 3 { 10 } else { 0 }));
    }
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
        Descriptors {
            device: device_descriptor(0x0200, [0, 0, 0], 64, classes::VID, 0x7360, 1),
            configurations: vec![configuration(1, &storage)],
            strings: vec!["usbip".into(), "Flash Drive".into(), "DRIVE1".into()],
            ..Default::default()
        },
        Storage::new(classes::fat16(64 << 20, "FLASH DRIVE")),
    )
    .export("1-1", classes::HIGH);

    let mut serial = with_endpoints(interface(0, 0, 1, [2, 2, 1]), &[(0x83, 3)]);
    serial.extend(with_endpoints(interface(1, 0, 2, [0x0a, 0, 0]), &[(0x02, 2), (0x82, 2)]));

    let exports = vec![
        drive,
        device(
            "1-2",
            0x7361,
            with_endpoints(interface(0, 0, 1, [3, 1, 1]), &[(0x81, 3)]),
            "Keyboard",
        ),
        device("1-3", 0x7362, serial, "Serial Adapter"),
        device(
            "1-4",
            0x7363,
            with_endpoints(interface(0, 0, 2, [0xff, 0, 0]), &[(0x01, 2), (0x81, 2)]),
            "Dev Board",
        ),
        device(
            "1-5",
            0x7364,
            with_endpoints(interface(0, 0, 2, [7, 1, 2]), &[(0x01, 2), (0x81, 2)]),
            "Label Printer",
        ),
        device(
            "1-6",
            0x7365,
            with_endpoints(interface(0, 0, 1, [0x0b, 0, 0]), &[(0x81, 3)]),
            "Card Reader",
        ),
    ];
    let server = support::server::Server::bind(&addr, exports).await;
    eprintln!("serving on {}", server.addr);
    tokio::signal::ctrl_c().await.unwrap();
}
