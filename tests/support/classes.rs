//! Simulated devices for speeds and classes the loopback device does not cover.

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

use tokio::sync::Notify;

use super::{
    server::{Completion, Handled, Outcome, Urb, EPIPE},
    usb::*,
};

pub const VID: u16 = 0x1209;
pub const PID_FIFO: u16 = 0x7358;
pub const PID_MOUSE: u16 = 0x7359;

/// USB/IP speed codes.
pub const LOW: u32 = 1;
pub const FULL: u32 = 2;
pub const HIGH: u32 = 3;
pub const SUPER: u32 = 5;
pub const SUPER_PLUS: u32 = 6;

/// Bulk OUT 0x01 feeds a FIFO that bulk IN 0x81 drains.
#[derive(Default)]
pub struct Fifo {
    data: Mutex<VecDeque<u8>>,
    ready: Notify,
    pub received: Mutex<usize>,
}

impl Function for Fifo {
    fn transfer(self: Arc<Self>, urb: Urb) -> Handled {
        let this = self;
        Box::pin(async move {
            if !urb.is_in() {
                this.data.lock().unwrap().extend(&urb.data);
                *this.received.lock().unwrap() += urb.data.len();
                this.ready.notify_waiters();
                return Outcome::Complete(Completion::written(urb.data.len() as u32));
            }
            loop {
                let ready = this.ready.notified();
                {
                    let mut data = this.data.lock().unwrap();
                    if !data.is_empty() {
                        let n = data.len().min(urb.length as usize);
                        return Outcome::Complete(Completion::ok(data.drain(..n).collect()));
                    }
                }
                ready.await;
            }
        })
    }
}

/// A bulk FIFO with descriptors valid for `speed`: 64-byte bulk packets at full speed, 512 at
/// high speed, 1024 plus companion descriptors and a BOS at SuperSpeed.
pub fn fifo(speed: u32) -> StdDevice<Fifo> {
    let (bcd_usb, mps0, bulk) = match speed {
        SUPER | SUPER_PLUS => (0x0320, 9, 1024),
        HIGH => (0x0200, 64, 512),
        _ => (0x0200, 64, 64),
    };
    let mut body = interface(0, 0, 2, [0xff, 0, 0]);
    for address in [0x01, 0x81] {
        body.extend(endpoint(address, 2, bulk, 0));
        if speed >= SUPER {
            body.extend(ss_companion(3, 0, 0));
        }
    }
    StdDevice::new(
        Descriptors {
            device: device_descriptor(bcd_usb, [0, 0, 0], mps0, VID, PID_FIFO, 1),
            configurations: vec![configuration(1, &body)],
            strings: vec![
                "usbip-macos tests".into(),
                "FIFO".into(),
                format!("FIFO{speed}"),
            ],
            bos: (speed >= SUPER).then(superspeed_bos),
            qualifier: None,
            ..Default::default()
        },
        Fifo::default(),
    )
}

/// A low-speed boot mouse that reports no movement.
#[derive(Default)]
pub struct Mouse {
    pub polls: Mutex<usize>,
}

const MOUSE_REPORT: [u8; 50] = [
    0x05, 0x01, 0x09, 0x02, 0xa1, 0x01, 0x09, 0x01, 0xa1, 0x00, 0x05, 0x09, 0x19, 0x01, 0x29, 0x03,
    0x15, 0x00, 0x25, 0x01, 0x95, 0x03, 0x75, 0x01, 0x81, 0x02, 0x95, 0x01, 0x75, 0x05, 0x81, 0x01,
    0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x15, 0x81, 0x25, 0x7f, 0x75, 0x08, 0x95, 0x02, 0x81, 0x06,
    0xc0, 0xc0,
];

impl Function for Mouse {
    fn control(&self, urb: &Urb) -> Option<Outcome> {
        match (urb.request_type(), urb.request()) {
            // SET_IDLE, SET_PROTOCOL
            (0x21, 0x0a | 0x0b) => Some(ack()),
            // GET_REPORT
            (0xa1, 0x01) => Some(reply(vec![0; 3], urb)),
            _ => None,
        }
    }

