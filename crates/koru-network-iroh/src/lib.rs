// SPDX-License-Identifier: MPL-2.0

//! Iroh endpoint and QUIC adapter for Koru's device network edge.
//!
//! Agent supplies volatile identity, authorization snapshots, and connection
//! intent. This crate owns network addresses, QUIC connections, and streams.
//! No iroh type crosses the Agent/Store control contract.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use iroh::{Endpoint, EndpointAddr, EndpointId, SecretKey, endpoint::presets};
use koru_device::{BuildPayload, DeviceId, RelationshipId, ReplicationEndpointId};
use koru_store_core::model::BlobHash;
use koru_sync_control::{
    AuthorizationSnapshot, AuthorizedPeer, PairingAttemptId, PairingEvent, TrustDeliveryId,
};
use serde::{Deserialize, Serialize};
use tokio::sync::{Semaphore, mpsc, oneshot, watch};

use koru_network_edge::{NetworkIngress, TransportSession};

pub use koru_network_edge::PathKind;
pub use koru_replication::protocol;
pub use koru_replication::{
    PEER_PROTOCOL_VERSION, STATE_EVENT_PAGE_SIZE, StateChangeEvent, StateChangePage, SyncEvent,
    sync_event_direction,
};

/// The framing version. Changing it refuses the connection at the iroh
/// handshake, before any frame — so it is bumped only for a change to the
/// frames themselves; a change to what they *mean* bumps
/// [`protocol::PEER_PROTOCOL_VERSION`], which refuses sync with a reason
/// instead.
pub const REPLICATION_ALPN_V1: &[u8] = b"koru/replication/1";
pub(crate) mod blob_path;
pub mod cloud;
pub const PAIRING_ALPN_V1: &[u8] = b"koru/pairing/1";
pub const ENROLLMENT_ALPN_V1: &[u8] = b"koru/enrollment/1";
pub const MAX_ENROLLMENT_FRAME_BYTES: usize = 128 * 1024;
pub const MAX_ENROLLMENT_FRAMES_PER_DIRECTION: usize = 8;
const MAX_CONCURRENT_ENROLLMENT_SESSIONS: usize = 4;
const ENROLLMENT_SESSION_TIMEOUT: Duration = Duration::from_secs(120);

/// Encode one enrollment frame with a ceremony-specific limit. Enrollment
/// never inherits replication's multi-megabyte content-frame allowance.
pub fn encode_enrollment_frame(
    frame: &koru_pairing::EnrollmentFrameV1,
) -> Result<Vec<u8>, EnrollmentFrameError> {
    let bytes = serde_json::to_vec(frame).map_err(|_| EnrollmentFrameError::Malformed)?;
    if bytes.len() > MAX_ENROLLMENT_FRAME_BYTES {
        return Err(EnrollmentFrameError::Oversized);
    }
    let mut framed = Vec::with_capacity(4 + bytes.len());
    framed.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    framed.extend_from_slice(&bytes);
    Ok(framed)
}

pub fn decode_enrollment_frame(
    bytes: &[u8],
) -> Result<koru_pairing::EnrollmentFrameV1, EnrollmentFrameError> {
    let length: [u8; 4] = bytes
        .get(..4)
        .ok_or(EnrollmentFrameError::Malformed)?
        .try_into()
        .expect("the slice is exactly four bytes");
    let length = u32::from_be_bytes(length) as usize;
    if length > MAX_ENROLLMENT_FRAME_BYTES {
        return Err(EnrollmentFrameError::Oversized);
    }
    let body = bytes.get(4..).ok_or(EnrollmentFrameError::Malformed)?;
    if body.len() != length {
        return Err(EnrollmentFrameError::Malformed);
    }
    serde_json::from_slice(body).map_err(|_| EnrollmentFrameError::Malformed)
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum EnrollmentFrameError {
    Malformed,
    Oversized,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum EnrollmentTransportError {
    RuntimeClosed,
    EndpointUnavailable,
    EndpointMismatch,
    ConnectFailed,
    StreamFailed,
    MalformedFrame,
    OversizedFrame,
    FrameLimit,
    TimedOut,
}

impl fmt::Display for EnrollmentTransportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::RuntimeClosed => "network runtime closed",
            Self::EndpointUnavailable => "enrollment endpoint unavailable",
            Self::EndpointMismatch => "enrollment endpoint identity mismatch",
            Self::ConnectFailed => "enrollment connection failed",
            Self::StreamFailed => "enrollment stream failed",
            Self::MalformedFrame => "malformed enrollment frame",
            Self::OversizedFrame => "oversized enrollment frame",
            Self::FrameLimit => "enrollment frame limit reached",
            Self::TimedOut => "enrollment session timed out",
        })
    }
}

impl std::error::Error for EnrollmentTransportError {}

/// One endpoint-authenticated enrollment ceremony carried by one bounded QUIC
/// bidirectional stream. The transport exposes no replication or legacy
/// pairing operations; Plan 4.5 supplies only the ceremony policy.
pub struct EnrollmentSession {
    connection: iroh::endpoint::Connection,
    send: iroh::endpoint::SendStream,
    recv: iroh::endpoint::RecvStream,
    remote_endpoint: ReplicationEndpointId,
    sent_frames: usize,
    received_frames: usize,
    deadline: tokio::time::Instant,
}

impl fmt::Debug for EnrollmentSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EnrollmentSession")
            .field("remote_endpoint", &self.remote_endpoint)
            .field("sent_frames", &self.sent_frames)
            .field("received_frames", &self.received_frames)
            .finish_non_exhaustive()
    }
}

impl EnrollmentSession {
    fn new(
        connection: iroh::endpoint::Connection,
        send: iroh::endpoint::SendStream,
        recv: iroh::endpoint::RecvStream,
    ) -> Self {
        let deadline = tokio::time::Instant::now()
            .checked_add(ENROLLMENT_SESSION_TIMEOUT)
            .expect("the fixed enrollment timeout fits in Instant");
        Self::with_deadline(connection, send, recv, deadline)
    }

    fn with_deadline(
        connection: iroh::endpoint::Connection,
        send: iroh::endpoint::SendStream,
        recv: iroh::endpoint::RecvStream,
        deadline: tokio::time::Instant,
    ) -> Self {
        let remote_endpoint = ReplicationEndpointId::from_bytes(*connection.remote_id().as_bytes());
        Self {
            connection,
            send,
            recv,
            remote_endpoint,
            sent_frames: 0,
            received_frames: 0,
            deadline,
        }
    }

    pub fn remote_endpoint(&self) -> ReplicationEndpointId {
        self.remote_endpoint
    }

    pub async fn send_frame(
        &mut self,
        frame: &koru_pairing::EnrollmentFrameV1,
    ) -> Result<(), EnrollmentTransportError> {
        if self.sent_frames >= MAX_ENROLLMENT_FRAMES_PER_DIRECTION {
            return Err(EnrollmentTransportError::FrameLimit);
        }
        let bytes = encode_enrollment_frame(frame).map_err(|error| match error {
            EnrollmentFrameError::Malformed => EnrollmentTransportError::MalformedFrame,
            EnrollmentFrameError::Oversized => EnrollmentTransportError::OversizedFrame,
        })?;
        tokio::time::timeout_at(self.deadline, self.send.write_all(&bytes))
            .await
            .map_err(|_| EnrollmentTransportError::TimedOut)?
            .map_err(|_| EnrollmentTransportError::StreamFailed)?;
        self.sent_frames += 1;
        Ok(())
    }

    pub async fn receive_frame(
        &mut self,
    ) -> Result<koru_pairing::EnrollmentFrameV1, EnrollmentTransportError> {
        if self.received_frames >= MAX_ENROLLMENT_FRAMES_PER_DIRECTION {
            return Err(EnrollmentTransportError::FrameLimit);
        }
        let mut length = [0; 4];
        tokio::time::timeout_at(self.deadline, self.recv.read_exact(&mut length))
            .await
            .map_err(|_| EnrollmentTransportError::TimedOut)?
            .map_err(|_| EnrollmentTransportError::StreamFailed)?;
        let length = u32::from_be_bytes(length) as usize;
        if length > MAX_ENROLLMENT_FRAME_BYTES {
            return Err(EnrollmentTransportError::OversizedFrame);
        }
        let mut body = vec![0; length];
        tokio::time::timeout_at(self.deadline, self.recv.read_exact(&mut body))
            .await
            .map_err(|_| EnrollmentTransportError::TimedOut)?
            .map_err(|_| EnrollmentTransportError::StreamFailed)?;
        let frame =
            serde_json::from_slice(&body).map_err(|_| EnrollmentTransportError::MalformedFrame)?;
        self.received_frames += 1;
        Ok(frame)
    }

    /// Finish this side of the ceremony stream and wait until the peer has
    /// acknowledged every buffered byte. This prevents the connection handle
    /// from being the last owner while a final Complete/Error frame is still
    /// in flight.
    pub async fn finish(&mut self) -> Result<(), EnrollmentTransportError> {
        self.send
            .finish()
            .map_err(|_| EnrollmentTransportError::StreamFailed)?;
        match tokio::time::timeout_at(self.deadline, self.send.stopped()).await {
            Ok(Ok(None)) => Ok(()),
            Ok(Ok(Some(_)) | Err(_)) => Err(EnrollmentTransportError::StreamFailed),
            Err(_) => Err(EnrollmentTransportError::TimedOut),
        }
    }

    fn close(&self, code: u32, reason: &'static [u8]) {
        self.connection.close(code.into(), reason);
    }
}

pub type EnrollmentHandlerFuture<'a> =
    Pin<Box<dyn Future<Output = Result<(), EnrollmentHandlerError>> + Send + 'a>>;

/// Inbound enrollment policy, injected by the future account coordinator.
/// Returning an error sends one bounded refusal frame before closing. The
/// runtime cancels this future when the ceremony deadline expires.
pub trait EnrollmentHandler: Send + Sync + 'static {
    fn handle<'a>(&'a self, session: &'a mut EnrollmentSession) -> EnrollmentHandlerFuture<'a>;
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct EnrollmentHandlerError {
    code: &'static str,
}

impl EnrollmentHandlerError {
    pub const fn refused(code: &'static str) -> Self {
        Self { code }
    }

    pub const fn code(self) -> &'static str {
        self.code
    }
}

struct EnrollmentHub {
    handler: RwLock<Option<Arc<dyn EnrollmentHandler>>>,
    sessions: Arc<Semaphore>,
}

impl Default for EnrollmentHub {
    fn default() -> Self {
        Self::new()
    }
}

impl EnrollmentHub {
    fn new() -> Self {
        Self {
            handler: RwLock::new(None),
            sessions: Arc::new(Semaphore::new(MAX_CONCURRENT_ENROLLMENT_SESSIONS)),
        }
    }
}

pub fn replication_protocol() -> koru_network_edge::ProtocolToken {
    koru_network_edge::ProtocolToken::parse(
        std::str::from_utf8(REPLICATION_ALPN_V1).expect("the static replication ALPN is ASCII"),
    )
    .expect("the static replication ALPN is a valid protocol token")
}

/// The exact Cloud content token, `koru-cloud-content-v1` (HOST-CONTROL §2).
///
/// It is a **local dispatch identifier only**. It does not change the frozen
/// Iroh ALPN and it does not change a single Cloud record byte: it is how a
/// host tells a Cloud content stream apart from a peer replication stream on
/// the bridge it already has. A Cloud permit cannot authorize
/// [`replication_protocol`], and a device relationship cannot authorize this.
pub const CLOUD_CONTENT_PROTOCOL: &str = "koru-cloud-content-v1";

pub fn cloud_content_protocol() -> koru_network_edge::ProtocolToken {
    koru_network_edge::ProtocolToken::parse(CLOUD_CONTENT_PROTOCOL)
        .expect("the static Cloud content token is a valid protocol token")
}

/// Project an Iroh-authorized peer into the transport-neutral edge principal.
/// It is always the device arm: this function has no way to mint a Cloud
/// authority context, and that is the point.
pub fn device_principal(
    peer: &AuthorizedPeer,
    authorization_revision: u64,
) -> koru_network_edge::NetworkPrincipal {
    koru_network_edge::NetworkPrincipal::Device {
        device: koru_network_edge::DevicePrincipal {
            device_id: peer.device_id,
            relationship_id: peer.relationship_id,
            endpoint_id: peer.endpoint_id,
            authorization_revision,
        },
    }
}

static NEXT_SYNC_PASS: AtomicU64 = AtomicU64::new(1);
use koru_replication::CHANGE_ANNOUNCEMENT_DEBOUNCE;
const SYNC_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// The peer answering that it does not have the Profile this workspace belongs
/// to — because somebody removed it there.
///
/// **The one refusal that retrying cannot mend.** Every other sync failure is a
/// timeout, a busy store or a race, and asking again is how it resolves; this
/// one is a settled fact on the other machine, and the answer will be identical
/// until a person changes something. So it is skipped rather than retried, and
/// the ordinary wake — a change on either side, or a reconnect — is what asks
/// again. That is also precisely when the answer could differ, so nothing is
/// lost by not asking in between.
///
/// It is still remembered as an error, so the condition stays visible to sync
/// status rather than becoming a silent hole in what replicates.
const PEER_LOST_THE_PROFILE: &str = "profile_not_found";
const SYNC_TRANSFER_TIMEOUT: Duration = Duration::from_secs(30);

mod reconnect;
use reconnect::{EARLY_DEATH, IncompatibleReport, IncompatibleReports, RetryReason, retry_delay};

/// Ordinary connection lifecycles are debug-only. The production signal is a
/// peer crossing this threshold inside one rolling window: eleven accepted
/// connections in a minute indicates a reconnect loop rather than normal
/// network movement. Report only the threshold crossing; another report is
/// possible after the rate has first fallen back below the threshold.
const RAPID_CONNECTION_WINDOW: Duration = Duration::from_secs(60);
const RAPID_CONNECTION_THRESHOLD: usize = 10;

pub mod hydration;

use hydration::{
    BlobHydrationCoordinator, BlobHydrationEntry, BlobHydrationKey, BlobHydrationOutcome,
    SharedBlobDirectory,
};
pub use hydration::{BlobHydrationError, BlobHydrationState, BlobHydrationTicket, BlobWaitReason};
const MAX_STATE_EVENTS: usize = 100;
const STATE_EVENT_RETENTION_MILLIS: i64 = 7 * 24 * 60 * 60 * 1_000;

/// Deployment configuration, never persisted into trust or Store data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IrohNetworkConfig {
    pub relay_provider: RelayProviderConfig,
    pub address_lookup: AddressLookupConfig,
    pub direct_paths: DirectPathPolicy,
}

impl Default for IrohNetworkConfig {
    /// **Iroh's own default network — not what Koru ships.** Since Plan 5.2
    /// every host builds its configuration through [`host_network_config`],
    /// whose relay list is the compiled Koru list for the build channel;
    /// `default()` remains only as the honest name for "whatever the iroh
    /// preset would do", for experiments and diagnostics that mean exactly
    /// that. No shipping path constructs it.
    fn default() -> Self {
        Self {
            relay_provider: RelayProviderConfig::DefaultNetwork,
            address_lookup: AddressLookupConfig::DefaultNetwork,
            direct_paths: DirectPathPolicy::Prefer,
        }
    }
}

