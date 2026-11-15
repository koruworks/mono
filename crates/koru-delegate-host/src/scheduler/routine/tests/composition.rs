// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::ledger::SharedLedger;
use crate::routine_bridge::{
    HostRoutineBridge,
    tests::{authority, gateway, receive, send},
};
use koru_delegate_core::*;
use koru_delegate_document::{
    RoutineExpiryAuthority, RoutineExpiryPlan, ScheduleClaimAuthority, ScheduleClaimPlan,
    ScheduleDocument,
};
use koru_delegate_runtime::*;
use koru_delegate_schedule::*;
use std::sync::atomic::AtomicUsize;

struct Clock;
impl koru_delegate_sqlite::Clock for Clock {
    fn now_ms(&self) -> i64 {
        61_000
    }
}
impl ScheduleClock for Clock {
    fn now_ms(&mut self, _: i64) -> i64 {
        61_000
    }
}
#[derive(Clone)]
struct Docs {
    workspace: WorkspaceId,
    document: DocumentId,
    snapshot: Arc<Mutex<Vec<u8>>>,
    handles: Arc<AtomicUsize>,
}
struct Doc {
    source: Docs,
    document: ScheduleDocument,
}
impl Drop for Doc {
    fn drop(&mut self) {
        self.source.handles.fetch_sub(1, Ordering::SeqCst);
    }
}
impl ScheduleDocumentHandle for Doc {
    fn document(&self) -> &ScheduleDocument {
        &self.document
    }
    fn claim_authority(
        &self,
        device: DeviceId,
        plan: ScheduleClaimPlan,
    ) -> PortResult<Option<ScheduleClaimAuthority>> {
        assert_eq!(
            self.source.document.as_uuid(),
            self.document.header()?.schedule.as_uuid()
        );
        self.document
            .claim_authority(self.source.workspace, device, plan)
            .map_err(Into::into)
    }
    fn expired_routine_authority(
        &self,
        device: DeviceId,
        plan: RoutineExpiryPlan,
    ) -> PortResult<Option<RoutineExpiryAuthority>> {
        self.document
            .expired_routine_authority(self.source.workspace, device, plan)
            .map_err(Into::into)
    }
    fn absorb(&mut self) -> PortResult<bool> {
        let before = self.document.version();
        self.document
            .import(&self.source.snapshot.lock().unwrap())?;
        Ok(before != self.document.version())
    }
    fn commit(
        &mut self,
        write: &mut dyn FnMut(
            &ScheduleDocument,
        ) -> Result<(), koru_delegate_document::ScheduleDocumentError>,
    ) -> PortResult<()> {
        write(&self.document)?;
        *self.source.snapshot.lock().unwrap() = self.document.export_snapshot()?;
        Ok(())
    }
}
struct Watch;
impl ScheduleChangeWatch for Watch {
    fn close(self: Box<Self>) {}
}
impl ScheduleDocuments for Docs {
    fn create(&mut self, _: WorkspaceId, _: &ScheduleHeader) -> PortResult<DocumentId> {
        panic!("no create")
    }
    fn open(
        &mut self,
        workspace: WorkspaceId,
        document: DocumentId,
    ) -> PortResult<Box<dyn ScheduleDocumentHandle>> {
        assert_eq!((workspace, document), (self.workspace, self.document));
        self.handles.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(Doc {
            source: self.clone(),
            document: ScheduleDocument::from_snapshot(&self.snapshot.lock().unwrap()).unwrap(),
        }))
    }
    fn discover_active(&mut self) -> PortResult<ScheduleCatalog> {
        Ok(ScheduleCatalog {
            workspaces: vec![self.workspace],
            schedules: vec![ListedSchedule {
                workspace: self.workspace,
                document: self.document,
                header: ScheduleDocument::from_snapshot(&self.snapshot.lock().unwrap())
                    .unwrap()
                    .header()?,
                trashed_at_ms: None,
            }],
            failed_workspaces: vec![],
            unavailable_documents: vec![],
            failures: 0,
        })
    }
    fn discover_identities(&mut self) -> PortResult<ScheduleCatalog> {
        self.discover_active()
    }
    fn watch_changes(
        &mut self,
        _: &ScheduleWatchTargets,
        _: ScheduleWake,
    ) -> PortResult<Box<dyn ScheduleChangeWatch>> {
        Ok(Box::new(Watch))
    }
}
struct Preparation(koru_delegate_scope::ScopeIdentity);
impl SchedulePreparation for Preparation {
    fn profile_delegatable(&mut self, _: &SessionProfileKey) -> PortResult<bool> {
        panic!("provider gate")
    }
    fn resolve_scope(
        &mut self,
        _: RunId,
        scope: &WorkspaceScope,
    ) -> Result<koru_delegate_scope::ResolvedScope, koru_delegate_scope::ScopeRefusal> {
        let WorkspaceScope::BotFolder { folder } = scope else {
            panic!("Bot folder")
        };
        Ok(koru_delegate_scope::ResolvedScope {
            identity: self.0.clone(),
            directory: std::path::PathBuf::from(folder.as_str()),
        })
    }
    fn resolve_and_pin(
        &mut self,
        _: RunId,
        _: &[ScheduleContextPointer],
        _: WorkspaceId,
    ) -> Result<Vec<RunContextItem>, CaptureFailure> {
        panic!("ordinary capture")
    }
}

