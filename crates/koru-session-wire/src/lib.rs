// SPDX-License-Identifier: MPL-2.0

//! Private IPC vocabulary shared by the session host and its direct clients.
//!
//! This crate describes frames only. It owns no socket, process, root, HTTP
//! behavior, window behavior, PTY, emulator, or reconnect loop. Interactive
//! bytes therefore cannot drift into `koru-agent-wire` by convenience.

use std::collections::BTreeMap;
use std::path::PathBuf;

use koru_session_protocol::{
    ApprovalDecision, ApprovalId, Attachment, AttachmentDeliveryPolicy, AttachmentId,
    AttachmentSource, CancelReason, CapabilitySet, CatalogEvent, ControlRequestOutcome,
    ControllerLease, ControllerLeaseToken, CreationLookup, DelegationLaunch, GridSize,
    IdempotencyKey, ImageDimensions, PendingInput, ProfileKind, ProjectId, ProjectSummary,
    ProtocolAgreement, ProtocolOffer, ProtocolRefusal, RawOutputChunk, RenderSnapshot,
    RenderUpdate, RowRange, ScrollbackPage, SendPendingInputResult, SequencedAgentEvent,
    SequencedSessionEvent, SessionCatalogGeneration, SessionCommand, SessionFailure, SessionId,
    SessionMode, SessionObservation, SessionProfileKey, SessionSummary, TakeoverUrgency,
    TerminalGeneration, ViewportGeneration, ViewportRequest, WorkspaceScope,
};
use serde::{Deserialize, Serialize};

/// The JSON frame shape. Protocol negotiation happens inside this readable
/// frame, so an incompatible protocol can return a stable refusal.
pub const FRAME_VERSION: u8 = 1;
pub const HANDSHAKE_TIMEOUT_MS: u64 = 5_000;

/// One Agent-to-host launch capability, delivered through the child's stdin.
/// The exact resolved root and sessions directory are data, never rediscovered
/// by the host. The authority is required for the ordered stop request and is
/// deliberately redacted from `Debug`.
///
/// **Two secrets, not one.** `authority` stops the host; `lifecycle_authority`
/// only closes the sessions one Koru workspace owns. The Agent mints both and
/// hands the narrow one on to the supervised Gateway, which is why they cannot
/// be the same value: a Gateway that could stop the session host would be able
/// to end every terminal on the machine by asking for a workspace nobody owns.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchBootstrap {
    pub launch_id: String,
    pub segment: String,
    pub root: PathBuf,
    pub sessions_directory: PathBuf,
    pub store_directory: PathBuf,
    pub explicit_root: bool,
    authority: String,
    /// Absent from a bootstrap written before SP-8a. A host that was handed
    /// none refuses every workspace-lifecycle request rather than accepting
    /// an empty string, which is what an older Agent would send.
    #[serde(default)]
    lifecycle_authority: String,
}

impl LaunchBootstrap {
    /// A bootstrap that stops the host and grants no workspace lifecycle.
    ///
    /// The narrow secret is added by [`Self::with_lifecycle_authority`] rather
    /// than taken here, so a supervisor that has no session-lifecycle story —
    /// a rooted host started by a window's own test harness — cannot end up
    /// passing the stop authority twice by filling in an argument it did not
    /// mean to.
    pub fn new(
        launch_id: String,
        segment: String,
        root: PathBuf,
        sessions_directory: PathBuf,
        store_directory: PathBuf,
        explicit_root: bool,
        authority: String,
    ) -> Self {
        Self {
            launch_id,
            segment,
            root,
            sessions_directory,
            store_directory,
            explicit_root,
            authority,
            lifecycle_authority: String::new(),
        }
    }

    /// Add the narrow secret the workspace-lifecycle verb takes.
    #[must_use]
    pub fn with_lifecycle_authority(mut self, lifecycle_authority: String) -> Self {
        self.lifecycle_authority = lifecycle_authority;
        self
    }

    pub fn authority(&self) -> &str {
        &self.authority
    }

    /// The narrow secret the workspace-lifecycle verb takes. Empty means the
    /// supervisor minted none, and an empty presented authority never matches.
    pub fn lifecycle_authority(&self) -> &str {
        &self.lifecycle_authority
    }
}

