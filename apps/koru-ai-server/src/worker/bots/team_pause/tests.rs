// SPDX-License-Identifier: MPL-2.0

//! M8 piece W, cohorts `m8_team_coordination` and `m8_team_races`.
//!
//! The real spawned worker over an in-memory ai.db, with a scripted owner
//! standing in for the delegate: it keeps one team value per run and applies
//! the delegate's own rule to it (an older epoch is `bot_team_stale`, the
//! same epoch with other content is `bot_team_conflict`). No Store, provider,
//! browser or network is opened.
use super::*;
use crate::worker::bots::events::recorder::{BotEventRecorder, assert_holds_none_of};
use crate::worker::bots::tests::{Authority, created, fixture};
use koru_delegate_api::bots::{BotCommand as D, BotDelegatePort, team_failure, team_stale};
use koru_delegate_core as d;
use std::sync::Mutex;

#[derive(Clone, Debug, PartialEq)]
enum Seen {
    Probe,
    Walk,
    Team(BotTeamBarrier),
    Sync(d::BotId),
}

struct Run {
    boot: DelegateBootId,
    /// What this run of the delegate holds; `None` is "not told".
    team: Option<BotTeamBarrier>,
    /// The Bots with unsettled work, for the per-Bot boot pass.
    bots: Vec<d::BotId>,
    seen: Vec<Seen>,
    /// The delegate does not answer a team value.
    away: bool,
    /// The delegate answers a team value with another one.
    garbled: bool,
    /// Hold the next team value until released.
    gate: Option<(SyncSender<()>, Receiver<()>)>,
}

/// The scripted delegate, and what every worker started over it published
/// (`bots.team_pause.changed`), kept across a restart of the worker.
struct Owner(Mutex<Run>, Arc<BotEventRecorder>);

impl Owner {
    fn new() -> Arc<Self> {
        Arc::new(Self(
            Mutex::new(Run {
                boot: DelegateBootId::mint(),
                team: None,
                bots: Vec::new(),
                seen: Vec::new(),
                away: false,
                garbled: false,
                gate: None,
            }),
            BotEventRecorder::new(),
        ))
    }
    fn with<T>(&self, work: impl FnOnce(&mut Run) -> T) -> T {
        work(&mut self.0.lock().unwrap())
    }
    fn held(&self) -> Option<BotTeamBarrier> {
        self.with(|run| run.team.clone())
    }
    fn seen(&self) -> Vec<Seen> {
        self.with(|run| run.seen.clone())
    }
    fn team_sends(&self) -> usize {
        self.seen()
            .iter()
            .filter(|seen| matches!(seen, Seen::Team(_)))
            .count()
    }
    /// A restart of the delegate: a new run that holds nothing in memory.
    fn restart(&self) {
        self.with(|run| {
            run.boot = DelegateBootId::mint();
            run.team = None;
            run.seen.clear();
        });
    }
}

