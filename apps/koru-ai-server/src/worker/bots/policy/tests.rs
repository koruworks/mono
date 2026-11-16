// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_ai_core::{DocumentId, ModelSelection, ProfileId};

pub(super) fn bot() -> Bot {
    let workspace = WorkspaceId::new();
    Bot {
        id: BotId::from_uuid("00000000-0000-4000-8000-000000000003".parse().unwrap()),
        user_scope: UserScopeId::from_uuid("00000000-0000-4000-8000-000000000001".parse().unwrap()),
        owner_device: DeviceId::from_uuid("00000000-0000-4000-8000-000000000002".parse().unwrap()),
        home_workspace: workspace,
        config: BotConfig {
            name: "fixture".into(),
            role: BotPage {
                workspace,
                document: DocumentId::new(),
            },
            working_folder: if cfg!(windows) {
                "D:/isolated"
            } else {
                "/isolated"
            }
            .into(),
            model: ModelSelection {
                provider: AiProviderId::parse("agent-codex").unwrap(),
                requested_model: "fixture".into(),
                default_reasoning: ReasoningChoice::Default,
            },
            mode: None,
            fast: None,
            avatar: None,
        },
        access: BotAccess::Default,
        selected_pages: vec![],
        revision: 7,
        policy_revision: 1,
        main_chat: ChatId::new(),
        paused: false,
        folder_lock_request: None,
        folder_frozen_at_ms: None,
        created_at_ms: 1,
        updated_at_ms: 1,
        lifecycle: Default::default(),
        open_operation: None,
        held_by: None,
        pending_change: None,
    }
}

#[test]
fn epoch_correlation_has_fixed_encoding_and_does_not_grant_authority() {
    let mut b = bot();
    assert_eq!(
        epoch_key(&b).unwrap().to_storage(),
        "702efbc8-a8b3-8a7f-b84a-8bdd51ac4385"
    );
    let first = epoch_key(&b).unwrap();
    b.config.name = "another title".into();
    b.paused = true;
    assert_eq!(epoch_key(&b).unwrap(), first);
    assert!(
        barrier(&b).unwrap().effects_allowed,
        "execution pause does not revoke the active task's policy"
    );
    b.revision += 1;
    assert_ne!(epoch_key(&b).unwrap(), first);
    b.revision = 0;
    assert!(epoch_key(&b).is_err());
}

struct Authority {
    owner: BotOwner,
    workspace: WorkspaceId,
    allowed: bool,
}
impl BotAuthority for Authority {
    fn owner(&self) -> Result<BotOwner, BotError> {
        Ok(self.owner)
    }
    fn workspace(&self, w: WorkspaceId) -> Result<ProfileId, BotError> {
        if self.allowed && self.workspace == w {
            Ok(ProfileId::new())
        } else {
            Err(BotError::Forbidden)
        }
    }
    fn page(&self, p: &BotPage) -> Result<(), BotError> {
        self.workspace(p.workspace).map(|_| ())
    }
}
struct Delegate {
    calls: std::sync::atomic::AtomicUsize,
    effective: bool,
}
impl BotDelegatePort for Delegate {
    fn execute(&self, command: DelegateCommand) -> Result<BotReply, koru_delegate_api::Failure> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let DelegateCommand::SynchronizePolicy { barrier } = command else {
            panic!("unexpected command")
        };
        Ok(BotReply::PolicyBarrier {
            receipt: koru_delegate_core::BotPolicyBarrierReceipt {
                barrier,
                state: if self.effective {
                    BotPolicyBarrierState::Effective
                } else {
                    BotPolicyBarrierState::Pending
                },
            },
        })
    }
}

#[test]
fn fresh_store_refusal_precedes_delegate_and_pending_is_not_effective() {
    let b = bot();
    let delegate = Arc::new(Delegate {
        calls: std::sync::atomic::AtomicUsize::new(0),
        effective: false,
    });
    let receipt = BotAccessReceipt {
        request: BotRequestId::mint(),
        fingerprint: "a".repeat(64),
        bot: b.clone(),
        barrier: Some(barrier(&b).unwrap()),
        completed: false,
    };
    let make = |allowed| BotPolicyJob {
        bot: b.clone(),
        work: PolicyWork::Coordinate(receipt.clone()),
        authority: Arc::new(Authority {
            owner: owner(&b),
            workspace: b.home_workspace,
            allowed,
        }),
        delegate: Some(delegate.clone()),
    };
    assert!(make(false).run().result.is_err());
    assert_eq!(delegate.calls.load(Ordering::SeqCst), 0);
    let observed = make(true).run().result.unwrap().1.unwrap();
    assert_eq!(observed.state, BotPolicyBarrierState::Pending);
    assert!(!receipt.completed);
    assert_eq!(delegate.calls.load(Ordering::SeqCst), 1);
}

