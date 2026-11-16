// SPDX-License-Identifier: MPL-2.0
//! The service against the scripted vendor, the scripted effect ledger, the
//! in-memory credential store, and the real ai.db writer on an in-memory
//! database. No socket, no browser, no OS keychain, no real vendor.

mod flows;
mod read;
mod surface;

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use koru_ai_core::bots::{BotOwner, BotRequestId, DeviceId, UserScopeId};
use koru_ai_core::connections::*;
use koru_ai_sqlite::Repository;
use koru_delegate_core::decisions::{DecisionDigest, DecisionRevision};
use koru_delegate_core::{
    PrivateEffectView, PrivateOperationProposal, private_request_fingerprint,
};
use koru_oauth_credentials::{CredentialContext, CredentialEntry};

use crate::support::*;
use crate::*;

pub(crate) const SUBJECT_A: &str = "subject-a";
pub(crate) const SUBJECT_B: &str = "subject-b";
pub(crate) const REDIRECT: &str = "http://127.0.0.1:49152/callback";
pub(crate) const CONTENT_1: &str = "MARK-CONTENT of the first file";

/// The real ai.db writer behind the service's port. Two switches make it
/// fail where no real database would on demand.
pub(crate) struct SqlStore {
    pub(crate) repository: Mutex<Repository>,
    pub(crate) fail_complete: AtomicBool,
    pub(crate) panic_on_item: AtomicBool,
}
impl SqlStore {
    fn new(repository: Repository) -> Self {
        Self {
            repository: Mutex::new(repository),
            fail_complete: AtomicBool::new(false),
            panic_on_item: AtomicBool::new(false),
        }
    }
    pub(crate) fn with<T>(&self, read: impl FnOnce(&mut Repository) -> T) -> T {
        read(
            &mut self
                .repository
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        )
    }
}
impl ConnectionStore for SqlStore {
    fn connection(
        &self,
        owner: BotOwner,
        id: ConnectionId,
    ) -> Result<Option<Connection>, ConnectionError> {
        self.with(|r| r.connection_by_id(owner, id))
    }
    fn live_connections(&self, owner: BotOwner) -> Result<Vec<Connection>, ConnectionError> {
        self.with(|r| r.live_connections(owner))
    }
    fn connection_item(
        &self,
        owner: BotOwner,
        id: ConnectionItemId,
    ) -> Result<Option<(Connection, ConnectionItem)>, ConnectionError> {
        if self.panic_on_item.load(Ordering::SeqCst) {
            panic!("the scripted store panicked");
        }
        self.with(|r| r.connection_item(owner, id))
    }
    fn receipt(
        &self,
        owner: BotOwner,
        request: BotRequestId,
    ) -> Result<Option<ConnectionCommandReceipt>, ConnectionError> {
        self.with(|r| r.connection_receipt(owner, request))
    }
    fn prepared_commands(
        &self,
        after: i64,
        limit: usize,
    ) -> Result<Vec<(i64, ConnectionCommandReceipt)>, ConnectionError> {
        self.with(|r| r.prepared_connection_commands(after, limit))
    }
    fn begin_connect(
        &self,
        owner: BotOwner,
        request: BotRequestId,
        fingerprint: &str,
        planned: ConnectionId,
        name: &str,
        now_ms: i64,
    ) -> Result<ConnectionCommandAdmission, ConnectionError> {
        self.with(|r| r.begin_connect(owner, request, fingerprint, planned, name, now_ms))
    }
    fn begin_sign_in_again(
        &self,
        owner: BotOwner,
        request: BotRequestId,
        fingerprint: &str,
        connection: ConnectionId,
        expected_revision: u64,
        now_ms: i64,
    ) -> Result<ConnectionCommandAdmission, ConnectionError> {
        self.with(|r| {
            r.begin_sign_in_again(
                owner,
                request,
                fingerprint,
                connection,
                expected_revision,
                now_ms,
            )
        })
    }
    fn complete_connect(
        &self,
        owner: BotOwner,
        request: BotRequestId,
        account: &ConnectedAccount,
        now_ms: i64,
    ) -> Result<Connection, ConnectionError> {
        if self.fail_complete.load(Ordering::SeqCst) {
            return Err(ConnectionError::Storage);
        }
        self.with(|r| r.complete_connect(owner, request, account, now_ms))
    }
    fn complete_sign_in_again(
        &self,
        owner: BotOwner,
        request: BotRequestId,
        account: &ConnectedAccount,
        now_ms: i64,
    ) -> Result<Connection, ConnectionError> {
        self.with(|r| r.complete_sign_in_again(owner, request, account, now_ms))
    }
    fn begin_add_items(
        &self,
        owner: BotOwner,
        request: BotRequestId,
        fingerprint: &str,
        connection: ConnectionId,
        expected_revision: u64,
        now_ms: i64,
    ) -> Result<ConnectionCommandAdmission, ConnectionError> {
        self.with(|r| {
            r.begin_add_items(
                owner,
                request,
                fingerprint,
                connection,
                expected_revision,
                now_ms,
            )
        })
    }
    fn complete_add_items(
        &self,
        owner: BotOwner,
        request: BotRequestId,
        items: &[PickedConnectionItem],
        now_ms: i64,
    ) -> Result<Connection, ConnectionError> {
        self.with(|r| r.complete_add_items(owner, request, items, now_ms))
    }
    fn begin_disconnect(
        &self,
        owner: BotOwner,
        request: BotRequestId,
        fingerprint: &str,
        connection: ConnectionId,
        expected_revision: u64,
        now_ms: i64,
    ) -> Result<(ConnectionCommandAdmission, Connection), ConnectionError> {
        self.with(|r| {
            r.begin_disconnect(
                owner,
                request,
                fingerprint,
                connection,
                expected_revision,
                now_ms,
            )
        })
    }
    fn complete_disconnect(
        &self,
        owner: BotOwner,
        request: BotRequestId,
        revoke: VendorRevoke,
        now_ms: i64,
    ) -> Result<Connection, ConnectionError> {
        self.with(|r| r.complete_disconnect(owner, request, revoke, now_ms))
    }
    fn fail_command(
        &self,
        owner: BotOwner,
        request: BotRequestId,
        failure: ConnectionFailure,
        now_ms: i64,
    ) -> Result<ConnectionCommandReceipt, ConnectionError> {
        self.with(|r| r.fail_connection_command(owner, request, failure, now_ms))
    }
    fn needs_sign_in(
        &self,
        owner: BotOwner,
        connection: ConnectionId,
        now_ms: i64,
    ) -> Result<Connection, ConnectionError> {
        self.with(|r| r.connection_needs_sign_in(owner, connection, now_ms))
    }
    fn refreshed(
        &self,
        owner: BotOwner,
        connection: ConnectionId,
        now_ms: i64,
    ) -> Result<(), ConnectionError> {
        self.with(|r| r.connection_refreshed(owner, connection, now_ms))
    }
    fn item_changed(
        &self,
        owner: BotOwner,
        item: ConnectionItemId,
        now_ms: i64,
    ) -> Result<Connection, ConnectionError> {
        self.with(|r| r.connection_item_changed(owner, item, now_ms))
    }
}

