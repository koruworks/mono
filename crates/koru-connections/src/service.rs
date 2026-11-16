// SPDX-License-Identifier: MPL-2.0

//! The connection service: connect, sign in again, choose more files,
//! disconnect, and the one read of one picked item.
//!
//! It is the only code that holds a secret. Its public surface returns text
//! that was read, codes, receipts and connection rows; no function here
//! returns a `SecretString`, and none takes one from a caller except the
//! authorization code on the callback, which goes straight to the vendor.
//!
//! **The rules a read is performed under** are the delegate's, and each has
//! its place in [`ConnectionService::read`]:
//!
//! 1. The vendor is called only after the delegate answered the dispatch
//!    mark with a row whose stage is `dispatched`.
//! 2. Content is handed over only on a reply with `deliver: true`, and only
//!    to a caller that is still there.
//! 3. A reply of `bot_private_effect_stale` means someone else closed the
//!    row: whatever was read is dropped.
//! 4. What the call told its caller is what it records. A read whose receipt
//!    could not be recorded in time is told, and recorded, as unknown.
//! 5. An effect row of this process that is still `prepared` and that no
//!    call here is performing (its `execute` reply was lost) is refused as
//!    not dispatched, so it does not stay open for the life of the process.
//! 6. Nothing is sent to the vendor twice for one effect, and an unknown
//!    outcome is never retried.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use base64::Engine;
use koru_ai_core::bots::{BotOwner, BotRequestId};
use koru_ai_core::connections::{
    CONNECTION_ITEM_LIMIT, CONNECTION_TITLE_BYTES, ConnectedAccount, Connection,
    ConnectionCommandAdmission, ConnectionCommandReceipt, ConnectionError, ConnectionFailure,
    ConnectionId, ConnectionItem, ConnectionItemState, ConnectionReceiptStage, ConnectionState,
    PickedConnectionItem, VendorRevoke, scopes_json,
};
use koru_delegate_core::{
    PrivateDispatcherId, PrivateEffectDisposition, PrivateEffectKey, PrivateEffectRefusal,
    PrivateEffectStage, PrivateEffectTransition, PrivateEffectView, PrivateOperationProposal,
    RunId,
};
use koru_oauth_credentials::{CredentialContext, CredentialEntry, CredentialStore, SecretString};
use sha2::Digest;

use crate::identity::{
    account_subject_digest, command_fingerprint, connection_slot, credential_slot_digest,
    item_ref_digest, process_dispatcher,
};
use crate::ports::{
    Clock, ConnectionChange, ConnectionChangeSink, ConnectionChanged, ConnectionStore,
    EffectLedger, LedgerError, ReadCaller,
};
use crate::vendor::{
    AccessToken, AuthorizationCallback, AuthorizationRequest, AuthorizedAccount, ConnectionVendor,
    ItemContent, ItemIdentityDigest, PickedItem, READ_LIMIT_BYTES, RevokeOutcome, VendorError,
    VendorItemRef,
};

/// How long the person has to finish signing in.
pub const CONSENT_WAIT: Duration = Duration::from_secs(300);
/// The vendor's share of one read: the refresh, the identity check and the
/// read itself.
pub const VENDOR_READ_DEADLINE: Duration = Duration::from_secs(20);
/// The code exchange at the end of a consent trip.
pub const AUTHORIZE_DEADLINE: Duration = Duration::from_secs(20);
pub const REVOKE_DEADLINE: Duration = Duration::from_secs(15);
/// The dispatcher's back-off for an advance the delegate did not answer:
/// 250 ms, doubling to 32 s, for as long as the process lives.
pub const ADVANCE_BACKOFF_FIRST: Duration = Duration::from_millis(250);
pub const ADVANCE_BACKOFF_MAX: Duration = Duration::from_secs(32);
/// An access token this close to its expiry is refreshed before use.
const ACCESS_MARGIN: Duration = Duration::from_secs(30);

/// What a stored text becomes when the vendor gave none.
const NO_TEXT: &str = "\u{2014}";

/// The text of one item, as it is handed to whoever asked for the read.
/// Never serialized to one of Koru's own wires, and never printed.
pub struct PrivateReadContent {
    text: String,
    truncated: bool,
    total_bytes: u64,
}
impl PrivateReadContent {
    /// At most [`READ_LIMIT_BYTES`] of text, cut at a character boundary. A
    /// cut is always stated.
    pub fn new(content: ItemContent) -> Self {
        let ItemContent {
            mut text,
            mut truncated,
            total_bytes,
        } = content;
        if text.len() > READ_LIMIT_BYTES {
            let mut end = READ_LIMIT_BYTES;
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            text.truncate(end);
            truncated = true;
        }
        let total_bytes = total_bytes.max(text.len() as u64);
        Self {
            text,
            truncated,
            total_bytes,
        }
    }
    pub fn text(&self) -> &str {
        &self.text
    }
    pub fn into_text(self) -> String {
        self.text
    }
    pub fn truncated(&self) -> bool {
        self.truncated
    }
    pub fn total_bytes(&self) -> u64 {
        self.total_bytes
    }
}
impl std::fmt::Debug for PrivateReadContent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PrivateReadContent")
            .field("bytes", &self.text.len())
            .field("truncated", &self.truncated)
            .field("total_bytes", &self.total_bytes)
            .finish()
    }
}

/// How one read ended, for the caller that asked. Only `Delivered` carries
/// anything that was read.
#[derive(Debug)]
pub enum ReadOutcome {
    /// The delegate recorded the read and said `deliver`. This is the read.
    Delivered(PrivateReadContent),
    /// Nothing was read, or the vendor refused. The row says why.
    Refused(PrivateEffectRefusal),
    /// The item was read and its content was dropped: access was removed, or
    /// the account disconnected, while the call was in flight.
    Withheld,
    /// Whether the read happened is not known. It is never tried again.
    Unknown,
    /// An earlier call performed this read and its reply was lost.
    ResultUnavailable,
    /// A call in this process is performing this exact read now.
    InProgress,
    /// The read was recorded and nobody was left to receive it. Dropped.
    CallerGone,
    /// The row belongs to another process's dispatcher. Nothing was done.
    NotOurs,
}