impl BotDelegatePort for Owner {
    fn execute(&self, command: D) -> Result<BotReply, koru_delegate_api::Failure> {
        let away = || koru_delegate_api::Failure::new("delegate_unavailable", "away", true);
        if matches!(command, D::TeamBarrier { .. })
            && let Some((entered, release)) = self.with(|run| run.gate.take())
        {
            entered.send(()).unwrap();
            release.recv_timeout(Duration::from_secs(10)).unwrap();
        }
        let mut run = self.0.lock().unwrap();
        match command {
            D::Hello { owner, scan } => {
                let entries = match scan {
                    d::BotBootScan::Start {} => {
                        run.seen.push(Seen::Walk);
                        run.bots
                            .iter()
                            .enumerate()
                            .map(|(index, bot)| d::BotBootEntry {
                                identity: d::BotBootIdentity {
                                    owner,
                                    bot: *bot,
                                    run: d::RunId::mint(),
                                    sequence: index as u64 + 1,
                                },
                                admission: d::BotBootAdmission::Missing {},
                            })
                            .collect()
                    }
                    _ => {
                        run.seen.push(Seen::Probe);
                        Vec::new()
                    }
                };
                Ok(BotReply::Hello {
                    hello: d::BotHello {
                        boot: run.boot,
                        entries,
                        next: None,
                    },
                })
            }
            D::TeamBarrier { barrier } => {
                run.seen.push(Seen::Team(barrier.clone()));
                if run.away {
                    return Err(away());
                }
                if run.garbled {
                    let mut other = barrier;
                    other.epoch += 1;
                    other.request = Some(BotRequestId::mint());
                    return Ok(BotReply::TeamBarrier { barrier: other });
                }
                match &run.team {
                    Some(held) if barrier.epoch < held.epoch => Err(team_stale(held.epoch)),
                    Some(held) if barrier.epoch == held.epoch && *held != barrier => {
                        Err(team_failure("bot_team_conflict").unwrap())
                    }
                    _ => {
                        run.team = Some(barrier.clone());
                        Ok(BotReply::TeamBarrier { barrier })
                    }
                }
            }
            D::SynchronizeBootBot {
                boot,
                barrier,
                policy,
            } => {
                // A boot pair of an older run is refused, as a delegate does.
                if boot != run.boot {
                    return Err(koru_delegate_api::Failure::new(
                        "bot_boot_stale",
                        "another delegate run",
                        false,
                    ));
                }
                run.seen.push(Seen::Sync(barrier.bot));
                Ok(BotReply::BootSynchronized {
                    receipt: d::BotBootReceipt {
                        boot,
                        barrier,
                        policy: policy.key,
                        config_revision: policy.config_revision,
                        policy_revision: policy.policy_revision,
                        state: d::BotBootState::Ready,
                    },
                })
            }
            // A Bot's own pause or resume is taken as it is sent.
            D::Barrier { barrier } => Ok(BotReply::Barrier { barrier }),
            _ => Err(away()),
        }
    }
}

struct Fixture {
    worker: WorkerHandle,
    owner: Arc<Owner>,
    bot: Bot,
    team: BotOwner,
    authority: Arc<Authority>,
    returned: Receiver<Repository>,
}

fn spawn(
    repository: Repository,
    authority: Arc<Authority>,
    owner: Arc<Owner>,
) -> (WorkerHandle, Receiver<Repository>) {
    let (return_tx, returned) = sync_channel(1);
    let mut config = WorkerConfig::new(
        PathBuf::from("unused-team-pause"),
        "internal-debug",
        ProviderRegistry::default(),
    )
    .with_bot_authority(authority)
    .with_bot_events(owner.1.clone())
    .with_bot_delegate(owner);
    config.test_repository = Some(repository);
    config.test_repository_return = Some(return_tx);
    config.test_bot_runtime_ready = true;
    (AiWorker::new(config).spawn().unwrap(), returned)
}

/// A worker with one Bot whose work the delegate lists as unsettled, so each
/// boot pass synchronizes it. `seed` writes the ai.db a previous worker left.
fn start(owner: Arc<Owner>, seed: impl FnOnce(&mut Repository, BotOwner)) -> Fixture {
    let (mut repository, authority, create) = fixture();
    let bot = created(&mut repository, authority.clone(), create);
    let team = authority.owner().unwrap();
    seed(&mut repository, team);
    owner.with(|run| run.bots = vec![bot.id]);
    let (worker, returned) = spawn(repository, authority.clone(), owner.clone());
    Fixture {
        worker,
        owner,
        bot,
        team,
        authority,
        returned,
    }
}

fn context() -> RequestContext {
    RequestContext {
        principal: koru_ai_api::AiPrincipal {
            label: "team-fixture".into(),
            scopes: vec![AiScope::Chat, AiScope::ManageBots],
        },
        trace_id: "team-fixture".into(),
        deadline_millis: None,
    }
}

fn until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn delegate_owner(owner: BotOwner) -> d::BotOwner {
    d::BotOwner {
        user_scope: owner.user_scope,
        device: owner.device,
    }
}

fn fingerprint(n: u8) -> String {
    format!("{n:02x}").repeat(32)
}

