// SPDX-License-Identifier: MPL-2.0

//! The AI worker's hosting of private connections (M5 piece F).
//!
//! The worker owns ai.db, so it is where the connection rows are written.
//! The connection service (`koru-connections`) is the only code that holds a
//! credential, and it never runs on the writer thread: it runs on its
//! caller's thread (the gateway's blocking thread for a read, the driver
//! thread below for upkeep) and reaches the rows through
//! [`store::WorkerConnectionStore`], one writer transaction per call.
//!
//! What is composed here, once per worker:
//!
//! - the service, from the vendor adapter, the credential store and the
//!   credential context the host supplies ([`ConnectionsSetup`]), ai.db
//!   through the writer, and the delegate's effect ledger through the private
//!   Bot channel;
//! - a **driver thread** that calls `recover()` once (again until it
//!   succeeds) and then `drive()` every [`DRIVE_EVERY`];
//! - an **announce thread** that sends `AnnouncePrivateDispatcher` with this
//!   process's dispatcher id whenever the writer adopts an owner, and again
//!   until the delegate acknowledges it.
//!
//! **With no vendor adapter there is nothing here.** A host that supplies no
//! [`ConnectionsSetup`] gets no service, no thread and no delegate command;
//! the worker starts as it always did, and every private read plans as
//! `private_unavailable`. No real adapter exists yet, so that is every
//! production build today.
//!
//! `connection.changed` is not published: the event-name grammar has no
//! `connection` area. The service is given the sink that drops every notice.

mod announce;
mod driver;
mod ledger;
pub(crate) mod plan;
pub(crate) mod store;

use super::*;
use koru_connections::{
    ConnectionPorts, ConnectionService, ConnectionVendor, NoConnectionChangeSink, SystemClock,
};
use koru_oauth_credentials::{CredentialContext, CredentialStore as ConnectionCredentials};

pub(in crate::worker) use announce::AnnounceBoard;
pub(in crate::worker) use store::StoreCall;

/// How often the service's own upkeep runs. The dispatcher's back-off starts
/// at 250 ms, so the timer must not be slower than that.
pub(in crate::worker) const DRIVE_EVERY: Duration = Duration::from_millis(200);

/// What the host supplies to compose private connections. All three, or the
/// worker hosts none: there is no partly configured state.
#[derive(Clone)]
pub struct ConnectionsSetup {
    /// The one qualified vendor adapter of this build.
    pub vendor: Arc<dyn ConnectionVendor>,
    /// The OS credential store in production; only the host constructs it.
    pub credentials: Arc<dyn ConnectionCredentials>,
    /// The installation the credential slots are scoped to.
    pub context: CredentialContext,
}
impl std::fmt::Debug for ConnectionsSetup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ConnectionsSetup([REDACTED])")
    }
}

/// The composed service and what the writer needs of it. Cloned into the
/// writer, the handle and every client.
#[derive(Clone)]
pub(in crate::worker) struct Hosted {
    pub(in crate::worker) service: Arc<ConnectionService>,
    /// Pure parsing of a link a model supplied, and the provider token.
    pub(in crate::worker) vendor: Arc<dyn ConnectionVendor>,
    pub(in crate::worker) announce: Arc<AnnounceBoard>,
}

impl Hosted {
    /// Compose the service and start its two threads. `None` when the host
    /// supplied no setup or no delegate: a read needs both.
    pub(in crate::worker) fn compose(
        setup: Option<ConnectionsSetup>,
        delegate: Option<Arc<dyn koru_delegate_api::bots::BotDelegatePort>>,
        sender: SyncSender<Envelope>,
        shutdown: Arc<AtomicBool>,
    ) -> Result<Option<Self>, String> {
        let (Some(setup), Some(delegate)) = (setup, delegate) else {
            return Ok(None);
        };
        let service = Arc::new(ConnectionService::new(ConnectionPorts {
            vendor: setup.vendor.clone(),
            credentials: setup.credentials,
            context: setup.context,
            store: Arc::new(store::WorkerConnectionStore::new(sender)),
            ledger: Arc::new(ledger::DelegateEffectLedger::new(delegate.clone())),
            clock: Arc::new(SystemClock),
            changes: Arc::new(NoConnectionChangeSink),
        }));
        let announce = Arc::new(AnnounceBoard::default());
        driver::spawn(service.clone(), shutdown.clone())?;
        announce::spawn(announce.clone(), delegate, shutdown)?;
        Ok(Some(Self {
            service,
            vendor: setup.vendor,
            announce,
        }))
    }
}

impl AiWorkerClient {
    /// Trusted host composition only; deliberately absent from
    /// `AiServerPort` and HTTP. The connection service, when this worker
    /// hosts one. Every call on it reaches ai.db through the writer and may
    /// reach the credential store and the vendor: **never call it on a
    /// thread that answers the person**.
    pub fn connection_service(&self) -> Option<Arc<ConnectionService>> {
        self.connections
            .as_ref()
            .map(|hosted| hosted.service.clone())
    }
}

#[cfg(test)]
mod tests;
