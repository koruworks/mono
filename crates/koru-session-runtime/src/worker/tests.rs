// SPDX-License-Identifier: MPL-2.0

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io::{self, Read, Write};
use std::str::FromStr;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use koru_events::{Draft, EmitContext};
use koru_session_protocol::{
    Attachment, AttachmentDeliveryPolicy, AttachmentId, AttachmentKind, AttachmentSource,
    AttachmentStatus, BuiltInProfileKey, CapabilityName, CapabilitySet, DeliveryMode,
    DeliveryState, GridSize, IdempotencyKey, KeyModifiers, KeyStroke, PendingInput, ProfileKind,
    ProviderKind, RawCommand, RenderUpdate, RowIndex, RowRange, SessionCommand, SessionProfileKey,
    ShellQuoting, StreamSequence, TerminalGeneration, TerminalKey, ViewportGeneration,
    ViewportRequest, capability,
};

use super::*;
use crate::platform::{PlatformControl, PlatformWaiter, ProcessExit};
use crate::{SessionRegistry, StreamReadError};

#[path = "tests/approval.rs"]
mod approval_tests;

#[path = "tests/creation.rs"]
mod creation_tests;
#[path = "tests/delivery.rs"]
mod delivery_tests;
#[path = "tests/origin.rs"]
mod origin_tests;

#[derive(Default)]
struct PipeState {
    chunks: VecDeque<Vec<u8>>,
    closed: bool,
}

#[derive(Clone)]
struct Feeder {
    pipe: Arc<(Mutex<PipeState>, Condvar)>,
}

impl Feeder {
    fn send(&self, bytes: impl Into<Vec<u8>>) {
        let (lock, ready) = &*self.pipe;
        lock.lock().unwrap().chunks.push_back(bytes.into());
        ready.notify_all();
    }

    fn close(&self) {
        let (lock, ready) = &*self.pipe;
        lock.lock().unwrap().closed = true;
        ready.notify_all();
    }
}

struct FakeReader {
    pipe: Arc<(Mutex<PipeState>, Condvar)>,
    current: Vec<u8>,
    offset: usize,
}

impl Read for FakeReader {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if self.offset < self.current.len() {
            return self.copy_current(output);
        }
        let (lock, ready) = &*self.pipe;
        let mut state = lock.lock().unwrap();
        while state.chunks.is_empty() && !state.closed {
            state = ready.wait(state).unwrap();
        }
        if let Some(chunk) = state.chunks.pop_front() {
            drop(state);
            self.current = chunk;
            self.offset = 0;
            self.copy_current(output)
        } else {
            Ok(0)
        }
    }
}

impl FakeReader {
    fn copy_current(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let count = output.len().min(self.current.len() - self.offset);
        output[..count].copy_from_slice(&self.current[self.offset..self.offset + count]);
        self.offset += count;
        Ok(count)
    }
}

#[derive(Default)]
struct CaptureState {
    bytes: Vec<u8>,
    terminated: bool,
    resizes: Vec<GridSize>,
}

struct CaptureWriter(Arc<(Mutex<CaptureState>, Condvar)>);

impl Write for CaptureWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let (lock, ready) = &*self.0;
        lock.lock().unwrap().bytes.extend_from_slice(bytes);
        ready.notify_all();
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct FakeControl {
    pipe: Feeder,
    capture: Arc<(Mutex<CaptureState>, Condvar)>,
}

impl PlatformControl for FakeControl {
    fn resize(&mut self, size: GridSize) -> crate::TransportResult<()> {
        self.capture.0.lock().unwrap().resizes.push(size);
        Ok(())
    }

    fn terminate_tree(&mut self) -> crate::TransportResult<()> {
        self.capture.0.lock().unwrap().terminated = true;
        self.pipe.close();
        Ok(())
    }

    fn close_pty(&mut self) {
        self.pipe.close();
    }

    fn process_id(&self) -> u32 {
        42
    }
}

struct FakeWaiter {
    pipe: Arc<(Mutex<PipeState>, Condvar)>,
}

impl PlatformWaiter for FakeWaiter {
    fn wait(self: Box<Self>) -> crate::TransportResult<ProcessExit> {
        let (lock, ready) = &*self.pipe;
        let mut state = lock.lock().unwrap();
        while !state.closed {
            state = ready.wait(state).unwrap();
        }
        Ok(ProcessExit {
            code: 0,
            success: true,
        })
    }
}