impl Fixture {
    fn client(&self) -> AiWorkerClient {
        self.worker.client()
    }
    fn command(&self, command: BotCommand) -> ApiResult<BotResponse> {
        self.client().bots(context(), command)
    }
    fn view(&self) -> TeamPauseView {
        match self.command(BotCommand::ReadTeamPause {}).unwrap() {
            BotResponse::TeamPause { team } => team,
            _ => panic!("team_pause"),
        }
    }
    fn change(&self, paused: bool, expected: u64) -> ApiResult<TeamPauseView> {
        let request_id = BotRequestId::mint();
        let command = if paused {
            BotCommand::PauseTeam {
                request_id,
                expected_team_revision: expected,
            }
        } else {
            BotCommand::ResumeTeam {
                request_id,
                expected_team_revision: expected,
            }
        };
        self.command(command).map(|response| match response {
            BotResponse::TeamPause { team } => team,
            _ => panic!("team_pause"),
        })
    }
    fn acknowledged(&self, revision: u64, paused: bool) {
        until("the acknowledgement", || {
            let view = self.view();
            view.revision == revision
                && view.paused == paused
                && view.stage == TeamPauseStage::Acknowledged
        });
        assert_eq!(self.view().pending_since_ms, None);
    }
    fn on_writer<T: Send + 'static>(
        &self,
        work: impl FnOnce(&mut WorkerState) -> T + Send + 'static,
    ) -> T {
        let (reply, answer) = sync_channel(1);
        self.client()
            .sender
            .send(Envelope::WithStateForFixture(Box::new(move |state| {
                let _ = reply.send(work(state));
            })))
            .unwrap();
        answer.recv_timeout(Duration::from_secs(10)).unwrap()
    }
    fn bot(&self) -> Bot {
        match self
            .command(BotCommand::Read {
                bot_id: self.bot.id,
            })
            .unwrap()
        {
            BotResponse::Bot { bot } => bot,
            _ => panic!("bot"),
        }
    }
    fn stop(self) -> Repository {
        self.worker.shutdown();
        assert_eq!(self.worker.join(), WorkerExit::Clean);
        self.returned
            .recv_timeout(Duration::from_secs(10))
            .expect("the worker hands its repository back")
    }
}

/// The team value was sent to this run after a `Hello` showed it and before
/// any Bot of it was synchronized.
fn told_before_the_per_bot_pass(seen: &[Seen]) -> bool {
    let team = seen.iter().position(|seen| matches!(seen, Seen::Team(_)));
    let sync = seen.iter().position(|seen| matches!(seen, Seen::Sync(_)));
    let hello = seen
        .iter()
        .position(|seen| matches!(seen, Seen::Probe | Seen::Walk));
    matches!((hello, team, sync), (Some(hello), Some(team), Some(sync)) if hello < team && team < sync)
}

/// `m8_team_coordination`, worker start: a team with no row is sent as
/// "never paused" after the first `Hello` and before the per-Bot pass, so
/// the closed delegate opens and its Bots are then synchronized.
#[test]
fn m8_team_coordination_a_worker_start_tells_never_paused_before_the_per_bot_pass() {
    let f = start(Owner::new(), |_, _| {});
    let never = BotTeamBarrier::never_paused(delegate_owner(f.team));
    until("the per-Bot pass", || {
        f.owner
            .seen()
            .iter()
            .any(|seen| *seen == Seen::Sync(f.bot.id))
    });
    assert!(told_before_the_per_bot_pass(&f.owner.seen()));
    assert_eq!(f.owner.held(), Some(never.clone()));
    // Told once: nothing is owed and nothing is sent again.
    assert!(f.on_writer(|state| state.bot_boot.team.told() && !state.bot_boot.team.owed()));
    assert_eq!(f.owner.team_sends(), 1);
    // A team never paused reads as revision 0, acknowledged, and no row.
    let view = f.view();
    assert_eq!(
        (view.revision, view.paused, view.stage),
        (0, false, TeamPauseStage::Acknowledged)
    );
    let repository = f.stop();
    assert!(
        repository
            .team_pause(BotOwner {
                user_scope: never.owner.user_scope,
                device: never.owner.device
            })
            .unwrap()
            .is_none()
    );
}

