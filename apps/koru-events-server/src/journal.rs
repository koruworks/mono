// SPDX-License-Identifier: MPL-2.0

//! The journal: `events.db`, and everything that is true about a stored event.
//!
//! **Pragmas are the cache profile; schema versioning is the durable
//! convention — a deliberate hybrid.** WAL + `synchronous = NORMAL`, because
//! events are observations rather than user content: a tail lost to a power
//! cut is ADR-101's "stale, never wrong" case — domain events re-derive from
//! store state, and a client send that never got its receipt retries under
//! the same `event_id` and dedupes. But schema changes *migrate* (forward-only
//! steps, `user_version`, refuse-newer) rather than drop-and-rebuild, because
//! client-originated rows cannot be reconstructed from any source.
//!
//! **A sequence is never reused.** `seq` is `AUTOINCREMENT`, which is
//! load-bearing: pruning old rows must not re-issue a number under a live
//! subscriber's cursor. Do not "simplify" it away —
//! `pruning_never_reuses_a_sequence` is the test that says so.
//!
//! **Nothing here runs on a timer.** Retention is enforced amortized on the
//! write path — every [`PRUNE_CHECK_EVERY`] appends — so the cost lands on
//! the writer that caused it and an idle journal costs nothing.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use koru_events::telemetry::{Consent, TelemetryEvent, project};
use koru_events::wire::{AppendReceipt, EventPage, EventsStatus, RejectedEvent, StoredEvent};
use koru_events::{Envelope, validate};
use rusqlite::Connection;

/// One step per version, each in its own transaction, stamped into
/// `PRAGMA user_version` — the koru-store-sqlite convention, restated here
/// because that crate's helper carries the durable pragma profile this
/// journal deliberately does not use.
const JOURNAL_SCHEMA_VERSION: i64 = 4;

/// How often the write path looks at retention. Amortized, not timed.
const PRUNE_CHECK_EVERY: u32 = 256;
/// Instrumentation events are usage observations; two weeks answers every
/// question the outbox or a subscriber will ask of them.
const INSTRUMENTATION_RETENTION_MS: i64 = 14 * 24 * 60 * 60 * 1_000;
/// Nothing outlives this, whatever its class. **Public as days** because it is
/// half of what an activity page means: a bounded per-subject read that comes
/// back empty is only honest if the reader is told how far back it could have
/// reached (SPEC §9). The milliseconds are derived from the days rather than
/// declared beside them, so the number the page publishes and the number the
/// `DELETE` uses cannot drift.
pub const RETENTION_DAYS: u32 = 90;
const RETENTION_MS: i64 = RETENTION_DAYS as i64 * 24 * 60 * 60 * 1_000;
/// The journal's hard row cap; oldest go first.
const MAX_ROWS: u64 = 200_000;
/// The outbox is a spool for an uploader that does not exist yet; cap it so
/// consent granted and forgotten cannot grow a database forever.
const MAX_OUTBOX_ROWS: u64 = 50_000;

/// This device's telemetry answer under the opt-in contract (2026-10-11):
/// `"1"` yes, `"0"` no, absent unanswered. A row in `meta`, not a column.
const OPT_IN_KEY: &str = "telemetry_opt_in";
/// The key ADR-176's default-on builds read, absent meaning granted. Written
/// to match the answer so an older build cannot read silence as a yes; never
/// read by this one.
const LEGACY_CONSENT_KEY: &str = "telemetry_consent";

/// `document.updated` means "was edited around then", never "one edit":
/// at most one per document per this window. The registry doc string is the
/// contract; this is the number.
pub const UPDATED_THROTTLE_MS: i64 = 15 * 60 * 1_000;