#[derive(Default)]
struct Events(Mutex<Vec<String>>);

impl SessionLifecycleSink for Events {
    fn publish(&self, event: Draft) {
        let envelope = event.into_envelope(&EmitContext::system("session-test"));
        self.0
            .lock()
            .unwrap()
            .push(envelope.name.as_str().to_owned());
    }
}

struct Harness {
    owner: SessionOwner,
    feeder: Feeder,
    capture: Arc<(Mutex<CaptureState>, Condvar)>,
    events: Arc<Events>,
}

fn harness() -> Harness {
    harness_with(
        AttachmentDeliveryPolicy::Unsupported,
        CapabilitySet::default(),
        Arc::new(NoopAttachmentPathResolver),
    )
}

fn harness_with(
    attachment_delivery: AttachmentDeliveryPolicy,
    capabilities: CapabilitySet,
    attachment_paths: Arc<dyn AttachmentPathResolver>,
) -> Harness {
    let pipe = Arc::new((Mutex::new(PipeState::default()), Condvar::new()));
    let feeder = Feeder {
        pipe: Arc::clone(&pipe),
    };
    let capture = Arc::new((Mutex::new(CaptureState::default()), Condvar::new()));
    let control = FakeControl {
        pipe: feeder.clone(),
        capture: Arc::clone(&capture),
    };
    let (transport, reader) = PtyTransport::from_parts(
        Box::new(control),
        Box::new(FakeWaiter {
            pipe: Arc::clone(&pipe),
        }),
        Box::new(FakeReader {
            pipe,
            current: Vec::new(),
            offset: 0,
        }),
        Box::new(CaptureWriter(Arc::clone(&capture))),
    )
    .unwrap();
    let events = Arc::new(Events::default());
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.keep();
    let size = GridSize::new(20, 6).unwrap();
    let create = SessionCreate {
        profile: SessionProfileKey::BuiltIn(BuiltInProfileKey::from_str("platform-shell").unwrap()),
        parent_session_id: None,
        profile_kind: ProfileKind::Shell,
        display_name: "Test shell".to_owned(),
        working_directory: cwd.clone(),
        attachment_delivery,
        capabilities,
        launch: SessionLaunch::Raw(SpawnRequest::new(["fake"], cwd, size)),
        created_at_ms: 10,
        origin: Default::default(),
    };
    let owner = SessionOwner::start(
        create,
        SessionRuntimeConfig {
            stats_enabled: true,
            trace: SessionTrace::new(false),
            lifecycle: events.clone(),
            resources: Arc::new(NoopResourceLifecycle),
            attachment_paths,
            managed_providers: Arc::new(ManagedProviderRegistry::default()),
        },
        transport,
        reader,
        Instant::now(),
    )
    .unwrap();
    Harness {
        owner,
        feeder,
        capture,
        events,
    }
}

fn viewport(generation: u64, first: u64, count: u16) -> ViewportRequest {
    ViewportRequest {
        terminal_generation: TerminalGeneration::new(1).unwrap(),
        viewport_generation: ViewportGeneration::new(generation).unwrap(),
        rows: RowRange {
            first: RowIndex(first),
            count,
        },
        columns: 20,
        overscan_rows: 0,
    }
}