    fn transfer(self: Arc<Self>, urb: Urb) -> Handled {
        let this = self;
        Box::pin(async move {
            if !urb.is_in() {
                return Outcome::Complete(Completion::status(-EPIPE));
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            *this.polls.lock().unwrap() += 1;
            Outcome::Complete(Completion::ok(vec![0; 3]))
        })
    }
}

pub fn low_speed_mouse() -> StdDevice<Mouse> {
    let mut body = interface(0, 0, 1, [3, 1, 2]);
    let len = (MOUSE_REPORT.len() as u16).to_le_bytes();
    body.extend([9, 0x21, 0x11, 0x01, 0, 1, 0x22, len[0], len[1]]);
    body.extend(endpoint(0x81, 3, 4, 10));
    StdDevice::new(
        Descriptors {
            device: device_descriptor(0x0110, [0, 0, 0], 8, VID, PID_MOUSE, 1),
            configurations: vec![configuration(1, &body)],
            strings: vec!["usbip-macos tests".into(), "Mouse".into(), "MOUSE1".into()],
            hid_reports: [(0, MOUSE_REPORT.to_vec())].into(),
            ..Default::default()
        },
        Mouse::default(),
    )
}

pub const PID_BURST: u16 = 0x735b;

/// Interrupt IN data, numbered so a test can tell reports apart.
#[derive(Default)]
pub struct Burst {
    pub reports: Mutex<u8>,
}

impl Function for Burst {
    fn transfer(self: Arc<Self>, urb: Urb) -> Handled {
        let this = self;
        Box::pin(async move {
            if !urb.is_in() {
                return Outcome::Complete(Completion::status(-EPIPE));
            }
            let n = {
                let mut reports = this.reports.lock().unwrap();
                *reports = reports.wrapping_add(1);
                *reports
            };
            let data = (0..urb.length).map(|i| n.wrapping_add(i as u8)).collect();
            Outcome::Complete(Completion::ok(data))
        })
    }
}

/// A high-speed, high-bandwidth interrupt IN endpoint: three 1024-byte packets per microframe
/// (`wMaxPacketSize` 0x1400), as some video and audio devices use.
pub fn high_bandwidth_interrupt() -> StdDevice<Burst> {
    let mut body = interface(0, 0, 1, [0xff, 0, 0]);
    body.extend(endpoint(0x81, 3, 0x1400, 1));
    StdDevice::new(
        Descriptors {
            device: device_descriptor(0x0200, [0, 0, 0], 64, VID, PID_BURST, 1),
            configurations: vec![configuration(1, &body)],
            strings: vec!["usbip-macos tests".into(), "Burst".into(), "BURST1".into()],
            ..Default::default()
        },
        Burst::default(),
    )
}

pub const PID_MULTI: u16 = 0x735a;

/// Two configurations: 1 has a bulk FIFO interface, 2 a single interrupt interface.
pub fn two_configurations() -> StdDevice<Fifo> {
    let mut first = interface(0, 0, 2, [0xff, 0, 0]);
    first.extend(endpoint(0x01, 2, 512, 0));
    first.extend(endpoint(0x81, 2, 512, 0));
    let mut second = interface(0, 0, 1, [0xff, 1, 0]);
    second.extend(endpoint(0x82, 3, 64, 4));
    StdDevice::new(
        Descriptors {
            device: device_descriptor(0x0200, [0, 0, 0], 64, VID, PID_MULTI, 2),
            configurations: vec![configuration(1, &first), configuration(2, &second)],
            strings: vec![
                "usbip-macos tests".into(),
                "Two configs".into(),
                "MULTI1".into(),
            ],
            ..Default::default()
        },
        Fifo::default(),
    )
}

pub const PID_STORAGE: u16 = 0x735c;
pub const STORAGE_BLOCK: usize = 512;

enum BotPhase {
    Command,
    DataIn(Vec<u8>),
    DataOut { offset: usize, remaining: usize },
    Status,
}

struct Bot {
    phase: BotPhase,
    tag: u32,
    expected: u32,
    transferred: u32,
    failed: bool,
    /// Sense key, ASC and ASCQ for the next REQUEST SENSE.
    sense: [u8; 3],
    /// Bumped by a reset. The test server keeps serving a dropped client's URBs, so an IN
    /// transfer from before the reset must not take data meant for the new one.
    generation: u64,
}

/// A Bulk-Only Transport SCSI disk held in memory. It understands the commands macOS sends to
/// mount, read and write a volume and fails the rest with ILLEGAL REQUEST.
pub struct Storage {
    disk: Mutex<Vec<u8>>,
    bot: Mutex<Bot>,
    ready: Notify,
}

impl Storage {
    pub fn new(bytes: usize) -> Self {
        Self {
            disk: Mutex::new(vec![0; bytes]),
            bot: Mutex::new(Bot {
                phase: BotPhase::Command,
                tag: 0,
                expected: 0,
                transferred: 0,
                failed: false,
                sense: [0; 3],
                generation: 0,
            }),
            ready: Notify::new(),
        }
    }

