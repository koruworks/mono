// SPDX-License-Identifier: MPL-2.0

//! The state, the worker threads, and the drain.
//!
//! The same discipline as `koru_update::UpdateController`: every check and
//! every update runs on a thread of its own and reports over a channel, the
//! notifier is called *after* the send so a wake never drains nothing, and
//! [`Controller::tick`] on the owner's loop is the only place state moves.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crate::catalogue::{self, KnownCli, LatestSource, UpdateCommand};
use crate::latest;
use crate::report::{Activity, CliReport, CliStage, Report, Status};
use crate::run;
use crate::state::State;
use crate::version::Version;

/// After Koru's own update check (5 s), and after the windows have drawn:
/// nothing about a third party's release is urgent at startup.
const STARTUP_DELAY: Duration = Duration::from_secs(20);
/// Daily, like the stable channel's own check.
const AUTOMATIC_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
/// A `--version` that takes longer than this is not going to answer.
const VERSION_TIMEOUT: Duration = Duration::from_secs(10);
/// `grok update --check` reaches its channel; give it a network's worth.
const CHECK_TIMEOUT: Duration = Duration::from_secs(30);
/// An npm install of a large CLI on a slow link. Past this it is killed and
/// reported, rather than left running under a card that says "updating".
const UPDATE_TIMEOUT: Duration = Duration::from_secs(10 * 60);

type Notifier = Arc<dyn Fn() + Send + Sync>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChoiceError {
    /// Not a CLI this crate knows, or not one the last check found installed.
    NoSuchCli,
    /// An update is already running, or there is nothing to dismiss.
    WrongStage,
}

impl ChoiceError {
    /// The wire's stable spelling, the same way `koru-agent`'s update codes
    /// are: ASCII, branched on, never shown.
    pub fn code(self) -> &'static str {
        match self {
            Self::NoSuchCli => "no_such_cli",
            Self::WrongStage => "wrong_stage",
        }
    }
}

enum WorkerEvent {
    CheckDone(Vec<Status>),
    Progress {
        id: &'static str,
        line: String,
    },
    UpdateDone {
        id: &'static str,
        activity: Activity,
    },
}

struct Events {
    tx: Sender<WorkerEvent>,
    notify: Option<Notifier>,
}

impl Events {
    /// Send first, wake second — see `koru_update::controller::Events`.
    fn send(&self, event: WorkerEvent) {
        let _ = self.tx.send(event);
        if let Some(notify) = &self.notify {
            notify();
        }
    }
}

struct Shared {
    /// `None` under test, where nothing is written.
    state_file: Option<PathBuf>,
    state: State,
    user_agent: String,
    /// `None` until the first check has reported.
    statuses: Option<Vec<Status>>,
    activities: BTreeMap<&'static str, Activity>,
    worker: Receiver<WorkerEvent>,
    events_tx: Sender<WorkerEvent>,
    notify: Option<Notifier>,
    check_in_flight: Arc<AtomicBool>,
}

#[derive(Clone)]
pub struct Controller {
    shared: Arc<Mutex<Shared>>,
}

impl Controller {
    /// `state_file` is where dismissals are kept; `user_agent` names this
    /// build to the registry.
    pub fn new(state_file: PathBuf, user_agent: impl Into<String>) -> Self {
        let state = State::load(&state_file);
        Self::with_state(Some(state_file), state, user_agent.into())
    }

    /// No file: dismissals are kept for this process only and nothing is
    /// written. What a test holds, and what a rooted development Agent could.
    pub fn detached() -> Self {
        Self::with_state(None, State::default(), "koru-cli-updates".into())
    }

    fn with_state(state_file: Option<PathBuf>, state: State, user_agent: String) -> Self {
        let (events_tx, worker) = mpsc::channel();
        Self {
            shared: Arc::new(Mutex::new(Shared {
                state_file,
                state,
                user_agent,
                statuses: None,
                activities: BTreeMap::new(),
                worker,
                events_tx,
                notify: None,
                check_in_flight: Arc::new(AtomicBool::new(false)),
            })),
        }
    }

    /// Called after every worker event, from the worker's thread. The owner
    /// wakes its loop and calls [`Self::tick`].
    pub fn set_notifier(&self, notify: impl Fn() + Send + Sync + 'static) {
        self.lock().notify = Some(Arc::new(notify));
    }

    /// One wait, then one check a day. Call once.
    pub fn start_automatic(&self) {
        // `cfg!` rather than `#[cfg]`, for the reason `koru-update` gives: a
        // `cargo test` that reached the npm registry would reveal nothing.
        if cfg!(test) {
            return;
        }
        let controller = self.clone();
        let _ = thread::Builder::new()
            .name("koru-cli-updates-automatic".into())
            .spawn(move || {
                thread::sleep(STARTUP_DELAY);
                loop {
                    controller.check();
                    thread::sleep(AUTOMATIC_INTERVAL);
                }
            });
    }

