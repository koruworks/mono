// SPDX-License-Identifier: MPL-2.0
//! Actual private client/router/worker composition, qualified on Windows IPC.
//! No full GatewayState/bootstrap, actual delegate SQL, provider or disk opens.
use koru_ai_server::{AiWorkerClient, RoutineCallContext, fixtures::RoutineWorkerFixture};
use koru_api_client::routine_bridge::{RoutineBridgeClient, RoutineBridgeTransportError};
use koru_delegate_api::{
    bots::{BotCommand, BotDelegateAuthority, BotDelegatePort, BotHello, BotReply},
    routines::*,
};
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    sync::{Arc, Condvar, Mutex, mpsc::sync_channel},
    thread::JoinHandle,
    time::{Duration, Instant},
};

const PARENT: &str = "PRIVATE-HTTP-WORKER-FIXTURE-PARENT-0123456789";
fn authority() -> RoutineBridgeAuthority {
    BotDelegateAuthority::new(PARENT.into())
        .unwrap()
        .routine_bridge_authority()
}

#[derive(Default)]
struct Gate {
    state: Mutex<(bool, bool, bool)>,
    changed: Condvar,
}
impl Gate {
    fn enter(&self) -> bool {
        let mut state = self.state.lock().unwrap();
        state.0 = true;
        self.changed.notify_all();
        let (state, _) = self
            .changed
            .wait_timeout_while(state, Duration::from_secs(15), |s| !s.1)
            .unwrap();
        state.1
    }
    fn release(&self) {
        self.state.lock().unwrap().1 = true;
        self.changed.notify_all();
    }
    fn complete(&self) {
        self.state.lock().unwrap().2 = true;
        self.changed.notify_all();
    }
    fn wait_complete(&self) {
        let state = self.state.lock().unwrap();
        let (state, _) = self
            .changed
            .wait_timeout_while(state, Duration::from_secs(4), |s| !s.2)
            .unwrap();
        assert!(state.2, "actual acknowledgement did not complete");
    }
}
struct AckHold(Arc<Gate>);
impl AckHold {
    fn entered(&self) {
        let state = self.0.state.lock().unwrap();
        let (state, _) = self
            .0
            .changed
            .wait_timeout_while(state, Duration::from_secs(4), |s| !s.0)
            .unwrap();
        assert!(state.0, "actual acknowledgement phase was not reached");
    }
    fn release(self) {
        self.0.release();
    }
}
impl Drop for AckHold {
    fn drop(&mut self) {
        self.0.release();
    }
}

