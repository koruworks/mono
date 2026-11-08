// SPDX-License-Identifier: MPL-2.0

//! **How far a Profile's Cloud sync has got**, across every Workspace: the poll
//! behind Koru Cloud's progress disclosures, and the words it says. Where
//! each row is drawn, and which of them can be clicked, is the page's
//! (`manage/cloud/progress.rs`).
//!
//! Before this, the only Cloud progress anybody could read was per Workspace,
//! in the context column's Cloud section — so a person with eighteen
//! Workspaces opened eighteen of them to learn whether their Profile had
//! finished moving. This is one summary line and one row per Workspace, under
//! *Workspaces on this device*.
//!
//! **The same source as the context pane, deliberately.** Each row is the
//! Store's `cloud-sync` answer for that Workspace — the very call
//! [`crate::mainview::cloud_sync`] polls — asked about the Workspace's
//! manifest document: `sync`, `pass` and `blobs` describe the Workspace.
//! Document holds use the context pane's `hold_sentence` vocabulary.
//!
//! **Documents are independent of files.** Live pass progress names the
//! manifest inventory and verified document exchanges. Completion requires
//! a finished pass without refusal, held directions, hints or invalidation.
//! A current coverage sample remains a fallback for an older gateway; an
//! unknown total never means zero. Files retain their separate expanded list.
//!
//! **A held row opens** (maintainers, 2026-09-24). *Waiting for the device that has
//! these files* names no files and no device; opening the row lists, under
//! it, each document that references a file Koru Cloud does not hold yet and
//! the device it is waiting for. One line per document, never per file.
//! [`openable`] is the only rule for which rows open, and paint, hit testing
//! and the control list all read it through the rows the view recorded.
//!
//! **And the same shape as the context pane's poll.** One worker thread per
//! read, one `(target, generation)`-stamped answer, and a deadline that exists
//! only while the section is showing — a hidden section costs nothing. The
//! waiting documents are part of that same read, asked only for the rows that
//! were open when it started, so a row's counts and its list are always one
//! moment. A second idiom for the same job would be a second thing to keep
//! working.
use std::collections::{BTreeSet, HashMap};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use super::*;
use crate::mainview::cloud_sync::Connection;
use koru_i18n::{Key as TextKey, Lang, fill, number::decimal, t};
use koru_store_core::ids::{DocumentId, WorkspaceId};
use koru_store_wire::CloudWaitingDocument;
use koru_store_wire::internal::CloudSyncStatus;

#[cfg(test)]
pub(crate) mod fixtures;
#[cfg(test)]
mod tests;

/// Visible-only progress; the cached gateway counters require no content walk.
const REFRESH_EVERY: Duration = Duration::from_secs(1);
/// At most this many lines under one open row, the last of them *and N more*
/// when there are more documents than that.
pub const WAITING_CAP: usize = 20;

/// What an open row says under itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Waiting {
    /// Opened since the last read began; the next one brings the list.
    Reading,
    /// The Store would not say.
    Unavailable,
    Documents(Vec<CloudWaitingDocument>),
}

/// One Workspace as the worker read it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceProgress {
    pub id: WorkspaceId,
    pub name: String,
    /// `None` when this one Workspace's read failed. The others still draw:
    /// one Workspace the Store will not answer about is not a reason to show
    /// nothing about seventeen it will.
    pub status: Option<CloudSyncStatus>,
    /// Which documents are waiting, read in the same pass as `status` and
    /// only for a row that was open when the pass began. `None` for every
    /// other row: nobody asked.
    pub waiting: Option<Waiting>,
}

#[derive(Debug)]
enum Answer {
    Read {
        rows: Vec<WorkspaceProgress>,
        manifests: HashMap<WorkspaceId, DocumentId>,
    },
    Failed,
}

#[derive(Debug)]
struct Stamped {
    target: ProfileId,
    generation: u64,
    answer: Answer,
}

