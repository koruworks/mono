// SPDX-License-Identifier: MPL-2.0

//! The Bot ingress adapter (M8): where a linked device's Bot stream arrives
//! in this process.
//!
//! The Agent's device network authenticated the peer and admitted only an
//! own device to `koru/bots/1`; it wrote no frame. The gateway routes the
//! session here by its token, **before the Store worker can see it** (the
//! Store worker resets any session that is not replication). This adapter is
//! the second of the three checks of contract 4.1:
//!
//! 1. it reads the stream's first frame, which must be a `client_hello`,
//!    within ten seconds;
//! 2. it asks the admission port ([`koru_store_server::BotPeers`]) whether
//!    the authenticated device is a client of this computer's Bots. A device
//!    the port does not name is answered `refused { not_own_device }`;
//! 3. it hands the stream, the hello, the port's scopes for that device and
//!    a linked-device principal to the owner server, which answers the hello
//!    and serves the stream.
//!
//! **The owner server is the AI worker's** ([`AiOwnerServer`]), installed by
//! the composition when this gateway has an AI worker that serves Bots.
//! Where there is none (the AI worker did not start, or it has no Bot
//! authority) there is nothing to serve: every admitted stream is answered
//! with one clear refusal and closed, the refusal the hello itself earns
//! (`unsupported_version`, `principal_mismatch`, …), and otherwise
//! `bots_unavailable`, "not ready, retry later". Nothing is left open and
//! nothing waits.
//!
//! No frame, device name, hello or scope is logged here.

use std::collections::BTreeSet;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use koru_api_gateway_core::{BOTS_CONFIGURE_SCOPE, GatewayPrincipal};
use koru_bot_client_wire::{
    BUILT_CAPABILITIES, ClientHello, FrameDecoder, Negotiated, Refused, WireMessage, encode,
    limits, refusal, scope,
};
use koru_network_edge::{DevicePrincipal, TransportSession};
use koru_store_server::BotPeers;

/// The engine that serves a linked device's Bot streams on the computer
/// that runs the Bots: in production the AI worker's ([`AiOwnerServer`]).
pub(crate) trait BotOwnerServer: Send + Sync + 'static {
    /// Take over one admitted stream. The server answers the hello (with
    /// `stream.scopes` as the hello's scopes) and owns the stream from here.
    /// It must not block: the caller is the task that admitted the stream.
    fn serve(&self, stream: AdmittedBotStream);
}

/// One Bot stream the admission port let in, before its hello is answered.
/// An owner server is the only thing that is ever handed one.
pub(crate) struct AdmittedBotStream {
    /// The stream's first frame.
    pub(crate) hello: ClientHello,
    /// The device the carrier authenticated. `hello.client_device` must
    /// equal its `device_id`; the owner server checks that.
    pub(crate) device: DevicePrincipal,
    /// The admission port's scopes for that device, to send in the hello
    /// and to hold the stream to.
    pub(crate) scopes: BTreeSet<String>,
    /// The linked device as a gateway caller: exactly the linked-device
    /// scopes those port scopes stand for, and never `bots.manage`. The AI
    /// worker's owner server does not take it: it holds the stream to the
    /// port's own scopes, read again for every message.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) principal: GatewayPrincipal,
    pub(crate) session: TransportSession,
    /// The decoder that read the hello. Any bytes the client sent after it
    /// are still inside.
    pub(crate) decoder: FrameDecoder,
}

impl std::fmt::Debug for AdmittedBotStream {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AdmittedBotStream")
            .field("role", &self.hello.role)
            .field("scopes", &self.scopes.len())
            .finish_non_exhaustive()
    }
}

/// The adapter. Cheap to clone.
#[derive(Clone)]
pub(crate) struct BotIngress {
    inner: Arc<Inner>,
}

struct Inner {
    peers: BotPeers,
    owner: RwLock<Option<Arc<dyn BotOwnerServer>>>,
    hello_timeout: Duration,
}

impl BotIngress {
    pub(crate) fn new(peers: BotPeers) -> Self {
        Self::with_hello_timeout(peers, Duration::from_millis(limits::HELLO_TIMEOUT_MS))
    }

    pub(crate) fn with_hello_timeout(peers: BotPeers, hello_timeout: Duration) -> Self {
        Self {
            inner: Arc::new(Inner {
                peers,
                owner: RwLock::new(None),
                hello_timeout,
            }),
        }
    }

    /// The admission port this adapter reads, for the engine that reads it
    /// again for every message of an admitted stream.
    pub(crate) fn peers(&self) -> BotPeers {
        self.inner.peers.clone()
    }

    /// Install the engine that serves admitted streams. Called once by the
    /// composition, when the AI worker serves Bots; without it every stream
    /// is refused.
    pub(crate) fn set_owner_server(&self, server: Arc<dyn BotOwnerServer>) {
        *self
            .inner
            .owner
            .write()
            .unwrap_or_else(|error| error.into_inner()) = Some(server);
    }