/// `m8_team_coordination`: the row is committed first and answered
/// `pending` while the delegate has not answered; only an echo of the same
/// value acknowledges it.
#[test]
fn m8_team_coordination_a_pause_is_sent_after_the_commit_and_acknowledged_only_on_an_equal_reply() {
    let f = start(Owner::new(), |_, _| {});
    until("the first value", || f.owner.held().is_some());
    let (entered_tx, entered) = sync_channel(1);
    let (release, release_rx) = sync_channel(1);
    f.owner.with(|run| {
        run.gate = Some((entered_tx, release_rx));
        run.garbled = true;
    });
    let view = f.change(true, 0).unwrap();
    assert_eq!(
        (view.revision, view.paused, view.stage),
        (1, true, TeamPauseStage::Pending)
    );
    assert!(view.pending_since_ms.is_some());
    assert_eq!(view.owner_device, f.team.device);
    // The delegate is being told and has not answered: the answer above did
    // not wait for it, and the AI side already refuses new work.
    entered.recv_timeout(Duration::from_secs(10)).unwrap();
    let team = f.team;
    assert!(f.on_writer(move |state| state.team_paused(team).unwrap()));
    assert!(!f.on_writer({
        let bot = f.bot.id;
        move |state| state.repository.bot_admitting(bot).unwrap()
    }));
    assert_eq!(f.view().stage, TeamPauseStage::Pending);
    // An answer that is not the value sent acknowledges nothing.
    release.send(()).unwrap();
    until("the garbled answer", || {
        f.on_writer(|state| state.bot_boot.team.last_answer() == Some(TEAM_UNEXPECTED))
    });
    assert_eq!(f.view().stage, TeamPauseStage::Pending);
    assert!(f.on_writer(|state| state.bot_boot.team.owed()));
    // The same value, echoed whole, does.
    f.owner.with(|run| run.garbled = false);
    f.acknowledged(1, true);
    let held = f.owner.held().unwrap();
    assert_eq!((held.epoch, held.paused), (1, true));
    assert!(f.on_writer(
        |state| !state.bot_boot.team.owed() && state.bot_boot.team.last_answer().is_none()
    ));
}

/// `m8_team_coordination`: while the delegate is away the value is sent
/// again and again, the row stays `pending`, and the AI side refuses
/// meanwhile. Silence is never read as done.
#[test]
fn m8_team_coordination_is_retried_while_the_delegate_is_away() {
    let f = start(Owner::new(), |_, _| {});
    until("the first value", || f.owner.held().is_some());
    let before = f.owner.team_sends();
    f.owner.with(|run| run.away = true);
    let view = f.change(true, 0).unwrap();
    assert_eq!(view.stage, TeamPauseStage::Pending);
    until("three sends to a delegate that is away", || {
        f.owner.team_sends() >= before + 3
    });
    let view = f.view();
    assert_eq!(
        (view.revision, view.paused, view.stage),
        (1, true, TeamPauseStage::Pending)
    );
    let team = f.team;
    assert!(f.on_writer(move |state| state.team_paused(team).unwrap()));
    assert!(f.on_writer(|state| state.bot_boot.team.last_answer() == Some("delegate_unavailable")));
    // The delegate still holds what it held: it was never told the pause.
    assert_eq!(f.owner.held().map(|held| held.epoch), Some(0));
    f.owner.with(|run| run.away = false);
    f.acknowledged(1, true);
    assert_eq!(
        f.owner.held().map(|held| (held.epoch, held.paused)),
        Some((1, true))
    );
}

/// `m8_team_coordination`, worker restart: the row is in ai.db, so a new
/// worker refuses new work from its first read and sends the row again,
/// before the per-Bot pass. Shown with the delegate kept running and with
/// the delegate restarted too, for a paused and for a resumed team.
#[test]
fn m8_team_coordination_a_worker_restart_sends_the_row_again() {
    for (paused, delegate_restarted) in [(true, false), (true, true), (false, false), (false, true)]
    {
        let owner = Owner::new();
        let f = start(owner.clone(), |_, _| {});
        f.change(true, 0).unwrap();
        f.acknowledged(1, true);
        let mut revision = 1;
        if !paused {
            f.change(false, 1).unwrap();
            f.acknowledged(2, false);
            revision = 2;
        }
        let (bot, team, authority) = (f.bot.clone(), f.team, f.authority.clone());
        let repository = f.stop();
        let row = repository.team_pause(team).unwrap().unwrap();
        assert_eq!((row.revision, row.paused), (revision, paused));

        // The worker process is gone. The delegate kept its value, or it
        // restarted as well and holds nothing.
        if delegate_restarted {
            owner.restart();
        } else {
            owner.with(|run| run.seen.clear());
        }
        let (worker, returned) = spawn(repository, authority.clone(), owner.clone());
        let f = Fixture {
            worker,
            owner: owner.clone(),
            bot,
            team,
            authority,
            returned,
        };
        until("the per-Bot pass of the new worker", || {
            f.owner
                .seen()
                .iter()
                .any(|seen| *seen == Seen::Sync(f.bot.id))
        });
        let case = format!("paused={paused} delegate_restarted={delegate_restarted}");
        assert!(told_before_the_per_bot_pass(&f.owner.seen()), "{case}");
        assert_eq!(f.owner.held(), Some(row.barrier()), "{case}");
        f.acknowledged(revision, paused);
        let admitting = f.on_writer({
            let bot = f.bot.id;
            move |state| state.repository.bot_admitting(bot).unwrap()
        });
        assert_eq!(admitting, !paused, "{case}");
        f.stop();
    }
}