/// The poll behind the section. The controller pushes in what is showing and
/// reads the rows back.
#[derive(Default)]
pub struct Poll {
    target: Option<ProfileId>,
    generation: u64,
    visible: bool,
    in_flight: bool,
    receiver: Option<Receiver<Stamped>>,
    next_refresh: Option<Instant>,
    /// The last answer for `target`. `None` until the first one arrives,
    /// which the section says as *Reading Cloud sync…* rather than as zeros.
    rows: Option<Vec<WorkspaceProgress>>,
    unavailable: bool,
    /// Manifest ids learned on an earlier read. A Workspace's manifest never
    /// changes, so each poll asks for it once rather than every five seconds.
    manifests: HashMap<WorkspaceId, DocumentId>,
    /// The rows a person opened. Cleared with the Profile: a row of one
    /// Profile's is nothing to another's.
    open: BTreeSet<WorkspaceId>,
}

impl Poll {
    /// The only clock this section adds to the window, and only while it is
    /// showing a Profile.
    pub fn deadline(&self) -> Option<Instant> {
        (self.visible && self.target.is_some())
            .then_some(self.next_refresh)
            .flatten()
    }

    /// Keep the poll pointed at what is showing. Returns true only when what
    /// the section draws changed.
    pub fn maintain(
        &mut self,
        target: Option<ProfileId>,
        visible: bool,
        client: Option<&koru_api_client::Client>,
        now: Instant,
    ) -> bool {
        let mut changed = false;
        if target != self.target {
            self.target = target;
            self.generation = self.generation.wrapping_add(1);
            self.rows = None;
            self.unavailable = false;
            self.open.clear();
            self.next_refresh = (self.visible && target.is_some()).then_some(now);
            changed = true;
        }
        if visible != self.visible {
            self.visible = visible;
            self.next_refresh = (visible && self.target.is_some()).then_some(now);
            changed = true;
        }
        if let Some(receiver) = &self.receiver
            && let Ok(stamped) = receiver.try_recv()
        {
            self.receiver = None;
            self.in_flight = false;
            // An answer about a Profile nobody is looking at any more is
            // dropped rather than drawn under the wrong name.
            if Some(stamped.target) == self.target && stamped.generation == self.generation {
                match stamped.answer {
                    Answer::Read {
                        mut rows,
                        manifests,
                    } => {
                        self.manifests = manifests;
                        self.unavailable = false;
                        for row in &rows {
                            let previous = self.rows.as_ref().and_then(|rows| {
                                rows.iter().find(|previous| previous.id == row.id)
                            });
                            log_sync_presence(stamped.target, row, previous);
                        }
                        // A row that stopped being able to open closes; one
                        // closed while the read was out keeps nothing it read.
                        self.open.retain(|id| {
                            rows.iter()
                                .any(|row| row.id == *id && openable(row.status.as_ref()))
                        });
                        for row in &mut rows {
                            if !self.open.contains(&row.id) {
                                row.waiting = None;
                            }
                        }
                        if self.rows.as_ref() != Some(&rows) {
                            self.rows = Some(rows);
                            changed = true;
                        }
                    }
                    Answer::Failed => {
                        changed |= !self.unavailable;
                        self.unavailable = true;
                    }
                }
            }
        }
        if self.visible
            && !self.in_flight
            && self.next_refresh.is_some_and(|due| due <= now)
            && let (Some(target), Some(client)) = (self.target, client)
        {
            self.start(target, client.clone());
            self.next_refresh = Some(now + REFRESH_EVERY);
        }
        changed
    }

    /// **Open or close one row.** Returns whether anything changed.
    ///
    /// Only a row [`openable`] says yes to opens. Opening asks at once rather
    /// than at the next five-second tick — a person who clicked is looking —
    /// and closing forgets what was read, so opening again never shows a
    /// list from a moment ago as if it were now.
    pub fn toggle(&mut self, id: WorkspaceId, now: Instant) -> bool {
        if self.open.remove(&id) {
            if let Some(row) = self
                .rows
                .as_mut()
                .and_then(|rows| rows.iter_mut().find(|row| row.id == id))
            {
                row.waiting = None;
            }
            return true;
        }
        let can_open = self
            .rows
            .as_ref()
            .and_then(|rows| rows.iter().find(|row| row.id == id))
            .is_some_and(|row| openable(row.status.as_ref()));
        if !can_open {
            return false;
        }
        self.open.insert(id);
        if self.visible {
            self.next_refresh = Some(now);
        }
        true
    }

