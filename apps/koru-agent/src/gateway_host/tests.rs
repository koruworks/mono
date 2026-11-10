// SPDX-License-Identifier: MPL-2.0

//! Fixture-driven proofs of what this file decides.
//!
//! **Nothing here starts a process, opens a Store or touches the disk.** The
//! supervision is a scripted [`FakeChildren`] and the public boundary is two
//! closures, so every ordering the plan names — a concurrent ensure, a
//! readiness failure, a wrong launch id, a wrong root, a stale probe, a crash
//! and a restart, an exhausted budget, an unrequested shutdown treated as an
//! unexpected exit — is a value test that runs in milliseconds. The
//! process-level halves live in `tests/lifecycle.rs`, where a real Agent
//! supervises a real child that publishes a real rendezvous.
//!
//! The three fencing properties are tested apart from each other, and each
//! test says which one it is about, because conflating them is the failure
//! this file exists to prevent.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use koru_agent_worker::machine::{Backoff, RestartBudget};
use koru_agent_worker::supervisor::{ExitReport, Forced, StopRequest};

use super::*;

/// How long a fixture consumer waits. Every case here arms the state it means
/// to test, so nothing legitimately needs longer than one poll.
const PATIENCE: Duration = Duration::from_millis(60);

/// A boundary that answers with one record and reports the child alive — the
/// ordinary case, so a test that is not about liveness says so once.
fn serving<'a>(
    discover: &'a (dyn Fn(&GatewayChild) -> Option<koru_api_discovery::GatewayHello> + Sync),
) -> Boundary<'a> {
    Boundary {
        still_there: &|_| true,
        discover,
    }
}

// ------------------------------------------------------------- the fixtures

fn hello(launch: Option<&str>, instance: &str) -> koru_api_discovery::GatewayHello {
    koru_api_discovery::GatewayHello {
        instance_id: instance.into(),
        supervisor_launch_id: launch.map(str::to_owned),
        gateway_version: "0".into(),
        segment: "test".into(),
        api_version: 1,
        api_endpoint: "http://127.0.0.1:1".into(),
        capabilities: vec!["store".into()],
        session: koru_api_discovery::SessionCredential::new(
            "bearer".into(),
            now_millis() + 60 * 60 * 1_000,
        ),
    }
}

fn child(launch: &str) -> GatewayChild {
    GatewayChild {
        launch_id: launch.into(),
        segment: "test".into(),
        root: std::path::PathBuf::from("X:/never-created"),
    }
}

/// A supervision whose state a test writes directly. It records every
/// owner-initiated request so the tests can assert *which* of them a decision
/// took, and how many times.
#[derive(Default)]
struct Script {
    state: Option<State>,
    generation: Option<Generation>,
    current: Option<Arc<GatewayChild>>,
    last_error: Option<String>,
    respawns: u64,
    holds: u32,
    resumes: u32,
    serving_stops: u32,
    stops: u32,
    /// What the next stop should answer. `None` means the ordinary case: the
    /// current child was asked, agreed and exited cleanly.
    next_outcome: Option<StopOutcome>,
    /// What each `resume()` should move the state to, in order.
    on_resume: Vec<(State, Option<Generation>, Option<Arc<GatewayChild>>)>,
}

impl Script {
    /// What a stop answers: the scripted outcome, or the ordinary one — the
    /// current child asked, agreed and exited 0. A stop with nothing running
    /// answers `NotRunning`, which is what the supervisor does.
    fn finish_stop(&mut self, request: StopRequest) -> StopOutcome {
        if let Some(outcome) = self.next_outcome.take() {
            return outcome;
        }
        match self.generation {
            Some(generation) => StopOutcome::Graceful {
                request,
                generation,
                code: Some(0),
            },
            None => StopOutcome::NotRunning {
                request,
                generation: None,
                last_exit: None,
            },
        }
    }
}

struct FakeChildren {
    script: Mutex<Script>,
    /// How many times anybody asked what the supervision was doing. A
    /// coalesced ensure is one that did not ask twice as often as it had to.
    reads: AtomicU64,
}

impl FakeChildren {
    fn ready(launch: &str, generation: u64) -> Arc<Self> {
        let payload = Arc::new(child(launch));
        Arc::new(Self {
            script: Mutex::new(Script {
                state: Some(State::Ready),
                generation: Some(Generation(generation)),
                current: Some(payload),
                ..Script::default()
            }),
            reads: AtomicU64::new(0),
        })
    }

    fn in_state(state: State) -> Arc<Self> {
        Arc::new(Self {
            script: Mutex::new(Script {
                state: Some(state),
                ..Script::default()
            }),
            reads: AtomicU64::new(0),
        })
    }

