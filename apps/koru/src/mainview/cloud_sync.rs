// SPDX-License-Identifier: MPL-2.0

//! Deadline-driven adapter between the Store's Cloud sync queries and the
//! context column's Cloud section.
//!
//! **The open document's own Cloud state, and nothing about the workspace
//! around it** (maintainers, 2026-09-24). The section is headed *Cloud sync · <the
//! document's title>*, and it used to mix that document's facts with the
//! workspace's — *files 169 of 270*, *sending 16 MB* — under the document's
//! name, so a person could not tell which of the numbers was about what they
//! were looking at. It now says three things, all about the document: whether
//! Koru Cloud holds it (*Synced* / *Not synced yet*, with the local *Changed 7
//! min · from this device* beside it), how many of its images are synced, and
//! how many of its other files. The workspace's view — every workspace's
//! counts, holds and transfer — lives on the Manage tab's Koru Cloud page
//! (`manage::profiles::progress`, drawn by `manage::cloud::progress`),
//! and so do the controls that act on a whole workspace.
//!
//! **The same shape as [`crate::mainview::history`], deliberately.** One
//! serialized worker thread, one `(target, generation)`-stamped answer, a
//! deadline that exists only while the section is visible, and a rebuild that
//! ages its relative times on a minute boundary. A second idiom for the same
//! job would be a second thing to keep working. The worker asks the Store two
//! things in a row — the document's `cloud-sync` report and its `coverage` —
//! and hands them back as one answer, so the lines are always drawn from one
//! poll rather than from two that happened to arrive near each other. A new
//! document is a new target, so switching documents retires the answer in
//! flight and asks again at once.
//!
//! **There is no push.** The daemon wakes an open document's *room* when Cloud
//! delivers bytes to it (`cloud_driver`'s `CloudDownloads`), but nothing
//! announces that a pass finished — the Agent itself polls the control host
//! for that. So this polls too, on the same five seconds History uses, and a
//! hidden section costs nothing at all.
//!
//! Every string it produces is written here, in the reader's language, and
//! handed to chrome as plain text: the column has no timezone, no locale and
//! no opinion about either.
//!
//! [`hold_sentence`] and [`transfer_tail`] are shared with Manage's
//! per-workspace progress rows (and their wording with the phone), which is why they
//! stay here although this section no longer draws a transfer.

use std::{
    sync::mpsc::{self, Receiver},
    time::{Duration, Instant},
};

use koru_i18n::{Key, Lang};
use koru_store_core::ids::{DocumentId, WorkspaceId};
use koru_store_wire::internal::{
    BlobFileReport, CloudChangeOrigin, CloudDocumentCoverage, CloudDocumentFiles, CloudSyncStatus,
};

use crate::panes::context::CloudLine;

const REFRESH_EVERY: Duration = Duration::from_secs(5);

/// What the panel needs from the rest of the window to write its lines, pushed
/// in rather than reached for — this module holds no `MainView`.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct CloudContext {
    /// `(device id, display name)` for every device this Profile has linked,
    /// so a peer origin becomes a name rather than a uuid. Empty is normal —
    /// the Agent may not have answered yet — and the line then says "from
    /// another device".
    pub devices: Vec<(String, String)>,
    /// How many images in the document showing this device does not hold, and
    /// the devices that are known to have them.
    ///
    /// **Sourced from the open document, not from the Store**, because the
    /// editor already knows: it is what paints the missing-image placeholder.
    /// It is a fact about *this device's* copies, which is not the Store's
    /// *synced* — that one is about Koru Cloud's — so the Images line carries
    /// both, the Cloud count first.
    pub missing_images: usize,
    pub missing_image_devices: Vec<String>,
    /// **The file a person has selected**, if that is what the selection is
    /// (D259 §4).
    ///
    /// The maintainers' instruction was *"when I click on a file, the context pane
    /// should update to show the context of the selected file"*, and the pane
    /// was showing the document's. These three facts come from the node the
    /// window already has — a name, a document id and a media type — and what
    /// the Store adds is everything about the **bytes**.
    pub file: Option<SelectedFile>,
    /// **The open Profile's Koru Cloud connection** — [`Connection::of`], the
    /// one function Manage's Profile row and its workspace rows read too, so
    /// this column cannot call Koru Cloud reachable while they say it is not.
    /// `None` when the window has no Profile open.
    pub connection: Option<Connection>,
}