    fn start(&mut self, target: ProfileId, client: koru_api_client::Client) {
        // The window and the Store spell a Profile id in two types; a Profile
        // whose id will not parse is one this read must not guess at.
        let Some(owner) = koru_store_core::ProfileId::parse(&target.to_storage()) else {
            return;
        };
        let generation = self.generation;
        let manifests = self.manifests.clone();
        let open = self.open.clone();
        let (send, receive) = mpsc::channel();
        if std::thread::Builder::new()
            .name("koru-cloud-progress-ui".into())
            .spawn(move || {
                let answer = read(&client, owner, manifests, &open);
                let _ = send.send(Stamped {
                    target,
                    generation,
                    answer,
                });
            })
            .is_ok()
        {
            self.in_flight = true;
            self.receiver = Some(receive);
        }
    }
}

/// **The worker's whole job**, off the winit thread: the Profile's Workspaces,
/// then the context pane's own query for each — and, for a row that is open
/// and still able to open, which documents are waiting.
///
/// The Workspaces are the ones *Workspaces on this device* counts — the
/// catalog's live rows owned by this Profile — so the two lines above each
/// other are about the same set.
fn read(
    client: &koru_api_client::Client,
    owner: koru_store_core::ProfileId,
    mut manifests: HashMap<WorkspaceId, DocumentId>,
    open: &BTreeSet<WorkspaceId>,
) -> Answer {
    let Ok(workspaces) = client.workspaces() else {
        return Answer::Failed;
    };
    let mut rows = Vec::new();
    let mut known = HashMap::new();
    for workspace in workspaces
        .into_iter()
        .filter(|workspace| workspace.owner == owner && workspace.deleted_at.is_none())
    {
        let manifest = match manifests.remove(&workspace.id) {
            Some(manifest) => Some(manifest),
            None => client.manifest(workspace.id).ok(),
        };
        let status = manifest.and_then(|manifest| {
            known.insert(workspace.id, manifest);
            client.cloud_sync_status(workspace.id, manifest).ok()
        });
        let waiting = (open.contains(&workspace.id) && openable(status.as_ref())).then(|| {
            match client.cloud_sync_waiting(workspace.id) {
                Ok(answer) => Waiting::Documents(answer.documents),
                Err(_) => Waiting::Unavailable,
            }
        });
        rows.push(WorkspaceProgress {
            id: workspace.id,
            name: workspace.name,
            status,
            waiting,
        });
    }
    Answer::Read {
        rows,
        manifests: known,
    }
}

/// One row, as the section draws it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RowLines {
    pub id: WorkspaceId,
    pub name: String,
    /// Its counts and state.
    pub detail: String,
    /// Whether a person can open it — [`openable`], and nothing else.
    pub openable: bool,
    /// What it says under itself, when it is open.
    pub waiting: Option<Waiting>,
}

/// What the section draws, in the reader's language.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lines {
    pub summary: String,
    pub rows: Vec<RowLines>,
}

/// File details remain expandable independently of document completion.
pub(super) fn openable(status: Option<&CloudSyncStatus>) -> bool {
    status.is_some_and(|status| {
        status
            .sync
            .as_ref()
            .is_some_and(|sync| sync.error_code.is_some())
            || status.blobs.as_ref().is_some_and(|blobs| {
                blobs.paused
                    || blobs.pending_reason.is_some()
                    || blobs.missing > 0
                    || blobs.coverage.as_ref().is_some_and(|coverage| {
                        coverage.state != "superseded" && coverage.blobs_missing > 0
                    })
            })
    })
}

fn on_cloud(status: &CloudSyncStatus) -> bool {
    status.sync.as_ref().is_some_and(|sync| sync.enabled)
}