    /// Check now. A check already running is left to finish.
    pub fn check(&self) {
        let (events, in_flight, user_agent) = {
            let shared = self.lock();
            if shared.check_in_flight.swap(true, Ordering::AcqRel) {
                return;
            }
            (
                shared.events(),
                Arc::clone(&shared.check_in_flight),
                shared.user_agent.clone(),
            )
        };
        let spawned = thread::Builder::new()
            .name("koru-cli-updates-check".into())
            .spawn(move || {
                let statuses = check_all(&user_agent);
                in_flight.store(false, Ordering::Release);
                events.send(WorkerEvent::CheckDone(statuses));
            });
        if spawned.is_err() {
            self.lock().check_in_flight.store(false, Ordering::Release);
        }
    }

    /// Run `id`'s updater. Refused while one is running for it.
    pub fn update(&self, id: &str) -> Result<(), ChoiceError> {
        let (cli, before, events) = {
            let mut shared = self.lock();
            let cli = shared.installed(id).ok_or(ChoiceError::NoSuchCli)?;
            if matches!(
                shared.activities.get(cli.id),
                Some(Activity::Updating { .. })
            ) {
                return Err(ChoiceError::WrongStage);
            }
            let before = shared
                .status(cli.id)
                .and_then(|status| status.installed.clone());
            shared
                .activities
                .insert(cli.id, Activity::Updating { last_line: None });
            (cli, before, shared.events())
        };
        let spawned = thread::Builder::new()
            .name(format!("koru-cli-updates-{}", cli.id))
            .spawn(move || {
                let activity = run_update(cli, before.as_ref(), |line| {
                    events.send(WorkerEvent::Progress {
                        id: cli.id,
                        line: line.to_owned(),
                    });
                });
                events.send(WorkerEvent::UpdateDone {
                    id: cli.id,
                    activity,
                });
            });
        if spawned.is_err() {
            self.lock().activities.remove(cli.id);
            return Err(ChoiceError::WrongStage);
        }
        Ok(())
    }

    /// Close `id`'s card: an offer is remembered as declined for that version,
    /// a finished or failed run is simply put away.
    pub fn dismiss(&self, id: &str) -> Result<(), ChoiceError> {
        let mut shared = self.lock();
        let cli = shared.installed(id).ok_or(ChoiceError::NoSuchCli)?;
        match shared.activities.get(cli.id).unwrap_or(&Activity::Idle) {
            Activity::Updating { .. } => Err(ChoiceError::WrongStage),
            Activity::Updated { .. } | Activity::Failed { .. } => {
                shared.activities.remove(cli.id);
                Ok(())
            }
            Activity::Idle => {
                let latest = shared
                    .status(cli.id)
                    .and_then(|status| status.latest.as_ref().ok())
                    .map(ToString::to_string)
                    .ok_or(ChoiceError::WrongStage)?;
                shared.state.dismissed.insert(cli.id.to_owned(), latest);
                shared.save_state();
                Ok(())
            }
        }
    }

    /// Drain the workers. Non-blocking; `true` when the report changed.
    pub fn tick(&self) -> bool {
        let mut shared = self.lock();
        let mut changed = false;
        while let Ok(event) = shared.worker.try_recv() {
            shared.apply(event);
            changed = true;
        }
        changed
    }

    pub fn report(&self) -> Report {
        self.lock().report()
    }

    #[cfg(test)]
    pub(crate) fn inject_check(&self, statuses: Vec<Status>) {
        self.lock().apply(WorkerEvent::CheckDone(statuses));
    }

    #[cfg(test)]
    pub(crate) fn inject_progress(&self, id: &'static str, line: &str) {
        self.lock().apply(WorkerEvent::Progress {
            id,
            line: line.to_owned(),
        });
    }

    #[cfg(test)]
    pub(crate) fn inject_done(&self, id: &'static str, activity: Activity) {
        self.lock().apply(WorkerEvent::UpdateDone { id, activity });
    }