/// **A Profile's Koru Cloud connection, as one value.**
///
/// Manage's Profile row subtitle, every workspace row under its sync progress
/// and this column's note all read it from [`Connection::of`]. They used to
/// decide separately: the Profile row read the Agent's connection stage and
/// said *Koru Cloud unavailable*, while each workspace row, which saw only
/// the Store's per-Workspace report, found no measured inventory and said
/// *Connecting to Koru Cloud…*. That happened whenever the hosted side
/// refused, for example a 503 `sync_unavailable` with no sync worker ready.
/// A Workspace has no permit then, so nothing in its own report says why.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Connection {
    /// Asked for and not yet answered. Only this state says *Connecting…*.
    Connecting,
    Syncing,
    WaitingForConsent,
    /// The build has no Koru Cloud (`AccountStage::NotConfigured`). Not an
    /// outage — there is nothing to connect to — so nothing says *retrying*,
    /// and every row under the Profile says the same, as for `Unavailable`.
    NotConfigured,
    /// Refused, retrying, or no account to connect with. Local content is
    /// still there, and the sentence says so. **Every** workspace row under
    /// the Profile says it — a row whose last pass completed included, which
    /// would otherwise read *Synced* beneath an unavailable Profile.
    Unavailable,
    /// Koru Cloud answered that something of this account is damaged on its
    /// side (root-catalog QUESTIONS.md Q12 (d)): `root_projection_corrupt`
    /// or `catalog_binding_corrupt`. The client keeps retrying — only an
    /// operator can repair it — but it is **not** an outage, and saying
    /// *unavailable* sent a person to check a connection that is fine.
    AccountProblem,
    /// A unit of this account is in a format this build cannot read
    /// (Q12 (a), `root_unit_format_too_new`). Everything else still syncs,
    /// so this is the Profile's line and not a stop: the one honest answer
    /// is that the person needs a newer Koru.
    NeedsUpdate,
}

impl Connection {
    /// The connection of `profile_id` (its storage spelling) under `account`.
    ///
    /// `refused` is a Profile-level refusal the caller holds that the account
    /// state does not carry, such as the Agent's error on Manage's per-Profile
    /// Cloud sync row.
    pub fn of(account: &koru_agent_wire::AccountState, profile_id: &str, refused: bool) -> Self {
        use koru_agent_wire::{AccountStage, CloudConnectionStage};
        if account.stage == AccountStage::NotConfigured {
            return Self::NotConfigured;
        }
        let row = account
            .cloud_profiles
            .iter()
            .find(|row| row.profile_id == profile_id);
        let discovery = row.and_then(|row| row.discovery.as_ref());
        // **A refusal Koru Cloud worded itself is not an outage** (Q12). The
        // pass's own code says which one it is, and it is read before the
        // generic *unavailable*: both of these look like a refusal to serve
        // from the outside, and both used to be told as one.
        if discovery
            .and_then(|discovery| discovery.failure_code.as_deref())
            .is_some_and(service_damaged)
        {
            return Self::AccountProblem;
        }
        if refused
            || matches!(
                account.stage,
                AccountStage::Unavailable | AccountStage::Failed | AccountStage::SignedOut
            )
        {
            return Self::Unavailable;
        }
        // One unit of this Profile needs a newer build. The pass is
        // otherwise healthy, so this reads ahead of the stage — a deferred
        // unit makes the Profile *retrying*, which would say *unavailable*
        // for ever about something no retry can fix.
        if discovery
            .and_then(|discovery| discovery.deferred_code.as_deref())
            .is_some_and(|code| code == UNIT_FORMAT_TOO_NEW)
        {
            return Self::NeedsUpdate;
        }
        match row.map(|row| row.stage) {
            Some(CloudConnectionStage::Syncing) => Self::Syncing,
            Some(CloudConnectionStage::Retrying) => Self::Unavailable,
            Some(CloudConnectionStage::WaitingForConsent) => Self::WaitingForConsent,
            Some(CloudConnectionStage::Connecting) | None => Self::Connecting,
        }
    }