/// Log the input behind the Off label on first observation and presence
/// transitions only, not on every one-second poll or progress increment.
/// `sync.enabled` describes saved Cloud policy; stage is transient permit state.
fn log_sync_presence(
    profile: ProfileId,
    row: &WorkspaceProgress,
    previous: Option<&WorkspaceProgress>,
) {
    let presence = |row: &WorkspaceProgress| {
        (
            row.status.is_some(),
            row.status
                .as_ref()
                .and_then(|status| status.sync.as_ref())
                .map(|sync| sync.enabled),
        )
    };
    if previous.is_some_and(|previous| presence(previous) == presence(row)) {
        return;
    }
    let status = row.status.as_ref();
    let sync = status.and_then(|status| status.sync.as_ref());
    let pass = status.and_then(|status| status.pass.as_ref());
    let reason = match (status, sync) {
        (None, _) => "status_read_unavailable",
        (Some(_), None) => "cloud_policy_unavailable",
        (_, Some(sync)) if !sync.enabled => "configured_cloud_off",
        _ => "configured_cloud_on",
    };
    // No names, content, credentials, or raw API errors. Unknown policy is
    // never proof of a disabled setting; correlate stages with Agent permits.
    tracing::info!(
        profile = %profile.to_storage(),
        workspace = %row.id.as_uuid(),
        reason,
        displays_off = sync.is_some_and(|sync| !sync.enabled),
        previous_presence = ?previous.map(presence),
        stage = ?sync.and_then(|sync| sync.stage.as_deref()),
        sync_error = ?sync.and_then(|sync| sync.error_code.as_deref()),
        pass_present = pass.is_some(),
        pass_finished = pass.is_some_and(|pass| pass.finished_at.is_some()),
        documents_synced = ?pass.map(|pass| pass.documents_synced),
        documents_required = ?pass.and_then(|pass| pass.documents_required),
        "cloud progress: workspace runtime sync presence changed"
    );
}

/// `(on Cloud, required)`, or `None` when no claim may be read.
fn documents(status: &CloudSyncStatus) -> Option<(u32, u32)> {
    if let Some(pass) = &status.pass {
        if !pass.documents_changed {
            if let Some(confirmed) = &pass.confirmed_documents {
                return Some((confirmed.synced.min(confirmed.required), confirmed.required));
            }
        }
        // An active pass supersedes the previous coverage sample, including
        // the interval before its manifest inventory is known.
        return pass
            .documents_required
            .map(|required| (pass.documents_synced.min(required), required));
    }
    let coverage = status.blobs.as_ref()?.coverage.as_ref()?;
    // **Superseded claims nothing** — the Workspace moved between the sample
    // and the boundary — so it is not read as a number either.
    if !coverage.current || coverage.state == "superseded" {
        return None;
    }
    Some((
        coverage
            .documents_required
            .saturating_sub(coverage.documents_missing),
        coverage.documents_required,
    ))
}

fn documents_synced(status: &CloudSyncStatus) -> bool {
    if !on_cloud(status)
        || status
            .sync
            .as_ref()
            .is_some_and(|sync| sync.error_code.is_some())
    {
        return false;
    }
    if let Some(pass) = &status.pass {
        if pass.refusal_code.is_none()
            && !pass.documents_changed
            && !pass.remote_hint
            && pass.deferred == 0
            && pass
                .confirmed_documents
                .as_ref()
                .is_some_and(|progress| progress.complete)
        {
            return true;
        }
        return pass.finished_at.is_some()
            && pass.refusal_code.is_none()
            && !pass.documents_changed
            && !pass.remote_hint
            && pass.deferred == 0
            && pass
                .documents_required
                .is_some_and(|required| pass.documents_synced == required);
    }
    documents(status).is_some_and(|(synced, required)| synced == required)
}

