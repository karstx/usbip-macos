//! Keeps one device attached: imports it, notices when the connection drops, and imports it
//! again until told to stop.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use log::{info, warn};
use tokio::{runtime::Runtime, sync::watch};
use usbip_macos::{
    app::{retry_delay, DeviceKey},
    attach::{AttachError, Attachment},
};

use super::{
    registry,
    state::{Link, Shared, State},
    system,
};

const IMPORT_TIMEOUT: Duration = Duration::from_secs(10);
/// How long ejecting a disk may take: generous when the user detaches it, since they wait for
/// it anyway, and short before sleep and quit, which can't wait long.
pub const EJECT_TIMEOUT: Duration = Duration::from_secs(60);
const EJECT_BEFORE_SLEEP: Duration = Duration::from_secs(4);
pub const EJECT_BEFORE_QUIT: Duration = Duration::from_secs(10);
/// An attachment that lasted this long resets the retry delay.
const STABLE: Duration = Duration::from_secs(30);
/// After a "disconnected" notification, further disconnects stay quiet this long.
const QUIET: Duration = Duration::from_secs(5 * 60);
/// A remembered device that keeps failing to attach is reported after this many attempts.
const FAILURES_BEFORE_NOTICE: u32 = 3;
const NAME_POLLS: u32 = 20;
const NAME_POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Starts keeping `key` attached. A device the user just clicked gives up after a failed first
/// import; one restored from a previous launch keeps trying, since its server may not be up yet.
pub fn start(
    shared: &Arc<Shared>,
    runtime: &Runtime,
    key: DeviceKey,
    address: String,
    user_initiated: bool,
) {
    let (stop, stopped) = watch::channel(false);
    shared.update(|m| {
        m.links.insert(
            key.clone(),
            Link {
                state: State::Attaching,
                stop,
                controller: None,
                device: None,
            },
        )
    });
    runtime.spawn(supervise(
        Arc::clone(shared),
        key,
        address,
        stopped,
        user_initiated,
    ));
}

enum End {
    Stopped,
    Slept,
    Lost { reason: String, lasted: Duration },
}

/// Decides which events are worth a notification, so a flapping connection doesn't post one
/// every few seconds. The log gets every event regardless.
#[derive(Default)]
struct Notices {
    /// A "disconnected" or "can't reattach" notification is waiting for its "reconnected".
    outstanding: bool,
    last_disconnect: Option<Instant>,
    failures: u32,
}

impl Notices {
    fn lost(&mut self, label: &str, server: &str) {
        if self.outstanding || self.last_disconnect.is_some_and(|t| t.elapsed() < QUIET) {
            return;
        }
        self.outstanding = true;
        self.last_disconnect = Some(Instant::now());
        system::post(
            format!("{label} disconnected"),
            format!("Lost the connection to {server}. Reconnecting."),
        );
    }

    fn failed(&mut self, label: &str, error: &str) {
        self.failures += 1;
        if self.outstanding || self.failures != FAILURES_BEFORE_NOTICE {
            return;
        }
        self.outstanding = true;
        system::post(
            format!("Can't reattach {label}"),
            format!("{error}. Still trying."),
        );
    }

    fn attached(&mut self, label: &str, server: &str) {
        self.failures = 0;
        if std::mem::take(&mut self.outstanding) {
            system::post(
                format!("{label} reconnected"),
                format!("Attached again from {server}."),
            );
        }
    }
}

async fn supervise(
    shared: Arc<Shared>,
    key: DeviceKey,
    address: String,
    stop: watch::Receiver<bool>,
    user_initiated: bool,
) {
    let mut retrying = !user_initiated;
    let mut attempt = 0;
    let mut notices = Notices::default();
    let mut error: Option<String> = None;
    loop {
        if !wait_awake(&shared, stop.clone()).await {
            break;
        }
        let state = error.clone().map_or(State::Attaching, State::Reconnecting);
        shared.set_state(&key, state);
        match attach_once(&shared, &key, &address, stop.clone(), &mut notices).await {
            Ok(End::Stopped) => break,
            Ok(End::Slept) => {
                shared.set_state(&key, State::Asleep);
                attempt = 0;
                error = None;
                continue;
            }
            Ok(End::Lost { reason, lasted }) => {
                warn!("{}: connection lost: {reason}", key.encode());
                retrying = true;
                if lasted > STABLE {
                    attempt = 0;
                }
                notices.lost(&shared.lock().label(&key), &key.server);
                error = Some(reason);
            }
            Err(e) if !retrying => {
                warn!("{}: {e}", key.encode());
                let label = shared.update(|m| {
                    m.forget(&key);
                    m.label(&key)
                });
                system::post(format!("Couldn't attach {label}"), e);
                break;
            }
            Err(e) => {
                warn!("{}: {e}", key.encode());
                notices.failed(&shared.lock().label(&key), &e);
                error = Some(e);
            }
        }
        if let Some(e) = &error {
            shared.set_state(&key, State::Reconnecting(e.clone()));
        }
        if !wait_retry(&shared, stop.clone(), retry_delay(attempt)).await {
            break;
        }
        attempt += 1;
    }
    shared.update(|m| m.links.remove(&key));
}