    fn edit<R>(&self, edit: impl FnOnce(&mut Script) -> R) -> R {
        edit(&mut self.script.lock().unwrap())
    }
}

/// A `Children` that forwards to a shared fake, so a test can keep a handle on
/// the script after the supervisor has taken ownership.
struct SharedChildren(Arc<FakeChildren>);

impl Children for SharedChildren {
    fn status(&self) -> StatusReport {
        self.0.reads.fetch_add(1, Ordering::Relaxed);
        let script = self.0.script.lock().unwrap();
        StatusReport {
            role: "gateway",
            state: script.state.unwrap_or(State::Starting),
            generation: script.generation,
            respawns: script.respawns,
            last_exit: None,
            last_error: script.last_error.clone(),
            last_stop: None,
        }
    }

    fn current(&self) -> Option<CurrentChild<GatewayChild>> {
        let script = self.0.script.lock().unwrap();
        Some(CurrentChild {
            generation: script.generation?,
            payload: Arc::clone(script.current.as_ref()?),
            pid: 0,
        })
    }

    fn issue(&self, generation: Generation) -> Result<Arc<GatewayChild>, StaleGeneration> {
        let script = self.0.script.lock().unwrap();
        match (&script.current, script.generation) {
            (Some(payload), Some(current)) if current == generation => Ok(Arc::clone(payload)),
            (_, current) => Err(StaleGeneration {
                requested: generation,
                current,
            }),
        }
    }

    fn hold(&self) {
        self.0.edit(|script| script.holds += 1);
    }

    fn resume(&self) {
        self.0.edit(|script| {
            script.resumes += 1;
            if !script.on_resume.is_empty() {
                let (state, generation, current) = script.on_resume.remove(0);
                script.state = Some(state);
                script.generation = generation;
                script.current = current;
            }
        });
    }

    fn stop_serving(&self) -> StopOutcome {
        self.0.edit(|script| {
            script.serving_stops += 1;
            let outcome = script.finish_stop(StopRequest::Serving);
            script.state = Some(State::Stopped);
            script.current = None;
            script.generation = None;
            outcome
        })
    }

    fn stop(&self) -> StopOutcome {
        self.0.edit(|script| {
            script.stops += 1;
            let outcome = script.finish_stop(StopRequest::Final);
            script.state = Some(State::Stopped);
            script.current = None;
            script.generation = None;
            outcome
        })
    }
}

fn supervisor(children: &Arc<FakeChildren>) -> Supervisor {
    Supervisor::over(Box::new(SharedChildren(Arc::clone(children))))
}

// ----------------------------------------- fencing property 3: the checked stop

/// The one that matters. A hello naming another launch is refused **before**
/// any client is built, so no authorized shutdown can reach a healthy
/// replacement.
#[test]
fn a_graceful_stop_for_a_replaced_child_asks_nobody() {
    let asked: Mutex<Vec<String>> = Mutex::new(Vec::new());
    let result = ask_the_checked_child_to_stop(
        "launch-a",
        || Ok(hello(Some("launch-b"), "instance-b")),
        |found| {
            asked.lock().unwrap().push(found.instance_id);
            Ok(())
        },
    );
    assert_eq!(
        result,
        StopAsk::Stale {
            expected: "launch-a".into(),
            found: Some("launch-b".into()),
        }
    );
    assert!(
        asked.lock().unwrap().is_empty(),
        "a shutdown was sent to the replacement"
    );
}

/// An unsupervised gateway — one nobody launched, publishing no launch id — is
/// equally not this child, and is equally not asked.
#[test]
fn a_graceful_stop_never_asks_an_unsupervised_gateway() {
    let asked = Mutex::new(0u32);
    let result = ask_the_checked_child_to_stop(
        "launch-a",
        || Ok(hello(None, "instance-x")),
        |_| {
            *asked.lock().unwrap() += 1;
            Ok(())
        },
    );
    assert_eq!(
        result,
        StopAsk::Stale {
            expected: "launch-a".into(),
            found: None
        }
    );
    assert_eq!(*asked.lock().unwrap(), 0);
}

/// The client is built from **the same hello that was checked**, which is what
/// makes "no renewal closure" a guarantee rather than a hope: there is no
/// second discovery to pick up another launch's authority.
#[test]
fn a_graceful_stop_asks_exactly_the_hello_it_checked() {
    let asked: Mutex<Vec<String>> = Mutex::new(Vec::new());
    let result = ask_the_checked_child_to_stop(
        "launch-a",
        || Ok(hello(Some("launch-a"), "instance-a")),
        |found| {
            asked.lock().unwrap().push(found.instance_id);
            Ok(())
        },
    );
    assert_eq!(result, StopAsk::Asked);
    assert_eq!(*asked.lock().unwrap(), vec!["instance-a".to_owned()]);
}