/// One Workspace's document progress. On-demand files have separate details.
///
/// `connection` is the Profile's, the very value its row subtitle says
/// ([`ProfilesController::cloud_connection`]). A Workspace's own report has no
/// permit while Koru Cloud refuses the Profile, so without it a refused
/// Profile's rows read *Connecting…* under a subtitle saying *unavailable*.
/// While it is [`Connection::Unavailable`] every row ends in that sentence,
/// including a row whose last pass completed (maintainers, 2026-10-10).
pub(super) fn row_detail(
    lang: Lang,
    status: Option<&CloudSyncStatus>,
    connection: Connection,
) -> String {
    let Some(status) = status else {
        return t(lang, TextKey::CloudTabUnavailable).to_owned();
    };
    if status.sync.is_none() {
        return t(lang, TextKey::CloudTabUnavailable).to_owned();
    }
    if !on_cloud(status) {
        return t(lang, TextKey::CloudTabSyncOff).to_owned();
    }
    // **A completed pass survives a missing permit only while connecting.**
    // Under an unavailable Profile every row says so, a completed one too:
    // *Synced* beneath *Koru Cloud unavailable* reads as a contradiction,
    // and nothing new reaches Koru Cloud until it is available again.
    if documents_synced(status) && !connection.is_stopped() {
        return t(lang, TextKey::CloudTabDocSynced).to_owned();
    }
    let mut parts = Vec::new();
    if let Some((synced, required)) = documents(status) {
        parts.push(fill(
            t(lang, TextKey::CloudProgressDocuments),
            &[
                &decimal(lang, synced.into()),
                &decimal(lang, required.into()),
            ],
        ));
    }
    // Files are fetched on demand. Their holds cannot relabel document sync.
    let hold = status
        .sync
        .as_ref()
        .and_then(|sync| sync.error_code.as_deref())
        .or_else(|| {
            status
                .pass
                .as_ref()
                .and_then(|pass| pass.refusal_code.as_deref())
        });
    let state = match hold {
        // A Workspace's own hold is the more specific sentence. A code that
        // means Koru Cloud refuses service reads as the unavailable wording.
        Some(code) => Some(crate::mainview::cloud_sync::hold_sentence(lang, code)),
        None => connection.is_stopped().then(|| connection.sentence(lang)),
    };
    parts.extend(state);
    if parts.is_empty() {
        // Enabled without a measured document inventory says what the
        // Profile's connection is doing: *Connecting…* only while it is.
        return connection.sentence(lang);
    }
    parts.join(" · ")
}

/// *Trip notes · 6 files · waiting for Pixel 8*.
///
/// One document, however many files it is missing, and the device it is
/// waiting for — or, when the Store knows of none, that no device online has
/// them, which is the honest sentence rather than a guessed name.
pub(super) fn document_line(lang: Lang, document: &CloudWaitingDocument) -> String {
    let title = match document.title.trim() {
        "" => t(lang, TextKey::DocUntitled),
        title => title,
    };
    let files = match document.missing_files {
        1 => t(lang, TextKey::CloudProgressWaitingOneFile).to_owned(),
        count => fill(
            t(lang, TextKey::CloudProgressWaitingFiles),
            &[&decimal(lang, count.into())],
        ),
    };
    let whom = match document.holders.as_slice() {
        [] => t(lang, TextKey::CloudProgressWaitingNoHolder).to_owned(),
        holders => fill(
            t(lang, TextKey::CloudProgressWaitingFor),
            &[&holders.join(", ")],
        ),
    };
    [title, &files, &whom].join(" · ")
}

/// **What an open row says under itself, in at most `limit` lines.**
///
/// One line per document; when there are more documents than lines, the
/// last line is *and N more* instead. Workspace rows themselves are never
/// capped; the complete row and these bounded details contribute to scrolling.
pub fn waiting_lines(lang: Lang, waiting: &Waiting, limit: usize) -> Vec<String> {
    if limit == 0 {
        return Vec::new();
    }
    let documents = match waiting {
        Waiting::Reading => return vec![t(lang, TextKey::CloudTabLoading).to_owned()],
        Waiting::Unavailable => return vec![t(lang, TextKey::CloudTabUnavailable).to_owned()],
        Waiting::Documents(documents) if documents.is_empty() => {
            return vec![t(lang, TextKey::CloudProgressWaitingNone).to_owned()];
        }
        Waiting::Documents(documents) => documents,
    };
    if documents.len() <= limit {
        return documents
            .iter()
            .map(|document| document_line(lang, document))
            .collect();
    }
    let shown = limit - 1;
    let mut lines: Vec<String> = documents[..shown]
        .iter()
        .map(|document| document_line(lang, document))
        .collect();
    lines.push(fill(
        t(lang, TextKey::CloudProgressMore),
        &[&decimal(lang, (documents.len() - shown) as u64)],
    ));
    lines
}