impl std::fmt::Debug for LaunchBootstrap {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LaunchBootstrap")
            .field("launch_id", &self.launch_id)
            .field("segment", &self.segment)
            .field("root", &self.root)
            .field("sessions_directory", &self.sessions_directory)
            .field("store_directory", &self.store_directory)
            .field("explicit_root", &self.explicit_root)
            .field("authority", &"[REDACTED]")
            .field("lifecycle_authority", &"[REDACTED]")
            .finish()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientHello {
    pub product: String,
    pub product_version: String,
    pub offer: ProtocolOffer,
    /// Protocol v16. What this connection is. `AiControl` asks the host for
    /// the next creation generation and is what admits chat-origin creates;
    /// absent is an ordinary client. Defaulted, so an older host reads the
    /// hello and simply never mints a generation — and a client that needs
    /// one checks the negotiated `session-origin` capability before relying
    /// on it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<koru_session_protocol::ClientRole>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostHello {
    pub product: String,
    pub product_version: String,
    pub instance_id: String,
    pub supervisor_launch_id: String,
    pub agreement: ProtocolAgreement,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Greeting {
    Welcome { hello: HostHello },
    Refused { refusal: ProtocolRefusal },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EnvironmentSpec {
    Inherit,
    Clean,
    InheritWith {
        #[serde(default)]
        set: BTreeMap<String, String>,
        #[serde(default)]
        remove: Vec<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateSession {
    pub profile: SessionProfileKey,
    #[serde(default)]
    pub parent_session_id: Option<SessionId>,
    pub profile_kind: ProfileKind,
    pub display_name: String,
    /// An already-resolved absolute path. The host validates it before spawn.
    pub working_directory: PathBuf,
    pub mode: SessionMode,
    #[serde(default)]
    pub attachment_delivery: AttachmentDeliveryPolicy,
    #[serde(default)]
    pub capabilities: CapabilitySet,
    /// Protocol v10. Placement happens inside creation, so no window ever
    /// shows a session that is briefly in no project; a peer that did not
    /// negotiate `projects` is refused rather than silently unplaced.
    #[serde(default)]
    pub project_id: Option<ProjectId>,
    /// Program followed by argv elements. No shell command is reparsed.
    pub argv: Vec<String>,
    pub environment: EnvironmentSpec,
    pub size: GridSize,
}

/// Profile-key creation used by surfaces that must not receive or resolve
/// executable argv. The Agent-owned host is the sole launch authority.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateProfileSession {
    pub idempotency_key: IdempotencyKey,
    pub profile: SessionProfileKey,
    pub parent_session_id: Option<SessionId>,
    /// Protocol v8. Omitted by older clients and raw callers.
    #[serde(default)]
    pub mode: SessionMode,
    /// Protocol v10. Atomic at create and replayed with the idempotency key
    /// above, so a retry re-reads the same placement instead of making a
    /// second one.
    #[serde(default)]
    pub project_id: Option<ProjectId>,
    /// Protocol v14. Present on a delegated creation and absent on a manual
    /// one. The host refuses it from a peer that did not negotiate
    /// `delegation`, because a provider spawned without its MCP endpoint
    /// would run and never reach its tools — and a v13 host would have
    /// ignored the field and done exactly that, which is why the client
    /// checks its agreement before sending one.
    #[serde(default)]
    pub delegation: Option<DelegationLaunch>,
    /// Protocol v16. What creates this session. Defaulted to `Manual`, which
    /// is what every creation before v16 was. A `Chat` origin is admitted
    /// only from an `AiControl` connection whose generation is current, and
    /// only when the chat has nothing live or in flight — the host refuses
    /// it from a peer that did not negotiate `session-origin`, because a
    /// chat session nobody can look up again is a process nobody can end.
    #[serde(default)]
    pub origin: koru_session_protocol::SessionOrigin,
    /// Protocol v16. An explicit model and effort for a managed session,
    /// separate from a delegation's own launch. Refused from a peer that
    /// did not negotiate `launch-selection`: an older host would have read
    /// the creation, ignored the field and run the default, which is the one
    /// outcome an explicit choice exists to prevent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<koru_session_protocol::LaunchSelection>,
    /// Start a raw agent on its most recent conversation in
    /// `working_directory` (the `launch-resume` capability). Raw agent
    /// profiles with a continue form only; refused otherwise, and from a peer
    /// that did not negotiate the capability.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub resume: bool,
    pub working_directory: PathBuf,
    pub size: GridSize,
}

/// One new project, carrying its own client-minted identity.
///
/// The id is the idempotency key: replaying this frame with the same id and
/// the same content returns the existing project, and with different content
/// is a conflict. There is no second key, because a project already has one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateProject {
    pub project_id: ProjectId,
    pub name: String,
    #[serde(default)]
    pub root_directory: Option<String>,
    #[serde(default)]
    pub workspace_scope: Option<WorkspaceScope>,
}

/// Image bytes offered to the rooted host. The host trusts no claimed media
/// type or filename: both are derived from decoded bytes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachmentUpload {
    pub session_id: SessionId,
    pub controller_lease: ControllerLeaseToken,
    pub attachment_id: AttachmentId,
    pub source: AttachmentSource,
    pub data_base64: String,
}

/// One explicit, bounded preview response. It is never a render subscription.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachmentPreview {
    pub attachment_id: AttachmentId,
    pub dimensions: ImageDimensions,
    pub media_type: String,
    pub data_base64: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SubscriptionId(pub u64);

impl SubscriptionId {
    pub fn new(value: u64) -> Option<Self> {
        (value > 0).then_some(Self(value))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Request {
    /// End only this client connection, releasing its controller lease while
    /// leaving every session process alive.
    Disconnect,
    Health,
    ListSessions,
    /// Generation-bearing catalogue for compare-and-consent clients. The
    /// legacy `ListSessions -> Sessions` shape remains unchanged for v1/v2.
    Catalog,
    /// Compare the active-set epoch and freeze creation while Agent closes UI
    /// clients. The launch authority makes this a lifecycle operation rather
    /// than an ordinary terminal command.
    PrepareShutdown {
        authority: String,
        observed_generation: SessionCatalogGeneration,
    },
    /// Undo a prepared shutdown after a window refuses or another gate aborts.
    CancelShutdown {
        authority: String,
    },
    CreateSession {
        create: CreateSession,
    },
    /// Protocol v6. The host resolves the profile and replays a matching
    /// idempotency key without spawning a second process.
    CreateProfileSession {
        create: CreateProfileSession,
    },
    IngestAttachment {
        upload: AttachmentUpload,
    },
    ListAttachments {
        session_id: SessionId,
    },
    GetAttachmentPreview {
        session_id: SessionId,
        attachment_id: AttachmentId,
        max_edge: u32,
    },
    RemoveAttachment {
        session_id: SessionId,
        controller_lease: ControllerLeaseToken,
        attachment_id: AttachmentId,
    },
    SendPendingInput {
        session_id: SessionId,
        controller_lease: ControllerLeaseToken,
        input: PendingInput,
    },
    /// Protocol v8 provider-neutral approval response.
    /// Requires negotiated provider-readiness; an old host cannot decode it.
    SubscribeProviders,
    DiscoverProviderModels {
        provider: koru_session_protocol::ProviderKind,
        working_directory: PathBuf,
    },
    RefreshProvider {
        #[serde(rename = "provider")]
        kind: koru_session_protocol::ProviderKind,
    },
    AnswerApproval {
        session_id: SessionId,
        controller_lease: ControllerLeaseToken,
        idempotency_key: IdempotencyKey,
        approval_id: ApprovalId,
        decision: ApprovalDecision,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        choice: Option<koru_session_protocol::ApprovalChoiceId>,
    },
    /// Protocol v9 turn-specific managed cancellation. A delayed request can
    /// never cancel a later active turn.
    CancelTurn {
        session_id: SessionId,
        controller_lease: ControllerLeaseToken,
        idempotency_key: IdempotencyKey,
        turn_id: koru_session_protocol::AgentTurnId,
        /// Protocol v14. A takeover boundary ends the turn for a handover and
        /// never the run (D36). Absent from an older peer, which reads `Run`.
        #[serde(default)]
        reason: CancelReason,
    },
    /// Protocol v9 sequence-safe acknowledgement that managed attention was
    /// actually presented. This never answers an approval.
    AcknowledgeAttention {
        session_id: SessionId,
        controller_lease: ControllerLeaseToken,
        through: koru_session_protocol::StreamSequence,
    },
    Attach {
        session_id: SessionId,
        viewport_generation: ViewportGeneration,
        columns: u16,
        overscan_rows: u16,
    },
    Detach {
        session_id: SessionId,
        lease: ControllerLeaseToken,
    },
    SuspendRender {
        session_id: SessionId,
        lease: ControllerLeaseToken,
    },
    SetViewport {
        session_id: SessionId,
        lease: ControllerLeaseToken,
        viewport: ViewportRequest,
    },
    RecoverSnapshot {
        session_id: SessionId,
        lease: ControllerLeaseToken,
    },
    /// Protocol v6 observer read; it neither acquires nor requires a lease.
    Snapshot {
        session_id: SessionId,
        viewport_generation: ViewportGeneration,
        range: RowRange,
        columns: u16,
    },
    /// Protocol v7 atomic observer recovery point. Unlike the legacy snapshot,
    /// it also returns the independent event and raw-output cursors.
    ObserveSnapshot {
        session_id: SessionId,
        viewport_generation: ViewportGeneration,
        range: RowRange,
        columns: u16,
    },
    /// Protocol v9 atomic retained managed-event tail and resume cursor.
    ObserveAgentEvents {
        session_id: SessionId,
    },
    ScrollbackPage {
        session_id: SessionId,
        lease: ControllerLeaseToken,
        terminal_generation: TerminalGeneration,
        viewport_generation: ViewportGeneration,
        range: RowRange,
    },
    /// Protocol v6 observer read; it neither acquires nor requires a lease.
    ObserveScrollbackPage {
        session_id: SessionId,
        terminal_generation: TerminalGeneration,
        viewport_generation: ViewportGeneration,
        range: RowRange,
    },
    Command {
        command: SessionCommand,
    },
    SubscribeEvents {
        session_id: SessionId,
    },
    /// Protocol v7 cursor-aware subscription. The host compares `after`
    /// atomically with installation and may emit an immediate gap.
    SubscribeEventsFrom {
        session_id: SessionId,
        after: Option<koru_session_protocol::StreamSequence>,
    },
    SubscribeOutput {
        session_id: SessionId,
    },
    /// Protocol v7 cursor-aware raw-output subscription.
    SubscribeOutputFrom {
        session_id: SessionId,
        after: Option<koru_session_protocol::StreamSequence>,
    },
    /// Protocol v8 retained managed-event stream. Raw sessions reject it.
    SubscribeAgentEventsFrom {
        session_id: SessionId,
        after: Option<koru_session_protocol::StreamSequence>,
    },
    /// Protocol v10 project catalogue. Ordering is not semantic: the surface
    /// sorts by name under its own collation, and there is no `sort_key`.
    ListProjects,
    /// Protocol v10. Replaying the same client-minted id is not a second
    /// project.
    CreateProject {
        create: CreateProject,
    },
    RenameProject {
        project_id: ProjectId,
        name: String,
    },
    /// Protocol v10. `root_directory: None` clears the folder; a project
    /// whose folder has gone missing refuses to launch rather than falling
    /// back to a directory nobody chose.
    SetProjectRoot {
        project_id: ProjectId,
        root_directory: Option<String>,
    },
    /// Protocol v13. **Make this one project carry this scope.**
    ///
    /// The orphan area's second resolution (ADR-264): a project whose workspace
    /// is not in the including-deleted catalog, or is there under another
    /// profile, is drawn in the missing-workspace area, and its two resolutions
    /// are *move it to a workspace* and *delete it*. Delete was
    /// [`Request::DeleteProject`] from v10; this is move, and until v13 the
    /// session protocol had no way to express it at all.
    ///
    /// **Not [`Request::RescopeWorkspaceProjects`], and not a spelling of it.**
    /// That verb is keyed on a *workspace*, writes only the *profile* half of
    /// every scope naming it, and is gated on the launch-scoped lifecycle
    /// authority a window does not hold — it is the repair a workspace changing
    /// owner needs, not a person moving one project. This one names a single
    /// project and the whole typed pair it should now carry, and it is
    /// window-callable under the ordinary `projects` capability because it
    /// mutates one row the caller can already rename, re-root and delete.
    ///
    /// **It names the destination, which is the whole of its idempotence.**
    /// "This project's scope is now this pair" is a statement about the state
    /// the registry should be in, so sending it twice changes nothing the
    /// second time and a replay after a crash lands on the same row rather than
    /// re-applying a remembered move. A verb carrying the *old* scope would be
    /// a diff, and a diff applied to state that has moved on is how a stale
    /// replay resurrects something.
    ///
    /// `workspace_scope: None` clears the scope, exactly as
    /// [`Request::SetProjectRoot`]'s `None` clears the folder: the field is an
    /// `Option` on [`ProjectSummary`], so a rewrite that could not write every
    /// value the field holds would have needed a second verb to say the rest.
    ///
    /// Answers with the project row, so a caller sees what it now is rather
    /// than assuming its own request landed unchanged.
    SetProjectScope {
        project_id: ProjectId,
        workspace_scope: Option<WorkspaceScope>,
    },
    /// Protocol v10. Refused while the project has running sessions.
    DeleteProject {
        project_id: ProjectId,
    },
    /// Protocol v10. Moving a session moves its whole subtree; a session
    /// whose parent lives in another project cannot be moved on its own.
    AssignProject {
        session_id: SessionId,
        project_id: Option<ProjectId>,
    },
    /// Protocol v10 named catalog subscription: a bounded invalidation stream
    /// every window holds, never a per-session render/event/output stream.
    SubscribeProjects,
    /// Protocol v11. **Close every session one Koru workspace owns.**
    ///
    /// Not a terminal command and not the lease-holder's `Terminate`: this
    /// ends *other* controllers' sessions because the workspace they belong to
    /// is being archived or deleted, which is a lifecycle decision no lease
    /// can express. It is therefore gated the way `StopHost` is — on a
    /// launch-scoped secret only the Agent's supervised children hold — and on
    /// the negotiated `workspace-lifecycle` capability.
    ///
    /// The scope is the typed `{ profile, workspace }` pair, resolved here
    /// against the project registry: a session is affected when the project it
    /// was placed in carries exactly this scope. A session in no project, or
    /// in a project with no scope, belongs to no workspace and is never closed
    /// by one being removed.
    ///
    /// Idempotent: closing none is `Ok` with an empty list.
    TerminateWorkspaceSessions {
        authority: String,
        scope: WorkspaceScope,
    },
    /// Protocol v12. **Make every project that names this workspace name the
    /// profile that now owns it.**
    ///
    /// The repair half of a workspace moving between profiles (SP-15a). A
    /// project's scope is a typed `{ profile, workspace }` pair; when the
    /// workspace's owner changes, every project naming it holds a pair whose
    /// profile half is no longer true, and the surface's derived visibility
    /// puts those projects in the missing-workspace area rather than lying.
    /// This is what makes them right again.
    ///
    /// **It names the destination and not the origin, which is what makes it
    /// idempotent.** "Every project scoped to this workspace now names this
    /// profile" is a statement about the state the registry should be in, so
    /// applying it twice changes nothing the second time, and applying it after
    /// a crash repairs whatever is left rather than replaying a remembered
    /// move. A verb naming the old profile would be a diff, and a diff applied
    /// to state that has moved on is how a stale replay resurrects something.
    ///
    /// Gated like [`Request::TerminateWorkspaceSessions`] — the same
    /// launch-scoped lifecycle authority, the same `workspace-lifecycle`
    /// capability — because it is the same authority doing the same job: no
    /// caller should be able to move somebody's projects between personas.
    ///
    /// Idempotent: repairing none is `Ok` with an empty list.
    RescopeWorkspaceProjects {
        authority: String,
        workspace: koru_session_protocol::CatalogWorkspaceId,
        profile: koru_session_protocol::CatalogProfileId,
    },
    /// Protocol v14. **Ask whether a creation happened, without causing one.**
    ///
    /// Retrying `create_profile_session` is not a question: on a key the
    /// replay ledger has evicted it falls through and spawns. This verb
    /// answers from the registry's creation ledger — a key reserved before
    /// its process is spawned, marked live when the session is inserted,
    /// remembered after it ends — and never creates, retires, or attaches.
    /// The answer carries the host's `instance_id` so a caller can tell "the
    /// same host saw nothing" from "a new host, so nothing survived".
    LookupCreation {
        idempotency_key: IdempotencyKey,
    },
    /// Protocol v14. **Fence a key before starting a fresh attempt.**
    ///
    /// A read alone cannot exclude the future: a create delayed in the
    /// host's blocking lane could still land after the caller decided
    /// nothing was running. Retiring the key first makes that late create a
    /// refusal (`creation_retired`) instead of a second provider. The answer
    /// is the lookup at the moment of retirement, so a caller that finds the
    /// key already live reattaches rather than spawning.
    RetireCreation {
        idempotency_key: IdempotencyKey,
    },
    /// Protocol v16. **What does the registry hold under this origin right
    /// now?** Live sessions and in-flight reservations, from the live
    /// registry only — never from the evictable creation history, which is
    /// why an empty answer means nothing is running rather than nothing was
    /// remembered. Never creates, never retires, never takes a lease.
    LookupOrigin {
        origin: koru_session_protocol::SessionOrigin,
    },
    /// Protocol v14. **End a delegated session because its run was
    /// cancelled — without taking the controller lease.**
    ///
    /// The lease belongs to whoever borrowed a step of the run; the session
    /// belongs to the run that created it. Cancelling a run must reach the
    /// session-owned process tree *including while a person holds control*
    /// (delegation §7.6), and a person who took control and walked away
    /// must not be able to hold a provider open by keeping the lease. Only
    /// a session created by a delegated launch answers this; anything else
    /// is `Forbidden`, so a manual managed profile and a raw terminal keep
    /// the lease rule exactly as they have it. Idempotent: a session that
    /// has already exited answers `Ok`.
    TerminateDelegated {
        session_id: SessionId,
    },
    /// Protocol v14. **Ask the controller's holder for the lease.**
    ///
    /// Release, not preemption: nothing takes a lease from its holder. If
    /// none is held the answer is `Available` and the caller attaches; if
    /// one is held the session's handover reads `Requested`, the holder is
    /// told through its event stream, and the caller waits for `Released`.
    /// `Now` cancels the active turn with `CancelReason::TakeoverBoundary`
    /// so the boundary the holder releases at arrives sooner — one rule, not
    /// a second branch.
    RequestControl {
        session_id: SessionId,
        #[serde(default)]
        urgency: TakeoverUrgency,
    },
    /// Protocol v14. The holder gives its lease up for a handover. Like a
    /// detach, but the session's handover reads `Released` and the next
    /// attach reads `Taken`, so a subscriber can tell a handover from a
    /// window closing.
    ReleaseControl {
        session_id: SessionId,
        lease: ControllerLeaseToken,
    },
    /// Protocol v14. The person gives the lease back. The handover reads
    /// `Returned` until the original holder attaches again.
    ReturnControl {
        session_id: SessionId,
        lease: ControllerLeaseToken,
    },
    /// Only the Agent supervisor knows this launch-scoped secret.
    StopHost {
        authority: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestFrame {
    pub id: u64,
    pub request: Request,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostHealth {
    pub instance_id: String,
    pub supervisor_launch_id: String,
    pub session_count: u32,
    pub controlled_count: u32,
    pub catalog_generation: SessionCatalogGeneration,
    pub uptime_ms: u64,
}

/// Content-free active-set fact used by update and quit consent. Summaries are
/// metadata only; terminal bytes and grids never enter this path.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionCatalog {
    pub generation: SessionCatalogGeneration,
    pub sessions: Vec<SessionSummary>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Response {
    ProviderModels {
        models: koru_session_protocol::ProviderModels,
    },
    Health {
        health: HostHealth,
    },
    Sessions {
        sessions: Vec<SessionSummary>,
    },
    Catalog {
        catalog: SessionCatalog,
    },
    Created {
        session: SessionSummary,
    },
    AttachmentIngested {
        attachment: Attachment,
    },
    Attachments {
        attachments: Vec<Attachment>,
    },
    AttachmentPreview {
        preview: AttachmentPreview,
    },
    AttachmentRemoved {
        attachment: Attachment,
    },
    PendingInputSent {
        result: SendPendingInputResult,
    },
    Attached {
        lease: ControllerLease,
        subscription_id: SubscriptionId,
    },
    Subscribed {
        subscription_id: SubscriptionId,
    },
    ScrollbackPage {
        page: ScrollbackPage,
    },
    Snapshot {
        snapshot: RenderSnapshot,
    },
    ObservedSnapshot {
        observation: SessionObservation,
    },
    ProvidersSubscribed {
        subscription_id: SubscriptionId,
        providers: Vec<koru_session_protocol::ProviderReadiness>,
    },
    AgentEventsObserved {
        observation: koru_session_protocol::AgentEventObservation,
    },
    Projects {
        projects: Vec<ProjectSummary>,
    },
    Project {
        project: ProjectSummary,
    },
    /// Protocol v11. Which sessions this call closed — the ids, not a count,
    /// because a caller reporting "three terminals were closed" after a retry
    /// that closed none would be reporting the first attempt's truth. A retry
    /// answers with an empty list, which is the honest second answer.
    WorkspaceSessionsTerminated {
        terminated: Vec<SessionId>,
    },
    /// Protocol v12. Which projects this call repaired — the ids, not a count,
    /// for [`Response::WorkspaceSessionsTerminated`]'s reason. A repair that
    /// found everything already right answers with an empty list, which is the
    /// honest answer and not a failure.
    WorkspaceProjectsRescoped {
        rescoped: Vec<ProjectId>,
    },
    /// Protocol v14. The non-creating lookup's answer, paired with the host
    /// incarnation that gave it.
    CreationLookup {
        instance_id: String,
        lookup: CreationLookup,
    },
    /// Protocol v14. The key is retired; `lookup` is what it held at that
    /// moment, from the same host incarnation.
    CreationRetired {
        instance_id: String,
        lookup: CreationLookup,
    },
    /// Protocol v16. The origin lookup's answer, paired with the host
    /// incarnation that gave it.
    OriginLookup {
        instance_id: String,
        lookup: koru_session_protocol::OriginLookup,
    },
    /// Protocol v14. Whether the lease is free now or the holder was asked.
    ControlRequested {
        outcome: ControlRequestOutcome,
    },
    Ok,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResponseFrame {
    pub id: u64,
    pub result: Result<Response, SessionFailure>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OutputUpdate {
    Chunk {
        chunk: RawOutputChunk,
    },
    Gap {
        expected: koru_session_protocol::StreamSequence,
        actual: koru_session_protocol::StreamSequence,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SubscriptionUpdate {
    Render {
        update: RenderUpdate,
    },
    Event {
        event: SequencedSessionEvent,
    },
    EventGap {
        expected: koru_session_protocol::StreamSequence,
        actual: koru_session_protocol::StreamSequence,
    },
    AgentEvent {
        event: SequencedAgentEvent,
    },
    Providers {
        provider: koru_session_protocol::ProviderReadiness,
    },
    AgentEventGap {
        expected: koru_session_protocol::StreamSequence,
        actual: koru_session_protocol::StreamSequence,
    },
    Output {
        update: OutputUpdate,
    },
    /// The named catalog subscription. It rides the same subscription frame
    /// as the per-session streams but carries no session id, because it is
    /// not about a session.
    Catalog {
        event: CatalogEvent,
    },
    Closed {
        error: Option<SessionFailure>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubscriptionFrame {
    pub subscription_id: SubscriptionId,
    pub update: SubscriptionUpdate,
}

/// One frame from the host. A response is built once per request and
/// written at once, so the size gap between a response carrying a session
/// summary and a subscription update buys nothing to box away.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)]
pub enum FromHost {
    Response { frame: ResponseFrame },
    Subscription { frame: SubscriptionFrame },
}

#[cfg(test)]
mod tests {
    use super::*;
    use koru_session_protocol::{
        CapabilityName, MAX_PROTOCOL_VERSION, MIN_PROTOCOL_VERSION, ProtocolRange,
    };
    use std::str::FromStr;

    #[test]
    fn handshake_and_request_tags_are_stable() {
        let hello = ClientHello {
            product: "koru-workspace".into(),
            product_version: "0.0.3".into(),
            offer: ProtocolOffer {
                versions: ProtocolRange {
                    min: MIN_PROTOCOL_VERSION,
                    max: MAX_PROTOCOL_VERSION,
                },
                capabilities: CapabilitySet::new([
                    CapabilityName::from_str("render-deltas").unwrap()
                ]),
            },
            role: None,
        };
        let value = serde_json::to_value(hello).unwrap();
        assert_eq!(value["product"], "koru-workspace");

        let request = serde_json::to_value(RequestFrame {
            id: 7,
            request: Request::Health,
        })
        .unwrap();
        assert_eq!(request["request"]["kind"], "health");
    }

    #[test]
    fn subscription_ids_reject_zero_at_construction() {
        assert_eq!(SubscriptionId::new(0), None);
        assert_eq!(SubscriptionId::new(1), Some(SubscriptionId(1)));
    }

    #[test]
    fn protocol_nine_managed_request_tags_are_stable_and_content_free() {
        let session_id = SessionId::mint();
        let lease = ControllerLeaseToken::mint();
        let through = koru_session_protocol::StreamSequence::new(7).unwrap();
        let acknowledgement = serde_json::to_value(Request::AcknowledgeAttention {
            session_id,
            controller_lease: lease,
            through,
        })
        .unwrap();
        assert_eq!(acknowledgement["kind"], "acknowledge_attention");
        assert_eq!(acknowledgement["through"], 7);

        let observation = serde_json::to_value(Request::ObserveAgentEvents { session_id }).unwrap();
        assert_eq!(observation["kind"], "observe_agent_events");

        let cancellation = serde_json::to_value(Request::CancelTurn {
            session_id,
            controller_lease: lease,
            idempotency_key: IdempotencyKey::mint(),
            turn_id: koru_session_protocol::AgentTurnId::mint(),
            reason: CancelReason::Run,
        })
        .unwrap();
        assert_eq!(cancellation["kind"], "cancel_turn");
        assert!(cancellation.get("text").is_none());
        assert!(cancellation.get("approval_id").is_none());
    }

    #[test]
    fn protocol_ten_project_request_tags_are_stable_and_carry_no_content() {
        let project_id = koru_session_protocol::ProjectId::mint();
        for (request, tag) in [
            (Request::ListProjects, "list_projects"),
            (
                Request::CreateProject {
                    create: CreateProject {
                        project_id,
                        name: "Koru".into(),
                        root_directory: None,
                        workspace_scope: None,
                    },
                },
                "create_project",
            ),
            (
                Request::RenameProject {
                    project_id,
                    name: "Koru".into(),
                },
                "rename_project",
            ),
            (
                Request::SetProjectRoot {
                    project_id,
                    root_directory: None,
                },
                "set_project_root",
            ),
            (Request::DeleteProject { project_id }, "delete_project"),
            (
                Request::AssignProject {
                    session_id: SessionId::mint(),
                    project_id: Some(project_id),
                },
                "assign_project",
            ),
            (Request::SubscribeProjects, "subscribe_projects"),
        ] {
            let value = serde_json::to_value(&request).unwrap();
            assert_eq!(value["kind"], tag);
            assert_eq!(
                serde_json::from_value::<Request>(value).unwrap(),
                request,
                "{tag} does not round trip"
            );
        }

        // The catalog subscription says only that something changed. A frame
        // carrying rows would make a bounded invalidation stream unbounded.
        let update = serde_json::to_value(SubscriptionUpdate::Catalog {
            event: koru_session_protocol::CatalogEvent::ProjectsChanged,
        })
        .unwrap();
        assert_eq!(update["kind"], "catalog");
        assert_eq!(update["event"]["kind"], "projects_changed");
        assert!(update["event"].get("projects").is_none());
    }

    /// `resume` is absent from every creation that does not ask for it, so an
    /// older host receives exactly the frame it always did, and present as a
    /// plain boolean when it does.
    #[test]
    fn a_resume_flag_is_spelled_only_when_it_is_asked_for() {
        let create = |resume| {
            serde_json::to_value(CreateProfileSession {
                idempotency_key: IdempotencyKey::mint(),
                profile: SessionProfileKey::BuiltIn("claude".parse().unwrap()),
                parent_session_id: None,
                mode: SessionMode::RawPty,
                project_id: None,
                delegation: None,
                origin: Default::default(),
                selection: None,
                resume,
                working_directory: PathBuf::from("/tmp"),
                size: GridSize {
                    columns: 80,
                    rows: 24,
                },
            })
            .unwrap()
        };
        assert!(create(false).get("resume").is_none());
        assert_eq!(create(true)["resume"], true);
        let back: CreateProfileSession =
            serde_json::from_value(create(false)).expect("a frame without the field reads");
        assert!(!back.resume);
    }

    #[test]
    fn a_pre_project_peer_reads_both_creation_frames_and_ignores_the_placement() {
        // An older host receives `project_id` it does not declare. Serde skips
        // an unknown field, so the session is still created — unplaced, which
        // is the truth on a host that has no projects.
        let profile_create = serde_json::to_value(CreateProfileSession {
            idempotency_key: IdempotencyKey::mint(),
            profile: SessionProfileKey::BuiltIn("bash".parse().unwrap()),
            parent_session_id: None,
            mode: SessionMode::RawPty,
            project_id: Some(koru_session_protocol::ProjectId::mint()),
            delegation: None,
            origin: Default::default(),
            selection: None,
            resume: false,
            working_directory: PathBuf::from("/tmp"),
            size: koru_session_protocol::GridSize::new(80, 24).unwrap(),
        })
        .unwrap();
        assert!(profile_create.get("project_id").is_some());

        // ...and the reverse: a v10 peer reading a v9 frame that never had the
        // field at all.
        let mut legacy = profile_create.clone();
        legacy.as_object_mut().unwrap().remove("project_id");
        assert_eq!(
            serde_json::from_value::<CreateProfileSession>(legacy)
                .unwrap()
                .project_id,
            None
        );

        let mut legacy_raw = serde_json::to_value(CreateSession {
            profile: SessionProfileKey::BuiltIn("bash".parse().unwrap()),
            parent_session_id: None,
            profile_kind: ProfileKind::Shell,
            display_name: "shell".into(),
            working_directory: PathBuf::from("/tmp"),
            mode: SessionMode::RawPty,
            attachment_delivery: AttachmentDeliveryPolicy::Unsupported,
            capabilities: CapabilitySet::default(),
            project_id: Some(koru_session_protocol::ProjectId::mint()),
            argv: vec!["/bin/sh".into()],
            environment: EnvironmentSpec::Inherit,
            size: koru_session_protocol::GridSize::new(80, 24).unwrap(),
        })
        .unwrap();
        legacy_raw.as_object_mut().unwrap().remove("project_id");
        assert_eq!(
            serde_json::from_value::<CreateSession>(legacy_raw)
                .unwrap()
                .project_id,
            None
        );
    }

    /// The v11 frames say what they are, carry the typed scope rather than a
    /// compound string, and name the sessions they closed rather than a count.
    #[test]
    fn protocol_eleven_workspace_lifecycle_tags_are_stable() {
        let scope = koru_session_protocol::WorkspaceScope {
            profile: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa".parse().unwrap(),
            workspace: "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb".parse().unwrap(),
        };
        let request = serde_json::to_value(Request::TerminateWorkspaceSessions {
            authority: "launch-secret".into(),
            scope,
        })
        .unwrap();
        assert_eq!(request["kind"], "terminate_workspace_sessions");
        assert!(
            request["scope"].get("workspace").is_some(),
            "the scope crossed as something other than the typed pair: {request}"
        );

        let session_id = SessionId::mint();
        let answer = serde_json::to_value(Response::WorkspaceSessionsTerminated {
            terminated: vec![session_id],
        })
        .unwrap();
        assert_eq!(answer["kind"], "workspace_sessions_terminated");
        assert_eq!(answer["terminated"].as_array().unwrap().len(), 1);
        let empty = serde_json::to_value(Response::WorkspaceSessionsTerminated {
            terminated: Vec::new(),
        })
        .unwrap();
        assert!(empty["terminated"].as_array().unwrap().is_empty());
    }

    /// The v12 frames say what they are, name the workspace and the profile as
    /// two typed ids rather than a scope pair, and answer with the projects
    /// they repaired rather than a count.
    ///
    /// **Not a `WorkspaceScope`, deliberately.** The request is "every project
    /// scoped to this workspace now names this profile" — the workspace is the
    /// one being searched for and the profile is the one being written, and a
    /// pair would have said they were two halves of one address. It would also
    /// have made "move these projects to a different workspace" expressible,
    /// which is not an operation and would have needed a refusal to say so.
    #[test]
    fn protocol_twelve_project_rescope_tags_are_stable() {
        let request = serde_json::to_value(Request::RescopeWorkspaceProjects {
            authority: "launch-secret".into(),
            workspace: "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb".parse().unwrap(),
            profile: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa".parse().unwrap(),
        })
        .unwrap();
        assert_eq!(request["kind"], "rescope_workspace_projects");
        assert_eq!(request["workspace"], "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb");
        assert_eq!(request["profile"], "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa");
        assert!(
            request.get("scope").is_none(),
            "the two ids collapsed into a pair: {request}"
        );

        let answer = serde_json::to_value(Response::WorkspaceProjectsRescoped {
            rescoped: vec![ProjectId::mint()],
        })
        .unwrap();
        assert_eq!(answer["kind"], "workspace_projects_rescoped");
        assert_eq!(answer["rescoped"].as_array().unwrap().len(), 1);
        let empty = serde_json::to_value(Response::WorkspaceProjectsRescoped {
            rescoped: Vec::new(),
        })
        .unwrap();
        assert!(empty["rescoped"].as_array().unwrap().is_empty());
    }

    /// **v13's frame says what it is, and carries the whole typed pair.**
    ///
    /// The opposite shape to v12's rescope, deliberately. That one names a
    /// workspace and a profile as two loose ids because it *searches* by one
    /// and *writes* the other; this one names a project and the scope it should
    /// carry, so the pair really is two halves of one address and crosses as
    /// one. A scope of `None` is the field's other legal value, not a second
    /// verb — see [`Request::SetProjectScope`].
    #[test]
    fn protocol_thirteen_project_scope_tags_are_stable() {
        let project_id = ProjectId::mint();
        let scope = koru_session_protocol::WorkspaceScope {
            profile: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa".parse().unwrap(),
            workspace: "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb".parse().unwrap(),
        };
        for workspace_scope in [Some(scope), None] {
            let request = Request::SetProjectScope {
                project_id,
                workspace_scope,
            };
            let value = serde_json::to_value(&request).unwrap();
            assert_eq!(value["kind"], "set_project_scope");
            assert_eq!(value["project_id"], project_id.to_string());
            assert_eq!(
                serde_json::from_value::<Request>(value).unwrap(),
                request,
                "the scope rewrite does not round trip with {workspace_scope:?}"
            );
        }

        // The pair crosses as a pair, and the halves are not flattened into
        // sibling fields the way v12's two loose ids are.
        let value = serde_json::to_value(Request::SetProjectScope {
            project_id,
            workspace_scope: Some(scope),
        })
        .unwrap();
        assert_eq!(
            value["workspace_scope"]["workspace"],
            "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"
        );
        assert_eq!(
            value["workspace_scope"]["profile"],
            "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
        );
        assert!(
            value.get("profile").is_none() && value.get("workspace").is_none(),
            "the pair was split into loose ids: {value}"
        );

        // It answers with the row, not an ack: the caller reads what the
        // project now is rather than assuming its own request landed.
        let answer = serde_json::to_value(Response::Project {
            project: ProjectSummary {
                id: project_id,
                name: "Koru".into(),
                root_directory: None,
                workspace_scope: Some(scope),
                created_at_ms: 1,
            },
        })
        .unwrap();
        assert_eq!(answer["kind"], "project");
        assert_eq!(
            answer["project"]["workspace_scope"]["workspace"],
            value["workspace_scope"]["workspace"]
        );
    }

    /// **v14's frames say what they are, and none of them is a create.** The
    /// lookup and the retirement name a key; the handover verbs name a
    /// session and, for the two the holder sends, its lease; the responses
    /// carry the host incarnation beside the answer so a caller pairs them
    /// atomically instead of trusting a handshake it did earlier.
    #[test]
    fn protocol_fourteen_delegation_tags_are_stable_and_carry_no_content() {
        let key = IdempotencyKey::mint();
        let session_id = SessionId::mint();
        let lease = ControllerLeaseToken::mint();
        for (request, tag) in [
            (
                Request::LookupCreation {
                    idempotency_key: key,
                },
                "lookup_creation",
            ),
            (
                Request::RetireCreation {
                    idempotency_key: key,
                },
                "retire_creation",
            ),
            (
                Request::RequestControl {
                    session_id,
                    urgency: TakeoverUrgency::Now,
                },
                "request_control",
            ),
            (
                Request::ReleaseControl { session_id, lease },
                "release_control",
            ),
            (
                Request::ReturnControl { session_id, lease },
                "return_control",
            ),
        ] {
            let value = serde_json::to_value(&request).unwrap();
            assert_eq!(value["kind"], tag);
            assert_eq!(
                serde_json::from_value::<Request>(value).unwrap(),
                request,
                "{tag} does not round trip"
            );
        }

        // A request from a peer that predates the urgency field is a
        // boundary request, which is the only kind there used to be.
        let legacy: Request = serde_json::from_value(serde_json::json!({
            "kind": "request_control",
            "session_id": session_id.to_string(),
        }))
        .unwrap();
        assert_eq!(
            legacy,
            Request::RequestControl {
                session_id,
                urgency: TakeoverUrgency::AtTurnBoundary,
            }
        );

        let answer = serde_json::to_value(Response::CreationLookup {
            instance_id: "host-incarnation".into(),
            lookup: CreationLookup::NoEvidence,
        })
        .unwrap();
        assert_eq!(answer["kind"], "creation_lookup");
        assert_eq!(answer["instance_id"], "host-incarnation");
        assert_eq!(answer["lookup"]["kind"], "no_evidence");
        let retired = serde_json::to_value(Response::CreationRetired {
            instance_id: "host-incarnation".into(),
            lookup: CreationLookup::Creating,
        })
        .unwrap();
        assert_eq!(retired["kind"], "creation_retired");
        assert_eq!(retired["lookup"]["kind"], "creating");
        let control = serde_json::to_value(Response::ControlRequested {
            outcome: ControlRequestOutcome::Waiting,
        })
        .unwrap();
        assert_eq!(control["kind"], "control_requested");
        assert_eq!(control["outcome"], "waiting");
    }

    /// **The two defaulted fields read back as what they used to mean.** A
    /// cancel without a reason cancels the run; a create without a
    /// delegation is a manual one. And the delegation, when present, prints
    /// neither its endpoint nor its resume token.
    #[test]
    fn a_pre_delegation_peer_reads_cancel_and_create_as_manual() {
        let session_id = SessionId::mint();
        let lease = ControllerLeaseToken::mint();
        let key = IdempotencyKey::mint();
        let turn_id = koru_session_protocol::AgentTurnId::mint();
        let cancel: Request = serde_json::from_value(serde_json::json!({
            "kind": "cancel_turn",
            "session_id": session_id.to_string(),
            "controller_lease": serde_json::to_value(lease).unwrap(),
            "idempotency_key": key.to_string(),
            "turn_id": turn_id.to_string(),
        }))
        .unwrap();
        assert_eq!(
            cancel,
            Request::CancelTurn {
                session_id,
                controller_lease: lease,
                idempotency_key: key,
                turn_id,
                reason: CancelReason::Run,
            }
        );

        let launch = DelegationLaunch {
            model: None,
            mcp_endpoint: "http://127.0.0.1:4/runs/SECRET_RUN_PATH_92741"
                .parse()
                .unwrap(),
            tools: koru_session_protocol::McpToolAllowlist::new(["koru_register_artifact"
                .parse()
                .unwrap()])
            .unwrap(),
            credential: Some("SECRET_BEARER_92741".parse().unwrap()),
            resume: Some("SECRET_RESUME_92741".parse().unwrap()),
        };
        let create = CreateProfileSession {
            idempotency_key: key,
            profile: SessionProfileKey::BuiltIn("codex".parse().unwrap()),
            parent_session_id: None,
            mode: SessionMode::Managed {
                provider: "codex".parse().unwrap(),
            },
            project_id: None,
            delegation: Some(launch),
            origin: Default::default(),
            selection: None,
            resume: false,
            working_directory: PathBuf::from("/tmp"),
            size: koru_session_protocol::GridSize::new(80, 24).unwrap(),
        };
        let printed = format!("{create:?}");
        assert!(!printed.contains("SECRET_RUN_PATH_92741"), "{printed}");
        assert!(!printed.contains("SECRET_RESUME_92741"), "{printed}");
        assert!(!printed.contains("SECRET_BEARER_92741"), "{printed}");
        assert!(printed.contains("tool_count: 1"), "{printed}");

        let mut value = serde_json::to_value(&create).unwrap();
        assert_eq!(
            serde_json::from_value::<CreateProfileSession>(value.clone()).unwrap(),
            create
        );
        // A defaulted field: a peer that predates the bearer reads the
        // message and gets `None` (protocol v14's rule above
        // `MAX_PROTOCOL_VERSION`).
        let mut older = value.clone();
        older["delegation"]
            .as_object_mut()
            .unwrap()
            .remove("credential")
            .unwrap();
        assert_eq!(
            serde_json::from_value::<CreateProfileSession>(older)
                .unwrap()
                .delegation
                .unwrap()
                .credential,
            None
        );
        value.as_object_mut().unwrap().remove("delegation").unwrap();
        assert_eq!(
            serde_json::from_value::<CreateProfileSession>(value)
                .unwrap()
                .delegation,
            None
        );
    }

    /// **Two secrets, and neither is printable.** The bootstrap is written to
    /// a child's stdin and logged nowhere, but a `Debug` that leaked either one
    /// would put both in a panic message.
    #[test]
    fn the_bootstrap_carries_two_distinct_secrets_and_prints_neither() {
        let bootstrap = LaunchBootstrap::new(
            "launch".into(),
            "internal-debug".into(),
            PathBuf::from("/root"),
            PathBuf::from("/root/sessions"),
            PathBuf::from("/root/store"),
            true,
            "stop-the-host".into(),
        )
        .with_lifecycle_authority("close-a-workspace".into());
        // A supervisor that never added one grants no workspace lifecycle at
        // all, and an empty authority never matches a presented one.
        assert!(
            LaunchBootstrap::new(
                "launch".into(),
                "internal-debug".into(),
                PathBuf::from("/root"),
                PathBuf::from("/root/sessions"),
                PathBuf::from("/root/store"),
                true,
                "stop-the-host".into(),
            )
            .lifecycle_authority()
            .is_empty()
        );
        assert_eq!(bootstrap.authority(), "stop-the-host");
        assert_eq!(bootstrap.lifecycle_authority(), "close-a-workspace");
        assert_ne!(bootstrap.authority(), bootstrap.lifecycle_authority());
        let printed = format!("{bootstrap:?}");
        assert!(!printed.contains("stop-the-host"), "{printed}");
        assert!(!printed.contains("close-a-workspace"), "{printed}");

        // A bootstrap written by an Agent that predates SP-8a has no lifecycle
        // field. It reads back empty, and an empty authority never matches —
        // a host with no secret must not be a host that accepts every caller
        // who also sends nothing.
        let mut older = serde_json::to_value(&bootstrap).unwrap();
        older
            .as_object_mut()
            .unwrap()
            .remove("lifecycle_authority")
            .expect("the field is written");
        assert_eq!(
            serde_json::from_value::<LaunchBootstrap>(older)
                .unwrap()
                .lifecycle_authority(),
            ""
        );
    }
}