/// **Checking A and then reaching for authority again is forbidden**, and the
/// signature is what forbids it: `discover` is an `FnOnce`, so a second
/// discovery cannot be written. This drives it with a source that would hand
/// back a *different* launch on a second call and proves the first answer is
/// the one used.
#[test]
fn a_graceful_stop_reaches_for_authority_exactly_once() {
    let calls = std::cell::Cell::new(0u32);
    let asked: Mutex<Vec<String>> = Mutex::new(Vec::new());
    let discover = || {
        calls.set(calls.get() + 1);
        Ok(hello(Some("launch-a"), "instance-a"))
    };
    let result = ask_the_checked_child_to_stop("launch-a", discover, |found| {
        asked.lock().unwrap().push(found.instance_id);
        Ok(())
    });
    assert_eq!(result, StopAsk::Asked);
    assert_eq!(calls.get(), 1, "authority was re-acquired after the check");
    assert_eq!(*asked.lock().unwrap(), vec!["instance-a".to_owned()]);
}

/// A hello captured and checked before the replacement is equally acceptable:
/// the same function, driven by a value already in hand.
#[test]
fn a_hello_captured_before_the_replacement_is_still_a_valid_check() {
    let captured = hello(Some("launch-a"), "instance-a");
    let asked: Mutex<Vec<String>> = Mutex::new(Vec::new());
    let result = ask_the_checked_child_to_stop(
        "launch-a",
        move || Ok(captured),
        |found| {
            asked.lock().unwrap().push(found.instance_id);
            Ok(())
        },
    );
    assert_eq!(result, StopAsk::Asked);
    assert_eq!(*asked.lock().unwrap(), vec!["instance-a".to_owned()]);
}

/// **The regression case, with the side effect modelled rather than the
/// request counted.**
///
/// A graceful-stop callback was started for child A. C2 lets it outlive A, so
/// it is released only after A was killed on the stop deadline and an aborted
/// update resumed into a ready B. **The invariant is that no authorized
/// shutdown executes on B** — not that nothing reaches B. A test that reused
/// A's address for B would legitimately see an old-bearer request arrive and
/// be rejected, and that rejection is a correct outcome, so `B` here is a
/// little world that authorizes what it is sent: it executes a shutdown only
/// for a bearer it minted, and records anything else as rejected.
///
/// Two shapes, both named by the plan:
///
/// - *paused before discovery* — the callback discovers B, sees a launch that
///   is not the one its stop began for, and sends nothing;
/// - *A's checked hello already captured* — the callback has A's endpoint and
///   A's bearer, so it does send, and what it sends is refused by the
///   replacement listening at that address.
///
/// In both, `executed` is empty and B is still serving.
#[test]
fn a_stop_callback_released_after_its_child_died_executes_nothing_on_the_replacement() {
    /// The replacement, as a thing that can be acted on. `bearer` is the only
    /// credential it honours; anything else is a request it rejects.
    #[derive(Default)]
    struct Replacement {
        executed: Mutex<Vec<String>>,
        rejected: Mutex<Vec<String>>,
    }

    impl Replacement {
        /// What `POST /v1/shutdown` does: authorize, then act. The gateway's
        /// route authorizes a *scope* carried by a bearer, and checks no
        /// per-child secret — which is exactly why the caller has to be the
        /// one bound to its child.
        fn shutdown(&self, endpoint: &str, bearer: &str) -> Result<(), String> {
            if bearer == "bearer-b" {
                self.executed.lock().unwrap().push(endpoint.to_owned());
                return Ok(());
            }
            self.rejected.lock().unwrap().push(bearer.to_owned());
            Err("unauthenticated: the gateway bearer was not recognised".into())
        }
    }

    fn credentialed(
        launch: &str,
        instance: &str,
        bearer: &str,
    ) -> koru_api_discovery::GatewayHello {
        koru_api_discovery::GatewayHello {
            session: koru_api_discovery::SessionCredential::new(
                bearer.into(),
                now_millis() + 60 * 60 * 1_000,
            ),
            // **B deliberately reuses A's address.** The dangerous case is the
            // one where a stale request can physically arrive.
            api_endpoint: "http://127.0.0.1:4242".into(),
            ..hello(Some(launch), instance)
        }
    }

    let a = credentialed("launch-a", "instance-a", "bearer-a");
    let b = credentialed("launch-b", "instance-b", "bearer-b");

    // Shape one: paused before discovery. By the time it looks, B owns the
    // rendezvous, and the check refuses before any client is built.
    let world = Replacement::default();
    let result = ask_the_checked_child_to_stop(
        "launch-a",
        || Ok(b.clone()),
        |found| world.shutdown(&found.api_endpoint, found.session.expose()),
    );
    assert_eq!(
        result,
        StopAsk::Stale {
            expected: "launch-a".into(),
            found: Some("launch-b".into()),
        }
    );
    assert!(
        world.executed.lock().unwrap().is_empty(),
        "an authorized shutdown executed on the replacement"
    );
    assert!(
        world.rejected.lock().unwrap().is_empty(),
        "a request was sent to a gateway that had already been refused"
    );

    // Shape two: A's hello was captured and checked before the replacement, so
    // the callback does send — with A's bearer, to A's address, which B is now
    // listening on. The request arrives and is rejected, and **no authorized
    // shutdown executes on B**, which is the invariant.
    let world = Replacement::default();
    let result = ask_the_checked_child_to_stop(
        "launch-a",
        move || Ok(a),
        |found| world.shutdown(&found.api_endpoint, found.session.expose()),
    );
    assert!(
        matches!(result, StopAsk::Failed(ref why) if why.contains("unauthenticated")),
        "the replacement accepted a predecessor's bearer: {result:?}"
    );
    // **The invariant, stated on its own** rather than left implied by the
    // refusal above: bytes reached B and nothing was done to it.
    assert!(
        world.executed.lock().unwrap().is_empty(),
        "an authorized shutdown executed on the replacement"
    );
    assert_eq!(
        world.rejected.lock().unwrap().as_slice(),
        ["bearer-a".to_owned()],
        "the stale request did not arrive at the replacement's address at all — \
         the case is only exercised when it does"
    );
}