/// What the store-side feed forwards — ids as strings (the koru-agent-wire
/// leaf rule: the feed's process knows the typed ids, this one does not need
/// to), revisions as the opaque bytes the projection compares.
#[derive(Clone, Debug)]
pub enum FeedNote {
    /// A document was projected at this revision.
    Projected {
        workspace: String,
        document: String,
        revision: Vec<u8>,
    },
    /// A document was dropped for good — not binned.
    Removed { workspace: String, document: String },
    /// A workspace's listing changed; every document's row-local trash flag.
    Listing {
        workspace: String,
        documents: Vec<(String, bool)>,
    },
    /// One content-free post-commit synchronized reminder wake.
    ReminderChanged {
        profile: String,
        change: ReminderWake,
    },
    /// A content-free post-commit personal-dictionary wake.
    DictionaryChanged { profile: String },
    /// A content-free post-commit wake for a Profile identity unit that
    /// arrived from a peer or the Cloud. The same two events a local rename
    /// or appearance change publishes from its route, so a consumer reacts to
    /// one pair of names whatever device made the change.
    ProfileIdentityChanged {
        profile: String,
        change: IdentityWake,
    },
    /// The unthrottled semantic due diff from the projection.
    DueChanged {
        workspace: String,
        document: String,
        entity: String,
    },
    /// A present-to-absent node transition from two current projections.
    EntityRemoved {
        workspace: String,
        document: String,
        entity: String,
    },
    /// A workspace's task rows changed in the read model. Content-free, and
    /// already coalesced by the feed to one per workspace per tick.
    TasksChanged { workspace: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReminderWake {
    Rule,
    Action,
    History,
    Settings,
}

/// Which identity fact an imported Profile identity unit moved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdentityWake {
    Name,
    Appearance,
}

/// One document the baseline seeding pass already knew about.
#[derive(Clone, Debug)]
pub struct BaselineEntry {
    pub workspace: String,
    pub document: String,
    pub revision: Vec<u8>,
    pub trashed: bool,
}

#[derive(Debug)]
pub enum JournalError {
    /// The database was written by a newer build; refuse rather than rewrite.
    SchemaTooNew {
        found: i64,
        understood: i64,
    },
    Storage(String),
}

impl std::fmt::Display for JournalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JournalError::SchemaTooNew { found, understood } => write!(
                f,
                "events.db is schema {found}, this build understands {understood}; a newer Koru wrote it"
            ),
            JournalError::Storage(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for JournalError {}

impl From<rusqlite::Error> for JournalError {
    fn from(error: rusqlite::Error) -> Self {
        JournalError::Storage(error.to_string())
    }
}

type Result<T> = std::result::Result<T, JournalError>;

pub struct Journal {
    conn: Connection,
    path: PathBuf,
    appends_since_prune: u32,
}

impl Journal {
    /// Open (or create) `events.db` under `dir`.
    pub fn open(dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(dir)
            .map_err(|error| JournalError::Storage(format!("events dir: {error}")))?;
        let path = dir.join("events.db");
        let conn = Connection::open(&path)?;
        configure(&conn)?;
        migrate(&conn)?;
        let mut journal = Self {
            conn,
            path,
            appends_since_prune: 0,
        };
        journal.settle_consent()?;
        Ok(journal)
    }

    #[cfg(test)]
    pub fn in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        configure(&conn)?;
        migrate(&conn)?;
        let mut journal = Self {
            conn,
            path: PathBuf::new(),
            appends_since_prune: 0,
        };
        journal.settle_consent()?;
        Ok(journal)
    }

    /// Append a batch: validate, dedupe by `event_id`, spool the exportable
    /// ones when consent is on — one transaction, then the caller notifies.
    ///
    /// Order within the batch is preserved. A rejected event rejects alone;
    /// the rest of the batch still lands.
    pub fn append(&mut self, events: &[Envelope], now_ms: i64) -> Result<AppendReceipt> {
        self.append_with_export(events, now_ms, true)
    }

    /// Append to a durable local operational journal without creating a
    /// telemetry outbox row. Hosted service events use this boundary because
    /// desktop consent/export state has no authority over a server process.
    pub fn append_local(&mut self, events: &[Envelope], now_ms: i64) -> Result<AppendReceipt> {
        self.append_with_export(events, now_ms, false)
    }

    fn append_with_export(
        &mut self,
        events: &[Envelope],
        now_ms: i64,
        export: bool,
    ) -> Result<AppendReceipt> {
        let consent = self.consent()?;
        let tx = self.conn.transaction()?;
        let mut receipt = append_events(&tx, events, now_ms, &consent, export)?;
        tx.commit()?;
        receipt.last_seq = self.last_seq()?;
        self.appends_since_prune += receipt.accepted;
        if self.appends_since_prune >= PRUNE_CHECK_EVERY {
            self.appends_since_prune = 0;
            self.prune(now_ms)?;
        }
        Ok(receipt)
    }

    /// One page after a cursor, oldest first.
    pub fn read_after(
        &self,
        after: u64,
        limit: u32,
        name_prefix: Option<&str>,
        workspace: Option<&str>,
    ) -> Result<EventPage> {
        let limit = limit.clamp(1, 1_000);
        let mut sql = format!("{SELECT_EVENT} WHERE seq > ?1");
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = vec![Box::new(after as i64)];
        if let Some(prefix) = name_prefix {
            sql.push_str(&format!(" AND name LIKE ?{} ESCAPE '\\'", params.len() + 1));
            params.push(Box::new(format!("{}%", like_escape(prefix))));
        }
        if let Some(workspace) = workspace {
            sql.push_str(&format!(" AND workspace = ?{}", params.len() + 1));
            params.push(Box::new(workspace.to_string()));
        }
        sql.push_str(&format!(" ORDER BY seq LIMIT ?{}", params.len() + 1));
        params.push(Box::new(limit as i64));
        let mut statement = self.conn.prepare(&sql)?;
        let refs: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|param| param.as_ref()).collect();
        let mut rows = statement.query(refs.as_slice())?;
        let mut events = Vec::new();
        let mut next_after = after;
        while let Some(row) = rows.next()? {
            let stored = stored_event(row)?;
            next_after = stored.seq;
            events.push(stored);
        }
        Ok(EventPage { events, next_after })
    }

    /// **One subject's events, and nothing else** — the read behind a task's
    /// activity (SPEC §9: a bounded, paginated per-subject query, never a
    /// journal scan per pane).
    ///
    /// Deliberately the same shape as [`read_after`](Self::read_after): a
    /// `seq` cursor, oldest first, a clamped limit, and `next_after` to
    /// continue. A pane wanting the newest first pages from the end and
    /// reverses; inventing a second ordering here would give the journal two
    /// cursor conventions, and a cursor that means different things in two
    /// places is how a page gets silently skipped.
    ///
    /// The subject match is **exact on both halves**. Kind is part of the
    /// identity — a `task:` subject and a `block:` reference to one node are
    /// different subjects (SPEC §3) — and the id is compared whole, never as
    /// a prefix, so one task's activity can never be answered by another's.
    pub fn read_subject(
        &self,
        kind: koru_events::SubjectKind,
        id: &str,
        after: u64,
        limit: u32,
    ) -> Result<EventPage> {
        let limit = limit.clamp(1, 1_000);
        let sql = format!(
            "{SELECT_EVENT} WHERE seq > ?1 AND subject_kind = ?2 AND subject_id = ?3 \
             ORDER BY seq LIMIT ?4"
        );
        let mut statement = self.conn.prepare(&sql)?;
        let mut rows = statement.query(rusqlite::params![
            after as i64,
            subject_kind_token(kind),
            id,
            limit as i64,
        ])?;
        let mut events = Vec::new();
        let mut next_after = after;
        while let Some(row) = rows.next()? {
            let stored = stored_event(row)?;
            next_after = stored.seq;
            events.push(stored);
        }
        Ok(EventPage { events, next_after })
    }

    pub fn last_seq(&self) -> Result<u64> {
        let seq: i64 =
            self.conn
                .query_row("SELECT COALESCE(MAX(seq), 0) FROM events", [], |row| {
                    row.get(0)
                })?;
        Ok(seq.max(0) as u64)
    }

    // ------------------------------------------------------------- the feed