    /// Take one session carrying the Bots token. Returns at once; the
    /// stream is read on its own task. **Called from inside the runtime.**
    pub(crate) fn admit(&self, session: TransportSession) {
        let adapter = self.clone();
        tokio::spawn(async move { adapter.serve(session).await });
    }

    async fn serve(self, mut session: TransportSession) {
        // Only a device is ever a Bot client. A Cloud principal on this
        // token is a stream nobody here owns: reset, as the Store worker
        // resets a token that is not its own.
        let Some(device) = session.principal.device().cloned() else {
            session.reset(1);
            return;
        };
        let mut decoder = FrameDecoder::new();
        let first = tokio::time::timeout(
            self.inner.hello_timeout,
            first_message(&mut session, &mut decoder),
        )
        .await;
        let hello = match first {
            // No hello in time: the stream is closed, and nothing is said.
            Err(_) => {
                session.reset(1);
                return;
            }
            // The client went away before it said anything.
            Ok(First::Closed) => return,
            Ok(First::Message(WireMessage::ClientHello(hello))) => hello,
            // A malformed or oversized frame, or a first message that is not
            // a hello.
            Ok(First::Malformed | First::Message(_)) => {
                refuse(session, Refused::new(refusal::PROTOCOL_ERROR)).await;
                return;
            }
        };
        let peers = self.inner.peers.current();
        let Some(scopes) = peers.client_scopes(&device.device_id).cloned() else {
            refuse(session, Refused::new(refusal::NOT_OWN_DEVICE)).await;
            return;
        };
        let owner = self
            .inner
            .owner
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        match owner {
            Some(owner) => owner.serve(AdmittedBotStream {
                principal: linked_device_principal(&scopes),
                hello,
                device,
                scopes,
                session,
                decoder,
            }),
            None => {
                // Nothing serves Bots here. The hello's own refusal comes
                // first, so a client that needs an update, or that named
                // another device, is told that and not "retry later".
                let supported: Vec<&str> = BUILT_CAPABILITIES
                    .iter()
                    .map(|capability| capability.name)
                    .collect();
                let refusal =
                    match Negotiated::owner_side(&hello, &device.device_id, &scopes, &supported) {
                        Err(refusal) => refusal,
                        Ok(_) => Refused::new(refusal::BOTS_UNAVAILABLE),
                    };
                refuse(session, refusal).await;
            }
        }
    }
}

/// The AI worker's owner server behind this adapter. It is handed the
/// stream as the adapter admitted it: the hello unanswered, the device the
/// carrier authenticated, the port's scopes, the session and the decoder.
pub(crate) struct AiOwnerServer(pub(crate) koru_ai_server::BotOwnerServer);

impl BotOwnerServer for AiOwnerServer {
    fn serve(&self, stream: AdmittedBotStream) {
        let AdmittedBotStream {
            hello,
            device,
            scopes,
            principal: _,
            session,
            decoder,
        } = stream;
        // Returns at once: the stream is served on its own task.
        self.0.serve(koru_ai_server::BotClientStream {
            hello,
            device: device.device_id,
            scopes,
            session,
            decoder,
        });
    }
}

/// The caller a linked device's commands run as: the linked-device scopes
/// the port's scopes stand for. `read` is `bots.read`, `control` is
/// `bots.run`, `configure` is `bots.configure`; a port scope this build does
/// not know grants nothing.
fn linked_device_principal(scopes: &BTreeSet<String>) -> GatewayPrincipal {
    let mut principal = GatewayPrincipal::linked_device("linked-device");
    principal.scopes.retain(|held| {
        let needed = match held.as_str() {
            "bots.read" => scope::READ,
            "bots.run" => scope::CONTROL,
            BOTS_CONFIGURE_SCOPE => scope::CONFIGURE,
            _ => return false,
        };
        scopes.contains(needed)
    });
    principal
}

enum First {
    Message(WireMessage),
    Malformed,
    Closed,
}

/// Read until the stream's first whole frame.
async fn first_message(session: &mut TransportSession, decoder: &mut FrameDecoder) -> First {
    loop {
        match decoder.next() {
            Ok(Some(message)) => return First::Message(message),
            Ok(None) => {}
            Err(_) => return First::Malformed,
        }
        match session.receive.recv().await {
            Ok(Some(bytes)) => {
                if decoder.push(&bytes).is_err() {
                    return First::Malformed;
                }
            }
            Ok(None) | Err(_) => return First::Closed,
        }
    }
}

/// Write one `refused` and end the stream. Dropping the session finishes
/// the send side after the frame, so the client reads the refusal and then
/// the end of the stream.
async fn refuse(session: TransportSession, refusal: Refused) {
    tracing::debug!(reason = %refusal.reason, "a Bot client stream was refused");
    if let Ok(frame) = encode(&WireMessage::Refused(refusal)) {
        let _ = session.send.send(frame).await;
    }
    drop(session);
}

#[cfg(test)]
mod tests;