#[derive(Default)]
pub(crate) struct RecordedChanges(Mutex<Vec<ConnectionChanged>>);
impl RecordedChanges {
    pub(crate) fn all(&self) -> Vec<ConnectionChanged> {
        self.0.lock().unwrap().clone()
    }
    pub(crate) fn kinds(&self) -> Vec<ConnectionChange> {
        self.all().iter().map(|change| change.change).collect()
    }
}
impl ConnectionChangeSink for RecordedChanges {
    fn connection_changed(&self, change: ConnectionChanged) {
        self.0.lock().unwrap().push(change);
    }
}

pub(crate) struct Gone(pub(crate) bool);
impl ReadCaller for Gone {
    fn gone(&self) -> bool {
        self.0
    }
}

pub(crate) struct Harness {
    pub(crate) clock: Arc<FakeClock>,
    pub(crate) journal: Journal,
    pub(crate) vendor: Arc<FakeVendor>,
    pub(crate) credentials: Arc<LockableCredentials>,
    pub(crate) store: Arc<SqlStore>,
    pub(crate) ledger: Arc<FakeLedger>,
    pub(crate) changes: Arc<RecordedChanges>,
    pub(crate) context: CredentialContext,
    pub(crate) owner: BotOwner,
    pub(crate) service: Arc<ConnectionService>,
}

pub(crate) fn context(root: &str, owner: BotOwner) -> CredentialContext {
    // The root is a name, never opened: the context reads no file.
    CredentialContext::new("nightly", Path::new(root), owner.device).unwrap()
}

impl Harness {
    pub(crate) fn new() -> Self {
        Self::over(Repository::in_memory().unwrap())
    }
    pub(crate) fn over(repository: Repository) -> Self {
        let clock = Arc::new(FakeClock::default());
        let journal = Journal::default();
        let vendor = Arc::new(FakeVendor::new(clock.clone(), journal.clone()));
        vendor.add_account(SUBJECT_A, "MARK-LABEL-a@example.test");
        vendor.add_account(SUBJECT_B, "MARK-LABEL-b@example.test");
        vendor.add_item("doc-1", SUBJECT_A, "MARK-TITLE-1", CONTENT_1);
        vendor.add_item("doc-2", SUBJECT_A, "MARK-TITLE-2", "MARK-CONTENT two");
        vendor.add_item("doc-b", SUBJECT_B, "MARK-TITLE-b", "MARK-CONTENT b");
        let owner = BotOwner {
            user_scope: UserScopeId::new(),
            device: DeviceId::from_uuid(uuid::Uuid::new_v4()),
        };
        let context = context("/isolated/first-root", owner);
        let credentials = Arc::new(LockableCredentials::default());
        let store = Arc::new(SqlStore::new(repository));
        let ledger = Arc::new(FakeLedger::new(journal.clone()));
        let changes = Arc::new(RecordedChanges::default());
        let service = Arc::new(ConnectionService::new(ConnectionPorts {
            vendor: vendor.clone(),
            credentials: credentials.clone(),
            context: context.clone(),
            store: store.clone(),
            ledger: ledger.clone(),
            clock: clock.clone(),
            changes: changes.clone(),
        }));
        Self {
            clock,
            journal,
            vendor,
            credentials,
            store,
            ledger,
            changes,
            context,
            owner,
            service,
        }
    }
    /// A second service over the same world: a later process, or the same
    /// installation seen from another credential context.
    pub(crate) fn another_service(&self, context: CredentialContext) -> ConnectionService {
        ConnectionService::new(ConnectionPorts {
            vendor: self.vendor.clone(),
            credentials: self.credentials.clone(),
            context,
            store: self.store.clone(),
            ledger: self.ledger.clone(),
            clock: self.clock.clone(),
            changes: self.changes.clone(),
        })
    }