    /// Apply the store feed's notes, deriving and appending domain events.
    /// Returns what was appended so the caller can notify subscribers.
    ///
    /// Before the baseline is seeded every note only records state — an
    /// upgrade on an existing store must not announce every document as born
    /// today.
    pub fn apply_notes(
        &mut self,
        notes: &[FeedNote],
        ctx: &koru_events::EmitContext,
        now_ms: i64,
    ) -> Result<Vec<Envelope>> {
        let seeded = self.baseline_seeded()?;
        let consent = self.consent()?;
        let tx = self.conn.transaction()?;
        let mut derived: Vec<Envelope> = Vec::new();
        // A workspace this journal has never seen, from any writer — the
        // coverage the app's own `workspace.created` cannot have, because it
        // fires only where a person clicked in this window.
        for workspace in notes.iter().filter_map(note_workspace) {
            let known = tx.execute(
                "INSERT INTO workspace_state (workspace, first_seen_ms) VALUES (?1, ?2) \
                 ON CONFLICT(workspace) DO NOTHING",
                rusqlite::params![workspace, now_ms],
            )?;
            if seeded && known > 0 {
                derived.push(workspace_event(
                    "workspace.observed",
                    workspace,
                    ctx,
                    now_ms,
                ));
            }
        }
        for note in notes {
            match note {
                FeedNote::Projected {
                    workspace,
                    document,
                    revision,
                } => {
                    let known: Option<(Vec<u8>, i64)> = tx
                        .query_row(
                            "SELECT revision, last_updated_emit_ms FROM doc_state \
                             WHERE workspace = ?1 AND document = ?2",
                            rusqlite::params![workspace, document],
                            |row| Ok((row.get(0)?, row.get(1)?)),
                        )
                        .map(Some)
                        .or_else(ignore_missing)?;
                    match known {
                        None => {
                            // Creation counts as activity: the throttle clock
                            // starts now, so "created" is not chased by an
                            // "updated" for the first keystroke.
                            tx.execute(
                                "INSERT INTO doc_state (workspace, document, revision, trashed, \
                                 last_updated_emit_ms) VALUES (?1, ?2, ?3, 0, ?4)",
                                rusqlite::params![workspace, document, revision, now_ms],
                            )?;
                            if seeded {
                                derived.push(document_event(
                                    "document.created",
                                    workspace,
                                    document,
                                    ctx,
                                    now_ms,
                                ));
                            }
                        }
                        Some((known_revision, _)) if known_revision.is_empty() => {
                            // A baseline row whose document had never been
                            // projected: it existed before this journal, so
                            // its first projection is neither creation nor
                            // an edit worth announcing — record and move on.
                            tx.execute(
                                "UPDATE doc_state SET revision = ?3 \
                                 WHERE workspace = ?1 AND document = ?2",
                                rusqlite::params![workspace, document, revision],
                            )?;
                        }
                        Some((known_revision, last_emit)) if known_revision != *revision => {
                            let throttled = now_ms - last_emit < UPDATED_THROTTLE_MS;
                            tx.execute(
                                "UPDATE doc_state SET revision = ?3, last_updated_emit_ms = ?4 \
                                 WHERE workspace = ?1 AND document = ?2",
                                rusqlite::params![
                                    workspace,
                                    document,
                                    revision,
                                    if throttled || !seeded {
                                        last_emit
                                    } else {
                                        now_ms
                                    },
                                ],
                            )?;
                            if seeded && !throttled {
                                derived.push(document_event(
                                    "document.updated",
                                    workspace,
                                    document,
                                    ctx,
                                    now_ms,
                                ));
                            }
                        }
                        Some(_) => {} // idempotent re-projection: same revision, nothing new
                    }
                }
                FeedNote::Removed {
                    workspace,
                    document,
                } => {
                    let known = tx.execute(
                        "DELETE FROM doc_state WHERE workspace = ?1 AND document = ?2",
                        rusqlite::params![workspace, document],
                    )?;
                    if seeded && known > 0 {
                        derived.push(document_event(
                            "document.removed",
                            workspace,
                            document,
                            ctx,
                            now_ms,
                        ));
                    }
                }
                FeedNote::Listing {
                    workspace,
                    documents,
                } => {
                    // A document the listing no longer names is gone for good
                    // — permanent deletion edits the manifest, and this note
                    // is the only immediate word of it (the projector's own
                    // `removed` waits for a sweep). A cross-workspace
                    // transfer reads as removed-here/created-there, which is
                    // what it looks like from either workspace's side.
                    let known: Vec<String> = {
                        let mut statement =
                            tx.prepare("SELECT document FROM doc_state WHERE workspace = ?1")?;
                        statement
                            .query_map([workspace], |row| row.get::<_, String>(0))?
                            .collect::<std::result::Result<Vec<_>, _>>()?
                    };
                    for document in known {
                        if documents.iter().any(|(listed, _)| *listed == document) {
                            continue;
                        }
                        tx.execute(
                            "DELETE FROM doc_state WHERE workspace = ?1 AND document = ?2",
                            rusqlite::params![workspace, document],
                        )?;
                        if seeded {
                            derived.push(document_event(
                                "document.removed",
                                workspace,
                                &document,
                                ctx,
                                now_ms,
                            ));
                        }
                    }
                    for (document, trashed) in documents {
                        let known: Option<bool> = tx
                            .query_row(
                                "SELECT trashed FROM doc_state \
                                 WHERE workspace = ?1 AND document = ?2",
                                rusqlite::params![workspace, document],
                                |row| row.get::<_, i64>(0).map(|flag| flag != 0),
                            )
                            .map(Some)
                            .or_else(ignore_missing)?;
                        match known {
                            // A listing can name a document before it is
                            // ever projected. Deliberately NOT recorded: a
                            // row minted here would make the projection that
                            // follows look like an update, and the diff will
                            // see any trash transition once the row exists.
                            None => {}
                            Some(known_trashed) if known_trashed != *trashed => {
                                tx.execute(
                                    "UPDATE doc_state SET trashed = ?3 \
                                     WHERE workspace = ?1 AND document = ?2",
                                    rusqlite::params![workspace, document, *trashed as i64],
                                )?;
                                if seeded {
                                    derived.push(document_event(
                                        if *trashed {
                                            "document.trashed"
                                        } else {
                                            "document.restored"
                                        },
                                        workspace,
                                        document,
                                        ctx,
                                        now_ms,
                                    ));
                                }
                            }
                            Some(_) => {}
                        }
                    }
                }
                FeedNote::ReminderChanged { profile, change } => {
                    if !seeded {
                        continue;
                    }
                    let Ok(profile_id) = profile.parse::<uuid::Uuid>() else {
                        continue;
                    };
                    let draft: koru_events::Draft = match change {
                        ReminderWake::Rule => {
                            koru_events::catalog::ReminderRuleChanged { profile_id }.into()
                        }
                        ReminderWake::Action => {
                            koru_events::catalog::ReminderActionAppended { profile_id }.into()
                        }
                        ReminderWake::History => {
                            koru_events::catalog::ReminderHistoryCleared { profile_id }.into()
                        }
                        ReminderWake::Settings => {
                            koru_events::catalog::ReminderSettingsChanged { profile_id }.into()
                        }
                    };
                    derived.push(draft.build(ctx, koru_events::EventId::mint(), now_ms));
                }
                FeedNote::DictionaryChanged { profile } => {
                    if !seeded {
                        continue;
                    }
                    let Ok(profile_id) = profile.parse::<uuid::Uuid>() else {
                        continue;
                    };
                    let draft: koru_events::Draft =
                        koru_events::catalog::ProfileDictionaryChanged { profile_id }.into();
                    derived.push(draft.build(ctx, koru_events::EventId::mint(), now_ms));
                }
                FeedNote::ProfileIdentityChanged { profile, change } => {
                    if !seeded {
                        continue;
                    }
                    let Ok(profile_id) = profile.parse::<uuid::Uuid>() else {
                        continue;
                    };
                    let draft: koru_events::Draft = match change {
                        IdentityWake::Name => {
                            koru_events::catalog::ProfileRenamed { profile_id }.into()
                        }
                        IdentityWake::Appearance => {
                            koru_events::catalog::ProfileAppearanceChanged { profile_id }.into()
                        }
                    };
                    derived.push(draft.build(ctx, koru_events::EventId::mint(), now_ms));
                }
                FeedNote::DueChanged {
                    workspace,
                    document,
                    entity,
                } => {
                    if !seeded {
                        continue;
                    }
                    if let (Ok(workspace_id), Ok(document_id), Ok(entity_id)) =
                        (workspace.parse(), document.parse(), entity.parse())
                    {
                        let draft: koru_events::Draft = koru_events::catalog::DueChanged {
                            workspace_id,
                            document_id,
                            entity_id,
                        }
                        .into();
                        derived.push(draft.build(ctx, koru_events::EventId::mint(), now_ms));
                    }
                }
                FeedNote::TasksChanged { workspace } => {
                    if !seeded {
                        continue;
                    }
                    if let Ok(workspace_id) = workspace.parse() {
                        let draft: koru_events::Draft =
                            koru_events::catalog::TasksChanged { workspace_id }.into();
                        derived.push(draft.build(ctx, koru_events::EventId::mint(), now_ms));
                    }
                }
                FeedNote::EntityRemoved {
                    workspace,
                    document,
                    entity,
                } => {
                    if !seeded {
                        continue;
                    }
                    if let (Ok(workspace_id), Ok(document_id), Ok(entity_id)) =
                        (workspace.parse(), document.parse(), entity.parse())
                    {
                        let draft: koru_events::Draft = koru_events::catalog::EntityRemoved {
                            workspace_id,
                            document_id,
                            entity_id,
                        }
                        .into();
                        derived.push(draft.build(ctx, koru_events::EventId::mint(), now_ms));
                    }
                }
            }
        }
        let accepted = if derived.is_empty() {
            0
        } else {
            append_events(&tx, &derived, now_ms, &consent, true)?.accepted
        };
        // State cursors and the events they suppress are one atomic fact. If
        // the event insert fails, retrying the same feed note must see the old
        // cursor and derive the event again.
        tx.commit()?;
        self.appends_since_prune += accepted;
        if self.appends_since_prune >= PRUNE_CHECK_EVERY {
            self.appends_since_prune = 0;
            if let Err(error) = self.prune(now_ms) {
                // The event batch is already durable and must still wake
                // subscribers; retention retries on a later append.
                tracing::warn!("events retention pass failed after feed commit: {error}");
            }
        }
        Ok(derived)
    }