#[test]
fn a_gateway_that_publishes_nothing_is_asked_nothing() {
    let asked = Mutex::new(0u32);
    let result = ask_the_checked_child_to_stop(
        "launch-a",
        || {
            Err(koru_api_discovery::DiscoveryError::NotRunning(
                "no rendezvous".into(),
            ))
        },
        |_| {
            *asked.lock().unwrap() += 1;
            Ok(())
        },
    );
    assert_eq!(result, StopAsk::NotRunning);
    assert_eq!(*asked.lock().unwrap(), 0);
}

/// A refused or failed request is reported as a failure, never as a stop that
/// happened. The supervisor's kill is what follows.
#[test]
fn a_refused_shutdown_is_a_failure_not_a_stop() {
    let result = ask_the_checked_child_to_stop(
        "launch-a",
        || Ok(hello(Some("launch-a"), "instance-a")),
        |_| Err("403".into()),
    );
    assert_eq!(result, StopAsk::Failed("403".into()));
}

// --------------------------------- fencing property 2: a fenced role operation

/// The per-child handle is handed out only for the generation it belongs to.
/// This is **not** an HTTP stop authority; it is how an operation minted
/// against child A learns that B has taken over.
#[test]
fn a_role_operation_minted_against_a_replaced_child_is_refused() {
    let children = FakeChildren::ready("launch-a", 1);
    let supervisor = supervisor(&children);
    assert_eq!(
        supervisor.issue(Generation(1)).unwrap().launch_id,
        "launch-a"
    );

    children.edit(|script| {
        script.generation = Some(Generation(2));
        script.current = Some(Arc::new(child("launch-b")));
    });
    let stale = supervisor.issue(Generation(1)).expect_err("A is gone");
    assert_eq!(stale.requested, Generation(1));
    assert_eq!(stale.current, Some(Generation(2)));
    assert_eq!(
        supervisor.issue(Generation(2)).unwrap().launch_id,
        "launch-b"
    );
}

#[test]
fn a_role_operation_is_refused_when_no_child_is_serving() {
    let children = FakeChildren::in_state(State::Backoff);
    let supervisor = supervisor(&children);
    let stale = supervisor
        .issue(Generation(1))
        .expect_err("nothing serving");
    assert_eq!(stale.current, None);
}

// ------------------------- fencing property 1: an owner stop is not fenced

/// `hold` and the stops mean *this service should not be running*. Stopping
/// whatever child is current is correct **even when it replaced the one the
/// caller observed** — so the request carries no generation, and the report
/// names the generation it found rather than one it was told.
#[test]
fn an_owner_stop_ends_whichever_child_is_current() {
    let children = FakeChildren::ready("launch-a", 1);
    let supervisor = supervisor(&children);
    // The caller observed A.
    assert_eq!(
        supervisor.issue(Generation(1)).unwrap().launch_id,
        "launch-a"
    );
    // A crashed and B replaced it before the quit arrived.
    children.edit(|script| {
        script.generation = Some(Generation(2));
        script.current = Some(Arc::new(child("launch-b")));
    });

    let report = supervisor.stop_for_quit();
    assert_eq!(report.operation, StopOperation::Quit);
    assert_eq!(
        report.generation(),
        Some(Generation(2)),
        "the stop described the child it observed at the moment it acted"
    );
    assert!(report.stopped());
    assert_eq!(children.edit(|script| script.stops), 1);
}

