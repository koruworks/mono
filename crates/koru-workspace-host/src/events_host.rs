// SPDX-License-Identifier: MPL-2.0

//! Fire-and-forget event emission for the desktop app.
//!
//! One named thread and one process-global sender: an emitter anywhere in the
//! process — the shell, the container, a view — pays one `mpsc` send, O(1)
//! and off the frame loop, and this thread owns the blocking client (the
//! `koru-profile-catalog` worker shape: build lazily, keep for the thread's
//! lifetime, drop on error to force reconnect). Events are observations:
//! before [`start`], with the gateway missing, or with the `events`
//! capability absent, an emit is silently not made — the app must never
//! degrade because the bus did.
//!
//! **Why a process global**: the emitters live on both sides of the
//! shell/container boundary (`new_workspace` here, `open_search` in
//! `apps/koru`), and neither may acquire the other to say one sentence. The
//! same idiom as the theme globals. Under `cargo test` nothing calls
//! [`start`] — only `App::new` does, and tests never construct one — so every
//! `emit` is a no-op and no test can dial a live store (the no-reader
//! lesson).

use std::sync::OnceLock;
use std::sync::mpsc::{Sender, SyncSender};
use std::time::Instant;

pub use koru_events::{ImportOutcome, PreviousSession, SearchSurface, WorkspaceSource};

enum Job {
    Emit(koru_events::Draft),
    /// Collect the machine's own facts *here* — several of them touch the
    /// registry, the disk and the network stack, and the loop that handed
    /// this over must not wait on any of it.
    Environment(crate::environment::WindowFacts),
    /// Everything before this has been attempted; answer.
    Flush(SyncSender<()>),
}

static SENDER: OnceLock<Sender<Job>> = OnceLock::new();
static STARTED: OnceLock<Instant> = OnceLock::new();
static MARKER: OnceLock<Option<std::path::PathBuf>> = OnceLock::new();

/// The clean-exit marker: one file with three states. Absent — this channel
/// never ran before (or has nowhere to keep one): `unknown`. `running` — the
/// previous run never said goodbye: `unclean`, which is the crash signal,
/// reported on the *next* start because a crashing process cannot report its
/// own. `clean` — the previous run exited through [`session_ended_and_flush`].
///
/// It lives beside the events journal (`events_root_beside`), so a private
/// `--root` stack keeps its marker inside its own tree and channels never
/// share one. Known limitation, accepted: two *processes* on one channel
/// (`--peer B`) share the file, so the first clean exit speaks for both — a
/// dev-workflow blur on an aggregate rate, not a correctness hazard.
fn session_marker(channel_root: Option<&std::path::Path>) -> Option<std::path::PathBuf> {
    let root = channel_root
        .map(koru_channel::store_root_under)
        .or_else(koru_channel::store_root)?;
    Some(koru_channel::events_root_beside(&root).join("app-session-state"))
}

fn read_previous_session(marker: Option<&std::path::PathBuf>) -> PreviousSession {
    match marker.map(std::fs::read_to_string) {
        Some(Ok(state)) if state.trim() == "clean" => PreviousSession::Clean,
        Some(Ok(_)) => PreviousSession::Unclean,
        Some(Err(_)) | None => PreviousSession::Unknown,
    }
}

fn write_marker(state: &str) {
    if let Some(Some(marker)) = MARKER.get() {
        if let Some(parent) = marker.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(marker, state);
    }
}

/// Start the host and announce the session. Called once from `App::new` after
/// the store is proven reachable; a second call is a no-op.
pub fn start(gateway_root: Option<std::path::PathBuf>) {
    if SENDER.get().is_some() || cfg!(test) {
        return;
    }
    let (tx, rx) = std::sync::mpsc::channel();
    if SENDER.set(tx).is_err() {
        return;
    }
    let _ = STARTED.set(Instant::now());
    let gateway_root_for_marker = gateway_root.clone();
    let _ = std::thread::Builder::new()
        .name("koru-events-emit".into())
        .spawn(move || {
            let segment = koru_workspace::fromstore::segment().to_owned();
            let mut client: Option<koru_api_client::Client> = None;
            // A gateway without the events worker answers once, not per event.
            let mut unavailable = false;
            while let Ok(job) = rx.recv() {
                // Both publishing jobs want the same thing first: a client,
                // built on demand and kept for this thread's lifetime.
                let draft = match job {
                    Job::Emit(draft) => draft,
                    // Collected here rather than by the caller — several of
                    // these readers touch the registry, the disk and the
                    // network stack, and the loop that asked must not wait.
                    Job::Environment(window) => {
                        koru_events::Draft::from(crate::environment::collect(window))
                    }
                    Job::Flush(ack) => {
                        let _ = ack.send(());
                        continue;
                    }
                };
                if unavailable {
                    continue;
                }
                if client.is_none() {
                    client = match gateway_root.as_deref() {
                        Some(root) => koru_api_client::Client::open_at(&segment, root).ok(),
                        None => koru_api_client::Client::open(&segment).ok(),
                    };
                    if let Some(connected) = client.as_ref()
                        && !connected.events().available()
                    {
                        unavailable = true;
                        client = None;
                        continue;
                    }
                }
                let Some(connected) = client.as_ref() else {
                    continue;
                };
                if connected.events().send(draft).is_err() {
                    // Reconnect on the next event; the observation itself is
                    // let go.
                    client = None;
                }
            }
        });
    let _ = MARKER.set(session_marker(gateway_root_for_marker.as_deref()));
    let previous = read_previous_session(MARKER.get().and_then(Option::as_ref));
    write_marker("running");
    emit(koru_events::AppSessionStarted {
        previous_session_clean: previous,
    });
}