/// Completed document passes, using exactly the same gate as each row.
pub(super) fn summary(lang: Lang, rows: &[WorkspaceProgress]) -> String {
    let statuses = || rows.iter().filter_map(|row| row.status.as_ref());
    let on = statuses().filter(|status| documents_synced(status)).count() as u64;
    let mut parts = vec![fill(
        t(lang, TextKey::CloudProgressWorkspaces),
        &[&decimal(lang, on), &decimal(lang, rows.len() as u64)],
    )];
    let held = statuses()
        .filter(|status| {
            status
                .sync
                .as_ref()
                .is_some_and(|sync| sync.error_code.is_some())
                || status
                    .pass
                    .as_ref()
                    .is_some_and(|pass| pass.refusal_code.is_some())
        })
        .count() as u64;
    if held > 0 {
        parts.push(fill(
            t(lang, TextKey::CloudProgressHeld),
            &[&decimal(lang, held)],
        ));
    }
    parts.join(" · ")
}

/// **Which Profile the poll follows.** Manage Profiles showed the selected
/// Profile's progress in its detail pane; the Manage tab's Koru Cloud page
/// shows it under the one Profile whose disclosure is open (maintainers' decision 20),
/// and none at all while that page is not in front.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ProgressOwner {
    /// The selected Profile.
    #[default]
    Selected,
    /// The Profile whose disclosure is open on Koru Cloud, if any.
    Disclosed(Option<ProfileId>),
}

impl ProfilesController {
    /// Say which Profile the progress poll follows.
    pub fn set_progress_owner(&mut self, owner: ProgressOwner) {
        self.progress_owner = owner;
    }

    /// **The Profile this section is about**, or `None` when there is no
    /// section: a Profile bound to the signed-in account whose Cloud sync is
    /// on. A local-only Profile shows nothing new.
    pub fn cloud_progress_profile(&self) -> Option<ProfileId> {
        let wanted = match self.progress_owner {
            ProgressOwner::Selected => self.selected,
            ProgressOwner::Disclosed(profile) => profile,
        };
        let profile = self
            .profiles
            .iter()
            .find(|profile| Some(profile.id) == wanted)?;
        (self.cloud_sync_availability(profile) == cloud::SyncAvailability::Enabled
            && self.cloud_sync_on(profile.id))
        .then_some(profile.id)
    }

    /// Point the poll at the selected Profile while the tab is in front.
    pub(super) fn maintain_cloud_progress(&mut self, now: Instant) -> bool {
        let target = self.cloud_progress_profile();
        let visible = self.cloud_visible && target.is_some();
        self.progress
            .maintain(target, visible, self.client.as_ref(), now)
    }

    /// What to draw, or `None` for no section.
    pub fn cloud_progress_lines(&self) -> Option<Lines> {
        let profile = self.cloud_progress_profile()?;
        let lang = crate::format::lang();
        let connection = self.cloud_connection(profile);
        let panel = &self.progress;
        if panel.target != Some(profile) {
            // Selected this frame; the poll has not been pointed at it yet.
            return Some(Lines {
                summary: t(lang, TextKey::CloudTabLoading).to_owned(),
                rows: Vec::new(),
            });
        }
        let Some(rows) = &panel.rows else {
            let key = match panel.unavailable {
                true => TextKey::CloudTabUnavailable,
                false => TextKey::CloudTabLoading,
            };
            return Some(Lines {
                summary: t(lang, key).to_owned(),
                rows: Vec::new(),
            });
        };
        Some(Lines {
            summary: summary(lang, rows),
            rows: rows
                .iter()
                .map(|row| {
                    let openable = openable(row.status.as_ref());
                    let open = openable && panel.open.contains(&row.id);
                    RowLines {
                        id: row.id,
                        name: if row.name.trim().is_empty() {
                            t(lang, TextKey::DocUntitled).to_owned()
                        } else {
                            row.name.clone()
                        },
                        detail: row_detail(lang, row.status.as_ref(), connection),
                        openable,
                        waiting: open.then(|| row.waiting.clone().unwrap_or(Waiting::Reading)),
                    }
                })
                .collect(),
        })
    }
}