/// A consent trip that began, or the receipt of one that already ended.
#[derive(Debug)]
pub enum TripStart {
    /// Open `consent` for the person; the vendor answers on the redirect.
    Consent {
        request: BotRequestId,
        consent: AuthorizationRequest,
    },
    Finished(ConnectionCommandReceipt),
}

/// The end of a Disconnect: the connection as it is now and the receipt.
#[derive(Clone, Debug)]
pub struct Disconnected {
    pub connection: Connection,
    pub receipt: ConnectionCommandReceipt,
}

/// What the start of the service found and finished.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Recovered {
    /// Trips a previous process began and could not finish: failed as
    /// `interrupted`.
    pub trips_interrupted: usize,
    /// Planned slots of connects that never became a connection: cleared.
    pub slots_cleared: usize,
    pub disconnects_finished: usize,
    /// Left `prepared` because the credential store could not be reached:
    /// the next start finds them again.
    pub deferred: usize,
}

enum TripKind {
    Connect { planned: ConnectionId, name: String },
    SignInAgain { connection: Box<Connection> },
    AddItems { connection: Box<Connection> },
}

struct Trip {
    owner: BotOwner,
    kind: TripKind,
    state: String,
    verifier: SecretString,
    redirect_uri: String,
    consent: AuthorizationRequest,
    deadline: Instant,
    /// The callback arrived and the trip is being completed: it is not
    /// expired, canceled or completed a second time meanwhile.
    finishing: std::sync::atomic::AtomicBool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// The dispatch mark was not sent: the vendor was not called.
    Registered,
    /// The mark was sent and its reply is not known.
    MarkSent,
    /// The delegate answered `dispatched`: the vendor may have been called.
    Dispatched,
}

/// One effect row this process must bring to a final stage. It lives in the
/// service, not on the calling thread's stack: a call that panics leaves it
/// here, and [`ConnectionService::drive`] finishes it.
struct Obligation {
    key: PrivateEffectKey,
    deadline: Instant,
    phase: Phase,
    /// The truth to record, once it is known and was not recorded yet.
    pending: Option<PrivateEffectTransition>,
    /// The call that registered it ended without settling it.
    abandoned: bool,
    backoff: Duration,
    next_attempt: Instant,
}

type ObligationId = (RunId, uuid::Uuid);

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Marks its obligation abandoned if the call unwinds before settling it.
struct Abandon<'a> {
    obligations: &'a Mutex<HashMap<ObligationId, Obligation>>,
    id: ObligationId,
    armed: bool,
}
impl Drop for Abandon<'_> {
    fn drop(&mut self) {
        if self.armed {
            if let Some(obligation) = lock(self.obligations).get_mut(&self.id) {
                obligation.abandoned = true;
            }
        }
    }
}

/// Everything the service is composed of. Each field is a port: the
/// composition supplies the real ones, a test the fakes.
pub struct ConnectionPorts {
    pub vendor: Arc<dyn ConnectionVendor>,
    /// The OS credential store in production. This crate never constructs it.
    pub credentials: Arc<dyn CredentialStore>,
    /// The installation the credential slots are scoped to.
    pub context: CredentialContext,
    pub store: Arc<dyn ConnectionStore>,
    pub ledger: Arc<dyn EffectLedger>,
    pub clock: Arc<dyn Clock>,
    pub changes: Arc<dyn ConnectionChangeSink>,
}

pub struct ConnectionService {
    ports: ConnectionPorts,
    trips: Mutex<HashMap<BotRequestId, Arc<Trip>>>,
    /// One cell per connection: holding it is the single flight of a refresh.
    access: Mutex<HashMap<ConnectionId, Arc<Mutex<Option<AccessToken>>>>>,
    obligations: Mutex<HashMap<ObligationId, Obligation>>,
    /// Connections whose rotated credential could not be stored: marked as
    /// needing sign-in once the read that found out has finished.
    lapsed: Mutex<Vec<(BotOwner, ConnectionId)>>,
}

impl std::fmt::Debug for ConnectionService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectionService")
            .field("open_trips", &lock(&self.trips).len())
            .field("open_obligations", &lock(&self.obligations).len())
            .finish_non_exhaustive()
    }
}

fn random_token() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

fn pkce_challenge(verifier: &SecretString) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(sha2::Sha256::digest(verifier.expose().as_bytes()))
}