    /// Record what already existed, emitting nothing, and mark the baseline.
    /// Idempotent: a second call on a seeded journal is a no-op.
    pub fn seed_baseline(&mut self, entries: &[BaselineEntry]) -> Result<bool> {
        if self.baseline_seeded()? {
            return Ok(false);
        }
        let tx = self.conn.transaction()?;
        for entry in entries {
            tx.execute(
                "INSERT INTO doc_state (workspace, document, revision, trashed, \
                 last_updated_emit_ms) VALUES (?1, ?2, ?3, ?4, 0) \
                 ON CONFLICT(workspace, document) DO UPDATE SET revision = ?3, trashed = ?4",
                rusqlite::params![
                    entry.workspace,
                    entry.document,
                    entry.revision,
                    entry.trashed as i64,
                ],
            )?;
            // Workspaces that already existed stay quiet too — the same
            // created-storm guard, one level up.
            tx.execute(
                "INSERT INTO workspace_state (workspace, first_seen_ms) VALUES (?1, 0) \
                 ON CONFLICT(workspace) DO NOTHING",
                rusqlite::params![entry.workspace],
            )?;
        }
        tx.execute(
            "INSERT INTO meta (key, value) VALUES ('baseline_seeded', '1') \
             ON CONFLICT(key) DO UPDATE SET value = '1'",
            [],
        )?;
        tx.commit()?;
        Ok(true)
    }

    pub fn baseline_seeded(&self) -> Result<bool> {
        Ok(self.meta("baseline_seeded")?.as_deref() == Some("1"))
    }

    /// Whether this journal has announced its installation — `app.installed`
    /// is once per journal, and one journal is one channel installation.
    pub fn installed_recorded(&self) -> Result<bool> {
        Ok(self.meta("installed_recorded")?.as_deref() == Some("1"))
    }

    /// Marked after the announcement lands, so a failed append retries on
    /// the next open rather than never saying it.
    pub fn mark_installed_recorded(&mut self) -> Result<()> {
        self.conn.execute(
            "INSERT INTO meta (key, value) VALUES ('installed_recorded', '1') \
             ON CONFLICT(key) DO UPDATE SET value = '1'",
            [],
        )?;
        Ok(())
    }

    // -------------------------------------------------- consent + the outbox

    /// **Only an explicit yes on this device allows an export.** The answer is
    /// [`OPT_IN_KEY`] in `meta`; absent means nobody has answered under the
    /// opt-in contract, and that exports nothing.
    ///
    /// The key `telemetry_consent` that ADR-176's default-on build read is
    /// deliberately **not** read here: under it, absent meant granted and an
    /// old build wrote `"1"` without anybody choosing, so neither of its
    /// values is evidence that a person agreed. An upgrade therefore reads as
    /// unanswered and is asked again (ADR-176-consenton's
    /// 2026-10-11 amendment).
    pub fn consent(&self) -> Result<Consent> {
        if self.meta(OPT_IN_KEY)?.as_deref() != Some("1") {
            return Ok(Consent::undecided());
        }
        Ok(Consent {
            granted: true,
            consent_version: self
                .meta("consent_version")?
                .filter(|version| !version.is_empty())
                .unwrap_or_else(|| koru_events::telemetry::DEFAULT_CONSENT_VERSION.to_string()),
        })
    }