    /// Whether Koru Cloud serves this Profile nothing right now — it is
    /// unavailable, damaged on the service's side, or this build has none.
    /// Every row under the Profile then ends in the Profile's sentence, a
    /// completed one included.
    ///
    /// **A build that needs updating is not among them**: one unit cannot be
    /// read and everything else is syncing, so a workspace that *is* synced
    /// still says so and only the Profile's line carries the prompt.
    pub fn is_stopped(self) -> bool {
        matches!(
            self,
            Self::Unavailable | Self::NotConfigured | Self::AccountProblem
        )
    }

    pub fn key(self) -> Key {
        match self {
            Self::Connecting => Key::CloudConnectionConnecting,
            Self::Syncing => Key::CloudSyncOn,
            Self::WaitingForConsent => Key::CloudConnectionConsent,
            Self::NotConfigured => Key::CloudNotConfigured,
            Self::Unavailable => Key::CloudConnectionRetrying,
            Self::AccountProblem => Key::CloudConnectionAccountProblem,
            Self::NeedsUpdate => Key::CloudConnectionUpdateRequired,
        }
    }

    pub fn sentence(self, lang: Lang) -> String {
        koru_i18n::t(lang, self.key()).to_owned()
    }
}

/// Whether a stage or refusal code means Koru Cloud will not serve this
/// Profile right now, rather than that something about this Workspace or
/// this device is holding it. These read as [`Connection::Unavailable`].
pub(crate) fn refuses_service(code: &str) -> bool {
    matches!(
        code,
        "cloud_unavailable" | "sync_unavailable" | "cloud_worker_unavailable"
    )
}

/// A unit written by a newer Koru than this build can read (root-catalog
/// QUESTIONS.md Q12 (a)). Spelled as the service spells it, which is also
/// what the pass puts in its diagnostics.
const UNIT_FORMAT_TOO_NEW: &str = "root_unit_format_too_new";

/// Whether the code says Koru Cloud has damage of its own on this account
/// (Q12 (d)): the projection or the account's catalog binding. Retried, and
/// never worded as an outage — telling a person to check their connection
/// about service-side damage sends them to look at the wrong thing.
pub(crate) fn service_damaged(code: &str) -> bool {
    matches!(code, "root_projection_corrupt" | "catalog_binding_corrupt")
}

/// One selected file, as the window knows it before asking anything.
///
/// **Three facts that cost nothing**: they are already on the explore row the
/// person clicked. The address is deliberately *not* among them — a workspace
/// file is a document whose root node is the blob, so learning its address
/// means fetching and decoding that document's snapshot, and paying that per
/// click on the thread that has to stay ready is exactly what
/// `PRINCIPLES.md` forbids. The Store turns the id into an address with one
/// indexed read on the poll that was already going.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SelectedFile {
    /// What the row is called: the person's own filename where there is one.
    pub name: String,
    /// The file document's id — what the window has, not what it would have
    /// to go and find.
    pub document: DocumentId,
    /// The media type the manifest recorded, when it did. Metadata, never a
    /// decision, and empty when the listing predates file metadata.
    pub mime: String,
}

#[derive(Debug)]
enum Answer {
    /// The document's report, and its coverage when the Store could say.
    ///
    /// A coverage the Store would not give — a build with no blob engine, a
    /// read model that would not answer — is `None` beside a report that did
    /// arrive: the lines that report can source are still drawn, and the ones
    /// only coverage can source say *not checked* rather than disappearing
    /// the whole section.
    Status(Box<CloudSyncStatus>, Option<Box<CloudDocumentCoverage>>),
    Failed,
}

#[derive(Debug)]
struct StampedAnswer {
    target: (WorkspaceId, DocumentId),
    generation: u64,
    answer: Answer,
}

#[derive(Default)]
pub struct CloudSyncPanel {
    client: Option<koru_api_client::Client>,
    target: Option<(WorkspaceId, DocumentId)>,
    generation: u64,
    visible: bool,
    in_flight: bool,
    receiver: Option<Receiver<StampedAnswer>>,
    next_refresh: Option<Instant>,
    lang: Lang,
    context: CloudContext,
    status: Option<CloudSyncStatus>,
    /// The open document's own coverage, from the same poll as `status`.
    coverage: Option<CloudDocumentCoverage>,
    unavailable: bool,
    /// The minute the lines were last written for. Relative times change at
    /// minute granularity at their finest, so a rebuild costs a handful of
    /// string formats per minute rather than one per maintenance tick.
    lines_minute: i64,
    lines: Vec<CloudLine>,
    note: Option<String>,
    /// Set when *Copy details* ran, and cleared by the next rebuild — an
    /// acknowledgement, not a state.
    copied: bool,
}