async fn attach_once(
    shared: &Arc<Shared>,
    key: &DeviceKey,
    address: &str,
    mut stop: watch::Receiver<bool>,
    notices: &mut Notices,
) -> Result<End, String> {
    let import = tokio::time::timeout(IMPORT_TIMEOUT, Attachment::import(address, &key.busid));
    let attachment = tokio::select! {
        biased;
        _ = stop.wait_for(|s| *s) => return Ok(End::Stopped),
        result = import => match result {
            Ok(Ok(attachment)) => attachment,
            Ok(Err(e)) => return Err(explain(shared, e)),
            Err(_) => return Err(format!("{} did not answer", key.server)),
        },
    };
    attachment.connect();
    let since = Instant::now();
    let uuid = attachment.controller_uuid();
    let device = attachment.session().device().clone();
    info!("attached {} on controller {uuid}", key.encode());
    let label = shared.update(|m| {
        m.want(key);
        if let Some(link) = m.links.get_mut(key) {
            link.state = State::Attached;
            link.controller = Some(uuid.clone());
            link.device = Some(device.clone());
        }
        m.label(key)
    });
    notices.attached(&label, &key.server);
    tokio::spawn(learn_name(
        Arc::clone(shared),
        uuid.clone(),
        (device.id_vendor, device.id_product),
    ));

    let mut awake = shared.awake.subscribe();
    let end = tokio::select! {
        biased;
        _ = stop.wait_for(|s| *s) => End::Stopped,
        _ = awake.wait_for(|a| !*a) => End::Slept,
        reason = attachment.closed() => End::Lost { reason, lasted: since.elapsed() },
    };
    if matches!(end, End::Slept) {
        shared.set_state(key, State::Ejecting);
        if let Err(e) = eject(uuid, EJECT_BEFORE_SLEEP).await {
            warn!("{}: ejecting before sleep: {e}", key.encode());
        }
    }
    shared.update(|m| {
        if let Some(link) = m.links.get_mut(key) {
            link.state = State::Detaching;
            link.controller = None;
        }
    });
    attachment.detach().await;
    info!("detached {}", key.encode());
    Ok(end)
}

/// Ejects the disks of the device on controller `uuid`, giving up after `timeout`.
pub async fn eject(uuid: String, timeout: Duration) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    tokio::task::spawn_blocking(move || registry::eject(&uuid, deadline))
        .await
        .unwrap_or_else(|e| Err(e.to_string()))
}

fn explain(shared: &Shared, e: AttachError) -> String {
    match e {
        AttachError::Controller(e) if !shared.lock().can_host => format!(
            "{e}. This copy of usbip isn't signed with the USB host controller entitlement, \
             so it can only attach devices when run as root."
        ),
        e => e.to_string(),
    }
}

/// Waits until the system is awake. False if told to stop first.
async fn wait_awake(shared: &Shared, mut stop: watch::Receiver<bool>) -> bool {
    let mut awake = shared.awake.subscribe();
    tokio::select! {
        biased;
        _ = stop.wait_for(|s| *s) => false,
        _ = awake.wait_for(|a| *a) => true,
    }
}

/// Waits out `delay`, cut short by a network change, Try Again or the system waking. False if
/// told to stop first.
async fn wait_retry(shared: &Shared, mut stop: watch::Receiver<bool>, delay: Duration) -> bool {
    let mut retry_now = shared.retry_now.subscribe();
    let mut awake = shared.awake.subscribe();
    retry_now.borrow_and_update();
    awake.borrow_and_update();
    tokio::select! {
        biased;
        _ = stop.wait_for(|s| *s) => false,
        _ = tokio::time::sleep(delay) => true,
        _ = retry_now.changed() => true,
        _ = awake.changed() => true,
    }
}

/// macOS reads the product string while enumerating the device, shortly after it is plugged in.
async fn learn_name(shared: Arc<Shared>, uuid: String, ids: (u16, u16)) {
    for _ in 0..NAME_POLLS {
        tokio::time::sleep(NAME_POLL_INTERVAL).await;
        let lookup = uuid.clone();
        let Ok(name) = tokio::task::spawn_blocking(move || registry::product_name(&lookup)).await
        else {
            return;
        };
        if let Some(name) = name {
            shared.update(|m| {
                if m.names.get(&ids) != Some(&name) {
                    m.names.insert(ids, name);
                    super::prefs::save_names(&m.names);
                }
            });
            return;
        }
    }
}