impl IrohNetworkConfig {
    /// Hermetic mode for tests and deployments that inject exact addresses.
    pub fn isolated() -> Self {
        Self {
            relay_provider: RelayProviderConfig::Disabled,
            address_lookup: AddressLookupConfig::Disabled,
            direct_paths: DirectPathPolicy::Prefer,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RelayProviderConfig {
    /// Use the provider selected by this deployment's iroh preset. It is a
    /// runtime choice and is never part of a device identity or invitation.
    DefaultNetwork,
    /// Relay URLs injected by this deployment, including Koru-operated relays.
    /// The values remain ephemeral and never enter trust or Store data.
    CustomUrls(Vec<String>),
    Disabled,
}

/// Decision №8's fixed relay precedence, as one pure function every host can
/// share (the desktop Agent today; the Store host and mobile when Plan 5.2
/// centralizes them).
///
/// The rows, in order:
///
/// 1. **Relay Off disables every relay**, whatever else is configured.
/// 2. **Relay On uses the injected override** (`KORU_IROH_RELAYS`, parsed by
///    the host) when one is set. A deliberate injection also applies to an
///    isolated configuration — that is how a test or an operator points an
///    isolated stack at a local relay without it ever seeing a public one.
/// 3. **Otherwise the compiled channel list**, passed in by the caller so
///    this function never knows a hostname.
/// 4. **Isolated configuration stays isolated**: it never falls through to
///    the compiled/public list.
///
/// `channel_relays` is whatever the build compiles in — since Plan 5.2 the
/// Koru-only list for the build channel ([`channel_relays`]).
pub fn resolve_relay_provider(
    relay_enabled: bool,
    isolated: bool,
    override_urls: Option<&[String]>,
    channel_relays: &RelayProviderConfig,
) -> RelayProviderConfig {
    if !relay_enabled {
        return RelayProviderConfig::Disabled;
    }
    if let Some(urls) = override_urls
        && !urls.is_empty()
    {
        return RelayProviderConfig::CustomUrls(urls.to_vec());
    }
    if isolated {
        return RelayProviderConfig::Disabled;
    }
    channel_relays.clone()
}

/// The compiled Koru relay for internal builds (decision №5). An alias of the
/// same service as [`PRODUCTION_RELAY_URL`] — one Droplet serves both names,
/// so dev and production share one failure and deployment domain (ADR-272).
pub const INTERNAL_RELAY_URL: &str = "https://dev1.relay.koru.do";

/// The compiled Koru relay for production builds (decision №5).
pub const PRODUCTION_RELAY_URL: &str = "https://use1-1.relay.koru.do";

/// Which compiled relay list this build ships (decision №5): internal builds
/// select `dev1.relay.koru.do`, everything published selects
/// `use1-1.relay.koru.do`.
///
/// Deliberately **not** read from `koru-channel` inside this crate: which
/// build a binary is belongs to its host. Desktop hosts answer with
/// `koru_channel::ReleaseChannel::this_build_is_internal()`; mobile has no
/// release-channel stamping at all (`build_info()` would call every phone
/// "internal"), so it names [`RelayChannelKind::Production`] outright.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum RelayChannelKind {
    Internal,
    Production,
}

impl RelayChannelKind {
    /// Map a host's "is this an internal build" answer to its relay channel,
    /// in one place so two desktop hosts cannot invert it differently.
    pub fn for_internal_build(internal_build: bool) -> Self {
        if internal_build {
            Self::Internal
        } else {
            Self::Production
        }
    }
}

/// The compiled channel list (decision №2: Koru relays only, on every
/// platform — no n0 relay, default or fallback). Defined once, here, so no
/// host carries its own copy of a hostname.
pub fn channel_relays(kind: RelayChannelKind) -> RelayProviderConfig {
    RelayProviderConfig::CustomUrls(vec![
        match kind {
            RelayChannelKind::Internal => INTERNAL_RELAY_URL,
            RelayChannelKind::Production => PRODUCTION_RELAY_URL,
        }
        .to_owned(),
    ])
}

/// The pure whole-configuration resolver behind [`host_network_config`]:
/// decision №8's relay precedence over the compiled channel list, plus the
/// isolated switch for the rest of the configuration. Address lookup stays
/// [`AddressLookupConfig::DefaultNetwork`] everywhere except isolated mode —
/// decision №2 keeps n0 address *lookup*; only relay *transport* is
/// Koru-only.
pub fn resolve_network_config(
    relay_enabled: bool,
    kind: RelayChannelKind,
    isolated: bool,
    override_urls: Option<&[String]>,
) -> IrohNetworkConfig {
    let mut config = if isolated {
        IrohNetworkConfig::isolated()
    } else {
        IrohNetworkConfig {
            relay_provider: RelayProviderConfig::Disabled, // replaced below
            address_lookup: AddressLookupConfig::DefaultNetwork,
            direct_paths: DirectPathPolicy::Prefer,
        }
    };
    config.relay_provider = resolve_relay_provider(
        relay_enabled,
        isolated,
        override_urls,
        &channel_relays(kind),
    );
    config
}

/// The one host entry point (Plan 5.2): every host — Agent, the diagnostic
/// Store host, Mobile — builds its startup network configuration through
/// this call, so isolated mode, the `KORU_IROH_RELAYS` override, the
/// compiled channel default and the Relay-Off precedence cannot drift
/// between them.
///
/// `relay_enabled` is the host's relay on/off value (the shared desktop
/// preference; platform-local storage on mobile); `kind` is the host's
/// answer to which build it is. Everything environmental is read here.
pub fn host_network_config(relay_enabled: bool, kind: RelayChannelKind) -> IrohNetworkConfig {
    resolve_network_config(
        relay_enabled,
        kind,
        koru_env::switch(koru_env::name::KORU_IROH_ISOLATED),
        override_relays_from_env().as_deref(),
    )
}

/// The relay provider alone, for a live relay switch
/// ([`RuntimeHandle::replace_relay_provider`]) — the same resolution as
/// [`host_network_config`], so a switch mid-session lands on exactly the
/// configuration a restart would.
pub fn host_relay_provider(relay_enabled: bool, kind: RelayChannelKind) -> RelayProviderConfig {
    host_network_config(relay_enabled, kind).relay_provider
}

/// `KORU_IROH_RELAYS`, parsed exactly as the hosts always have:
/// `;`-separated, trimmed, empty entries dropped, an empty result meaning
/// unset.
fn override_relays_from_env() -> Option<Vec<String>> {
    let value = std::env::var(koru_env::name::KORU_IROH_RELAYS).ok()?;
    let urls = value
        .split(';')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    (!urls.is_empty()).then_some(urls)
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum AddressLookupConfig {
    DefaultNetwork,
    Disabled,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum DirectPathPolicy {
    Prefer,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PeerConnectionState {
    Disconnected,
    Resolving,
    Connecting,
    Connected { path: PathKind },
    BackingOff,
    Unauthorized,
    Failed { code: &'static str },
}

/// The relay connection as this runtime currently reads it — one value for
/// every status surface (the desktop tray row today, mobile's device screen
/// later), populated event-driven from the endpoint's own
/// [`Endpoint::home_relay_status`] watcher and never by a timer.
///
/// `host` is the relay URL's host string and is never translated. iroh
/// tracks the *selected home relay only*, and a relay that was never
/// reachable is never selected (`guide/contributing/deps/IROH_RELAY.md`), so an empty
/// watcher reading under a configured relay list reads as `Disconnected`
/// with the first configured URL's host — the honest "never reached"
/// reading. iroh's public `RelayStatus` exposes only `is_connected()` and
/// `last_error()` (the state enum itself is crate-private), so *Connecting*
/// here is a not-connected reading that has recorded no error: an attempt
/// in flight that nothing has failed yet.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum RelayConnectionStatus {
    /// Relays are disabled by configuration — there is nothing to connect to.
    #[default]
    Off,
    Connecting {
        host: String,
    },
    Connected {
        host: String,
    },
    Disconnected {
        host: String,
    },
}

/// What [`NetworkStatus::relay`] reads before — or without — a live
/// home-relay report: off when relays are disabled, otherwise "configured
/// and not reached" under the first configured URL's host. Also what a bind
/// failure or a torn-down endpoint honestly deserves: told to use relays,
/// holding no relay connection.
fn relay_provider_baseline(provider: &RelayProviderConfig) -> RelayConnectionStatus {
    match provider {
        RelayProviderConfig::Disabled => RelayConnectionStatus::Off,
        RelayProviderConfig::CustomUrls(urls) => RelayConnectionStatus::Disconnected {
            host: urls
                .first()
                .map(|url| relay_url_host(url))
                .unwrap_or_default(),
        },
        // Never produced by Koru's configuration rows ([`resolve_relay_provider`]);
        // which default-network relay would become home is unknowable before
        // the watcher reports one, so there is no host to name yet.
        RelayProviderConfig::DefaultNetwork => RelayConnectionStatus::Disconnected {
            host: String::new(),
        },
    }
}

/// The host string shown for a configured relay URL — `host_str` of the
/// parsed URL, or the trimmed input when it does not parse (an unparseable
/// configured URL is still better named than hidden).
fn relay_url_host(url: &str) -> String {
    url.parse::<iroh::RelayUrl>()
        .ok()
        .and_then(|parsed| parsed.host_str().map(str::to_owned))
        .unwrap_or_else(|| url.trim().to_owned())
}

/// The one-line news for a relay state transition — the log message and
/// the host it names (none for `Off`, which needs no relay to name). Pure
/// so the mapping is testable; [`set_relay_status`] emits it exactly when
/// the stored reading changes.
fn relay_transition_news(reading: &RelayConnectionStatus) -> (&'static str, Option<&str>) {
    match reading {
        RelayConnectionStatus::Off => ("relay off", None),
        RelayConnectionStatus::Connecting { host } => ("relay connecting", Some(host)),
        RelayConnectionStatus::Connected { host } => ("relay connected", Some(host)),
        RelayConnectionStatus::Disconnected { host } => ("relay disconnected", Some(host)),
    }
}

/// The single writer of [`NetworkStatus::relay`] after construction. A
/// changed reading is rare, consequential lifecycle news — connected,
/// disconnected, connecting, off — and logs one `info` line under the
/// `koru_sync::relay` target, which desktop logs and mobile logcat both
/// inherit. An unchanged reading writes and logs nothing: the watcher can
/// wake without a change, and retries are activity, not news
/// (`guide/contributing/PRINCIPLES.md`).
fn set_relay_status(status: &RwLock<NetworkStatus>, reading: RelayConnectionStatus) {
    let mut status = status.write().unwrap_or_else(|error| error.into_inner());
    if status.relay == reading {
        return;
    }
    let (message, host) = relay_transition_news(&reading);
    match host {
        Some(host) => tracing::info!(target: "koru_sync::relay", host = %host, "{message}"),
        None => tracing::info!(target: "koru_sync::relay", "{message}"),
    }
    status.relay = reading;
}

/// One home-relay reading mapped to the status value, as a pure function a
/// test can hold: connected wins; a recorded error means disconnected; not
/// connected with no error yet is an attempt in flight.
fn relay_state_reading(host: String, connected: bool, has_error: bool) -> RelayConnectionStatus {
    if connected {
        RelayConnectionStatus::Connected { host }
    } else if has_error {
        RelayConnectionStatus::Disconnected { host }
    } else {
        RelayConnectionStatus::Connecting { host }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NetworkStatus {
    pub endpoint_id: Option<ReplicationEndpointId>,
    pub peers: BTreeMap<DeviceId, PeerConnectionState>,
    pub last_seen_millis: BTreeMap<DeviceId, i64>,
    pub last_sync_event: Option<SyncEvent>,
    /// The bounded, current-runtime history of remote events that reached a
    /// state-changing boundary. Unlike `last_sync_event`, progress stages do
    /// not enter this list.
    pub state_events: VecDeque<StateChangeEvent>,
    /// Durable last-received timestamps, restored from the Store on startup.
    pub last_state_change_millis: BTreeMap<DeviceId, i64>,
    /// Accumulated backoff per peer. A peer flapping between Connected and
    /// BackingOff while this climbs is being refused or is dying on first
    /// use; a peer that drops after proving itself resets to zero.
    pub backoff_failures: BTreeMap<DeviceId, u8>,
    /// The relay connection, tracked event-driven from the endpoint's own
    /// home-relay watcher. See [`RelayConnectionStatus`].
    pub relay: RelayConnectionStatus,
}

#[derive(Clone)]
pub struct RuntimeHandle {
    commands: mpsc::UnboundedSender<Command>,
    status: Arc<RwLock<NetworkStatus>>,
    endpoint_addr: Arc<RwLock<Option<EndpointAddr>>>,
    pairing: Arc<PairingHub>,
    enrollment: Arc<EnrollmentHub>,
    blob_hydrations: BlobHydrationCoordinator,
    blob_directory: SharedBlobDirectory,
    store: Option<Arc<dyn protocol::ReplicationStore>>,
}

impl RuntimeHandle {
    pub fn spawn(config: IrohNetworkConfig) -> Self {
        Self::spawn_inner(config, None, None)
    }

    /// Start the data plane around a Store adapter. Keeping the adapter behind
    /// a port lets desktop host it in the daemon while mobile embeds the same
    /// protocol beside its own local Store.
    pub fn spawn_with_store(
        config: IrohNetworkConfig,
        store: Arc<dyn protocol::ReplicationStore>,
    ) -> Self {
        Self::spawn_inner(config, Some(store), None)
    }

    /// Start only the authenticated Iroh edge. Replication stream bytes are
    /// forwarded untouched to `ingress`; pairing remains local to this runtime.
    pub fn spawn_with_opaque_ingress(
        config: IrohNetworkConfig,
        ingress: Arc<dyn NetworkIngress>,
    ) -> Self {
        Self::spawn_inner(config, None, Some(ingress))
    }

    fn spawn_inner(
        config: IrohNetworkConfig,
        store: Option<Arc<dyn protocol::ReplicationStore>>,
        opaque_ingress: Option<Arc<dyn NetworkIngress>>,
    ) -> Self {
        let (commands, receiver) = mpsc::unbounded_channel();
        let mut initial_status = NetworkStatus {
            // The configuration's honest reading before any endpoint exists;
            // the home-relay watch replaces it once one is bound.
            relay: relay_provider_baseline(&config.relay_provider),
            ..NetworkStatus::default()
        };
        if let Some(store) = store.as_deref() {
            let retain_after = state_event_cutoff();
            if let Err(error) = store.prune_state_changes(retain_after) {
                tracing::warn!(code = error.code, "could not prune sync event history");
            }
            match store.latest_state_change_times(retain_after) {
                Ok(times) => initial_status.last_state_change_millis.extend(times),
                Err(error) => tracing::warn!(
                    code = error.code,
                    "could not restore linked-device last-sync times"
                ),
            }
        }
        let status = Arc::new(RwLock::new(initial_status));
        let endpoint_addr = Arc::new(RwLock::new(None));
        let pairing = Arc::new(PairingHub::default());
        let enrollment = Arc::new(EnrollmentHub::new());
        let blob_hydrations = Arc::new(Mutex::new(HashMap::new()));
        let blob_directory: SharedBlobDirectory =
            Arc::new(Mutex::new(hydration::BlobDirectory::new()));
        tokio::spawn(run(
            config,
            receiver,
            commands.clone(),
            Arc::clone(&status),
            Arc::clone(&endpoint_addr),
            Arc::clone(&pairing),
            Arc::clone(&enrollment),
            Arc::clone(&blob_hydrations),
            Arc::clone(&blob_directory),
            store.clone(),
            opaque_ingress.clone(),
        ));
        Self {
            commands,
            status,
            endpoint_addr,
            pairing,
            enrollment,
            blob_hydrations,
            blob_directory,
            store,
        }
    }

    pub fn provision_endpoint(&self, secret: [u8; 32]) {
        let _ = self.commands.send(Command::ProvisionEndpoint(secret));
    }

    /// Switch relay configuration live, without an app restart (decision №6).
    ///
    /// Relays are a route, not the endpoint: switching them never touches a
    /// connection that does not run over them. Off edits the live relay map
    /// and closes exactly the connections whose live paths are relay-only —
    /// a direct/LAN connection survives untouched, which is the product
    /// contract. A peer reachable only through a relay waits in the ordinary
    /// reconnect states (`Connecting`/`BackingOff`) while relays are off; it
    /// is never marked failed or unauthorized. On restores the relays into
    /// the same endpoint and wakes only the waiting peers. The one rebind
    /// left is switching on from a start with relays disabled — that
    /// endpoint was built without relay transport, so there is nothing to
    /// insert relays into ([`RuntimeState::replace_relay_provider`]); the
    /// rebind keeps the provisioned node secret, so the device identity is
    /// unchanged. Replacing the provider with the current value does
    /// nothing.
    ///
    /// This takes the already-resolved provider: preference, override and
    /// channel-list precedence is [`resolve_relay_provider`]'s job, and the
    /// caller's.
    pub fn replace_relay_provider(&self, provider: RelayProviderConfig) {
        let _ = self.commands.send(Command::ReplaceRelayProvider(provider));
    }

    pub fn replace_authorization(&self, snapshot: AuthorizationSnapshot) {
        let _ = self.commands.send(Command::ReplaceAuthorization(snapshot));
    }

    pub fn connect_peer(&self, device_id: DeviceId) {
        let _ = self.commands.send(Command::ConnectPeer {
            device_id,
            exact_addr: None,
        });
    }

    /// Test/embedding seam for an exact address learned outside Koru. The
    /// authenticated EndpointId still comes from authorization and must match.
    pub fn connect_peer_at(&self, device_id: DeviceId, addr: EndpointAddr) {
        let _ = self.commands.send(Command::ConnectPeer {
            device_id,
            exact_addr: Some(addr),
        });
    }

    pub fn disconnect_peer(&self, device_id: DeviceId) {
        let _ = self.commands.send(Command::DisconnectPeer(device_id));
    }

    pub fn begin_pairing(&self, invitation: koru_pairing::PairingInvitationV1) {
        *self
            .pairing
            .claimed
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = false;
        *self
            .pairing
            .invitation
            .write()
            .unwrap_or_else(|error| error.into_inner()) = Some(invitation);
    }

    pub fn cancel_pairing(&self) {
        self.pairing.cancel("pairing_cancelled");
    }

    /// Install or remove the policy handler for incoming enrollment sessions.
    /// The data plane authenticates the remote Iroh endpoint and retains all
    /// framing, concurrency, timeout, and close behavior.
    pub fn set_enrollment_handler(&self, handler: Option<Arc<dyn EnrollmentHandler>>) {
        *self
            .enrollment
            .handler
            .write()
            .unwrap_or_else(|error| error.into_inner()) = handler;
    }

    /// Open one enrollment ceremony using discovery's authenticated endpoint id.
    pub async fn open_enrollment(
        &self,
        expected_endpoint: ReplicationEndpointId,
    ) -> Result<EnrollmentSession, EnrollmentTransportError> {
        let endpoint = EndpointId::from_bytes(expected_endpoint.as_bytes())
            .map_err(|_| EnrollmentTransportError::EndpointUnavailable)?;
        self.open_enrollment_at(expected_endpoint, EndpointAddr::new(endpoint))
            .await
    }

    /// Open one enrollment ceremony to an exact, expected endpoint. The
    /// address is only routing data: both its ID and QUIC's authenticated
    /// remote ID must equal `expected_endpoint`.
    pub async fn open_enrollment_at(
        &self,
        expected_endpoint: ReplicationEndpointId,
        addr: EndpointAddr,
    ) -> Result<EnrollmentSession, EnrollmentTransportError> {
        let (answer, waiting) = oneshot::channel();
        self.commands
            .send(Command::OpenEnrollment {
                expected_endpoint,
                addr,
                answer,
            })
            .map_err(|_| EnrollmentTransportError::RuntimeClosed)?;
        waiting
            .await
            .unwrap_or(Err(EnrollmentTransportError::RuntimeClosed))
    }

    pub fn join_pairing(&self, join: koru_pairing::PairingJoinV1) {
        let _ = self.commands.send(Command::JoinPairing {
            join: Box::new(join),
            exact_addr: None,
        });
    }

    /// Test/mobile embedding seam for an address already resolved by the host.
    pub fn join_pairing_at(&self, join: koru_pairing::PairingJoinV1, addr: EndpointAddr) {
        let _ = self.commands.send(Command::JoinPairing {
            join: Box::new(join),
            exact_addr: Some(addr),
        });
    }

    pub fn complete_pairing(
        &self,
        attempt_id: PairingAttemptId,
        prepared: koru_pairing::PreparedPairingV1,
    ) {
        let pending = self
            .pairing
            .pending
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&attempt_id);
        if let Some(pending) = pending {
            let _ = pending.send(Ok(prepared));
        }
    }

    pub fn send_trust_record(&self, peer: DeviceId, record: koru_device::TrustRecord) {
        let _ = self.commands.send(Command::SendTrustRecord {
            peer,
            record: Box::new(record),
        });
    }

    /// Gossip the device list itself: the whole trust-record set in canonical
    /// order. The returned receiver reports whether the batch was *queued
    /// onto a live connection* — which a snapshot replacement can race by
    /// closing that connection in the same instant. A caller that tracks
    /// what it has sent must only mark the batch on `true`; a wire-level
    /// failure after queueing is healed by the Agent's anti-entropy beat.
    pub fn send_trust_records(
        &self,
        peer: DeviceId,
        records: Vec<koru_device::TrustRecord>,
    ) -> std::sync::mpsc::Receiver<bool> {
        let (ack, delivered) = std::sync::mpsc::channel();
        if records.is_empty() {
            let _ = ack.send(false);
        } else {
            let _ = self
                .commands
                .send(Command::SendTrustRecords { peer, records, ack });
        }
        delivered
    }

    pub fn complete_trust_record(&self, delivery_id: TrustDeliveryId, accepted: bool) {
        let pending = self
            .pairing
            .pending_trust
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&delivery_id);
        if let Some(pending) = pending {
            let _ = pending.send(accepted);
        }
    }

    pub fn take_pairing_events(&self) -> Vec<PairingEvent> {
        self.pairing
            .events
            .lock()
            .map(|mut events| events.drain(..).collect())
            .unwrap_or_default()
    }

    /// Put back events that were taken for a reply the caller could not send.
    ///
    /// **The drain happens before the write, so a failed write loses them.**
    /// That is how a batch too large for a control frame went missing rather
    /// than merely undelivered: the records were drained, the reply failed to
    /// encode, and nothing had them any more. Raising the frame cap removed
    /// that trigger; it did not remove the shape, because a broken pipe or an
    /// agent killed mid-reply lands in the same place.
    ///
    /// They go back on the **front**: these were queued before anything that
    /// arrived while the reply was in flight, and a pairing event is only
    /// meaningful in the order it happened.
    pub fn restore_pairing_events(&self, events: Vec<PairingEvent>) {
        if events.is_empty() {
            return;
        }
        let mut queue = self
            .pairing
            .events
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        for event in events.into_iter().rev() {
            queue.push_front(event);
        }
    }

    /// **Which devices this session saw advertise each of `hashes`** — the
    /// holder directory, read for a person rather than for routing. A waiting
    /// list names these beside a blob's durable origin, so a device that holds
    /// a copy it did not create is named too. Ids, not names: the caller names
    /// them through the same authorization snapshot it names the origin with.
    /// Never persisted, so empty after a restart until the next inventory.
    pub fn advertised_blob_holders(
        &self,
        workspace: koru_store_core::WorkspaceId,
        hashes: &[BlobHash],
    ) -> HashMap<BlobHash, Vec<DeviceId>> {
        self.blob_directory
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .advertised(workspace, hashes)
    }

    /// Hydrate a missing content-addressed blob from any connected peer allowed
    /// to serve the workspace. Concurrent callers share one transfer. With no
    /// eligible connected peer the ticket waits without a retry timer and is
    /// woken by authorization or connection-state changes.
    pub fn hydrate_blob(
        &self,
        workspace: koru_store_core::WorkspaceId,
        hash: BlobHash,
    ) -> BlobHydrationTicket {
        let key = BlobHydrationKey { workspace, hash };
        let (completion, waiting) = oneshot::channel();
        let (state, start) = {
            let mut hydrations = self
                .blob_hydrations
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if let Some(entry) = hydrations.get_mut(&key) {
                entry.waiters.push(completion);
                (entry.state.subscribe(), false)
            } else {
                let (state, receiver) = watch::channel(BlobHydrationState::CheckingLocal);
                hydrations.insert(
                    key.clone(),
                    BlobHydrationEntry {
                        state,
                        waiters: vec![completion],
                        generation: 0,
                        running: false,
                    },
                );
                (receiver, true)
            }
        };
        if start
            && self
                .commands
                .send(Command::HydrateBlob(key.clone()))
                .is_err()
        {
            hydration::complete_blob_hydration(
                &self.blob_hydrations,
                &key,
                Err(BlobHydrationError::new("runtime_closed", true)),
            );
        }
        BlobHydrationTicket::new(state, waiting)
    }

    /// **§8's peer-first read.** One bounded range of a blob from an authorized
    /// peer whose selected path is an IP path, or `None` — which the blob
    /// engine reads as "go to Cloud" rather than as a failure.
    pub async fn read_blob_range(
        &self,
        workspace: koru_store_core::WorkspaceId,
        hash: BlobHash,
        offset: u64,
        max: u64,
    ) -> Option<PeerBlobRange> {
        let (answer, waiting) = oneshot::channel();
        if self
            .commands
            .send(Command::ReadBlobRange {
                workspace,
                hash,
                offset,
                max,
                answer,
            })
            .is_err()
        {
            return None;
        }
        waiting.await.ok().flatten()
    }

    /// Coalesce a durable local change and ask every connected peer to compare
    /// its authoritative inventory. The notification carries no object data;
    /// normal version comparison remains the source of truth.
    pub fn notify_local_change(&self) {
        let _ = self.commands.send(Command::NotifyLocalChange);
    }

    pub fn clear(&self) {
        let _ = self.commands.send(Command::Clear);
    }

    /// Start a bounded grace period before clearing a released data plane.
    /// An Agent restart can reclaim it with [`Self::retain`] without rebinding
    /// the endpoint or reconnecting every peer.
    pub fn release(&self, grace: Duration) {
        let _ = self.commands.send(Command::Release(grace));
    }

    /// Reclaim a released data plane and cancel its pending clear.
    pub fn retain(&self) {
        let _ = self.commands.send(Command::Retain);
    }

    pub fn status(&self) -> NetworkStatus {
        let mut status = self
            .status
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        let retain_after = state_event_cutoff();
        status
            .last_state_change_millis
            .retain(|_, at| *at >= retain_after);
        status
    }

    /// Who this runtime currently believes holds a blob. Test-only: the
    /// directory is a hint, and production callers should never branch on a
    /// hint.
    #[cfg(test)]
    pub(crate) fn blob_holders(
        &self,
        workspace: koru_store_core::WorkspaceId,
        hash: &BlobHash,
    ) -> Vec<DeviceId> {
        self.blob_directory
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .holders(workspace, hash)
            .map(|holders| holders.iter().copied().collect())
            .unwrap_or_default()
    }

    /// Forget every advertised holder. Test-only: a restart is a fresh
    /// directory over the same store, and a test of durable provenance needs
    /// exactly that without rebuilding the runtime.
    #[cfg(test)]
    pub(crate) fn clear_blob_directory(&self) {
        self.blob_directory
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clear();
    }

    pub fn state_change_page(
        &self,
        peer: DeviceId,
        before: Option<i64>,
    ) -> Result<StateChangePage, protocol::StoreFailure> {
        let Some(store) = self.store.as_deref() else {
            return Ok(StateChangePage::default());
        };
        let mut events = store.state_changes(
            peer,
            state_event_cutoff(),
            before,
            STATE_EVENT_PAGE_SIZE + 1,
        )?;
        let next_before = (events.len() > STATE_EVENT_PAGE_SIZE)
            .then(|| events[STATE_EVENT_PAGE_SIZE - 1].event_id)
            .flatten();
        events.truncate(STATE_EVENT_PAGE_SIZE);
        Ok(StateChangePage {
            events,
            next_before,
        })
    }

    pub fn endpoint_addr(&self) -> Option<EndpointAddr> {
        self.endpoint_addr
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    /// Open one opaque product stream on the authenticated live connection.
    ///
    /// The Device arm is the existing peer dial. The Cloud arm is **not
    /// implemented here**: the Agent's Cloud coordinator, a later increment,
    /// owns resolving its exact in-memory permit, rechecking generations and
    /// current authority, obtaining a credential and a route, confirming the
    /// authenticated remote endpoint equals the route's endpoint, and
    /// completing the handshake. Until it exists this returns
    /// [`koru_network_edge::EdgeError::Closed`] rather than dialing something
    /// on incomplete authority.
    pub async fn open_product_session(
        &self,
        target: koru_network_edge::NetworkTarget,
        protocol: koru_network_edge::ProtocolToken,
    ) -> Result<TransportSession, koru_network_edge::EdgeError> {
        let (answer, waiting) = oneshot::channel();
        self.commands
            .send(Command::OpenProductSession {
                target,
                protocol,
                answer,
            })
            .map_err(|_| koru_network_edge::EdgeError::Closed)?;
        waiting
            .await
            .unwrap_or(Err(koru_network_edge::EdgeError::Closed))
    }

    /// Dial one Cloud worker route on the Cloud ALPN. Nothing is
    /// authenticated here: the caller opens a stream on the returned
    /// connection and runs the four-message handshake with
    /// [`cloud::authenticate`], which is also where the route's endpoint is
    /// compared with the connection's. A remote whose identity is not the
    /// route's endpoint is closed here already and reported
    /// `endpoint_mismatch`, so no handshake byte ever reaches an impostor.
    ///
    /// The route is dialed as issued. With relays switched off the relay
    /// address is dropped from the dial, exactly as the peer dial does, and
    /// the runtime never adds an address of its own.
    pub async fn connect_cloud(
        &self,
        route: CloudRoute,
    ) -> Result<iroh::endpoint::Connection, &'static str> {
        let (answer, waiting) = oneshot::channel();
        self.commands
            .send(Command::ConnectCloud { route, answer })
            .map_err(|_| "network_runtime_stopped")?;
        waiting.await.unwrap_or(Err("network_runtime_stopped"))
    }

    /// Hand one authenticated Cloud stream to the installed opaque ingress
    /// under its Cloud principal and the Cloud content token. This is the
    /// desktop's bridge step: the Agent admits, the gateway's
    /// [`SessionRuntimeHandle::accept`] routes the Cloud principal to its
    /// Cloud acceptor, and the gateway owns the reconciliation pass. The
    /// caller has already had `install_cloud_session` acknowledged; this
    /// runtime cannot check that and does not pretend to. A runtime spawned
    /// without an opaque ingress (Android, the standalone Store) refuses.
    pub async fn admit_cloud_session(
        &self,
        principal: koru_device::cloud::CloudPrincipalV1,
        connection: &iroh::endpoint::Connection,
        send: iroh::endpoint::SendStream,
        recv: iroh::endpoint::RecvStream,
    ) -> Result<(), koru_network_edge::EdgeError> {
        let (answer, waiting) = oneshot::channel();
        self.commands
            .send(Command::AdmitCloudSession {
                principal: Box::new(principal),
                connection_id: connection.stable_id() as u64,
                path: path_kind(connection),
                send,
                recv,
                answer,
            })
            .map_err(|_| koru_network_edge::EdgeError::Closed)?;
        waiting
            .await
            .unwrap_or(Err(koru_network_edge::EdgeError::Closed))
    }
}

/// An issued Cloud dataset route, exactly as `CloudSyncRouteV1` carried it:
/// the worker's endpoint, its direct socket addresses and its relay, if any.
/// The runtime dials it as given and confirms the authenticated remote id is
/// the route's endpoint before handing the connection back; it never
/// substitutes an address of its own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CloudRoute {
    pub endpoint_id: ReplicationEndpointId,
    pub direct_addresses: Vec<std::net::SocketAddr>,
    pub relay_url: Option<String>,
}

/// Store-side replication over transport-neutral sessions supplied by the
/// desktop bridge (or directly by a mobile host). This runtime never creates
/// an Iroh endpoint and never receives an Iroh stream or address.
#[derive(Clone)]
pub struct SessionRuntimeHandle {
    store: Arc<dyn protocol::ReplicationStore>,
    egress: Arc<dyn koru_network_edge::NetworkEgress>,
    authorization: Arc<RwLock<Option<AuthorizationSnapshot>>>,
    authorization_fences: Arc<RwLock<BTreeMap<DeviceId, protocol::AuthorizationFence>>>,
    status: Arc<RwLock<NetworkStatus>>,
    pairing: Arc<PairingHub>,
    connections: Arc<RwLock<BTreeMap<u64, EdgeRequestConnection>>>,
    sync_coordinator: Arc<Mutex<BTreeMap<DeviceId, PeerSyncState>>>,
    blob_hydrations: BlobHydrationCoordinator,
    blob_hydration_slots: Arc<Semaphore>,
    blob_directory: SharedBlobDirectory,
    announcement: Arc<Mutex<SessionAnnouncement>>,
    /// Where a Cloud-principal session goes. Installed by the gateway daemon
    /// once its Cloud coordinator exists; until then a Cloud session is reset
    /// rather than dropped silently or pushed down the peer path.
    cloud_acceptor: Arc<RwLock<Option<Arc<dyn Fn(TransportSession) + Send + Sync>>>>,
}

impl SessionRuntimeHandle {
    pub fn new(
        store: Arc<dyn protocol::ReplicationStore>,
        egress: Arc<dyn koru_network_edge::NetworkEgress>,
    ) -> Self {
        let retain_after = state_event_cutoff();
        let _ = store.prune_state_changes(retain_after);
        let mut status = NetworkStatus::default();
        if let Ok(times) = store.latest_state_change_times(retain_after) {
            status.last_state_change_millis.extend(times);
        }
        Self {
            store,
            egress,
            authorization: Arc::new(RwLock::new(None)),
            authorization_fences: Arc::new(RwLock::new(BTreeMap::new())),
            status: Arc::new(RwLock::new(status)),
            pairing: Arc::new(PairingHub::default()),
            connections: Arc::new(RwLock::new(BTreeMap::new())),
            sync_coordinator: Arc::new(Mutex::new(BTreeMap::new())),
            blob_hydrations: Arc::new(Mutex::new(HashMap::new())),
            blob_hydration_slots: Arc::new(Semaphore::new(
                hydration::MAX_CONCURRENT_BLOB_HYDRATIONS,
            )),
            blob_directory: Arc::new(Mutex::new(hydration::BlobDirectory::new())),
            announcement: Arc::new(Mutex::new(SessionAnnouncement::default())),
            cloud_acceptor: Arc::new(RwLock::new(None)),
        }
    }

    /// Install the owner of Cloud content sessions. Until one is installed a
    /// Cloud-principal session is reset: there is no fall-back onto the peer
    /// path, and no `AuthenticatedPeer` is fabricated for it.
    pub fn set_cloud_acceptor(&self, acceptor: Arc<dyn Fn(TransportSession) + Send + Sync>) {
        *self
            .cloud_acceptor
            .write()
            .unwrap_or_else(|error| error.into_inner()) = Some(acceptor);
    }

    pub fn replace_authorization(&self, snapshot: AuthorizationSnapshot) {
        let unchanged = self
            .authorization
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .is_some_and(|current| current.same_grant(&snapshot));
        if unchanged {
            *self
                .authorization
                .write()
                .unwrap_or_else(|error| error.into_inner()) = Some(snapshot);
            return;
        }
        let fences = std::mem::take(
            &mut *self
                .authorization_fences
                .write()
                .unwrap_or_else(|error| error.into_inner()),
        );
        for (_, fence) in fences {
            fence.revoke();
        }
        *self
            .authorization
            .write()
            .unwrap_or_else(|error| error.into_inner()) = Some(snapshot);
        self.connections
            .write()
            .unwrap_or_else(|error| error.into_inner())
            .clear();
        self.sync_coordinator
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clear();
        self.wake_blob_hydrations();
    }

    pub fn connection_event(&self, event: koru_network_edge::ConnectionEvent) {
        match event {
            koru_network_edge::ConnectionEvent::Connected(metadata) => {
                let Some(peer) = self.authenticated_principal(&metadata.principal) else {
                    return;
                };
                let connection = EdgeRequestConnection {
                    connection_id: metadata.connection_id as usize,
                    peer: peer.device_id,
                    egress: Arc::clone(&self.egress),
                    // Seeded from what the endpoint owner observed when the
                    // connection came up; every session opened afterwards
                    // replaces it with a fresher answer.
                    selected_is_ip: Arc::new(std::sync::atomic::AtomicBool::new(
                        metadata.path.selected_is_ip,
                    )),
                };
                self.connections
                    .write()
                    .unwrap_or_else(|error| error.into_inner())
                    .insert(metadata.connection_id, connection.clone());
                set_peer(
                    &self.status,
                    peer.device_id,
                    PeerConnectionState::Connected {
                        path: metadata.path.kind,
                    },
                );
                if peer.protocol_refusal().is_none() {
                    request_sync(
                        connection,
                        peer,
                        Arc::clone(&self.store),
                        Arc::clone(&self.status),
                        Arc::clone(&self.sync_coordinator),
                        Arc::clone(&self.blob_directory),
                    );
                }
                self.wake_blob_hydrations();
            }
            koru_network_edge::ConnectionEvent::Disconnected { connection_id } => {
                let (removed, still_connected) = {
                    let mut connections = self
                        .connections
                        .write()
                        .unwrap_or_else(|error| error.into_inner());
                    let removed = connections.remove(&connection_id);
                    let still_connected = removed.as_ref().is_some_and(|connection| {
                        connections
                            .values()
                            .any(|live| live.peer == connection.peer)
                    });
                    (removed, still_connected)
                };
                if let Some(connection) = removed {
                    let mut sync = self
                        .sync_coordinator
                        .lock()
                        .unwrap_or_else(|error| error.into_inner());
                    if sync
                        .get(&connection.peer)
                        .is_some_and(|state| state.connection_id == connection.connection_id)
                    {
                        sync.remove(&connection.peer);
                    }
                    drop(sync);
                    if still_connected {
                        return;
                    }
                    set_peer(
                        &self.status,
                        connection.peer,
                        PeerConnectionState::Disconnected,
                    );
                }
            }
        }
    }

    /// **A Cloud session never enters the peer path.** The two are separated
    /// here, at the first branch, rather than by a later check that could be
    /// forgotten: a Cloud principal on the Cloud token goes to the installed
    /// acceptor, a Cloud principal on anything else is reset, and the device
    /// path below only ever sees a device principal.
    pub fn accept(&self, session: TransportSession) {
        if session.principal.is_cloud() {
            if session.protocol != cloud_content_protocol() {
                // A permit cannot authorize a peer protocol token.
                session.reset(1);
                return;
            }
            let acceptor = self
                .cloud_acceptor
                .read()
                .unwrap_or_else(|error| error.into_inner())
                .clone();
            match acceptor {
                Some(acceptor) => acceptor(session),
                // No coordinator is installed, so nothing here owns this
                // stream's lifetime. Resetting says so; dropping it would look
                // like a stall on the other side.
                None => session.reset(1),
            }
            return;
        }
        if session.protocol != replication_protocol() {
            session.reset(1);
            return;
        }
        let Some(peer) = self.authenticated_principal(&session.principal) else {
            session.reset(1);
            return;
        };
        let runtime = self.clone();
        tokio::spawn(async move {
            runtime.serve_session(session, peer).await;
        });
    }

    pub fn notify_local_change(&self) {
        let trace_peer = self
            .connections
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .values()
            .next()
            .map(|connection| connection.peer)
            .or_else(|| {
                self.authorization
                    .read()
                    .unwrap_or_else(|error| error.into_inner())
                    .as_ref()
                    .and_then(|snapshot| snapshot.peers.first())
                    .map(|peer| peer.device_id)
            });
        if let Some(peer) = trace_peer {
            record_sync(
                &self.status,
                peer,
                "change_debounce_reset",
                None,
                None,
                None,
            );
        }

        let mut announcement = self
            .announcement
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        announcement.generation = announcement.generation.wrapping_add(1);
        let generation = announcement.generation;
        let reset = announcement.task.is_some();
        if let Some(task) = announcement.task.take() {
            task.abort();
        }
        tracing::debug!(
            generation,
            reset,
            debounce_ms = CHANGE_ANNOUNCEMENT_DEBOUNCE.as_millis(),
            at_millis = wall_time_millis(),
            "durable change reset session sync announcement debounce"
        );
        let runtime = self.clone();
        announcement.task = Some(tokio::spawn(async move {
            tokio::time::sleep(CHANGE_ANNOUNCEMENT_DEBOUNCE).await;
            runtime.announce_local_changes(generation);
        }));
    }

    fn announce_local_changes(&self, generation: u64) {
        {
            let mut announcement = self
                .announcement
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if generation != announcement.generation {
                tracing::debug!(
                    generation,
                    current_generation = announcement.generation,
                    "stale session sync announcement debounce ignored"
                );
                return;
            }
            announcement.task = None;
        }

        // The edge may briefly report an old and replacement connection at
        // once. An announcement is per peer, and opening the stream through
        // egress always selects the edge's current connection for that peer.
        let connections = self
            .connections
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .values()
            .cloned()
            .fold(BTreeMap::new(), |mut peers, connection| {
                peers.insert(connection.peer, connection);
                peers
            });
        tracing::debug!(
            generation,
            peers = connections.len(),
            quiet_ms = CHANGE_ANNOUNCEMENT_DEBOUNCE.as_millis(),
            at_millis = wall_time_millis(),
            "session sync announcement debounce elapsed"
        );
        if connections.is_empty() {
            if let Some(authorization) = self
                .authorization
                .read()
                .unwrap_or_else(|error| error.into_inner())
                .as_ref()
            {
                for peer in &authorization.peers {
                    record_sync(
                        &self.status,
                        peer.device_id,
                        "change_waiting_for_connection",
                        None,
                        None,
                        None,
                    );
                }
            }
            return;
        }

        for (device_id, connection) in connections {
            record_sync(
                &self.status,
                device_id,
                "change_announcement",
                None,
                None,
                None,
            );
            let status = Arc::clone(&self.status);
            let connection_id = connection.stable_id();
            tokio::spawn(async move {
                let response = tokio::time::timeout(
                    SYNC_REQUEST_TIMEOUT,
                    request_frame(&connection, &protocol::PeerFrame::SyncRequest),
                )
                .await;
                match response {
                    Ok(Ok(protocol::PeerFrame::SyncAck)) => {
                        record_sync(&status, device_id, "change_acknowledged", None, None, None);
                    }
                    response => {
                        record_sync(
                            &status,
                            device_id,
                            "change_announcement_failed",
                            None,
                            None,
                            Some("sync_request"),
                        );
                        tracing::warn!(
                            peer = %device_id.to_storage(),
                            connection_id,
                            outcome = %timed_frame_result_summary(&response),
                            "sync request failed request=session_announcement"
                        );
                    }
                }
            });
        }
    }

    /// **Which devices this session saw advertise each of `hashes`** — the
    /// holder directory, read for a person rather than for routing. A waiting
    /// list names these beside a blob's durable origin, so a device that holds
    /// a copy it did not create is named too. Ids, not names: the caller names
    /// them through the same authorization snapshot it names the origin with.
    /// Never persisted, so empty after a restart until the next inventory.
    pub fn advertised_blob_holders(
        &self,
        workspace: koru_store_core::WorkspaceId,
        hashes: &[BlobHash],
    ) -> HashMap<BlobHash, Vec<DeviceId>> {
        self.blob_directory
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .advertised(workspace, hashes)
    }

    pub fn hydrate_blob(
        &self,
        workspace: koru_store_core::WorkspaceId,
        hash: BlobHash,
    ) -> BlobHydrationTicket {
        let key = BlobHydrationKey { workspace, hash };
        let (completion, waiting) = oneshot::channel();
        let (state, start) = {
            let mut hydrations = self
                .blob_hydrations
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if let Some(entry) = hydrations.get_mut(&key) {
                entry.waiters.push(completion);
                (entry.state.subscribe(), false)
            } else {
                let (state, receiver) = watch::channel(BlobHydrationState::CheckingLocal);
                hydrations.insert(
                    key.clone(),
                    BlobHydrationEntry {
                        state,
                        waiters: vec![completion],
                        generation: 0,
                        running: false,
                    },
                );
                (receiver, true)
            }
        };
        if start {
            self.start_blob_hydration(key);
        }
        BlobHydrationTicket::new(state, waiting)
    }

    /// **§8's peer-first read**, on the bridged shape. The same answer as the
    /// in-process runtime's, reached through the edge.
    pub async fn read_blob_range(
        &self,
        workspace: koru_store_core::WorkspaceId,
        hash: BlobHash,
        offset: u64,
        max: u64,
    ) -> Option<PeerBlobRange> {
        let workspace_profile = self.store.workspace_profile(workspace).ok();
        let authorized = self
            .authorization
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .map(|snapshot| {
                snapshot
                    .peers
                    .iter()
                    .filter(|peer| {
                        peer.all_data
                            || peer.workspaces.iter().any(|id| *id == workspace)
                            || workspace_profile
                                .is_some_and(|profile| peer.authorized_profiles.contains(&profile))
                    })
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let mut attempts: Vec<(protocol::AuthenticatedPeer, EdgeRequestConnection)> = Vec::new();
        {
            let connections = self
                .connections
                .read()
                .unwrap_or_else(|error| error.into_inner());
            for peer in &authorized {
                let peer =
                    authenticated_peer(peer, &self.authorization_fences, &self.authorization);
                if peer.protocol_refusal().is_some() {
                    continue;
                }
                if let Some(connection) = connections
                    .values()
                    .find(|held| held.peer == peer.device_id)
                    .cloned()
                {
                    attempts.push((peer, connection));
                }
            }
        }
        read_blob_range_from(attempts, workspace, hash, offset, max).await
    }

    fn wake_blob_hydrations(&self) {
        let keys = self
            .blob_hydrations
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .iter()
            .filter(|(_, entry)| !entry.running)
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        for key in keys {
            self.start_blob_hydration(key);
        }
    }

    fn start_blob_hydration(&self, key: BlobHydrationKey) {
        let workspace_profile = self.store.workspace_profile(key.workspace).ok();
        let authorized = self
            .authorization
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .map(|snapshot| {
                snapshot
                    .peers
                    .iter()
                    .filter(|peer| {
                        peer.all_data
                            || peer.workspaces.iter().any(|id| *id == key.workspace)
                            || workspace_profile
                                .is_some_and(|profile| peer.authorized_profiles.contains(&profile))
                    })
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if authorized.is_empty() {
            hydration::set_blob_hydration_waiting(
                &self.blob_hydrations,
                &key,
                BlobWaitReason::NoLinkedDevices,
            );
            return;
        }
        // Authenticate every authorized peer, not only the connected ones: a
        // peer on another protocol number is not a source when online, and is
        // not a reason to wait when offline either.
        let candidates: Vec<protocol::AuthenticatedPeer> = authorized
            .iter()
            .map(|peer| authenticated_peer(peer, &self.authorization_fences, &self.authorization))
            .filter(|peer| peer.protocol_refusal().is_none())
            .collect();
        let mut by_id: HashMap<DeviceId, (protocol::AuthenticatedPeer, EdgeRequestConnection)> =
            HashMap::new();
        {
            let connections = self
                .connections
                .read()
                .unwrap_or_else(|error| error.into_inner());
            for peer in &candidates {
                if let Some(connection) = connections
                    .values()
                    .find(|connection| connection.peer == peer.device_id)
                    .cloned()
                {
                    by_id.insert(peer.device_id, (peer.clone(), connection));
                }
            }
        }
        let connected: BTreeSet<DeviceId> = by_id.keys().copied().collect();
        let directory_holders = self
            .blob_directory
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .holders(key.workspace, &key.hash)
            .cloned();
        // The durable origin can name a holder the directory has not seen this
        // session — after a restart, before the first inventory arrives.
        let origin = candidates
            .iter()
            .find_map(|peer| self.store.blob_origin(peer, key.workspace, &key.hash))
            .and_then(|id| DeviceId::parse(&id));
        let (order, park) = match hydration::plan_blob_hydration(
            &candidates,
            &connected,
            directory_holders.as_ref(),
            origin,
        ) {
            hydration::HydrationPlan::Wait(reason) => {
                hydration::set_blob_hydration_waiting(&self.blob_hydrations, &key, reason);
                return;
            }
            hydration::HydrationPlan::Attempt { order, park } => (order, park),
        };
        tracing::info!(
            workspace = %key.workspace.to_storage(),
            attempts = order.len(),
            holders_known = directory_holders.as_ref().map_or(0, BTreeSet::len),
            origin_known = origin.is_some(),
            "blob_hydration action=plan"
        );
        let attempts = order
            .into_iter()
            .filter_map(|device| by_id.remove(&device))
            .collect::<Vec<_>>();
        let (generation, state) = {
            let mut hydrations = self
                .blob_hydrations
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let Some(entry) = hydrations.get_mut(&key) else {
                return;
            };
            if entry.running {
                return;
            }
            entry.generation = entry.generation.saturating_add(1);
            entry.running = true;
            (entry.generation, entry.state.clone())
        };
        let runtime = self.clone();
        tokio::spawn(async move {
            let outcome = match runtime.blob_hydration_slots.clone().acquire_owned().await {
                Ok(_permit) => {
                    hydration::hydrate_blob_from_peers(
                        &key,
                        attempts,
                        park,
                        Arc::clone(&runtime.store),
                        &state,
                        Arc::clone(&runtime.status),
                        Arc::clone(&runtime.blob_directory),
                    )
                    .await
                }
                Err(_) => {
                    BlobHydrationOutcome::Failed(BlobHydrationError::new("runtime_closed", true))
                }
            };
            let current = runtime
                .blob_hydrations
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .get(&key)
                .is_some_and(|entry| entry.generation == generation);
            if !current {
                return;
            }
            match outcome {
                BlobHydrationOutcome::Available => {
                    hydration::complete_blob_hydration(&runtime.blob_hydrations, &key, Ok(()));
                }
                BlobHydrationOutcome::WaitingForPeer(reason) => {
                    hydration::set_blob_hydration_waiting(&runtime.blob_hydrations, &key, reason);
                }
                BlobHydrationOutcome::Failed(error) => {
                    hydration::complete_blob_hydration(&runtime.blob_hydrations, &key, Err(error));
                }
            }
        });
    }

    pub fn status(&self) -> NetworkStatus {
        self.status
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    pub fn state_change_page(
        &self,
        peer: DeviceId,
        before: Option<i64>,
    ) -> Result<StateChangePage, protocol::StoreFailure> {
        let mut events = self.store.state_changes(
            peer,
            state_event_cutoff(),
            before,
            STATE_EVENT_PAGE_SIZE + 1,
        )?;
        let next_before = (events.len() > STATE_EVENT_PAGE_SIZE)
            .then(|| events[STATE_EVENT_PAGE_SIZE - 1].event_id)
            .flatten();
        events.truncate(STATE_EVENT_PAGE_SIZE);
        Ok(StateChangePage {
            events,
            next_before,
        })
    }

    pub fn take_pairing_events(&self) -> Vec<PairingEvent> {
        self.pairing
            .events
            .lock()
            .map(|mut events| events.drain(..).collect())
            .unwrap_or_default()
    }

    pub fn restore_pairing_events(&self, events: Vec<PairingEvent>) {
        let mut queue = self
            .pairing
            .events
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        for event in events.into_iter().rev() {
            queue.push_front(event);
        }
    }

    pub fn complete_trust_record(&self, delivery_id: TrustDeliveryId, accepted: bool) {
        if let Some(waiting) = self
            .pairing
            .pending_trust
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&delivery_id)
        {
            let _ = waiting.send(accepted);
        }
    }

    /// **Explicitly rejects Cloud, never downcasts it.** There is no
    /// projection from a Cloud authority context to a bilateral device trust
    /// principal, and inventing one here is exactly the mistake the union at
    /// the edge exists to make impossible.
    fn authenticated_principal(
        &self,
        principal: &koru_network_edge::NetworkPrincipal,
    ) -> Option<protocol::AuthenticatedPeer> {
        let principal = principal.device()?;
        let peer = self
            .authorization
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()?
            .peers
            .iter()
            .find(|peer| {
                peer.device_id == principal.device_id
                    && peer.relationship_id == principal.relationship_id
                    && peer.endpoint_id == principal.endpoint_id
            })
            .cloned()?;
        Some(authenticated_peer(
            &peer,
            &self.authorization_fences,
            &self.authorization,
        ))
    }

    async fn serve_session(
        &self,
        mut session: TransportSession,
        peer: protocol::AuthenticatedPeer,
    ) {
        let frame = match read_edge_frame(&mut session.receive).await {
            Ok(frame) => frame,
            Err(_) => {
                if let Some(code) = peer.protocol_refusal() {
                    let _ = send_edge_frame(
                        &session.send,
                        &protocol::PeerFrame::Error { code: code.into() },
                    )
                    .await;
                    return;
                }
                session.reset(1);
                return;
            }
        };
        let is_trust_frame = matches!(
            &frame,
            protocol::PeerFrame::TrustRecord { .. } | protocol::PeerFrame::TrustRecords { .. }
        );
        if let Some(code) = peer.protocol_refusal().filter(|_| !is_trust_frame) {
            let _ = send_edge_frame(
                &session.send,
                &protocol::PeerFrame::Error { code: code.into() },
            )
            .await;
            return;
        }
        if matches!(&frame, protocol::PeerFrame::SyncRequest) {
            let _ = send_edge_frame(&session.send, &protocol::PeerFrame::SyncAck).await;
            let connection = self
                .connections
                .read()
                .unwrap_or_else(|error| error.into_inner())
                .get(&session.connection_id)
                .cloned()
                .unwrap_or_else(|| EdgeRequestConnection {
                    connection_id: session.connection_id as usize,
                    peer: peer.device_id,
                    egress: Arc::clone(&self.egress),
                    // No connect event was seen for this one, so nothing has
                    // been observed about its selected path. Blob bytes wait
                    // for the first session that says otherwise.
                    selected_is_ip: Arc::new(std::sync::atomic::AtomicBool::new(
                        session.path.selected_is_ip,
                    )),
                });
            request_sync(
                connection,
                peer,
                Arc::clone(&self.store),
                Arc::clone(&self.status),
                Arc::clone(&self.sync_coordinator),
                Arc::clone(&self.blob_directory),
            );
            return;
        }
        if is_trust_frame {
            let delivery_id = TrustDeliveryId::new();
            let (answer, waiting) = oneshot::channel();
            self.pairing
                .pending_trust
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .insert(delivery_id, answer);
            match frame {
                protocol::PeerFrame::TrustRecord { record } => {
                    self.pairing.event(PairingEvent::TrustRecordReceived {
                        delivery_id,
                        from: peer.device_id,
                        record,
                    });
                }
                protocol::PeerFrame::TrustRecords { records } => {
                    self.pairing.event(PairingEvent::TrustRecordsReceived {
                        delivery_id,
                        from: peer.device_id,
                        records,
                    });
                }
                _ => unreachable!(),
            }
            let accepted = tokio::time::timeout(Duration::from_secs(30), waiting)
                .await
                .ok()
                .and_then(Result::ok)
                .unwrap_or(false);
            let response = if accepted {
                protocol::PeerFrame::TrustAck
            } else {
                protocol::PeerFrame::Error {
                    code: "trust_record_rejected".into(),
                }
            };
            let _ = send_edge_frame(&session.send, &response).await;
            return;
        }

        // **§6's third point, fenced by D246**, on the bridged shape. The
        // selected-path fact arrives with the session the endpoint owner opened
        // for this frame, so it is as fresh as one request.
        if matches!(frame, protocol::PeerFrame::BlobRequest { .. }) && !session.path.selected_is_ip
        {
            let refusal = protocol::PeerFrame::Error {
                code: blob_path::RELAY_PATH_REFUSED.into(),
            };
            let _ = send_edge_frame(&session.send, &refusal).await;
            return;
        }
        let state_targets = protocol::doc_update_targets(self.store.as_ref(), &peer, &frame)
            .into_iter()
            .map(|(workspace, document, updates)| {
                let before = self
                    .store
                    .version(&peer, workspace, document)
                    .unwrap_or_default();
                (workspace, document, updates, before)
            })
            .collect::<Vec<_>>();
        let responses = protocol::handle_frame(self.store.as_ref(), &peer, frame);
        for (workspace, document, updates, before) in state_targets {
            let after = self
                .store
                .version(&peer, workspace, document)
                .unwrap_or_default();
            if after != before {
                let is_manifest = self.store.manifest_id(&peer, workspace).ok() == Some(document);
                record_state_change(
                    &self.status,
                    Some(self.store.as_ref()),
                    peer.device_id,
                    if is_manifest {
                        "workspace_updated"
                    } else {
                        "document_updated"
                    },
                    None,
                    Some(workspace),
                    Some(document),
                    self.store.document_name(&peer, workspace, document),
                    None,
                    None,
                    imported_op_count(&before, &after).or(Some(updates)),
                );
            }
        }
        for response in responses {
            if send_edge_frame(&session.send, &response).await.is_err() {
                session.reset(1);
                return;
            }
        }
    }
}

enum Command {
    ProvisionEndpoint([u8; 32]),
    ReplaceRelayProvider(RelayProviderConfig),
    ReplaceAuthorization(AuthorizationSnapshot),
    OpenProductSession {
        target: koru_network_edge::NetworkTarget,
        protocol: koru_network_edge::ProtocolToken,
        answer: oneshot::Sender<Result<TransportSession, koru_network_edge::EdgeError>>,
    },
    Release(Duration),
    Retain,
    ClearIfReleased(u64),
    ConnectPeer {
        device_id: DeviceId,
        exact_addr: Option<EndpointAddr>,
    },
    DisconnectPeer(DeviceId),
    RetryPeer {
        device_id: DeviceId,
        generation: u64,
        retry: u64,
    },
    ConnectionAttemptFinished {
        device_id: DeviceId,
        generation: u64,
        peer: AuthorizedPeer,
        outcome: ConnectionAttemptOutcome,
    },
    InboundConnection {
        peer: AuthorizedPeer,
        connection: iroh::endpoint::Connection,
    },
    PeerClosed {
        device_id: DeviceId,
        connection_id: usize,
        direction: ConnectionDirection,
        path: PathKind,
        connected_at: std::time::Instant,
        reason: String,
    },
    HydrateBlob(BlobHydrationKey),
    /// §8's peer-first read: one bounded range from an authorized peer on a
    /// direct path, or `None`, which is the fall-through to Cloud.
    ReadBlobRange {
        workspace: koru_store_core::WorkspaceId,
        hash: BlobHash,
        offset: u64,
        max: u64,
        answer: oneshot::Sender<Option<PeerBlobRange>>,
    },
    BlobHydrationFinished {
        key: BlobHydrationKey,
        generation: u64,
        outcome: BlobHydrationOutcome,
    },
    JoinPairing {
        join: Box<koru_pairing::PairingJoinV1>,
        exact_addr: Option<EndpointAddr>,
    },
    OpenEnrollment {
        expected_endpoint: ReplicationEndpointId,
        addr: EndpointAddr,
        answer: oneshot::Sender<Result<EnrollmentSession, EnrollmentTransportError>>,
    },
    SendTrustRecord {
        peer: DeviceId,
        record: Box<koru_device::TrustRecord>,
    },
    SendTrustRecords {
        peer: DeviceId,
        records: Vec<koru_device::TrustRecord>,
        ack: std::sync::mpsc::Sender<bool>,
    },
    NotifyLocalChange,
    AnnounceLocalChanges {
        generation: u64,
    },
    Clear,
    ConnectCloud {
        route: CloudRoute,
        answer: oneshot::Sender<Result<iroh::endpoint::Connection, &'static str>>,
    },
    AdmitCloudSession {
        principal: Box<koru_device::cloud::CloudPrincipalV1>,
        connection_id: u64,
        path: PathKind,
        send: iroh::endpoint::SendStream,
        recv: iroh::endpoint::RecvStream,
        answer: oneshot::Sender<Result<(), koru_network_edge::EdgeError>>,
    },
}

enum ConnectionAttemptOutcome {
    Connected(iroh::endpoint::Connection),
    IdentityMismatch(iroh::endpoint::Connection),
    /// The dial failed. `code` is a stable snake_case diagnostic set only
    /// when the endpoint's own state is evidence for the cause (see
    /// [`dial_failure_code`]); `None` means the cause is not distinguishable
    /// and the peer retries under plain backoff, exactly as before.
    Failed {
        code: Option<&'static str>,
    },
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum ConnectionDirection {
    Inbound,
    Outbound,
}

impl ConnectionDirection {
    fn as_str(self) -> &'static str {
        match self {
            Self::Inbound => "inbound",
            Self::Outbound => "outbound",
        }
    }

    fn outbound(self) -> bool {
        matches!(self, Self::Outbound)
    }
}

#[derive(Clone)]
struct LiveConnection {
    connection: iroh::endpoint::Connection,
    direction: ConnectionDirection,
    path: PathKind,
    connected_at: std::time::Instant,
}

impl LiveConnection {
    fn id(&self) -> usize {
        self.connection.stable_id()
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum ConnectionChoice {
    KeepCurrent,
    UseCandidate,
}

/// Both peers may dial at the same time. Pick the same physical connection at
/// both ends: the lower device id owns the outbound side and the higher device
/// id owns the corresponding inbound side. This collapses crossed desktop
/// dials without making either device a permanent server.
///
/// **A same-direction candidate replaces the current connection**, and this
/// reverses the first cut of 8d16aa1, which rejected it as a duplicate. The
/// hole that decision left: QUIC takes minutes to notice a dead idle session,
/// so after one side restarts, every peer still "has" a connection to the old
/// process and rejected the new process's dials — a whole mesh answering
/// `duplicate peer connection` every few seconds until the five-minute
/// timeout, observed live within hours of shipping. A second session of the
/// same orientation from the same authenticated device is the remote saying
/// its previous session is gone — a restart, or a re-dial after a failure it
/// can see and this side cannot — and the replace path closes the old
/// connection explicitly, so nothing is orphaned. Crossed dials keep the
/// deterministic device-id rule below, unchanged.
fn choose_connection(
    local: DeviceId,
    remote: DeviceId,
    current: ConnectionDirection,
    candidate: ConnectionDirection,
) -> ConnectionChoice {
    if current == candidate {
        return ConnectionChoice::UseCandidate;
    }
    let preferred = if local < remote {
        ConnectionDirection::Outbound
    } else {
        ConnectionDirection::Inbound
    };
    if candidate == preferred {
        ConnectionChoice::UseCandidate
    } else {
        ConnectionChoice::KeepCurrent
    }
}

#[derive(Default)]
struct PairingHub {
    invitation: RwLock<Option<koru_pairing::PairingInvitationV1>>,
    claimed: Mutex<bool>,
    events: Mutex<VecDeque<PairingEvent>>,
    pending: Mutex<
        HashMap<PairingAttemptId, oneshot::Sender<Result<koru_pairing::PreparedPairingV1, String>>>,
    >,
    pending_trust: Mutex<HashMap<TrustDeliveryId, oneshot::Sender<bool>>>,
}

impl PairingHub {
    fn event(&self, event: PairingEvent) {
        self.events
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push_back(event);
    }

    fn cancel(&self, code: &str) {
        *self
            .invitation
            .write()
            .unwrap_or_else(|error| error.into_inner()) = None;
        *self
            .claimed
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = false;
        let pending = std::mem::take(
            &mut *self
                .pending
                .lock()
                .unwrap_or_else(|error| error.into_inner()),
        );
        for (_, response) in pending {
            let _ = response.send(Err(code.to_owned()));
        }
        let pending_trust = std::mem::take(
            &mut *self
                .pending_trust
                .lock()
                .unwrap_or_else(|error| error.into_inner()),
        );
        for (_, response) in pending_trust {
            let _ = response.send(false);
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct PairingWireResponse {
    prepared: Option<Box<koru_pairing::PreparedPairingV1>>,
    error_code: Option<String>,
}

#[derive(Clone)]
struct ConnectionIntent {
    exact_addr: Option<EndpointAddr>,
    generation: u64,
    failures: u8,
    /// The one retry allowed to dial. Every scheduled retry takes a new
    /// number and only a `RetryPeer` carrying the current one dials, so
    /// scheduling a retry *replaces* the pending one. Before this, every
    /// close of a connection spawned another timer beside the ones already
    /// waiting, and against a peer that kills each connection on arrival
    /// they multiplied into hundreds firing together.
    retry: u64,
    /// When the current connection was established, so its close can be told
    /// apart as *died on first use* (a failure, backs off) from *proved
    /// itself and dropped* (a blip, starts fresh).
    connected_at: Option<std::time::Instant>,
}

struct RuntimeState {
    config: IrohNetworkConfig,
    endpoint: Option<Endpoint>,
    /// The provisioned node secret, kept so a live relay switch can rebind
    /// the endpoint under the new configuration with the *same* identity.
    /// Never logged, never persisted here — Agent owns it at rest.
    secret: Option<[u8; 32]>,
    /// Whether the bound endpoint was built *with* relay transport —
    /// recorded at bind time from the provider it was bound under, never
    /// inferred later. An endpoint bound with `RelayMode::Disabled` has no
    /// relay transport at all (`guide/contributing/deps/IROH_RELAY.md`), so the live
    /// relay switch can only edit the relay map in place when this is
    /// true; otherwise switching relays on must still rebind.
    endpoint_has_relay_transport: bool,
    authorization: Arc<RwLock<Option<AuthorizationSnapshot>>>,
    authorization_fences: Arc<RwLock<BTreeMap<DeviceId, protocol::AuthorizationFence>>>,
    connections: Arc<RwLock<BTreeMap<DeviceId, LiveConnection>>>,
    status: Arc<RwLock<NetworkStatus>>,
    endpoint_addr: Arc<RwLock<Option<EndpointAddr>>>,
    pairing: Arc<PairingHub>,
    enrollment: Arc<EnrollmentHub>,
    store: Option<Arc<dyn protocol::ReplicationStore>>,
    commands: mpsc::UnboundedSender<Command>,
    desired: BTreeMap<DeviceId, ConnectionIntent>,
    /// One outbound dial per device generation. Dials are network waits and
    /// therefore run outside the command actor; this map makes repeated retry
    /// commands idempotent while allowing a newer generation to supersede an
    /// older in-flight attempt.
    active_connection_attempts: BTreeMap<DeviceId, u64>,
    lifecycle_logs: BTreeMap<DeviceId, LifecycleLogState>,
    /// Peers whose protocol this build refuses, and when each may next be
    /// reported — see [`reconnect::IncompatibleReports`].
    incompatible_reports: IncompatibleReports,
    next_generation: u64,
    /// A delayed clear only runs while it still names the latest release.
    release_epoch: u64,
    announcement_generation: u64,
    announcement_task: Option<tokio::task::JoinHandle<()>>,
    /// One watch on the bound endpoint's [`Endpoint::home_relay_status`],
    /// writing [`NetworkStatus::relay`] on every change. Replaced by
    /// [`RuntimeState::rebind_endpoint`] and aborted by it and by
    /// [`RuntimeState::clear`], so it dies with the endpoint it watches —
    /// the `announcement_task` arrangement.
    relay_status_task: Option<tokio::task::JoinHandle<()>>,
    sync_coordinator: Arc<Mutex<BTreeMap<DeviceId, PeerSyncState>>>,
    blob_hydrations: BlobHydrationCoordinator,
    blob_hydration_slots: Arc<Semaphore>,
    blob_directory: SharedBlobDirectory,
    opaque_ingress: Option<Arc<dyn NetworkIngress>>,
}

#[derive(Default)]
struct LifecycleLogState {
    connections: VecDeque<Instant>,
    rapid_reported: bool,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum LifecycleLogDecision {
    Ordinary,
    Rapid { connections: usize },
}

impl LifecycleLogState {
    fn connected(&mut self, now: Instant) -> LifecycleLogDecision {
        while self
            .connections
            .front()
            .is_some_and(|connected| now.duration_since(*connected) >= RAPID_CONNECTION_WINDOW)
        {
            self.connections.pop_front();
        }
        if self.connections.len() <= RAPID_CONNECTION_THRESHOLD {
            self.rapid_reported = false;
        }
        self.connections.push_back(now);
        if self.connections.len() > RAPID_CONNECTION_THRESHOLD && !self.rapid_reported {
            self.rapid_reported = true;
            LifecycleLogDecision::Rapid {
                connections: self.connections.len(),
            }
        } else {
            LifecycleLogDecision::Ordinary
        }
    }
}

#[derive(Clone, Debug)]
struct PeerSyncState {
    connection_id: usize,
    running: bool,
    dirty: bool,
    wake_generation: u64,
    wake: watch::Sender<u64>,
}

impl Default for PeerSyncState {
    fn default() -> Self {
        let (wake, _) = watch::channel(0);
        Self {
            connection_id: 0,
            running: false,
            dirty: false,
            wake_generation: 0,
            wake,
        }
    }
}

#[derive(Default)]
struct SessionAnnouncement {
    generation: u64,
    task: Option<tokio::task::JoinHandle<()>>,
}

async fn run(
    config: IrohNetworkConfig,
    mut commands: mpsc::UnboundedReceiver<Command>,
    command_sender: mpsc::UnboundedSender<Command>,
    status: Arc<RwLock<NetworkStatus>>,
    endpoint_addr: Arc<RwLock<Option<EndpointAddr>>>,
    pairing: Arc<PairingHub>,
    enrollment: Arc<EnrollmentHub>,
    blob_hydrations: BlobHydrationCoordinator,
    blob_directory: SharedBlobDirectory,
    store: Option<Arc<dyn protocol::ReplicationStore>>,
    opaque_ingress: Option<Arc<dyn NetworkIngress>>,
) {
    let mut state = RuntimeState {
        config,
        endpoint: None,
        secret: None,
        endpoint_has_relay_transport: false,
        authorization: Arc::new(RwLock::new(None)),
        authorization_fences: Arc::new(RwLock::new(BTreeMap::new())),
        connections: Arc::new(RwLock::new(BTreeMap::new())),
        status,
        endpoint_addr,
        pairing,
        enrollment,
        store,
        commands: command_sender,
        desired: BTreeMap::new(),
        active_connection_attempts: BTreeMap::new(),
        lifecycle_logs: BTreeMap::new(),
        incompatible_reports: IncompatibleReports::default(),
        next_generation: 0,
        release_epoch: 0,
        announcement_generation: 0,
        announcement_task: None,
        relay_status_task: None,
        sync_coordinator: Arc::new(Mutex::new(BTreeMap::new())),
        blob_hydrations,
        blob_hydration_slots: Arc::new(Semaphore::new(hydration::MAX_CONCURRENT_BLOB_HYDRATIONS)),
        blob_directory,
        opaque_ingress,
    };
    while let Some(command) = commands.recv().await {
        match command {
            Command::ProvisionEndpoint(secret) => state.provision(secret).await,
            Command::ReplaceRelayProvider(provider) => {
                state.replace_relay_provider(provider).await;
            }
            Command::ReplaceAuthorization(snapshot) => state.authorize(snapshot),
            Command::OpenProductSession {
                target,
                protocol,
                answer,
            } => state.open_product_session(target, protocol, answer),
            Command::ConnectPeer {
                device_id,
                exact_addr,
            } => {
                if exact_addr.is_none() && state.is_connected(device_id) {
                    state.adopt_live_connection(device_id);
                } else {
                    state.request_connection(device_id, exact_addr);
                    state.start_connect(device_id);
                }
            }
            Command::DisconnectPeer(device_id) => state.disconnect(device_id),
            Command::RetryPeer {
                device_id,
                generation,
                retry,
            } => {
                if state
                    .desired
                    .get(&device_id)
                    .is_some_and(|intent| intent.generation == generation && intent.retry == retry)
                {
                    state.start_connect(device_id);
                }
            }
            Command::ConnectionAttemptFinished {
                device_id,
                generation,
                peer,
                outcome,
            } => state.connection_attempt_finished(device_id, generation, peer, outcome),
            Command::InboundConnection { peer, connection } => {
                state.inbound_connection(peer, connection);
            }
            Command::PeerClosed {
                device_id,
                connection_id,
                direction,
                path,
                connected_at,
                reason,
            } => state.peer_closed(
                device_id,
                connection_id,
                direction,
                path,
                connected_at,
                &reason,
            ),
            Command::HydrateBlob(key) => state.schedule_blob_hydration(key),
            Command::ReadBlobRange {
                workspace,
                hash,
                offset,
                max,
                answer,
            } => state.read_blob_range(workspace, hash, offset, max, answer),
            Command::BlobHydrationFinished {
                key,
                generation,
                outcome,
            } => state.blob_hydration_finished(key, generation, outcome),
            Command::JoinPairing { join, exact_addr } => state.join_pairing(*join, exact_addr),
            Command::OpenEnrollment {
                expected_endpoint,
                addr,
                answer,
            } => state.open_enrollment(expected_endpoint, addr, answer),
            Command::SendTrustRecord { peer, record } => state.send_trust_record(peer, *record),
            Command::SendTrustRecords { peer, records, ack } => {
                state.send_trust_records(peer, records, ack);
            }
            Command::NotifyLocalChange => state.schedule_change_announcement(),
            Command::AnnounceLocalChanges { generation } => {
                state.announce_local_changes(generation);
            }
            Command::Release(grace) => state.release(grace),
            Command::Retain => state.retain(),
            Command::ClearIfReleased(epoch) => {
                if state.release_epoch == epoch {
                    state.clear().await;
                }
            }
            Command::Clear => state.clear().await,
            Command::ConnectCloud { route, answer } => state.connect_cloud(route, answer),
            Command::AdmitCloudSession {
                principal,
                connection_id,
                path,
                send,
                recv,
                answer,
            } => state.admit_cloud_session(*principal, connection_id, path, send, recv, answer),
        }
    }
    state.clear().await;
}

impl RuntimeState {
    fn schedule_change_announcement(&mut self) {
        self.announcement_generation = self.announcement_generation.saturating_add(1);
        let generation = self.announcement_generation;
        let reset = self.announcement_task.is_some();
        if let Some(task) = self.announcement_task.take() {
            task.abort();
        }
        let trace_peer = self
            .connections
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .keys()
            .next()
            .copied()
            .or_else(|| self.desired.keys().next().copied());
        if let Some(peer) = trace_peer {
            // Reaching this point means an opaque editor update has already
            // crossed the Store's durable commit boundary. Keep this event
            // visible for the entire quiet period so the 100 ms Agent status
            // sampler cannot miss the boundary we most need when diagnosing
            // propagation latency.
            record_sync(
                &self.status,
                peer,
                "change_debounce_reset",
                None,
                None,
                None,
            );
        }
        // **Debug, not info.** One per durable commit, which is one per
        // keystroke once the debounce is running -- routine by construction,
        // and `guide/contributing/PRINCIPLES.md` says report news rather than activity. It
        // still reaches the rotated log at debug level when somebody is asking.
        tracing::debug!(
            generation,
            reset,
            debounce_ms = CHANGE_ANNOUNCEMENT_DEBOUNCE.as_millis(),
            at_millis = wall_time_millis(),
            "durable change reset sync announcement debounce"
        );
        let commands = self.commands.clone();
        self.announcement_task = Some(tokio::spawn(async move {
            tokio::time::sleep(CHANGE_ANNOUNCEMENT_DEBOUNCE).await;
            let _ = commands.send(Command::AnnounceLocalChanges { generation });
        }));
    }

    fn announce_local_changes(&mut self, generation: u64) {
        // Aborting a timer and receiving its already-queued command can race.
        // Only the newest quiet-period generation may announce durable state.
        if generation != self.announcement_generation {
            tracing::debug!(
                generation,
                current_generation = self.announcement_generation,
                "stale sync announcement debounce ignored"
            );
            return;
        }
        self.announcement_task = None;
        let connections: Vec<_> = self
            .connections
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .iter()
            .map(|(device_id, live)| (*device_id, live.connection.clone()))
            .collect();
        // Its partner above, and the same argument: one per quiet period.
        tracing::debug!(
            generation,
            peers = connections.len(),
            quiet_ms = CHANGE_ANNOUNCEMENT_DEBOUNCE.as_millis(),
            at_millis = wall_time_millis(),
            "sync announcement debounce elapsed"
        );
        if connections.is_empty() {
            // A reconnect always performs a bidirectional authoritative pull,
            // so there is nothing to queue separately while every peer is
            // offline. This event explains why no live announcement followed
            // a durable local commit.
            for device_id in self.desired.keys().copied() {
                record_sync(
                    &self.status,
                    device_id,
                    "change_waiting_for_connection",
                    None,
                    None,
                    None,
                );
            }
            return;
        }
        for (device_id, connection) in connections {
            record_sync(
                &self.status,
                device_id,
                "change_announcement",
                None,
                None,
                None,
            );
            let status = Arc::clone(&self.status);
            let connection_id = connection.stable_id();
            tokio::spawn(async move {
                let response = tokio::time::timeout(
                    SYNC_REQUEST_TIMEOUT,
                    request_frame(&connection, &protocol::PeerFrame::SyncRequest),
                )
                .await;
                match response {
                    Ok(Ok(protocol::PeerFrame::SyncAck)) => {
                        record_sync(&status, device_id, "change_acknowledged", None, None, None);
                    }
                    response => {
                        // A failed request stream is not proof that the QUIC
                        // connection is dead. Its close watcher owns that
                        // decision; the next durable hint or reconnect will
                        // retry authoritative comparison.
                        record_sync(
                            &status,
                            device_id,
                            "change_announcement_failed",
                            None,
                            None,
                            Some("sync_request"),
                        );
                        tracing::warn!(
                            peer = %device_id.to_storage(),
                            connection_id,
                            outcome = %timed_frame_result_summary(&response),
                            "sync request failed request=announcement"
                        );
                    }
                }
            });
        }
    }

    async fn provision(&mut self, secret: [u8; 32]) {
        // Provisioning claims the runtime, including the idempotent fast path.
        self.release_epoch = self.release_epoch.wrapping_add(1);
        let key = SecretKey::from_bytes(&secret);
        self.secret = Some(secret);
        if self
            .endpoint
            .as_ref()
            .is_some_and(|ep| ep.id() == key.public())
        {
            return;
        }
        self.rebind_endpoint(key, b"endpoint replaced").await;
    }

    /// The live relay switch (decision №6): install the new provider with
    /// as little disturbance as the endpoint allows. Asymmetric by
    /// necessity (`guide/contributing/deps/IROH_RELAY.md`):
    ///
    /// - **Off, on an endpoint bound with relay transport** — every shipped
    ///   configuration binds with `CustomUrls` — edits the live relay map
    ///   and closes only the relay-only connections
    ///   ([`Self::disable_relays_in_place`]). No rebind: a direct/LAN
    ///   connection is never touched, which is the product contract.
    /// - **On, onto that same endpoint** — inserts the configured relays
    ///   back and wakes only the waiting peers
    ///   ([`Self::enable_relays_in_place`]).
    /// - **On, but the endpoint was bound with relays disabled**: an
    ///   endpoint built with `RelayMode::Disabled` has no relay transport
    ///   at all (`From<RelayMode>` maps it to no `TransportConfig`), so
    ///   there is nothing to insert into — this one transition still
    ///   rebinds under the same provisioned secret (identity unchanged)
    ///   and reconnects every desired peer through the retry machinery.
    /// - Anything else — `DefaultNetwork` on either side (never produced
    ///   by Koru's configuration rows), a changed custom list, a runtime
    ///   whose bind failed — keeps the rebind, the always-correct fallback.
    async fn replace_relay_provider(&mut self, provider: RelayProviderConfig) {
        if self.config.relay_provider == provider {
            return;
        }
        let previous = std::mem::replace(&mut self.config.relay_provider, provider);
        let Some(secret) = self.secret else {
            // Not provisioned yet: the stored configuration simply applies
            // when the endpoint is first provisioned. The status still
            // follows the switch — there is no endpoint to watch, so the
            // configuration's baseline is the whole reading.
            set_relay_status(
                &self.status,
                relay_provider_baseline(&self.config.relay_provider),
            );
            return;
        };
        enum Switch {
            MapOff(Vec<String>),
            MapOn(Vec<String>),
            Rebind,
        }
        let switch = if self.endpoint.is_some() && self.endpoint_has_relay_transport {
            match (&previous, &self.config.relay_provider) {
                (RelayProviderConfig::CustomUrls(urls), RelayProviderConfig::Disabled) => {
                    Switch::MapOff(urls.clone())
                }
                (RelayProviderConfig::Disabled, RelayProviderConfig::CustomUrls(urls)) => {
                    Switch::MapOn(urls.clone())
                }
                _ => Switch::Rebind,
            }
        } else {
            Switch::Rebind
        };
        match switch {
            Switch::MapOff(urls) => self.disable_relays_in_place(&urls).await,
            Switch::MapOn(urls) => self.enable_relays_in_place(&urls).await,
            Switch::Rebind => {
                self.rebind_endpoint(
                    SecretKey::from_bytes(&secret),
                    b"relay configuration changed",
                )
                .await;
            }
        }
    }

    /// Relays off without touching the endpoint: remove every configured
    /// relay from the live map, pin the status at `Off`, and close exactly
    /// the connections whose *live* path set is relay-only. A connection
    /// holding any direct path survives untouched — paths migrate after
    /// establishment, so the live [`iroh::endpoint::Connection::paths`]
    /// reading decides, never the recorded establishment path.
    ///
    /// iroh 1.1.0's map edit alone gates nothing (verified by experiment in
    /// the tests named `remove_relay_*`): an established relay route keeps
    /// forwarding, and a new dial naming a relay URL still connects through
    /// it — the map feeds net-report probing and auth tokens, not datagram
    /// routing. Off therefore also means the runtime strips relay
    /// addresses from its own dials and refuses relay-only connections at
    /// establishment ([`Self::refuse_relay_only_connection`]) for as long
    /// as the provider is `Disabled`.
    ///
    /// The closed peers fall to the ordinary `PeerClosed` → retry path.
    /// Their re-dials run with `relays_configured` already false, so they
    /// rest in `Connecting`/`BackingOff` — never
    /// `Failed { relay_unreachable }`: relays are deliberately off, and an
    /// owner who switched them off has been denied nothing.
    async fn disable_relays_in_place(&mut self, urls: &[String]) {
        let Some(endpoint) = self.endpoint.clone() else {
            return;
        };
        for url in urls {
            let Ok(url) = url.parse::<iroh::RelayUrl>() else {
                continue;
            };
            endpoint.remove_relay(&url).await;
        }
        // The watch dies first: iroh never clears its home-relay reading on
        // a map edit, so left alive it would keep reporting the stale
        // connection. `Off` is the pinned truth from here.
        if let Some(task) = self.relay_status_task.take() {
            task.abort();
        }
        set_relay_status(&self.status, RelayConnectionStatus::Off);
        let relay_only: Vec<_> = self
            .connections
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .values()
            .filter(|live| path_kind(&live.connection) == PathKind::Relay)
            .map(|live| live.connection.clone())
            .collect();
        for connection in relay_only {
            connection.close(0u32.into(), b"relays disabled");
        }
    }

    /// Relays on, onto an endpoint that was bound with relay transport and
    /// had its map emptied by [`Self::disable_relays_in_place`]: insert the
    /// configured relays back — built through the same [`iroh::RelayMap`]
    /// parse as a fresh bind, so the entries cannot drift — touch no
    /// connection, resume the status watch from the configured baseline,
    /// and wake the retry machinery for the peers that were waiting on a
    /// relay route. A peer already connected (the direct peer the off
    /// switch left alone) is not redialed: its own replacement would
    /// supersede a perfectly healthy connection.
    async fn enable_relays_in_place(&mut self, urls: &[String]) {
        let Some(endpoint) = self.endpoint.clone() else {
            return;
        };
        if let Ok(map) = iroh::RelayMap::try_from_iter(urls.iter().map(String::as_str)) {
            for relay in map.relays::<Vec<_>>() {
                endpoint.insert_relay(relay.url.clone(), relay).await;
            }
        }
        if let Some(task) = self.relay_status_task.take() {
            task.abort();
        }
        let baseline = relay_provider_baseline(&self.config.relay_provider);
        set_relay_status(&self.status, baseline.clone());
        self.relay_status_task = Some(spawn_relay_status_watch(
            &endpoint,
            baseline,
            Arc::clone(&self.status),
        ));
        self.retry_waiting_desired_now();
    }

    /// With the provider `Disabled`, a connection whose live paths are
    /// relay-only must not be adopted, whichever side opened it: iroh's
    /// transport still carries relay traffic for URLs a peer advertises
    /// even with the relay map emptied (verified by experiment in
    /// `remove_relay_does_not_gate_new_dials_so_the_runtime_must`), so the
    /// refusal is the runtime's own. Closes and reports `true` when
    /// refused; a connection holding any direct path passes untouched.
    fn refuse_relay_only_connection(&self, connection: &iroh::endpoint::Connection) -> bool {
        if !matches!(self.config.relay_provider, RelayProviderConfig::Disabled) {
            return false;
        }
        if path_kind(connection) != PathKind::Relay {
            return false;
        }
        connection.close(0u32.into(), b"relays disabled");
        true
    }

    /// Tear down and bind a fresh endpoint under the current configuration.
    /// Shared by provisioning and the live relay switch; the caller supplies
    /// the key, so identity is whatever was provisioned.
    async fn rebind_endpoint(&mut self, key: SecretKey, close_reason: &'static [u8]) {
        // The old endpoint's relay watch dies with it; a fresh one is spawned
        // below once the replacement is bound.
        if let Some(task) = self.relay_status_task.take() {
            task.abort();
        }
        self.close_connections(close_reason);
        if let Some(endpoint) = self.endpoint.take() {
            endpoint.close().await;
        }
        self.endpoint_has_relay_transport = false;
        self.status
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .endpoint_id = None;
        // The new configuration's baseline — Off when relays are
        // disabled, "configured and not reached" otherwise — which is
        // also what a failed bind below honestly leaves behind.
        set_relay_status(
            &self.status,
            relay_provider_baseline(&self.config.relay_provider),
        );
        *self
            .endpoint_addr
            .write()
            .unwrap_or_else(|error| error.into_inner()) = None;
        let mut builder = match self.config.address_lookup {
            AddressLookupConfig::DefaultNetwork => Endpoint::builder(presets::N0),
            AddressLookupConfig::Disabled => Endpoint::builder(presets::Minimal),
        };
        match &self.config.relay_provider {
            RelayProviderConfig::DefaultNetwork => {}
            RelayProviderConfig::Disabled => {
                builder = builder.relay_mode(iroh::RelayMode::Disabled);
            }
            RelayProviderConfig::CustomUrls(urls) => {
                let Ok(relays) = iroh::RelayMap::try_from_iter(urls.iter().map(String::as_str))
                else {
                    return;
                };
                builder = builder.relay_mode(iroh::RelayMode::Custom(relays));
            }
        }
        builder = builder.secret_key(key).alpns(vec![
            REPLICATION_ALPN_V1.to_vec(),
            PAIRING_ALPN_V1.to_vec(),
            ENROLLMENT_ALPN_V1.to_vec(),
        ]);
        match builder.bind().await {
            Ok(endpoint) => {
                // Recorded from the provider actually bound with, at the one
                // place binding happens: only `Disabled` builds an endpoint
                // without relay transport.
                self.endpoint_has_relay_transport =
                    !matches!(self.config.relay_provider, RelayProviderConfig::Disabled);
                let endpoint_id = endpoint.id();
                self.status
                    .write()
                    .unwrap_or_else(|e| e.into_inner())
                    .endpoint_id = Some(ReplicationEndpointId::from_bytes(*endpoint_id.as_bytes()));
                *self
                    .endpoint_addr
                    .write()
                    .unwrap_or_else(|error| error.into_inner()) = Some(endpoint.addr());
                spawn_accept_loop(
                    endpoint.clone(),
                    Arc::clone(&self.authorization),
                    Arc::clone(&self.pairing),
                    Arc::clone(&self.enrollment),
                    self.commands.clone(),
                );
                // With relays disabled the baseline `Off` already written is
                // the whole truth and there is nothing to watch.
                if !matches!(self.config.relay_provider, RelayProviderConfig::Disabled) {
                    self.relay_status_task = Some(spawn_relay_status_watch(
                        &endpoint,
                        relay_provider_baseline(&self.config.relay_provider),
                        Arc::clone(&self.status),
                    ));
                }
                self.endpoint = Some(endpoint);
                self.retry_desired_now();
            }
            Err(error) => {
                tracing::warn!(%error, "replication endpoint bind failed");
                self.status
                    .write()
                    .unwrap_or_else(|e| e.into_inner())
                    .endpoint_id = None;
            }
        }
    }

    fn authorize(&mut self, snapshot: AuthorizationSnapshot) {
        // A fresh control session resends its complete grant. Refresh cosmetic
        // session data without fencing and reconnecting an unchanged mesh.
        let unchanged = self
            .authorization
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .is_some_and(|current| current.same_grant(&snapshot));
        if unchanged {
            *self
                .authorization
                .write()
                .unwrap_or_else(|error| error.into_inner()) = Some(snapshot);
            return;
        }
        // A snapshot replaces the entire grant. Fence every live stream before
        // installing it: even a peer that remains trusted may have lost one
        // workspace, and a stream authenticated under the old scope must not
        // be allowed to carry another frame.
        self.invalidate_blob_hydration_attempts();
        self.revoke_authorization_fences();
        self.close_connections(b"authorization replaced");
        // A changed build record may be the update that repaired a storming
        // peer. Its first connection under the new grant is news again.
        self.lifecycle_logs.clear();
        let granted: BTreeSet<DeviceId> =
            snapshot.peers.iter().map(|peer| peer.device_id).collect();
        *self
            .authorization
            .write()
            .unwrap_or_else(|e| e.into_inner()) = Some(snapshot);
        self.clear_repaired_incompatible_peers();
        // A device the new grant no longer names stops being a holder hint;
        // what it advertised is as revoked as what it may serve.
        self.blob_directory
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .forget_except(&granted);
        self.retry_desired_now();
        self.wake_blob_hydrations();
    }

    /// After a new grant: a refused peer whose build record now speaks our
    /// protocol has left the incompatible state, and that is news; one the
    /// grant no longer names has simply gone. `retry_desired` resets the
    /// backoff itself.
    fn clear_repaired_incompatible_peers(&mut self) {
        for device_id in self.incompatible_reports.devices() {
            match authorized_device(&self.authorization, device_id) {
                None => {
                    self.incompatible_reports.clear(device_id);
                }
                Some(peer) if peer_protocol_refusal(&self.authorization, &peer).is_none() => {
                    self.incompatible_reports.clear(device_id);
                    log_incompatible_cleared(&self.authorization, &peer);
                }
                Some(_) => {}
            }
        }
    }

    fn join_pairing(&self, join: koru_pairing::PairingJoinV1, exact_addr: Option<EndpointAddr>) {
        let Some(endpoint) = self.endpoint.clone() else {
            self.pairing.event(PairingEvent::Failed {
                code: "endpoint_unavailable".into(),
            });
            return;
        };
        let pairing = Arc::clone(&self.pairing);
        tokio::spawn(async move {
            let endpoint_id =
                match EndpointId::from_bytes(join.transcript.inviter.endpoint_id.as_bytes()) {
                    Ok(id) => id,
                    Err(_) => {
                        pairing.event(PairingEvent::Failed {
                            code: "endpoint_id".into(),
                        });
                        return;
                    }
                };
            let target = exact_addr.unwrap_or_else(|| EndpointAddr::new(endpoint_id));
            if target.id != endpoint_id {
                pairing.event(PairingEvent::Failed {
                    code: "endpoint_mismatch".into(),
                });
                return;
            }
            let connection = match endpoint.connect(target, PAIRING_ALPN_V1).await {
                Ok(connection) if connection.remote_id() == endpoint_id => connection,
                Ok(connection) => {
                    connection.close(1u32.into(), b"endpoint identity mismatch");
                    pairing.event(PairingEvent::Failed {
                        code: "endpoint_mismatch".into(),
                    });
                    return;
                }
                Err(_) => {
                    pairing.event(PairingEvent::Failed {
                        code: "connect_failed".into(),
                    });
                    return;
                }
            };
            let result = async {
                let (mut send, mut recv) = connection
                    .open_bi()
                    .await
                    .map_err(|error| FrameError::new("open", error))?;
                write_json(&mut send, &join).await?;
                send.finish()
                    .map_err(|error| FrameError::new("finish", error))?;
                read_json::<PairingWireResponse>(&mut recv).await
            }
            .await;
            match result {
                Ok(PairingWireResponse {
                    prepared: Some(prepared),
                    error_code: None,
                }) if prepared.verify().is_ok() && prepared.transcript == join.transcript => {
                    pairing.event(PairingEvent::Completed { prepared });
                }
                Ok(response) => pairing.event(PairingEvent::Failed {
                    code: response
                        .error_code
                        .unwrap_or_else(|| "invalid_pairing_response".into()),
                }),
                Err(_) => pairing.event(PairingEvent::Failed {
                    code: "pairing_stream".into(),
                }),
            }
        });
    }

    fn open_enrollment(
        &self,
        expected_endpoint: ReplicationEndpointId,
        addr: EndpointAddr,
        answer: oneshot::Sender<Result<EnrollmentSession, EnrollmentTransportError>>,
    ) {
        let Some(endpoint) = self.endpoint.clone() else {
            let _ = answer.send(Err(EnrollmentTransportError::EndpointUnavailable));
            return;
        };
        let expected = match EndpointId::from_bytes(expected_endpoint.as_bytes()) {
            Ok(expected) => expected,
            Err(_) => {
                let _ = answer.send(Err(EnrollmentTransportError::EndpointMismatch));
                return;
            }
        };
        if addr.id != expected {
            let _ = answer.send(Err(EnrollmentTransportError::EndpointMismatch));
            return;
        }
        tokio::spawn(async move {
            let opened = async {
                let connection = endpoint
                    .connect(addr, ENROLLMENT_ALPN_V1)
                    .await
                    .map_err(|_| EnrollmentTransportError::ConnectFailed)?;
                if connection.remote_id() != expected {
                    connection.close(1u32.into(), b"endpoint identity mismatch");
                    return Err(EnrollmentTransportError::EndpointMismatch);
                }
                let (send, recv) = connection
                    .open_bi()
                    .await
                    .map_err(|_| EnrollmentTransportError::StreamFailed)?;
                Ok(EnrollmentSession::new(connection, send, recv))
            };
            let result = tokio::time::timeout(ENROLLMENT_SESSION_TIMEOUT, opened)
                .await
                .unwrap_or(Err(EnrollmentTransportError::TimedOut));
            let _ = answer.send(result);
        });
    }

    fn request_connection(&mut self, device_id: DeviceId, exact_addr: Option<EndpointAddr>) {
        self.next_generation = self.next_generation.wrapping_add(1);
        self.desired.insert(
            device_id,
            ConnectionIntent {
                exact_addr,
                generation: self.next_generation,
                failures: 0,
                retry: 0,
                connected_at: None,
            },
        );
    }

    fn intent_generation(&self, device_id: DeviceId) -> Option<u64> {
        self.desired.get(&device_id).map(|intent| intent.generation)
    }

    /// Preserve an already-live path when a reconnect repeats ConnectPeer.
    fn adopt_live_connection(&mut self, device_id: DeviceId) {
        if self.desired.contains_key(&device_id) {
            return;
        }
        self.next_generation = self.next_generation.wrapping_add(1);
        self.desired.insert(
            device_id,
            ConnectionIntent {
                exact_addr: None,
                generation: self.next_generation,
                failures: 0,
                retry: 0,
                connected_at: Some(std::time::Instant::now()),
            },
        );
    }

    fn is_connected(&self, device_id: DeviceId) -> bool {
        self.connections
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .contains_key(&device_id)
    }

    /// The path of the live connection carrying this peer, if one is up.
    /// `close_reason` weeds out a connection that has died but whose
    /// `PeerClosed` has not been processed yet — the dedup in
    /// [`Self::establish_connection`] treats those as replaceable for the
    /// same reason.
    fn live_connection_path(&self, device_id: DeviceId) -> Option<PathKind> {
        self.connections
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .get(&device_id)
            .filter(|live| live.connection.close_reason().is_none())
            .map(|live| live.path)
    }

    fn retry_desired_now(&mut self) {
        self.retry_desired(false);
    }

    /// The retry wake for a switch that left live connections standing
    /// ([`Self::enable_relays_in_place`]): only peers without a live
    /// connection are redialed. `retry_desired_now` redials everyone,
    /// which is right after a rebind or a grant replacement — nothing
    /// survives those — but here a redial of a connected peer would
    /// supersede its own healthy connection.
    fn retry_waiting_desired_now(&mut self) {
        self.retry_desired(true);
    }

    fn retry_desired(&mut self, only_disconnected: bool) {
        let devices: Vec<_> = self.desired.keys().copied().collect();
        for device_id in devices {
            if only_disconnected && self.is_connected(device_id) {
                continue;
            }
            self.next_generation = self.next_generation.wrapping_add(1);
            let generation = self.next_generation;
            let Some(intent) = self.desired.get_mut(&device_id) else {
                continue;
            };
            intent.generation = generation;
            intent.failures = 0;
            intent.connected_at = None;
            intent.retry = intent.retry.wrapping_add(1);
            let retry = intent.retry;
            // A new snapshot or endpoint is a deliberate fresh start: any
            // accumulated backoff is about the *old* situation. That includes
            // the incompatible-protocol backoff — a peer's build record is
            // part of the grant, so the update that repairs a refused peer
            // arrives here and is dialed at once.
            self.status
                .write()
                .unwrap_or_else(|error| error.into_inner())
                .backoff_failures
                .insert(device_id, 0);
            let _ = self.commands.send(Command::RetryPeer {
                device_id,
                generation,
                retry,
            });
        }
    }

    /// Start an outbound connection without awaiting network I/O in the
    /// command actor. A dial may wait on discovery, relay reachability, or a
    /// dead address for many seconds; none of those may delay durable-change
    /// announcements, revocation, disconnect, or blob wakeups.
    fn start_connect(&mut self, device_id: DeviceId) {
        let Some(intent) = self.desired.get(&device_id).cloned() else {
            return;
        };
        if self.active_connection_attempts.get(&device_id) == Some(&intent.generation) {
            return;
        }
        // A live connection already carries this peer — typically inbound,
        // because the peer could dial us while our own dials cannot reach it
        // (the desktop dialing a phone that has no route back). Redialing
        // would at best duplicate that connection and at worst repaint its
        // `Connected` state with `Resolving`/`Connecting`/`BackingOff` —
        // which is exactly what showed a syncing peer as offline. Re-assert
        // what the connections map knows and stand down; `peer_closed`
        // restarts the dial loop when the connection actually dies.
        if let Some(path) = self.live_connection_path(device_id) {
            set_peer(
                &self.status,
                device_id,
                PeerConnectionState::Connected { path },
            );
            return;
        }
        let Some(endpoint) = self.endpoint.clone() else {
            self.schedule_retry(device_id, intent.generation, RetryReason::Transient);
            return;
        };
        let peer = self
            .authorization
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .and_then(|snapshot| {
                snapshot
                    .peers
                    .iter()
                    .find(|peer| peer.device_id == device_id)
            })
            .cloned();
        let Some(peer) = peer else {
            set_peer(&self.status, device_id, PeerConnectionState::Unauthorized);
            return;
        };
        set_peer(&self.status, device_id, PeerConnectionState::Resolving);
        let Ok(endpoint_id) = EndpointId::from_bytes(peer.endpoint_id.as_bytes()) else {
            set_peer(
                &self.status,
                device_id,
                PeerConnectionState::Failed {
                    code: "endpoint_id",
                },
            );
            return;
        };
        let target = intent
            .exact_addr
            .unwrap_or_else(|| EndpointAddr::new(endpoint_id));
        if target.id != endpoint_id {
            set_peer(&self.status, device_id, PeerConnectionState::Unauthorized);
            return;
        }
        let relays_configured =
            !matches!(self.config.relay_provider, RelayProviderConfig::Disabled);
        // With relays off, no dial of ours may name a relay route: iroh
        // routes datagrams to whatever relay URL the target carries, relay
        // map or no relay map (verified by experiment in
        // `remove_relay_does_not_gate_new_dials_so_the_runtime_must`). The
        // stored intent keeps its full address — the relay route returns
        // the moment relays are switched back on.
        let target = if relays_configured {
            target
        } else {
            EndpointAddr::from_parts(
                target.id,
                target.addrs.iter().filter(|addr| !addr.is_relay()).cloned(),
            )
        };
        set_peer(&self.status, device_id, PeerConnectionState::Connecting);
        self.active_connection_attempts
            .insert(device_id, intent.generation);
        let commands = self.commands.clone();
        log_dial_started(&endpoint, device_id, &target);
        tokio::spawn(async move {
            let outcome = match endpoint.connect(target, REPLICATION_ALPN_V1).await {
                Ok(connection) if connection.remote_id() == endpoint_id => {
                    ConnectionAttemptOutcome::Connected(connection)
                }
                Ok(connection) => ConnectionAttemptOutcome::IdentityMismatch(connection),
                Err(error) => {
                    let code = dial_failure_code(&endpoint, relays_configured);
                    log_dial_failed(&endpoint, device_id, &error, code);
                    ConnectionAttemptOutcome::Failed { code }
                }
            };
            let _ = commands.send(Command::ConnectionAttemptFinished {
                device_id,
                generation: intent.generation,
                peer,
                outcome,
            });
        });
    }

    /// The Cloud dial. It is a network wait, so it runs outside the command
    /// actor like every peer dial; the actor only resolves the endpoint and
    /// shapes the target. There is no retry here: the Cloud coordinator owns
    /// backoff, and a second dial is a second decision of its own.
    fn connect_cloud(
        &self,
        route: CloudRoute,
        answer: oneshot::Sender<Result<iroh::endpoint::Connection, &'static str>>,
    ) {
        let Some(endpoint) = self.endpoint.clone() else {
            let _ = answer.send(Err("endpoint_unavailable"));
            return;
        };
        let Ok(endpoint_id) = EndpointId::from_bytes(route.endpoint_id.as_bytes()) else {
            let _ = answer.send(Err("endpoint_id"));
            return;
        };
        let relays_configured =
            !matches!(self.config.relay_provider, RelayProviderConfig::Disabled);
        let mut target = EndpointAddr::new(endpoint_id);
        for address in route.direct_addresses {
            target = target.with_ip_addr(address);
        }
        // Relays off means no dial of ours names a relay route (the peer
        // dial's rule, verified there by experiment); the issued route keeps
        // its relay for the next dial after relays are switched back on.
        if relays_configured
            && let Some(url) = route.relay_url.as_deref()
            && let Ok(url) = url.parse::<iroh::RelayUrl>()
        {
            target = target.with_relay_url(url);
        }
        tokio::spawn(async move {
            let result = match tokio::time::timeout(
                Duration::from_secs(20),
                endpoint.connect(target, koru_replication::cloud::CLOUD_ALPN_V1),
            )
            .await
            {
                Ok(Ok(connection)) if connection.remote_id() == endpoint_id => Ok(connection),
                Ok(Ok(connection)) => {
                    connection.close(1u32.into(), b"endpoint mismatch");
                    Err("endpoint_mismatch")
                }
                Ok(Err(_)) => Err("cloud_dial_failed"),
                Err(_) => Err("cloud_dial_timeout"),
            };
            let _ = answer.send(result);
        });
    }

    /// **A Cloud stream never takes the device path.** It is admitted under
    /// the Cloud principal and the Cloud content token only; the gateway's
    /// first branch sends it to its Cloud acceptor or resets it, and nothing
    /// here can make it look like a paired device.
    fn admit_cloud_session(
        &self,
        principal: koru_device::cloud::CloudPrincipalV1,
        connection_id: u64,
        path: PathKind,
        send: iroh::endpoint::SendStream,
        recv: iroh::endpoint::RecvStream,
        answer: oneshot::Sender<Result<(), koru_network_edge::EdgeError>>,
    ) {
        let Some(ingress) = self.opaque_ingress.clone() else {
            let _ = answer.send(Err(koru_network_edge::EdgeError::Closed));
            return;
        };
        let session = opaque_session(
            koru_network_edge::NetworkPrincipal::Cloud { cloud: principal },
            cloud_content_protocol(),
            koru_network_edge::PathMetadata {
                kind: path,
                // A Cloud content session, which carries no peer blob frame.
                // D246's gate is about the bilateral path, so there is nothing
                // truthful to claim here and `false` claims nothing.
                selected_is_ip: false,
                outbound: true,
            },
            connection_id,
            send,
            recv,
        );
        let _ = answer.send(ingress.admit(session));
    }

    fn open_product_session(
        &self,
        target: koru_network_edge::NetworkTarget,
        requested_protocol: koru_network_edge::ProtocolToken,
        answer: oneshot::Sender<Result<TransportSession, koru_network_edge::EdgeError>>,
    ) {
        let device_id = match target {
            koru_network_edge::NetworkTarget::Device { device_id } => device_id,
            // The Agent's Cloud coordinator supplies this dial in a later
            // increment: it resolves the exact live permit, rechecks
            // generations and current authority, obtains a credential and an
            // issued route, and confirms the authenticated remote endpoint
            // equals the route's endpoint before the handshake. None of that
            // exists here, and this runtime must not invent it — a wrong
            // dataset or a stale permit has to be denied *before* dialing.
            koru_network_edge::NetworkTarget::Cloud { .. } => {
                let _ = answer.send(Err(koru_network_edge::EdgeError::Closed));
                return;
            }
        };
        // A device target never carries the Cloud token, and a permit never
        // authorizes the peer token.
        if requested_protocol != replication_protocol() {
            let _ = answer.send(Err(koru_network_edge::EdgeError::InvalidProtocol));
            return;
        }
        let connection = self
            .connections
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .get(&device_id)
            .map(|live| live.connection.clone());
        let peer = self
            .authorization
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .and_then(|snapshot| {
                snapshot
                    .peers
                    .iter()
                    .find(|peer| peer.device_id == device_id)
            })
            .cloned();
        let Some((connection, peer)) = connection.zip(peer) else {
            let _ = answer.send(Err(koru_network_edge::EdgeError::Closed));
            return;
        };
        let principal = device_principal(
            &peer,
            self.authorization
                .read()
                .unwrap_or_else(|error| error.into_inner())
                .as_ref()
                .map_or(0, |snapshot| snapshot.sequence),
        );
        let path = path_kind(&connection);
        tokio::spawn(async move {
            let result = match connection.open_bi().await {
                Ok((send, recv)) => Ok(opaque_session(
                    principal,
                    requested_protocol,
                    koru_network_edge::PathMetadata {
                        kind: path,
                        // **Observed at open, not at establishment.** One
                        // session is opened per frame, so this is the freshest
                        // answer the bridged shape can carry and it lands once
                        // per blob chunk.
                        selected_is_ip: blob_path::selected_path_is_ip(&connection),
                        outbound: true,
                    },
                    connection.stable_id() as u64,
                    send,
                    recv,
                )),
                Err(_) => Err(koru_network_edge::EdgeError::Closed),
            };
            let _ = answer.send(result);
        });
    }

    fn connection_attempt_finished(
        &mut self,
        device_id: DeviceId,
        generation: u64,
        peer: AuthorizedPeer,
        outcome: ConnectionAttemptOutcome,
    ) {
        if self.active_connection_attempts.get(&device_id) == Some(&generation) {
            self.active_connection_attempts.remove(&device_id);
        }
        if self.intent_generation(device_id) != Some(generation) {
            if let ConnectionAttemptOutcome::Connected(connection)
            | ConnectionAttemptOutcome::IdentityMismatch(connection) = outcome
            {
                connection.close(0u32.into(), b"stale connection attempt");
            }
            return;
        }
        match outcome {
            ConnectionAttemptOutcome::Connected(connection) => {
                if self.refuse_relay_only_connection(&connection) {
                    // A dial that could only complete over a relay while
                    // relays are off — possible through iroh's own cached
                    // or discovered addressing, which the dial-time strip
                    // cannot reach. Refused like any failed attempt: the
                    // ordinary backoff, no failure code.
                    self.schedule_retry(device_id, generation, RetryReason::Transient);
                    return;
                }
                self.establish_connection(peer, connection, ConnectionDirection::Outbound);
            }
            ConnectionAttemptOutcome::IdentityMismatch(connection) => {
                connection.close(1u32.into(), b"endpoint identity mismatch");
                set_peer(&self.status, device_id, PeerConnectionState::Unauthorized);
            }
            ConnectionAttemptOutcome::Failed { code } => {
                self.schedule_retry(device_id, generation, RetryReason::Transient);
                // Diagnostic-only: when the failure carries an evidence-backed
                // code, the peer rests on it instead of bare `BackingOff`
                // until the scheduled retry dials again. The retry cadence and
                // the accumulated `backoff_failures` are unchanged. Not when a
                // live connection carries the peer, though — our failed dial
                // is then a footnote, not the peer's state.
                if let Some(code) = code {
                    if self.live_connection_path(device_id).is_none() {
                        set_peer(
                            &self.status,
                            device_id,
                            PeerConnectionState::Failed { code },
                        );
                    }
                }
            }
        }
    }

    fn inbound_connection(&mut self, peer: AuthorizedPeer, connection: iroh::endpoint::Connection) {
        let still_authorized = authorized_endpoint(&self.authorization, connection.remote_id())
            .is_some_and(|current| current.device_id == peer.device_id);
        if !still_authorized {
            connection.close(1u32.into(), b"authorization changed during accept");
            return;
        }
        // A peer that still has relays on can reach this endpoint through
        // its old relay registration; with relays off here, that inbound
        // relay-only session is refused. The remote retries on its own
        // cadence and lands the moment a direct route exists.
        if self.refuse_relay_only_connection(&connection) {
            return;
        }
        self.establish_connection(peer, connection, ConnectionDirection::Inbound);
    }

    fn establish_connection(
        &mut self,
        peer: AuthorizedPeer,
        connection: iroh::endpoint::Connection,
        direction: ConnectionDirection,
    ) {
        let device_id = peer.device_id;
        let Some(local_device) = self
            .authorization
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .map(|snapshot| snapshot.local_device)
        else {
            connection.close(1u32.into(), b"authorization unavailable");
            return;
        };
        let candidate = LiveConnection {
            path: path_kind(&connection),
            connection,
            direction,
            connected_at: std::time::Instant::now(),
        };
        let decision = {
            let mut connections = self
                .connections
                .write()
                .unwrap_or_else(|error| error.into_inner());
            match connections.get(&device_id) {
                None => Ok(connections.insert(device_id, candidate.clone())),
                Some(current)
                    if current.connection.close_reason().is_some()
                        || choose_connection(
                            local_device,
                            device_id,
                            current.direction,
                            candidate.direction,
                        ) == ConnectionChoice::UseCandidate =>
                {
                    Ok(connections.insert(device_id, candidate.clone()))
                }
                Some(current) => Err(current.clone()),
            }
        };
        let replaced = match decision {
            Ok(replaced) => replaced,
            Err(current) => {
                tracing::info!(
                    peer_name = peer.display_name.as_deref().unwrap_or("?"),
                    device = %device_id.to_storage(),
                    connection_id = candidate.id(),
                    kept_connection_id = current.id(),
                    direction = candidate.direction.as_str(),
                    kept_direction = current.direction.as_str(),
                    "peer connection discarded: deterministic path already active"
                );
                candidate
                    .connection
                    .close(0u32.into(), b"duplicate peer connection");
                return;
            }
        };
        if let Some(replaced) = replaced {
            tracing::info!(
                peer_name = peer.display_name.as_deref().unwrap_or("?"),
                device = %device_id.to_storage(),
                connection_id = replaced.id(),
                replacement_connection_id = candidate.id(),
                direction = replaced.direction.as_str(),
                replacement_direction = candidate.direction.as_str(),
                "peer connection superseded by deterministic path"
            );
            replaced
                .connection
                .close(0u32.into(), b"superseded peer connection");
        }

        set_peer(
            &self.status,
            device_id,
            PeerConnectionState::Connected {
                path: candidate.path,
            },
        );
        let log_decision = self
            .lifecycle_logs
            .entry(device_id)
            .or_default()
            .connected(Instant::now());
        if let LifecycleLogDecision::Rapid { connections } = log_decision {
            log_rapid_peer_reconnects(
                &self.authorization,
                &peer,
                candidate.id(),
                candidate.path,
                candidate.direction.as_str(),
                connections,
            );
        }
        let report_incompatible = if peer_protocol_refusal(&self.authorization, &peer).is_some() {
            self.incompatible_reports
                .connected(device_id, peer.build.as_ref(), Instant::now())
                == IncompatibleReport::Entered
        } else {
            // A peer we were refusing came up on our protocol — typically
            // its own dial, the moment it updated: the incompatible backoff
            // is over.
            if self.incompatible_reports.clear(device_id) {
                if let Some(intent) = self.desired.get_mut(&device_id) {
                    intent.failures = 0;
                }
                self.status
                    .write()
                    .unwrap_or_else(|error| error.into_inner())
                    .backoff_failures
                    .insert(device_id, 0);
                log_incompatible_cleared(&self.authorization, &peer);
            }
            false
        };
        log_peer_connected(
            &self.authorization,
            &peer,
            candidate.id(),
            candidate.path,
            candidate.direction.as_str(),
            report_incompatible,
        );
        if let Some(ingress) = self.opaque_ingress.clone() {
            spawn_opaque_connection(
                candidate.connection.clone(),
                device_principal(
                    &peer,
                    self.authorization
                        .read()
                        .unwrap_or_else(|error| error.into_inner())
                        .as_ref()
                        .map_or(0, |snapshot| snapshot.sequence),
                ),
                ingress,
                candidate.path,
                candidate.direction.outbound(),
            );
        } else {
            spawn_peer_session(
                candidate.connection.clone(),
                authenticated_peer(&peer, &self.authorization_fences, &self.authorization),
                self.store.clone(),
                Arc::clone(&self.pairing),
                Arc::clone(&self.status),
                Arc::clone(&self.sync_coordinator),
                Arc::clone(&self.blob_directory),
            );
        }
        if let Some(intent) = self.desired.get_mut(&device_id) {
            // Stamp, don't reset: whether this connection was useful is
            // decided when it closes, by how long it lived.
            intent.connected_at = Some(candidate.connected_at);
        }
        self.wake_blob_hydrations();
        let commands = self.commands.clone();
        tokio::spawn(async move {
            let reason = candidate.connection.closed().await.to_string();
            let _ = commands.send(Command::PeerClosed {
                device_id,
                connection_id: candidate.id(),
                direction: candidate.direction,
                path: candidate.path,
                connected_at: candidate.connected_at,
                reason,
            });
        });
    }

    /// Schedule the next dial to a desired peer, replacing any retry already
    /// pending for it, and return how long it waits — `None` when no dial was
    /// scheduled (a stale generation, or a live connection already carries
    /// the peer). `reason` picks the schedule ([`reconnect::retry_delay`]).
    fn schedule_retry(
        &mut self,
        device_id: DeviceId,
        generation: u64,
        reason: RetryReason,
    ) -> Option<Duration> {
        if self.intent_generation(device_id) != Some(generation) {
            return None;
        }
        // A failed dial to a peer a live connection already carries — the
        // peer won the race by dialing us. There is nothing to back off
        // from, and writing `BackingOff` here painted that syncing peer
        // offline. Re-assert the live state; the connection's own close is
        // what schedules the next dial.
        if let Some(path) = self.live_connection_path(device_id) {
            set_peer(
                &self.status,
                device_id,
                PeerConnectionState::Connected { path },
            );
            return None;
        }
        let Some(intent) = self.desired.get_mut(&device_id) else {
            return None;
        };
        let delay = retry_delay(reason, intent.failures, device_id);
        intent.failures = intent.failures.saturating_add(1);
        intent.retry = intent.retry.wrapping_add(1);
        let retry = intent.retry;
        let failures = intent.failures;
        set_peer(&self.status, device_id, PeerConnectionState::BackingOff);
        self.status
            .write()
            .unwrap_or_else(|error| error.into_inner())
            .backoff_failures
            .insert(device_id, failures);
        let commands = self.commands.clone();
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            let _ = commands.send(Command::RetryPeer {
                device_id,
                generation,
                retry,
            });
        });
        Some(delay)
    }

    fn peer_closed(
        &mut self,
        device_id: DeviceId,
        connection_id: usize,
        direction: ConnectionDirection,
        path: PathKind,
        connected_at: std::time::Instant,
        reason: &str,
    ) {
        let removed = {
            let mut connections = self
                .connections
                .write()
                .unwrap_or_else(|error| error.into_inner());
            if connections
                .get(&device_id)
                .is_some_and(|connection| connection.id() == connection_id)
            {
                connections.remove(&device_id);
                true
            } else {
                false
            }
        };
        let lifetime = connected_at.elapsed();
        log_peer_disconnected(
            &self.authorization,
            device_id,
            connection_id,
            direction,
            path,
            lifetime,
            reason,
            removed,
        );
        // Crossed/replacement connections are normal. A close callback from
        // the old path must never evict the newer live path stored for this
        // device.
        if !removed {
            return;
        }
        let mut sync = self
            .sync_coordinator
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if sync
            .get(&device_id)
            .is_some_and(|state| state.connection_id == connection_id)
        {
            sync.remove(&device_id);
        }
        drop(sync);
        if let Some(generation) = self.intent_generation(device_id) {
            // Died on first use, or dropped after proving itself? The first
            // is a failure and keeps the backoff climbing; the second is a
            // blip and starts fresh. Resetting on *connect* instead — the old
            // rule — pinned a refused peer at the fastest retry forever.
            let early = self
                .desired
                .get_mut(&device_id)
                .and_then(|intent| intent.connected_at.take())
                .is_some_and(|connected_at| connected_at.elapsed() < EARLY_DEATH);
            if !early {
                if let Some(intent) = self.desired.get_mut(&device_id) {
                    intent.failures = 0;
                }
                self.status
                    .write()
                    .unwrap_or_else(|error| error.into_inner())
                    .backoff_failures
                    .insert(device_id, 0);
            }
            // An early death of a peer whose protocol we refuse is the older
            // build closing on us, and will be until somebody updates: that
            // waits minutes, not milliseconds. Anything else keeps the
            // transient schedule compatible peers always had.
            let peer = authorized_device(&self.authorization, device_id);
            let refused = peer
                .as_ref()
                .is_some_and(|peer| peer_protocol_refusal(&self.authorization, peer).is_some());
            let reason = if early && refused {
                RetryReason::Incompatible
            } else {
                RetryReason::Transient
            };
            let delay = self.schedule_retry(device_id, generation, reason);
            if let (RetryReason::Incompatible, Some(delay), Some(peer)) = (reason, delay, peer) {
                let report = self.incompatible_reports.backing_off(
                    device_id,
                    peer.build.as_ref(),
                    Instant::now(),
                    delay,
                );
                log_incompatible_backoff(&self.authorization, &peer, report, delay);
            }
        } else {
            set_peer(&self.status, device_id, PeerConnectionState::Disconnected);
        }
        self.wake_blob_hydrations();
    }

    /// §8's peer-first read, on the runtime that owns the connections.
    ///
    /// It reuses the *authorization* half of the hydration plan and none of its
    /// scheduling: no park, no rotation, no directory record. The engine that
    /// asked already knows what it wants and what it does next if this answers
    /// `None`.
    fn read_blob_range(
        &mut self,
        workspace: koru_store_core::WorkspaceId,
        hash: BlobHash,
        offset: u64,
        max: u64,
        answer: oneshot::Sender<Option<PeerBlobRange>>,
    ) {
        let workspace_profile = self
            .store
            .as_ref()
            .and_then(|store| store.workspace_profile(workspace).ok());
        let authorized = self
            .authorization
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .map(|snapshot| {
                snapshot
                    .peers
                    .iter()
                    .filter(|peer| {
                        peer.all_data
                            || peer.workspaces.iter().any(|id| *id == workspace)
                            || workspace_profile
                                .is_some_and(|profile| peer.authorized_profiles.contains(&profile))
                    })
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let mut attempts: Vec<(protocol::AuthenticatedPeer, iroh::endpoint::Connection)> =
            Vec::new();
        {
            let connections = self
                .connections
                .read()
                .unwrap_or_else(|error| error.into_inner());
            for peer in &authorized {
                let peer =
                    authenticated_peer(peer, &self.authorization_fences, &self.authorization);
                if peer.protocol_refusal().is_some() {
                    continue;
                }
                if let Some(connection) = connections
                    .get(&peer.device_id)
                    .map(|live| live.connection.clone())
                {
                    attempts.push((peer, connection));
                }
            }
        }
        tokio::spawn(async move {
            let _ = answer.send(read_blob_range_from(attempts, workspace, hash, offset, max).await);
        });
    }

    fn schedule_blob_hydration(&mut self, key: BlobHydrationKey) {
        let workspace_profile = self
            .store
            .as_ref()
            .and_then(|store| store.workspace_profile(key.workspace).ok());
        let authorized = self
            .authorization
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .map(|snapshot| {
                snapshot
                    .peers
                    .iter()
                    .filter(|peer| {
                        peer.all_data
                            || peer.workspaces.iter().any(|id| *id == key.workspace)
                            || workspace_profile
                                .is_some_and(|profile| peer.authorized_profiles.contains(&profile))
                    })
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if authorized.is_empty() {
            hydration::set_blob_hydration_waiting(
                &self.blob_hydrations,
                &key,
                BlobWaitReason::NoLinkedDevices,
            );
            return;
        }
        // Authenticate every authorized peer, not only the connected ones: a
        // peer on another protocol number is not a source when online, and is
        // not a reason to wait when offline either.
        let candidates: Vec<protocol::AuthenticatedPeer> = authorized
            .iter()
            .map(|peer| authenticated_peer(peer, &self.authorization_fences, &self.authorization))
            .filter(|peer| peer.protocol_refusal().is_none())
            .collect();
        let mut by_id: HashMap<
            DeviceId,
            (protocol::AuthenticatedPeer, iroh::endpoint::Connection),
        > = HashMap::new();
        {
            let connections = self
                .connections
                .read()
                .unwrap_or_else(|error| error.into_inner());
            for peer in &candidates {
                if let Some(connection) = connections
                    .get(&peer.device_id)
                    .map(|live| live.connection.clone())
                {
                    by_id.insert(peer.device_id, (peer.clone(), connection));
                }
            }
        }
        let connected: BTreeSet<DeviceId> = by_id.keys().copied().collect();

        let directory_holders = self
            .blob_directory
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .holders(key.workspace, &key.hash)
            .cloned();
        // The durable origin can name a holder the directory has not seen this
        // session — after a restart, before the first inventory arrives.
        let origin = self
            .store
            .as_ref()
            .and_then(|store| {
                candidates
                    .iter()
                    .find_map(|peer| store.blob_origin(peer, key.workspace, &key.hash))
            })
            .and_then(|id| DeviceId::parse(&id));
        // Holder-first (directory and durable origin), then everyone else
        // connected, each list rotated so a run of misses spreads over the
        // mesh instead of landing on whoever sorts first.
        let (order, park) = match hydration::plan_blob_hydration(
            &candidates,
            &connected,
            directory_holders.as_ref(),
            origin,
        ) {
            hydration::HydrationPlan::Wait(reason) => {
                hydration::set_blob_hydration_waiting(&self.blob_hydrations, &key, reason);
                return;
            }
            hydration::HydrationPlan::Attempt { order, park } => (order, park),
        };
        tracing::info!(
            workspace = %key.workspace.to_storage(),
            attempts = order.len(),
            holders_known = directory_holders.as_ref().map_or(0, BTreeSet::len),
            origin_known = origin.is_some(),
            "blob_hydration action=plan"
        );
        let attempts = order
            .into_iter()
            .filter_map(|device| by_id.remove(&device))
            .collect::<Vec<_>>();
        let Some(store) = self.store.as_ref().cloned() else {
            hydration::complete_blob_hydration(
                &self.blob_hydrations,
                &key,
                Err(BlobHydrationError::new("store_unavailable", true)),
            );
            return;
        };

        let (generation, state) = {
            let mut hydrations = self
                .blob_hydrations
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let Some(entry) = hydrations.get_mut(&key) else {
                return;
            };
            if entry.running {
                return;
            }
            entry.generation = entry.generation.saturating_add(1);
            entry.running = true;
            (entry.generation, entry.state.clone())
        };
        let commands = self.commands.clone();
        let slots = Arc::clone(&self.blob_hydration_slots);
        let status = Arc::clone(&self.status);
        let directory = Arc::clone(&self.blob_directory);
        tokio::spawn(async move {
            let permit = slots.acquire_owned().await;
            let outcome = match permit {
                Ok(_permit) => {
                    hydration::hydrate_blob_from_peers(
                        &key, attempts, park, store, &state, status, directory,
                    )
                    .await
                }
                Err(_) => {
                    BlobHydrationOutcome::Failed(BlobHydrationError::new("runtime_closed", true))
                }
            };
            let _ = commands.send(Command::BlobHydrationFinished {
                key,
                generation,
                outcome,
            });
        });
    }

    fn blob_hydration_finished(
        &mut self,
        key: BlobHydrationKey,
        generation: u64,
        outcome: BlobHydrationOutcome,
    ) {
        let current = self
            .blob_hydrations
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(&key)
            .is_some_and(|entry| entry.generation == generation);
        if !current {
            return;
        }
        match outcome {
            BlobHydrationOutcome::Available => {
                hydration::complete_blob_hydration(&self.blob_hydrations, &key, Ok(()));
            }
            BlobHydrationOutcome::WaitingForPeer(reason) => {
                let mut hydrations = self
                    .blob_hydrations
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                if let Some(entry) = hydrations.get_mut(&key) {
                    entry.running = false;
                    entry
                        .state
                        .send_replace(BlobHydrationState::WaitingForPeer { reason });
                }
            }
            BlobHydrationOutcome::Failed(error) => {
                hydration::complete_blob_hydration(&self.blob_hydrations, &key, Err(error));
            }
        }
    }

    fn wake_blob_hydrations(&mut self) {
        let keys = {
            let mut hydrations = self
                .blob_hydrations
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            hydrations
                .iter_mut()
                .filter_map(|(key, entry)| {
                    if entry.running {
                        None
                    } else {
                        Some(key.clone())
                    }
                })
                .collect::<Vec<_>>()
        };
        for key in keys {
            self.schedule_blob_hydration(key);
        }
    }

    fn invalidate_blob_hydration_attempts(&mut self) {
        let mut hydrations = self
            .blob_hydrations
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        for entry in hydrations.values_mut() {
            entry.generation = entry.generation.saturating_add(1);
            entry.running = false;
        }
    }

    fn disconnect(&mut self, device_id: DeviceId) {
        self.desired.remove(&device_id);
        self.active_connection_attempts.remove(&device_id);
        self.sync_coordinator
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&device_id);
        if let Some(connection) = self
            .connections
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&device_id)
        {
            connection.connection.close(0u32.into(), b"disconnected");
        }
        self.status
            .write()
            .unwrap_or_else(|error| error.into_inner())
            .backoff_failures
            .remove(&device_id);
        set_peer(&self.status, device_id, PeerConnectionState::Disconnected);
        self.wake_blob_hydrations();
    }

    fn send_trust_record(&self, device_id: DeviceId, record: koru_device::TrustRecord) {
        // Teardown is revocation semantics, not trust-record semantics. A
        // revocation stops this peer's writes before the network round trip
        // and ends the connection; the control contract only lets the
        // singular path carry one. Anything else must not interrupt the sync
        // it travels beside.
        let revoking = matches!(record, koru_device::TrustRecord::Revocation(_));
        let connection = self
            .connections
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .get(&device_id)
            .map(|live| live.connection.clone());
        if revoking {
            if let Some(fence) = self
                .authorization_fences
                .read()
                .unwrap_or_else(|error| error.into_inner())
                .get(&device_id)
                .cloned()
            {
                // Local safety changes before any network wait. Existing Store
                // operations finish under the fence; later ones are refused.
                fence.revoke();
            }
        }
        let commands = self.commands.clone();
        if let Some(connection) = connection {
            tokio::spawn(async move {
                let _ = request_frame(
                    &connection,
                    &protocol::PeerFrame::TrustRecord {
                        record: Box::new(record),
                    },
                )
                .await;
                if revoking {
                    let _ = commands.send(Command::DisconnectPeer(device_id));
                }
            });
        } else if revoking {
            let _ = commands.send(Command::DisconnectPeer(device_id));
        }
    }

    fn send_trust_records(
        &self,
        device_id: DeviceId,
        records: Vec<koru_device::TrustRecord>,
        ack: std::sync::mpsc::Sender<bool>,
    ) {
        // Gossip carries no fence and no disconnect, and the caller is told
        // whether the batch was queued onto a live connection: that answer
        // is what lets an unlink's revocation survive racing the snapshot
        // replacement that closes every connection in the same instant.
        let connection = self
            .connections
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .get(&device_id)
            .map(|live| live.connection.clone());
        let queued = connection.is_some();
        let _ = ack.send(queued);
        if let Some(connection) = connection {
            tokio::spawn(async move {
                let _ = request_frame(&connection, &protocol::PeerFrame::TrustRecords { records })
                    .await;
            });
        }
    }

    fn release(&mut self, grace: Duration) {
        self.release_epoch = self.release_epoch.wrapping_add(1);
        let epoch = self.release_epoch;
        let commands = self.commands.clone();
        tokio::spawn(async move {
            tokio::time::sleep(grace).await;
            let _ = commands.send(Command::ClearIfReleased(epoch));
        });
    }

    fn retain(&mut self) {
        self.release_epoch = self.release_epoch.wrapping_add(1);
    }

    async fn clear(&mut self) {
        self.release_epoch = self.release_epoch.wrapping_add(1);
        self.announcement_generation = self.announcement_generation.saturating_add(1);
        if let Some(task) = self.announcement_task.take() {
            task.abort();
        }
        if let Some(task) = self.relay_status_task.take() {
            task.abort();
        }
        self.desired.clear();
        self.active_connection_attempts.clear();
        self.lifecycle_logs.clear();
        self.incompatible_reports = IncompatibleReports::default();
        self.invalidate_blob_hydration_attempts();
        hydration::fail_all_blob_hydrations(
            &self.blob_hydrations,
            BlobHydrationError::new("runtime_closed", true),
        );
        *self
            .blob_directory
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = hydration::BlobDirectory::new();
        self.sync_coordinator
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clear();
        self.pairing.cancel("control_session_ended");
        self.revoke_authorization_fences();
        self.close_connections(b"control session ended");
        *self
            .authorization
            .write()
            .unwrap_or_else(|e| e.into_inner()) = None;
        self.secret = None;
        if let Some(endpoint) = self.endpoint.take() {
            endpoint.close().await;
        }
        self.endpoint_has_relay_transport = false;
        {
            let mut status = self.status.write().unwrap_or_else(|e| e.into_inner());
            let state_events = std::mem::take(&mut status.state_events);
            let last_state_change_millis = std::mem::take(&mut status.last_state_change_millis);
            let relay = std::mem::take(&mut status.relay);
            *status = NetworkStatus {
                state_events,
                last_state_change_millis,
                relay,
                ..NetworkStatus::default()
            };
        }
        // The configuration outlives the session: with the endpoint gone
        // the honest reading is the baseline, not `Off` — an owner who
        // left relays on has not switched anything off. Written through
        // the transition logger: a connected relay dying with its session
        // is lifecycle news.
        set_relay_status(
            &self.status,
            relay_provider_baseline(&self.config.relay_provider),
        );
        *self
            .endpoint_addr
            .write()
            .unwrap_or_else(|error| error.into_inner()) = None;
    }

    fn close_connections(&mut self, reason: &'static [u8]) {
        self.sync_coordinator
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clear();
        let connections =
            std::mem::take(&mut *self.connections.write().unwrap_or_else(|e| e.into_inner()));
        for (_, connection) in connections {
            connection.connection.close(0u32.into(), reason);
        }
    }

    fn revoke_authorization_fences(&mut self) {
        let fences = std::mem::take(
            &mut *self
                .authorization_fences
                .write()
                .unwrap_or_else(|error| error.into_inner()),
        );
        // Silent until now, and its silence cost a diagnosis. A revocation is
        // the other half of every fence interaction: it waits for operations
        // already inside the barrier, and it is what a wedged Store worker
        // would have been waiting on. Without this line the next occurrence
        // shows a device stuck with nothing anywhere saying a revocation was
        // even in flight.
        if !fences.is_empty() {
            tracing::info!(
                target: "koru_replication",
                event = "authorization.fences_revoked",
                count = fences.len(),
                "authorization replaced: revoking device fences",
            );
        }
        for (_, fence) in fences {
            fence.revoke();
        }
    }
}

/// A version vector in a form short enough to log and still compare: its
/// byte length plus the first eight hex of its blake3. Two sides naming the
/// same state produce the same tag; a divergence shows as different tags.
fn vv_tag(version: &[u8]) -> String {
    let hash = blake3::hash(version).to_hex().to_string();
    format!("{}:{}", version.len(), &hash[..8])
}

/// Follow [`Endpoint::home_relay_status`] into [`NetworkStatus::relay`].
///
/// One task per bound endpoint, held in `RuntimeState::relay_status_task`:
/// [`RuntimeState::rebind_endpoint`] replaces it and
/// [`RuntimeState::clear`] aborts it, so it dies with the endpoint it
/// watches. Event-driven — the iroh watcher wakes it on change and it
/// sleeps otherwise; never a timer (`guide/contributing/PRINCIPLES.md`), and never
/// [`Endpoint::online`], which hangs forever on a dead relay.
///
/// An empty reading is a home relay never selected — under a configured
/// relay list that is the never-reached relay, and the caller's `fallback`
/// (the configured baseline, [`relay_provider_baseline`]) is what it
/// honestly reads as.
fn spawn_relay_status_watch(
    endpoint: &Endpoint,
    fallback: RelayConnectionStatus,
    status: Arc<RwLock<NetworkStatus>>,
) -> tokio::task::JoinHandle<()> {
    use iroh::Watcher as _;
    let mut watcher = endpoint.home_relay_status();
    tokio::spawn(async move {
        loop {
            let reading = watcher
                .get()
                .first()
                .map(|home| {
                    relay_state_reading(
                        home.url()
                            .host_str()
                            .map(str::to_owned)
                            .unwrap_or_else(|| home.url().to_string()),
                        home.is_connected(),
                        home.last_error().is_some(),
                    )
                })
                .unwrap_or_else(|| fallback.clone());
            set_relay_status(&status, reading);
            if watcher.updated().await.is_err() {
                // The watched endpoint is gone. The runtime actor writes the
                // next configuration's baseline before this can matter.
                return;
            }
        }
    })
}

fn spawn_accept_loop(
    endpoint: Endpoint,
    authorization: Arc<RwLock<Option<AuthorizationSnapshot>>>,
    pairing: Arc<PairingHub>,
    enrollment: Arc<EnrollmentHub>,
    commands: mpsc::UnboundedSender<Command>,
) {
    tokio::spawn(async move {
        while let Some(incoming) = endpoint.accept().await {
            let authorization = Arc::clone(&authorization);
            let pairing = Arc::clone(&pairing);
            let enrollment = Arc::clone(&enrollment);
            let commands = commands.clone();
            tokio::spawn(async move {
                let Ok(accepting) = incoming.accept() else {
                    return;
                };
                let Ok(connection) = accepting.await else {
                    return;
                };
                if connection.alpn() == PAIRING_ALPN_V1 {
                    serve_pairing_connection(connection, pairing).await;
                    return;
                }
                if connection.alpn() == ENROLLMENT_ALPN_V1 {
                    serve_enrollment_connection(connection, enrollment).await;
                    return;
                }
                if connection.alpn() != REPLICATION_ALPN_V1 {
                    connection.close(1u32.into(), b"unsupported protocol");
                    return;
                }
                let remote = connection.remote_id();
                let peer = authorized_endpoint(&authorization, remote);
                if let Some(peer) = peer {
                    if let Err(error) =
                        commands.send(Command::InboundConnection { peer, connection })
                    {
                        if let Command::InboundConnection { connection, .. } = error.0 {
                            connection.close(0u32.into(), b"network runtime stopped");
                        }
                    }
                } else {
                    connection.close(1u32.into(), b"unauthorized endpoint");
                }
            });
        }
    });
}

async fn serve_enrollment_connection(
    connection: iroh::endpoint::Connection,
    enrollment: Arc<EnrollmentHub>,
) {
    serve_enrollment_connection_with_timeout(connection, enrollment, ENROLLMENT_SESSION_TIMEOUT)
        .await;
}

async fn serve_enrollment_connection_with_timeout(
    connection: iroh::endpoint::Connection,
    enrollment: Arc<EnrollmentHub>,
    session_timeout: Duration,
) {
    let deadline = tokio::time::Instant::now()
        .checked_add(session_timeout)
        .unwrap_or_else(tokio::time::Instant::now);
    let permit = match Arc::clone(&enrollment.sessions).try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => {
            connection.close(1u32.into(), b"enrollment capacity reached");
            return;
        }
    };
    let streams = tokio::time::timeout_at(deadline, connection.accept_bi()).await;
    let Ok(Ok((send, recv))) = streams else {
        connection.close(1u32.into(), b"enrollment stream unavailable");
        return;
    };
    let mut session = EnrollmentSession::with_deadline(connection, send, recv, deadline);
    let handler = enrollment
        .handler
        .read()
        .unwrap_or_else(|error| error.into_inner())
        .clone();
    let Some(handler) = handler else {
        let _ = session
            .send_frame(&koru_pairing::EnrollmentFrameV1::Error {
                code: "enrollment_unavailable".into(),
            })
            .await;
        let _ = session.finish().await;
        return;
    };
    let outcome = tokio::time::timeout_at(deadline, handler.handle(&mut session)).await;
    match outcome {
        Ok(Ok(())) => {
            let _ = session.finish().await;
        }
        Ok(Err(error)) => {
            let _ = session
                .send_frame(&koru_pairing::EnrollmentFrameV1::Error {
                    code: error.code().into(),
                })
                .await;
            let _ = session.finish().await;
        }
        Err(_) => session.close(1, b"enrollment timed out"),
    }
    drop(permit);
}

async fn serve_pairing_connection(
    connection: iroh::endpoint::Connection,
    pairing: Arc<PairingHub>,
) {
    let remote = connection.remote_id();
    let result = async {
        let (mut send, mut recv) = connection
            .accept_bi()
            .await
            .map_err(|error| FrameError::new("accept", error))?;
        let join = read_json::<koru_pairing::PairingJoinV1>(&mut recv).await?;
        let invitation = pairing
            .invitation
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
            .ok_or_else(|| FrameError::message("authorize", "no active invitation"))?;
        if join.verify(&invitation).is_err()
            || join.transcript.joiner.endpoint_id.as_bytes() != remote.as_bytes()
        {
            let response = PairingWireResponse {
                prepared: None,
                error_code: Some("invalid_join".into()),
            };
            let _ = write_json(&mut send, &response).await;
            let _ = send.finish();
            return Err(FrameError::message("authorize", "invalid pairing join"));
        }
        let already_claimed = {
            let mut claimed = pairing
                .claimed
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if *claimed {
                true
            } else {
                *claimed = true;
                false
            }
        };
        if already_claimed {
            let response = PairingWireResponse {
                prepared: None,
                error_code: Some("already_claimed".into()),
            };
            let _ = write_json(&mut send, &response).await;
            let _ = send.finish();
            return Err(FrameError::message(
                "authorize",
                "invitation already claimed",
            ));
        }
        let attempt_id = PairingAttemptId::new();
        let (answer, waiting) = oneshot::channel();
        pairing
            .pending
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(attempt_id, answer);
        pairing.event(PairingEvent::JoinRequested {
            attempt_id,
            join: Box::new(join),
        });
        let response = match tokio::time::timeout(Duration::from_secs(120), waiting).await {
            Ok(Ok(Ok(prepared))) => PairingWireResponse {
                prepared: Some(Box::new(prepared)),
                error_code: None,
            },
            Ok(Ok(Err(code))) => PairingWireResponse {
                prepared: None,
                error_code: Some(code),
            },
            _ => PairingWireResponse {
                prepared: None,
                error_code: Some("pairing_timeout".into()),
            },
        };
        write_json(&mut send, &response).await?;
        send.finish()
            .map_err(|error| FrameError::new("finish", error))?;
        // `finish` queues FIN; it does not mean the peer has consumed the
        // response. Keep the sole connection handle alive until the joiner
        // drops its receive half, otherwise QUIC may close before delivery.
        let _ = send.stopped().await;
        Ok(())
    }
    .await;
    let _ = result;
}

fn authenticated_peer(
    peer: &AuthorizedPeer,
    fences: &RwLock<BTreeMap<DeviceId, protocol::AuthorizationFence>>,
    authorization: &RwLock<Option<AuthorizationSnapshot>>,
) -> protocol::AuthenticatedPeer {
    let authorization_revision = authorization
        .read()
        .unwrap_or_else(|error| error.into_inner())
        .as_ref()
        .map_or(0, |snapshot| snapshot.sequence);
    let principal = device_principal(peer, authorization_revision);
    let principal = principal
        .device()
        .expect("device_principal always mints the device arm");
    let authorization_fence = fences
        .write()
        .unwrap_or_else(|error| error.into_inner())
        .entry(peer.device_id)
        .or_insert_with(protocol::AuthorizationFence::active)
        .clone();
    protocol::AuthenticatedPeer {
        relationship_id: principal.relationship_id,
        device_id: principal.device_id,
        endpoint_id: principal.endpoint_id,
        all_data: peer.all_data,
        authorized_profiles: peer.authorized_profiles.iter().copied().collect(),
        authorized_workspaces: peer.workspaces.iter().copied().collect::<BTreeSet<_>>(),
        authorization_fence,
        display_name: peer.display_name.clone(),
        incompatible: build_drift(authorization, peer).is_incompatible(),
        peer_protocol: peer.build.as_ref().and_then(|build| build.peer_protocol),
    }
}

fn spawn_opaque_connection(
    connection: iroh::endpoint::Connection,
    principal: koru_network_edge::NetworkPrincipal,
    ingress: Arc<dyn NetworkIngress>,
    path: PathKind,
    outbound: bool,
) {
    tokio::spawn(async move {
        let connection_id = connection.stable_id() as u64;
        if ingress
            .connected(koru_network_edge::ConnectionMetadata {
                connection_id,
                principal: principal.clone(),
                path: koru_network_edge::PathMetadata {
                    kind: path,
                    selected_is_ip: blob_path::selected_path_is_ip(&connection),
                    outbound,
                },
            })
            .is_err()
        {
            connection.close(2u32.into(), b"gateway bridge unavailable");
            return;
        }
        while let Ok((send, recv)) = connection.accept_bi().await {
            let session = opaque_session(
                principal.clone(),
                replication_protocol(),
                koru_network_edge::PathMetadata {
                    kind: path,
                    // Re-read per accepted stream rather than once per
                    // connection: `path` is the establishment record and this
                    // is what is carrying data right now.
                    selected_is_ip: blob_path::selected_path_is_ip(&connection),
                    outbound,
                },
                connection.stable_id() as u64,
                send,
                recv,
            );
            if ingress.admit(session).is_err() {
                connection.close(2u32.into(), b"gateway bridge saturated");
                return;
            }
        }
        ingress.disconnected(connection_id);
    });
}

/// Adapt one QUIC stream to the transport-neutral bounded byte contract. This
/// function never inspects a byte; framing remains with the gateway/Store side.
fn opaque_session(
    principal: koru_network_edge::NetworkPrincipal,
    protocol: koru_network_edge::ProtocolToken,
    path: koru_network_edge::PathMetadata,
    connection_id: u64,
    mut quic_send: iroh::endpoint::SendStream,
    mut quic_recv: iroh::endpoint::RecvStream,
) -> TransportSession {
    let (mut local, pump) = TransportSession::pair(principal.clone(), principal, protocol, 8);
    local.connection_id = connection_id;
    local.path = path;
    let control = pump.control();
    let to_local = pump.send.clone();
    let mut from_local = pump.receive;

    let receive_control = control.clone();
    tokio::spawn(async move {
        loop {
            let received = tokio::select! {
                biased;
                _ = to_local.closed() => {
                    let _ = quic_recv.stop(1u32.into());
                    return;
                },
                received = quic_recv.read_chunk(64 * 1024) => received,
            };
            match received {
                Ok(Some(bytes)) => {
                    if to_local.send(bytes.to_vec()).await.is_err() {
                        let _ = quic_recv.stop(1u32.into());
                        return;
                    }
                }
                Ok(None) => return,
                Err(_) => {
                    receive_control.reset(1);
                    return;
                }
            }
        }
    });
    tokio::spawn(async move {
        loop {
            match from_local.recv().await {
                Ok(Some(bytes)) => {
                    if quic_send.write_all(&bytes).await.is_err() {
                        control.reset(1);
                        return;
                    }
                }
                Ok(None) => {
                    let _ = quic_send.finish();
                    return;
                }
                Err(koru_network_edge::EdgeError::Reset(code)) => {
                    let code = u32::try_from(code).unwrap_or(u32::MAX);
                    let _ = quic_send.reset(code.into());
                    return;
                }
                Err(koru_network_edge::EdgeError::Cancelled) => {
                    let _ = quic_send.reset(1u32.into());
                    return;
                }
                Err(koru_network_edge::EdgeError::Closed)
                | Err(koru_network_edge::EdgeError::InvalidProtocol) => return,
            }
        }
    });
    local
}

/// How the peer's advertised build relates to ours, from the two
/// descriptions the authorization snapshot carries. `Unknown` when either
/// side has not said. The independent Profile-unit protocol gate refuses
/// unknown observations before data exchange; build drift remains unknown.
fn build_drift(
    authorization: &RwLock<Option<AuthorizationSnapshot>>,
    peer: &AuthorizedPeer,
) -> koru_device::BuildDrift {
    let local_build = authorization
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(|snapshot| snapshot.local_build.clone());
    match (&local_build, &peer.build) {
        (Some(ours), Some(theirs)) => ours.drift_from(theirs),
        _ => koru_device::BuildDrift::Unknown,
    }
}

/// Whether this runtime refuses `peer`'s data plane, and why — the rule
/// [`protocol::AuthenticatedPeer::protocol_refusal`] applies to a live
/// connection, read straight from the authorization snapshot so a caller
/// with no connection (the dialer choosing a backoff) gets the same answer.
fn peer_protocol_refusal(
    authorization: &RwLock<Option<AuthorizationSnapshot>>,
    peer: &AuthorizedPeer,
) -> Option<&'static str> {
    protocol::protocol_refusal(
        peer.build.as_ref().and_then(|build| build.peer_protocol),
        build_drift(authorization, peer).is_incompatible(),
    )
}

/// The authorized peer with this device id in the current snapshot.
fn authorized_device(
    authorization: &RwLock<Option<AuthorizationSnapshot>>,
    device_id: DeviceId,
) -> Option<AuthorizedPeer> {
    authorization
        .read()
        .unwrap_or_else(|error| error.into_inner())
        .as_ref()?
        .peers
        .iter()
        .find(|peer| peer.device_id == device_id)
        .cloned()
}

/// A refused peer's connection died on first use and the next dial waits
/// `delay`. Said when the state is entered and then once per backoff window,
/// with when the next attempt is; between those, only under
/// `KORU_SYNC_DEBUG`.
fn log_incompatible_backoff(
    authorization: &RwLock<Option<AuthorizationSnapshot>>,
    peer: &AuthorizedPeer,
    report: IncompatibleReport,
    delay: Duration,
) {
    let local_build = authorization
        .read()
        .unwrap_or_else(|error| error.into_inner())
        .as_ref()
        .and_then(|snapshot| snapshot.local_build.clone());
    let peer_name = peer.display_name.as_deref().unwrap_or("?");
    let peer_label = peer.build.as_ref().map(BuildPayload::label);
    let local_label = local_build.as_ref().map(BuildPayload::label);
    let peer_build = peer_label.as_deref().unwrap_or("unknown");
    let local_build_label = local_label.as_deref().unwrap_or("unknown");
    let peer_protocol = peer.build.as_ref().and_then(|build| build.peer_protocol);
    let local_protocol = local_build.as_ref().and_then(|build| build.peer_protocol);
    let refusal = peer_protocol_refusal(authorization, peer).unwrap_or("-");
    let next_attempt_in = format!("{}s", delay.as_secs());
    match report {
        IncompatibleReport::Entered | IncompatibleReport::StillRefused => tracing::error!(
            peer_name,
            peer_build,
            local_build = local_build_label,
            peer_protocol,
            local_protocol,
            refusal,
            next_attempt_in,
            "incompatible_protocol: sync refused until one side updates; the peer closed the connection, backing off"
        ),
        IncompatibleReport::Quiet => {
            if koru_env::switch(koru_env::name::KORU_SYNC_DEBUG) {
                tracing::debug!(
                    target: "koru_sync::peer",
                    peer_name,
                    device = %peer.device_id.to_storage(),
                    refusal,
                    next_attempt_in,
                    "incompatible peer closed the connection; backing off"
                );
            }
        }
    }
}

/// A peer this runtime was refusing now speaks its protocol.
fn log_incompatible_cleared(
    authorization: &RwLock<Option<AuthorizationSnapshot>>,
    peer: &AuthorizedPeer,
) {
    let local_build = authorization
        .read()
        .unwrap_or_else(|error| error.into_inner())
        .as_ref()
        .and_then(|snapshot| snapshot.local_build.clone());
    let peer_label = peer.build.as_ref().map(BuildPayload::label);
    let local_label = local_build.as_ref().map(BuildPayload::label);
    tracing::info!(
        peer_name = peer.display_name.as_deref().unwrap_or("?"),
        peer_build = peer_label.as_deref().unwrap_or("unknown"),
        local_build = local_label.as_deref().unwrap_or("unknown"),
        "incompatible_protocol cleared: the peer speaks this build's protocol; reconnecting without backoff"
    );
}

fn spawn_peer_session(
    connection: iroh::endpoint::Connection,
    peer: protocol::AuthenticatedPeer,
    store: Option<Arc<dyn protocol::ReplicationStore>>,
    pairing: Arc<PairingHub>,
    status: Arc<RwLock<NetworkStatus>>,
    sync_coordinator: Arc<Mutex<BTreeMap<DeviceId, PeerSyncState>>>,
    blob_directory: SharedBlobDirectory,
) {
    let serving = connection.clone();
    let serving_peer = peer.clone();
    let serving_store = store.clone();
    let serving_status = Arc::clone(&status);
    let serving_coordinator = Arc::clone(&sync_coordinator);
    let serving_directory = Arc::clone(&blob_directory);
    tokio::spawn(async move {
        serve_streams(
            serving,
            serving_peer,
            serving_store,
            pairing,
            serving_status,
            serving_coordinator,
            serving_directory,
        )
        .await;
    });
    if let Some(code) = peer.protocol_refusal() {
        // Said once per connection, where a status surface reads it; the
        // connection itself stays for trust gossip.
        record_sync(
            &status,
            peer.device_id,
            "sync_refused",
            None,
            None,
            Some(code),
        );
        return;
    }
    if let Some(store) = store {
        request_sync(
            connection,
            peer,
            store,
            status,
            sync_coordinator,
            blob_directory,
        );
    }
}

/// Each bidirectional QUIC stream is one request and one response. The stream
/// boundary prevents a malformed or abandoned request from desynchronising
/// every later workspace on the connection.
async fn serve_streams(
    connection: iroh::endpoint::Connection,
    peer: protocol::AuthenticatedPeer,
    store: Option<Arc<dyn protocol::ReplicationStore>>,
    pairing: Arc<PairingHub>,
    status: Arc<RwLock<NetworkStatus>>,
    sync_coordinator: Arc<Mutex<BTreeMap<DeviceId, PeerSyncState>>>,
    blob_directory: SharedBlobDirectory,
) {
    while let Ok((mut send, mut recv)) = connection.accept_bi().await {
        let store = store.clone();
        let peer = peer.clone();
        let pairing = Arc::clone(&pairing);
        let connection = connection.clone();
        let status = Arc::clone(&status);
        let sync_coordinator = Arc::clone(&sync_coordinator);
        let blob_directory = Arc::clone(&blob_directory);
        tokio::spawn(async move {
            let frame = match read_frame(&mut recv).await {
                Ok(frame) => frame,
                Err(error) => {
                    if let Some(code) = peer.protocol_refusal() {
                        let _ = write_frame(
                            &mut send,
                            &protocol::PeerFrame::Error { code: code.into() },
                        )
                        .await;
                        let _ = send.finish();
                        return;
                    }
                    tracing::warn!(
                        peer = %peer.device_id.to_storage(),
                        peer_name = %peer.label(),
                        connection_id = connection.stable_id(),
                        error = %error,
                        "sync inbound frame failed"
                    );
                    return;
                }
            };
            let is_trust_frame = matches!(
                &frame,
                protocol::PeerFrame::TrustRecord { .. } | protocol::PeerFrame::TrustRecords { .. }
            );
            if let Some(code) = peer.protocol_refusal().filter(|_| !is_trust_frame) {
                // Every shipped build reads an `Error` frame as a named sync
                // failure, so this is the one answer an older peer can
                // understand; what it means is in its device list.
                let refusal = protocol::PeerFrame::Error { code: code.into() };
                let _ = write_frame(&mut send, &refusal).await;
                let _ = send.finish();
                return;
            }
            if matches!(&frame, protocol::PeerFrame::SyncRequest) {
                if let Err(error) = write_frame(&mut send, &protocol::PeerFrame::SyncAck).await {
                    tracing::warn!(
                        peer = %peer.device_id.to_storage(),
                        peer_name = %peer.label(),
                        connection_id = connection.stable_id(),
                        error = %error,
                        "sync response failed response=sync_ack"
                    );
                    return;
                }
                let _ = send.finish();
                if let Some(store) = store {
                    request_sync(
                        connection,
                        peer,
                        store,
                        status,
                        sync_coordinator,
                        blob_directory,
                    );
                }
                return;
            }
            if is_trust_frame {
                // A singular frame is a revocation (the control contract
                // allows nothing else): stop admitting this peer before the
                // Agent performs its durable signature check. A gossip batch
                // carries third-party records that say nothing about this
                // connection, so it never touches the fence — an applied
                // change reaches the data plane through the next
                // authorization snapshot, which fences and reconnects
                // everything wholesale.
                if matches!(
                    &frame,
                    protocol::PeerFrame::TrustRecord { record }
                        if matches!(record.as_ref(), koru_device::TrustRecord::Revocation(_))
                ) {
                    peer.authorization_fence.revoke();
                }
                let delivery_id = TrustDeliveryId::new();
                let (answer, waiting) = oneshot::channel();
                pairing
                    .pending_trust
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .insert(delivery_id, answer);
                // Ack is withheld until Agent reports that the trust
                // repository commit succeeded.
                match frame {
                    protocol::PeerFrame::TrustRecord { record } => {
                        pairing.event(PairingEvent::TrustRecordReceived {
                            delivery_id,
                            from: peer.device_id,
                            record,
                        });
                    }
                    protocol::PeerFrame::TrustRecords { records } => {
                        pairing.event(PairingEvent::TrustRecordsReceived {
                            delivery_id,
                            from: peer.device_id,
                            records,
                        });
                    }
                    _ => unreachable!("the frame was matched as a trust frame above"),
                }
                let accepted = tokio::time::timeout(Duration::from_secs(30), waiting)
                    .await
                    .ok()
                    .and_then(Result::ok)
                    .unwrap_or(false);
                let response = if accepted {
                    protocol::PeerFrame::TrustAck
                } else {
                    protocol::PeerFrame::Error {
                        code: "trust_record_rejected".into(),
                    }
                };
                let _ = write_frame(&mut send, &response).await;
                let _ = send.finish();
                return;
            }
            let Some(store) = store else {
                return;
            };
            // **§6's third point, fenced by D246.** A peer asking for blob
            // bytes over a path this device is not actually sending
            // application data on is refused with a stable code it already
            // understands - no frame is added, so no build is refused.
            if matches!(frame, protocol::PeerFrame::BlobRequest { .. })
                && !blob_path::selected_path_is_ip(&connection)
            {
                let refusal = protocol::PeerFrame::Error {
                    code: blob_path::RELAY_PATH_REFUSED.into(),
                };
                let _ = write_frame(&mut send, &refusal).await;
                let _ = send.finish();
                return;
            }
            let state_targets = protocol::doc_update_targets(store.as_ref(), &peer, &frame)
                .into_iter()
                .map(|(workspace, document, updates)| {
                    let before = store
                        .version(&peer, workspace, document)
                        .unwrap_or_default();
                    (workspace, document, updates, before)
                })
                .collect::<Vec<_>>();
            let responses = protocol::handle_frame(store.as_ref(), &peer, frame);
            for (workspace, document, updates, before) in state_targets {
                let after = store
                    .version(&peer, workspace, document)
                    .unwrap_or_default();
                if after == before {
                    continue;
                }
                let is_manifest = store.manifest_id(&peer, workspace).ok() == Some(document);
                record_state_change(
                    &status,
                    Some(store.as_ref()),
                    peer.device_id,
                    if is_manifest {
                        "workspace_updated"
                    } else {
                        "document_updated"
                    },
                    None,
                    Some(workspace),
                    Some(document),
                    store.document_name(&peer, workspace, document),
                    None,
                    None,
                    imported_op_count(&before, &after).or(Some(updates)),
                );
            }
            for response in responses {
                if let Err(error) = write_frame(&mut send, &response).await {
                    tracing::warn!(
                        peer = %peer.device_id.to_storage(),
                        peer_name = %peer.label(),
                        connection_id = connection.stable_id(),
                        error = %error,
                        "sync response failed"
                    );
                    return;
                }
            }
            let _ = send.finish();
        });
    }
}

#[derive(Default)]
struct SyncPassResult {
    retry_all: bool,
    failed_workspaces: BTreeSet<koru_store_core::WorkspaceId>,
    retry_error_code: Option<String>,
    /// The workspace the first error belongs to, so the pass-level
    /// `sync_incomplete` event can name its culprit. The per-failure events
    /// carry it too, but a 1 Hz consumer only sees the last event of a pass.
    retry_error_workspace: Option<koru_store_core::WorkspaceId>,
}

impl SyncPassResult {
    fn remember_error(
        &mut self,
        code: impl Into<String>,
        workspace: Option<koru_store_core::WorkspaceId>,
    ) {
        if self.retry_error_code.is_none() {
            self.retry_error_code = Some(code.into());
            self.retry_error_workspace = workspace;
        }
    }
}

/// Schedule one authoritative pull for a peer. Hints received while a pass is
/// running set `dirty`; they never create overlapping catalog scans. A new
/// connection generation replaces the old state immediately, so a dead
/// connection sleeping in backoff cannot hold up its replacement.
fn request_sync<C>(
    connection: C,
    peer: protocol::AuthenticatedPeer,
    store: Arc<dyn protocol::ReplicationStore>,
    status: Arc<RwLock<NetworkStatus>>,
    coordinator: Arc<Mutex<BTreeMap<DeviceId, PeerSyncState>>>,
    blob_directory: SharedBlobDirectory,
) where
    C: RequestTransport,
{
    let connection_id = connection.stable_id();
    let mut retry_wake = {
        let mut states = coordinator
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let state = states.entry(peer.device_id).or_default();
        if state.connection_id != connection_id {
            *state = PeerSyncState {
                connection_id,
                ..PeerSyncState::default()
            };
        }
        if state.running {
            state.dirty = true;
            state.wake_generation = state.wake_generation.saturating_add(1);
            state.wake.send_replace(state.wake_generation);
            tracing::debug!(
                peer = %peer.device_id.to_storage(),
                peer_name = %peer.label(),
                connection_id,
                "sync hint coalesced into running peer pass"
            );
            return;
        }
        state.running = true;
        state.wake.subscribe()
    };

    tokio::spawn(async move {
        let mut scope = None;
        let mut retry_delay = Duration::from_secs(1);
        loop {
            if !sync_connection_is_current(&coordinator, peer.device_id, connection_id) {
                return;
            }
            let pass_id = NEXT_SYNC_PASS.fetch_add(1, Ordering::Relaxed);
            let result = pull_missing(
                connection.clone(),
                peer.clone(),
                Arc::clone(&store),
                Arc::clone(&status),
                Arc::clone(&blob_directory),
                scope.take(),
                pass_id,
            )
            .await;

            let dirty = {
                let mut states = coordinator
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                let Some(state) = states.get_mut(&peer.device_id) else {
                    return;
                };
                if state.connection_id != connection_id {
                    return;
                }
                std::mem::take(&mut state.dirty)
            };
            if dirty {
                // The watch notification and dirty bit describe the same
                // hint. Mark the current generation seen before immediately
                // running its full catalog comparison, otherwise its retained
                // watch value would cause a redundant third pass.
                let _ = retry_wake.borrow_and_update();
                retry_delay = Duration::from_secs(1);
                continue;
            }

            if result.retry_all || !result.failed_workspaces.is_empty() {
                scope = (!result.retry_all).then_some(result.failed_workspaces);
                tracing::warn!(
                    peer = %peer.device_id.to_storage(),
                    peer_name = %peer.label(),
                    connection_id,
                    pass_id,
                    retry_ms = retry_delay.as_millis(),
                    scope = if scope.is_some() { "failed_workspaces" } else { "catalog" },
                    "sync pass incomplete; retry scheduled"
                );
                let hinted = wait_for_retry_or_hint(retry_delay, &mut retry_wake).await;
                if hinted {
                    let mut states = coordinator
                        .lock()
                        .unwrap_or_else(|error| error.into_inner());
                    let Some(state) = states.get_mut(&peer.device_id) else {
                        return;
                    };
                    if state.connection_id != connection_id {
                        return;
                    }
                    state.dirty = false;
                    // A fresh durable hint may concern any workspace. Restart
                    // from the catalog instead of making it wait behind the
                    // old failed-workspace scope's 30-second retry.
                    scope = None;
                    retry_delay = Duration::from_secs(1);
                    continue;
                }
                retry_delay = retry_delay.saturating_mul(2).min(Duration::from_secs(30));
                continue;
            }

            let mut states = coordinator
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if let Some(state) = states.get_mut(&peer.device_id)
                && state.connection_id == connection_id
            {
                state.running = false;
            }
            return;
        }
    });
}

async fn wait_for_retry_or_hint(
    retry_delay: Duration,
    retry_wake: &mut watch::Receiver<u64>,
) -> bool {
    tokio::select! {
        _ = tokio::time::sleep(retry_delay) => false,
        changed = retry_wake.changed() => changed.is_ok(),
    }
}

fn sync_connection_is_current(
    coordinator: &Mutex<BTreeMap<DeviceId, PeerSyncState>>,
    peer: DeviceId,
    connection_id: usize,
) -> bool {
    coordinator
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .get(&peer)
        .is_some_and(|state| state.connection_id == connection_id)
}

#[derive(Default)]
struct CatalogChanges {
    /// Root catalog and personal v2 units committed this pass (protocol 6).
    root: usize,
    profiles: usize,
    workspaces: usize,
    documents: usize,
}

impl CatalogChanges {
    fn total(&self) -> usize {
        self.root + self.profiles + self.workspaces + self.documents
    }

    fn summary(&self) -> String {
        let mut parts = Vec::new();
        for (count, singular) in [
            (self.root, "placement change"),
            (self.profiles, "profile change"),
            (self.workspaces, "workspace change"),
            (self.documents, "document change"),
        ] {
            if count > 0 {
                parts.push(format!(
                    "{count} {singular}{}",
                    if count == 1 { "" } else { "s" }
                ));
            }
        }
        parts.join(", ")
    }
}

/// Number of Loro operation ids accepted between two durable positions.
/// A network update can contain many operations, and fragmentation can make
/// one update span several frames, so frame counts are not meaningful UI copy.
fn imported_op_count(before: &[u8], after: &[u8]) -> Option<usize> {
    let before = loro::VersionVector::decode(before).ok()?;
    let after = loro::VersionVector::decode(after).ok()?;
    usize::try_from(
        after
            .sub_iter(&before)
            .map(|span| i64::from(span.counter.end - span.counter.start))
            .sum::<i64>(),
    )
    .ok()
}

async fn pull_profile_units<C: RequestTransport>(
    connection: &C,
    peer: &protocol::AuthenticatedPeer,
    store: &dyn protocol::ReplicationStore,
    status: &Arc<RwLock<NetworkStatus>>,
    mut units: Vec<protocol::ProfileReplicaState>,
    result: &mut SyncPassResult,
    changes: &mut CatalogChanges,
) {
    units.sort_by_key(|remote| protocol::profile_unit_sort_key(remote.unit));
    for remote in units {
        let unit = remote.unit;
        let checked = peer
            .check_authorized_profile_unit(unit)
            .and_then(|()| remote.validate());
        if let Err(error) = checked {
            result.retry_all = true;
            result.remember_error(error.code, unit.workspace());
            continue;
        }
        if let Some(workspace) = unit.workspace() {
            // Catalog materialization is needed by the personal-unit FK. Its
            // listing owner does not rewrite the personal unit's author.
            if let Err(error) = store.workspace_profile(workspace) {
                result.retry_all = true;
                result.remember_error(error.code, Some(workspace));
                continue;
            }
        }
        if matches!(unit, protocol::ProfileUnitRef::Identity { .. }) {
            if let Err(error) = store.provision_profile(peer, &remote) {
                result.retry_all = true;
                result.remember_error(error.code, None);
                continue;
            }
        }
        let (was_missing, local) = match store.profile_version(peer, unit) {
            Ok(version) => (false, version),
            Err(error) if matches!(error.code, "profile_not_found" | "profile_unit_not_found") => {
                (true, loro::VersionVector::new().encode())
            }
            Err(error) => {
                result.retry_all = true;
                result.remember_error(error.code, unit.workspace());
                continue;
            }
        };
        let (Ok(local_version), Ok(remote_version)) = (
            loro::VersionVector::decode(&local),
            loro::VersionVector::decode(&remote.version),
        ) else {
            result.retry_all = true;
            result.remember_error("invalid_profile_version", unit.workspace());
            continue;
        };
        if !was_missing && local_version.includes_vv(&remote_version) {
            continue;
        }
        let response = tokio::time::timeout(
            SYNC_REQUEST_TIMEOUT,
            request_frame(
                connection,
                &protocol::PeerFrame::ProfileRequest {
                    unit,
                    version: local.clone(),
                },
            ),
        )
        .await;
        let error_code = match response {
            Ok(Ok(protocol::PeerFrame::ProfileUpdates {
                unit: received,
                updates,
            })) if received == unit => {
                let update_count = updates.len();
                let applied = peer
                    .check_authorized_profile_unit(unit)
                    .and_then(|()| store.apply_remote_profile_updates(peer, unit, updates))
                    .and_then(|_| store.profile_version(peer, unit));
                match applied {
                    Ok(after)
                        if loro::VersionVector::decode(&after)
                            .is_ok_and(|after| after.includes_vv(&remote_version)) =>
                    {
                        if was_missing || after != local {
                            changes.profiles += 1;
                            record_state_change(
                                status,
                                Some(store),
                                peer.device_id,
                                if was_missing
                                    && matches!(unit, protocol::ProfileUnitRef::Identity { .. })
                                {
                                    "profile_created"
                                } else {
                                    "profile_updated"
                                },
                                Some(unit.profile()),
                                unit.workspace(),
                                None,
                                None,
                                None,
                                None,
                                Some(update_count),
                            );
                        }
                        continue;
                    }
                    Ok(_) => "profile_unit_stalled".to_owned(),
                    Err(error) => error.code.to_owned(),
                }
            }
            Ok(Ok(protocol::PeerFrame::Error { code })) => code,
            Ok(Err(error)) => error.stage.to_owned(),
            Err(_) => "timeout".to_owned(),
            Ok(Ok(_)) => "unexpected_profile_frame".to_owned(),
        };
        if error_code != PEER_LOST_THE_PROFILE {
            result.retry_all = true;
        }
        result.remember_error(error_code, unit.workspace());
    }
}

/// **The root exchange** (root-catalog TECHNICAL.final §7.2): the eligible
/// root inventory is compared by digest, and every unit the peer holds ahead
/// of this device's receipt is pulled from the receipt's frontier, staged,
/// validated and locally committed with its projections. Answers the
/// personal v2 units the peer advertised, to be pulled after content
/// bootstrap. Only ever called for a scope-bound peer; a refusal or an
/// invalid row ends the root part of the pass and schedules a retry.
async fn pull_root_catalog<C: RequestTransport>(
    connection: &C,
    peer: &protocol::AuthenticatedPeer,
    store: &dyn protocol::ReplicationStore,
    status: &Arc<RwLock<NetworkStatus>>,
    result: &mut SyncPassResult,
    changes: &mut CatalogChanges,
    pass_id: u64,
) -> Vec<protocol::PersonalUnitState> {
    record_sync(
        status,
        peer.device_id,
        "root_catalog_request",
        None,
        None,
        None,
    );
    let local = store.root_catalog(peer);
    let local_digest = local.as_ref().ok().and_then(|inventory| {
        protocol::root_catalog_digest(peer, &inventory.units, &inventory.personal)
    });
    let response = tokio::time::timeout(
        SYNC_REQUEST_TIMEOUT,
        request_frame(connection, &protocol::PeerFrame::RootCatalogRequest),
    )
    .await;
    let (digest, mut units, personal) = match response {
        Ok(Ok(protocol::PeerFrame::RootCatalog {
            digest,
            units,
            personal,
        })) => (digest, units, personal),
        error => {
            let error_code = match &error {
                Ok(Ok(protocol::PeerFrame::Error { code })) => code.clone(),
                Ok(Err(error)) => error.stage.to_owned(),
                Err(_) => "timeout".to_owned(),
                Ok(Ok(_)) => "unexpected_root_catalog_frame".to_owned(),
            };
            record_sync(
                status,
                peer.device_id,
                "root_catalog_failed",
                None,
                None,
                Some(&error_code),
            );
            tracing::warn!(
                peer = %peer.device_id.to_storage(),
                peer_name = %peer.label(),
                connection_id = connection.stable_id(),
                pass_id,
                outcome = %timed_frame_result_summary(&error),
                "sync request failed request=root_catalog"
            );
            result.retry_all = true;
            result.remember_error(error_code, None);
            return Vec::new();
        }
    };
    let distinct_units = units.iter().map(|unit| unit.unit).collect::<BTreeSet<_>>();
    let distinct_personal = personal
        .iter()
        .map(|unit| unit.workspace)
        .collect::<BTreeSet<_>>();
    if units.iter().any(|unit| unit.validate().is_err())
        || personal.iter().any(|unit| unit.validate().is_err())
        || distinct_units.len() != units.len()
        || distinct_personal.len() != personal.len()
        || (!personal.is_empty() && !peer.binds_root_scope())
    {
        result.retry_all = true;
        result.remember_error("unauthorized_root_row", None);
        tracing::warn!(
            peer = %peer.device_id.to_storage(),
            peer_name = %peer.label(),
            connection_id = connection.stable_id(),
            pass_id,
            "sync root catalog refused: malformed or out-of-scope rows"
        );
        return Vec::new();
    }
    let digests_equal = local_digest.as_deref() == Some(digest.as_slice())
        && protocol::root_catalog_digest(peer, &units, &personal).as_deref()
            == Some(digest.as_slice());
    tracing::info!(
        peer = %peer.device_id.to_storage(),
        peer_name = %peer.label(),
        action = "root_catalog_recv",
        units = units.len(),
        personal = personal.len(),
        equal = digests_equal,
        "root_sync"
    );
    record_sync(
        status,
        peer.device_id,
        "root_catalog_received",
        None,
        Some(units.len()),
        None,
    );
    if local.is_ok() && digests_equal {
        record_sync(
            status,
            peer.device_id,
            "root_catalog_equal",
            None,
            None,
            None,
        );
        return Vec::new();
    }
    // Profile units before Workspace units: the wire tag order puts them
    // there, so a Workspace's placement meets a Profile this device knows.
    units.sort_by_key(|unit| protocol::root_unit_sort_key(unit.unit));
    for remote in units {
        pull_root_unit(connection, peer, store, status, remote, result, changes).await;
    }
    personal
}

async fn pull_root_unit<C: RequestTransport>(
    connection: &C,
    peer: &protocol::AuthenticatedPeer,
    store: &dyn protocol::ReplicationStore,
    status: &Arc<RwLock<NetworkStatus>>,
    remote: protocol::RootUnitState,
    result: &mut SyncPassResult,
    changes: &mut CatalogChanges,
) {
    let unit = remote.unit;
    let Ok(remote_version) = loro::VersionVector::decode(&remote.version) else {
        result.retry_all = true;
        result.remember_error("invalid_root_version", unit.workspace());
        return;
    };
    let (local, was_missing) = match store.root_unit_version(peer, unit) {
        Ok(version) => (version, false),
        Err(error) if error.code == protocol::ROOT_UNIT_NOT_FOUND => {
            (loro::VersionVector::new().encode(), true)
        }
        Err(error) if error.code == protocol::MODE_REFUSES_TRANSPORT => {
            // A known Local-only subject admits no inbound metadata (§4.4).
            // A state, not a failure.
            record_sync(
                status,
                peer.device_id,
                "root_unit_blocked",
                unit.workspace(),
                None,
                Some(error.code),
            );
            return;
        }
        Err(error) => {
            result.retry_all = true;
            result.remember_error(error.code, unit.workspace());
            return;
        }
    };
    let local_version = loro::VersionVector::decode(&local).unwrap_or_default();
    if !was_missing && local_version.includes_vv(&remote_version) {
        return;
    }
    let response = tokio::time::timeout(
        SYNC_REQUEST_TIMEOUT,
        request_frame(
            connection,
            &protocol::PeerFrame::RootUnitRequest {
                unit,
                version: local,
            },
        ),
    )
    .await;
    let error_code = match response {
        Ok(Ok(protocol::PeerFrame::RootUnitUpdates {
            unit: received,
            updates,
        })) if received == unit => {
            let update_count = updates.len();
            match store.apply_remote_root_updates(peer, unit, updates, Some(&remote_version)) {
                Ok(protocol::RootJoin::Applied { reconciled, .. }) => {
                    changes.root += 1;
                    record_state_change(
                        status,
                        Some(store),
                        peer.device_id,
                        if was_missing {
                            "root_unit_received"
                        } else {
                            "root_unit_updated"
                        },
                        unit.profile(),
                        unit.workspace(),
                        None,
                        None,
                        None,
                        None,
                        Some(update_count),
                    );
                    if !reconciled {
                        // Locally committed short of what the peer advertised:
                        // ask again from the new frontier next pass.
                        result.retry_all = true;
                        result.remember_error("root_unit_behind", unit.workspace());
                    }
                    return;
                }
                Ok(protocol::RootJoin::Pending { .. }) => {
                    tracing::info!(
                        peer = %peer.device_id.to_storage(),
                        peer_name = %peer.label(),
                        unit = ?unit,
                        "root_sync direction=peer_to_local phase=staged pending=missing_ancestors"
                    );
                    record_sync(
                        status,
                        peer.device_id,
                        "root_unit_pending",
                        unit.workspace(),
                        Some(update_count),
                        Some("root_unit_pending"),
                    );
                    "root_unit_pending".to_owned()
                }
                Err(error) => {
                    if error.code == protocol::POLICY_CHANGED {
                        tracing::info!(
                            peer = %peer.device_id.to_storage(),
                            peer_name = %peer.label(),
                            unit = ?unit,
                            "root_sync direction=peer_to_local action=candidate_discarded reason=policy_changed"
                        );
                    }
                    error.code.to_owned()
                }
            }
        }
        Ok(Ok(protocol::PeerFrame::Error { code })) if code == protocol::ROOT_UNIT_HELD => {
            // The peer holds this unit back for this recipient. Nothing to
            // retry from here; its policy decides when it lifts.
            record_sync(
                status,
                peer.device_id,
                "root_unit_held",
                unit.workspace(),
                None,
                Some(&code),
            );
            return;
        }
        Ok(Ok(protocol::PeerFrame::Error { code })) => code,
        Ok(Err(error)) => error.stage.to_owned(),
        Err(_) => "timeout".to_owned(),
        Ok(Ok(_)) => "unexpected_root_unit_frame".to_owned(),
    };
    result.retry_all = true;
    result.remember_error(error_code, unit.workspace());
}

/// Personal v2 units (§5.1), pulled after content bootstrap because their
/// rows reference the Workspace row. Only a scope-bound peer advertises any.
async fn pull_personal_units<C: RequestTransport>(
    connection: &C,
    peer: &protocol::AuthenticatedPeer,
    store: &dyn protocol::ReplicationStore,
    status: &Arc<RwLock<NetworkStatus>>,
    mut units: Vec<protocol::PersonalUnitState>,
    result: &mut SyncPassResult,
    changes: &mut CatalogChanges,
) {
    units.sort_by_key(|unit| unit.workspace);
    for remote in units {
        let workspace = remote.workspace;
        let Ok(remote_version) = loro::VersionVector::decode(&remote.version) else {
            result.retry_all = true;
            result.remember_error("invalid_root_version", Some(workspace));
            continue;
        };
        let (local, was_missing) = match store.personal_version(peer, workspace) {
            Ok(version) => (version, false),
            Err(error) if error.code == protocol::ROOT_UNIT_NOT_FOUND => {
                (loro::VersionVector::new().encode(), true)
            }
            Err(error) if error.code == protocol::MODE_REFUSES_TRANSPORT => {
                record_sync(
                    status,
                    peer.device_id,
                    "personal_unit_blocked",
                    Some(workspace),
                    None,
                    Some(error.code),
                );
                continue;
            }
            Err(error) => {
                result.retry_all = true;
                result.remember_error(error.code, Some(workspace));
                continue;
            }
        };
        let local_version = loro::VersionVector::decode(&local).unwrap_or_default();
        if !was_missing && local_version.includes_vv(&remote_version) {
            continue;
        }
        let response = tokio::time::timeout(
            SYNC_REQUEST_TIMEOUT,
            request_frame(
                connection,
                &protocol::PeerFrame::PersonalRequest {
                    workspace,
                    version: local,
                },
            ),
        )
        .await;
        let error_code = match response {
            Ok(Ok(protocol::PeerFrame::PersonalUpdates {
                workspace: received,
                updates,
            })) if received == workspace => {
                let update_count = updates.len();
                match store.apply_remote_personal_updates(
                    peer,
                    workspace,
                    updates,
                    Some(&remote_version),
                ) {
                    Ok(protocol::RootJoin::Applied { reconciled, .. }) => {
                        changes.root += 1;
                        record_state_change(
                            status,
                            Some(store),
                            peer.device_id,
                            "personal_updated",
                            None,
                            Some(workspace),
                            None,
                            None,
                            None,
                            None,
                            Some(update_count),
                        );
                        if !reconciled {
                            result.retry_all = true;
                            result.remember_error("root_unit_behind", Some(workspace));
                        }
                        continue;
                    }
                    Ok(protocol::RootJoin::Pending { .. }) => "root_unit_pending".to_owned(),
                    Err(error) => error.code.to_owned(),
                }
            }
            Ok(Ok(protocol::PeerFrame::Error { code })) if code == protocol::ROOT_UNIT_HELD => {
                record_sync(
                    status,
                    peer.device_id,
                    "root_unit_held",
                    Some(workspace),
                    None,
                    Some(&code),
                );
                continue;
            }
            Ok(Ok(protocol::PeerFrame::Error { code })) => code,
            Ok(Err(error)) => error.stage.to_owned(),
            Err(_) => "timeout".to_owned(),
            Ok(Ok(_)) => "unexpected_personal_frame".to_owned(),
        };
        result.retry_all = true;
        result.remember_error(error_code, Some(workspace));
    }
}

/// Pull every room whose remote version is ahead. The peer runs the same loop,
/// so a single authenticated connection exchanges missing changes in both
/// directions without assigning either device a server role.
async fn pull_missing<C>(
    connection: C,
    peer: protocol::AuthenticatedPeer,
    store: Arc<dyn protocol::ReplicationStore>,
    status: Arc<RwLock<NetworkStatus>>,
    blob_directory: SharedBlobDirectory,
    retry_scope: Option<BTreeSet<koru_store_core::WorkspaceId>>,
    pass_id: u64,
) -> SyncPassResult
where
    C: RequestTransport,
{
    let mut result = SyncPassResult::default();
    if let Some(code) = peer.protocol_refusal() {
        result.remember_error(code, None);
        return result;
    }
    let scoped_catalog = peer.all_data || !peer.authorized_profiles.is_empty();
    let reconciles_catalog = retry_scope.is_none() && scoped_catalog;
    // **The publication job** (root-catalog TECHNICAL.final §4.3, §4.4): a
    // placement or lifecycle fact written while its subject could not export
    // waits in a private overlay; every pass folds the ones whose admission
    // now allows export into their durable unit before the inventory is
    // framed. Failures are logged by the Store and left for the next pass.
    if let Err(error) = store.publish_pending_root_overlays() {
        tracing::warn!(
            peer = %peer.device_id.to_storage(),
            peer_name = %peer.label(),
            error_code = error.code,
            "root_sync action=publication_job_failed"
        );
    }
    // **Root units before Workspace inventories** (§7.2): placement is known
    // before content is provisioned, so a descriptor's Profile never has to
    // stand in for it. Only a scope-bound relationship — the same person's
    // device — exchanges root units; a Profile or Workspace grant is asked
    // nothing and installs nothing.
    let mut personal_units = Vec::new();
    let mut catalog_changes = CatalogChanges::default();
    if retry_scope.is_none() {
        match store.bind_root_scope(&peer) {
            Ok(true) => {
                personal_units = pull_root_catalog(
                    &connection,
                    &peer,
                    store.as_ref(),
                    &status,
                    &mut result,
                    &mut catalog_changes,
                    pass_id,
                )
                .await;
            }
            Ok(false) => {}
            Err(error) => {
                result.retry_all = true;
                result.remember_error(error.code, None);
                tracing::warn!(
                    peer = %peer.device_id.to_storage(),
                    peer_name = %peer.label(),
                    error_code = error.code,
                    "root_sync action=scope_binding_refused"
                );
                return result;
            }
        }
    }
    // A Profile-scoped retry must re-establish the catalog's immutable owner
    // binding. Retrying a bare WorkspaceId would let an inventory response
    // choose a different authorized Profile after the first pass failed.
    let direct_retry_scope =
        retry_scope.filter(|_| peer.all_data || peer.authorized_profiles.is_empty());
    let mut catalog_profiles_by_workspace = BTreeMap::new();
    let (profiles, workspaces): (Vec<protocol::ProfileReplicaState>, BTreeSet<_>) = if let Some(
        scope,
    ) =
        direct_retry_scope
    {
        (Vec::new(), scope)
    } else if scoped_catalog {
        record_sync(&status, peer.device_id, "catalog_request", None, None, None);
        let local_profiles = store.profiles(&peer);
        let local_catalog = store.catalog(&peer);
        let local_digest = match (&local_profiles, &local_catalog) {
            (Ok(profiles), Ok(workspaces)) => protocol::catalog_digest(&peer, profiles, workspaces),
            _ => None,
        };
        tracing::info!(
            peer = %peer.device_id.to_storage(),
            peer_name = %peer.label(),
            peer_name = %peer.display_name.as_deref().unwrap_or("-"),
            action = "catalog_send",
            digest = local_digest.as_deref().map(vv_tag).as_deref().unwrap_or("-"),
            profiles = local_profiles.as_ref().map_or(0, Vec::len),
            workspaces = local_catalog.as_ref().map_or(0, Vec::len),
            "sync"
        );
        let catalog_response = tokio::time::timeout(
            SYNC_REQUEST_TIMEOUT,
            request_frame(&connection, &protocol::PeerFrame::CatalogRequest),
        )
        .await;
        match catalog_response {
            Ok(Ok(protocol::PeerFrame::Catalog {
                digest,
                profiles,
                workspaces,
            })) => {
                let invalid_profile = profiles.iter().any(|profile| {
                    peer.check_authorized_profile_unit(profile.unit).is_err()
                        || profile.validate().is_err()
                }) || {
                    let distinct = profiles
                        .iter()
                        .map(|profile| profile.unit)
                        .collect::<BTreeSet<_>>();
                    distinct.len() != profiles.len()
                };
                let invalid_workspace = workspaces.iter().any(|workspace| {
                    !peer.all_data
                        && !peer.authorized_profiles.contains(&workspace.profile_id)
                        && !peer.authorized_workspaces.contains(&workspace.workspace_id)
                });
                if invalid_profile || invalid_workspace {
                    result.retry_all = true;
                    result.remember_error("unauthorized_catalog_row", None);
                    tracing::warn!(
                        peer = %peer.device_id.to_storage(),
                        peer_name = %peer.label(),
                        connection_id = connection.stable_id(),
                        pass_id,
                        "sync catalog refused: peer advertised a row outside its scope"
                    );
                    return result;
                }
                let digests_equal = local_digest.as_deref() == Some(digest.as_slice())
                    && protocol::catalog_digest(&peer, &profiles, &workspaces).as_deref()
                        == Some(digest.as_slice());
                tracing::info!(
                    peer = %peer.device_id.to_storage(),
                    peer_name = %peer.label(),
                    peer_name = %peer.display_name.as_deref().unwrap_or("-"),
                    action = "catalog_recv",
                    digest = vv_tag(&digest),
                    profiles = profiles.len(),
                    workspaces = workspaces.len(),
                    equal = digests_equal,
                    "sync"
                );
                record_sync(
                    &status,
                    peer.device_id,
                    "catalog_received",
                    None,
                    Some(workspaces.len()),
                    None,
                );
                if let (Ok(_), Ok(_)) = (&local_profiles, &local_catalog) {
                    if digests_equal {
                        record_sync(
                            &status,
                            peer.device_id,
                            "catalog_equal",
                            None,
                            Some(workspaces.len()),
                            None,
                        );
                        record_sync(
                            &status,
                            peer.device_id,
                            "sync_complete",
                            None,
                            Some(0),
                            None,
                        );
                        return result;
                    }
                } else {
                    tracing::warn!(
                        peer = %peer.device_id.to_storage(),
                        peer_name = %peer.label(),
                        connection_id = connection.stable_id(),
                        pass_id,
                        local_profiles = ?local_profiles.as_ref().err().map(|error| error.code),
                        local_catalog = ?local_catalog.as_ref().err().map(|error| error.code),
                        "sync local catalog digest unavailable; comparing every remote workspace"
                    );
                }
                let local_by_workspace = local_catalog
                    .unwrap_or_default()
                    .into_iter()
                    .map(|workspace| {
                        (
                            workspace.workspace_id,
                            (workspace.profile_id, workspace.digest),
                        )
                    })
                    .collect::<BTreeMap<_, _>>();
                let changed = workspaces
                    .into_iter()
                    .filter(|workspace| {
                        local_by_workspace.get(&workspace.workspace_id).is_none_or(
                            |(profile, digest)| {
                                *profile != workspace.profile_id || digest != &workspace.digest
                            },
                        )
                    })
                    .map(|workspace| {
                        catalog_profiles_by_workspace
                            .insert(workspace.workspace_id, workspace.profile_id);
                        workspace.workspace_id
                    })
                    .collect::<BTreeSet<_>>();
                (profiles, changed)
            }
            error => {
                let error_code = match &error {
                    Ok(Ok(protocol::PeerFrame::Error { code })) => code.clone(),
                    Ok(Err(error)) => error.stage.to_owned(),
                    Err(_) => "timeout".to_owned(),
                    Ok(Ok(_)) => "unexpected_catalog_frame".to_owned(),
                };
                record_sync(
                    &status,
                    peer.device_id,
                    "catalog_failed",
                    None,
                    None,
                    Some(&error_code),
                );
                tracing::warn!(
                    peer = %peer.device_id.to_storage(),
                    peer_name = %peer.label(),
                    connection_id = connection.stable_id(),
                    pass_id,
                    outcome = %timed_frame_result_summary(&error),
                    "sync request failed request=catalog"
                );
                result.retry_all = true;
                result.remember_error(error_code, None);
                return result;
            }
        }
    } else {
        (Vec::new(), peer.authorized_workspaces.clone())
    };
    // Materialize Profile identity/private first; personal rows need their
    // referenced Workspace catalog row and are pulled after workspace bootstrap.
    let (legacy_personal_units, profile_units): (Vec<_>, Vec<_>) = profiles
        .into_iter()
        .partition(|remote| matches!(remote.unit, protocol::ProfileUnitRef::Personal { .. }));
    pull_profile_units(
        &connection,
        &peer,
        store.as_ref(),
        &status,
        profile_units,
        &mut result,
        &mut catalog_changes,
    )
    .await;
    for workspace in &workspaces {
        // §4.4: a Workspace this device holds Local-only is not asked of a
        // peer at all. A state, not a failure — nothing is retried for it.
        if let Err(error) = store.admit_peer_subject(*workspace) {
            tracing::info!(
                peer = %peer.device_id.to_storage(),
                peer_name = %peer.label(),
                workspace = %workspace.to_storage(),
                error_code = error.code,
                "workspace_sync action=skipped reason=mode_blocks_peer_path"
            );
            record_sync(
                &status,
                peer.device_id,
                "workspace_blocked",
                Some(*workspace),
                None,
                Some(error.code),
            );
            continue;
        }
        // Both sides of the exchange, in the same shape: a mismatch between
        // what we think this workspace is and what the peer serves is the
        // whole story of a workspace that will not converge.
        let workspace_was_missing = match store.inventory(&peer, *workspace) {
            Ok(ours) => {
                let replica = &ours.replica;
                tracing::info!(
                    peer = %peer.device_id.to_storage(),
                    peer_name = %peer.label(),
                    workspace = %workspace.to_storage(),
                    name = %replica.workspace_name,
                    direction = "local_to_peer",
                    action = "inventory_send",
                    documents = ours.documents.len(),
                    vv = vv_tag(&ours.documents.iter().find(|d| d.is_manifest).map_or(Vec::new(), |d| d.version.clone())),
                    archived = replica.workspace_archived,
                    deleted_at_ms = replica.workspace_deleted_at_ms,
                    "workspace_sync"
                );
                false
            }
            Err(error) => {
                // workspace_not_found is the ordinary first contact: we do
                // not have the workspace yet, which is why we are asking.
                // Anything else is a real failure in our own inventory and
                // would otherwise surface only as a peer-side refusal.
                if error.code != "workspace_not_found" {
                    tracing::warn!(
                        peer = %peer.device_id.to_storage(),
                        peer_name = %peer.label(),
                        workspace = %workspace.to_storage(),
                        error_code = error.code,
                        "sync inventory of our own store failed before the request"
                    );
                }
                error.code == "workspace_not_found"
            }
        };
        let inventory_response = tokio::time::timeout(
            SYNC_REQUEST_TIMEOUT,
            request_frame(
                &connection,
                &protocol::PeerFrame::InventoryRequest {
                    workspace_id: *workspace,
                },
            ),
        )
        .await;
        let Ok(Ok(protocol::PeerFrame::Inventory(inventory))) = inventory_response else {
            let error_code = match &inventory_response {
                Ok(Ok(protocol::PeerFrame::Error { code })) => code.as_str(),
                Ok(Err(error)) => error.stage,
                Err(_) => "timeout",
                Ok(Ok(_)) => "unexpected_inventory_frame",
            };
            // **A profile the peer no longer has is not a failure to retry.**
            // Retrying asks the same question of the same answer every thirty
            // seconds, and it keeps the pass "incomplete", which drags every
            // healthy workspace on this peer into the same loop. Skipped
            // instead: the pass finishes, and the ordinary wake — a change on
            // either side, or a reconnect — is what tries again, which is also
            // exactly when the answer could have changed.
            if error_code != PEER_LOST_THE_PROFILE {
                result.failed_workspaces.insert(*workspace);
            }
            result.remember_error(error_code, Some(*workspace));
            record_sync(
                &status,
                peer.device_id,
                "inventory_failed",
                Some(*workspace),
                None,
                Some(error_code),
            );
            tracing::warn!(
                peer = %peer.device_id.to_storage(),
                peer_name = %peer.label(),
                connection_id = connection.stable_id(),
                pass_id,
                workspace = %workspace.to_storage(),
                outcome = %timed_frame_result_summary(&inventory_response),
                "sync request failed request=inventory"
            );
            continue;
        };
        if inventory.workspace_id != *workspace
            || inventory.replica.workspace_id != *workspace
            || (!peer.all_data && inventory.replica.has_profile_metadata())
            || catalog_profiles_by_workspace
                .get(workspace)
                .is_some_and(|profile| *profile != inventory.replica.profile_id)
            || (inventory.replica.workspace_deleted_at_ms.is_none()
                && !inventory.documents.iter().any(|document| {
                    document.is_manifest
                        && document.document_id == inventory.replica.manifest_document
                }))
        {
            result.failed_workspaces.insert(*workspace);
            result.remember_error("invalid_inventory", Some(*workspace));
            record_sync(
                &status,
                peer.device_id,
                "inventory_failed",
                Some(*workspace),
                None,
                Some("invalid_inventory"),
            );
            continue;
        }
        {
            let replica = &inventory.replica;
            tracing::info!(
                peer = %peer.device_id.to_storage(),
                peer_name = %peer.label(),
                workspace = %workspace.to_storage(),
                name = %replica.workspace_name,
                direction = "peer_to_local",
                action = "inventory_recv",
                documents = inventory.documents.len(),
                vv = vv_tag(&inventory.documents.iter().find(|d| d.is_manifest).map_or(Vec::new(), |d| d.version.clone())),
                archived = replica.workspace_archived,
                deleted_at_ms = replica.workspace_deleted_at_ms,
                "workspace_sync"
            );
        }
        // The advertisement hydration routes by: this peer says it holds these
        // blobs, so a later local miss asks it first. This used to be dropped
        // unread, which is why a missing image could only ever be asked for
        // blindly, in snapshot order.
        if !inventory.blobs.is_empty() {
            let mut directory = blob_directory
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            for blob in &inventory.blobs {
                directory.record(*workspace, &blob.hash, peer.device_id);
            }
            tracing::debug!(
                peer = %peer.device_id.to_storage(),
                peer_name = %peer.label(),
                workspace = %workspace.to_storage(),
                blobs = inventory.blobs.len(),
                "blob_hydration action=holders_recorded"
            );
            // The *origin* an advertisement names is durable provenance, not a
            // session hint: parked hydrations name it on the placeholder, and a
            // restart must not take that name away. A store that cannot record
            // it is not broken — the placeholder simply stays unnamed.
            match store.record_blob_origins(&peer, *workspace, &inventory.blobs) {
                Ok(recorded) if recorded > 0 => {
                    tracing::info!(
                        peer = %peer.device_id.to_storage(),
                        peer_name = %peer.label(),
                        workspace = %workspace.to_storage(),
                        origins = recorded,
                        "blob_hydration action=origins_recorded"
                    );
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(
                        peer = %peer.device_id.to_storage(),
                        peer_name = %peer.label(),
                        workspace = %workspace.to_storage(),
                        error_code = error.code,
                        "blob_hydration action=origins_record_failed"
                    );
                }
            }
        }
        if let Err(error) = store.provision_replica(&peer, &inventory.replica) {
            if error.code == protocol::PLACEMENT_PENDING {
                // Not a fault: no root placement is known here yet, and the
                // descriptor's Profile may not stand in for one (§7.2). The
                // whole pass retries, so the root exchange runs again before
                // this inventory is asked for.
                result.retry_all = true;
                result.remember_error(error.code, Some(*workspace));
                tracing::info!(
                    peer = %peer.device_id.to_storage(),
                    peer_name = %peer.label(),
                    workspace = %workspace.to_storage(),
                    "root_sync direction=peer_to_local action=workspace_pending reason=no_root_placement"
                );
                record_sync(
                    &status,
                    peer.device_id,
                    "workspace_pending",
                    Some(*workspace),
                    Some(inventory.documents.len()),
                    Some(error.code),
                );
                continue;
            }
            result.failed_workspaces.insert(*workspace);
            result.remember_error(error.code, Some(*workspace));
            tracing::warn!(
                peer = %peer.device_id.to_storage(),
                peer_name = %peer.label(),
                connection_id = connection.stable_id(),
                pass_id,
                workspace = %workspace.to_storage(),
                profile = %inventory.replica.profile_id.as_uuid(),
                manifest = %inventory.replica.manifest_document.as_uuid(),
                peer_all_data = peer.all_data,
                descriptor_has_profile_metadata = inventory.replica.has_profile_metadata(),
                descriptor_profile_created_at_ms = inventory.replica.profile_created_at_ms,
                descriptor_workspace_created_at_ms = inventory.replica.workspace_created_at_ms,
                error_code = error.code,
                error_detail = error.detail.as_deref().unwrap_or("-"),
                "sync workspace provisioning failed"
            );
            record_sync(
                &status,
                peer.device_id,
                "workspace_failed",
                Some(*workspace),
                Some(inventory.documents.len()),
                Some(error.code),
            );
            continue;
        }
        if workspace_was_missing {
            catalog_changes.workspaces += 1;
            record_state_change(
                &status,
                Some(store.as_ref()),
                peer.device_id,
                "workspace_created",
                None,
                Some(*workspace),
                None,
                None,
                None,
                None,
                Some(inventory.documents.len()),
            );
        }
        record_sync(
            &status,
            peer.device_id,
            "workspace_syncing",
            Some(*workspace),
            Some(inventory.documents.len()),
            None,
        );
        let mut workspace_failed = false;
        for remote in inventory.documents {
            let document_was_missing =
                !remote.is_manifest && matches!(store.locate(&peer, remote.document_id), Ok(None));
            if !remote.is_manifest {
                if let Err(error) = store.provision_document(&peer, *workspace, remote.document_id)
                {
                    result.failed_workspaces.insert(*workspace);
                    result.remember_error(error.code, Some(*workspace));
                    workspace_failed = true;
                    tracing::warn!(
                        peer = %peer.device_id.to_storage(),
                        peer_name = %peer.label(),
                        connection_id = connection.stable_id(),
                        pass_id,
                        workspace = %workspace.to_storage(),
                        document = %remote.document_id.to_storage(),
                        error_code = error.code,
                        error_detail = error.detail.as_deref().unwrap_or("-"),
                        "sync document provisioning failed"
                    );
                    continue;
                }
                if document_was_missing {
                    catalog_changes.documents += 1;
                    record_state_change(
                        &status,
                        Some(store.as_ref()),
                        peer.device_id,
                        "document_created",
                        None,
                        Some(*workspace),
                        Some(remote.document_id),
                        store.document_name(&peer, *workspace, remote.document_id),
                        None,
                        None,
                        Some(1),
                    );
                }
            }
            let version = store
                .version(&peer, *workspace, remote.document_id)
                .unwrap_or_else(|_| loro::VersionVector::new().encode());
            let local_version = loro::VersionVector::decode(&version).unwrap_or_default();
            let remote_version = loro::VersionVector::decode(&remote.version).unwrap_or_default();
            let missing = koru_replication::reconcile::missing(&local_version, &remote_version);
            let remote_ahead = missing.download;
            let local_ahead = missing.upload;
            if remote.is_manifest && (remote_ahead || local_ahead) {
                tracing::info!(
                    peer = %peer.device_id.to_storage(),
                    peer_name = %peer.label(),
                    workspace = %workspace.to_storage(),
                    name = %inventory.replica.workspace_name,
                    document = %remote.document_id.to_storage(),
                    remote_ahead,
                    local_ahead,
                    "workspace_sync direction=peer_to_local object=manifest action=compare_versions"
                );
            }
            // Inventory version vectors are authoritative Store state. If the
            // remote vector contributes nothing we do not need to open a Loro
            // stream merely to have the peer reach the same conclusion. This
            // is the difference between an incremental scan and 1,333 serial
            // round trips through an unchanged workspace.
            if !remote_ahead {
                continue;
            }
            let message = loro_protocol::protocol::ProtocolMessage::JoinRequest {
                crdt: loro_protocol::protocol::CrdtType::Loro,
                room_id: remote.room_id.clone(),
                version: version.clone(),
                auth: Vec::new(),
            };
            let Ok(message) = loro_protocol::encoding::encode(&message) else {
                result.failed_workspaces.insert(*workspace);
                result.remember_error("encode", Some(*workspace));
                workspace_failed = true;
                tracing::warn!(
                    peer = %peer.device_id.to_storage(),
                    peer_name = %peer.label(),
                    connection_id = connection.stable_id(),
                    pass_id,
                    workspace = %workspace.to_storage(),
                    document = %remote.document_id.to_storage(),
                    manifest = remote.is_manifest,
                    "sync request failed request=loro stage=encode"
                );
                continue;
            };
            let response = tokio::time::timeout(
                SYNC_REQUEST_TIMEOUT,
                request_frame(
                    &connection,
                    &protocol::PeerFrame::Loro {
                        workspace_id: *workspace,
                        message,
                    },
                ),
            )
            .await;
            let response = match response {
                Ok(Ok(response)) => response,
                Ok(Err(error)) => {
                    result.failed_workspaces.insert(*workspace);
                    result.remember_error(error.stage, Some(*workspace));
                    workspace_failed = true;
                    tracing::warn!(
                        peer = %peer.device_id.to_storage(),
                        peer_name = %peer.label(),
                        connection_id = connection.stable_id(),
                        pass_id,
                        workspace = %workspace.to_storage(),
                        document = %remote.document_id.to_storage(),
                        manifest = remote.is_manifest,
                        error = %error,
                        "sync request failed request=loro"
                    );
                    break;
                }
                Err(_) => {
                    result.failed_workspaces.insert(*workspace);
                    result.remember_error("timeout", Some(*workspace));
                    workspace_failed = true;
                    tracing::warn!(
                        peer = %peer.device_id.to_storage(),
                        peer_name = %peer.label(),
                        connection_id = connection.stable_id(),
                        pass_id,
                        workspace = %workspace.to_storage(),
                        document = %remote.document_id.to_storage(),
                        manifest = remote.is_manifest,
                        timeout_ms = SYNC_REQUEST_TIMEOUT.as_millis() as u64,
                        "sync request failed request=loro stage=timeout"
                    );
                    break;
                }
            };
            let update_count = protocol::doc_update_count(&response);
            // A refused join is not a completed document: without this check
            // the refusal was decoded and dropped by `handle_frame`, and the
            // workspace reported complete while the document never arrived.
            if let Some((room_id, code)) = protocol::join_refusal(&response) {
                result.failed_workspaces.insert(*workspace);
                result.remember_error(code.as_str(), Some(*workspace));
                workspace_failed = true;
                tracing::warn!(
                    peer = %peer.device_id.to_storage(),
                    peer_name = %peer.label(),
                    connection_id = connection.stable_id(),
                    pass_id,
                    workspace = %workspace.to_storage(),
                    document = %remote.document_id.to_storage(),
                    room_id,
                    error_code = code.as_str(),
                    "sync document join refused by peer"
                );
                record_sync(
                    &status,
                    peer.device_id,
                    "document_refused",
                    Some(*workspace),
                    None,
                    Some(code.as_str()),
                );
                continue;
            }
            if remote.is_manifest && remote_ahead {
                tracing::info!(
                    peer = %peer.device_id.to_storage(),
                    peer_name = %peer.label(),
                    workspace = %workspace.to_storage(),
                    document = %remote.document_id.to_storage(),
                    updates = update_count,
                    "workspace_sync direction=peer_to_local object=manifest action=response_received"
                );
            }
            // Join replies are informational; DocUpdate replies traverse the
            // same durable handler as unsolicited peer updates.
            let _ = protocol::handle_frame(store.as_ref(), &peer, response);
            let after_version = store
                .version(&peer, *workspace, remote.document_id)
                .unwrap_or_default();
            if after_version != version {
                if remote.is_manifest {
                    catalog_changes.workspaces += 1;
                } else {
                    catalog_changes.documents += 1;
                }
                record_state_change(
                    &status,
                    Some(store.as_ref()),
                    peer.device_id,
                    if remote.is_manifest {
                        "workspace_updated"
                    } else {
                        "document_updated"
                    },
                    None,
                    Some(*workspace),
                    Some(remote.document_id),
                    (!remote.is_manifest)
                        .then(|| store.document_name(&peer, *workspace, remote.document_id))
                        .flatten(),
                    None,
                    None,
                    imported_op_count(&version, &after_version).or(Some(update_count)),
                );
            }
            // A manifest pull that changes nothing is the silent failure that
            // keeps a workspace listing empty forever: the apply failed, or
            // Loro parked the updates as pending because their causal
            // ancestors are missing — both import `Ok` and move no version.
            // The entries index derives from the manifest, so a stalled
            // manifest is a workspace whose documents replicate and never
            // appear. Name it, and let the retry scope carry the workspace.
            if remote.is_manifest && remote_ahead {
                let manifest_advanced = |store: &dyn protocol::ReplicationStore| {
                    let after = store
                        .version(&peer, *workspace, remote.document_id)
                        .unwrap_or_default();
                    let after_vv = loro::VersionVector::decode(&after).unwrap_or_default();
                    (after_vv.includes_vv(&remote_version), after)
                };
                let (mut advanced, mut after) = manifest_advanced(store.as_ref());
                if !advanced {
                    // Ask for everything before declaring a stall. A delta
                    // whose causal ancestors are missing imports `Ok` and is
                    // parked as pending, but a full-history export merges as
                    // a plain CRDT union — even when the two histories share
                    // no common root. This is the self-heal for a replica
                    // that received one bad delta and could never catch up.
                    let full_join = loro_protocol::protocol::ProtocolMessage::JoinRequest {
                        crdt: loro_protocol::protocol::CrdtType::Loro,
                        room_id: remote.room_id.clone(),
                        version: loro::VersionVector::new().encode(),
                        auth: Vec::new(),
                    };
                    if let Ok(message) = loro_protocol::encoding::encode(&full_join) {
                        if let Ok(Ok(response)) = tokio::time::timeout(
                            SYNC_TRANSFER_TIMEOUT,
                            request_frame(
                                &connection,
                                &protocol::PeerFrame::Loro {
                                    workspace_id: *workspace,
                                    message,
                                },
                            ),
                        )
                        .await
                        {
                            let _ = protocol::handle_frame(store.as_ref(), &peer, response);
                        }
                    }
                    (advanced, after) = manifest_advanced(store.as_ref());
                    if advanced {
                        tracing::info!(
                            peer = %peer.device_id.to_storage(),
                            peer_name = %peer.label(),
                            workspace = %workspace.to_storage(),
                            document = %remote.document_id.to_storage(),
                            "workspace_sync direction=peer_to_local object=manifest action=full_resync_healed"
                        );
                    }
                }
                if !advanced {
                    result.failed_workspaces.insert(*workspace);
                    result.remember_error("manifest_stalled", Some(*workspace));
                    workspace_failed = true;
                    tracing::warn!(
                        peer = %peer.device_id.to_storage(),
                        peer_name = %peer.label(),
                        connection_id = connection.stable_id(),
                        pass_id,
                        workspace = %workspace.to_storage(),
                        document = %remote.document_id.to_storage(),
                        local_vv = %vv_tag(&after),
                        remote_vv = %vv_tag(&remote.version),
                        updates = update_count,
                        "sync manifest did not advance even after a full-history pull"
                    );
                    record_sync(
                        &status,
                        peer.device_id,
                        "manifest_stalled",
                        Some(*workspace),
                        Some(update_count),
                        Some("manifest_stalled"),
                    );
                }
            }
        }
        if !workspace_failed {
            record_sync(
                &status,
                peer.device_id,
                "workspace_complete",
                Some(*workspace),
                None,
                None,
            );
        }
    }
    pull_profile_units(
        &connection,
        &peer,
        store.as_ref(),
        &status,
        legacy_personal_units,
        &mut result,
        &mut catalog_changes,
    )
    .await;
    // Personal v2 units after content bootstrap: their rows reference the
    // Workspace row, which provisioning above may just have created.
    pull_personal_units(
        &connection,
        &peer,
        store.as_ref(),
        &status,
        personal_units,
        &mut result,
        &mut catalog_changes,
    )
    .await;
    if reconciles_catalog && catalog_changes.total() > 0 {
        record_state_change(
            &status,
            Some(store.as_ref()),
            peer.device_id,
            "catalog_reconciled",
            None,
            None,
            None,
            None,
            None,
            Some(catalog_changes.summary()),
            Some(catalog_changes.total()),
        );
    }
    if result.retry_all || !result.failed_workspaces.is_empty() {
        let error_code = result
            .retry_error_code
            .as_deref()
            .unwrap_or("workspace_retry");
        record_sync(
            &status,
            peer.device_id,
            "sync_incomplete",
            // Name the workspace the first error belongs to: a 1 Hz consumer
            // only sees a pass's last event, and `workspace_id=-` next to a
            // real error code is a log that cannot be acted on.
            result.retry_error_workspace,
            Some(workspaces.len()),
            Some(error_code),
        );
    } else {
        record_sync(
            &status,
            peer.device_id,
            "sync_complete",
            None,
            Some(workspaces.len()),
            None,
        );
    }
    result
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct FrameError {
    stage: &'static str,
    detail: String,
}

/// One ranged read of a blob from an authorized peer on a **direct** path.
///
/// §8's first candidate, and deliberately **not** a second hydration path: it
/// does not park, retry, wake or record a holder. The blob engine owns the
/// ordering and the fall-through; what it needs from a transport is one
/// bounded read, and `None` — no such peer right now — is that fall-through
/// rather than a failure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeerBlobRange {
    pub bytes: Vec<u8>,
    pub total: u64,
    pub mime: String,
    pub filename: Option<String>,
}

/// Ask one admitted peer for one range, over whichever set of connections the
/// caller has already narrowed to authorized ones.
///
/// D246's gate is applied per candidate, here, so a peer whose selected path is
/// a relay is skipped rather than refused loudly: there may be another peer on
/// a direct path, and the engine's next candidate after all of them is Cloud.
pub(crate) async fn read_blob_range_from<C>(
    peers: Vec<(protocol::AuthenticatedPeer, C)>,
    workspace: koru_store_core::WorkspaceId,
    hash: BlobHash,
    offset: u64,
    max: u64,
) -> Option<PeerBlobRange>
where
    C: RequestTransport,
{
    for (_, connection) in peers {
        if !connection.blob_path_admitted() {
            continue;
        }
        let request = protocol::PeerFrame::BlobRequest {
            workspace_id: workspace,
            hash: hash.clone(),
            offset,
            max_bytes: max.min(hydration::BLOB_CHUNK_BYTES as u64) as u32,
        };
        let answer =
            tokio::time::timeout(SYNC_TRANSFER_TIMEOUT, request_frame(&connection, &request)).await;
        let Ok(Ok(protocol::PeerFrame::BlobChunk(chunk))) = answer else {
            // A peer that refused, timed out or answered something else is one
            // this read does not use. The next candidate, then Cloud.
            continue;
        };
        if chunk.workspace_id != workspace || chunk.blob.hash != hash || chunk.offset != offset {
            continue;
        }
        if chunk.bytes.is_empty() {
            continue;
        }
        return Some(PeerBlobRange {
            bytes: chunk.bytes,
            total: chunk.blob.size,
            mime: chunk.blob.mime,
            filename: chunk.blob.filename,
        });
    }
    None
}

trait RequestTransport: Clone + Send + Sync + 'static {
    fn stable_id(&self) -> usize;

    fn request<'a>(
        &'a self,
        frame: &'a protocol::PeerFrame,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<protocol::PeerFrame, FrameError>> + Send + 'a>,
    >;

    /// **D246's byte gate**: is this connection's *selected* path an IP path
    /// right now? Never `path_kind()` — see [`crate::blob_path`].
    fn blob_path_admitted(&self) -> bool;

    /// Resolves when the answer above stops being true, so an in-flight
    /// transfer can be raced against it rather than discovering the change at
    /// the end of its current chunk.
    fn until_blob_path_closes<'a>(
        &'a self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>>;
}

impl RequestTransport for iroh::endpoint::Connection {
    fn stable_id(&self) -> usize {
        iroh::endpoint::Connection::stable_id(self)
    }

    fn request<'a>(
        &'a self,
        frame: &'a protocol::PeerFrame,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<protocol::PeerFrame, FrameError>> + Send + 'a>,
    > {
        Box::pin(request_frame_iroh(self, frame))
    }

    fn blob_path_admitted(&self) -> bool {
        blob_path::selected_path_is_ip(self)
    }

    fn until_blob_path_closes<'a>(
        &'a self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
        // Cloned, because `paths_stream()` borrows the connection and the
        // borrow cannot cross a task boundary.
        let connection = self.clone();
        Box::pin(blob_path::until_selected_path_leaves_ip(connection))
    }
}

#[derive(Clone)]
struct EdgeRequestConnection {
    connection_id: usize,
    peer: DeviceId,
    egress: Arc<dyn koru_network_edge::NetworkEgress>,
    /// **What the last opened session observed about the selected path.**
    ///
    /// This transport holds no `Connection` — the endpoint lives in another
    /// process, and the only live fact it gets back is the `PathMetadata` on
    /// each session it opens. That turns out to be exactly the right grain:
    /// one session is opened **per frame**, so the answer is re-observed once
    /// per blob chunk, which is the same bound the watcher gives the in-process
    /// runtime.
    ///
    /// Seeded from the connect event and refreshed on every open. `false` until
    /// something is observed: an edge that cannot see its selected path does
    /// not carry blob bytes on a guess.
    selected_is_ip: Arc<std::sync::atomic::AtomicBool>,
}

impl RequestTransport for EdgeRequestConnection {
    fn stable_id(&self) -> usize {
        self.connection_id
    }

    fn request<'a>(
        &'a self,
        frame: &'a protocol::PeerFrame,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<protocol::PeerFrame, FrameError>> + Send + 'a>,
    > {
        Box::pin(async move {
            let mut session = self
                .egress
                .open(
                    koru_network_edge::NetworkTarget::Device {
                        device_id: self.peer,
                    },
                    replication_protocol(),
                )
                .await
                .map_err(|error| FrameError::new("open", error))?;
            // The freshest observation this transport can have, recorded
            // before the frame goes out so the next chunk's gate reads it.
            self.selected_is_ip.store(
                session.path.selected_is_ip,
                std::sync::atomic::Ordering::Relaxed,
            );
            let bytes = encode_json_frame(frame)?;
            session
                .send
                .send(bytes)
                .await
                .map_err(|error| FrameError::new("write", error))?;
            read_edge_frame(&mut session.receive).await
        })
    }

    fn blob_path_admitted(&self) -> bool {
        self.selected_is_ip
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    fn until_blob_path_closes<'a>(
        &'a self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
        // **No watcher here, and that is the honest answer.** A change in the
        // selected path reaches this process only on the next session open, so
        // the per-chunk gate above *is* the whole fence for the bridged shape —
        // one chunk, the same bound the watcher gives, arrived at differently.
        Box::pin(std::future::pending())
    }
}

impl FrameError {
    fn new(stage: &'static str, error: impl fmt::Display) -> Self {
        Self {
            stage,
            detail: error.to_string(),
        }
    }

    fn message(stage: &'static str, detail: impl Into<String>) -> Self {
        Self {
            stage,
            detail: detail.into(),
        }
    }
}

impl fmt::Display for FrameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.stage, self.detail)
    }
}

fn frame_kind(frame: &protocol::PeerFrame) -> &'static str {
    match frame {
        protocol::PeerFrame::SyncRequest => "sync_request",
        protocol::PeerFrame::SyncAck => "sync_ack",
        protocol::PeerFrame::CatalogRequest => "catalog_request",
        protocol::PeerFrame::Catalog { .. } => "catalog",
        protocol::PeerFrame::ProfileRequest { .. } => "profile_request",
        protocol::PeerFrame::ProfileUpdates { .. } => "profile_updates",
        protocol::PeerFrame::RootCatalogRequest => "root_catalog_request",
        protocol::PeerFrame::RootCatalog { .. } => "root_catalog",
        protocol::PeerFrame::RootUnitRequest { .. } => "root_unit_request",
        protocol::PeerFrame::RootUnitUpdates { .. } => "root_unit_updates",
        protocol::PeerFrame::PersonalRequest { .. } => "personal_request",
        protocol::PeerFrame::PersonalUpdates { .. } => "personal_updates",
        protocol::PeerFrame::InventoryRequest { .. } => "inventory_request",
        protocol::PeerFrame::Inventory(_) => "inventory",
        protocol::PeerFrame::CloudCompare { .. } => "cloud_compare",
        protocol::PeerFrame::CloudCompared { .. } => "cloud_compared",
        protocol::PeerFrame::BlobRequest { .. } => "blob_request",
        protocol::PeerFrame::BlobChunk(_) => "blob_chunk",
        protocol::PeerFrame::Loro { .. } => "loro",
        protocol::PeerFrame::LoroBatch { .. } => "loro_batch",
        protocol::PeerFrame::TrustRecord { .. } => "trust_record",
        protocol::PeerFrame::TrustRecords { .. } => "trust_records",
        protocol::PeerFrame::TrustAck => "trust_ack",
        protocol::PeerFrame::Error { .. } => "error",
    }
}

/// A privacy-safe request result for diagnostics. Never debug-print a
/// `PeerFrame`: Loro and blob frames contain customer bytes.
fn frame_result_summary(result: &Result<protocol::PeerFrame, FrameError>) -> String {
    match result {
        Ok(protocol::PeerFrame::Error { code }) => format!("peer_error:{code}"),
        Ok(frame) => format!("unexpected_frame:{}", frame_kind(frame)),
        Err(error) => error.to_string(),
    }
}

fn timed_frame_result_summary(
    result: &Result<Result<protocol::PeerFrame, FrameError>, tokio::time::error::Elapsed>,
) -> String {
    match result {
        Ok(result) => frame_result_summary(result),
        Err(error) => format!("timeout: {error}"),
    }
}

async fn request_frame<C>(
    connection: &C,
    request: &protocol::PeerFrame,
) -> Result<protocol::PeerFrame, FrameError>
where
    C: RequestTransport,
{
    connection.request(request).await
}

async fn request_frame_iroh(
    connection: &iroh::endpoint::Connection,
    request: &protocol::PeerFrame,
) -> Result<protocol::PeerFrame, FrameError> {
    let (mut send, mut recv) = connection
        .open_bi()
        .await
        .map_err(|error| FrameError::new("open", error))?;
    write_frame(&mut send, request).await?;
    send.finish()
        .map_err(|error| FrameError::new("finish", error))?;
    read_frame(&mut recv).await
}

fn encode_json_frame<T: Serialize>(frame: &T) -> Result<Vec<u8>, FrameError> {
    let bytes = serde_json::to_vec(frame).map_err(|error| FrameError::new("encode", error))?;
    if bytes.len() > protocol::MAX_FRAME_BYTES {
        return Err(FrameError::message(
            "encode",
            format!(
                "frame is {} bytes; limit is {}",
                bytes.len(),
                protocol::MAX_FRAME_BYTES
            ),
        ));
    }
    let mut framed = Vec::with_capacity(4 + bytes.len());
    framed.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    framed.extend_from_slice(&bytes);
    Ok(framed)
}

async fn read_edge_frame(
    receive: &mut koru_network_edge::ByteReceiver,
) -> Result<protocol::PeerFrame, FrameError> {
    let mut bytes = Vec::new();
    let mut expected = None;
    loop {
        if let Some(length) = expected
            && bytes.len() >= 4 + length
        {
            return serde_json::from_slice(&bytes[4..4 + length])
                .map_err(|error| FrameError::new("decode", error));
        }
        let chunk = receive
            .recv()
            .await
            .map_err(|error| FrameError::new("read", error))?
            .ok_or_else(|| FrameError::message("read", "stream ended before one frame"))?;
        bytes.extend_from_slice(&chunk);
        if expected.is_none() && bytes.len() >= 4 {
            let length =
                u32::from_be_bytes(bytes[..4].try_into().expect("four-byte length")) as usize;
            if length > protocol::MAX_FRAME_BYTES {
                return Err(FrameError::message(
                    "read_length",
                    format!(
                        "peer announced {length} bytes; limit is {}",
                        protocol::MAX_FRAME_BYTES
                    ),
                ));
            }
            expected = Some(length);
        }
    }
}

async fn send_edge_frame(
    send: &koru_network_edge::ByteSender,
    frame: &protocol::PeerFrame,
) -> Result<(), FrameError> {
    send.send(encode_json_frame(frame)?)
        .await
        .map_err(|error| FrameError::new("write", error))
}

async fn write_frame(
    send: &mut iroh::endpoint::SendStream,
    frame: &protocol::PeerFrame,
) -> Result<(), FrameError> {
    write_json(send, frame).await
}

async fn write_json<T: Serialize>(
    send: &mut iroh::endpoint::SendStream,
    frame: &T,
) -> Result<(), FrameError> {
    let bytes = serde_json::to_vec(frame).map_err(|error| FrameError::new("encode", error))?;
    if bytes.len() > protocol::MAX_FRAME_BYTES {
        return Err(FrameError::message(
            "encode",
            format!(
                "frame is {} bytes; limit is {}",
                bytes.len(),
                protocol::MAX_FRAME_BYTES
            ),
        ));
    }
    send.write_all(&(bytes.len() as u32).to_be_bytes())
        .await
        .map_err(|error| FrameError::new("write_length", error))?;
    send.write_all(&bytes)
        .await
        .map_err(|error| FrameError::new("write_body", error))
}

async fn read_frame(
    recv: &mut iroh::endpoint::RecvStream,
) -> Result<protocol::PeerFrame, FrameError> {
    read_json(recv).await
}

async fn read_json<T: for<'de> Deserialize<'de>>(
    recv: &mut iroh::endpoint::RecvStream,
) -> Result<T, FrameError> {
    let mut length = [0; 4];
    recv.read_exact(&mut length)
        .await
        .map_err(|error| FrameError::new("read_length", error))?;
    let length = u32::from_be_bytes(length) as usize;
    if length > protocol::MAX_FRAME_BYTES {
        return Err(FrameError::message(
            "read_length",
            format!(
                "peer announced {length} bytes; limit is {}",
                protocol::MAX_FRAME_BYTES
            ),
        ));
    }
    let mut bytes = vec![0; length];
    recv.read_exact(&mut bytes)
        .await
        .map_err(|error| FrameError::new("read_body", error))?;
    serde_json::from_slice(&bytes).map_err(|error| FrameError::new("decode", error))
}

fn authorized_endpoint(
    authorization: &RwLock<Option<AuthorizationSnapshot>>,
    endpoint: EndpointId,
) -> Option<AuthorizedPeer> {
    authorization
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()?
        .peers
        .iter()
        .find(|peer| {
            peer.endpoint_id.as_bytes() == endpoint.as_bytes()
                && (peer.all_data
                    || !peer.authorized_profiles.is_empty()
                    || !peer.workspaces.is_empty())
        })
        .cloned()
}

/// Under `KORU_SYNC_DEBUG`, say who came up, how, on what build and how far
/// apart the two builds are. Drift warnings/errors remain visible without the
/// switch: a phone and desktop built from one tree are not a mismatch; a minor
/// gap warns, while a major gap or different protocol number is an error.
///
/// The protocol error is said only when `report_incompatible` — the peer has
/// just entered the incompatible state ([`reconnect::IncompatibleReports`]).
/// Repeating it on every connection is what turned an older peer closing on
/// us into an error line every few milliseconds.
fn log_peer_connected(
    authorization: &RwLock<Option<AuthorizationSnapshot>>,
    peer: &AuthorizedPeer,
    connection_id: usize,
    path: PathKind,
    direction: &'static str,
    report_incompatible: bool,
) {
    let local_build = authorization
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(|snapshot| snapshot.local_build.clone());
    let peer_name = peer.display_name.as_deref().unwrap_or("?");
    let peer_build = peer.build.as_ref().map(BuildPayload::label);
    let local_label = local_build.as_ref().map(BuildPayload::label);
    let drift = build_drift(authorization, peer);
    if koru_env::switch(koru_env::name::KORU_SYNC_DEBUG) {
        tracing::debug!(
            target: "koru_sync::peer",
            peer_name,
            device = %peer.device_id.to_storage(),
            connection_id,
            ?path,
            direction,
            peer_build = peer_build.as_deref().unwrap_or("unknown"),
            local_build = local_label.as_deref().unwrap_or("unknown"),
            drift = drift.as_str(),
            "peer connected"
        );
    }
    let peer_build = peer_build.as_deref().unwrap_or("unknown");
    let local_build = local_label.as_deref().unwrap_or("unknown");
    match drift {
        koru_device::BuildDrift::Incompatible { .. } if !report_incompatible => {}
        koru_device::BuildDrift::Incompatible { ours, theirs } => tracing::error!(
            peer_name,
            peer_build,
            local_build,
            peer_protocol = theirs,
            local_protocol = ours,
            "incompatible_protocol: sync refused until one side updates"
        ),
        koru_device::BuildDrift::Major => tracing::error!(
            peer_name,
            peer_build,
            local_build,
            drift = "major",
            "build_drift: the two devices are a major version apart"
        ),
        koru_device::BuildDrift::Minor => tracing::warn!(
            peer_name,
            peer_build,
            local_build,
            drift = "minor",
            "build_drift: the two devices are a minor version apart"
        ),
        _ => {}
    }
}

fn log_rapid_peer_reconnects(
    authorization: &RwLock<Option<AuthorizationSnapshot>>,
    peer: &AuthorizedPeer,
    connection_id: usize,
    path: PathKind,
    direction: &'static str,
    connections: usize,
) {
    let local_build = authorization
        .read()
        .unwrap_or_else(|error| error.into_inner())
        .as_ref()
        .and_then(|snapshot| snapshot.local_build.clone());
    let peer_build = peer.build.as_ref().map(BuildPayload::label);
    let local_label = local_build.as_ref().map(BuildPayload::label);
    tracing::info!(
        peer_name = peer.display_name.as_deref().unwrap_or("?"),
        device = %peer.device_id.to_storage(),
        connection_id,
        ?path,
        direction,
        peer_build = peer_build.as_deref().unwrap_or("unknown"),
        local_build = local_label.as_deref().unwrap_or("unknown"),
        drift = build_drift(authorization, peer).as_str(),
        connections,
        window_seconds = RAPID_CONNECTION_WINDOW.as_secs(),
        "Rapid disconnects: peer connected more than 10 times in one minute"
    );
}

fn log_peer_disconnected(
    authorization: &RwLock<Option<AuthorizationSnapshot>>,
    device_id: DeviceId,
    connection_id: usize,
    direction: ConnectionDirection,
    path: PathKind,
    lifetime: Duration,
    reason: &str,
    was_current: bool,
) {
    let peer_name = authorization
        .read()
        .unwrap_or_else(|error| error.into_inner())
        .as_ref()
        .and_then(|snapshot| {
            snapshot
                .peers
                .iter()
                .find(|peer| peer.device_id == device_id)
                .and_then(|peer| peer.display_name.as_deref())
        })
        .unwrap_or("?")
        .to_owned();
    if koru_env::switch(koru_env::name::KORU_SYNC_DEBUG) {
        tracing::debug!(
            target: "koru_sync::peer",
            peer_name,
            device = %device_id.to_storage(),
            connection_id,
            ?path,
            direction = direction.as_str(),
            lifetime_ms = lifetime.as_millis(),
            reason,
            was_current,
            "peer disconnected"
        );
    }
}

/// The stable diagnostic code for a failed outbound dial, or `None` when
/// nothing observable names the cause.
///
/// `relay_unreachable` requires evidence, never a reading of a generic
/// timeout: this runtime is configured to use relays
/// ([`RelayProviderConfig`] is not `Disabled`) **and** the endpoint's own
/// relay-connection state ([`Endpoint::home_relay_status`]) reports no live
/// relay connection at the moment the dial failed.
///
/// What iroh 1.1 exposes, and therefore what the code means exactly: the
/// endpoint tracks the connection state of its *selected home relay* only. A
/// relay that dies after connecting reads `Disconnected { last_error }`; a
/// relay that was never reachable is never selected as home (selection needs
/// a measured probe latency), leaving the status list empty. Both read here
/// as "told to use relays, holding no relay connection" — the dead-relay
/// signature — while any dial that fails for another reason under a healthy
/// relay keeps a connected entry and gets no code. The dial error chain
/// itself (`ConnectError`) never distinguishes relay reachability and is
/// deliberately not consulted.
fn dial_failure_code(endpoint: &Endpoint, relays_configured: bool) -> Option<&'static str> {
    use iroh::Watcher as _;
    if !relays_configured {
        return None;
    }
    let relay_connected = endpoint
        .home_relay_status()
        .get()
        .iter()
        .any(|status| status.is_connected());
    (!relay_connected).then_some("relay_unreachable")
}

/// Under `KORU_SYNC_DEBUG`, narrate the outbound dial: the target's address
/// composition, the ALPN, and the relay-connection state at dial time. The
/// connected stage is `log_peer_connected` (with its path), and the failed
/// stage is [`log_dial_failed`]. These lines are activity, not news; the
/// unconditional surface is only the stable code in [`NetworkStatus`].
fn log_dial_started(endpoint: &Endpoint, device_id: DeviceId, target: &EndpointAddr) {
    use iroh::Watcher as _;
    if !koru_env::switch(koru_env::name::KORU_SYNC_DEBUG) {
        return;
    }
    tracing::debug!(
        target: "koru_sync::peer",
        device = %device_id.to_storage(),
        direct_addrs = target.ip_addrs().count(),
        relay_addrs = target.relay_urls().count(),
        alpn = %String::from_utf8_lossy(REPLICATION_ALPN_V1),
        relay_status = ?endpoint.home_relay_status().get(),
        "peer dial started"
    );
}

/// Under `KORU_SYNC_DEBUG`, say how a dial failed: the iroh error chain, the
/// relay-connection state it was judged against, and the resulting stable
/// code (if the evidence named one).
fn log_dial_failed(
    endpoint: &Endpoint,
    device_id: DeviceId,
    error: &iroh::endpoint::ConnectError,
    code: Option<&'static str>,
) {
    use iroh::Watcher as _;
    if !koru_env::switch(koru_env::name::KORU_SYNC_DEBUG) {
        return;
    }
    tracing::debug!(
        target: "koru_sync::peer",
        device = %device_id.to_storage(),
        error = ?error,
        relay_status = ?endpoint.home_relay_status().get(),
        code = code.unwrap_or("none"),
        "peer dial failed"
    );
}

fn set_peer(status: &RwLock<NetworkStatus>, device: DeviceId, state: PeerConnectionState) {
    let mut status = status.write().unwrap_or_else(|error| error.into_inner());
    let was_connected = status
        .peers
        .get(&device)
        .is_some_and(|value| matches!(value, PeerConnectionState::Connected { .. }));
    let is_connected = matches!(state, PeerConnectionState::Connected { .. });
    if is_connected || was_connected {
        status.last_seen_millis.insert(device, wall_time_millis());
    }
    status.peers.insert(device, state);
}

fn record_sync(
    status: &RwLock<NetworkStatus>,
    peer: DeviceId,
    stage: &'static str,
    workspace: Option<koru_store_core::WorkspaceId>,
    items: Option<usize>,
    error_code: Option<&str>,
) {
    let mut status = status.write().unwrap_or_else(|error| error.into_inner());
    let sequence = status
        .last_sync_event
        .as_ref()
        .map_or(1, |event| event.sequence.saturating_add(1));
    status.last_sync_event = Some(SyncEvent {
        sequence,
        at_millis: wall_time_millis(),
        peer,
        stage,
        workspace,
        items,
        error_code: error_code.map(str::to_owned),
    });
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn record_state_change(
    status: &RwLock<NetworkStatus>,
    store: Option<&dyn protocol::ReplicationStore>,
    peer: DeviceId,
    kind: &'static str,
    profile: Option<koru_store_core::ProfileId>,
    workspace: Option<koru_store_core::WorkspaceId>,
    document: Option<koru_store_core::DocumentId>,
    document_name: Option<String>,
    blob_hash: Option<String>,
    detail: Option<String>,
    items: Option<usize>,
) {
    let at_millis = wall_time_millis();
    let sequence = status
        .read()
        .unwrap_or_else(|error| error.into_inner())
        .state_events
        .back()
        .map_or(1, |event| event.sequence.saturating_add(1));
    let event = StateChangeEvent {
        event_id: None,
        sequence,
        at_millis,
        peer,
        kind: kind.into(),
        profile,
        workspace,
        document,
        document_name,
        blob_hash,
        detail,
        items,
    };
    let event = store.map_or(event.clone(), |store| {
        store
            .record_state_change(&event, state_event_cutoff())
            .unwrap_or_else(|error| {
                tracing::warn!(
                    code = error.code,
                    event_kind = kind,
                    "remote state changed but its history row could not be stored"
                );
                event.clone()
            })
    });
    let mut status = status.write().unwrap_or_else(|error| error.into_inner());
    status.last_state_change_millis.insert(peer, at_millis);
    status.state_events.push_back(event);
    while status.state_events.len() > MAX_STATE_EVENTS {
        status.state_events.pop_front();
    }
}

fn state_event_cutoff() -> i64 {
    wall_time_millis().saturating_sub(STATE_EVENT_RETENTION_MILLIS)
}

fn wall_time_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

fn path_kind(connection: &iroh::endpoint::Connection) -> PathKind {
    let paths = connection.paths();
    if paths.iter().any(|path| path.is_ip()) {
        PathKind::Direct
    } else if paths.iter().any(|path| path.is_relay()) {
        PathKind::Relay
    } else {
        PathKind::Unknown
    }
}

pub fn relationship_for_endpoint(
    snapshot: &AuthorizationSnapshot,
    endpoint: ReplicationEndpointId,
) -> Option<(RelationshipId, DeviceId)> {
    snapshot
        .peers
        .iter()
        .find(|peer| peer.endpoint_id == endpoint && !peer.workspaces.is_empty())
        .map(|peer| (peer.relationship_id, peer.device_id))
}

#[cfg(test)]
mod tests;
