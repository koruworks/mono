// SPDX-License-Identifier: MPL-2.0

//! Desktop-only discovery and authentication rendezvous for `koru-api-gateway`.
//! It cannot launch a process and contains no Store or AI business type.

use std::fmt;
use std::time::Duration;

use koru_ipc::Endpoint;
use serde::{Deserialize, Serialize};

pub const HANDSHAKE_VERSION: u8 = 1;
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientHello {
    pub product: String,
    pub client_version: String,
    pub segment: String,
    pub api_version: u32,
    #[serde(default)]
    pub requested_scopes: Vec<String>,
    /// **What the supervisor says about this caller**, when there is anything
    /// to say. `product` above is a free-form string a caller chose for itself;
    /// this is a short-lived token the Agent minted for a process it saw
    /// running the `koru` beside it, and the gateway spends it against the
    /// Agent before believing it.
    ///
    /// Absent from every ordinary caller and from every hello written before
    /// SP-8a. Absent means "not vouched for", which costs nothing except the
    /// one scope that requires it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grant: Option<koru_agent_wire::FirstPartyGrant>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionCredential {
    bearer: String,
    pub expires_at_millis: i64,
}

impl SessionCredential {
    pub fn new(bearer: String, expires_at_millis: i64) -> Self {
        Self {
            bearer,
            expires_at_millis,
        }
    }

    pub fn expose(&self) -> &str {
        &self.bearer
    }
}

impl fmt::Debug for SessionCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SessionCredential")
            .field("bearer", &"[REDACTED]")
            .field("expires_at_millis", &self.expires_at_millis)
            .finish()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatewayHello {
    pub instance_id: String,
    #[serde(default)]
    pub supervisor_launch_id: Option<String>,
    pub gateway_version: String,
    pub segment: String,
    pub api_version: u32,
    pub api_endpoint: String,
    pub capabilities: Vec<String>,
    pub session: SessionCredential,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GatewayResponse {
    Welcome(Box<GatewayHello>),
    Refused {
        what: String,
        ours: u32,
        theirs: u32,
        message: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiscoveryError {
    NotRunning(String),
    Ipc(String),
    Handshake(String),
    Incompatible {
        what: String,
        ours: u32,
        theirs: u32,
        message: String,
    },
}

impl fmt::Display for DiscoveryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotRunning(value) => write!(formatter, "API gateway is not running: {value}"),
            Self::Ipc(value) | Self::Handshake(value) => formatter.write_str(value),
            Self::Incompatible {
                what,
                ours,
                theirs,
                message,
            } => write!(
                formatter,
                "incompatible {what}: client {ours}, gateway {theirs}: {message}"
            ),
        }
    }
}

impl std::error::Error for DiscoveryError {}

pub fn gateway_endpoint(segment: &str) -> Result<Endpoint, DiscoveryError> {
    Endpoint::for_service(koru_ipc::API_GATEWAY, segment)
        .map_err(|error| DiscoveryError::Ipc(error.to_string()))
}

/// Why a gateway that should self-start may keep answering "not running":
/// the Store worker cannot claim the channel's data because an **older
/// installation's process still holds it** — macOS happily lets a running
/// app's bundle be replaced on disk, and its orphaned store keeps serving
/// from nowhere. Best effort, one `lsof`, for a fatal dialog's second
/// sentence: `None` means nothing could be named and the message should say
/// nothing. The caller supplies the channel's store root because this crate
/// deliberately does not know where roots live.
pub fn diagnose_gateway_absence(segment: &str, store_root: &std::path::Path) -> Option<String> {
    let claim = Endpoint::for_service_root(koru_ipc::STORE, segment, store_root).ok()?;
    let holder = koru_ipc::diagnose::holder_of(claim.name())?;
    Some(format!(
        "This channel's data is held by {holder} — usually an older Koru still \
         running after its app was replaced. Quit that process and open Koru again."
    ))
}

pub fn gateway_endpoint_at(
    segment: &str,
    root: &std::path::Path,
) -> Result<Endpoint, DiscoveryError> {
    Endpoint::for_service_channel_root(koru_ipc::API_GATEWAY, segment, root)
        .map_err(|error| DiscoveryError::Ipc(error.to_string()))
}

pub fn connect(segment: &str, hello: &ClientHello) -> Result<GatewayHello, DiscoveryError> {
    connect_to(&gateway_endpoint(segment)?, hello)
}

pub fn connect_at(
    segment: &str,
    root: &std::path::Path,
    hello: &ClientHello,
) -> Result<GatewayHello, DiscoveryError> {
    connect_to(&gateway_endpoint_at(segment, root)?, hello)
}

/// Test only whether a root-qualified gateway rendezvous is accepting local
/// connections. No hello is sent, so the gateway cannot mint a credential for
/// a liveness poll that will immediately be discarded.
pub fn probe_at(segment: &str, root: &std::path::Path) -> Result<(), DiscoveryError> {
    let endpoint = gateway_endpoint_at(segment, root)?;
    koru_ipc::connect_timed(&endpoint, HANDSHAKE_TIMEOUT)
        .map(drop)
        .map_err(|error| DiscoveryError::NotRunning(error.to_string()))
}