fn eventually(mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while !predicate() {
        assert!(Instant::now() < deadline, "condition did not settle");
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn command(handle: &SessionHandle, lease: ControllerLeaseToken, action: RawCommand) {
    handle
        .command(SessionCommand {
            idempotency_key: IdempotencyKey::mint(),
            session_id: handle.id(),
            controller_lease: lease,
            action,
        })
        .unwrap();
}

#[test]
fn detach_keeps_the_process_discoverable_and_reattach_is_one_committed_fact() {
    let harness = harness();
    let handle = harness.owner.handle();
    let first = handle.attach(viewport(1, 0, 6), 20).unwrap();
    assert!(matches!(
        first.render.recv_timeout(Duration::from_secs(1)),
        Ok(RenderUpdate::Snapshot { .. })
    ));
    assert!(matches!(
        handle.attach(viewport(2, 0, 6), 21),
        Err(ErrorCode::ControllerConflict)
    ));
    handle.detach(first.lease.token).unwrap();
    harness.feeder.send(b"background".to_vec());
    eventually(|| handle.summary().attention.unread_count > 0);
    assert!(!handle.is_ended());

    let second = handle.attach(viewport(2, 0, 6), 30).unwrap();
    assert_eq!(handle.summary().controller, ControllerState::Controlled);
    assert!(matches!(
        handle.set_viewport(second.lease.token, viewport(1, 0, 6)),
        Err(ErrorCode::StaleGeneration)
    ));
    let names = harness.events.0.lock().unwrap().clone();
    assert_eq!(
        names
            .iter()
            .filter(|name| name.as_str() == "terminal.session.started")
            .count(),
        1
    );
    assert_eq!(
        names
            .iter()
            .filter(|name| name.as_str() == "terminal.session.reattached")
            .count(),
        1
    );
}

#[test]
fn live_attach_clamps_a_wider_window_to_the_detached_pty_grid() {
    let harness = harness();
    let handle = harness.owner.handle();
    let attached = handle
        .attach_live(ViewportGeneration::new(1).unwrap(), 200, 0, 20)
        .unwrap();
    let RenderUpdate::Snapshot { snapshot } = attached
        .render
        .recv_timeout(Duration::from_secs(1))
        .unwrap()
    else {
        panic!("live attach did not begin with a bounded snapshot");
    };
    assert_eq!(snapshot.size, GridSize::new(20, 6).unwrap());
    assert!(
        snapshot
            .rows
            .iter()
            .flat_map(|row| &row.cells)
            .all(|cell| cell.column.0 < 20)
    );
}

#[test]
fn width_only_resize_publishes_the_new_grid_before_viewport_replacement() {
    let harness = harness();
    let handle = harness.owner.handle();
    let attached = handle.attach(viewport(1, 0, 6), 20).unwrap();
    assert!(matches!(
        attached.render.recv_timeout(Duration::from_secs(1)),
        Ok(RenderUpdate::Snapshot { .. })
    ));

    command(
        &handle,
        attached.lease.token,
        RawCommand::Resize {
            size: GridSize::new(30, 6).unwrap(),
        },
    );
    eventually(|| harness.capture.0.lock().unwrap().resizes == [GridSize::new(30, 6).unwrap()]);
    let RenderUpdate::Snapshot { snapshot } = attached
        .render
        .recv_timeout(Duration::from_secs(1))
        .unwrap()
    else {
        panic!("width-only resize did not publish a replacement snapshot");
    };
    assert_eq!(snapshot.size, GridSize::new(30, 6).unwrap());
    assert!(
        snapshot
            .rows
            .iter()
            .all(|row| row.cells.len() == usize::from(snapshot.size.columns)),
        "the replacement snapshot must serialize the enlarged grid width"
    );

    harness.feeder.send(b"\x1b[1;25HX".to_vec());
    let RenderUpdate::Delta { delta } = attached
        .render
        .recv_timeout(Duration::from_secs(1))
        .unwrap()
    else {
        panic!("output in the enlarged columns did not publish a delta");
    };
    assert!(
        delta
            .rows
            .iter()
            .flat_map(|row| &row.cells)
            .any(|cell| { cell.column.0 == 24 && cell.text == "X" })
    );
}

#[test]
fn output_is_parsed_directly_and_render_payload_is_only_viewport_damage() {
    let harness = harness();
    let handle = harness.owner.handle();
    let attached = handle.attach(viewport(1, 0, 6), 20).unwrap();
    let snapshot = attached
        .render
        .recv_timeout(Duration::from_secs(1))
        .unwrap();
    assert!(matches!(snapshot, RenderUpdate::Snapshot { .. }));
    harness.feeder.send(b"hello".to_vec());
    let delta = attached
        .render
        .recv_timeout(Duration::from_secs(1))
        .unwrap();
    let RenderUpdate::Delta { delta } = delta else {
        panic!("expected delta");
    };
    assert_eq!(delta.rows.len(), 1);
    assert!(delta.rows.iter().all(|row| row.index.0 < 6));
    let stats = handle.stats().unwrap();
    assert_eq!(stats.counter(Counter::OutputBytes), 5);
    assert_eq!(stats.counter(Counter::ParsedBatches), 1);
    assert_eq!(stats.counter(Counter::ViewportRowsSent), 1);
}

#[test]
fn slow_render_and_raw_observers_gap_without_growing_and_recover_with_one_snapshot() {
    let harness = harness();
    let handle = harness.owner.handle();
    let attached = handle.attach(viewport(1, 0, 6), 20).unwrap();
    let output = handle.subscribe_output();
    for _ in 0..20 {
        harness.feeder.send(b"x".to_vec());
    }
    eventually(|| handle.stats().unwrap().counter(Counter::GappedSubscribers) >= 2);

    let mut render_gap = false;
    while let Ok(update) = attached.render.try_recv() {
        render_gap |= matches!(update, RenderUpdate::Gap { .. });
    }
    assert!(render_gap);
    let mut output_gap = false;
    while let Ok(update) = output.try_recv() {
        output_gap |= matches!(update, RawOutputUpdate::Gap { .. });
    }
    assert!(output_gap);

    handle.recover_snapshot(attached.lease.token).unwrap();
    assert!(matches!(
        attached.render.recv_timeout(Duration::from_secs(1)),
        Ok(RenderUpdate::Snapshot { .. })
    ));
}

#[test]
fn observation_cursor_reports_output_missed_before_subscription_even_when_quiet() {
    let harness = harness();
    let handle = harness.owner.handle();
    let observation = handle
        .observe_snapshot(
            ViewportGeneration::new(1).unwrap(),
            RowRange {
                first: RowIndex(0),
                count: 6,
            },
            20,
        )
        .unwrap();
    assert_eq!(observation.output_sequence, StreamSequence::new(1).unwrap());

    // There is deliberately no output subscriber yet. The sequence must
    // still move, and subscribing from the snapshot cursor must immediately
    // expose the missed interval without waiting for another byte.
    harness.feeder.send(b"between".to_vec());
    eventually(|| handle.stats().unwrap().counter(Counter::OutputBytes) == 7);
    let output = handle.subscribe_output_from(Some(observation.output_sequence));
    assert_eq!(
        output.recv_timeout(Duration::from_secs(1)),
        Ok(RawOutputUpdate::Gap {
            expected: StreamSequence::new(2).unwrap(),
            actual: StreamSequence::new(2).unwrap(),
        })
    );
}

#[test]
fn historical_rows_are_paged_in_a_fixed_window_and_suspended_render_sends_no_grid() {
    let harness = harness();
    let handle = harness.owner.handle();
    harness.feeder.send("line\r\n".repeat(100).into_bytes());
    eventually(|| handle.stats().unwrap().counter(Counter::OutputBytes) == 600);
    let terminal = handle.terminal_state().unwrap();
    assert!(terminal.retained.len() > u64::from(terminal.visible.count));
    let attached = handle
        .attach_live(ViewportGeneration::new(1).unwrap(), 20, 0, 20)
        .unwrap();
    let page = handle
        .scrollback_page(
            attached.lease.token,
            terminal.terminal_generation,
            ViewportGeneration::new(1).unwrap(),
            RowRange {
                first: terminal.retained.first,
                count: 3,
            },
        )
        .unwrap();
    assert_eq!(page.rows.len(), 3);
    assert_eq!(page.range.count, 3);

    handle.suspend_render(attached.lease.token).unwrap();
    assert!(matches!(
        handle.set_viewport(attached.lease.token, viewport(1, 0, 6)),
        Err(ErrorCode::StaleGeneration)
    ));
    let rows_before = handle.stats().unwrap().counter(Counter::ViewportRowsSent);
    harness.feeder.send("hidden\r\n".repeat(100).into_bytes());
    eventually(|| handle.stats().unwrap().counter(Counter::OutputBytes) == 1_400);
    assert_eq!(
        handle.stats().unwrap().counter(Counter::ViewportRowsSent),
        rows_before
    );
}

#[test]
fn detached_processing_accounts_for_bytes_linearly_without_render_work() {
    for multiplier in [1usize, 10, 100] {
        let harness = harness();
        let handle = harness.owner.handle();
        let bytes = vec![b'x'; 512 * multiplier];
        harness.feeder.send(bytes);
        eventually(|| {
            handle.stats().unwrap().counter(Counter::OutputBytes) == (512 * multiplier) as u64
        });
        let stats = handle.stats().unwrap();
        assert_eq!(stats.counter(Counter::ViewportRowsSent), 0);
        assert_eq!(stats.counter(Counter::Deltas), 0);
    }
}

#[test]
fn commands_are_lease_gated_retry_safe_mode_aware_and_off_thread() {
    let harness = harness();
    let handle = harness.owner.handle();
    let attached = handle.attach(viewport(1, 0, 6), 20).unwrap();
    harness.feeder.send(b"\x1b[?2004h".to_vec());
    eventually(|| handle.stats().unwrap().counter(Counter::ParsedBatches) == 1);
    command(
        &handle,
        attached.lease.token,
        RawCommand::PasteText {
            text: "paste".to_owned(),
        },
    );
    command(
        &handle,
        attached.lease.token,
        RawCommand::SendKey {
            key: KeyStroke {
                key: TerminalKey::ArrowUp,
                modifiers: KeyModifiers::default(),
            },
        },
    );
    command(
        &handle,
        attached.lease.token,
        RawCommand::Resize {
            size: GridSize::new(30, 8).unwrap(),
        },
    );
    eventually(|| {
        let capture = harness.capture.0.lock().unwrap();
        capture.bytes.ends_with(b"\x1b[A") && capture.resizes == [GridSize::new(30, 8).unwrap()]
    });
    assert!(
        harness
            .capture
            .0
            .lock()
            .unwrap()
            .bytes
            .windows(b"\x1b[200~paste\x1b[201~".len())
            .any(|window| window == b"\x1b[200~paste\x1b[201~")
    );

    let key = IdempotencyKey::mint();
    let first = SessionCommand {
        idempotency_key: key,
        session_id: handle.id(),
        controller_lease: attached.lease.token,
        action: RawCommand::WriteText {
            text: "once".to_owned(),
        },
    };
    handle.command(first.clone()).unwrap();
    handle.command(first).unwrap();
    assert_eq!(
        handle
            .command(SessionCommand {
                idempotency_key: key,
                session_id: handle.id(),
                controller_lease: attached.lease.token,
                action: RawCommand::WriteText {
                    text: "different".to_owned(),
                },
            })
            .unwrap_err(),
        ErrorCode::IdempotencyConflict
    );
}

#[test]
fn registry_lock_never_owns_worker_lifetime_and_shutdown_reclaims_every_tree() {
    let registry = SessionRegistry::new(SessionRuntimeConfig::default());
    let first = harness();
    let first_capture = Arc::clone(&first.capture);
    let first_handle = registry.insert_owner_for_test(first.owner);
    let second = harness();
    let second_capture = Arc::clone(&second.capture);
    let second_handle = registry.insert_owner_for_test(second.owner);
    assert_eq!(registry.list().len(), 2);
    assert!(registry.get(first_handle.id()).is_ok());
    assert!(registry.get(second_handle.id()).is_ok());

    registry.shutdown();
    assert!(first_capture.0.lock().unwrap().terminated);
    assert!(second_capture.0.lock().unwrap().terminated);
    assert!(registry.list().is_empty());
}

#[test]
fn natural_eof_and_explicit_termination_end_once_and_close_subscriptions() {
    let mut natural = harness();
    let handle = natural.owner.handle();
    let events = handle.subscribe_events();
    natural.feeder.close();
    eventually(|| handle.is_ended());
    natural.owner.shutdown();
    assert!(matches!(
        events.recv_timeout(Duration::from_millis(10)),
        Ok(SessionEventUpdate::Event(SequencedSessionEvent {
            event: SessionEvent::Exited { .. },
            ..
        }))
    ));
    assert_eq!(
        natural
            .events
            .0
            .lock()
            .unwrap()
            .iter()
            .filter(|name| name.as_str() == "terminal.session.ended")
            .count(),
        1
    );

    let mut terminated = harness();
    let handle = terminated.owner.handle();
    let attached = handle.attach(viewport(1, 0, 6), 20).unwrap();
    handle.terminate(attached.lease.token).unwrap();
    eventually(|| handle.is_ended());
    terminated.owner.shutdown();
    assert!(terminated.capture.0.lock().unwrap().terminated);
    assert_eq!(
        terminated
            .events
            .0
            .lock()
            .unwrap()
            .iter()
            .filter(|name| name.as_str() == "terminal.session.ended")
            .count(),
        1
    );
    loop {
        match attached.render.try_recv() {
            Ok(_) => {}
            Err(StreamReadError::Closed) => break,
            Err(StreamReadError::Empty) => panic!("closed stream became empty"),
        }
    }
}

#[test]
fn an_overrun_event_subscriber_receives_one_explicit_gap() {
    let mut harness = harness();
    let handle = harness.owner.handle();
    let events = handle.subscribe_events();

    for _ in 0..=EVENT_QUEUE_CAPACITY {
        let mut state = handle.shared.lock();
        publish_event_locked(
            &mut state,
            handle.id(),
            SessionEvent::AttentionChanged {
                attention: AttentionState::default(),
            },
            &handle.shared.stats,
        );
    }

    let update = events.recv_timeout(Duration::from_millis(10)).unwrap();
    let SessionEventUpdate::Gap { expected, actual } = update else {
        panic!("overflow did not replace queued events with a gap: {update:?}");
    };
    assert_eq!(actual.get() - expected.get(), EVENT_QUEUE_CAPACITY as u64);
    assert!(matches!(
        events.recv_timeout(Duration::from_millis(10)),
        Err(StreamReadError::Empty)
    ));
    harness.owner.shutdown();
}

/// **A second client may watch a session whose controller somebody else
/// holds** (ADR-361), and watching is not a second claim on it.
///
/// This is the rule the read-only run session rests on. The delegated
/// orchestrator holds the controller lease for the whole of a run, so a
/// window that wanted to *see* the agent working had only one way in — take
/// the lease off it. The observer verbs never required one: they take the
/// state lock, read the retained tail and install a subscriber, and
/// `require_lease` is not on that path. Asserted here rather than assumed,
/// because "the lease is untouched" is the whole safety claim.
#[test]
fn a_managed_session_can_be_observed_while_its_controller_lease_is_held() {
    let mut harness = harness();
    let handle = harness.owner.handle();
    let held = handle.attach(viewport(1, 0, 6), 20).unwrap();
    {
        let mut state = handle.shared.lock();
        state.summary.mode = SessionMode::Managed {
            provider: ProviderKind::from_str("codex").unwrap(),
        };
    }
    // A second attach is refused, which is what makes the rest of this test
    // worth writing: there is exactly one controller and it is taken.
    assert!(matches!(
        handle.attach(viewport(2, 0, 6), 21),
        Err(ErrorCode::ControllerConflict)
    ));

    // The observer reads anyway — the retained tail with its resume cursor,
    // then a live subscription from that cursor. No lease is passed, because
    // neither call has a parameter for one.
    let observation = handle.observe_agent_events().unwrap();
    let watched = handle.subscribe_agent_events_from(Some(observation.cursor));
    assert!(
        matches!(watched.try_recv(), Err(StreamReadError::Empty)),
        "a watch installed at the tail's own cursor starts with no gap"
    );

    // What the agent does next reaches the watcher, live, with the lease
    // still where it was.
    let turn_id = koru_session_protocol::AgentTurnId::mint();
    {
        let mut state = handle.shared.lock();
        let id = handle.id();
        state.agent_stream.publish(
            id,
            koru_session_protocol::AgentEvent::TurnStarted { turn_id },
            &handle.shared.stats,
        );
    }
    let seen = watched.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(matches!(
        seen,
        AgentEventUpdate::Event(koru_session_protocol::SequencedAgentEvent {
            event: koru_session_protocol::AgentEvent::TurnStarted { turn_id: seen },
            ..
        }) if seen == turn_id
    ));

    // And the lease that was held is still the lease that is held: the
    // controller can still act, and nothing about its standing moved.
    assert_eq!(handle.summary().controller, ControllerState::Controlled);
    handle.detach(held.lease.token).unwrap();
    assert_eq!(handle.summary().controller, ControllerState::Detached);
    harness.owner.shutdown();
}

#[test]
fn managed_attention_clears_only_after_its_presented_sequence() {
    let mut harness = harness();
    let handle = harness.owner.handle();
    let attached = handle.attach(viewport(1, 0, 6), 20).unwrap();
    let approval_id = ApprovalId::mint();
    let raised_at = StreamSequence::new(9).unwrap();
    {
        let mut state = handle.shared.lock();
        state.summary.mode = SessionMode::Managed {
            provider: ProviderKind::from_str("codex").unwrap(),
        };
        state.summary.managed = Some(koru_session_protocol::ManagedState::WaitingForApproval);
        state.summary.attention = AttentionState {
            unread_count: 1,
            requires_user: true,
            reason: Some(AttentionReason::ApprovalRequired),
        };
        state.managed_attention = Some(ManagedAttention {
            approval_id,
            sequence: raised_at,
        });
    }

    handle
        .acknowledge_attention(attached.lease.token, StreamSequence::new(8).unwrap())
        .unwrap();
    assert!(handle.summary().attention.requires_user);

    handle
        .acknowledge_attention(attached.lease.token, raised_at)
        .unwrap();
    assert_eq!(handle.summary().attention, AttentionState::default());
    harness.owner.shutdown();
}

#[test]
#[ignore = "reproducible engineering measurement; see docs/contributing/operations/RUNBOOK.md#terminal-split-stack-probe"]
fn session_worker_strategy_measurement() {
    let started = Instant::now();
    let sessions: Vec<_> = (0..100).map(|_| harness()).collect();
    let created = started.elapsed();
    #[cfg(windows)]
    {
        let query = format!(
            "$p=Get-Process -Id {}; Write-Output ('threads=' + $p.Threads.Count + ' handles=' + $p.HandleCount + ' working_set=' + $p.WorkingSet64 + ' private=' + $p.PrivateMemorySize64)",
            std::process::id()
        );
        let output = std::process::Command::new("powershell.exe")
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                &query,
            ])
            .output()
            .unwrap();
        eprintln!("{}", String::from_utf8_lossy(&output.stdout).trim());
    }
    eprintln!(
        "created_100={created:?} requested_stack_per_session={}KiB",
        (512 + 128 + 128 + 128)
    );
    drop(sessions);
}