impl CloudSyncPanel {
    pub fn set_client(&mut self, client: Option<koru_api_client::Client>) {
        self.client = client;
    }

    pub fn set_lang(&mut self, lang: Lang) {
        self.lang = lang;
    }

    /// The tab's own label, localized. Pushed into chrome beside the lines.
    pub fn section_label(&self) -> String {
        koru_i18n::t(self.lang, Key::CloudTabSection).to_owned()
    }

    pub fn copy_label(&self) -> String {
        koru_i18n::t(self.lang, Key::CloudTabCopyDetails).to_owned()
    }

    pub fn lines(&self) -> Vec<CloudLine> {
        self.lines.clone()
    }

    pub fn note(&self) -> Option<String> {
        self.note.clone()
    }

    /// *Copy details* happened. The next rebuild says so once and then stops.
    pub fn acknowledge_copy(&mut self) -> bool {
        self.copied = true;
        self.rebuild(now_ms())
    }

    /// The only clock this section contributes to the event loop. A hidden
    /// section has no deadline and therefore no background traffic.
    pub fn deadline(&self) -> Option<Instant> {
        (self.visible && self.target.is_some())
            .then_some(self.next_refresh)
            .flatten()
    }

    /// Maintain the visible target and its one serialized worker.
    ///
    /// Returns true only when the lines changed and chrome needs them again.
    pub fn maintain(
        &mut self,
        target: Option<(WorkspaceId, DocumentId)>,
        visible: bool,
        context: &CloudContext,
        now: Instant,
    ) -> bool {
        let mut changed = false;

        if target != self.target {
            // **A different document is a different question.** Nothing
            // about the last document may stand under this one's title for
            // even one poll, so both answers are dropped and the next poll is
            // due now.
            self.target = target;
            self.generation = self.generation.wrapping_add(1);
            self.status = None;
            self.coverage = None;
            self.unavailable = false;
            self.copied = false;
            self.next_refresh = (self.visible && target.is_some()).then_some(now);
            changed |= self.rebuild(now_ms());
        }

        if visible != self.visible {
            self.visible = visible;
            self.next_refresh = (visible && self.target.is_some()).then_some(now);
            changed = true;
        }

        if &self.context != context {
            // **A different file is a different question** (D259 §4). The
            // answer in flight was asked about the last selection, so it is
            // retired the way a document change retires one — a stamped
            // answer for an old generation is dropped rather than drawn — and
            // the cached file is cleared at once, because showing the
            // previous file's facts under this file's name for one poll is
            // exactly the class of lie this work is about.
            let moved = self.context.file.as_ref().map(|file| file.document)
                != context.file.as_ref().map(|file| file.document);
            self.context = context.clone();
            if moved {
                self.generation = self.generation.wrapping_add(1);
                if let Some(status) = self.status.as_mut() {
                    status.file = None;
                }
                self.next_refresh = (self.visible && self.target.is_some()).then_some(now);
            }
            changed |= self.rebuild(now_ms());
        }

        if let Some(receiver) = &self.receiver
            && let Ok(stamped) = receiver.try_recv()
        {
            self.receiver = None;
            self.in_flight = false;
            // A stale answer is dropped rather than shown: it is about a
            // document that is no longer in front of anybody.
            if Some(stamped.target) == self.target && stamped.generation == self.generation {
                match stamped.answer {
                    Answer::Status(status, coverage) => {
                        self.unavailable = false;
                        let coverage = coverage.map(|coverage| *coverage);
                        if self.status.as_ref() != Some(&status) || self.coverage != coverage {
                            self.status = Some(*status);
                            self.coverage = coverage;
                            changed |= self.rebuild(now_ms());
                        }
                    }
                    Answer::Failed => {
                        if !self.unavailable {
                            self.unavailable = true;
                            changed |= self.rebuild(now_ms());
                        }
                    }
                }
            }
        }

        if self.visible
            && self.target.is_some()
            && !self.in_flight
            && self.next_refresh.is_some_and(|deadline| deadline <= now)
        {
            self.start();
            self.next_refresh = Some(now + REFRESH_EVERY);
        }

        if self.visible && self.status.is_some() {
            let now_ms = now_ms();
            if now_ms / 60_000 != self.lines_minute {
                changed |= self.rebuild(now_ms);
            }
        }
        changed
    }