/// `text` cut to `max` bytes at a character boundary; never empty.
fn bounded_text(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.is_empty() {
        return NO_TEXT.to_owned();
    }
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

fn trip_failure(error: VendorError) -> ConnectionFailure {
    match error {
        VendorError::NotConfigured => ConnectionFailure::NotConfigured,
        VendorError::ConsentDenied => ConnectionFailure::ConsentDenied,
        VendorError::Unavailable | VendorError::Unconfirmed => ConnectionFailure::VendorUnavailable,
        _ => ConnectionFailure::VendorRejected,
    }
}

fn store_failure(error: ConnectionError) -> ConnectionFailure {
    match error {
        ConnectionError::AccountInUse => ConnectionFailure::AccountInUse,
        ConnectionError::NameInUse => ConnectionFailure::NameInUse,
        ConnectionError::ItemLimit => ConnectionFailure::ItemLimit,
        ConnectionError::WrongAccount => ConnectionFailure::WrongAccount,
        ConnectionError::RevisionConflict => ConnectionFailure::RevisionConflict,
        _ => ConnectionFailure::Interrupted,
    }
}

/// What a final row tells a caller that did not perform it.
fn told_by(view: &PrivateEffectView) -> ReadOutcome {
    match view.stage {
        PrivateEffectStage::Confirmed if view.withheld == Some(true) => ReadOutcome::Withheld,
        PrivateEffectStage::Confirmed => ReadOutcome::ResultUnavailable,
        PrivateEffectStage::Refused => {
            ReadOutcome::Refused(view.refusal.unwrap_or(PrivateEffectRefusal::Unavailable))
        }
        PrivateEffectStage::Unknown => ReadOutcome::Unknown,
        PrivateEffectStage::Prepared | PrivateEffectStage::Dispatched => ReadOutcome::InProgress,
    }
}

impl ConnectionService {
    pub fn new(ports: ConnectionPorts) -> Self {
        Self {
            ports,
            trips: Mutex::default(),
            access: Mutex::default(),
            obligations: Mutex::default(),
            lapsed: Mutex::default(),
        }
    }

    /// The identity every effect this process performs is opened under.
    pub fn dispatcher(&self) -> PrivateDispatcherId {
        process_dispatcher()
    }

    /// Effect rows this process still owes a final stage.
    pub fn open_obligations(&self) -> usize {
        lock(&self.obligations).len()
    }

    fn wall(&self) -> i64 {
        self.ports.clock.wall_ms()
    }

    fn slot(&self, connection: ConnectionId) -> Option<CredentialEntry> {
        connection_slot(
            &self.ports.context,
            self.ports.vendor.service_origin(),
            connection,
        )
        .ok()
    }

    fn notify(&self, connection: &Connection, change: ConnectionChange) {
        self.ports.changes.connection_changed(ConnectionChanged {
            connection: connection.id,
            provider: connection.provider,
            change,
            revision: connection.revision,
            vendor_revoke: (change == ConnectionChange::Disconnected)
                .then_some(connection.vendor_revoke)
                .flatten(),
        });
    }

    fn access_cell(&self, connection: ConnectionId) -> Arc<Mutex<Option<AccessToken>>> {
        lock(&self.access).entry(connection).or_default().clone()
    }

    fn forget_access(&self, connection: ConnectionId) {
        lock(&self.access).remove(&connection);
    }

    fn mark_needs_sign_in(&self, owner: BotOwner, connection: ConnectionId) {
        self.forget_access(connection);
        if let Ok(current) = self
            .ports
            .store
            .needs_sign_in(owner, connection, self.wall())
        {
            if current.state == ConnectionState::NeedsSignIn {
                self.notify(&current, ConnectionChange::NeedsSignIn);
            }
        }
    }

    // ------------------------------------------------------------ trips

    /// Connect an account. `name` is the name the person gives the
    /// connection; `redirect_uri` is where the host listens for the vendor's
    /// answer. One connect at a time per person. The same request returns
    /// the same consent while the trip is open, and its receipt once it
    /// ended.
    pub fn begin_connect(
        &self,
        owner: BotOwner,
        request: BotRequestId,
        name: &str,
        redirect_uri: &str,
    ) -> Result<TripStart, ConnectionError> {
        let fingerprint = command_fingerprint("connect", &[name.as_bytes()]);
        let planned = ConnectionId::mint();
        let admission = self.ports.store.begin_connect(
            owner,
            request,
            &fingerprint,
            planned,
            name,
            self.wall(),
        )?;
        // A retained receipt keeps the id it planned the first time.
        let planned = admission.receipt().planned_connection.unwrap_or(planned);
        self.open_trip(
            owner,
            request,
            admission,
            TripKind::Connect {
                planned,
                name: name.to_owned(),
            },
            redirect_uri,
        )
    }

    /// The same trip for a connection that needs sign-in. The account that
    /// comes back must be the connection's own.
    pub fn begin_sign_in_again(
        &self,
        owner: BotOwner,
        request: BotRequestId,
        connection: ConnectionId,
        expected_revision: u64,
        redirect_uri: &str,
    ) -> Result<TripStart, ConnectionError> {
        let fingerprint = command_fingerprint(
            "sign_in_again",
            &[
                connection.to_storage().as_bytes(),
                &expected_revision.to_le_bytes(),
            ],
        );
        let admission = self.ports.store.begin_sign_in_again(
            owner,
            request,
            &fingerprint,
            connection,
            expected_revision,
            self.wall(),
        )?;
        let current = self
            .ports
            .store
            .connection(owner, connection)?
            .ok_or(ConnectionError::NotFound)?;
        self.open_trip(
            owner,
            request,
            admission,
            TripKind::SignInAgain {
                connection: Box::new(current),
            },
            redirect_uri,
        )
    }

    /// The picker trip again, to choose more files or to choose again one
    /// that changed.
    pub fn begin_add_items(
        &self,
        owner: BotOwner,
        request: BotRequestId,
        connection: ConnectionId,
        expected_revision: u64,
        redirect_uri: &str,
    ) -> Result<TripStart, ConnectionError> {
        let fingerprint = command_fingerprint(
            "add_items",
            &[
                connection.to_storage().as_bytes(),
                &expected_revision.to_le_bytes(),
            ],
        );
        let admission = self.ports.store.begin_add_items(
            owner,
            request,
            &fingerprint,
            connection,
            expected_revision,
            self.wall(),
        )?;
        let current = self
            .ports
            .store
            .connection(owner, connection)?
            .ok_or(ConnectionError::NotFound)?;
        self.open_trip(
            owner,
            request,
            admission,
            TripKind::AddItems {
                connection: Box::new(current),
            },
            redirect_uri,
        )
    }

    fn open_trip(
        &self,
        owner: BotOwner,
        request: BotRequestId,
        admission: ConnectionCommandAdmission,
        kind: TripKind,
        redirect_uri: &str,
    ) -> Result<TripStart, ConnectionError> {
        let receipt = admission.receipt().clone();
        if receipt.stage != ConnectionReceiptStage::Prepared {
            return Ok(TripStart::Finished(receipt));
        }
        if matches!(admission, ConnectionCommandAdmission::Retained(_)) {
            if let Some(trip) = lock(&self.trips).get(&request) {
                return Ok(TripStart::Consent {
                    request,
                    consent: trip.consent.clone(),
                });
            }
            // Prepared, and nothing in memory holds its verifier: the process
            // that began it is gone, and it can never finish.
            return self
                .ports
                .store
                .fail_command(owner, request, ConnectionFailure::Interrupted, self.wall())
                .map(TripStart::Finished);
        }
        let state = random_token();
        let Ok(verifier) = SecretString::new(format!("{}{}", random_token(), random_token()))
        else {
            return Err(ConnectionError::Storage);
        };
        match self.ports.vendor.authorization_request(
            redirect_uri,
            &state,
            &pkce_challenge(&verifier),
        ) {
            Ok(consent) => {
                lock(&self.trips).insert(
                    request,
                    Arc::new(Trip {
                        owner,
                        kind,
                        state,
                        verifier,
                        redirect_uri: redirect_uri.to_owned(),
                        consent: consent.clone(),
                        deadline: self.ports.clock.now() + CONSENT_WAIT,
                        finishing: false.into(),
                    }),
                );
                Ok(TripStart::Consent { request, consent })
            }
            // No registration in this build, or the adapter could not build
            // the request: the command ends here, with its reason.
            Err(error) => self
                .ports
                .store
                .fail_command(owner, request, trip_failure(error), self.wall())
                .map(TripStart::Finished),
        }
    }

    /// The vendor answered on the redirect. Exchanges the code, compares the
    /// account, stores the credential and completes the command, in that
    /// order. A callback whose `state` is not this trip's is refused and the
    /// trip keeps waiting. Returns the command's final receipt.
    pub fn complete_trip(
        &self,
        owner: BotOwner,
        request: BotRequestId,
        callback: AuthorizationCallback,
    ) -> Result<ConnectionCommandReceipt, ConnectionError> {
        let live = lock(&self.trips).get(&request).cloned();
        let Some(trip) = live else {
            // No live trip: answer what was retained, if anything.
            return self
                .ports
                .store
                .receipt(owner, request)?
                .ok_or(ConnectionError::NotFound);
        };
        if trip.owner != owner {
            return Err(ConnectionError::Forbidden);
        }
        if trip.state != callback.state {
            return Err(ConnectionError::Invalid);
        }
        if trip
            .finishing
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            return Err(ConnectionError::Busy);
        }
        let outcome = if self.ports.clock.now() > trip.deadline {
            Err(ConnectionFailure::TimedOut)
        } else if callback.code.is_none() {
            Err(ConnectionFailure::ConsentDenied)
        } else {
            self.finish_trip(owner, request, &trip, &callback)
        };
        lock(&self.trips).remove(&request);
        match outcome {
            Ok(()) => self
                .ports
                .store
                .receipt(owner, request)?
                .ok_or(ConnectionError::Storage),
            Err(failure) => self
                .ports
                .store
                .fail_command(owner, request, failure, self.wall()),
        }
    }

    fn finish_trip(
        &self,
        owner: BotOwner,
        request: BotRequestId,
        trip: &Trip,
        callback: &AuthorizationCallback,
    ) -> Result<(), ConnectionFailure> {
        let vendor = &self.ports.vendor;
        let account: AuthorizedAccount = vendor
            .complete_authorization(
                callback,
                &trip.verifier,
                &trip.redirect_uri,
                self.ports.clock.now() + AUTHORIZE_DEADLINE,
            )
            .map_err(trip_failure)?;
        let provider = vendor.provider();
        let subject = account_subject_digest(provider, &account.subject);
        if account.items.len() > CONNECTION_ITEM_LIMIT {
            return Err(ConnectionFailure::ItemLimit);
        }
        let mut items: Vec<PickedConnectionItem> = Vec::new();
        for PickedItem {
            reference,
            identity,
            title,
        } in &account.items
        {
            let digest = item_ref_digest(provider, reference);
            if items.iter().all(|item| item.item_ref_digest != digest) {
                items.push(PickedConnectionItem {
                    item_ref: reference.as_str().to_owned(),
                    item_ref_digest: digest,
                    identity_digest: identity.0,
                    title: bounded_text(title, CONNECTION_TITLE_BYTES),
                });
            }
        }
        let mut scopes: Vec<String> = account
            .scopes
            .iter()
            .filter(|scope| !scope.is_empty())
            .take(64)
            .cloned()
            .collect();
        if scopes_json(&scopes).is_none() {
            scopes.clear();
        }
        let described = |name: &str, slot: &CredentialEntry| ConnectedAccount {
            provider,
            service_origin: vendor.service_origin().to_owned(),
            name: name.to_owned(),
            account_subject_digest: subject,
            account_label: bounded_text(&account.label, 320),
            scopes: scopes.clone(),
            credential_slot_digest: credential_slot_digest(slot),
            items: items.clone(),
        };
        match &trip.kind {
            TripKind::Connect { planned, name } => {
                // The same account twice is decided before anything is
                // stored. The new tokens are dropped here and never revoked:
                // a revoke would end the grant of the connection that
                // already holds this account.
                let live = self
                    .ports
                    .store
                    .live_connections(owner)
                    .map_err(store_failure)?;
                if live
                    .iter()
                    .any(|c| c.provider == provider && c.account_subject_digest == subject)
                {
                    return Err(ConnectionFailure::AccountInUse);
                }
                let slot = self
                    .slot(*planned)
                    .ok_or(ConnectionFailure::CredentialStoreUnavailable)?;
                // The credential is in the planned slot before the row
                // exists, and is cleared again if the row is not written.
                self.ports
                    .credentials
                    .store(&slot, account.refresh.clone())
                    .map_err(|_| ConnectionFailure::CredentialStoreUnavailable)?;
                match self.ports.store.complete_connect(
                    owner,
                    request,
                    &described(name, &slot),
                    self.wall(),
                ) {
                    Ok(connection) => {
                        *lock(&self.access_cell(connection.id)) = Some(account.access.clone());
                        self.notify(&connection, ConnectionChange::Connected);
                        Ok(())
                    }
                    Err(error) => {
                        let _ = self.ports.credentials.clear(&slot);
                        Err(store_failure(error))
                    }
                }
            }
            TripKind::SignInAgain { connection } | TripKind::AddItems { connection } => {
                // The account is compared before the token is stored: a
                // wrong account never overwrites the slot, and its tokens
                // are dropped, never revoked.
                if connection.provider != provider || connection.account_subject_digest != subject {
                    return Err(ConnectionFailure::WrongAccount);
                }
                let slot = self
                    .slot(connection.id)
                    .filter(|slot| {
                        credential_slot_digest(slot) == connection.credential_slot_digest
                    })
                    .ok_or(ConnectionFailure::CredentialStoreUnavailable)?;
                self.ports
                    .credentials
                    .store(&slot, account.refresh.clone())
                    .map_err(|_| ConnectionFailure::CredentialStoreUnavailable)?;
                let (completed, change) = if matches!(trip.kind, TripKind::SignInAgain { .. }) {
                    (
                        self.ports.store.complete_sign_in_again(
                            owner,
                            request,
                            &described(&connection.name, &slot),
                            self.wall(),
                        ),
                        ConnectionChange::SignedIn,
                    )
                } else {
                    (
                        self.ports
                            .store
                            .complete_add_items(owner, request, &items, self.wall()),
                        ConnectionChange::ItemsChanged,
                    )
                };
                // On a failure the slot is left alone: it holds a credential
                // of the same account.
                let connection = completed.map_err(store_failure)?;
                *lock(&self.access_cell(connection.id)) = Some(account.access.clone());
                self.notify(&connection, change);
                Ok(())
            }
        }
    }

    /// The person gave up. The command fails as `canceled`.
    pub fn cancel_trip(
        &self,
        owner: BotOwner,
        request: BotRequestId,
    ) -> Result<ConnectionCommandReceipt, ConnectionError> {
        let live = lock(&self.trips).get(&request).cloned();
        let Some(trip) = live else {
            return self
                .ports
                .store
                .receipt(owner, request)?
                .ok_or(ConnectionError::NotFound);
        };
        if trip.owner != owner {
            return Err(ConnectionError::Forbidden);
        }
        if trip
            .finishing
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            return Err(ConnectionError::Busy);
        }
        lock(&self.trips).remove(&request);
        self.ports
            .store
            .fail_command(owner, request, ConnectionFailure::Canceled, self.wall())
    }

    // ------------------------------------------------------- disconnect

    /// Disconnect an account, for every Bot and Routine at once. The fence
    /// (`disconnecting`, a higher revision) is committed first; then the
    /// vendor is asked to revoke, the credential is cleared, and the
    /// tombstone records what the vendor said. If the credential store
    /// cannot be reached the command fails, the connection stays fenced, and
    /// the command can be repeated.
    pub fn disconnect(
        &self,
        owner: BotOwner,
        request: BotRequestId,
        connection: ConnectionId,
        expected_revision: u64,
    ) -> Result<Disconnected, ConnectionError> {
        let fingerprint = command_fingerprint(
            "disconnect",
            &[
                connection.to_storage().as_bytes(),
                &expected_revision.to_le_bytes(),
            ],
        );
        let (admission, current) = self.ports.store.begin_disconnect(
            owner,
            request,
            &fingerprint,
            connection,
            expected_revision,
            self.wall(),
        )?;
        let receipt = admission.receipt().clone();
        if receipt.stage != ConnectionReceiptStage::Prepared {
            return Ok(Disconnected {
                connection: current,
                receipt,
            });
        }
        // From here no read of this connection is dispatched, and one in
        // flight is withheld. Its cached access token is dropped now.
        self.forget_access(connection);
        if matches!(admission, ConnectionCommandAdmission::Prepared(_)) {
            self.notify(&current, ConnectionChange::Disconnecting);
        }
        self.finish_disconnect(owner, request, &current)
    }

    /// The command's own finish: when the credential store cannot be
    /// reached the receipt fails, the connection stays fenced, and the
    /// person repeats the command.
    fn finish_disconnect(
        &self,
        owner: BotOwner,
        request: BotRequestId,
        connection: &Connection,
    ) -> Result<Disconnected, ConnectionError> {
        if let Some(done) = self.disconnect_steps(owner, request, connection)? {
            return Ok(done);
        }
        let receipt = self.ports.store.fail_command(
            owner,
            request,
            ConnectionFailure::CredentialStoreUnavailable,
            self.wall(),
        )?;
        let connection = self
            .ports
            .store
            .connection(owner, connection.id)?
            .ok_or(ConnectionError::Storage)?;
        Ok(Disconnected {
            connection,
            receipt,
        })
    }

    /// Steps 2 to 4 of a Disconnect. `None` when the credential store could
    /// not be reached: nothing was recorded, and the receipt is still
    /// `prepared`.
    fn disconnect_steps(
        &self,
        owner: BotOwner,
        request: BotRequestId,
        connection: &Connection,
    ) -> Result<Option<Disconnected>, ConnectionError> {
        let Some(slot) = self.slot(connection.id) else {
            return Ok(None);
        };
        let revoke = match self.ports.credentials.load(&slot) {
            Err(_) => return Ok(None),
            // Nothing to revoke with: Koru cannot say what the vendor holds.
            Ok(None) => VendorRevoke::Unconfirmed,
            Ok(Some(refresh)) => match self
                .ports
                .vendor
                .revoke(&refresh, self.ports.clock.now() + REVOKE_DEADLINE)
            {
                Ok(RevokeOutcome::Revoked) => VendorRevoke::Confirmed,
                Ok(RevokeOutcome::Unsupported) => VendorRevoke::Unsupported,
                Err(_) => VendorRevoke::Unconfirmed,
            },
        };
        if self.ports.credentials.clear(&slot).is_err() {
            return Ok(None);
        }
        self.forget_access(connection.id);
        let connection =
            self.ports
                .store
                .complete_disconnect(owner, request, revoke, self.wall())?;
        self.notify(&connection, ConnectionChange::Disconnected);
        let receipt = self
            .ports
            .store
            .receipt(owner, request)?
            .ok_or(ConnectionError::Storage)?;
        Ok(Some(Disconnected {
            connection,
            receipt,
        }))
    }

    // ---------------------------------------------------------- recovery

    /// Once, when the service starts. A trip a previous process began holds
    /// its verifier nowhere, so it is failed as `interrupted`; the planned
    /// slot of a connect that never became a connection is cleared first, so
    /// no credential is left that no row names. A sign-in again's slot is
    /// the connection's own and is left alone. A Disconnect that was fenced
    /// and not finished is finished.
    pub fn recover(&self) -> Result<Recovered, ConnectionError> {
        let mut found = Recovered::default();
        let mut after = 0;
        loop {
            let page = self.ports.store.prepared_commands(after, 64)?;
            if page.is_empty() {
                return Ok(found);
            }
            for (row, receipt) in page {
                after = row;
                if lock(&self.trips).contains_key(&receipt.request) {
                    continue;
                }
                let owner = receipt.owner;
                if receipt.kind == koru_ai_core::connections::ConnectionCommandKind::Disconnect {
                    // With the credential store locked the receipt is left
                    // `prepared`, so the next start finishes it.
                    let finished = receipt
                        .connection
                        .and_then(|id| self.ports.store.connection(owner, id).ok().flatten())
                        .map(|connection| {
                            self.disconnect_steps(owner, receipt.request, &connection)
                        });
                    match finished {
                        Some(Ok(Some(_))) => found.disconnects_finished += 1,
                        _ => found.deferred += 1,
                    }
                    continue;
                }
                // A connect that planned a new connection: clear its slot
                // unless a row with that id exists.
                if let (Some(planned), None) = (receipt.planned_connection, receipt.connection) {
                    let unused = matches!(self.ports.store.connection(owner, planned), Ok(None));
                    if unused {
                        let cleared = self
                            .slot(planned)
                            .is_some_and(|slot| self.ports.credentials.clear(&slot).is_ok());
                        if !cleared {
                            // The store is locked: keep the receipt, so the
                            // next start finds the slot again.
                            found.deferred += 1;
                            continue;
                        }
                        found.slots_cleared += 1;
                    }
                }
                self.ports.store.fail_command(
                    owner,
                    receipt.request,
                    ConnectionFailure::Interrupted,
                    self.wall(),
                )?;
                found.trips_interrupted += 1;
            }
        }
    }

    // -------------------------------------------------------------- read

    /// Perform one read of one picked item, for an effect the delegate
    /// opened under this process's dispatcher.
    ///
    /// `effect` is the delegate's reply to the prepare or the consume. The
    /// read is performed only for the one reply that carries `execute`; any
    /// other reply is reconciled and nothing is sent to the vendor. `budget`
    /// is when the whole call must have answered. `caller` says whether
    /// whoever asked is still there when the content is ready.
    pub fn read(
        &self,
        owner: BotOwner,
        effect: &PrivateEffectView,
        caller: &dyn ReadCaller,
        budget: Instant,
    ) -> ReadOutcome {
        let me = process_dispatcher();
        if effect.dispatcher != me {
            return ReadOutcome::NotOurs;
        }
        let id: ObligationId = (effect.key.run, effect.key.operation_id);
        match (effect.stage, effect.disposition) {
            (PrivateEffectStage::Prepared, PrivateEffectDisposition::Execute) => {}
            (PrivateEffectStage::Prepared | PrivateEffectStage::Dispatched, _) => {
                return self.reconcile_open(effect, id);
            }
            _ => return told_by(effect),
        }
        {
            let mut obligations = lock(&self.obligations);
            if obligations.contains_key(&id) {
                return ReadOutcome::InProgress;
            }
            let now = self.ports.clock.now();
            obligations.insert(
                id,
                Obligation {
                    key: effect.key.clone(),
                    deadline: budget,
                    phase: Phase::Registered,
                    pending: None,
                    abandoned: false,
                    backoff: ADVANCE_BACKOFF_FIRST,
                    next_attempt: now,
                },
            );
        }
        let mut abandon = Abandon {
            obligations: &self.obligations,
            id,
            armed: true,
        };
        let outcome = self.perform(owner, effect, id, caller, budget);
        abandon.armed = false;
        drop(abandon);
        // A rotated credential that could not be stored: the read went
        // ahead with the token it had, and the connection now needs sign-in.
        let lapsed: Vec<_> = lock(&self.lapsed).drain(..).collect();
        for (owner, connection) in lapsed {
            self.mark_needs_sign_in(owner, connection);
        }
        outcome
    }

    /// A replay of an open row of this process. If a call here is performing
    /// it, that is all there is to say. If none is, the reply that carried
    /// its license was lost: a `prepared` row is refused as not dispatched,
    /// and a `dispatched` row, which this process cannot account for, is
    /// recorded as unknown.
    fn reconcile_open(&self, effect: &PrivateEffectView, id: ObligationId) -> ReadOutcome {
        if lock(&self.obligations).contains_key(&id) {
            return ReadOutcome::InProgress;
        }
        let transition = match effect.stage {
            PrivateEffectStage::Prepared => PrivateEffectTransition::Refused {
                code: PrivateEffectRefusal::NotDispatched,
            },
            _ => PrivateEffectTransition::Unknown {},
        };
        match self
            .ports
            .ledger
            .advance(&effect.key, process_dispatcher(), &transition)
        {
            Ok(view) => told_by(&view),
            // Closed by someone else, or the delegate did not answer: this
            // call sent nothing and can say nothing about the row.
            Err(LedgerError::Stale) => ReadOutcome::Unknown,
            Err(_) => ReadOutcome::InProgress,
        }
    }

    /// The bound connection and item as ai.db holds them now, or why the
    /// read cannot go ahead.
    fn bound(
        &self,
        owner: BotOwner,
        proposal: &PrivateOperationProposal,
    ) -> Result<(Connection, ConnectionItem), PrivateEffectRefusal> {
        use PrivateEffectRefusal as R;
        let (connection, item) = match self.ports.store.connection_item(owner, proposal.item) {
            Ok(Some(found)) => found,
            Ok(None) => return Err(R::ItemUnavailable),
            Err(ConnectionError::Forbidden) => return Err(R::Revoked),
            Err(_) => return Err(R::Unavailable),
        };
        if connection.id != proposal.connection {
            return Err(R::ItemUnavailable);
        }
        match connection.state {
            ConnectionState::Connected => {}
            ConnectionState::NeedsSignIn => return Err(R::CredentialExpired),
            ConnectionState::Disconnecting | ConnectionState::Disconnected => {
                return Err(R::Revoked);
            }
        }
        if connection.revision != proposal.connection_revision.get() {
            return Err(R::Revoked);
        }
        if &connection.account_subject_digest != proposal.account.as_bytes() {
            return Err(R::WrongAccount);
        }
        if &item.item_ref_digest != proposal.item_ref.as_bytes() {
            return Err(R::ItemUnavailable);
        }
        if item.state != ConnectionItemState::Available {
            return Err(R::DestinationChanged);
        }
        Ok((connection, item))
    }

    /// An access token for the connection: the cached one, or a refresh.
    /// Holding the connection's cell is the single flight: a second read
    /// waits here and finds the token the first one fetched.
    fn access_token(
        &self,
        owner: BotOwner,
        connection: &Connection,
        deadline: Instant,
    ) -> Result<SecretString, PrivateEffectRefusal> {
        use PrivateEffectRefusal as R;
        let cell = self.access_cell(connection.id);
        let mut cached = lock(&cell);
        let now = self.ports.clock.now();
        if now >= deadline {
            return Err(R::Unavailable);
        }
        if let Some(token) = cached.as_ref() {
            if token.expires_at > now + ACCESS_MARGIN {
                return Ok(token.secret.clone());
            }
        }
        *cached = None;
        // The slot this connection was made with. A different digest means
        // another root, channel or device: the credential is not here, and
        // another slot is never read in its place.
        let slot = self
            .slot(connection.id)
            .filter(|slot| credential_slot_digest(slot) == connection.credential_slot_digest);
        let Some(slot) = slot else {
            drop(cached);
            self.mark_needs_sign_in(owner, connection.id);
            return Err(R::CredentialMissing);
        };
        let refresh = match self.ports.credentials.load(&slot) {
            // The store could not be read. The connection is not changed.
            Err(_) => return Err(R::CredentialLocked),
            Ok(None) => {
                drop(cached);
                self.mark_needs_sign_in(owner, connection.id);
                return Err(R::CredentialMissing);
            }
            Ok(Some(refresh)) => refresh,
        };
        match self.ports.vendor.refresh(&refresh, deadline) {
            Ok(refreshed) => {
                if let Some(rotated) = refreshed.rotated_refresh {
                    if self.ports.credentials.store(&slot, rotated).is_err() {
                        lock(&self.lapsed).push((owner, connection.id));
                    }
                }
                let _ = self
                    .ports
                    .store
                    .refreshed(owner, connection.id, self.wall());
                let secret = refreshed.access.secret.clone();
                *cached = Some(refreshed.access);
                Ok(secret)
            }
            Err(VendorError::InvalidGrant) => {
                drop(cached);
                self.mark_needs_sign_in(owner, connection.id);
                Err(R::CredentialExpired)
            }
            Err(
                VendorError::Unavailable | VendorError::Unconfirmed | VendorError::NotConfigured,
            ) => Err(R::Unavailable),
            Err(_) => Err(R::VendorRejected),
        }
    }

    fn set_phase(&self, id: ObligationId, phase: Phase) {
        if let Some(obligation) = lock(&self.obligations).get_mut(&id) {
            obligation.phase = phase;
        }
    }

    /// Send one advance, repeating it on the back-off while the delegate
    /// does not answer and the call still has time.
    fn advance_within(
        &self,
        key: &PrivateEffectKey,
        transition: &PrivateEffectTransition,
        budget: Instant,
    ) -> Result<PrivateEffectView, LedgerError> {
        let mut wait = ADVANCE_BACKOFF_FIRST;
        loop {
            match self
                .ports
                .ledger
                .advance(key, process_dispatcher(), transition)
            {
                Err(LedgerError::Unavailable) => {
                    if self.ports.clock.now() + wait >= budget {
                        return Err(LedgerError::Unavailable);
                    }
                    self.ports.clock.sleep(wait);
                    wait = (wait * 2).min(ADVANCE_BACKOFF_MAX);
                }
                answered => return answered,
            }
        }
    }

    fn perform(
        &self,
        owner: BotOwner,
        effect: &PrivateEffectView,
        id: ObligationId,
        caller: &dyn ReadCaller,
        budget: Instant,
    ) -> ReadOutcome {
        use PrivateEffectRefusal as R;
        let key = &effect.key;
        let proposal = &effect.operation;
        let vendor_deadline = (self.ports.clock.now() + VENDOR_READ_DEADLINE).min(budget);

        // Step 2: the credential. A failure here ends in `refused` from
        // `prepared`: nothing was sent that performs the operation.
        let (connection, _) = match self.bound(owner, proposal) {
            Ok(found) => found,
            Err(code) => return self.settle(id, key, refused(code), None, caller, budget),
        };
        let access = match self.access_token(owner, &connection, vendor_deadline) {
            Ok(access) => access,
            Err(code) => return self.settle(id, key, refused(code), None, caller, budget),
        };
        // Reread before the mark: nothing changed while the token was fetched.
        let item = match self.bound(owner, proposal) {
            Ok((_, item)) => item,
            Err(code) => return self.settle(id, key, refused(code), None, caller, budget),
        };
        let Some(reference) = VendorItemRef::new(item.item_ref.clone()) else {
            return self.settle(id, key, refused(R::ItemUnavailable), None, caller, budget);
        };

        // Step 3: the dispatch mark. The vendor is called only after a
        // reply whose stage is `dispatched`.
        self.set_phase(id, Phase::MarkSent);
        match self.advance_within(key, &PrivateEffectTransition::Dispatched {}, budget) {
            Ok(view) if view.stage == PrivateEffectStage::Dispatched => {
                self.set_phase(id, Phase::Dispatched);
            }
            Ok(view) => {
                // The ledger answered with a final row: a bound barrier
                // moved and it recorded `refused/access_denied`. A reply,
                // not a failure. Nothing was sent.
                lock(&self.obligations).remove(&id);
                return told_by(&view);
            }
            Err(LedgerError::Stale) => {
                lock(&self.obligations).remove(&id);
                return ReadOutcome::Unknown;
            }
            // Not answered, or refused with nothing committed. The vendor
            // was not called, whatever became of the mark; `refused` is
            // legal from `prepared` and from `dispatched`.
            Err(_) => return self.settle(id, key, refused(R::Unavailable), None, caller, budget),
        }

        // Step 4: the vendor call. The adapter checks the item's identity
        // before it reads any content (D17).
        let (transition, content) = match self.ports.vendor.read_item(
            &access,
            &reference,
            &ItemIdentityDigest(item.identity_digest),
            READ_LIMIT_BYTES,
            vendor_deadline,
        ) {
            Ok(content) => {
                let content = PrivateReadContent::new(content);
                // Reread once more: a revision that moved while the call was
                // in flight (a Disconnect, a sign-in lapse) withholds it.
                let withheld = self.bound(owner, proposal).is_err();
                let bytes = u32::try_from(content.text.len()).unwrap_or(u32::MAX);
                (
                    PrivateEffectTransition::Confirmed {
                        outcome_bytes: bytes,
                        withheld,
                    },
                    (!withheld).then_some(content),
                )
            }
            Err(VendorError::IdentityChanged) => {
                // No content was read. The person picks the item again,
                // which is the fresh grant.
                if let Ok(changed) = self.ports.store.item_changed(owner, item.id, self.wall()) {
                    self.notify(&changed, ConnectionChange::ItemsChanged);
                }
                (refused(R::DestinationChanged), None)
            }
            Err(VendorError::NotFound | VendorError::Forbidden) => {
                (refused(R::ItemUnavailable), None)
            }
            Err(VendorError::InvalidGrant) => {
                self.mark_needs_sign_in(owner, connection.id);
                (refused(R::CredentialExpired), None)
            }
            Err(VendorError::Unauthorized) => {
                // The access token was refused. It is dropped, so the next
                // read refreshes; this one is not repeated.
                self.forget_access(connection.id);
                (refused(R::VendorRejected), None)
            }
            Err(VendorError::Unavailable) => (refused(R::Unavailable), None),
            // The request left and nothing trustworthy came back. Final.
            Err(VendorError::Unconfirmed) => (PrivateEffectTransition::Unknown {}, None),
            Err(_) => (refused(R::VendorRejected), None),
        };

        // Step 5: the receipt.
        self.settle(id, key, transition, content, caller, budget)
    }

    /// Record the truth of one effect and say what the caller is told.
    fn settle(
        &self,
        id: ObligationId,
        key: &PrivateEffectKey,
        transition: PrivateEffectTransition,
        content: Option<PrivateReadContent>,
        caller: &dyn ReadCaller,
        budget: Instant,
    ) -> ReadOutcome {
        if let Some(obligation) = lock(&self.obligations).get_mut(&id) {
            obligation.pending = Some(transition);
        }
        match self.advance_within(key, &transition, budget) {
            Ok(view) => {
                lock(&self.obligations).remove(&id);
                match transition {
                    PrivateEffectTransition::Confirmed { withheld: true, .. } => {
                        ReadOutcome::Withheld
                    }
                    PrivateEffectTransition::Confirmed { .. } => match content {
                        // Content leaves this function only here: the reply
                        // said `deliver`, and the caller is still there.
                        Some(content) if view.deliver => {
                            if caller.gone() {
                                ReadOutcome::CallerGone
                            } else {
                                ReadOutcome::Delivered(content)
                            }
                        }
                        // Access was removed while the read was in flight.
                        _ => ReadOutcome::Withheld,
                    },
                    PrivateEffectTransition::Refused { code } => ReadOutcome::Refused(code),
                    _ => ReadOutcome::Unknown,
                }
            }
            // Someone else closed the row. Whatever was read is dropped.
            Err(LedgerError::Stale | LedgerError::Refused) => {
                lock(&self.obligations).remove(&id);
                ReadOutcome::Unknown
            }
            Err(LedgerError::Unavailable) => {
                // The call has to answer before the receipt is recorded.
                // What it tells its caller is what it records: a read that
                // may have happened becomes unknown, never confirmed, and
                // its content is dropped here.
                let (told, outcome) = match transition {
                    PrivateEffectTransition::Refused { code } => {
                        (transition, ReadOutcome::Refused(code))
                    }
                    _ => (PrivateEffectTransition::Unknown {}, ReadOutcome::Unknown),
                };
                let now = self.ports.clock.now();
                if let Some(obligation) = lock(&self.obligations).get_mut(&id) {
                    obligation.pending = Some(told);
                    obligation.backoff = ADVANCE_BACKOFF_FIRST;
                    obligation.next_attempt = now;
                }
                outcome
            }
        }
    }

    // ------------------------------------------------------------- drive

    /// The service's own upkeep, called by its host on a timer. It never
    /// calls a vendor. It ends consent trips nobody finished in time, sends
    /// again each receipt the delegate has not acknowledged (on the back-off,
    /// for as long as the process lives), and finishes an obligation whose
    /// call ended without settling it once that call's deadline has passed:
    /// not dispatched if the mark was never sent, unknown otherwise. Returns
    /// how many things it did.
    pub fn drive(&self) -> usize {
        let now = self.ports.clock.now();
        let mut done = 0;
        let expired: Vec<(BotRequestId, BotOwner)> = {
            let mut trips = lock(&self.trips);
            let expired: Vec<_> = trips
                .iter()
                .filter(|(_, trip)| {
                    now > trip.deadline && !trip.finishing.load(std::sync::atomic::Ordering::SeqCst)
                })
                .map(|(request, trip)| (*request, trip.owner))
                .collect();
            for (request, _) in &expired {
                trips.remove(request);
            }
            expired
        };
        for (request, owner) in expired {
            let _ = self.ports.store.fail_command(
                owner,
                request,
                ConnectionFailure::TimedOut,
                self.wall(),
            );
            done += 1;
        }
        let due: Vec<(
            ObligationId,
            PrivateEffectKey,
            PrivateEffectTransition,
            bool,
        )> = lock(&self.obligations)
            .iter()
            .filter(|(_, obligation)| obligation.next_attempt <= now)
            .filter_map(|(id, obligation)| {
                let (transition, guessed) = match (obligation.pending, obligation.abandoned) {
                    (Some(transition), _) => (transition, false),
                    (None, true) if now >= obligation.deadline => (
                        match obligation.phase {
                            Phase::Registered => refused(PrivateEffectRefusal::NotDispatched),
                            Phase::MarkSent | Phase::Dispatched => {
                                PrivateEffectTransition::Unknown {}
                            }
                        },
                        obligation.phase == Phase::MarkSent,
                    ),
                    // A live call still owns it.
                    _ => return None,
                };
                Some((*id, obligation.key.clone(), transition, guessed))
            })
            .collect();
        for (id, key, transition, guessed) in due {
            let answer = self
                .ports
                .ledger
                .advance(&key, process_dispatcher(), &transition);
            let mut obligations = lock(&self.obligations);
            let Some(obligation) = obligations.get_mut(&id) else {
                continue;
            };
            done += 1;
            match answer {
                Err(LedgerError::Unavailable) => {
                    obligation.next_attempt = now + obligation.backoff;
                    obligation.backoff = (obligation.backoff * 2).min(ADVANCE_BACKOFF_MAX);
                }
                // The mark's reply was never seen, and `unknown` was not
                // legal: the row is still `prepared`. Say so.
                Err(LedgerError::Stale) if guessed => {
                    obligation.phase = Phase::Registered;
                }
                _ => {
                    obligations.remove(&id);
                }
            }
        }
        done
    }
}

fn refused(code: PrivateEffectRefusal) -> PrivateEffectTransition {
    PrivateEffectTransition::Refused { code }
}