/// **A run cancellation outranks a takeover boundary on the same turn, and
/// the worker's recorded reason replaces whatever the adapter wrote.** The
/// adapter knows an interruption happened, not why; a takeover asked for
/// and then a cancel of the run is a run cancellation, and the reverse
/// order is too. A cancellation the worker never recorded — the provider's
/// own — keeps the adapter's default, which is `Run`.
#[test]
fn a_recorded_cancel_reason_is_stamped_and_run_outranks_takeover() {
    use koru_session_protocol::{AgentEvent, AgentTurnId, CancelReason};

    let harness = harness();
    let handle = harness.owner.handle();
    let takeover_then_run = AgentTurnId::mint();
    let run_then_takeover = AgentTurnId::mint();
    let takeover_only = AgentTurnId::mint();
    let unrecorded = AgentTurnId::mint();
    {
        let mut state = handle.shared.lock();
        record_cancel_reason(
            &mut state,
            takeover_then_run,
            CancelReason::TakeoverBoundary,
        );
        record_cancel_reason(&mut state, takeover_then_run, CancelReason::Run);
        record_cancel_reason(&mut state, run_then_takeover, CancelReason::Run);
        record_cancel_reason(
            &mut state,
            run_then_takeover,
            CancelReason::TakeoverBoundary,
        );
        record_cancel_reason(&mut state, takeover_only, CancelReason::TakeoverBoundary);
    }
    let stamped = |turn_id: AgentTurnId| {
        let mut state = handle.shared.lock();
        match stamp_cancel_reason(
            &mut state,
            AgentEvent::TurnCancelled {
                turn_id,
                reason: CancelReason::default(),
            },
        ) {
            AgentEvent::TurnCancelled { reason, .. } => reason,
            other => panic!("not a cancellation: {other:?}"),
        }
    };
    assert_eq!(stamped(takeover_then_run), CancelReason::Run);
    assert_eq!(stamped(run_then_takeover), CancelReason::Run);
    assert_eq!(stamped(takeover_only), CancelReason::TakeoverBoundary);
    assert_eq!(stamped(unrecorded), CancelReason::Run);
    // Stamping consumes the record: a second cancellation of the same turn
    // is the provider's own again.
    assert_eq!(stamped(takeover_only), CancelReason::Run);
    assert!(handle.shared.lock().cancel_reasons.is_empty());
}