    #[cfg(test)]
    pub(crate) fn dismissed(&self) -> BTreeMap<String, String> {
        self.lock().state.dismissed.clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Shared> {
        self.shared.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Shared {
    fn events(&self) -> Events {
        Events {
            tx: self.events_tx.clone(),
            notify: self.notify.clone(),
        }
    }

    fn status(&self, id: &str) -> Option<&Status> {
        self.statuses
            .as_deref()?
            .iter()
            .find(|status| status.cli.id == id)
    }

    fn installed(&self, id: &str) -> Option<&'static KnownCli> {
        self.status(id).map(|status| status.cli)
    }

    fn save_state(&self) {
        if let Some(path) = &self.state_file {
            let _ = self.state.save(path);
        }
    }

    fn apply(&mut self, event: WorkerEvent) {
        match event {
            WorkerEvent::CheckDone(statuses) => {
                // A CLI that was uninstalled between checks takes its activity
                // with it; one that is still there keeps a run in progress.
                self.activities
                    .retain(|id, _| statuses.iter().any(|status| status.cli.id == *id));
                self.statuses = Some(statuses);
            }
            WorkerEvent::Progress { id, line } => {
                if let Some(Activity::Updating { last_line }) = self.activities.get_mut(id)
                    && !line.trim().is_empty()
                {
                    *last_line = Some(line);
                }
            }
            WorkerEvent::UpdateDone { id, activity } => {
                if let Activity::Updated { version } = &activity
                    && let Some(status) = self
                        .statuses
                        .as_deref_mut()
                        .and_then(|s| s.iter_mut().find(|status| status.cli.id == id))
                {
                    // The installed fact moves with the install, so a later
                    // check that fails to read the registry still compares
                    // against the right version.
                    status.installed = Some(version.clone());
                }
                self.activities.insert(id, activity);
            }
        }
    }

    fn report(&self) -> Report {
        let Some(statuses) = &self.statuses else {
            return Report::default();
        };
        Report {
            checked: true,
            clis: statuses
                .iter()
                .map(|status| {
                    let activity = self
                        .activities
                        .get(status.cli.id)
                        .cloned()
                        .unwrap_or_default();
                    crate::report::row(status, &activity, self.state.dismissed(status.cli.id))
                })
                .collect(),
        }
    }
}

/// The rows a surface draws cards for: everything but up to date.
pub fn attention(report: &Report) -> impl Iterator<Item = &CliReport> {
    report
        .clis
        .iter()
        .filter(|row| row.stage != CliStage::UpToDate)
}

// ------------------------------------------------------------ the workers

fn check_all(user_agent: &str) -> Vec<Status> {
    catalogue::KNOWN
        .iter()
        .filter_map(|cli| {
            let program = koru_session_profiles::resolve_executable(cli.program)?;
            Some(check_one(cli, program.into_os_string(), user_agent))
        })
        .collect()
}

fn check_one(cli: &'static KnownCli, program: OsString, user_agent: &str) -> Status {
    let installed = run::run(&program, &["--version"], VERSION_TIMEOUT, |_| {})
        .ok()
        .and_then(|finished| Version::first_in(&finished.detail()));
    let latest = match cli.latest {
        LatestSource::Npm(package) => latest::fetch_npm_latest(package, user_agent),
        LatestSource::GrokCheck => run::run(
            &program,
            &["update", "--check", "--json"],
            CHECK_TIMEOUT,
            |_| {},
        )
        .map_err(|error| error.to_string())
        .and_then(|finished| latest::parse_grok_check(&finished.detail()))
        .map(|check| check.latest),
    };
    Status {
        cli,
        installed,
        latest,
    }
}

/// The program and arguments that update `cli`, before PATH resolution.
pub fn update_argv(cli: &KnownCli) -> (&'static str, Vec<String>) {
    match cli.update {
        UpdateCommand::SelfUpdate(args) => (
            cli.program,
            args.iter().map(|arg| (*arg).to_owned()).collect(),
        ),
        UpdateCommand::NpmInstall(package) => (
            "npm",
            vec!["install".into(), "-g".into(), format!("{package}@latest")],
        ),
    }
}

/// `before` is what `--version` said at the last check, so an updater that
/// exits clean having changed nothing is not reported as an update.
fn run_update(
    cli: &'static KnownCli,
    before: Option<&Version>,
    on_line: impl FnMut(&str),
) -> Activity {
    let (program, args) = update_argv(cli);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let Some(program) = koru_session_profiles::resolve_executable(program) else {
        return Activity::Failed {
            message: Some(format!("{program} is not on PATH")),
            detail: format!("{program} did not resolve"),
        };
    };
    let finished = match run::run(program.as_os_str(), &args, UPDATE_TIMEOUT, on_line) {
        Ok(finished) => finished,
        Err(error) => {
            return Activity::Failed {
                message: Some(error.to_string()),
                detail: format!("spawn {}: {error}", program.display()),
            };
        }
    };
    if !finished.success {
        let status = if finished.timed_out {
            "timed out".to_owned()
        } else {
            match finished.exit_code {
                Some(code) => format!("exit {code}"),
                None => "stopped by a signal".to_owned(),
            }
        };
        return Activity::Failed {
            message: finished.last_line().map(str::to_owned),
            detail: format!("{status}\n{}", finished.detail()),
        };
    }
    // What the CLI says it is now, read from the CLI rather than assumed from
    // a zero exit — an updater that decided not to update also exits zero.
    let executable = koru_session_profiles::resolve_executable(cli.program)
        .map(|path| path.into_os_string())
        .unwrap_or_else(|| cli.program.into());
    match run::run(&executable, &["--version"], VERSION_TIMEOUT, |_| {})
        .ok()
        .and_then(|probe| Version::first_in(&probe.detail()))
    {
        Some(version) if before == Some(&version) => Activity::Failed {
            message: finished.last_line().map(str::to_owned),
            detail: format!(
                "updater exited 0 and the CLI is still {version}\n{}",
                finished.detail()
            ),
        },
        Some(version) => Activity::Updated { version },
        None => Activity::Failed {
            message: finished.last_line().map(str::to_owned),
            detail: format!(
                "updater exited 0 but --version printed no version\n{}",
                finished.detail()
            ),
        },
    }
}