/// A stop report names its operation, so an update handoff's completion can
/// never be read off a quit's — or off a later snapshot of the same
/// supervision.
#[test]
fn a_stop_report_names_the_operation_and_generation_it_describes() {
    let children = FakeChildren::ready("launch-a", 7);
    let supervisor = supervisor(&children);
    let report = supervisor.hold_and_stop_for_update();
    assert_eq!(report.operation, StopOperation::UpdateHandoff);
    assert_eq!(report.generation(), Some(Generation(7)));
    assert!(report.to_string().contains("update handoff"));
    assert!(report.to_string().contains('7'));
    // The hold precedes the stop, or the stop races a respawn.
    assert_eq!(
        children.edit(|script| (script.holds, script.serving_stops)),
        (1, 1)
    );
}

/// An update holds; the install aborts; the service comes back. A held stop
/// that could not be resumed would leave the gateway parked for the rest of
/// the run.
#[test]
fn an_aborted_update_resumes_the_gateway() {
    let children = FakeChildren::ready("launch-a", 1);
    children.edit(|script| {
        script.on_resume.push((
            State::Ready,
            Some(Generation(2)),
            Some(Arc::new(child("launch-b"))),
        ));
    });
    let supervisor = supervisor(&children);
    supervisor.hold_and_stop_for_update();
    supervisor.resume();
    assert_eq!(children.edit(|script| script.resumes), 1);
    assert_eq!(
        supervisor.status().state,
        State::Ready,
        "an aborted update left the gateway stopped"
    );
}

// ------------------------------------------------------- the update gate

/// **A delayed Store completion blocks a successful update handoff.**
///
/// The gateway child exits 0 only after it has joined its Store worker and
/// found it clean, so that code is the Store's own confirmation carried across
/// a process boundary. A Store still finishing its last commit does not exit
/// in time, the supervisor kills it on the stop deadline, and the outcome says
/// so — which is what `hold_and_stop_gateway_for_update` refuses on. **An
/// update never installs over a Store that has not finished its last commit**,
/// and the image being free is not the same question.
#[test]
fn a_store_that_did_not_finish_is_never_reported_as_a_clean_stop() {
    let children = FakeChildren::ready("launch-a", 1);
    children.edit(|script| {
        script.next_outcome = Some(StopOutcome::Forced {
            request: StopRequest::Serving,
            generation: Generation(1),
            why: Forced::DeadlineElapsed,
        });
    });
    let report = supervisor(&children).hold_and_stop_for_update();
    assert!(
        report.image_is_free(),
        "the child was killed and reaped, so the image is free"
    );
    assert!(
        !report.was_graceful(),
        "a deadline kill was reported as a graceful stop"
    );
    assert!(
        !report.store_confirmed_its_shutdown(),
        "an install would have run over a Store that had not finished"
    );
    assert!(report.to_string().contains("killed on the stop deadline"));
}

/// A child that exited on request but whose Store did not confirm — exit code
/// 1, which is what the role writes when `Completion::is_clean` is false — is
/// equally not an install this may proceed on.
#[test]
fn a_non_zero_exit_is_never_reported_as_a_confirmed_store_shutdown() {
    let children = FakeChildren::ready("launch-a", 1);
    children.edit(|script| {
        script.next_outcome = Some(StopOutcome::Graceful {
            request: StopRequest::Serving,
            generation: Generation(1),
            code: Some(1),
        });
    });
    let report = supervisor(&children).hold_and_stop_for_update();
    assert!(report.was_graceful(), "the child did end on its own terms");
    assert!(
        !report.store_confirmed_its_shutdown(),
        "the child said its Store was not clean and the install went ahead"
    );
}

/// **A timeout is not permission to swap.** The supervision still owns a child
/// it has not reaped, so the image is not even free — and neither question
/// answers yes.
#[test]
fn a_timed_out_stop_is_not_permission_to_swap() {
    let children = FakeChildren::ready("launch-a", 1);
    children.edit(|script| {
        script.next_outcome = Some(StopOutcome::TimedOut {
            request: StopRequest::Serving,
            generation: Some(Generation(1)),
        });
    });
    let report = supervisor(&children).hold_and_stop_for_update();
    assert!(!report.image_is_free(), "a timeout freed the image");
    assert!(!report.store_confirmed_its_shutdown());
    assert!(report.to_string().contains("still owns it"));
}