/// Emit one event. Never blocks, never fails.
pub fn emit(event: impl Into<koru_events::Draft>) {
    if let Some(sender) = SENDER.get() {
        let _ = sender.send(Job::Emit(event.into()));
    }
}

/// The exit path's one bounded wait: announce the session's end, then wait —
/// briefly — until the thread has attempted everything queued, so
/// `app.session.ended` leaves with the process instead of dying with it.
pub fn session_ended_and_flush(deadline: std::time::Duration) {
    let Some(sender) = SENDER.get() else { return };
    let minutes = STARTED
        .get()
        .map(|started| started.elapsed().as_secs() / 60)
        .unwrap_or(0);
    let _ = sender.send(Job::Emit(
        koru_events::AppSessionEnded {
            duration_bucket: koru_events::bucket(minutes),
        }
        .into(),
    ));
    let (ack, done) = std::sync::mpsc::sync_channel(1);
    if sender.send(Job::Flush(ack)).is_ok() {
        let _ = done.recv_timeout(deadline);
    }
    // The goodbye itself: the next start reads `clean` and reports it. Last,
    // so a crash inside the flush still reads as the unclean exit it was.
    write_marker("clean");
}

pub fn workspace_created(workspace: uuid::Uuid, source: WorkspaceSource) {
    emit(koru_events::WorkspaceCreated {
        workspace_id: workspace,
        source,
    });
}

pub fn workspace_deleted(workspace: uuid::Uuid) {
    emit(koru_events::WorkspaceDeleted {
        workspace_id: workspace,
    });
}

pub fn search_opened(surface: SearchSurface) {
    emit(koru_events::SearchOpened { surface });
}

pub fn onboarding(telemetry: crate::onboarding::Telemetry, ab: koru_events::OnboardingAbAttrs) {
    use crate::onboarding::Telemetry;
    match telemetry {
        Telemetry::Shown => emit(koru_events::OnboardingShown { ab }),
        Telemetry::EntryOffline => emit(koru_events::OnboardingEntrySelected {
            choice: koru_events::OnboardingEntryChoice::Offline,
            ab,
        }),
        Telemetry::EntryGoogle => emit(koru_events::OnboardingEntrySelected {
            choice: koru_events::OnboardingEntryChoice::Google,
            ab,
        }),
        Telemetry::EntryApple => emit(koru_events::OnboardingEntrySelected {
            choice: koru_events::OnboardingEntryChoice::Apple,
            ab,
        }),
        Telemetry::EntryEmail => emit(koru_events::OnboardingEntrySelected {
            choice: koru_events::OnboardingEntryChoice::Email,
            ab,
        }),
    }
}

/// Report the machine, once, after the first frame. The caller passes what
/// only it can see — its monitors and its window — and the worker collects
/// the rest off the loop. A second call does nothing: a machine does not
/// change between two frames, and this is a per-run fact.
pub fn app_environment(window: crate::environment::WindowFacts) {
    static REPORTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if REPORTED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    if let Some(sender) = SENDER.get() {
        let _ = sender.send(Job::Environment(window));
    }
}

/// `format` is the importer's stable id (`"markdown"`, `"textbundle"`,
/// `"onenote"`, `"legacy_workspace"`, `"files"`, `"google-docs"`,
/// `"notion"`, `"evernote"`); an unknown one emits nothing rather than inventing
/// vocabulary.
pub fn import_completed(format: &str, documents: usize, outcome: ImportOutcome) {
    let source = match format {
        "markdown" => koru_events::ImportSource::Markdown,
        "textbundle" => koru_events::ImportSource::Textbundle,
        "onenote" => koru_events::ImportSource::Onenote,
        "legacy_workspace" => koru_events::ImportSource::LegacyWorkspace,
        "files" => koru_events::ImportSource::Files,
        "google-docs" => koru_events::ImportSource::GoogleDocs,
        "notion" => koru_events::ImportSource::Notion,
        "evernote" => koru_events::ImportSource::Evernote,
        _ => return,
    };
    emit(koru_events::ImportCompleted {
        source,
        document_count_bucket: koru_events::bucket(documents as u64),
        outcome,
    });
}