    /// Whether this device holds an answer — yes or no — under the opt-in
    /// contract. `false` is what makes a surface ask.
    pub fn consent_answered(&self) -> Result<bool> {
        Ok(matches!(self.meta(OPT_IN_KEY)?.as_deref(), Some("0" | "1")))
    }

    /// Record this device's answer.
    ///
    /// **Withdrawing** deletes the unsent spool *and the installation id* — the
    /// PRODUCT_METRICS rule: off means collection stops, what was queued goes,
    /// and what identified this installation goes with it.
    ///
    /// **Granting** from any other state also starts from an empty spool and
    /// no installation id, so an event recorded before the yes — under the old
    /// default, or while the answer was no — is never uploaded, even now that
    /// consent exists. Granting while already granted changes nothing, so a
    /// repeated yes does not throw away what the yes already queued.
    ///
    /// All of it in one transaction, which is the whole argument for keeping
    /// the id here rather than in a file beside the journal: an answer is a
    /// single atomic act, and opting back in mints a new id rather than
    /// resuming an old one.
    ///
    /// The legacy `telemetry_consent` key is written to match, so a build from
    /// before the opt-in contract that opens this journal — a downgrade —
    /// keeps the person's answer instead of reading its absence as a yes.
    pub fn set_consent(&mut self, granted: bool, consent_version: &str) -> Result<()> {
        let already_granted = self.consent()?.granted;
        let tx = self.conn.transaction()?;
        let value = if granted { "1" } else { "0" };
        for key in [OPT_IN_KEY, LEGACY_CONSENT_KEY] {
            tx.execute(
                "INSERT INTO meta (key, value) VALUES (?1, ?2) \
                 ON CONFLICT(key) DO UPDATE SET value = ?2",
                rusqlite::params![key, value],
            )?;
        }
        tx.execute(
            "INSERT INTO meta (key, value) VALUES ('consent_version', ?1) \
             ON CONFLICT(key) DO UPDATE SET value = ?1",
            rusqlite::params![consent_version],
        )?;
        if !granted || !already_granted {
            tx.execute("DELETE FROM telemetry_outbox", [])?;
            tx.execute("DELETE FROM meta WHERE key = 'installation_id'", [])?;
        }
        tx.commit()?;
        Ok(())
    }

    /// **What opening a journal settles about consent**, every time, before
    /// anything can be appended or drained.
    ///
    /// With no answer on record — a fresh journal, or one an older build wrote
    /// under the default-on rule — nothing it holds may ever leave: the outbox
    /// is emptied and the installation id is deleted, so a later yes starts
    /// clean. The legacy key is set to refuse, so an older build that opens
    /// this journal afterwards does not resume sending by default either.
    /// With an answer on record this changes nothing.
    ///
    /// No schema change: these are rows in the existing `meta` key-value table,
    /// and `JOURNAL_SCHEMA_VERSION` does not move.
    fn settle_consent(&mut self) -> Result<()> {
        if self.consent_answered()? {
            return Ok(());
        }
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM telemetry_outbox", [])?;
        tx.execute("DELETE FROM meta WHERE key = 'installation_id'", [])?;
        tx.execute(
            "INSERT INTO meta (key, value) VALUES (?1, '0') \
             ON CONFLICT(key) DO UPDATE SET value = '0'",
            rusqlite::params![LEGACY_CONSENT_KEY],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// The anonymous installation id, minted on first ask.
    ///
    /// A v4 UUID and **never derived** from a device id, a profile, a path or
    /// a name ([ADR-39](../../../docs/decisions/ADR-026-050.md#adr-39-identity--identity-is-client-minted-document-scoped-and-cloud-ready)) — it is the only thing
    /// that lets two events be known to come from the same installation, and
    /// deriving it would make it a fingerprint of the machine instead.
    ///
    /// It lives beside consent because [`set_consent`](Self::set_consent)
    /// deletes it in the same transaction.
    pub fn installation_id(&mut self) -> Result<String> {
        if let Some(existing) = self.meta("installation_id")?
            && !existing.is_empty()
        {
            return Ok(existing);
        }
        let minted = uuid::Uuid::new_v4().to_string();
        self.conn.execute(
            "INSERT INTO meta (key, value) VALUES ('installation_id', ?1) \
             ON CONFLICT(key) DO UPDATE SET value = ?1",
            rusqlite::params![minted],
        )?;
        Ok(minted)
    }

    /// What would be uploaded, oldest first — the inspection window the
    /// Milestone 2 uploader will drain.
    pub fn telemetry_pending(&self, limit: u32) -> Result<Vec<TelemetryEvent>> {
        let mut statement = self
            .conn
            .prepare("SELECT body FROM telemetry_outbox ORDER BY seq LIMIT ?1")?;
        let mut rows = statement.query([limit.clamp(1, 5_000) as i64])?;
        let mut events = Vec::new();
        while let Some(row) = rows.next()? {
            let body: String = row.get(0)?;
            if let Ok(event) = serde_json::from_str(&body) {
                events.push(event);
            }
        }
        Ok(events)
    }

    /// Forget what an uploader confirmed was delivered.
    ///
    /// **The half the outbox was missing.** `telemetry_pending` hands events
    /// out and, until now, nothing put them back — so any uploader would have
    /// resent the same rows forever. Idempotent by construction: an id that is
    /// not there is not an error, which is what makes acking a batch twice
    /// after a crash harmless.
    ///
    /// Answers how many rows actually went, so a caller can notice the
    /// difference between "delivered" and "deleted" rather than assume it.
    pub fn telemetry_ack(&mut self, event_ids: &[String]) -> Result<u32> {
        if event_ids.is_empty() {
            return Ok(0);
        }
        let tx = self.conn.transaction()?;
        let mut removed = 0u32;
        {
            let mut statement = tx.prepare("DELETE FROM telemetry_outbox WHERE event_id = ?1")?;
            for event_id in event_ids {
                removed += statement.execute([event_id])? as u32;
            }
        }
        tx.commit()?;
        Ok(removed)
    }

    // ----------------------------------------------------------- the status

    pub fn status(&self) -> Result<EventsStatus> {
        let count = |sql: &str| -> Result<u64> {
            let count: i64 = self.conn.query_row(sql, [], |row| row.get(0))?;
            Ok(count.max(0) as u64)
        };
        let consent = self.consent()?;
        Ok(EventsStatus {
            disabled: false,
            total_events: count("SELECT COUNT(*) FROM events")?,
            domain_events: count("SELECT COUNT(*) FROM events WHERE class = 'domain'")?,
            instrumentation_events: count(
                "SELECT COUNT(*) FROM events WHERE class = 'instrumentation'",
            )?,
            last_seq: self.last_seq()?,
            pruned_events: self
                .meta("pruned_total")?
                .and_then(|value| value.parse().ok())
                .unwrap_or(0),
            outbox_pending: count("SELECT COUNT(*) FROM telemetry_outbox")?,
            telemetry_consent: consent.granted,
            telemetry_consent_answered: self.consent_answered()?,
            baseline_seeded: self.baseline_seeded()?,
            tracked_documents: count("SELECT COUNT(*) FROM doc_state")?,
            sse_subscribers: 0, // the state layer fills this in; the journal cannot know
            journal_bytes: std::fs::metadata(&self.path)
                .map(|meta| meta.len())
                .unwrap_or(0),
            // Per-run counters, filled in by the state layer for the same
            // reason `sse_subscribers` is: they belong to the worker, not to
            // the database.
            activity_queries: 0,
            activity_rows: 0,
        })
    }

    // ------------------------------------------------------------ retention

    fn prune(&mut self, now_ms: i64) -> Result<()> {
        let mut pruned = 0i64;
        pruned += self.conn.execute(
            "DELETE FROM events WHERE class = 'instrumentation' AND recorded_at_ms < ?1",
            [now_ms - INSTRUMENTATION_RETENTION_MS],
        )? as i64;
        pruned += self.conn.execute(
            "DELETE FROM events WHERE recorded_at_ms < ?1",
            [now_ms - RETENTION_MS],
        )? as i64;
        let rows: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))?;
        if rows as u64 > MAX_ROWS {
            pruned += self.conn.execute(
                "DELETE FROM events WHERE seq IN \
                 (SELECT seq FROM events ORDER BY seq LIMIT ?1)",
                [rows - MAX_ROWS as i64],
            )? as i64;
        }
        let outbox: i64 =
            self.conn
                .query_row("SELECT COUNT(*) FROM telemetry_outbox", [], |r| r.get(0))?;
        if outbox as u64 > MAX_OUTBOX_ROWS {
            self.conn.execute(
                "DELETE FROM telemetry_outbox WHERE seq IN \
                 (SELECT seq FROM telemetry_outbox ORDER BY seq LIMIT ?1)",
                [outbox - MAX_OUTBOX_ROWS as i64],
            )?;
        }
        if pruned > 0 {
            let total: i64 = self
                .meta("pruned_total")?
                .and_then(|value| value.parse().ok())
                .unwrap_or(0);
            self.conn.execute(
                "INSERT INTO meta (key, value) VALUES ('pruned_total', ?1) \
                 ON CONFLICT(key) DO UPDATE SET value = ?1",
                [(total + pruned).to_string()],
            )?;
        }
        Ok(())
    }

