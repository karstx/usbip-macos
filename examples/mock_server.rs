//! Serves simulated devices for manual experiments:
//! `cargo run --example mock_server -- 127.0.0.1:3240 [speed]`. It exports the loopback device
//! used by the tests as `1-1` (at `speed`), a 64 MiB RAM disk as `1-2` and a mouse as `1-3`.

#[path = "../tests/support/mod.rs"]
mod support;

use support::classes;

#[tokio::main]
async fn main() {
    support::init_logging();
    let addr = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "127.0.0.1:3240".into());
    let speed: u32 = std::env::args().nth(2).map_or(3, |s| s.parse().unwrap());
    let device = support::device::LoopbackDevice::default();
    let mut loopback = device.export("1-1");
    loopback.speed = speed;
    let exports = vec![
        loopback,
        classes::storage(64 << 20).export("1-2", classes::HIGH),
        classes::low_speed_mouse().export("1-3", classes::LOW),
    ];
    let server = support::server::Server::bind(&addr, exports).await;
    eprintln!("serving 1-1, 1-2 and 1-3 on {}", server.addr);
    tokio::signal::ctrl_c().await.unwrap();
    for e in server.log.violations() {
        eprintln!("protocol violation: {e}");
    }
}