/// `m8_team_coordination`, delegate restart: a new delegate run holds
/// nothing and is closed. The worker's next `Hello` shows the new run and
/// the row is sent again before that run's per-Bot pass, for a team never
/// paused, a paused one and a resumed one. A Bot never waits on a run that
/// was not told.
#[test]
fn m8_team_coordination_a_new_delegate_run_is_told_again_before_its_per_bot_pass() {
    for state in ["never", "paused", "resumed"] {
        let f = start(Owner::new(), |_, _| {});
        until("the first value", || f.owner.held().is_some());
        let mut expected = BotTeamBarrier::never_paused(delegate_owner(f.team));
        if state != "never" {
            f.change(true, 0).unwrap();
            f.acknowledged(1, true);
            if state == "resumed" {
                f.change(false, 1).unwrap();
                f.acknowledged(2, false);
            }
            expected = f.owner.held().unwrap();
        }
        f.owner.restart();
        assert!(
            f.owner.held().is_none(),
            "{state}: the new run was not told"
        );
        // The worker notices the new run on its next `Hello`: here asked for
        // at once, in the product on the 32 second probe or after any change.
        f.on_writer(|state| state.request_bot_boot_refresh());
        until("the per-Bot pass of the new run", || {
            f.owner
                .seen()
                .iter()
                .any(|seen| *seen == Seen::Sync(f.bot.id))
        });
        assert!(told_before_the_per_bot_pass(&f.owner.seen()), "{state}");
        assert_eq!(f.owner.held(), Some(expected), "{state}");
        assert!(f.on_writer(|state| state.bot_boot.team.told() && !state.bot_boot.team.owed()));
        f.stop();
    }
}

/// `m8_team_coordination`: a first send that fails does not hold the
/// per-Bot pass for ever, and it is still owed and sent again. Until the
/// echo the delegate holds every Bot of the owner anyway.
#[test]
fn m8_team_coordination_a_failed_first_send_is_repeated_beside_the_per_bot_pass() {
    let owner = Owner::new();
    owner.with(|run| run.away = true);
    let f = start(owner, |_, _| {});
    until("the per-Bot pass", || {
        f.owner
            .seen()
            .iter()
            .any(|seen| *seen == Seen::Sync(f.bot.id))
    });
    assert!(told_before_the_per_bot_pass(&f.owner.seen()));
    assert!(f.owner.held().is_none());
    assert!(f.on_writer(|state| !state.bot_boot.team.told() && state.bot_boot.team.owed()));
    f.owner.with(|run| run.away = false);
    until("the value", || f.owner.held().is_some());
    until("told", || f.on_writer(|state| state.bot_boot.team.told()));
}

/// `m8_team_coordination`, a stale epoch after a restore: the delegate
/// holds a newer epoch than this ai.db. The worker writes its row at that
/// epoch plus one, keeping its own `paused`, and sends again, so the team
/// cannot stay held behind a row that says it is not paused. The same for
/// another value at the same epoch.
#[test]
fn m8_team_coordination_a_stale_or_conflicting_epoch_is_rewritten_past_the_held_one() {
    // No row here; the delegate holds epoch 5, paused.
    let owner = Owner::new();
    let f = start(owner.clone(), |_, team| {
        owner.with(|run| {
            run.team = Some(BotTeamBarrier {
                owner: delegate_owner(team),
                epoch: 5,
                paused: true,
                request: Some(BotRequestId::mint()),
            })
        });
    });
    f.acknowledged(6, false);
    assert_eq!(
        f.owner.held().map(|held| (held.epoch, held.paused)),
        Some((6, false))
    );

    // A row at epoch 1, paused; the delegate holds another epoch 1.
    let owner = Owner::new();
    let f = start(owner.clone(), |repository, team| {
        repository
            .change_team_pause(team, BotRequestId::mint(), &fingerprint(7), 0, true, 10)
            .unwrap();
        owner.with(|run| {
            run.team = Some(BotTeamBarrier {
                owner: delegate_owner(team),
                epoch: 1,
                paused: false,
                request: Some(BotRequestId::mint()),
            })
        });
    });
    f.acknowledged(2, true);
    assert_eq!(
        f.owner.held().map(|held| (held.epoch, held.paused)),
        Some((2, true))
    );
    // A retry of the request the rewrite replaced reads again.
    assert_eq!(
        f.change(true, 0).unwrap_err().code,
        koru_ai_wire::bot_team_codes::TEAM_REVISION_CONFLICT
    );
}

