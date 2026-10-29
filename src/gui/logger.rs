//! Logs to the unified log, and keeps the recent informational lines for the Settings window.

use std::{
    collections::VecDeque,
    sync::{Mutex, OnceLock},
    time::SystemTime,
};

use log::{Level, LevelFilter, Log, Metadata, Record};
use oslog::OsLogger;

const SUBSYSTEM: &str = "net.karstx.usbip";
const KEEP: usize = 2000;
/// The window shows this much; the unified log may get more.
const SHOWN: Level = Level::Info;

pub struct Entry {
    pub time: SystemTime,
    pub level: Level,
    pub target: String,
    pub message: String,
}

struct Logger {
    os: OsLogger,
    recent: Mutex<VecDeque<Entry>>,
    listener: OnceLock<fn()>,
}

static LOGGER: OnceLock<Logger> = OnceLock::new();

impl Log for Logger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= SHOWN || self.os.enabled(metadata)
    }

    fn log(&self, record: &Record) {
        if self.os.enabled(record.metadata()) {
            self.os.log(record);
        }
        if record.level() > SHOWN {
            return;
        }
        let target = record.target();
        let entry = Entry {
            time: SystemTime::now(),
            level: record.level(),
            target: target
                .strip_prefix("usbip_macos::")
                .unwrap_or(target)
                .to_string(),
            message: record.args().to_string(),
        };
        {
            let mut recent = self.recent.lock().unwrap_or_else(|e| e.into_inner());
            recent.push_back(entry);
            while recent.len() > KEEP {
                recent.pop_front();
            }
        }
        if let Some(listener) = self.listener.get() {
            listener();
        }
    }

    fn flush(&self) {}
}

fn logger() -> Option<&'static Logger> {
    LOGGER.get()
}

pub fn init(level: LevelFilter) {
    let logger = LOGGER.get_or_init(|| Logger {
        os: OsLogger::new(SUBSYSTEM).level_filter(level),
        recent: Mutex::default(),
        listener: OnceLock::new(),
    });
    if log::set_logger(logger).is_ok() {
        log::set_max_level(level.max(SHOWN.to_level_filter()));
    }
}

/// Calls `listener`, from whatever thread logged, after every kept line.
pub fn set_listener(listener: fn()) {
    if let Some(logger) = logger() {
        let _ = logger.listener.set(listener);
    }
}

pub fn with_recent<T>(f: impl FnOnce(&VecDeque<Entry>) -> T) -> T {
    let empty = VecDeque::new();
    match logger() {
        Some(logger) => f(&logger.recent.lock().unwrap_or_else(|e| e.into_inner())),
        None => f(&empty),
    }
}

pub fn clear() {
    if let Some(logger) = logger() {
        logger
            .recent
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }
}