    #[cfg(test)]
    pub fn prune_now(&mut self, now_ms: i64) -> Result<()> {
        self.prune(now_ms)
    }

    fn meta(&self, key: &str) -> Result<Option<String>> {
        self.conn
            .query_row("SELECT value FROM meta WHERE key = ?1", [key], |row| {
                row.get(0)
            })
            .map(Some)
            .or_else(ignore_missing)
    }
}

fn ignore_missing<T>(error: rusqlite::Error) -> std::result::Result<Option<T>, JournalError> {
    match error {
        rusqlite::Error::QueryReturnedNoRows => Ok(None),
        other => Err(other.into()),
    }
}

fn append_events(
    tx: &rusqlite::Transaction<'_>,
    events: &[Envelope],
    now_ms: i64,
    consent: &Consent,
    export: bool,
) -> Result<AppendReceipt> {
    let mut receipt = AppendReceipt {
        accepted: 0,
        duplicates: 0,
        rejected: Vec::new(),
        last_seq: 0,
    };
    for (index, envelope) in events.iter().enumerate() {
        if let Err(violation) = validate(envelope) {
            receipt.rejected.push(RejectedEvent {
                index: index as u32,
                event_id: envelope.event_id.to_string(),
                code: violation.code().to_string(),
                message: violation.to_string(),
            });
            continue;
        }
        let spec = koru_events::registry::spec(envelope.name.as_str())
            .expect("validate admits only registered names");
        let inserted = tx.execute(
            "INSERT INTO events (event_id, name, class, version, occurred_at_ms, \
             recorded_at_ms, actor_kind, actor_id, actor_token, device_id, session_id, \
             profile_id, workspace, subject_kind, subject_id, correlation_id, causation_id, \
             attrs) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, \
             ?17, ?18) \
             ON CONFLICT(event_id) DO NOTHING",
            rusqlite::params![
                envelope.event_id.to_string(),
                envelope.name.as_str(),
                spec.class.as_str(),
                envelope.version,
                envelope.occurred_at_ms,
                now_ms,
                actor_kind(envelope),
                envelope.actor.id.map(|id| id.to_string()),
                envelope.actor.token.clone(),
                envelope.origin.device_id.map(|id| id.to_string()),
                envelope.origin.session_id.to_string(),
                envelope.scope.profile_id.map(|id| id.to_string()),
                envelope.scope.workspace_id.map(|id| id.to_string()),
                envelope.subject.as_ref().map(subject_kind),
                envelope.subject.as_ref().map(|subject| subject.id.clone()),
                envelope.correlation_id.map(|id| id.to_string()),
                envelope.causation_id.map(|id| id.to_string()),
                serde_json::to_string(&envelope.attrs)
                    .map_err(|error| JournalError::Storage(error.to_string()))?,
            ],
        )?;
        if inserted == 0 {
            receipt.duplicates += 1;
            continue;
        }
        receipt.accepted += 1;
        if export
            && consent.granted
            && let Some(exported) = project(envelope, consent)
        {
            tx.execute(
                "INSERT INTO telemetry_outbox (event_id, body, queued_at_ms) \
                 VALUES (?1, ?2, ?3) ON CONFLICT(event_id) DO NOTHING",
                rusqlite::params![
                    envelope.event_id.to_string(),
                    serde_json::to_string(&exported)
                        .map_err(|error| JournalError::Storage(error.to_string()))?,
                    now_ms,
                ],
            )?;
        }
    }
    Ok(receipt)
}