/// **C1 at this boundary: absence of a child is never evidence of a clean
/// Store join.** A child that crashed inside the update window leaves nothing
/// to stop; the stop completes, carries the exit it found, and refuses the
/// install rather than reading the emptiness as success.
#[test]
fn a_child_that_died_inside_the_window_is_not_a_clean_store_shutdown() {
    let children = FakeChildren::ready("launch-a", 1);
    children.edit(|script| {
        script.next_outcome = Some(StopOutcome::NotRunning {
            request: StopRequest::Serving,
            generation: None,
            last_exit: Some(ExitReport {
                generation: Generation(1),
                code: Some(101),
            }),
        });
    });
    let report = supervisor(&children).hold_and_stop_for_update();
    assert!(
        report.image_is_free(),
        "there is no child holding the image"
    );
    assert!(!report.was_graceful());
    assert!(
        !report.store_confirmed_its_shutdown(),
        "a crash inside the update window was read as a clean shutdown"
    );
    assert!(report.to_string().contains("101"));
}

/// The ordinary case, so the gate is not simply always closed: the child was
/// asked, agreed, joined its Store and exited 0.
#[test]
fn a_child_that_exited_zero_is_the_one_answer_an_install_may_proceed_on() {
    let children = FakeChildren::ready("launch-a", 1);
    let report = supervisor(&children).hold_and_stop_for_update();
    assert!(report.was_graceful());
    assert!(report.store_confirmed_its_shutdown());
    assert_eq!(report.operation, StopOperation::UpdateHandoff);
}

/// The window is honest while it is open: an ensure inside an update hold is
/// told so rather than reviving the child the install is about to replace.
#[test]
fn an_ensure_inside_an_update_hold_refuses_rather_than_reviving() {
    let children = FakeChildren::ready("launch-a", 1);
    let supervisor = supervisor(&children);
    supervisor.hold_and_stop_for_update();
    let error = supervisor
        .ensure_within(PATIENCE, &serving(&|_| None))
        .expect_err("held");
    assert_eq!(error.code(), "gateway_unreachable");
    assert_eq!(
        children.edit(|script| script.resumes),
        0,
        "an ensure fought the update"
    );
}

// ------------------------------------------------------------------- ensure

/// **Concurrent ensure.** Eight threads ask at once; one readiness wait and
/// one discovery serve all of them, and every one gets the same answer.
#[test]
fn concurrent_ensures_are_coalesced_into_one_answer() {
    let children = FakeChildren::ready("launch-a", 1);
    let supervisor = supervisor(&children);
    let discoveries = AtomicU64::new(0);
    let discover = |_child: &GatewayChild| {
        discoveries.fetch_add(1, Ordering::Relaxed);
        Some(hello(Some("launch-a"), "instance-a"))
    };
    let boundary = serving(&discover);

    let answers = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let supervisor = &supervisor;
                let boundary = &boundary;
                scope.spawn(move || {
                    supervisor
                        .ensure_within(PATIENCE, boundary)
                        .map(|found| found.instance_id)
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>()
    });

    for answer in &answers {
        let found = answer.as_ref().expect("every consumer was served");
        assert_eq!(found.as_str(), "instance-a");
    }
    assert_eq!(
        discoveries.load(Ordering::Relaxed),
        1,
        "eight consumers each minted their own credential"
    );
}

/// A restart invalidates the coalesced answer: the next consumer is given the
/// **replacement's** record, never the predecessor's.
#[test]
fn a_restart_invalidates_the_coalesced_answer() {
    let children = FakeChildren::ready("launch-a", 1);
    let supervisor = supervisor(&children);
    let first = supervisor
        .ensure_within(
            PATIENCE,
            &serving(&|_| Some(hello(Some("launch-a"), "instance-a"))),
        )
        .unwrap();
    assert_eq!(first.instance_id, "instance-a");

    children.edit(|script| {
        script.generation = Some(Generation(2));
        script.current = Some(Arc::new(child("launch-b")));
    });
    let second = supervisor
        .ensure_within(
            PATIENCE,
            &serving(&|_| Some(hello(Some("launch-b"), "instance-b"))),
        )
        .unwrap();
    assert_eq!(second.instance_id, "instance-b");
}