/// `m8_team_races`: pause, then resume before the first is acknowledged.
/// The second has the next epoch and the delegate ends at the newest one.
#[test]
fn m8_team_races_pause_then_resume_before_the_first_is_acknowledged() {
    let f = start(Owner::new(), |_, _| {});
    until("the first value", || f.owner.held().is_some());
    let (entered_tx, entered) = sync_channel(1);
    let (release, release_rx) = sync_channel(1);
    f.owner
        .with(|run| run.gate = Some((entered_tx, release_rx)));
    assert_eq!(f.change(true, 0).unwrap().revision, 1);
    entered.recv_timeout(Duration::from_secs(10)).unwrap();
    let view = f.change(false, 1).unwrap();
    assert_eq!(
        (view.revision, view.paused, view.stage),
        (2, false, TeamPauseStage::Pending)
    );
    release.send(()).unwrap();
    f.acknowledged(2, false);
    assert_eq!(
        f.owner.held().map(|held| (held.epoch, held.paused)),
        Some((2, false))
    );
    // Both values were sent, in order; the echo of the first acknowledged
    // nothing, because the row had moved.
    let epochs: Vec<_> = f
        .owner
        .seen()
        .into_iter()
        .filter_map(|seen| match seen {
            Seen::Team(barrier) if barrier.epoch > 0 => Some(barrier.epoch),
            _ => None,
        })
        .collect();
    assert_eq!(epochs, [1, 2]);
}

/// `m8_team_races`: two windows with the same expected revision. One wins;
/// the other gets `team_revision_conflict` and reads again. A repeat of the
/// winner's own request is answered from the row, and the same id with other
/// content is `request_conflict`.
#[test]
fn m8_team_races_two_windows_with_the_same_expected_revision() {
    let f = start(Owner::new(), |_, _| {});
    let first = BotRequestId::mint();
    let pause = |request_id| BotCommand::PauseTeam {
        request_id,
        expected_team_revision: 0,
    };
    let BotResponse::TeamPause { team: won } = f.command(pause(first)).unwrap() else {
        panic!("team_pause")
    };
    assert_eq!((won.revision, won.paused), (1, true));
    let lost = f.command(pause(BotRequestId::mint())).unwrap_err();
    assert_eq!(
        lost.code,
        koru_ai_wire::bot_team_codes::TEAM_REVISION_CONFLICT
    );
    assert!(!lost.retryable);
    let lost = f
        .command(BotCommand::ResumeTeam {
            request_id: BotRequestId::mint(),
            expected_team_revision: 0,
        })
        .unwrap_err();
    assert_eq!(
        lost.code,
        koru_ai_wire::bot_team_codes::TEAM_REVISION_CONFLICT
    );
    // The winner's retry is answered from the row and writes nothing.
    let BotResponse::TeamPause { team: again } = f.command(pause(first)).unwrap() else {
        panic!("team_pause")
    };
    assert_eq!((again.revision, again.paused), (1, true));
    let conflict = f
        .command(BotCommand::ResumeTeam {
            request_id: first,
            expected_team_revision: 1,
        })
        .unwrap_err();
    assert_eq!(conflict.code, "request_conflict");
    // Pausing a team that is already paused answers the current state.
    let view = f.change(true, 1).unwrap();
    assert_eq!((view.revision, view.paused), (1, true));
    f.acknowledged(1, true);
    assert_eq!(
        f.owner
            .seen()
            .iter()
            .filter(|seen| matches!(seen, Seen::Team(barrier) if barrier.epoch == 1))
            .count(),
        1,
        "one change, one value sent"
    );
}