struct Peer {
    seed: PrepareOccurrence,
    hello: BotHello,
    // Wire values are immediately decoded into the existing strict BotReply.
    // No new DTO or dependency edge is needed just to name neutral leaf types.
    record: Mutex<Option<Value>>,
    calls: Mutex<Vec<&'static str>>,
    ack: Mutex<Option<Arc<Gate>>>,
}
impl Peer {
    fn new(seed: &PrepareOccurrence) -> Self {
        Self {
            seed: seed.clone(),
            hello: BotHello {
                boot: RoutineBridgeEnvelope::new(seed.clone())
                    .unwrap()
                    .request_id
                    .to_string()
                    .parse()
                    .unwrap(),
                entries: vec![],
                next: None,
            },
            record: Mutex::new(None),
            calls: Mutex::new(vec![]),
            ack: Mutex::new(None),
        }
    }
    fn log(&self, call: &'static str) {
        self.calls.lock().unwrap().push(call);
    }
    fn hold_ack(&self) -> AckHold {
        let gate = Arc::new(Gate::default());
        let mut slot = self.ack.lock().unwrap();
        assert!(slot.is_none());
        *slot = Some(gate.clone());
        AckHold(gate)
    }
    fn release(&self) {
        if let Some(gate) = self.ack.lock().unwrap().as_ref() {
            gate.release();
        }
    }
    fn claim(&self, reply: &PrepareOccurrenceReply) -> FinalizeOccurrence {
        let PrepareOccurrenceReply::Prepared { preparation } = reply else {
            panic!("real preparation required")
        };
        assert_eq!(preparation.slot, self.seed.slot);
        let request: FinalizeOccurrence = serde_json::from_value(json!({"claim":{
            "slot":preparation.slot, "definition":preparation.definition, "ids":preparation.ids,
            "preparation_hash":preparation.preparation_hash,
            "claimed_at_ms":preparation.slot.occurrence_ms + 123
        }}))
        .unwrap();
        let record = json!({"claim":request.claim,"submission_version":2,
            "submission":preparation.submission,"link_state":{"state":"held"},
            "updated_at_ms":request.claim.claimed_at_ms});
        let decoded: BotReply = serde_json::from_value(json!({"result":"routine_claim",
            "observation":{"observation":"claimed","record":record}}))
        .unwrap();
        let BotReply::RoutineClaim { observation } = decoded else {
            unreachable!()
        };
        assert!(observation.valid());
        *self.record.lock().unwrap() = Some(record);
        request
    }
}
impl BotDelegatePort for Peer {
    fn execute(&self, _: BotCommand) -> Result<BotReply, koru_delegate_api::Failure> {
        panic!("only finite private owner calls")
    }
    fn execute_within(
        &self,
        command: BotCommand,
        within: Duration,
    ) -> Result<BotReply, koru_delegate_api::Failure> {
        assert!(within <= Duration::from_secs(10));
        match command {
            BotCommand::Hello { owner, .. } => {
                assert_eq!(owner, self.seed.slot.owner);
                self.log("hello");
                Ok(BotReply::Hello {
                    hello: self.hello.clone(),
                })
            }
            BotCommand::PendingNativeEffects { .. } => Err(koru_delegate_api::Failure::new(
                "unavailable",
                "no fixture native effects",
                true,
            )),
            BotCommand::ReadRoutineClaim { slot } => {
                assert!(within <= ROUTINE_BRIDGE_DEADLINE);
                assert_eq!(slot, self.seed.slot);
                self.log("read");
                let observation = self.record.lock().unwrap().as_ref().map_or_else(
                    || json!({"observation":"absent","slot":slot,"observed_at_ms":slot.occurrence_ms+1}),
                    |record| json!({"observation":"claimed","record":record}),
                );
                Ok(serde_json::from_value(
                    json!({"result":"routine_claim","observation":observation}),
                )
                .unwrap())
            }
            BotCommand::AcknowledgeRoutineLink { acknowledgement } => {
                assert!(within <= ROUTINE_BRIDGE_DEADLINE);
                assert_eq!(acknowledgement.claim.slot, self.seed.slot);
                self.log("ack");
                let gate = self.ack.lock().unwrap().clone();
                // Never hold record/log/gate-slot mutexes while the physical peer waits.
                if let Some(gate) = &gate {
                    if !gate.enter() {
                        return Err(koru_delegate_api::Failure::new(
                            "unavailable",
                            "fixture ack bound",
                            true,
                        ));
                    }
                }
                let mut records = self.record.lock().unwrap();
                let record = records.as_mut().unwrap();
                assert_eq!(record["claim"], json!(acknowledgement.claim));
                assert!(
                    record["link_state"] == json!({"state":"held"})
                        || record["link_state"] == json!(acknowledgement.outcome)
                );
                record["link_state"] = json!(acknowledgement.outcome);
                let reply = serde_json::from_value(
                    json!({"result":"routine_link_acknowledged","record":record}),
                )
                .unwrap();
                drop(records);
                if let Some(gate) = gate {
                    self.ack.lock().unwrap().take();
                    gate.complete();
                }
                Ok(reply)
            }
            BotCommand::Reconcile { key } => {
                self.log("reconcile");
                assert_eq!(key.owner, self.seed.slot.owner);
                assert_eq!(key.bot, self.seed.slot.bot);
                let record = self.record.lock().unwrap();
                let record = record.as_ref().unwrap();
                assert_eq!(json!(key.request), record["claim"]["ids"]["submission"]);
                Ok(
                    serde_json::from_value(json!({"result":"admission","admission":{
                        "key":key,"run":record["claim"]["ids"]["run"],"state":"queued",
                        "queue_sequence":1,"fence":null
                    }}))
                    .unwrap(),
                )
            }
            other => panic!("unexpected execution/provider/session command: {other:?}"),
        }
    }
}