    fn start(&mut self) {
        let (Some(target), Some(client)) = (self.target, self.client.clone()) else {
            return;
        };
        let generation = self.generation;
        // **The selected file rides the poll that was already going** (D259
        // §4): two round trips would be two moments, and a pane drawing the
        // document's lines from one and this file's state from the other
        // would contradict itself the way the panel already did once.
        let file = self.context.file.as_ref().map(|file| file.document);
        let (send, receive) = mpsc::channel();
        if std::thread::Builder::new()
            .name("koru-cloud-sync-ui".into())
            .spawn(move || {
                let answer = match client.cloud_sync_status_for(target.0, target.1, file) {
                    // Asked with the **document**, not the workspace's
                    // manifest: this is the one question here that is about
                    // the document's own content. Asked after the report, on
                    // this same thread, so the two travel as one answer.
                    Ok(status) => Answer::Status(
                        Box::new(status),
                        client
                            .cloud_sync_coverage(target.0, target.1)
                            .ok()
                            .map(Box::new),
                    ),
                    Err(_) => Answer::Failed,
                };
                let _ = send.send(StampedAnswer {
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

    /// Write the lines from the cached answer at `now_ms`.
    ///
    /// **Every line is omitted when it has no source.** Nothing here invents a
    /// zero, a "never" that was not read, or a device name that was not
    /// recorded — an absent fact draws nothing, which is what makes the lines
    /// that are there worth reading.
    fn rebuild(&mut self, now_ms: i64) -> bool {
        self.lines_minute = now_ms / 60_000;
        let lang = self.lang;
        let mut lines: Vec<CloudLine> = Vec::new();
        let mut note = None;
        let selected_file = self.context.file.as_ref();

        if let Some(status) = &self.status {
            // ------------------------------------------- the selected file
            //
            // **The pane explains the selection** (D259 §4). A file is a
            // selectable thing with its own facts, and while one is selected
            // the document's own lines are not what anybody is asking about —
            // so they are replaced rather than added to. Clearing the
            // selection brings them straight back, because nothing here is
            // stored: the lines are a projection of what is selected now.
            if let Some(selected) = selected_file {
                lines.push(plain(lang, Key::CloudTabLabelFile, selected.name.clone()));
                // The type, and the size when this device has the bytes to
                // measure. A file nobody here holds has no local size and is
                // not given a guessed one.
                let mut kind = selected.mime.clone();
                if let Some(size) = status.file.as_ref().and_then(|file| file.size) {
                    kind = format!("{kind} · {}", koru_i18n::number::bytes(lang, size));
                }
                lines.push(plain(lang, Key::CloudTabLabelKind, kind));
                lines.push(plain(
                    lang,
                    Key::CloudTabLabelWhere,
                    // **Until the Store answers, nothing is claimed.** The
                    // poll that carries this file's address has not come back
                    // on the first frame after a click, and a pane that
                    // guessed would be wrong for exactly as long as a person
                    // is looking at it.
                    match status.file.as_ref() {
                        Some(file) => where_the_bytes_are(lang, file),
                        None => koru_i18n::t(lang, Key::CloudTabFileUnmeasured).to_owned(),
                    },
                ));
                note = status.file.as_ref().and_then(|file| file_note(lang, file));
            } else {
                let sync_on = status.sync.as_ref().is_some_and(|sync| sync.enabled);

                // -------------------------------------------- This document
                let mut synced = document_sentence(lang, sync_on, self.coverage.as_ref());
                let code = status.sync.as_ref().and_then(|s| s.error_code.as_deref());
                if let Some(code) = code {
                    synced = format!("{synced} · {code}");
                }
                lines.push(plain(lang, Key::CloudTabLabelDocument, synced));
                // **The Profile's connection, in the words Manage uses.** The
                // code above stays verbatim for support; this is the sentence
                // a person reads, and it is the same one the Profile row and
                // its workspace rows say about the same moment.
                let stopped = self.context.connection.filter(|c| c.is_stopped());
                if sync_on && (stopped.is_some() || code.is_some_and(refuses_service)) {
                    note = Some(stopped.unwrap_or(Connection::Unavailable).sentence(lang));
                }

                // ---------------------------------------------------- Local
                if let Some(local) = &status.local {
                    let from = match local.origin {
                        CloudChangeOrigin::Store | CloudChangeOrigin::ThisDevice => {
                            koru_i18n::t(lang, Key::CloudTabFromThisDevice).to_owned()
                        }
                        CloudChangeOrigin::Cloud => {
                            koru_i18n::t(lang, Key::CloudTabFromCloud).to_owned()
                        }
                        CloudChangeOrigin::PeerDevice => match self.device_name(&local.device_id) {
                            Some(name) => koru_i18n::fill(
                                koru_i18n::t(lang, Key::CloudTabFromDevice),
                                &[&name],
                            ),
                            None => koru_i18n::t(lang, Key::CloudTabFromAnotherDevice).to_owned(),
                        },
                    };
                    let changed = koru_i18n::fill(
                        koru_i18n::t(lang, Key::CloudTabLocalChanged),
                        &[&koru_i18n::time::relative_narrow(
                            lang,
                            local.changed_at,
                            now_ms,
                        )],
                    );
                    lines.push(CloudLine {
                        label: label(lang, Key::CloudTabLabelLocal),
                        value: format!("{changed} · {from}"),
                        detail: format!(
                            "{} · {from}",
                            crate::format::absolute_local(lang, local.changed_at)
                        ),
                    });
                }

                // ------------------------------------- Images, then Files
                //
                // **The document's own files, never the workspace's.** Two
                // lines because a picture and an archive are different things
                // to be waiting for, and a document with neither draws
                // neither — *0 of 0* is a sentence about nothing.
                let files = sync_on
                    .then_some(self.coverage.as_ref())
                    .flatten()
                    .and_then(|coverage| coverage.files.map(|files| (coverage, files)));
                let (images, others) = match files {
                    Some((coverage, files)) => file_counts(lang, coverage, &files),
                    None => (None, None),
                };
                if let Some(value) = images_value(lang, images, &self.context) {
                    lines.push(plain(lang, Key::CloudTabLabelImages, value));
                }
                if let Some(value) = others {
                    lines.push(plain(lang, Key::CloudTabLabelFiles, value));
                }
            }
        } else {
            if self.unavailable {
                note = Some(koru_i18n::t(lang, Key::CloudTabUnavailable).to_owned());
            } else if self.target.is_some() {
                note = Some(koru_i18n::t(lang, Key::CloudTabLoading).to_owned());
            }
            // What this device's editor already knows needs no Store to say:
            // a placeholder is on screen now, so its line is too.
            if selected_file.is_none()
                && let Some(value) = images_value(lang, None, &self.context)
            {
                lines.push(plain(lang, Key::CloudTabLabelImages, value));
            }
        }

        // Said once, plainly, under the lines — so a placeholder reads as a
        // wait rather than a failure. It explains the Images line, so it
        // appears only while that line names a missing picture.
        if selected_file.is_none() && self.context.missing_images > 0 {
            note = note.or_else(|| Some(koru_i18n::t(lang, Key::CloudTabImagesNote).to_owned()));
        }

        if self.copied {
            self.copied = false;
            note = Some(koru_i18n::t(lang, Key::CloudTabCopied).to_owned());
        }

        let changed = lines != self.lines || note != self.note;
        self.lines = lines;
        self.note = note;
        changed
    }

    fn device_name(&self, device_id: &Option<String>) -> Option<String> {
        let wanted = device_id.as_deref()?;
        self.context
            .devices
            .iter()
            .find(|(id, _)| id.eq_ignore_ascii_case(wanted))
            .map(|(_, name)| name.clone())
            .filter(|name| !name.is_empty())
    }
}

/// **Does Koru Cloud hold this document as it is now** — the one sentence
/// the *This document* line says.
///
/// *Synced* is the coverage claim for this document: its CRDT state has
/// reached Koru Cloud at the state this device holds, so an edit since reads
/// as *Not synced yet* until the next pass carries it. A Profile that does not
/// sync says so instead, because *not synced yet* would promise a sync that is
/// not coming; and a Store that has not measured says *not checked* rather
/// than guessing either way.
fn document_sentence(
    lang: Lang,
    sync_on: bool,
    coverage: Option<&CloudDocumentCoverage>,
) -> String {
    let key = match (sync_on, coverage.and_then(|coverage| coverage.synced)) {
        (false, _) => Key::CloudTabSyncOff,
        (true, Some(true)) => Key::CloudTabDocSynced,
        (true, Some(false)) => Key::CloudTabDocNotSynced,
        (true, None) => Key::CloudTabFileUnmeasured,
    };
    koru_i18n::t(lang, key).to_owned()
}

/// *7 of 7 synced* for the images and for the other files, or `None` for a
/// kind the document has none of.
///
/// Before any coverage has been measured the counts are known and the synced
/// half is not, so the line says *not checked* rather than *0 of 7* — a zero
/// nobody measured reads as seven files lost.
fn file_counts(
    lang: Lang,
    coverage: &CloudDocumentCoverage,
    files: &CloudDocumentFiles,
) -> (Option<String>, Option<String>) {
    let count = |synced: u32, total: u32| {
        (total > 0).then(|| match coverage.synced {
            None => koru_i18n::t(lang, Key::CloudTabFileUnmeasured).to_owned(),
            Some(_) => koru_i18n::fill(
                koru_i18n::t(lang, Key::CloudTabCountSynced),
                &[&synced.to_string(), &total.to_string()],
            ),
        })
    };
    (
        count(files.images_synced, files.images_total),
        count(files.files_synced, files.files_total),
    )
}

/// The Images line: Koru Cloud's count first, then what this device is
/// missing and who has it — two different facts about one set of pictures,
/// said side by side rather than merged into one number.
fn images_value(lang: Lang, synced: Option<String>, context: &CloudContext) -> Option<String> {
    let missing = (context.missing_images > 0).then(|| {
        let missing = koru_i18n::fill(
            koru_i18n::t(lang, Key::CloudTabImagesMissing),
            &[&context.missing_images.to_string()],
        );
        match context.missing_image_devices.is_empty() {
            true => missing,
            false => format!(
                "{missing} · {}",
                koru_i18n::fill(
                    koru_i18n::t(lang, Key::CloudTabImagesWaiting),
                    &[&context.missing_image_devices.join(", ")],
                )
            ),
        }
    });
    match (synced, missing) {
        (Some(synced), Some(missing)) => Some(format!("{synced} · {missing}")),
        (synced, missing) => synced.or(missing),
    }
}

/// **Where one file's bytes are** (D259 §4).
///
/// Two facts, never collapsed into one word: what *this device* holds and
/// what *Koru Cloud* holds. A single "state" would have to choose which it
/// meant, and *on this laptop only* and *on Koru Cloud only* are precisely
/// the two answers a person most needs told apart — one of them means their
/// file has no second copy, and the other means this machine can fetch it.
///
/// An unmeasured Cloud half is said as unmeasured rather than guessed at. The
/// engine answers `None` for an address no coverage pass has looked at, and
/// reading that as *missing* would be claiming a completeness nobody
/// computed — the same rule `coverage` follows for a Workspace, applied to
/// one file.
fn where_the_bytes_are(lang: Lang, file: &BlobFileReport) -> String {
    match (file.here, file.cloud.as_deref()) {
        (true, Some("durable")) => koru_i18n::t(lang, Key::CloudTabFileHereAndCloud).to_owned(),
        (true, Some("uploading")) => koru_i18n::t(lang, Key::CloudTabFileSendingNow).to_owned(),
        (true, Some("missing")) => koru_i18n::t(lang, Key::CloudTabFileHereOnly).to_owned(),
        (false, Some("durable")) => koru_i18n::t(lang, Key::CloudTabFileCloudOnly).to_owned(),
        (false, Some("uploading" | "missing")) => {
            koru_i18n::t(lang, Key::CloudTabFileNowhere).to_owned()
        }
        // Nothing has measured the Cloud half — or it answered a word this
        // build does not know, which is the same amount of knowledge. The
        // local half is still a fact this device owns, so it is still said.
        (here, _) => format!(
            "{} · {}",
            koru_i18n::t(
                lang,
                match here {
                    true => Key::CloudTabFileHere,
                    false => Key::CloudTabFileNotHere,
                }
            ),
            koru_i18n::t(lang, Key::CloudTabFileUnmeasured),
        ),
    }
}

/// **The one honest sentence under a selected file** (D259 §4).
///
/// Why first, then where. A hold is the only thing here a person can act on,
/// so it outranks a description of the state it caused — telling somebody a
/// file is *not on Koru Cloud* while withholding *because you paused it* is
/// the §13 failure at the scale of one file.
fn file_note(lang: Lang, file: &BlobFileReport) -> Option<String> {
    if let Some(hold) = file.hold.as_deref() {
        return Some(hold_sentence(lang, hold));
    }
    match (file.here, file.cloud.as_deref()) {
        (_, Some("durable")) if file.here => {
            Some(koru_i18n::t(lang, Key::CloudTabFileNoteSafe).to_owned())
        }
        (false, Some("durable")) => {
            Some(koru_i18n::t(lang, Key::CloudTabFileNoteMissing).to_owned())
        }
        (_, Some("uploading")) => Some(koru_i18n::t(lang, Key::CloudTabFileNoteSending).to_owned()),
        (true, Some("missing")) => {
            Some(koru_i18n::t(lang, Key::CloudTabFileNoteHereOnly).to_owned())
        }
        // Nothing measured, or nothing to add that the line above did not
        // already say. An absent note draws nothing, which is what makes the
        // notes that are there worth reading.
        _ => None,
    }
}

/// One hold, as a sentence. **One place**, because a Workspace's row and a
/// selected file's note are the same fact at two scales, and two copies of
/// this map is two places for them to start disagreeing.
///
/// **Also Manage's per-workspace progress rows**
/// (`manage::profiles::progress`), which is why it is `pub(crate)`: the
/// same hold must read the same way on both surfaces.
pub(crate) fn hold_sentence(lang: Lang, code: &str) -> String {
    let key = match code {
        "network_metered" => Some(Key::CloudTabPausedMetered),
        "network_low_data" => Some(Key::CloudTabPausedLowData),
        "network_offline" => Some(Key::CloudTabWaitingOffline),
        // **Not a fault, and the branch below says so itself.** Cloud has no
        // bytes for a file this Workspace references because the device
        // holding them has not uploaded them yet — the ordinary state while
        // another machine is shut. Shown raw on the first hardware run, it
        // read as damage to somebody's pictures.
        "object_missing" => Some(Key::CloudTabWaitingHolder),
        "mode_refuses_transport" => Some(Key::CloudTabRefusedMode),
        "paused_by_person" => Some(Key::CloudTabPausedByYou),
        // The state a person lands in the moment they switch the Profile
        // off, and the one they will see most often. A raw code there would
        // make the ordinary answer look like a fault.
        "cloud_access_unavailable" => Some(Key::CloudTabCloudOff),
        // Koru Cloud refusing to serve is the Profile's connection, not a
        // hold on this Workspace, so it reads as the Profile row reads.
        code if refuses_service(code) => Some(Connection::Unavailable.key()),
        // The same for the two refusals Koru Cloud words itself (Q12): they
        // are about the account and this build, not about this Workspace,
        // and each already has its sentence.
        code if service_damaged(code) => Some(Connection::AccountProblem.key()),
        UNIT_FORMAT_TOO_NEW => Some(Connection::NeedsUpdate.key()),
        _ => None,
    };
    match key {
        Some(key) => koru_i18n::t(lang, key).to_owned(),
        // A code is shown verbatim: it is a stable identifier a support
        // conversation can search for, not a sentence to translate.
        None => koru_i18n::fill(koru_i18n::t(lang, Key::CloudTabDocWaiting), &[code]),
    }
}

fn label(lang: Lang, key: Key) -> String {
    koru_i18n::t(lang, key).to_owned()
}

fn plain(lang: Lang, key: Key, value: String) -> CloudLine {
    CloudLine {
        label: label(lang, key),
        value,
        detail: String::new(),
    }
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

#[cfg(test)]
mod tests;