fn actor_kind(envelope: &Envelope) -> &'static str {
    match envelope.actor.kind {
        koru_events::ActorKind::User => "user",
        koru_events::ActorKind::Agent => "agent",
        koru_events::ActorKind::System => "system",
    }
}

/// Every column [`stored_event`] reads, in the order it reads them. One
/// declaration, because two queries answer the same rows and a column added
/// to one list and not the other is an off-by-one over `row.get(n)` that
/// nothing catches at compile time.
const SELECT_EVENT: &str = "SELECT seq, recorded_at_ms, event_id, name, version, \
     occurred_at_ms, actor_kind, actor_id, actor_token, device_id, session_id, \
     profile_id, workspace, subject_kind, subject_id, correlation_id, \
     causation_id, attrs FROM events";

fn subject_kind(subject: &koru_events::Subject) -> &'static str {
    subject_kind_token(subject.kind)
}

/// **The stored spelling of a subject kind, and the compiler holds it
/// exhaustive.** A new [`koru_events::SubjectKind`] fails to build here until
/// somebody decides what it is called on disk — which is the point: the token
/// is written into every row and read back by the per-subject query, so it is
/// storage, not a label.
fn subject_kind_token(kind: koru_events::SubjectKind) -> &'static str {
    match kind {
        koru_events::SubjectKind::Workspace => "workspace",
        koru_events::SubjectKind::Document => "document",
        // A task is a *node* addressed as a task, and its id carries the
        // whole `task:<workspace>:<document>:<node>` spelling — so this
        // column says which vocabulary the id belongs to, never which table
        // to look in.
        koru_events::SubjectKind::Task => "task",
    }
}

/// The other direction, over a value this process wrote. It cannot be
/// exhaustive — the column is text — so each arm is named and the fallback is
/// deliberate: a newer build's journal is refused at [`migrate`] rather than
/// read, so an unknown token here can only be a row this build's own
/// [`subject_kind_token`] wrote, and `document` is what that has always
/// meant. Reading it as a document keeps a stored event decodable, which is
/// the behavior a journal owes: stale, never wrong.
fn subject_kind_from_stored(stored: &str) -> koru_events::SubjectKind {
    match stored {
        "workspace" => koru_events::SubjectKind::Workspace,
        "task" => koru_events::SubjectKind::Task,
        "document" => koru_events::SubjectKind::Document,
        _ => koru_events::SubjectKind::Document,
    }
}