struct Server {
    addr: SocketAddr,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    join: Option<JoinHandle<()>>,
}
impl Server {
    fn new(worker: Option<AiWorkerClient>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let router = koru_api_gateway_core::fixtures::routine_http_router(authority(), worker);
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let (ready, receive) = sync_channel(1);
        let join = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async move {
                let listener = tokio::net::TcpListener::from_std(listener).unwrap();
                ready.send(()).unwrap();
                axum::serve(listener, router)
                    .with_graceful_shutdown(async {
                        let _ = stopped.await;
                    })
                    .await
                    .unwrap();
            });
        });
        receive.recv_timeout(Duration::from_secs(3)).unwrap();
        Self {
            addr,
            stop: Some(stop),
            join: Some(join),
        }
    }
    fn client(&self) -> RoutineBridgeClient {
        koru_api_client::Client::from_gateway(koru_api_discovery::GatewayHello {
            instance_id: "routine-network-fixture".into(),
            supervisor_launch_id: None,
            gateway_version: "fixture".into(),
            segment: "internal-debug".into(),
            api_version: 1,
            api_endpoint: format!("http://{}", self.addr),
            capabilities: vec![],
            session: koru_api_discovery::SessionCredential::new(
                "ORDINARY-BEARER-MUST-NOT-BE-USED".into(),
                0,
            ),
        })
        .unwrap()
        .routine_bridge(authority())
        .unwrap()
    }
    fn stop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
    fn join(&mut self) {
        if let Some(join) = self.join.take() {
            join.join().unwrap();
        }
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop();
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

struct Harness {
    fixture: Option<RoutineWorkerFixture>,
    peer: Arc<Peer>,
    server: Server,
    seed: PrepareOccurrence,
}
impl Harness {
    fn new() -> Self {
        let mut peer = None;
        let fixture = RoutineWorkerFixture::spawn(|request| {
            let p = Arc::new(Peer::new(request));
            peer = Some(p.clone());
            p
        })
        .unwrap();
        let seed = fixture.preparation();
        let server = Server::new(Some(fixture.client()));
        let h = Self {
            fixture: Some(fixture),
            peer: peer.unwrap(),
            server,
            seed,
        };
        h.ready();
        h
    }
    fn fixture(&self) -> &RoutineWorkerFixture {
        self.fixture.as_ref().unwrap()
    }
    fn read_absent(&self) -> RoutineBridgeEnvelope<ReadPreparation> {
        // Before a draft exists any valid caller hash can positively observe
        // absence for this exact slot. Prepared reads use its actual owner hash.
        RoutineBridgeEnvelope::new(
            serde_json::from_value(json!({"slot":self.seed.slot,"semantic_hash":"11".repeat(32)}))
                .unwrap(),
        )
        .unwrap()
    }
    fn ready(&self) {
        let client = self.server.client();
        let end = Instant::now() + Duration::from_secs(4);
        loop {
            let reply = client.read_preparation(&self.read_absent()).unwrap();
            if matches!(reply.payload, ReadPreparationReply::Missing { .. }) {
                return;
            }
            assert!(matches!(
                reply.payload,
                ReadPreparationReply::Refused {
                    code: RoutineBridgeRefusal::Unavailable
                }
            ));
            retry_wait(end);
        }
    }
    fn prepare(&self) -> RoutineBridgeEnvelope<PrepareOccurrenceReply> {
        let client = self.server.client();
        let end = Instant::now() + Duration::from_secs(4);
        loop {
            let request = RoutineBridgeEnvelope::new(self.seed.clone()).unwrap();
            let reply = client.prepare(&request).unwrap();
            reply.validate_reply_to(&request).unwrap();
            if matches!(reply.payload, PrepareOccurrenceReply::Prepared { .. }) {
                return reply;
            }
            assert!(matches!(
                reply.payload,
                PrepareOccurrenceReply::Refused {
                    code: RoutineBridgeRefusal::Capacity | RoutineBridgeRefusal::Unavailable
                }
            ));
            retry_wait(end);
        }
    }
    fn finish(mut self) -> koru_ai_server::fixtures::Repository {
        self.server.stop();
        self.peer.release();
        let repository = self.fixture.take().unwrap().finish().unwrap();
        self.server.join();
        repository
    }
}
impl Drop for Harness {
    fn drop(&mut self) {
        self.server.stop();
        self.peer.release();
        if let Some(fixture) = self.fixture.take() {
            let _ = fixture.finish();
        }
        if let Some(join) = self.server.join.take() {
            let _ = join.join();
        }
    }
}

fn retry_wait(end: Instant) {
    let remaining = end.saturating_duration_since(Instant::now());
    assert!(
        !remaining.is_zero(),
        "bounded owner observation did not settle"
    );
    let lock = Mutex::new(());
    let changed = Condvar::new();
    let (_guard, _elapsed) = changed
        .wait_timeout(
            lock.lock().unwrap(),
            remaining.min(Duration::from_millis(20)),
        )
        .unwrap();
}
fn context(seed: &PrepareOccurrence) -> RoutineCallContext {
    RoutineCallContext::new(
        RoutineBridgeEnvelope::new(seed.clone()).unwrap().request_id,
        Instant::now() + ROUTINE_BRIDGE_DEADLINE,
        Default::default(),
    )
}

struct Task<T>(Option<JoinHandle<T>>);
impl<T: Send + 'static> Task<T> {
    fn start(run: impl FnOnce() -> T + Send + 'static) -> Self {
        Self(Some(std::thread::spawn(run)))
    }
    fn join(mut self) -> T {
        self.0.take().unwrap().join().unwrap()
    }
}
impl<T> Drop for Task<T> {
    fn drop(&mut self) {
        if let Some(join) = self.0.take() {
            let _ = join.join();
        }
    }
}

fn raw(
    addr: SocketAddr,
    path: &str,
    authorization: &str,
    declared: usize,
    body: &[u8],
) -> (u16, Vec<u8>) {
    let mut stream = TcpStream::connect(addr).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(8)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    write!(stream,"POST {path} HTTP/1.1\r\nHost: {addr}\r\nAuthorization: {authorization}\r\nContent-Type: application/json\r\nContent-Length: {declared}\r\nConnection: close\r\n\r\n").unwrap();
    stream.write_all(body).unwrap();
    let mut response = Vec::new();
    stream
        .take((ROUTINE_BRIDGE_BODY_LIMIT + 32769) as u64)
        .read_to_end(&mut response)
        .unwrap();
    assert!(response.len() < ROUTINE_BRIDGE_BODY_LIMIT + 32769);
    let split = response.windows(4).position(|x| x == b"\r\n\r\n").unwrap() + 4;
    let headers = std::str::from_utf8(&response[..split]).unwrap();
    let status = headers
        .lines()
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    (status, response[split..].to_vec())
}

#[cfg(test)]
mod tests;