#[test]
fn routine_composition_real_host_actor_bridge_and_shared_sql_keep_launch_closed() {
    let ledger = SharedLedger::new(
        koru_delegate_sqlite::RunsDb::open_with_clock(
            std::path::Path::new(":memory:"),
            Box::new(Clock),
        )
        .unwrap(),
    );
    let mut f = Fixture::with_store(Box::new(ledger.clone()));
    let route = RoutineLedger::routine_route(&ledger, f.prepared.slot.schedule)
        .unwrap()
        .unwrap();
    let p = &mut f.prepared;
    let mut definition = ScheduleDefinition {
        revision: 1,
        effective_at_ms: 0,
        template: DelegationTemplate {
            requested_by: p.submission.request.requested_by.clone(),
            objective: p.submission.request.objective.clone(),
            agent: p.submission.request.agent.clone(),
            requested_model: None,
            scope: p.submission.request.scope.clone(),
            grants: [p.submission.workspace].into_iter().collect(),
            context: vec![],
        },
        recurrence: RecurrenceRule::floating("* * * * *".parse().unwrap()),
        lifecycle: ScheduleLifecycle::Paused {
            reason: SchedulePauseReason::Manual,
        },
        routine: Some(
            RoutineAssociation::new(
                route.owner,
                route.bot,
                route.workspace,
                route.creation_request,
            )
            .unwrap(),
        ),
    };
    let document = ScheduleDocument::create(&ScheduleHeader {
        schedule: route.schedule,
        created_at_ms: 0,
        initial_owner: route.owner.device,
        definition: definition.clone(),
    })
    .unwrap();
    document
        .set_lifecycle(route.owner.device, 1, ScheduleLifecycle::Active, 1)
        .unwrap();
    definition = document.header().unwrap().definition;
    p.slot.occurrence_ms = 60_000;
    p.definition = RoutineDefinitionIdentity {
        revision: 2,
        digest: routine_definition_digest(&definition, route.workspace, None).unwrap(),
        owner_handoff: None,
    };
    p.submission.provenance.routine = Some(RoutineProvenance {
        schedule: route.schedule,
        occurrence_ms: 60_000,
        definition: p.definition,
        grant_revision: 1,
    });
    p.submission.request.origin = RunOrigin::Scheduled {
        schedule: route.schedule,
        occurrence_ms: 60_000,
        revision: 2,
    };
    p.semantic_hash = routine_semantic_digest(&p.slot, &definition, route.workspace, None).unwrap();
    assert!(p.valid());
    let prepared = p.clone();
    let offered = f.proof.clone();
    let admission = f.admission.clone();
    let handles = Arc::new(AtomicUsize::new(0));
    let docs = Docs {
        workspace: route.workspace,
        document: DocumentId::from_uuid(route.schedule.as_uuid()),
        snapshot: Arc::new(Mutex::new(document.export_snapshot().unwrap())),
        handles: handles.clone(),
    };
    let gate = Arc::new(ScheduleEventAuthorityGate::new());
    let server_gate = gate.clone();
    let server_ledger = ledger.clone();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let client = gateway(&format!("http://{}", listener.local_addr().unwrap()));
    let server = std::thread::spawn(move || {
        for phase in 0..4 {
            let mut stream = crate::routine_bridge::tests::accept(&listener);
            let request = receive(&mut stream);
            assert_eq!(handles.load(Ordering::SeqCst), 0);
            drop(
                server_gate
                    .enter_until(Instant::now() + Duration::from_secs(1))
                    .expect("HTTP outside shared gate"),
            );
            let payload = match phase {
                0 => serde_json::json!({"kind":"prepared","preparation":prepared}),
                1 => {
                    serde_json::json!({"kind":"ready","slot":prepared.slot,"semantic_hash":prepared.semantic_hash,"preparation_hash":prepared.preparation_hash,"admission":admission,"proof":offered})
                }
                2 => serde_json::json!({"kind":"missing","claim":request["payload"]["claim"]}),
                _ => {
                    let claim: RoutineClaimIdentity =
                        serde_json::from_value(request["payload"]["claim"].clone()).unwrap();
                    let observed =
                        RoutineLedger::read_routine_claim(&server_ledger, &claim.slot).unwrap();
                    assert!(
                        matches!(observed,RoutineClaimObservation::Claimed{record} if record.link_state==RoutineLinkState::Held{} && record.claim==claim)
                    );
                    RoutineLedger::acknowledge_routine_link(
                        &mut server_ledger.clone(),
                        &RoutineLinkAcknowledgement {
                            claim: claim.clone(),
                            outcome: RoutineLinkOutcome::Linked {},
                        },
                    )
                    .unwrap();
                    serde_json::json!({"kind":"linked","claim":claim})
                }
            };
            send(&mut stream,"200 OK",&serde_json::to_vec(&serde_json::json!({"version":1,"request_id":request["request_id"],"payload":payload})).unwrap());
        }
    });
    let stopped = Arc::new(AtomicBool::new(false));
    let bridge = HostRoutineBridge::new(&client, Some(&authority()), stopped.clone());
    let mut coordinator = ScheduleCoordinator::new(
        Box::new(docs),
        Box::new(ledger.clone()),
        Box::new(Preparation(f.scope.clone())),
        Box::new(Clock),
        route.owner.device,
    )
    .with_routines(
        Box::new(ledger.clone()),
        Box::new(bridge),
        Box::new(gate.as_ref().clone()),
    );
    let (commands, inbox) = mpsc::channel();
    let mut effects = ActorEffects::new(commands, stopped);
    let task = std::thread::spawn(move || {
        let guard = gate.enter().unwrap();
        let zone = koru_time_zone::IanaZoneRules::parse("UTC").unwrap();
        let mut report = coordinator.evaluate(&mut effects, 61_000, &zone).unwrap();
        drop(guard);
        coordinator
            .evaluate_routine_work(
                report.routine_work.take().unwrap(),
                &mut effects,
                Instant::now() + Duration::from_secs(5),
            )
            .unwrap()
    });
    f.handle(inbox.recv_timeout(Duration::from_secs(3)).unwrap());
    assert!(f.held());
    f.handle(inbox.recv_timeout(Duration::from_secs(3)).unwrap());
    assert!(!f.held());
    assert!(task.join().unwrap().started.is_empty());
    server.join().unwrap();
    let RoutineClaimObservation::Claimed { record } =
        RoutineLedger::read_routine_claim(&ledger, &f.prepared.slot).unwrap()
    else {
        panic!("claim")
    };
    assert_eq!(record.link_state, RoutineLinkState::Linked {});
    assert_eq!(record.claim.ids, f.prepared.ids);
    assert!(ScheduleLedger::unsettled(&ledger).unwrap().is_empty());
    assert!(
        koru_delegate_sqlite::BotLedger::reserve_bot(&mut ledger.clone(), f.prepared.ids.run, None)
            .is_err()
    );
}