/// **The handover moves only forward, and only through the verbs.** With a
/// raw harness and no orchestrator: a request while held reads `Requested`
/// and publishes; release reads `Released`; the next attach reads `Taken`;
/// return reads `Returned`; the next attach reads `Idle`. A return without a
/// taken lease is refused, and a request on a free session moves nothing.
#[test]
fn the_handover_walks_its_cycle_through_the_verbs() {
    use koru_session_protocol::{ControlRequestOutcome, HandoverState, TakeoverUrgency};

    let mut harness = harness();
    let handle = harness.owner.handle();
    let events = handle.subscribe_events();
    let mut handovers = Vec::new();
    let mut drain = |events: &BoundedStream<SessionEventUpdate>| {
        while let Ok(SessionEventUpdate::Event(event)) =
            events.recv_timeout(Duration::from_millis(50))
        {
            if let SessionEvent::HandoverChanged { handover } = event.event {
                handovers.push(handover);
            }
        }
    };

    assert_eq!(
        handle
            .request_control(TakeoverUrgency::AtTurnBoundary)
            .unwrap(),
        ControlRequestOutcome::Available
    );
    assert_eq!(handle.summary().handover, HandoverState::Idle);

    let holder = handle.attach(viewport(1, 0, 6), 20).unwrap();
    assert_eq!(
        handle.return_control(holder.lease.token).unwrap_err(),
        ErrorCode::InvalidTransition,
        "nothing was taken, so nothing can be returned"
    );
    assert_eq!(
        handle
            .request_control(TakeoverUrgency::AtTurnBoundary)
            .unwrap(),
        ControlRequestOutcome::Waiting
    );
    assert_eq!(
        handle.request_control(TakeoverUrgency::Now).unwrap(),
        ControlRequestOutcome::Waiting,
        "a repeated request is the same request, and a raw session has no turn to end"
    );
    assert_eq!(handle.summary().handover, HandoverState::Requested);
    assert_eq!(
        handle.attach(viewport(2, 0, 6), 21).err(),
        Some(ErrorCode::ControllerConflict)
    );

    handle.release_control(holder.lease.token).unwrap();
    assert_eq!(handle.summary().handover, HandoverState::Released);
    assert_eq!(handle.summary().controller, ControllerState::Detached);
    let taken = handle.attach(viewport(2, 0, 6), 22).unwrap();
    assert_eq!(handle.summary().handover, HandoverState::Taken);
    handle.return_control(taken.lease.token).unwrap();
    assert_eq!(handle.summary().handover, HandoverState::Returned);
    let reclaimed = handle.attach(viewport(3, 0, 6), 23).unwrap();
    assert_eq!(handle.summary().handover, HandoverState::Idle);
    handle.detach(reclaimed.lease.token).unwrap();
    assert_eq!(handle.summary().handover, HandoverState::Idle);

    drain(&events);
    assert_eq!(
        handovers,
        vec![
            HandoverState::Requested,
            HandoverState::Released,
            HandoverState::Taken,
            HandoverState::Returned,
            HandoverState::Idle,
        ]
    );
    harness.owner.shutdown();
}

/// D19: a refused level carries its own token, so the chat can say it was
/// the level; any other failure's message stays the code's.
#[test]
fn a_refused_level_tells_the_client_it_was_the_level() {
    let told = super::provider_failure(crate::providers::ProviderFailure::new(
        ErrorCode::ProviderModelUnavailable,
        koru_session_protocol::ProviderStage::Initialize,
        koru_session_protocol::ProviderDiagnostic::ReasoningRejected,
    ));
    assert_eq!(told.code, ErrorCode::ProviderModelUnavailable);
    assert_eq!(told.message, koru_session_protocol::REASONING_REJECTED);
    let model = super::provider_failure(crate::providers::ProviderFailure::new(
        ErrorCode::ProviderModelUnavailable,
        koru_session_protocol::ProviderStage::Initialize,
        koru_session_protocol::ProviderDiagnostic::HandshakeRejected,
    ));
    assert_eq!(model.message, ErrorCode::ProviderModelUnavailable.as_str());
}