/// The mechanism-level checks shared by every desktop gateway host. Session
/// minting and welcome capabilities remain policy-owned.
pub fn admission_refusal(
    hello: &ClientHello,
    segment: &str,
    api_version: u32,
) -> Option<GatewayResponse> {
    if hello.segment != segment {
        return Some(GatewayResponse::Refused {
            what: "channel".into(),
            ours: api_version,
            theirs: hello.api_version,
            message: "the client and gateway belong to different release channels".into(),
        });
    }
    if hello.api_version != api_version {
        return Some(GatewayResponse::Refused {
            what: "api".into(),
            ours: api_version,
            theirs: hello.api_version,
            message: "the client and gateway API versions differ".into(),
        });
    }
    None
}

pub fn connect_to(
    endpoint: &Endpoint,
    hello: &ClientHello,
) -> Result<GatewayHello, DiscoveryError> {
    let mut stream = koru_ipc::connect_timed(endpoint, HANDSHAKE_TIMEOUT)
        .map_err(|error| DiscoveryError::NotRunning(error.to_string()))?;
    koru_ipc::frame::write_frame(&mut stream, HANDSHAKE_VERSION, hello)
        .map_err(|error| DiscoveryError::Handshake(error.to_string()))?;
    let (version, response): (u8, GatewayResponse) = koru_ipc::frame::read_frame(&mut stream)
        .map_err(|error| DiscoveryError::Handshake(error.to_string()))?;
    if version != HANDSHAKE_VERSION {
        return Err(DiscoveryError::Incompatible {
            what: "handshake".into(),
            ours: u32::from(HANDSHAKE_VERSION),
            theirs: u32::from(version),
            message: "unsupported gateway handshake".into(),
        });
    }
    match response {
        GatewayResponse::Welcome(hello) => Ok(*hello),
        GatewayResponse::Refused {
            what,
            ours: theirs,
            theirs: _,
            message,
        } => Err(DiscoveryError::Incompatible {
            what,
            ours: hello.api_version,
            theirs,
            message,
        }),
    }
}

