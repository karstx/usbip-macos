//! What the menu shows, shared between the main thread and the tasks that talk to servers.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex, MutexGuard},
};

use tokio::sync::watch;
use usbip_macos::{
    app::{device_kind, DeviceKey},
    cli,
    protocol::ExportedDevice,
};

use super::prefs;

pub struct Server {
    /// What the user typed.
    pub input: String,
    pub address: String,
    /// `None` until the first listing finishes.
    pub devices: Option<Result<Vec<ExportedDevice>, String>>,
}

impl Server {
    pub fn new(input: &str) -> Option<Self> {
        Some(Self {
            input: input.into(),
            address: cli::server_address(input)?,
            devices: None,
        })
    }

    pub fn listed(&self, busid: &str) -> Option<&ExportedDevice> {
        match &self.devices {
            Some(Ok(devices)) => devices.iter().find(|d| d.busid == busid),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    Attaching,
    Attached,
    /// Waiting to try again after the given error.
    Reconnecting(String),
    Ejecting,
    Detaching,
    /// Detached for system sleep, attaches again on wake.
    Asleep,
}

pub struct Link {
    pub state: State,
    pub stop: watch::Sender<bool>,
    /// UUID of the virtual controller while attached.
    pub controller: Option<String>,
    /// The device as the server described it on import, which leaves out its interfaces.
    pub device: Option<ExportedDevice>,
}

#[derive(Default)]
pub struct Model {
    pub servers: Vec<Server>,
    pub links: BTreeMap<DeviceKey, Link>,
    /// Devices to attach again after a relaunch.
    pub wanted: BTreeSet<DeviceKey>,
    pub names: BTreeMap<(u16, u16), String>,
    pub update: Option<(String, String)>,
    pub can_host: bool,
}

impl Model {
    pub fn server(&self, input: &str) -> Option<&Server> {
        self.servers.iter().find(|s| s.input == input)
    }

    pub fn server_mut(&mut self, input: &str) -> Option<&mut Server> {
        self.servers.iter_mut().find(|s| s.input == input)
    }

    /// The listing comes first: only it carries the interface classes.
    pub fn device(&self, key: &DeviceKey) -> Option<&ExportedDevice> {
        self.server(&key.server)
            .and_then(|s| s.listed(&key.busid))
            .or_else(|| self.links.get(key)?.device.as_ref())
    }

    /// The device's product name once one was seen, otherwise what kind of device it is.
    pub fn label(&self, key: &DeviceKey) -> String {
        match self.device(key) {
            Some(d) => self
                .names
                .get(&(d.id_vendor, d.id_product))
                .cloned()
                .unwrap_or_else(|| device_kind(d).into()),
            None => key.busid.clone(),
        }
    }

    pub fn want(&mut self, key: &DeviceKey) {
        if self.wanted.insert(key.clone()) {
            prefs::save_attached(&self.wanted);
        }
    }

    pub fn forget(&mut self, key: &DeviceKey) {
        if self.wanted.remove(key) {
            prefs::save_attached(&self.wanted);
        }
    }
}

pub struct Shared {
    pub model: Mutex<Model>,
    /// False between the system announcing sleep and waking up.
    pub awake: watch::Sender<bool>,
    /// Bumped when the network changes or the user asks to try again, so waiting reconnections
    /// try right away.
    pub retry_now: watch::Sender<u64>,
    /// Redraws the menu; posted to the main queue.
    pub redraw: Box<dyn Fn() + Send + Sync>,
}

impl Shared {
    pub fn new(model: Model, redraw: impl Fn() + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(Self {
            model: Mutex::new(model),
            awake: watch::channel(true).0,
            retry_now: watch::channel(0).0,
            redraw: Box::new(redraw),
        })
    }

    pub fn lock(&self) -> MutexGuard<'_, Model> {
        self.model.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Changes the model and redraws the menu.
    pub fn update<T>(&self, f: impl FnOnce(&mut Model) -> T) -> T {
        let result = f(&mut self.lock());
        (self.redraw)();
        result
    }

    pub fn set_state(&self, key: &DeviceKey, state: State) {
        self.update(|m| {
            if let Some(link) = m.links.get_mut(key) {
                link.state = state;
            }
        });
    }
}
