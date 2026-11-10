// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_session_protocol::{DelegationLaunch, ErrorCode, SessionFailure};
use koru_session_wire::{FRAME_VERSION, FromHost, HostHello, Request, RequestFrame, ResponseFrame};
use std::io::{Cursor, Read, Write};

#[cfg(feature = "test-support")]
#[test]
fn fixture_preflight_requires_every_guard_and_only_codex() {
    let codex = SessionProfileKey::BuiltIn("codex".parse().unwrap());
    let shell = SessionProfileKey::BuiltIn("platform-shell".parse().unwrap());
    assert!(test_fixture_preflight_guards(
        &codex, true, true, true, true
    ));
    assert!(!test_fixture_preflight_guards(
        &shell, true, true, true, true
    ));
    for missing in 0..4 {
        let mut guards = [true; 4];
        guards[missing] = false;
        assert!(!test_fixture_preflight_guards(
            &codex, guards[0], guards[1], guards[2], guards[3]
        ));
    }
}

#[test]
fn deleted_custom_profile_is_a_stable_not_delegatable_answer() {
    let key = koru_session_protocol::SessionProfileKey::Custom(
        koru_session_protocol::SessionProfileId::mint(),
    );
    assert!(profile_for_readiness(&key).unwrap().is_none());
}

struct Reader {
    frames: mpsc::Receiver<Vec<u8>>,
    current: Cursor<Vec<u8>>,
}
impl Read for Reader {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        loop {
            let count = self.current.read(bytes)?;
            if count != 0 {
                return Ok(count);
            }
            match self.frames.recv() {
                Ok(frame) => self.current = Cursor::new(frame),
                Err(_) => return Ok(0),
            }
        }
    }
}
struct Writer {
    frames: mpsc::Sender<Vec<u8>>,
    pending: Vec<u8>,
    observed: Arc<std::sync::Mutex<Option<DelegationLaunch>>>,
}
impl Write for Writer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.pending.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        let (_, frame): (_, RequestFrame) =
            koru_ipc::frame::read_frame(&mut self.pending.as_slice())?;
        self.pending.clear();
        match frame.request {
            Request::CreateProfileSession { create } => {
                *self.observed.lock().unwrap() = create.delegation
            }
            Request::Disconnect => return Ok(()),
            _ => panic!("unexpected request"),
        }
        let response = FromHost::Response {
            frame: ResponseFrame {
                id: frame.id,
                result: Err(SessionFailure {
                    code: ErrorCode::ProviderModelUnavailable,
                    message: "fixture host received selection".into(),
                }),
            },
        };
        let mut bytes = Vec::new();
        koru_ipc::frame::write_frame(&mut bytes, FRAME_VERSION, &response)?;
        self.frames.send(bytes).unwrap();
        Ok(())
    }
}

/// Exercise the actual worker offer and actual client capability gate. The
/// in-memory host refuses after receiving the frame: no process or account.
#[test]
fn production_worker_offer_allows_selected_model_to_reach_session_host() {
    let offered = offer();
    let agreement = koru_session_protocol::negotiate(&offered, &offered).unwrap();
    let observed = Arc::new(std::sync::Mutex::new(None));
    let (frames, reader) = mpsc::channel();
    let client = SessionClient::from_test_transport(
        HostHello {
            product: "fixture".into(),
            product_version: "fixture".into(),
            instance_id: "fixture".into(),
            supervisor_launch_id: "fixture".into(),
            agreement,
        },
        Reader {
            frames: reader,
            current: Cursor::new(vec![]),
        },
        Writer {
            frames,
            pending: vec![],
            observed: observed.clone(),
        },
    )
    .unwrap();
    let selection = DelegationLaunch {
        model: Some("fixture/deepseek".parse().unwrap()),
        mcp_endpoint: "http://127.0.0.1:1/mcp".parse().unwrap(),
        credential: None,
        tools: Default::default(),
        resume: None,
    };
    let result = client.create_delegated(
        uuid::Uuid::new_v4().to_string().parse().unwrap(),
        koru_session_protocol::SessionProfileKey::BuiltIn("opencode".parse().unwrap()),
        "opencode".parse().unwrap(),
        None,
        selection.clone(),
        std::path::PathBuf::from("C:/fixture"),
        GridSize {
            columns: 80,
            rows: 24,
        },
    );
    assert!(matches!(
        result,
        Err(ClientError::Session(SessionFailure {
            code: ErrorCode::ProviderModelUnavailable,
            ..
        }))
    ));
    assert_eq!(*observed.lock().unwrap(), Some(selection));
}

