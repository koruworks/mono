// SPDX-License-Identifier: MPL-2.0
//! Real AgentSessions discovery against a catalog-only named-pipe peer.
use serde_json::{Value, json};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread::JoinHandle,
    time::Duration,
};
use uuid::Uuid;

pub(crate) struct Catalog {
    pub endpoint: koru_ipc::Endpoint,
    pub reads: Arc<AtomicUsize>,
    #[cfg(test)]
    pub offered: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}
impl Catalog {
    pub fn new() -> Self {
        let endpoint = koru_ipc::Endpoint::for_service_claim(
            koru_ipc::SESSION_HOST,
            "internal-debug",
            std::path::Path::new("unused-routine-catalog"),
            &Uuid::new_v4().to_string(),
        )
        .unwrap();
        let listener = koru_ipc::Listener::bind(endpoint.clone()).unwrap();
        let reads = Arc::new(AtomicUsize::new(0));
        let offered = Arc::new(AtomicBool::new(true));
        let stop = Arc::new(AtomicBool::new(false));
        let (count, available, stopping) = (reads.clone(), offered.clone(), stop.clone());
        let thread = std::thread::spawn(move || {
            while !stopping.load(Ordering::Acquire) {
                let mut stream = listener.accept().unwrap();
                if stopping.load(Ordering::Acquire) {
                    break;
                }
                koru_ipc::set_timeouts(&stream, Some(Duration::from_secs(2)));
                let (version, hello): (_, Value) =
                    koru_ipc::frame::read_frame(&mut stream).unwrap();
                assert_eq!(version, 1);
                assert_eq!(hello["product"], "koru-ai-agent-models");
                assert!(hello["role"].is_null(), "no session-control connection");
                let agreement = koru_session_protocol::ProtocolAgreement {
                    version: koru_session_protocol::MAX_PROTOCOL_VERSION,
                    capabilities: koru_session_protocol::CapabilitySet::new(vec![
                        koru_session_protocol::capability::PROVIDER_MODELS
                            .parse()
                            .unwrap(),
                    ]),
                };
                koru_ipc::frame::write_frame(&mut stream, 1, &json!({"kind":"welcome","hello":{
                    "product":"catalog-fixture","product_version":"fixture","instance_id":"fixture","supervisor_launch_id":"fixture","agreement":agreement
                }})).unwrap();
                let (version, request): (_, Value) =
                    koru_ipc::frame::read_frame(&mut stream).unwrap();
                assert_eq!(version, 1);
                assert_eq!(request["request"]["kind"], "discover_provider_models");
                assert_eq!(request["request"]["provider"], "codex");
                count.fetch_add(1, Ordering::AcqRel);
                let models = koru_session_protocol::ProviderModels::try_from(
                    if available.load(Ordering::Acquire) {
                        vec![koru_session_protocol::ProviderModel::new(
                            "fixture".parse().unwrap(),
                            "fixture".parse().unwrap(),
                        )]
                    } else {
                        vec![]
                    },
                )
                .unwrap();
                koru_ipc::frame::write_frame(&mut stream, 1, &json!({"kind":"response","frame":{
                    "id":request["id"],"result":{"Ok":{"kind":"provider_models","models":models}}
                }})).unwrap();
            }
        });
        Self {
            endpoint,
            reads,
            #[cfg(test)]
            offered,
            stop,
            thread: Some(thread),
        }
    }
}
impl Drop for Catalog {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        // Wake only this fixture's accept; no process, file or real service.
        let _ = koru_ipc::connect_timed(&self.endpoint, Duration::from_secs(2));
        if let Some(thread) = self.thread.take() {
            let joined = thread.join();
            if !std::thread::panicking() {
                joined.unwrap();
            }
        }
    }
}
