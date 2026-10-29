//! Attaches an exported device to a virtual host controller and detaches it again.

use std::time::Duration;

use thiserror::Error;

use crate::{
    client::{self, Session},
    host::Controller,
    protocol::ProtocolError,
};

/// How long in-flight transfers get to be unlinked before the controller goes away.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Debug, Error)]
pub enum AttachError {
    #[error("connecting to {address}: {source}")]
    Connect {
        address: String,
        source: std::io::Error,
    },
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
    #[error("{0}")]
    Controller(String),
}

pub struct Attachment {
    session: Session,
    controller: Controller,
}

impl Attachment {
    /// Imports `busid` from the server at `address` onto a new virtual controller. The device
    /// stays unplugged until `connect` is called.
    pub async fn import(address: &str, busid: &str) -> Result<Self, AttachError> {
        let stream = client::connect(address)
            .await
            .map_err(|source| AttachError::Connect {
                address: address.into(),
                source,
            })?;
        Self::new(client::import(stream, busid).await?).await
    }

    pub async fn new(session: Session) -> Result<Self, AttachError> {
        let controller = Controller::new(session.clone(), tokio::runtime::Handle::current())
            .map_err(AttachError::Controller)?;
        controller.wait_powered().await;
        Ok(Self {
            session,
            controller,
        })
    }

    pub fn session(&self) -> &Session {
        &self.session
    }

    /// Identifies this attachment's controller in the I/O Registry.
    pub fn controller_uuid(&self) -> String {
        self.controller.uuid()
    }

    /// Plugs the device into the controller's port, which makes macOS enumerate it.
    pub fn connect(&self) {
        self.controller.set_connected(true);
    }

    /// Resolves with the reason once the server connection is gone.
    pub async fn closed(&self) -> String {
        self.session.closed().await
    }

    pub async fn detach(self) {
        self.controller.set_connected(false);
        self.session.unlink_all();
        let _ = tokio::time::timeout(DRAIN_TIMEOUT, async {
            while self.session.pending() > 0 && !self.session.is_closed() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        drop(self.controller);
        self.session.close();
    }
}