#[cfg(feature = "server")]
pub mod server {
    use super::*;
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    pub trait Policy: Send + Sync + 'static {
        fn greet(&self, hello: &ClientHello) -> GatewayResponse;
    }

    impl<F> Policy for F
    where
        F: Fn(&ClientHello) -> GatewayResponse + Send + Sync + 'static,
    {
        fn greet(&self, hello: &ClientHello) -> GatewayResponse {
            self(hello)
        }
    }

    #[derive(Debug)]
    pub struct Rendezvous {
        listener: koru_ipc::AsyncListener,
    }

    impl Rendezvous {
        pub fn bind_at(segment: &str, root: &std::path::Path) -> Result<Self, DiscoveryError> {
            let endpoint = gateway_endpoint_at(segment, root)?;
            koru_ipc::AsyncListener::bind(endpoint)
                .map(|listener| Self { listener })
                .map_err(|error| DiscoveryError::Ipc(error.to_string()))
        }

        pub async fn serve<P: Policy>(self, policy: Arc<P>) {
            loop {
                let Ok(mut stream) = self.listener.accept().await else {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                };
                let policy = Arc::clone(&policy);
                tokio::spawn(async move {
                    let _ = tokio::time::timeout(HANDSHAKE_TIMEOUT, async {
                        if !koru_ipc::same_user(&stream) {
                            return;
                        }
                        let mut header = [0u8; koru_ipc::frame::HEADER_LEN];
                        if stream.read_exact(&mut header).await.is_err() {
                            return;
                        }
                        let Ok(header) = koru_ipc::frame::parse_header(&header) else {
                            return;
                        };
                        let response = if header.version != HANDSHAKE_VERSION {
                            GatewayResponse::Refused {
                                what: "handshake".into(),
                                ours: u32::from(HANDSHAKE_VERSION),
                                theirs: u32::from(header.version),
                                message: "unsupported gateway handshake".into(),
                            }
                        } else {
                            let mut body = vec![0; header.body_len];
                            if stream.read_exact(&mut body).await.is_err() {
                                return;
                            }
                            let Ok(hello) = koru_ipc::frame::decode::<ClientHello>(&body) else {
                                return;
                            };
                            policy.greet(&hello)
                        };
                        let Ok(bytes) = koru_ipc::frame::encode(HANDSHAKE_VERSION, &response)
                        else {
                            return;
                        };
                        let _ = stream.write_all(&bytes).await;
                        let _ = stream.flush().await;
                    })
                    .await;
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_are_redacted() {
        let credential = SessionCredential::new("secret".into(), 42);
        assert!(!format!("{credential:?}").contains("secret"));
        assert_eq!(credential.expose(), "secret");
    }

    #[test]
    fn gateway_and_store_service_tokens_are_distinct() {
        assert_ne!(koru_ipc::API_GATEWAY, koru_ipc::STORE);
        assert_ne!(koru_ipc::API_GATEWAY, koru_ipc::AGENT);
    }

    #[test]
    fn incompatibility_display_keeps_the_server_explanation() {
        let error = DiscoveryError::Incompatible {
            what: "api".into(),
            ours: 1,
            theirs: 2,
            message: "upgrade the desktop app".into(),
        };
        assert!(error.to_string().contains("upgrade the desktop app"));
    }

    #[test]
    fn common_admission_refuses_channel_and_api_drift_only() {
        let hello = ClientHello {
            product: "test".into(),
            client_version: "1".into(),
            segment: "nightly".into(),
            api_version: 7,
            requested_scopes: Vec::new(),
            grant: None,
        };
        assert!(matches!(
            admission_refusal(&hello, "stable", 7),
            Some(GatewayResponse::Refused { ref what, .. }) if what == "channel"
        ));
        assert!(matches!(
            admission_refusal(&hello, "nightly", 8),
            Some(GatewayResponse::Refused { ref what, .. }) if what == "api"
        ));
        assert_eq!(admission_refusal(&hello, "nightly", 7), None);
    }

    #[cfg(feature = "server")]
    #[tokio::test]
    async fn a_probe_does_not_greet() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let root = tempfile::tempdir().expect("temp root");
        let segment = "internal-debug".to_owned();
        let rendezvous = server::Rendezvous::bind_at(&segment, root.path()).expect("bind");
        let greetings = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&greetings);
        let server = tokio::spawn(rendezvous.serve(Arc::new(move |hello: &ClientHello| {
            counted.fetch_add(1, Ordering::SeqCst);
            GatewayResponse::Welcome(Box::new(GatewayHello {
                instance_id: "test".into(),
                supervisor_launch_id: None,
                gateway_version: "1".into(),
                segment: hello.segment.clone(),
                api_version: hello.api_version,
                api_endpoint: "http://127.0.0.1:1".into(),
                capabilities: Vec::new(),
                session: SessionCredential::new("token".into(), i64::MAX),
            }))
        })));

        let probe_root = root.path().to_path_buf();
        let probe_segment = segment.clone();
        tokio::task::spawn_blocking(move || probe_at(&probe_segment, &probe_root))
            .await
            .expect("join")
            .expect("probe");
        tokio::time::sleep(Duration::from_millis(25)).await;
        assert_eq!(greetings.load(Ordering::SeqCst), 0);

        let endpoint = gateway_endpoint_at(&segment, root.path()).expect("endpoint");
        let hello = ClientHello {
            product: "real".into(),
            client_version: "1".into(),
            segment: segment.clone(),
            api_version: 1,
            requested_scopes: Vec::new(),
            grant: None,
        };
        tokio::task::spawn_blocking(move || connect_to(&endpoint, &hello))
            .await
            .expect("join")
            .expect("greet");
        assert_eq!(greetings.load(Ordering::SeqCst), 1);

        server.abort();
    }

    #[cfg(feature = "server")]
    #[tokio::test]
    async fn a_half_open_connection_does_not_block_a_real_greet_forever() {
        use std::sync::Arc;

        let root = tempfile::tempdir().expect("temp root");
        let segment = "internal-debug".to_owned();
        let rendezvous = server::Rendezvous::bind_at(&segment, root.path()).expect("bind");
        let server = tokio::spawn(rendezvous.serve(Arc::new(|hello: &ClientHello| {
            GatewayResponse::Welcome(Box::new(GatewayHello {
                instance_id: "test".into(),
                supervisor_launch_id: None,
                gateway_version: "1".into(),
                segment: hello.segment.clone(),
                api_version: hello.api_version,
                api_endpoint: "http://127.0.0.1:1".into(),
                capabilities: Vec::new(),
                session: SessionCredential::new("token".into(), i64::MAX),
            }))
        })));

        let endpoint = gateway_endpoint_at(&segment, root.path()).expect("endpoint");
        let half_open = tokio::task::spawn_blocking(move || {
            koru_ipc::connect_timed(&endpoint, HANDSHAKE_TIMEOUT)
        })
        .await
        .expect("join")
        .expect("half open");
        let endpoint = gateway_endpoint_at(&segment, root.path()).expect("endpoint");
        let hello = ClientHello {
            product: "real".into(),
            client_version: "1".into(),
            segment,
            api_version: 1,
            requested_scopes: Vec::new(),
            grant: None,
        };
        let greet = tokio::task::spawn_blocking(move || connect_to(&endpoint, &hello));
        let answer = tokio::time::timeout(HANDSHAKE_TIMEOUT + Duration::from_secs(2), greet).await;
        drop(half_open);
        let answer = answer.expect("the half-open handshake timeout must release the listener");
        answer.expect("join").expect("greet");

        server.abort();
    }
}