fn like_escape(prefix: &str) -> String {
    prefix
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

/// Which workspace a feed note is about — every note names one.
fn note_workspace(note: &FeedNote) -> Option<&str> {
    match note {
        FeedNote::Projected { workspace, .. }
        | FeedNote::Removed { workspace, .. }
        | FeedNote::Listing { workspace, .. } => Some(workspace),
        FeedNote::TasksChanged { workspace } => {
            workspace.parse::<uuid::Uuid>().is_ok().then_some(workspace)
        }
        FeedNote::DueChanged {
            workspace,
            document,
            entity,
        }
        | FeedNote::EntityRemoved {
            workspace,
            document,
            entity,
        } => [workspace, document, entity]
            .iter()
            .all(|value| value.parse::<uuid::Uuid>().is_ok())
            .then_some(workspace),
        FeedNote::ReminderChanged { .. }
        | FeedNote::DictionaryChanged { .. }
        | FeedNote::ProfileIdentityChanged { .. } => None,
    }
}

/// A derived workspace event: the workspace is both the scope and the
/// subject, which is what makes it joinable from either side.
fn workspace_event(
    name: &'static str,
    workspace: &str,
    ctx: &koru_events::EmitContext,
    now_ms: i64,
) -> Envelope {
    let mut draft = koru_events::Draft::new(koru_events::EventName::from_static(name))
        .subject(koru_events::Subject::workspace(workspace));
    if let Ok(id) = workspace.parse::<uuid::Uuid>() {
        draft = draft.workspace(koru_events::WorkspaceId(id));
    }
    draft.build(ctx, koru_events::EventId::mint(), now_ms)
}

/// A derived lifecycle event. The ids arrive as strings; ones that are not
/// uuids (a test's "doc-1") still make a valid subject, and the workspace
/// scope is set only when the string parses.
fn document_event(
    name: &'static str,
    workspace: &str,
    document: &str,
    ctx: &koru_events::EmitContext,
    now_ms: i64,
) -> Envelope {
    let mut draft = koru_events::Draft::new(koru_events::EventName::from_static(name))
        .subject(koru_events::Subject::document(document));
    if let Ok(id) = workspace.parse::<uuid::Uuid>() {
        draft = draft.workspace(koru_events::WorkspaceId(id));
    }
    draft.build(ctx, koru_events::EventId::mint(), now_ms)
}

fn stored_event(row: &rusqlite::Row<'_>) -> std::result::Result<StoredEvent, JournalError> {
    use koru_events as ev;
    let parse_id = |value: Option<String>| -> Option<uuid::Uuid> {
        value.and_then(|value| value.parse().ok())
    };
    let seq: i64 = row.get(0)?;
    let recorded_at_ms: i64 = row.get(1)?;
    let event_id: String = row.get(2)?;
    let name: String = row.get(3)?;
    let version: i64 = row.get(4)?;
    let occurred_at_ms: i64 = row.get(5)?;
    let actor_kind: String = row.get(6)?;
    let actor_id: Option<String> = row.get(7)?;
    let actor_token: Option<String> = row.get(8)?;
    let device_id: Option<String> = row.get(9)?;
    let session_id: String = row.get(10)?;
    let profile_id: Option<String> = row.get(11)?;
    let workspace: Option<String> = row.get(12)?;
    let subject_kind: Option<String> = row.get(13)?;
    let subject_id: Option<String> = row.get(14)?;
    let correlation_id: Option<String> = row.get(15)?;
    let causation_id: Option<String> = row.get(16)?;
    let attrs: String = row.get(17)?;
    let attrs: BTreeMap<String, ev::AttrValue> = serde_json::from_str(&attrs).unwrap_or_default();
    let envelope = ev::Envelope {
        event_id: ev::EventId(
            event_id
                .parse()
                .map_err(|_| JournalError::Storage("bad event_id".into()))?,
        ),
        name: ev::EventName::new(name),
        version: version as u16,
        occurred_at_ms,
        actor: ev::Actor {
            kind: match actor_kind.as_str() {
                "user" => ev::ActorKind::User,
                "agent" => ev::ActorKind::Agent,
                _ => ev::ActorKind::System,
            },
            id: parse_id(actor_id).map(ev::ActorId),
            token: actor_token,
        },
        origin: ev::Origin {
            device_id: parse_id(device_id).map(ev::DeviceId),
            session_id: ev::SessionId(session_id.parse().unwrap_or_default()),
        },
        scope: ev::Scope {
            profile_id: parse_id(profile_id).map(ev::ProfileId),
            workspace_id: parse_id(workspace).map(ev::WorkspaceId),
        },
        subject: match (subject_kind.as_deref(), subject_id) {
            (Some(kind), Some(id)) => Some(ev::Subject {
                kind: subject_kind_from_stored(kind),
                id,
                version: None,
            }),
            _ => None,
        },
        correlation_id: parse_id(correlation_id).map(ev::EventId),
        causation_id: parse_id(causation_id).map(ev::EventId),
        attrs,
    };
    Ok(StoredEvent {
        seq: seq.max(0) as u64,
        recorded_at_ms,
        envelope,
    })
}

/// WAL + `synchronous = NORMAL` — the cache profile, argued in the module
/// doc. `busy_timeout` covers the moment a checkpointer holds the file.
fn configure(conn: &Connection) -> Result<()> {
    conn.busy_timeout(std::time::Duration::from_secs(2))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    Ok(())
}

fn migrate(conn: &Connection) -> Result<()> {
    let found: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if found > JOURNAL_SCHEMA_VERSION {
        return Err(JournalError::SchemaTooNew {
            found,
            understood: JOURNAL_SCHEMA_VERSION,
        });
    }
    if found < 1 {
        conn.execute_batch(
            "BEGIN IMMEDIATE;
             CREATE TABLE IF NOT EXISTS events (
               seq            INTEGER PRIMARY KEY AUTOINCREMENT,
               event_id       TEXT NOT NULL UNIQUE,
               name           TEXT NOT NULL,
               class          TEXT NOT NULL,
               version        INTEGER NOT NULL,
               occurred_at_ms INTEGER NOT NULL,
               recorded_at_ms INTEGER NOT NULL,
               actor_kind     TEXT NOT NULL,
               actor_id       TEXT,
               actor_token    TEXT,
               device_id      TEXT,
               session_id     TEXT NOT NULL,
               profile_id     TEXT,
               workspace      TEXT,
               subject_kind   TEXT,
               subject_id     TEXT,
               correlation_id TEXT,
               causation_id   TEXT,
               attrs          TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS events_by_name ON events(name, seq);
             CREATE INDEX IF NOT EXISTS events_by_correlation ON events(correlation_id)
               WHERE correlation_id IS NOT NULL;
             CREATE TABLE IF NOT EXISTS doc_state (
               workspace            TEXT NOT NULL,
               document             TEXT NOT NULL,
               revision             BLOB NOT NULL,
               trashed              INTEGER NOT NULL DEFAULT 0,
               last_updated_emit_ms INTEGER NOT NULL DEFAULT 0,
               PRIMARY KEY (workspace, document)
             );
             CREATE TABLE IF NOT EXISTS telemetry_outbox (
               seq          INTEGER PRIMARY KEY AUTOINCREMENT,
               event_id     TEXT NOT NULL UNIQUE,
               body         TEXT NOT NULL,
               queued_at_ms INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             PRAGMA user_version = 1;
             COMMIT;",
        )?;
    }
    // **A step per version, and a shipped step is never edited again.**
    // `workspace_state` arrived after v1 had been stamped on real journals,
    // and adding it to the block above reached only *new* databases: an
    // existing one skipped the whole block, then failed every feed note with
    // "no such table" — silently, because the feed logs and carries on, so
    // derived document events simply stopped. That is the failure
    // `koru-store-sqlite`'s `Migration` doc records as `documents.trashed_by`,
    // and it is the reason the version below goes up instead.
    if found < 2 {
        conn.execute_batch(
            "BEGIN IMMEDIATE;
             CREATE TABLE IF NOT EXISTS workspace_state (
               workspace     TEXT PRIMARY KEY,
               first_seen_ms INTEGER NOT NULL
             );
             PRAGMA user_version = 2;
             COMMIT;",
        )?;
    }
    // **v2 created the table and left it empty, which was its own bug.**
    // `seed_baseline` is what fills it, and an upgraded journal already has
    // `baseline_seeded` set — so the seeding pass never ran again and every
    // workspace the journal had known for months announced itself as newly
    // observed on its next feed note. Seen live: two false sightings with
    // thirty-three more queued behind them. That is the created-storm the
    // document baseline exists to prevent, one level up.
    //
    // A separate step rather than a repaired v2, because v2 is stamped on
    // real journals now and a step that has run is never edited again — the
    // rule that was broken to get here. `doc_state` already names every
    // workspace this journal knows, so it *is* the baseline; `first_seen_ms
    // = 0` says "before this table existed". A fresh journal has an empty
    // `doc_state`, so this is a no-op there and seeding proceeds as usual.
    if found < 3 {
        conn.execute_batch(
            "BEGIN IMMEDIATE;
             INSERT INTO workspace_state (workspace, first_seen_ms)
               SELECT DISTINCT workspace, 0 FROM doc_state
               WHERE workspace NOT IN (SELECT workspace FROM workspace_state);
             PRAGMA user_version = 3;
             COMMIT;",
        )?;
    }
    // The per-subject read (`read_subject`) is what a task's activity is
    // answered from, and SPEC §9 says it is never a journal scan per pane —
    // so it gets an index rather than a promise. `(subject_kind, subject_id,
    // seq)` covers the whole `WHERE` plus the cursor's ordering; partial, so
    // the rows with no subject at all cost nothing to keep out of it.
    if found < 4 {
        conn.execute_batch(
            "BEGIN IMMEDIATE;
             CREATE INDEX IF NOT EXISTS events_by_subject
               ON events(subject_kind, subject_id, seq)
               WHERE subject_id IS NOT NULL;
             PRAGMA user_version = 4;
             COMMIT;",
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