    fn reset(&self) {
        let mut bot = self.bot.lock().unwrap();
        bot.phase = BotPhase::Command;
        bot.generation += 1;
        drop(bot);
        self.ready.notify_waiters();
    }

    fn command(&self, bot: &mut Bot, cdb: &[u8]) {
        let blocks = (self.disk.lock().unwrap().len() / STORAGE_BLOCK) as u32;
        let lba = u32::from_be_bytes([cdb[2], cdb[3], cdb[4], cdb[5]]) as usize;
        let count = u16::from_be_bytes([cdb[7], cdb[8]]) as usize;
        bot.failed = false;
        bot.phase = match cdb[0] {
            // TEST UNIT READY, START STOP UNIT, PREVENT ALLOW MEDIUM REMOVAL, SYNCHRONIZE CACHE
            0x00 | 0x1b | 0x1e | 0x35 => BotPhase::Status,
            // REQUEST SENSE
            0x03 => {
                let mut sense = vec![0; 18];
                sense[0] = 0x70;
                sense[2] = bot.sense[0];
                sense[7] = 10;
                sense[12] = bot.sense[1];
                sense[13] = bot.sense[2];
                bot.sense = [0; 3];
                BotPhase::DataIn(sense)
            }
            // INQUIRY
            0x12 => {
                let mut inquiry = vec![0, 0x80, 4, 2, 31, 0, 0, 0];
                inquiry.extend(b"usbip   Test Disk       1.0 ");
                BotPhase::DataIn(inquiry)
            }
            // MODE SENSE (6) and (10), no pages and not write protected
            0x1a => BotPhase::DataIn(vec![3, 0, 0, 0]),
            0x5a => BotPhase::DataIn(vec![0, 6, 0, 0, 0, 0, 0, 0]),
            // READ FORMAT CAPACITIES
            0x23 => {
                let mut caps = vec![0, 0, 0, 8];
                caps.extend(blocks.to_be_bytes());
                caps.extend([2, 0, (STORAGE_BLOCK >> 8) as u8, STORAGE_BLOCK as u8]);
                BotPhase::DataIn(caps)
            }
            // READ CAPACITY (10)
            0x25 => {
                let mut caps = (blocks - 1).to_be_bytes().to_vec();
                caps.extend((STORAGE_BLOCK as u32).to_be_bytes());
                BotPhase::DataIn(caps)
            }
            // READ (10)
            0x28 => {
                let start = lba * STORAGE_BLOCK;
                let end = start + count * STORAGE_BLOCK;
                let disk = self.disk.lock().unwrap();
                BotPhase::DataIn(disk.get(start..end).unwrap_or_default().to_vec())
            }
            // WRITE (10)
            0x2a => BotPhase::DataOut {
                offset: lba * STORAGE_BLOCK,
                remaining: count * STORAGE_BLOCK,
            },
            _ => {
                bot.failed = true;
                bot.sense = [5, 0x20, 0];
                if bot.expected > 0 {
                    BotPhase::DataIn(Vec::new())
                } else {
                    BotPhase::Status
                }
            }
        };
        if let BotPhase::DataIn(data) = &mut bot.phase {
            data.truncate(bot.expected as usize);
        }
    }