/// `m8_team_races`: the team pause and a Bot's own pause are independent.
/// A Bot's own Pause and Resume are accepted while the team is paused; the
/// Bot stays held while either is paused; a team resume leaves a Bot that
/// was paused on its own paused; and no team change moves a Bot's revision.
#[test]
fn m8_team_races_a_bots_own_pause_and_the_team_pause_are_independent() {
    let f = start(Owner::new(), |_, _| {});
    let admitting = |f: &Fixture| {
        f.on_writer({
            let bot = f.bot.id;
            move |state| state.repository.bot_admitting(bot).unwrap()
        })
    };
    let own = |f: &Fixture, pause: bool| {
        let bot = f.bot();
        let request_id = BotRequestId::mint();
        let command = if pause {
            BotCommand::Pause {
                request_id,
                bot_id: bot.id,
                expected_revision: bot.revision,
            }
        } else {
            BotCommand::Resume {
                request_id,
                bot_id: bot.id,
                expected_revision: bot.revision,
            }
        };
        assert!(matches!(
            f.command(command).unwrap(),
            BotResponse::Operation { .. }
        ));
        until("the Bot's own change", || {
            let bot = f.bot();
            bot.paused == pause && bot.pending_change.is_none()
        });
    };
    assert!(admitting(&f));
    let revision = f.bot().revision;
    f.change(true, 0).unwrap();
    f.acknowledged(1, true);
    // No Bot's revision moved, and the Bot is not paused on its own.
    assert_eq!(f.bot().revision, revision);
    assert!(!f.bot().paused);
    assert!(!admitting(&f));
    // The Bot's own Pause, then its own Resume, under the team pause.
    own(&f, true);
    assert!(!admitting(&f));
    own(&f, false);
    assert!(!f.bot().paused);
    assert!(!admitting(&f), "held while the team is paused");
    // The Bot pauses on its own, then the team resumes: it stays paused.
    own(&f, true);
    let before = f.bot().revision;
    f.change(false, 1).unwrap();
    f.acknowledged(2, false);
    assert!(f.bot().paused);
    assert_eq!(f.bot().revision, before);
    assert!(!admitting(&f), "held by its own pause");
    own(&f, false);
    assert!(admitting(&f));
}

/// `m8_team_races`: creating and editing a Bot while the team is paused and
/// while a team change is in flight. Both are allowed, the edit does not
/// conflict, and the new Bot is held with the others.
#[test]
fn m8_team_races_a_bot_is_created_and_edited_while_the_team_is_paused() {
    let f = start(Owner::new(), |_, _| {});
    until("the first value", || f.owner.held().is_some());
    // The change is in flight: the delegate has not answered.
    let (entered_tx, entered) = sync_channel(1);
    let (release, release_rx) = sync_channel(1);
    f.owner
        .with(|run| run.gate = Some((entered_tx, release_rx)));
    f.change(true, 0).unwrap();
    entered.recv_timeout(Duration::from_secs(10)).unwrap();
    let mut config = f.bot.config.clone();
    config.name = "Edited under the team pause".into();
    let BotResponse::Bot { bot: edited } = f
        .command(BotCommand::Update {
            request_id: BotRequestId::mint(),
            bot_id: f.bot.id,
            expected_revision: f.bot.revision,
            config: config.clone(),
        })
        .unwrap()
    else {
        panic!("bot")
    };
    assert_eq!(edited.revision, f.bot.revision + 1);
    let mut second = config;
    second.name = "Created under the team pause".into();
    second.working_folder = if cfg!(windows) {
        "D:/isolated-second"
    } else {
        "/isolated-second"
    }
    .into();
    let BotResponse::Bot { bot: created } = f
        .command(BotCommand::Create {
            request_id: BotRequestId::mint(),
            bot_id: BotId::mint(),
            home_workspace: f.bot.home_workspace,
            config: second,
            access: BotAccess::Default,
            selected_pages: vec![],
        })
        .unwrap()
    else {
        panic!("bot")
    };
    assert!(!created.paused);
    release.send(()).unwrap();
    f.acknowledged(1, true);
    // Held, because the team value is per owner: nothing is admitted for it.
    let new = created.id;
    assert!(!f.on_writer(move |state| state.repository.bot_admitting(new).unwrap()));
    f.change(false, 1).unwrap();
    f.acknowledged(2, false);
    assert!(f.on_writer(move |state| state.repository.bot_admitting(new).unwrap()));
}

// Cohort `m8_events_content_free`: `bots.team_pause.changed`.
mod events;