/// **The generation is not enough on its own, and a real child taught this.**
///
/// A child's rendezvous closes when it exits; the supervisor learns of the exit
/// a moment later, when its waiter reaps the process. In that gap the cached
/// answer still describes the "current" generation and the endpoint behind it
/// is already gone. A consumer handed that record discovers nothing and has
/// been told everything is fine — which is exactly what
/// `a_replaced_gateway_child_rebuilds_the_private_bridge` caught against a real
/// process before this check existed.
#[test]
fn a_coalesced_answer_for_a_child_that_stopped_answering_is_not_handed_out() {
    let children = FakeChildren::ready("launch-a", 1);
    let supervisor = supervisor(&children);
    let discoveries = AtomicU64::new(0);
    let discover = |_child: &GatewayChild| {
        discoveries.fetch_add(1, Ordering::Relaxed);
        Some(hello(Some("launch-a"), "instance-a"))
    };
    supervisor
        .ensure_within(PATIENCE, &serving(&discover))
        .expect("serving");
    assert_eq!(discoveries.load(Ordering::Relaxed), 1);

    // The child exited. Its generation is still the current one until the
    // waiter reports the exit, but nothing answers at its endpoint.
    let gone = Boundary {
        still_there: &|_| false,
        discover: &|_| None,
    };
    supervisor
        .ensure_within(PATIENCE, &gone)
        .expect_err("a record for a child that stopped answering was handed out");
}

/// **A stale probe says nothing.** A record still naming the predecessor's
/// launch is not this child being ready; the ensure keeps waiting and then
/// refuses rather than handing back somebody else's endpoint.
#[test]
fn a_record_belonging_to_another_launch_is_never_answered_with() {
    let children = FakeChildren::ready("launch-b", 2);
    let supervisor = supervisor(&children);
    let error = supervisor
        .ensure_within(
            PATIENCE,
            &serving(&|_| Some(hello(Some("launch-a"), "instance-a"))),
        )
        .expect_err("the predecessor's record is not an answer");
    assert_eq!(error.code(), "gateway_unreachable");
}

/// **An unrelated discovered endpoint is not an adoptable worker.** A gateway
/// somebody else started publishes no supervisor launch id, and adopting it is
/// exactly what the deleted ensure loop used to do.
#[test]
fn an_unsupervised_endpoint_is_not_adopted() {
    let children = FakeChildren::ready("launch-a", 1);
    let supervisor = supervisor(&children);
    let error = supervisor
        .ensure_within(
            PATIENCE,
            &serving(&|_| Some(hello(None, "somebody-elses-gateway"))),
        )
        .expect_err("an unsupervised gateway is not this Agent's worker");
    assert_eq!(error.code(), "gateway_unreachable");
}

/// **Readiness failure.** A child that never publishes leaves the supervision
/// in `Starting`; the consumer is told it is not answering, and is told the
/// supervisor's own last error rather than a guess.
#[test]
fn a_child_that_never_becomes_ready_refuses_with_the_supervisors_reason() {
    let children = FakeChildren::in_state(State::Starting);
    children.edit(|script| {
        script.last_error = Some("worker was not ready in time: no rendezvous".into());
    });
    let supervisor = supervisor(&children);
    let error = supervisor
        .ensure_within(PATIENCE, &serving(&|_| None))
        .expect_err("never ready");
    assert_eq!(error.code(), "gateway_unreachable");
    assert!(error.to_string().contains("not ready in time"));
}

/// **An exhausted budget** is a launch failure with its count, not an endless
/// wait — and it is revived exactly once per ask, so a consumer asking again
/// is a decision to serve rather than a retry storm.
#[test]
fn an_exhausted_budget_is_revived_once_and_then_reported() {
    let children = FakeChildren::in_state(State::Exhausted);
    children.edit(|script| {
        script.respawns = 5;
        script.last_error = Some("the gateway child exited".into());
    });
    let supervisor = supervisor(&children);
    let error = supervisor
        .ensure_within(PATIENCE, &serving(&|_| None))
        .expect_err("exhausted");
    assert_eq!(error.code(), "gateway_launch_failed");
    assert!(error.to_string().contains('5'));
    assert_eq!(
        children.edit(|script| script.resumes),
        1,
        "an exhausted budget was retried in a loop"
    );
}

/// **Crash and restart, and an unrequested shutdown treated as an unexpected
/// exit.** A gateway that accepted `/v1/shutdown` from an authorized client —
/// idle or not — simply exits, and an exit nobody requested is a crash: the
/// supervision backs off, replaces the child, and the next consumer is served
/// by the replacement. Nothing about the exit is special-cased.
#[test]
fn an_exit_nobody_requested_is_recovered_from_like_any_crash() {
    let children = FakeChildren::ready("launch-a", 1);
    let supervisor = supervisor(&children);
    supervisor
        .ensure_within(
            PATIENCE,
            &serving(&|_| Some(hello(Some("launch-a"), "instance-a"))),
        )
        .expect("serving");

    // `/v1/shutdown?only_if_idle=true` was granted while the parent was alive.
    // The child exits; the supervisor reaps it and schedules a restart.
    children.edit(|script| {
        script.state = Some(State::Backoff);
        script.generation = None;
        script.current = None;
        script.respawns = 1;
        script.on_resume.clear();
    });
    // The backoff elapses and a replacement is serving.
    children.edit(|script| {
        script.state = Some(State::Ready);
        script.generation = Some(Generation(2));
        script.current = Some(Arc::new(child("launch-b")));
    });

    let recovered = supervisor
        .ensure_within(
            PATIENCE,
            &serving(&|_| Some(hello(Some("launch-b"), "instance-b"))),
        )
        .expect("the replacement serves");
    assert_eq!(recovered.instance_id, "instance-b");
    assert_eq!(
        children.edit(|script| script.stops + script.serving_stops),
        0,
        "an unrequested exit was mistaken for an owner-initiated stop"
    );
}

