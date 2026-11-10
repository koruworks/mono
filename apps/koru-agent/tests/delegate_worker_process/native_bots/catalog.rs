// SPDX-License-Identifier: MPL-2.0
//! Discovery-only session-wire fixture; execution always uses the real owner.
use super::*;
use koru_session_protocol::{CapabilitySet, MAX_PROTOCOL_VERSION, ProtocolOffer, ProtocolRange};
use koru_session_wire::{
    ClientHello, FromHost, Greeting, HostHello, Request, RequestFrame, Response, ResponseFrame,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Default)]
pub(super) struct Catalog {
    discoveries: AtomicUsize,
    forbidden: AtomicUsize,
}
impl Catalog {
    pub(super) fn diagnose(&self) {
        eprintln!(
            "catalog fixture counters: discovery={} forbidden={}",
            self.discoveries.load(Ordering::SeqCst),
            self.forbidden.load(Ordering::SeqCst)
        );
    }
    pub(super) async fn start(
        root: &Path,
    ) -> (koru_ipc::Endpoint, Arc<Self>, tokio::task::JoinHandle<()>) {
        let endpoint = koru_ipc::Endpoint::for_service_channel_root(
            "native-bot-catalog-fixture",
            segment(),
            root,
        )
        .unwrap();
        let listener = koru_ipc::AsyncListener::bind(endpoint.clone()).unwrap();
        let state = Arc::new(Self::default());
        let served = state.clone();
        let task = tokio::spawn(async move {
            let mut connections = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept(), if connections.len() < 4 => {
                        let Ok(stream) = accepted else { break };
                        let state = served.clone();
                        connections.spawn(async move { let _ = state.connection(stream).await; });
                    }
                    _ = connections.join_next(), if !connections.is_empty() => {}
                }
            }
        });
        (endpoint, state, task)
    }
    pub(super) fn verify(&self) {
        assert!(
            self.discoveries.load(Ordering::SeqCst) > 0,
            "catalog fixture was actually queried"
        );
        assert_eq!(
            self.forbidden.load(Ordering::SeqCst),
            0,
            "no execution or control reached catalog fixture"
        );
        eprintln!("catalog fixture verified: discovery only; execution uses real sessions owner");
    }
    async fn connection(&self, mut stream: koru_ipc::AsyncStream) -> std::io::Result<()> {
        let hello: ClientHello = read(&mut stream).await?;
        if hello.role.is_some() || hello.product != "koru-ai-agent-models" {
            self.forbidden.fetch_add(1, Ordering::SeqCst);
            return Err(std::io::ErrorKind::PermissionDenied.into());
        }
        let offer = ProtocolOffer {
            versions: ProtocolRange {
                min: MAX_PROTOCOL_VERSION,
                max: MAX_PROTOCOL_VERSION,
            },
            capabilities: CapabilitySet::new([koru_session_protocol::capability::PROVIDER_MODELS
                .parse()
                .unwrap()]),
        };
        let agreement = koru_session_protocol::negotiate(&hello.offer, &offer).unwrap();
        write(
            &mut stream,
            &Greeting::Welcome {
                hello: HostHello {
                    product: "native-bot-catalog-fixture".into(),
                    product_version: "fixture".into(),
                    instance_id: "fixture".into(),
                    supervisor_launch_id: "fixture".into(),
                    agreement,
                },
            },
        )
        .await?;
        loop {
            let frame: RequestFrame = read(&mut stream).await?;
            let disconnect = matches!(frame.request, Request::Disconnect);
            let result = match frame.request {
                Request::DiscoverProviderModels { provider, .. }
                    if provider.as_str() == "codex" =>
                {
                    self.discoveries.fetch_add(1, Ordering::SeqCst);
                    Ok(Response::ProviderModels {
                        models: vec![koru_session_protocol::ProviderModel::new(
                            "fixture-model".parse().unwrap(),
                            "fixture-model".parse().unwrap(),
                        )]
                        .try_into()
                        .unwrap(),
                        modes: None,
                    })
                }
                Request::Disconnect => Ok(Response::Ok),
                _ => {
                    self.forbidden.fetch_add(1, Ordering::SeqCst);
                    Err(koru_session_protocol::SessionFailure {
                        code: koru_session_protocol::ErrorCode::CapabilityNotNegotiated,
                        message: "catalog fixture accepts discovery only".into(),
                    })
                }
            };
            write(
                &mut stream,
                &FromHost::Response {
                    frame: ResponseFrame {
                        id: frame.id,
                        result,
                    },
                },
            )
            .await?;
            if disconnect {
                return Ok(());
            }
        }
    }
}
async fn read<T: for<'de> serde::Deserialize<'de>>(
    stream: &mut koru_ipc::AsyncStream,
) -> std::io::Result<T> {
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut bytes = [0; koru_ipc::frame::HEADER_LEN];
        stream.read_exact(&mut bytes).await?;
        let header = koru_ipc::frame::parse_header(&bytes)?;
        if header.version != koru_session_wire::FRAME_VERSION || header.body_len > 64 * 1024 {
            return Err(std::io::ErrorKind::InvalidData.into());
        }
        let mut body = vec![0; header.body_len];
        stream.read_exact(&mut body).await?;
        koru_ipc::frame::decode(&body)
    })
    .await
    .map_err(|_| std::io::ErrorKind::TimedOut)?
}
async fn write<T: serde::Serialize>(
    stream: &mut koru_ipc::AsyncStream,
    value: &T,
) -> std::io::Result<()> {
    let bytes = koru_ipc::frame::encode(koru_session_wire::FRAME_VERSION, value)?;
    tokio::time::timeout(Duration::from_secs(10), stream.write_all(&bytes))
        .await
        .map_err(|_| std::io::ErrorKind::TimedOut)??;
    Ok(())
}