    pub(crate) fn begin(&self, name: &str) -> (BotRequestId, AuthorizationRequest) {
        let request = BotRequestId::mint();
        match self
            .service
            .begin_connect(self.owner, request, name, REDIRECT)
            .unwrap()
        {
            TripStart::Consent { consent, .. } => (request, consent),
            other => panic!("the connect did not begin: {other:?}"),
        }
    }
    /// Connect `subject` under `name`, picking `picked`.
    pub(crate) fn connect(&self, name: &str, subject: &str, picked: &[&str]) -> Connection {
        let (request, consent) = self.begin(name);
        let receipt = self
            .service
            .complete_trip(
                self.owner,
                request,
                self.vendor.approve(&consent, subject, picked),
            )
            .unwrap();
        assert_eq!(
            receipt.stage,
            ConnectionReceiptStage::Completed,
            "{receipt:?}"
        );
        self.connection(receipt.connection.unwrap())
    }
    pub(crate) fn connection(&self, id: ConnectionId) -> Connection {
        self.store
            .with(|r| r.connection_by_id(self.owner, id))
            .unwrap()
            .unwrap()
    }
    pub(crate) fn items(&self, id: ConnectionId) -> Vec<ConnectionItem> {
        self.store
            .with(|r| r.connection_items(self.owner, id))
            .unwrap()
    }
    pub(crate) fn item(&self, id: ConnectionId, reference: &str) -> ConnectionItem {
        self.items(id)
            .into_iter()
            .find(|item| item.item_ref == reference)
            .unwrap_or_else(|| panic!("no picked item {reference}"))
    }
    pub(crate) fn receipt(&self, request: BotRequestId) -> ConnectionCommandReceipt {
        self.store
            .with(|r| r.connection_receipt(self.owner, request))
            .unwrap()
            .unwrap()
    }
    pub(crate) fn slot(&self, id: ConnectionId) -> CredentialEntry {
        connection_slot(&self.context, FAKE_ORIGIN, id).unwrap()
    }
    /// The refresh token in a connection's slot, read around any lock.
    pub(crate) fn stored(&self, id: ConnectionId) -> Option<String> {
        self.credentials
            .peek(&self.slot(id))
            .map(|secret| secret.expose().to_owned())
    }
    /// The proposal the AI owner would plan for this item now.
    pub(crate) fn proposal(&self, id: ConnectionId, reference: &str) -> PrivateOperationProposal {
        let connection = self.connection(id);
        let item = self.item(id, reference);
        let item_ref = DecisionDigest::from_bytes(item.item_ref_digest);
        PrivateOperationProposal {
            operation_id: uuid::Uuid::new_v4(),
            connection: id,
            connection_revision: DecisionRevision::new(connection.revision).unwrap(),
            account: DecisionDigest::from_bytes(connection.account_subject_digest),
            item: item.id,
            item_ref,
            operation: PrivateOperationKind::ReadItem,
            request_fingerprint: private_request_fingerprint(
                PrivateOperationKind::ReadItem,
                id,
                item.id,
                item_ref,
            ),
        }
    }
    /// The delegate's `execute` reply for a read of this item.
    pub(crate) fn open(&self, id: ConnectionId, reference: &str) -> PrivateEffectView {
        self.ledger
            .open(self.proposal(id, reference), process_dispatcher())
    }
    pub(crate) fn budget(&self) -> Instant {
        self.clock.now() + Duration::from_secs(30)
    }
    pub(crate) fn read(&self, effect: &PrivateEffectView) -> ReadOutcome {
        self.service
            .read(self.owner, effect, &PresentCaller, self.budget())
    }
    /// Past the access token's life: the next read refreshes.
    pub(crate) fn expire_access(&self) {
        self.clock.advance(Duration::from_secs(2 * 3600));
    }
    pub(crate) fn text(outcome: ReadOutcome) -> String {
        match outcome {
            ReadOutcome::Delivered(content) => content.into_text(),
            other => panic!("nothing was delivered: {other:?}"),
        }
    }
}