struct TransientDelegate(std::sync::atomic::AtomicUsize);
impl BotDelegatePort for TransientDelegate {
    fn execute(&self, command: DelegateCommand) -> Result<BotReply, koru_delegate_api::Failure> {
        let DelegateCommand::SynchronizePolicy { barrier } = command else {
            panic!("unexpected command")
        };
        if self.0.fetch_add(1, Ordering::SeqCst) == 0 {
            // A malformed/lost response is uncertain, not an Effective barrier.
            return Ok(BotReply::RunAuthority {
                authority: koru_delegate_core::RunToolAuthority::Ordinary {},
            });
        }
        Ok(BotReply::PolicyBarrier {
            receipt: koru_delegate_core::BotPolicyBarrierReceipt {
                barrier,
                state: BotPolicyBarrierState::Effective,
            },
        })
    }
}

#[test]
fn retained_access_recovers_after_transient_reply_without_new_request() {
    let mut b = bot();
    b.revision = 1;
    let profile = ProfileId::new();
    let mut repository = Repository::in_memory().unwrap();
    repository
        .save_bot(&b, profile, BotRequestId::mint(), "create", None)
        .unwrap();
    b.revision += 1;
    b.policy_revision += 1;
    b.access = BotAccess::Workspace;
    let request = BotRequestId::mint();
    let receipt = repository
        .prepare_bot_access(
            &b,
            profile,
            request,
            &"a".repeat(64),
            1,
            &barrier(&b).unwrap(),
        )
        .unwrap();
    let delegate = Arc::new(TransientDelegate(std::sync::atomic::AtomicUsize::new(0)));
    let make = || BotPolicyJob {
        bot: b.clone(),
        work: PolicyWork::Coordinate(receipt.clone()),
        authority: Arc::new(Authority {
            owner: owner(&b),
            workspace: b.home_workspace,
            allowed: true,
        }),
        delegate: Some(delegate.clone()),
    };
    let now = Instant::now();
    let mut recovery = PolicyRecovery::default();
    recovery.retry_needed =
        !acknowledge_policy_result(&mut repository, &receipt, true, make().run().result);
    assert!(recovery.retry_needed);
    recovery.finish_pass(now);
    assert!(!recovery.ready(now));
    assert!(recovery.due(now + Duration::from_secs(1)));
    assert!(recovery.ready(now + Duration::from_secs(1)));
    let pending = repository
        .pending_bot_access(recovery.after.unwrap(), 16)
        .unwrap();
    assert_eq!(pending.pending.len(), 1);
    assert_eq!(pending.pending[0].3, request);
    let retained = repository
        .bot_access_receipt(owner(&b), b.id, request)
        .unwrap()
        .unwrap();
    assert!(acknowledge_policy_result(
        &mut repository,
        &retained,
        true,
        make().run().result
    ));
    assert!(
        repository
            .bot_access_receipt(owner(&b), b.id, request)
            .unwrap()
            .unwrap()
            .completed
    );
    assert!(
        repository
            .pending_bot_access(0, 16)
            .unwrap()
            .pending
            .is_empty()
    );
    assert_eq!(delegate.0.load(Ordering::SeqCst), 2);
}

#[test]
fn ordinary_admin_completion_wakes_policy_recovery() {
    let (output, events) = sync_channel(4);
    let shutdown = Arc::new(AtomicBool::new(false));
    let mut pool = ProviderAdminPool::spawn(None, None, output, shutdown).unwrap();
    let (reply, response) = sync_channel(1);
    assert!(
        pool.try_submit(ProviderAdminJob::Validate {
            provider: AiProviderId::parse("openai").unwrap(),
            reply,
        })
        .is_ok()
    );
    let _ = response.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(matches!(
        events.recv_timeout(Duration::from_secs(5)).unwrap(),
        Envelope::BotRecover
    ));
    assert!(matches!(
        events.recv_timeout(Duration::from_secs(5)).unwrap(),
        Envelope::BotPolicyRecover { restart: false }
    ));
    pool.stop();
}