    fn csw(bot: &mut Bot) -> Vec<u8> {
        let mut csw = b"USBS".to_vec();
        csw.extend(bot.tag.to_le_bytes());
        csw.extend(bot.expected.saturating_sub(bot.transferred).to_le_bytes());
        csw.push(u8::from(bot.failed));
        bot.phase = BotPhase::Command;
        csw
    }
}

impl Function for Storage {
    fn control(&self, urb: &Urb) -> Option<Outcome> {
        match (urb.request_type(), urb.request()) {
            // GET MAX LUN
            (0xa1, 0xfe) => Some(reply(vec![0], urb)),
            // Bulk-Only Mass Storage Reset
            (0x21, 0xff) => {
                self.reset();
                Some(ack())
            }
            _ => None,
        }
    }

    fn set_configuration(&self, _value: u8) {
        self.reset();
    }

    fn transfer(self: Arc<Self>, urb: Urb) -> Handled {
        let this = self;
        Box::pin(async move {
            if !urb.is_in() {
                let mut bot = this.bot.lock().unwrap();
                match bot.phase {
                    BotPhase::DataOut { offset, remaining } => {
                        let n = remaining.min(urb.data.len());
                        if let Some(dst) = this.disk.lock().unwrap().get_mut(offset..offset + n) {
                            dst.copy_from_slice(&urb.data[..n]);
                        }
                        bot.transferred += n as u32;
                        bot.phase = if remaining > n {
                            BotPhase::DataOut {
                                offset: offset + n,
                                remaining: remaining - n,
                            }
                        } else {
                            BotPhase::Status
                        };
                    }
                    _ if urb.data.len() == 31 && urb.data.starts_with(b"USBC") => {
                        bot.tag = u32::from_le_bytes(urb.data[4..8].try_into().unwrap());
                        bot.expected = u32::from_le_bytes(urb.data[8..12].try_into().unwrap());
                        bot.transferred = 0;
                        let mut cdb = [0; 16];
                        cdb.copy_from_slice(&urb.data[15..31]);
                        this.command(&mut bot, &cdb);
                    }
                    _ => return Outcome::Complete(Completion::status(-EPIPE)),
                }
                drop(bot);
                this.ready.notify_waiters();
                return Outcome::Complete(Completion::written(urb.data.len() as u32));
            }
            let generation = this.bot.lock().unwrap().generation;
            loop {
                let ready = this.ready.notified();
                {
                    let mut bot = this.bot.lock().unwrap();
                    if bot.generation != generation {
                        return Outcome::Complete(Completion::status(-EPIPE));
                    }
                    match &mut bot.phase {
                        BotPhase::DataIn(data) => {
                            let n = data.len().min(urb.length as usize);
                            let chunk: Vec<u8> = data.drain(..n).collect();
                            if data.is_empty() {
                                bot.phase = BotPhase::Status;
                            }
                            bot.transferred += n as u32;
                            return Outcome::Complete(Completion::ok(chunk));
                        }
                        BotPhase::Status => {
                            return Outcome::Complete(Completion::ok(Self::csw(&mut bot)))
                        }
                        _ => {}
                    }
                }
                ready.await;
            }
        })
    }
}

/// A high-speed Bulk-Only mass storage device backed by `bytes` of RAM.
pub fn storage(bytes: usize) -> StdDevice<Storage> {
    let mut body = interface(0, 0, 2, [8, 6, 0x50]);
    body.extend(endpoint(0x01, 2, 512, 0));
    body.extend(endpoint(0x81, 2, 512, 0));
    StdDevice::new(
        Descriptors {
            device: device_descriptor(0x0200, [0, 0, 0], 64, VID, PID_STORAGE, 1),
            configurations: vec![configuration(1, &body)],
            strings: vec!["usbip-macos tests".into(), "Disk".into(), "DISK1".into()],
            ..Default::default()
        },
        Storage::new(bytes),
    )
}