#[test]
fn bot_creation_offers_interactive_controls_without_enabling_ordinary_questions() {
    assert!(!offer().capabilities.supports(capability::MANAGED_QUESTIONS));
    assert!(!offer().capabilities.supports("conditional-turn-steering"));
    assert!(
        bot_offer()
            .capabilities
            .supports(capability::MANAGED_QUESTIONS)
    );
    assert!(
        bot_offer()
            .capabilities
            .supports("conditional-turn-steering")
    );
    assert!(bot_offer().capabilities.supports(capability::DELEGATION));
    assert!(
        bot_offer()
            .capabilities
            .supports(capability::BEGIN_PENDING_INPUT)
    );
    assert!(
        !offer()
            .capabilities
            .supports(capability::BEGIN_PENDING_INPUT)
    );
}

struct OwnedWriter {
    frames: mpsc::Sender<Vec<u8>>,
    pending: Vec<u8>,
    observed: Arc<std::sync::Mutex<Vec<SessionCommand>>>,
}
impl Write for OwnedWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.pending.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        let (_, frame): (_, RequestFrame) =
            koru_ipc::frame::read_frame(&mut self.pending.as_slice())?;
        self.pending.clear();
        match frame.request {
            Request::Command { command } => self.observed.lock().unwrap().push(command),
            Request::Disconnect => return Ok(()),
            _ => panic!("owned completion must never use a delegated termination or another route"),
        }
        let response = FromHost::Response {
            frame: ResponseFrame {
                id: frame.id,
                result: Err(SessionFailure {
                    code: ErrorCode::StaleLease,
                    message: "fixture takeover".into(),
                }),
            },
        };
        let mut bytes = Vec::new();
        koru_ipc::frame::write_frame(&mut bytes, FRAME_VERSION, &response)?;
        self.frames.send(bytes).unwrap();
        Ok(())
    }
}

#[test]
fn owned_completion_uses_current_lease_and_never_falls_back_after_takeover() {
    let offered = offer();
    let agreement = koru_session_protocol::negotiate(&offered, &offered).unwrap();
    let observed = Arc::new(std::sync::Mutex::new(Vec::new()));
    let (frames, reader) = mpsc::channel();
    let client = SessionClient::from_test_transport(
        HostHello {
            product: "fixture".into(),
            product_version: "fixture".into(),
            instance_id: "fixture".into(),
            supervisor_launch_id: "fixture".into(),
            agreement: agreement.clone(),
        },
        Reader {
            frames: reader,
            current: Cursor::new(vec![]),
        },
        OwnedWriter {
            frames,
            pending: vec![],
            observed: observed.clone(),
        },
    )
    .unwrap();
    let (frames, reader) = mpsc::channel();
    let unary = SessionClient::from_test_transport(
        HostHello {
            product: "fixture".into(),
            product_version: "fixture".into(),
            instance_id: "fixture".into(),
            supervisor_launch_id: "fixture".into(),
            agreement,
        },
        Reader {
            frames: reader,
            current: Cursor::new(vec![]),
        },
        OwnedWriter {
            frames,
            pending: vec![],
            observed: observed.clone(),
        },
    )
    .unwrap();
    let session = SessionId::mint();
    let lease = ControllerLeaseToken::mint();
    let mut sessions = HostSessions {
        endpoint: koru_ipc::Endpoint::for_service("session", "internal-debug").unwrap(),
        unary,
        controllers: HashMap::from([(session, Controller { client, lease })]),
        provider_readiness: vec![],
        provider_subscription: None,
    };
    assert!(sessions.terminate_owned(session).is_err());
    assert!(!sessions.controllers.contains_key(&session));
    let observed = observed.lock().unwrap();
    assert_eq!(observed.len(), 1);
    assert_eq!(observed[0].session_id, session);
    assert_eq!(observed[0].controller_lease, lease);
    assert!(matches!(observed[0].action, RawCommand::Terminate));
}