/// A quit is not undone by a consumer that was mid-ask: the person asked the
/// Agent to end, and reviving the service behind them would leave a process
/// nobody wants.
#[test]
fn an_ensure_racing_a_quit_does_not_revive_the_service() {
    let children = FakeChildren::ready("launch-a", 1);
    let supervisor = supervisor(&children);
    supervisor.stop_for_quit();
    let error = supervisor
        .ensure_within(PATIENCE, &serving(&|_| None))
        .expect_err("quitting");
    assert_eq!(error.code(), "gateway_unreachable");
    assert_eq!(children.edit(|script| script.resumes), 0);
}

// ------------------------------------------------- the launch payload's shape

/// Two children never share a launch id or a bridge capability. The
/// supervisor's generation is its own counter and is deliberately not smuggled
/// into either.
#[test]
fn every_child_gets_a_fresh_launch_identity() {
    let first = koru_network_bridge::BridgeCapability::generate();
    let second = koru_network_bridge::BridgeCapability::generate();
    assert_ne!(first, second);
    assert_ne!(
        uuid::Uuid::new_v4().to_string(),
        uuid::Uuid::new_v4().to_string()
    );
}

/// **The fingerprinted root is the *store* root**, which is the half the
/// gateway's own bootstrap validation checks. Fingerprinting the channel root
/// would refuse every child on a rooted Agent.
#[test]
fn the_launch_fingerprints_the_store_root_the_gateway_checks() {
    let paths = Paths::for_root(std::path::PathBuf::from("X:/never-created"));
    assert_eq!(
        koru_network_bridge::fingerprint(paths.store_dir().to_string_lossy().as_bytes()),
        koru_network_bridge::fingerprint(
            koru_channel::store_root_under(std::path::Path::new("X:/never-created"))
                .to_string_lossy()
                .as_bytes()
        )
    );
}

/// The role the supervisor names is the role `main` dispatches — the same
/// check `session_host` makes, and the only half of the pairing a type can
/// hold.
#[test]
fn the_worker_role_is_dispatched_by_this_executable() {
    assert_eq!(crate::gateway_worker::role().name, "gateway");
    let paths = Paths::for_root(std::path::PathBuf::from("X:/never-created"));
    assert_eq!(
        gateway_spec(&paths, koru_device::DeviceId::new()).role,
        "gateway"
    );
}

/// Startup does not wait: the spec's readiness patience is the supervisor's
/// and is far longer than the answer a consumer waits for, so a slow child is
/// reported as "not yet" rather than killed.
#[test]
fn a_consumer_waits_for_less_time_than_the_supervisor_gives_a_child() {
    let paths = Paths::for_root(std::path::PathBuf::from("X:/never-created"));
    let spec = gateway_spec(&paths, koru_device::DeviceId::new());
    assert!(
        ENSURE_DEADLINE < spec.ready_deadline,
        "a consumer would time out only after the supervisor had killed the child"
    );
    // And the defaults the mechanism carries are the ones this role wants.
    let RestartBudget { max_restarts, .. } = spec.restart_budget;
    assert!(max_restarts > 0);
    let Backoff { initial, max } = spec.backoff;
    assert!(initial < max);
}

/// The cached answer is dropped before its credential expires, so a consumer
/// that does use the bearer is never handed a dead one.
#[test]
fn a_coalesced_answer_is_dropped_before_its_credential_expires() {
    let now = now_millis();
    let fresh = Ensured {
        generation: Generation(1),
        child: Arc::new(child("launch-a")),
        hello: hello(Some("launch-a"), "instance-a"),
    };
    assert!(fresh.still_good(Some(Generation(1)), now));
    assert!(
        !fresh.still_good(Some(Generation(2)), now),
        "a predecessor's record was reused"
    );
    let expiring = Ensured {
        generation: Generation(1),
        child: Arc::new(child("launch-a")),
        hello: koru_api_discovery::GatewayHello {
            session: koru_api_discovery::SessionCredential::new("bearer".into(), now + 1_000),
            ..hello(Some("launch-a"), "instance-a")
        },
    };
    assert!(!expiring.still_good(Some(Generation(1)), now));
}
