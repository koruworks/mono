// SPDX-License-Identifier: MPL-2.0

//! The per-workspace **system document** — custom attribute definitions and
//! the tag table.
//!
//! One auxiliary document per workspace, listed in the store manifest under
//! the kind token [`KIND_SYSTEM`](crate::explore::KIND_SYSTEM) and filtered
//! out of Explore's listing — the `KIND_FILE` precedent: a new sort of entry
//! is a new token and a renderer, never a schema change. Its content is two
//! root maps:
//!
//! ```text
//! System doc
//! ├── meta          LoroMap   system_schema_version
//! ├── definitions   LoroMap   definition id -> { key, label, type, … }
//! ├── tags          LoroMap   TagId -> { name, color, created, alias }
//! ├── lists         LoroMap   ListId -> { name, color, order, created, deleted, page_gens }
//! └── pages         LoroMap   bucket token -> { page_gens }
//! ```
//!
//! **The seam** ([ADR-176]): this module owns the document's *content* and
//! nothing about where it lives. Creating it lazily on first definition or
//! tag write, registering it in the workspace manifest, and reconciling
//! concurrent creation are `koru-workspace`'s, which can see the manifest:
//! open every `"koru-system"` entry, use the one whose [`DocumentId`] is
//! lowest as canonical ([`SystemDoc::canonical_document`]), and [`absorb`]
//! each duplicate into it. Duplicate manifest rows stay hidden and listed as
//! recovery anchors for offline replicas and outboxes. The union itself is
//! free: both documents share the same named root containers, so a CRDT import
//! *is* the union.
//!
//! **Tags are entities** (D5). A node stores a [`TagId`]; this table maps it
//! to a name and an optional accent color. Rename updates one record here and
//! rewrites nothing node-side; merge marks the losing tag as an alias of the
//! winner, and readers resolve through [`SystemDoc::resolve`] — no node is
//! ever re-pointed. Usage counts are **derived at query time** by whoever can
//! see the nodes; nothing here stores a count that would drift.
//!
//! **A tag's identity is its folded name, and its hierarchy is that name's `/`
//! path** ([ADR-185], and [`crate::tags`] for both rules). Neither costs a
//! record key: `#HOME` reuses `#home`'s record rather than minting a second,
//! `#fun/home` is a tag *named* `fun/home` whose parent record is created
//! beside it, and two peers who each minted `#home` offline are folded by
//! [`SystemDoc::fold_duplicate_tags`] on the next import. So `#fun/home` in a
//! document written by this build reads in an older one as a tag merely *named*
//! "fun/home" — a degraded label, never a corrupt document, which is why
//! [`SYSTEM_SCHEMA_VERSION`] does not move.
//!
//! **Lists follow the tag idiom, and lists are filters** (SPEC §8, [ADR-285]'s
//! 2026-09-01 amendment). A task stores one [`ListId`] under `koru.list`; this
//! table maps it to a name, an accent color and the rail's order. Rename
//! touches one record and no task. The one place the two tables deliberately
//! differ is deletion: a tag is never deleted, only merged — its record is the
//! tombstone that keeps every node carrying the old id resolvable — while a
//! list *is* deleted, because SPEC §8 says deleting a list clears memberships
//! and touches no task. So [`SystemDoc::delete_list`] tombstones the record the
//! same way an alias does, the list leaves [`SystemDoc::lists`] at once, and
//! `WorkspaceDoc::clear_list_membership` is the document-side sweep that takes
//! the dead value off the nodes. Until a document is swept its stale membership
//! resolves to no list — lazily wrong in the one direction that loses nothing.
//!
//! [ADR-176]: ../../../docs/decisions/ADR-176-200.md#adr-199-attributes--attributes-are-typed-values-on-the-node-under-registered-keys-tags-are-entities-and-the-workspace-keeps-a-hidden-system-document
//! [ADR-185]: ../../../docs/decisions/ADR-201-225.md#adr-208-tagnames--a-tag-name-folds-for-identity-and-parses-for-hierarchy
//! [ADR-285]: ../../../docs/decisions/ADR-276-300.md#adr-285-taskmodel--a-task-is-a-marked-node-and-the-marker-or-a-due-date-is-the-whole-derivation
//! [`ListId`]: crate::ids::ListId
//! [`absorb`]: SystemDoc::absorb
//! [`DocumentId`]: crate::ids::DocumentId
//! [`TagId`]: crate::ids::TagId

use std::collections::{BTreeMap, HashMap};

use loro::{LoroDoc, LoroMap, LoroValue, ValueOrContainer};

use crate::attributes::{
    AttributeDefinition, AttributeScope, Cardinality, ValueType, system_definition,
};
use crate::ids::{DocumentId, ListId, PageKey, TagId};
use crate::pages::deletion::{DecisionId, DeletionRecord, EventId, Keep, Replica};
use crate::schema::{Result, StoreError};
use crate::tags;
use crate::tasks::Bucket;
use crate::time::TimeZoneName;

// Root containers.
const C_META: &str = "meta";
const C_DEFINITIONS: &str = "definitions";
const C_TAGS: &str = "tags";
const C_LISTS: &str = "lists";
/// The workspace spelling dictionary: language tag → a map whose keys are the
/// words (a set spelled as a map, so two collaborators adding words merge).
/// A root container beside the others, so an older build walks past it
/// untouched — additive, no [`SYSTEM_SCHEMA_VERSION`] bump (spell-check plan
/// D3). Unlike spelling *issues*, these words are workspace configuration and
/// deliberately sync.
const C_DICTIONARY: &str = "dictionary";
/// The agenda buckets' page records: bucket token → the same page keys a
/// list record carries (page-document plan §6.2, §6.8). A root container
/// beside the others, additive like the dictionary; a list's page keys live
/// on the list's own record instead, so a page record is found from its
/// [`PageKey`] either way ([`SystemDoc::page_generations`]).
const C_PAGES: &str = "pages";

// Meta keys.
const K_SYSTEM_SCHEMA: &str = "system_schema_version";
const SYSTEM_SCHEMA_VERSION: i64 = 1;
/// The workspace's declared timezone — see [`SystemDoc::default_timezone`].
/// Absent is the normal state and means "this device's".
const K_DEFAULT_TIMEZONE: &str = "default_timezone";

// Keys inside one definition record.
const K_DEF_KEY: &str = "key";
const K_DEF_LABEL: &str = "label";
const K_DEF_TYPE: &str = "type";
const K_DEF_CARDINALITY: &str = "cardinality";
const K_DEF_SCOPE: &str = "scope";
const K_DEF_OPTIONS: &str = "options";
const K_DEF_FILTERABLE: &str = "filterable";
const K_DEF_SORTABLE: &str = "sortable";
const K_DEF_GROUPABLE: &str = "groupable";

// Keys inside one tag record.
const K_TAG_NAME: &str = "name";
const K_TAG_COLOR: &str = "color";
const K_TAG_CREATED: &str = "created";
/// A merged-away tag's forwarding pointer: the [`TagId`] it now means. The
/// record stays — it is the tombstone that keeps every node still carrying
/// the old id resolvable — and readers follow it through
/// [`SystemDoc::resolve`].
const K_TAG_ALIAS: &str = "alias";

// Keys inside one list record. Name, color and created are the tag record's,
// spelled the same way on purpose.
const K_LIST_NAME: &str = "name";
const K_LIST_COLOR: &str = "color";
const K_LIST_CREATED: &str = "created";
/// Where this list sits in the rail. A rank the sidebar chooses, last-writer-
/// wins like every other field here; absent sorts after every ranked list, so
/// a build that never sets one lists alphabetically.
const K_LIST_ORDER: &str = "order";
/// A deleted list's tombstone: the moment it went. The record stays — it is
/// what tells a reader holding a stale `koru.list` value that the list is gone
/// rather than merely unknown — and [`SystemDoc::lists`] omits it.
const K_LIST_DELETED: &str = "deleted";
/// On a page record: the generations of the page's document, a grow-only
/// set spelled as a map of decimal keys → `true`. The current generation is
/// the numeric maximum; a replacement after a deletion adds the next key.
/// Absent on a page that has never had a document.
const K_PAGE_GENS: &str = "page_gens";
/// On a page record: a direct rename of generation *n*'s document — a
/// person's title, last-writer-wins, always text. `page_override.<n>`.
/// A list rename removes the current generation's (plan Q10).
const K_PAGE_OVERRIDE: &str = "page_override";
/// On a page record: a superseded generation's own name, frozen when its
/// replacement was made — **one tagged value** `{kind, value}` written by one
/// insert, so two concurrent writers can never leave a kind from one and a
/// value from the other. `page_frozen.<n>`.
const K_PAGE_FROZEN: &str = "page_frozen";
const F_FROZEN_KIND: &str = "kind";
const F_FROZEN_VALUE: &str = "value";
const FROZEN_TEXT: &str = "text";
const FROZEN_BUCKET: &str = "bucket";
/// On a page record: the deletion protocol of each generation
/// ([`crate::pages::deletion`]), keyed by generation then by the protocol's
/// own keys. The persisted shape of a [`DeletionRecord`]; grow-only sets and
/// a monotone join, so writing the one new key converges.
const K_PAGE_DELETIONS: &str = "page_deletions";
const K_PAGE_RECOVERS: &str = "page_recovers";
const K_PAGE_KEEPS: &str = "page_keeps";
const K_PAGE_UNCOVERED: &str = "page_uncovered";
const F_COVERS: &str = "covers";
const F_DELETED_MS: &str = "deleted_ms";
const F_KEEP_REPLICA: &str = "replica";

/// One tag, as the table knows it.
#[derive(Clone, PartialEq, Debug)]
pub struct TagEntry {
    pub id: TagId,
    /// The **whole path**, as typed: `fun/home`. A chip draws only the leaf;
    /// a picker draws the path. See [`crate::tags`].
    pub name: String,
    /// An [`AccentColor`](crate::attributes::AccentColor) token, or `None` for
    /// the default chip. Kept as the raw token so one from a newer build's
    /// palette round-trips.
    pub color: Option<String>,
    /// Unix ms, as passed in by the caller — this crate has no clock.
    pub created_ms: Option<i64>,
    /// `Some` marks a merged-away alias: this id now means that one.
    pub alias_of: Option<TagId>,
}

impl TagEntry {
    /// What decides whether this and another entry are one tag. The unit a
    /// group-by heading folds on, so a mis-cased duplicate does not split it.
    pub fn fold_key(&self) -> String {
        tags::fold_key(&self.name)
    }

    /// The last path segment — what a chip renders.
    pub fn leaf(&self) -> &str {
        tags::leaf(&self.name)
    }

    /// Whether this tag is `name` or anything under it, folded per segment.
    pub fn is_under(&self, name: &str) -> bool {
        tags::covers(name, &self.name)
    }
}

/// One list, as the table knows it.
///
/// **A row of a registry, not a container**: what makes a task a member is the
/// `koru.list` value on the task, and everything here is how that id is drawn —
/// a name, a swatch, and where the row sits in the rail.
#[derive(Clone, PartialEq, Debug)]
pub struct ListEntry {
    pub id: ListId,
    /// As typed. Lists do not nest and a `/` in a name is just a `/` — the
    /// path grammar is the tag table's, and SPEC §8 keeps nesting out.
    pub name: String,
    /// An [`AccentColor`](crate::attributes::AccentColor) token, or `None` for
    /// the default swatch. Kept as the raw token so one from a newer build's
    /// palette round-trips.
    pub color: Option<String>,
    /// Where it sits in the rail; `None` sorts after every ranked list.
    pub order: Option<i64>,
    /// Unix ms, as passed in by the caller — this crate has no clock.
    pub created_ms: Option<i64>,
    /// `Some` marks a deleted list. The record survives so a stale membership
    /// reads as *gone* rather than as *unknown*; nothing lists it.
    pub deleted_ms: Option<i64>,
}

impl ListEntry {
    /// What decides whether this and another entry are one list — the same
    /// case fold a tag name uses, so `Wedding` and `wedding` are not offered
    /// as two rows of the same rail.
    pub fn fold_key(&self) -> String {
        tags::fold_key(&self.name)
    }
}

/// A superseded page document's own name, frozen when its replacement was
/// made (page-document plan §6.6). A person's title or a list's name is
/// shown literally; a bucket's token is localized by every reader, so a
/// Spanish device replacing *Today* leaves an archived note that reads *Hoy*
/// there and *Today* here.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum FrozenTitle {
    Text(String),
    Bucket(String),
}

/// A page document's resolved title: a semantic value, never a localized
/// label. The manifest caches [`semantic`](Self::semantic); a reader shows
/// a bucket in its own language.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum PageTitle {
    Text(String),
    Bucket(Bucket),
}

impl PageTitle {
    /// The stored form: the text itself, or a bucket's token.
    pub fn semantic(&self) -> String {
        match self {
            Self::Text(text) => text.clone(),
            Self::Bucket(bucket) => bucket.as_str().to_owned(),
        }
    }
}

/// The workspace system document. See the module docs for what it is and for
/// the seam that decides where it lives.
pub struct SystemDoc {
    doc: LoroDoc,
}

impl SystemDoc {
    pub fn create() -> Result<Self> {
        let doc = LoroDoc::new();
        doc.get_map(C_META)
            .insert(K_SYSTEM_SCHEMA, SYSTEM_SCHEMA_VERSION)?;
        doc.commit();
        Ok(Self { doc })
    }

    pub fn from_snapshot(bytes: &[u8]) -> Result<Self> {
        let doc = LoroDoc::new();
        doc.import(bytes)?;
        let mut opened = Self { doc };
        opened.fold_duplicate_tags()?;
        Ok(opened)
    }

    /// The one open-or-create door, for `koru-workspace` to call with
    /// whatever the manifest holds: an existing snapshot opens, none creates.
    pub fn open_or_create(snapshot: Option<&[u8]>) -> Result<Self> {
        match snapshot {
            Some(bytes) => Self::from_snapshot(bytes),
            None => Self::create(),
        }
    }

    pub fn export_snapshot(&self) -> Result<Vec<u8>> {
        Ok(self.doc.export(loro::ExportMode::snapshot())?)
    }

    pub fn export_all_updates(&self) -> Result<Vec<u8>> {
        Ok(self.doc.export(loro::ExportMode::all_updates())?)
    }

    /// The incremental form, for a live sync session.
    pub fn export_updates_since(&self, from: &loro::VersionVector) -> Result<Vec<u8>> {
        Ok(self.doc.export(loro::ExportMode::updates(from))?)
    }

    pub fn version(&self) -> loro::VersionVector {
        self.doc.oplog_vv()
    }

    /// This replica's Loro peer id — a replica handle, never an identity
    /// (ADR-39's rule, same as `WorkspaceDoc::peer_id`).
    pub fn peer_id(&self) -> u64 {
        self.doc.peer_id()
    }

    /// Merge a batch of update frames. The same shape `WorkspaceDoc` has, so a
    /// host can sync this document through the same session machinery.
    pub fn import_frames(&mut self, frames: &[Vec<u8>]) -> Result<()> {
        for frame in frames {
            self.doc.import(frame)?;
        }
        self.fold_duplicate_tags()?;
        Ok(())
    }

    pub fn import(&mut self, bytes: &[u8]) -> Result<()> {
        self.doc.import(bytes)?;
        self.fold_duplicate_tags()?;
        Ok(())
    }

    /// Union a duplicate system document into this one. A CRDT import of a
    /// same-shaped document *is* the union: both hold the same named root
    /// maps, definition ids and tag ids are v4 and never collide, and a key
    /// both somehow wrote converges the way any concurrent map write does.
    pub fn absorb(&mut self, snapshot: &[u8]) -> Result<()> {
        self.import(snapshot)
    }

    /// Which of several concurrently created system documents is canonical:
    /// the lowest id, so every peer picks the same one without talking. Other
    /// rows remain listed as recovery anchors; this chooses the live target,
    /// not a deletion winner.
    pub fn canonical_document(ids: &[DocumentId]) -> Option<DocumentId> {
        ids.iter().copied().min()
    }

    // -------------------------------------------------------------- timezone

    /// The workspace's declared timezone, if it has one.
    ///
    /// **A default for resolving a *new* shared timed value, and nothing
    /// else** ([ADR-183], rule 6). A team that works to one calendar can say
    /// so here, and "due Friday 17:00" then means 17:00 *there* for everyone,
    /// whatever laptop it was typed on.
    ///
    /// Three things it deliberately is not:
    ///
    /// - **It never rewrites a deadline that already exists.** A stored
    ///   [`ZonedDateTime`](crate::time::ZonedDateTime) carries its own zone;
    ///   changing this key re-answers the next one somebody sets and touches
    ///   nothing already written (foundation invariant 24).
    /// - **It is not a display setting.** Which zone a reader *sees* a due in
    ///   is presentation's, decided per surface.
    /// - **It does not reach personal values.** A `koru.plan` floats by
    ///   design; a workspace does not get to pin one person's morning.
    ///
    /// Absent is the normal state — the resolution seam
    /// ([`zone_for_new_shared_value`](crate::time::zone_for_new_shared_value))
    /// then falls through to the device. There is no settings UI yet; the
    /// field is reserved and the seam is real so a surface can land without a
    /// storage change.
    ///
    /// [ADR-183]: ../../../docs/decisions/ADR-201-225.md#adr-206-timemodel--koru-has-three-temporal-types-not-one-with-a-nullable-timezone
    /// Add one word to the workspace spelling dictionary for one language —
    /// jargon every collaborator in this workspace accepts (plan D3).
    /// Idempotent, and merge-safe when two people add their first word.
    pub fn add_dictionary_word(&mut self, language: &str, word: &str) -> Result<bool> {
        let root = self.doc.get_map(C_DICTIONARY);
        let entry = root.ensure_mergeable_map(language)?;
        if entry.get(word).is_some() {
            return Ok(false);
        }
        entry.insert(word, true)?;
        self.doc.commit();
        Ok(true)
    }

    /// Remove one word; removal is a deleted key. Answers whether anything
    /// was there, and creates nothing when it was not.
    pub fn remove_dictionary_word(&mut self, language: &str, word: &str) -> Result<bool> {
        let root = self.doc.get_map(C_DICTIONARY);
        let Some(loro::ValueOrContainer::Container(container)) = root.get(language) else {
            return Ok(false);
        };
        let Ok(entry) = container.into_map() else {
            return Ok(false);
        };
        if entry.get(word).is_none() {
            return Ok(false);
        }
        entry.delete(word)?;
        self.doc.commit();
        Ok(true)
    }

    /// The whole workspace dictionary, language tag → sorted words. A pure
    /// read; an entry whose shape this build does not recognise is skipped.
    pub fn dictionary(&self) -> std::collections::BTreeMap<String, Vec<String>> {
        let mut out = std::collections::BTreeMap::new();
        let root = self.doc.get_map(C_DICTIONARY);
        root.for_each(|language, value| {
            let loro::ValueOrContainer::Container(container) = value else {
                return;
            };
            let Ok(entry) = container.into_map() else {
                return;
            };
            let mut words: Vec<String> = Vec::new();
            entry.for_each(|word, _| words.push(word.to_string()));
            words.sort();
            if !words.is_empty() {
                out.insert(language.to_string(), words);
            }
        });
        out
    }

    pub fn default_timezone(&self) -> Option<TimeZoneName> {
        self.doc
            .get_map(C_META)
            .get(K_DEFAULT_TIMEZONE)
            .and_then(crate::schema::as_string)
            .as_deref()
            .and_then(TimeZoneName::parse)
    }

    /// Declare — or clear — the workspace timezone. Clearing is a **deleted
    /// key**, never a stored "none", and both halves are guarded: writing what
    /// is already there is a no-op, so nothing churns the document.
    pub fn set_default_timezone(&mut self, zone: Option<&TimeZoneName>) -> Result<bool> {
        let meta = self.doc.get_map(C_META);
        let current = self.default_timezone();
        if current.as_ref() == zone {
            return Ok(false);
        }
        match zone {
            Some(zone) => meta.insert(K_DEFAULT_TIMEZONE, zone.as_str())?,
            None if meta.get(K_DEFAULT_TIMEZONE).is_some() => meta.delete(K_DEFAULT_TIMEZONE)?,
            None => return Ok(false),
        }
        self.doc.commit();
        Ok(true)
    }

    // ------------------------------------------------------------ definitions

    /// Record a custom attribute definition, keyed by its stable id.
    ///
    /// The `koru.*` namespace is refused: those keys mean what this build says
    /// they mean, and a workspace record shadowing one would give the same
    /// key two authorities.
    pub fn define(&mut self, def: &AttributeDefinition) -> Result<()> {
        if def.key.starts_with("koru.") || def.system {
            return Err(StoreError::AttributeRejected {
                key: def.key.clone(),
                why: "is in the reserved koru.* namespace".into(),
            });
        }
        let defs = self.doc.get_map(C_DEFINITIONS);
        let record = defs.ensure_mergeable_map(&def.id)?;
        record.insert(K_DEF_KEY, def.key.as_str())?;
        record.insert(K_DEF_LABEL, def.label.as_str())?;
        record.insert(K_DEF_TYPE, def.value_type.as_str())?;
        record.insert(K_DEF_CARDINALITY, def.cardinality.as_str())?;
        record.insert(K_DEF_SCOPE, def.scope.as_str())?;
        let options: Vec<LoroValue> = def
            .options
            .iter()
            .map(|o| LoroValue::from(o.as_str()))
            .collect();
        record.insert(K_DEF_OPTIONS, options)?;
        record.insert(K_DEF_FILTERABLE, def.filterable)?;
        record.insert(K_DEF_SORTABLE, def.sortable)?;
        record.insert(K_DEF_GROUPABLE, def.groupable)?;
        self.doc.commit();
        Ok(())
    }

    /// Every custom definition, sorted by key so reads are reproducible. A
    /// record whose type token this build does not know is skipped — the same
    /// additive rule attribute values follow.
    pub fn definitions(&self) -> Vec<AttributeDefinition> {
        let defs = self.doc.get_map(C_DEFINITIONS);
        let mut out = Vec::new();
        let keys: Vec<String> = defs.keys().map(|k| k.to_string()).collect();
        for id in keys {
            let Some(ValueOrContainer::Container(c)) = defs.get(&id) else {
                continue;
            };
            let Ok(record) = c.into_map() else { continue };
            if let Some(def) = read_definition(&id, &record) {
                out.push(def);
            }
        }
        out.sort_by(|a, b| a.key.cmp(&b.key).then(a.id.cmp(&b.id)));
        out
    }

    /// The definition behind `key`: the system registry first — its semantics
    /// are this build's, never the workspace's to override — then the
    /// workspace's custom definitions.
    pub fn definition(&self, key: &str) -> Option<AttributeDefinition> {
        system_definition(key).or_else(|| self.definitions().into_iter().find(|d| d.key == key))
    }

    // ------------------------------------------------------------ tags

    /// Create a tag — or answer the one that already means this name.
    /// `now_ms` is passed in, never read from a clock — this crate has none,
    /// deliberately.
    ///
    /// Three things happen here, and only the first is old:
    ///
    /// - A name shadowing a system concept (`#high`, `#today`) is the entry
    ///   surface's to *warn* about, never this table's to block (D5): the tag
    ///   is data, and refusing it here would make the model police prose. The
    ///   same goes for the shape of a path — `TagName::parse` runs where
    ///   somebody types, not here, so a name written by another build always
    ///   reads back.
    /// - **A matching fold key reuses the record.** `#HOME` typed where
    ///   `#home` exists answers `#home`'s id and leaves its name and color
    ///   exactly as they were: identity is case-insensitive, display is
    ///   case-preserving ([`crate::tags`]). Only *live* tags are candidates —
    ///   a merged-away tombstone's name is history, not a claim on the name.
    /// - **Parents are created.** `#fun/home` with no `fun` creates both
    ///   records, so `fun` is a real tag: renameable, colorable and taggable in
    ///   its own right, while the tree itself stays derived from names
    ///   ([ADR-185]). A mis-cased parent cannot spawn a second one, because the
    ///   ancestor lookup folds too.
    ///
    /// [ADR-185]: ../../../docs/decisions/ADR-201-225.md#adr-208-tagnames--a-tag-name-folds-for-identity-and-parses-for-hierarchy
    pub fn create_tag(&mut self, name: &str, color: Option<&str>, now_ms: i64) -> Result<TagId> {
        let key = tags::fold_key(name);
        if !key.is_empty()
            && let Some(existing) = self.live_tag_for_key(&key)
        {
            return Ok(existing);
        }
        for ancestor in tags::ancestors(name) {
            let ancestor_key = tags::fold_key(&ancestor);
            if ancestor_key.is_empty() || self.live_tag_for_key(&ancestor_key).is_some() {
                continue;
            }
            self.write_tag(TagId::new(), &ancestor, None, now_ms)?;
        }
        let id = TagId::new();
        self.write_tag(id, name, color, now_ms)?;
        self.doc.commit();
        Ok(id)
    }

    /// Rename a tag — **and its subtree**. One record here per renamed tag and
    /// zero nodes anywhere (Foundation invariant 19), because a node stores a
    /// [`TagId`]. Answers whether the tag exists.
    ///
    /// Renaming `fun` to `leisure` rewrites every tag whose first folded
    /// segment is `fun` — `fun/home` becomes `leisure/home` — which is N tag
    /// records and no documents at all. Bear has to rewrite every note; this is
    /// what tags-as-entities buys.
    ///
    /// The pass ends in [`fold_duplicate_tags`](Self::fold_duplicate_tags),
    /// because a rename is the one local edit that can collide: renaming `ui`
    /// onto an existing `design` means one tag by the fold rule, and the merge
    /// that follows is the same alias machinery
    /// [`merge_tags`](Self::merge_tags) uses.
    pub fn rename_tag(&mut self, tag: TagId, name: &str) -> Result<bool> {
        let Some(record) = self.tag_record(tag) else {
            return Ok(false);
        };
        let old = read_tag(tag, &record).name;
        record.insert(K_TAG_NAME, name)?;

        let depth = tags::fold_segments(&old).len();
        let renamed: Vec<&str> = tags::segments(name).collect();
        if depth > 0 && !renamed.is_empty() {
            // A snapshot of the listing, taken *after* this tag's own rename —
            // hence the id skip. Descendants are recognised by folded segment,
            // so a mis-cased child travels with the branch.
            let descendants: Vec<(TagId, String)> = self
                .tags()
                .into_iter()
                .filter(|entry| entry.id != tag && tags::is_ancestor(&old, &entry.name))
                .map(|entry| {
                    let tail = tags::segments(&entry.name).skip(depth);
                    let path: Vec<&str> = renamed.iter().copied().chain(tail).collect();
                    (entry.id, path.join("/"))
                })
                .collect();
            for (id, path) in descendants {
                if let Some(record) = self.tag_record(id) {
                    record.insert(K_TAG_NAME, path.as_str())?;
                }
            }
        }
        self.doc.commit();
        self.fold_duplicate_tags()?;
        Ok(true)
    }

    /// Set or clear a tag's chip color token. Clearing an absent color is a
    /// guarded no-op, the `set_heading` rule.
    pub fn set_tag_color(&mut self, tag: TagId, color: Option<&str>) -> Result<bool> {
        let Some(record) = self.tag_record(tag) else {
            return Ok(false);
        };
        match color {
            Some(c) => record.insert(K_TAG_COLOR, c)?,
            None if record.get(K_TAG_COLOR).is_some() => record.delete(K_TAG_COLOR)?,
            None => {}
        }
        self.doc.commit();
        Ok(true)
    }

    /// Merge `from` into `into`: `from` becomes an alias tombstone pointing at
    /// `into`. **No node is re-pointed** — nodes store ids, and
    /// [`resolve`](Self::resolve) is how a reader follows the merge. Refuses a
    /// self-merge and answers whether both tags exist.
    pub fn merge_tags(&mut self, from: TagId, into: TagId) -> Result<bool> {
        if from == into || self.tag_record(into).is_none() {
            return Ok(false);
        }
        let Some(record) = self.tag_record(from) else {
            return Ok(false);
        };
        record.insert(K_TAG_ALIAS, into.to_storage())?;
        self.doc.commit();
        Ok(true)
    }

    /// **Fold tags that two peers minted independently.** Same fold key ⇒ the
    /// higher [`TagId`] becomes an alias of the lower, and the lower's display
    /// name is the one that survives. Answers how many were folded.
    ///
    /// No picker check can prevent the split: two replicas offline both offer
    /// *Create `#home`*, both are right, and both mint a v4 id. So the fix is a
    /// reconcile rather than a guard, and it is deliberately the **same
    /// mechanism a merge already uses** — an alias tombstone, resolved through
    /// [`resolve`](Self::resolve) — so no node is re-pointed and nothing needs
    /// a second bounded walk.
    ///
    /// **Lowest id wins, so both peers reach the same answer without talking.**
    /// That is [`canonical_document`](Self::canonical_document)'s tie-break and
    /// `ensure_trash_root`'s, applied a third time; the two peers write the
    /// same key with the same value, which is a concurrent map write that
    /// converges by construction.
    ///
    /// **The seam is every door bytes come in through** —
    /// [`from_snapshot`](Self::from_snapshot), [`import`](Self::import) and
    /// [`import_frames`](Self::import_frames), and therefore
    /// [`absorb`](Self::absorb), which is one of them. A fold hung on `absorb`
    /// alone would cover only the *manifest* race (two system documents) and
    /// miss the ordinary one (one document, two replicas), which is the case
    /// that actually happens; a fold on read cannot write, and a fold left to a
    /// caller is a fold somebody forgets.
    pub fn fold_duplicate_tags(&mut self) -> Result<usize> {
        let mut groups: BTreeMap<String, Vec<TagId>> = BTreeMap::new();
        for entry in self.tags() {
            let key = tags::fold_key(&entry.name);
            if key.is_empty() {
                continue;
            }
            groups.entry(key).or_default().push(entry.id);
        }
        let mut folded = 0usize;
        for (_, mut ids) in groups {
            if ids.len() < 2 {
                continue;
            }
            ids.sort_unstable();
            let winner = ids[0];
            for loser in &ids[1..] {
                let Some(record) = self.tag_record(*loser) else {
                    continue;
                };
                record.insert(K_TAG_ALIAS, winner.to_storage())?;
                folded += 1;
            }
        }
        if folded > 0 {
            self.doc.commit();
        }
        Ok(folded)
    }

    /// The live tag a fold key names — the lowest id, if a fold has not yet
    /// run and several still answer to it.
    fn live_tag_for_key(&self, key: &str) -> Option<TagId> {
        self.tags()
            .into_iter()
            .filter(|entry| tags::fold_key(&entry.name) == key)
            .map(|entry| entry.id)
            .min()
    }

    /// Write one tag record. Uncommitted — the caller commits, so creating a
    /// path and its parents is one change rather than three.
    fn write_tag(
        &mut self,
        id: TagId,
        name: &str,
        color: Option<&str>,
        now_ms: i64,
    ) -> Result<TagId> {
        let record = self
            .doc
            .get_map(C_TAGS)
            .ensure_mergeable_map(&id.to_storage())?;
        record.insert(K_TAG_NAME, name)?;
        if let Some(color) = color {
            record.insert(K_TAG_COLOR, color)?;
        }
        record.insert(K_TAG_CREATED, now_ms)?;
        Ok(id)
    }

    /// What a stored [`TagId`] means today, following merge aliases. Bounded,
    /// so two tags concurrently merged into each other cannot spin a reader;
    /// on a cycle the id where the walk stopped is the answer, and it is the
    /// same answer on every peer because the walk is deterministic.
    pub fn resolve(&self, tag: TagId) -> TagId {
        let mut at = tag;
        for _ in 0..32 {
            match self.tag_entry(at).and_then(|t| t.alias_of) {
                Some(next) if next != at => at = next,
                _ => return at,
            }
        }
        at
    }

    /// One tag's record, aliases included — `None` only for an id the table
    /// has never held.
    pub fn tag_entry(&self, tag: TagId) -> Option<TagEntry> {
        let record = self.tag_record(tag)?;
        Some(read_tag(tag, &record))
    }

    /// The live tags — alias tombstones excluded — sorted by name then id, so
    /// two peers list the same table the same way.
    pub fn tags(&self) -> Vec<TagEntry> {
        let tags = self.doc.get_map(C_TAGS);
        let mut out = Vec::new();
        let keys: Vec<String> = tags.keys().map(|k| k.to_string()).collect();
        for key in keys {
            let Some(id) = TagId::parse(&key) else {
                continue;
            };
            let Some(ValueOrContainer::Container(c)) = tags.get(&key) else {
                continue;
            };
            let Ok(record) = c.into_map() else { continue };
            let entry = read_tag(id, &record);
            if entry.alias_of.is_none() {
                out.push(entry);
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
        out
    }

    /// **Every id the table has ever held, mapped to the name it means today** —
    /// alias tombstones included, each resolved through [`resolve`] to the name
    /// of the tag it was merged into.
    ///
    /// [`tags`] is the *live* table and is what a picker offers; this is what a
    /// reader holding a stored [`TagId`] needs, and the two are different sets
    /// exactly because a merge re-points nothing node-side. The search indexer
    /// is the caller: it turns `koru.tag` attributes into matchable words, and
    /// a node still carrying a merged-away id has to come out spelled like the
    /// tag it now belongs to rather than vanishing
    /// ([ADR-205](../../../docs/decisions/ADR-201-225.md#adr-205-tagsearch--a-tag-is-indexed-by-its-name-resolved-at-index-time-and-a-rename-re-indexes-what-carries-it)).
    ///
    /// [`resolve`]: SystemDoc::resolve
    /// [`tags`]: SystemDoc::tags
    pub fn tag_names(&self) -> Vec<(TagId, String)> {
        // Keys first, as in `tags`: the map is not borrowed while each record
        // is read back through `resolve`.
        let keys: Vec<String> = self
            .doc
            .get_map(C_TAGS)
            .keys()
            .map(|k| k.to_string())
            .collect();
        let mut out: Vec<(TagId, String)> = keys
            .iter()
            .filter_map(|key| TagId::parse(key))
            .filter_map(|id| {
                let name = self.tag_entry(self.resolve(id))?.name;
                (!name.is_empty()).then_some((id, name))
            })
            .collect();
        out.sort_unstable();
        out
    }

    fn tag_record(&self, tag: TagId) -> Option<LoroMap> {
        match self.doc.get_map(C_TAGS).get(&tag.to_storage())? {
            ValueOrContainer::Container(c) => c.into_map().ok(),
            _ => None,
        }
    }

    // ------------------------------------------------------------ lists

    /// Create a list — or answer the live one that already carries this name.
    /// `now_ms` is passed in, never read from a clock.
    ///
    /// **Reusing a matching name is the local half of the tag rule and only the
    /// local half.** Typing *Wedding* where `wedding` exists answers the
    /// existing list rather than drawing a second identical row; two *peers*
    /// each creating "Wedding" offline, though, keep both rows. That is
    /// deliberately unlike [`create_tag`](Self::create_tag), which folds
    /// concurrent duplicates into one id: a tag's name **is** its identity,
    /// typed afresh every time, while a list is picked from a rail and its name
    /// is a label on a bucket somebody made. Folding two of those would have to
    /// choose which bucket's memberships survive, and nothing in SPEC §8 says
    /// which. Two rows are visible and one rename or one delete fixes them;
    /// a silent fold is not recoverable.
    ///
    /// A deleted list's name is history rather than a reservation, exactly as a
    /// merged-away tag's is: creating it again mints a new id.
    pub fn create_list(&mut self, name: &str, color: Option<&str>, now_ms: i64) -> Result<ListId> {
        let key = tags::fold_key(name);
        if !key.is_empty()
            && let Some(existing) = self.live_list_for_key(&key)
        {
            return Ok(existing);
        }
        let id = ListId::new();
        let record = self
            .doc
            .get_map(C_LISTS)
            .ensure_mergeable_map(&id.to_storage())?;
        record.insert(K_LIST_NAME, name)?;
        if let Some(color) = color {
            record.insert(K_LIST_COLOR, color)?;
        }
        record.insert(K_LIST_CREATED, now_ms)?;
        self.doc.commit();
        Ok(id)
    }

    /// Rename a list. One record here and **no task at all** (Foundation
    /// invariant 19), because a task stores the [`ListId`]. Answers whether the
    /// list exists; a deleted one does not.
    ///
    /// There is no subtree to carry along and no fold afterwards — lists do not
    /// nest, and renaming one onto another's name leaves two rows with one name
    /// rather than merging them, for [`create_list`](Self::create_list)'s
    /// reason.
    pub fn rename_list(&mut self, list: ListId, name: &str) -> Result<bool> {
        let Some(record) = self.live_list_record(list) else {
            return Ok(false);
        };
        record.insert(K_LIST_NAME, name)?;
        // The list's note follows the list again (plan Q5, Q10): a direct
        // rename of the current generation's document is taken back in the
        // same commit, as this replica sees the current generation. A
        // superseded generation keeps its own name.
        if let Some(current) = self.current_page_generation(PageKey::List(list))
            && let Some(overrides) = record_map(&record, K_PAGE_OVERRIDE)
            && overrides.get(&current.to_string()).is_some()
        {
            overrides.delete(&current.to_string())?;
        }
        self.doc.commit();
        Ok(true)
    }

    /// Set or clear a list's swatch token. Clearing an absent color is a
    /// guarded no-op, the `set_heading` rule.
    pub fn set_list_color(&mut self, list: ListId, color: Option<&str>) -> Result<bool> {
        let Some(record) = self.live_list_record(list) else {
            return Ok(false);
        };
        match color {
            Some(c) => record.insert(K_LIST_COLOR, c)?,
            None if record.get(K_LIST_COLOR).is_some() => record.delete(K_LIST_COLOR)?,
            None => {}
        }
        self.doc.commit();
        Ok(true)
    }

    /// Set or clear where a list sits in the rail. Absent means unranked, which
    /// sorts after every ranked list.
    pub fn set_list_order(&mut self, list: ListId, order: Option<i64>) -> Result<bool> {
        let Some(record) = self.live_list_record(list) else {
            return Ok(false);
        };
        match order {
            Some(rank) => record.insert(K_LIST_ORDER, rank)?,
            None if record.get(K_LIST_ORDER).is_some() => record.delete(K_LIST_ORDER)?,
            None => {}
        }
        self.doc.commit();
        Ok(true)
    }

    /// Delete a list. **Never a task** (SPEC §8): the record is tombstoned so
    /// the row leaves the rail on every peer, and the memberships are cleared
    /// document-side by `WorkspaceDoc::clear_list_membership`, which this
    /// cannot do from here — a registry cannot see a node.
    ///
    /// The record survives for the tag table's reason turned around: an alias
    /// tombstone keeps an old id *resolvable*, and this one keeps a stale
    /// membership *legible* — a reader can tell "the list was deleted" from
    /// "this build has never heard of that id", which are different bugs.
    /// Deleting an already-deleted list is a no-op; answers whether it exists.
    pub fn delete_list(&mut self, list: ListId, now_ms: i64) -> Result<bool> {
        let Some(record) = self.live_list_record(list) else {
            return Ok(false);
        };
        record.insert(K_LIST_DELETED, now_ms)?;
        self.doc.commit();
        Ok(true)
    }

    /// The live lists — tombstones excluded — in rail order: ranked first by
    /// rank, then the unranked by name, and id last so two peers list the same
    /// table the same way.
    pub fn lists(&self) -> Vec<ListEntry> {
        let lists = self.doc.get_map(C_LISTS);
        let mut out = Vec::new();
        let keys: Vec<String> = lists.keys().map(|k| k.to_string()).collect();
        for key in keys {
            let Some(id) = ListId::parse(&key) else {
                continue;
            };
            let Some(ValueOrContainer::Container(c)) = lists.get(&key) else {
                continue;
            };
            let Ok(record) = c.into_map() else { continue };
            let entry = read_list(id, &record);
            if entry.deleted_ms.is_none() {
                out.push(entry);
            }
        }
        out.sort_by(|a, b| {
            a.order
                .is_none()
                .cmp(&b.order.is_none())
                .then(a.order.cmp(&b.order))
                .then(a.name.cmp(&b.name))
                .then(a.id.cmp(&b.id))
        });
        out
    }

    /// One list's record, tombstones included — `None` only for an id the table
    /// has never held. A caller holding a stored `koru.list` value asks this;
    /// [`lists`](Self::lists) is what a rail draws.
    pub fn list_entry(&self, list: ListId) -> Option<ListEntry> {
        let record = self.list_record(list)?;
        Some(read_list(list, &record))
    }

    /// The live list a fold key names — the lowest id, if two peers minted one
    /// each and nobody has renamed either.
    fn live_list_for_key(&self, key: &str) -> Option<ListId> {
        self.lists()
            .into_iter()
            .filter(|entry| tags::fold_key(&entry.name) == key)
            .map(|entry| entry.id)
            .min()
    }

    // ------------------------------------------------------------ pages

    /// The record a page's keys live on.
    ///
    /// For a list, the list's own record — **tombstones included**, because a
    /// deleted list keeps its document (page-document plan D4) and the map
    /// from a document id back to its page has to find it. For a bucket, the
    /// entry under the `pages` root map, keyed by the bucket's token.
    fn page_record(&self, page: PageKey) -> Option<LoroMap> {
        match page {
            PageKey::List(list) => self.list_record(list),
            PageKey::Bucket(bucket) => match self.doc.get_map(C_PAGES).get(bucket.as_str())? {
                ValueOrContainer::Container(c) => c.into_map().ok(),
                _ => None,
            },
        }
    }

    /// The record to write a page's keys into: a bucket's is created on first
    /// use; a list's must exist, in any state. `None` for a list the table
    /// has never held.
    fn page_record_for_write(&self, page: PageKey) -> Result<Option<LoroMap>> {
        Ok(match page {
            PageKey::List(list) => self.list_record(list),
            PageKey::Bucket(bucket) => Some(
                self.doc
                    .get_map(C_PAGES)
                    .ensure_mergeable_map(bucket.as_str())?,
            ),
        })
    }

    /// Every generation a page has had, as written — unsorted, tombstones
    /// none because the set only grows. Empty for a page that never had a
    /// document.
    pub fn page_generations(&self, page: PageKey) -> Vec<u64> {
        let Some(record) = self.page_record(page) else {
            return Vec::new();
        };
        let Some(ValueOrContainer::Container(c)) = record.get(K_PAGE_GENS) else {
            return Vec::new();
        };
        let Ok(gens) = c.into_map() else {
            return Vec::new();
        };
        let keys: Vec<String> = gens.keys().map(|k| k.to_string()).collect();
        crate::pages::generations(keys.iter().map(String::as_str))
    }

    /// The page's current generation — the numeric maximum of its keys
    /// ([`crate::pages::current_generation`]) — or `None` for a page that has
    /// never had a document.
    pub fn current_page_generation(&self, page: PageKey) -> Option<u64> {
        crate::pages::current_generation(
            self.page_generations(page)
                .iter()
                .map(|g| g.to_string())
                .collect::<Vec<_>>()
                .iter()
                .map(String::as_str),
        )
    }

    /// Record that `page` has a document of generation `generation`.
    ///
    /// **Grow-only.** The key is added and never removed; two devices adding
    /// generations offline converge on the union, and the current one is the
    /// numeric maximum whichever order the keys arrive in. Adding a
    /// generation the page already has is a no-op. Answers `false` only for a
    /// list the table has never held; a deleted list still takes the write,
    /// because its document outlives it.
    pub fn add_page_generation(&mut self, page: PageKey, generation: u64) -> Result<bool> {
        let Some(record) = self.page_record_for_write(page)? else {
            return Ok(false);
        };
        let known = self.page_generations(page);
        if known.contains(&generation) {
            return Ok(true);
        }
        // **A replacement freezes the names it supersedes**, in the same
        // commit (§6.6): each older generation keeps the name it had — its
        // own title if it was renamed, else the list's name as it stands, else
        // the bucket's token — so a later list rename moves only the new one.
        for older in known.into_iter().filter(|g| *g < generation) {
            if self.page_frozen(page, older).is_none() {
                let frozen = self.frozen_name(page, older);
                write_frozen(&record, older, &frozen)?;
            }
        }
        record
            .ensure_mergeable_map(K_PAGE_GENS)?
            .insert(&generation.to_string(), true)?;
        self.doc.commit();
        Ok(true)
    }

    // ----------------------------------------------------- deletion events

    /// The deletion protocol of one page-document generation, read whole
    /// (page-document plan §6.2). The persisted keys are grow-only and the
    /// uncovered values are monotone joins, so this reconstructs the same
    /// [`DeletionRecord`] on every replica whatever order they arrived in.
    pub fn deletion_record(&self, page: PageKey, generation: u64) -> DeletionRecord {
        let mut record = DeletionRecord::new();
        let Some(page_record) = self.page_record(page) else {
            return record;
        };
        let gen_key = generation.to_string();
        // deletions.<gen>.<event> -> { covers, deleted_ms }
        if let Some(by_gen) = record_map(&page_record, K_PAGE_DELETIONS)
            && let Some(events) = record_map(&by_gen, &gen_key)
        {
            for event_key in events.keys() {
                let Some(event) = parse_event(&event_key) else {
                    continue;
                };
                let Some(fields) = record_map(&events, &event_key) else {
                    continue;
                };
                let covers = read_vv(&fields, F_COVERS).unwrap_or_default();
                let deleted_ms = fields
                    .get(F_DELETED_MS)
                    .and_then(crate::schema::as_i64)
                    .unwrap_or(0);
                record.delete(event, covers, deleted_ms);
            }
        }
        // recovers.<gen>.<replica|event> -> true
        if let Some(by_gen) = record_map(&page_record, K_PAGE_RECOVERS)
            && let Some(recovers) = record_map(&by_gen, &gen_key)
        {
            for key in recovers.keys() {
                if let Some((replica, event)) = parse_replica_event(&key) {
                    record.recover(replica, [event]);
                }
            }
        }
        // keeps.<gen>.<event>.<decision> -> { replica, covers }
        if let Some(by_gen) = record_map(&page_record, K_PAGE_KEEPS)
            && let Some(events) = record_map(&by_gen, &gen_key)
        {
            for event_key in events.keys() {
                let Some(event) = parse_event(&event_key) else {
                    continue;
                };
                let Some(decisions) = record_map(&events, &event_key) else {
                    continue;
                };
                for decision_key in decisions.keys() {
                    let Some(decision) = parse_decision(&decision_key) else {
                        continue;
                    };
                    let Some(fields) = record_map(&decisions, &decision_key) else {
                        continue;
                    };
                    let replica = fields
                        .get(F_KEEP_REPLICA)
                        .and_then(crate::schema::as_string)
                        .and_then(|s| u64::from_str_radix(&s, 16).ok())
                        .unwrap_or(0);
                    let covers = read_vv(&fields, F_COVERS).unwrap_or_default();
                    record.keep(event, decision, Keep { replica, covers });
                }
            }
        }
        // uncovered.<gen>.<replica|event> -> covers
        if let Some(by_gen) = record_map(&page_record, K_PAGE_UNCOVERED)
            && let Some(uncovered) = record_map(&by_gen, &gen_key)
        {
            for key in uncovered.keys() {
                if let Some((replica, event)) = parse_replica_event(&key)
                    && let Some(covers) = read_vv(&uncovered, &key)
                {
                    record.report_uncovered(replica, event, &covers);
                }
            }
        }
        record
    }

    /// Write one deletion event (§6.4 step 1): a fresh id, its coverage and
    /// when it happened. Grow-only — a repeat is a no-op.
    pub fn write_deletion(
        &mut self,
        page: PageKey,
        generation: u64,
        event: EventId,
        covers: &loro::VersionVector,
        deleted_ms: i64,
    ) -> Result<bool> {
        let Some(record) = self.page_record_for_write(page)? else {
            return Ok(false);
        };
        let events = record
            .ensure_mergeable_map(K_PAGE_DELETIONS)?
            .ensure_mergeable_map(&generation.to_string())?;
        let fields = events.ensure_mergeable_map(&event.to_string())?;
        if fields.get(F_COVERS).is_none() {
            fields.insert(F_COVERS, LoroValue::Binary(covers.encode().into()))?;
            fields.insert(F_DELETED_MS, deleted_ms)?;
        }
        self.doc.commit();
        Ok(true)
    }

    /// Record a *Recover* by `replica` of every event it has observed (§6.3):
    /// one key each, so an event it never saw stays in force.
    pub fn write_recover(
        &mut self,
        page: PageKey,
        generation: u64,
        replica: Replica,
        observed: impl IntoIterator<Item = EventId>,
    ) -> Result<bool> {
        let Some(record) = self.page_record_for_write(page)? else {
            return Ok(false);
        };
        let recovers = record
            .ensure_mergeable_map(K_PAGE_RECOVERS)?
            .ensure_mergeable_map(&generation.to_string())?;
        for event in observed {
            recovers.insert(&replica_event_key(replica, event), true)?;
        }
        self.doc.commit();
        Ok(true)
    }

    /// Record one *Keep deleted* decision under its own id (§6.3): grow-only,
    /// written once.
    pub fn write_keep(
        &mut self,
        page: PageKey,
        generation: u64,
        event: EventId,
        decision: DecisionId,
        keep: &Keep,
    ) -> Result<bool> {
        let Some(record) = self.page_record_for_write(page)? else {
            return Ok(false);
        };
        let decisions = record
            .ensure_mergeable_map(K_PAGE_KEEPS)?
            .ensure_mergeable_map(&generation.to_string())?
            .ensure_mergeable_map(&event.to_string())?;
        let fields = decisions.ensure_mergeable_map(&decision.to_string())?;
        if fields.get(F_COVERS).is_none() {
            fields.insert(F_KEEP_REPLICA, format!("{:016x}", keep.replica))?;
            fields.insert(F_COVERS, LoroValue::Binary(keep.covers.encode().into()))?;
        }
        self.doc.commit();
        Ok(true)
    }

    /// Join `version` into `replica`'s record of writing outside `event`'s
    /// coverage (§6.2, *evidence only grows*). The stored value is replaced
    /// by the join, which is monotone, so a smaller or concurrent report
    /// never shrinks it.
    pub fn report_uncovered(
        &mut self,
        page: PageKey,
        generation: u64,
        replica: Replica,
        event: EventId,
        version: &loro::VersionVector,
    ) -> Result<bool> {
        let Some(record) = self.page_record_for_write(page)? else {
            return Ok(false);
        };
        let uncovered = record
            .ensure_mergeable_map(K_PAGE_UNCOVERED)?
            .ensure_mergeable_map(&generation.to_string())?;
        let key = replica_event_key(replica, event);
        let joined = match read_vv(&uncovered, &key) {
            Some(previous) => crate::pages::join(&previous, version),
            None => version.clone(),
        };
        uncovered.insert(&key, LoroValue::Binary(joined.encode().into()))?;
        self.doc.commit();
        Ok(true)
    }

    /// Every list record, **tombstones included**, in rail order — what the
    /// map from a page document back to its page needs, since a deleted
    /// list keeps its document.
    pub fn all_lists(&self) -> Vec<ListEntry> {
        let lists = self.doc.get_map(C_LISTS);
        let mut out = Vec::new();
        let keys: Vec<String> = lists.keys().map(|k| k.to_string()).collect();
        for key in keys {
            let Some(id) = ListId::parse(&key) else {
                continue;
            };
            let Some(ValueOrContainer::Container(c)) = lists.get(&key) else {
                continue;
            };
            let Ok(record) = c.into_map() else { continue };
            out.push(read_list(id, &record));
        }
        out.sort_by(|a, b| {
            a.order
                .is_none()
                .cmp(&b.order.is_none())
                .then(a.order.cmp(&b.order))
                .then(a.name.cmp(&b.name))
                .then(a.id.cmp(&b.id))
        });
        out
    }

    /// A direct rename of generation `generation`'s document, if one stands.
    pub fn page_override(&self, page: PageKey, generation: u64) -> Option<String> {
        let record = self.page_record(page)?;
        let overrides = record_map(&record, K_PAGE_OVERRIDE)?;
        overrides
            .get(&generation.to_string())
            .and_then(crate::schema::as_string)
    }

    /// Rename generation `generation`'s document directly — Explore's
    /// *Rename*, MCP's — or take the rename back with `None`. A superseded
    /// generation's rename is also its frozen name: for one, `frozen` *is*
    /// its name. Answers `false` for a page record that does not exist.
    pub fn set_page_override(
        &mut self,
        page: PageKey,
        generation: u64,
        title: Option<&str>,
    ) -> Result<bool> {
        let Some(record) = self.page_record_for_write(page)? else {
            return Ok(false);
        };
        let key = generation.to_string();
        let overrides = record.ensure_mergeable_map(K_PAGE_OVERRIDE)?;
        match title {
            Some(title) => overrides.insert(&key, title)?,
            None if overrides.get(&key).is_some() => overrides.delete(&key)?,
            None => {}
        }
        let superseded = self
            .current_page_generation(page)
            .is_some_and(|current| generation < current);
        if superseded && let Some(title) = title {
            write_frozen(&record, generation, &FrozenTitle::Text(title.to_owned()))?;
        }
        self.doc.commit();
        Ok(true)
    }

    /// A superseded generation's frozen name, if its replacement froze one.
    pub fn page_frozen(&self, page: PageKey, generation: u64) -> Option<FrozenTitle> {
        let record = self.page_record(page)?;
        let frozen = record_map(&record, K_PAGE_FROZEN)?;
        let ValueOrContainer::Value(value) = frozen.get(&generation.to_string())? else {
            return None;
        };
        read_frozen(&value)
    }

    /// What generation `generation`'s name would freeze as right now.
    fn frozen_name(&self, page: PageKey, generation: u64) -> FrozenTitle {
        if let Some(title) = self.page_override(page, generation) {
            return FrozenTitle::Text(title);
        }
        match page {
            PageKey::List(list) => FrozenTitle::Text(
                self.list_entry(list)
                    .map(|entry| entry.name)
                    .unwrap_or_default(),
            ),
            PageKey::Bucket(bucket) => FrozenTitle::Bucket(bucket.as_str().to_owned()),
        }
    }

    /// **The title of generation `generation`'s document** (plan §6.6): the
    /// current generation is its override, else the list's name as it stands
    /// (a deleted list's last name) or the bucket; a superseded one is its
    /// frozen name, else its override, else the same. Never root text.
    pub fn page_title(&self, page: PageKey, generation: u64) -> PageTitle {
        let superseded = self
            .current_page_generation(page)
            .is_some_and(|current| generation < current);
        if superseded && let Some(frozen) = self.page_frozen(page, generation) {
            return match frozen {
                FrozenTitle::Text(text) => PageTitle::Text(text),
                FrozenTitle::Bucket(token) => Bucket::ALL
                    .into_iter()
                    .find(|b| b.as_str() == token)
                    .map(PageTitle::Bucket)
                    .unwrap_or(PageTitle::Text(token)),
            };
        }
        if let Some(title) = self.page_override(page, generation) {
            return PageTitle::Text(title);
        }
        match page {
            PageKey::List(list) => PageTitle::Text(
                self.list_entry(list)
                    .map(|entry| entry.name)
                    .unwrap_or_default(),
            ),
            PageKey::Bucket(bucket) => PageTitle::Bucket(bucket),
        }
    }

    fn list_record(&self, list: ListId) -> Option<LoroMap> {
        match self.doc.get_map(C_LISTS).get(&list.to_storage())? {
            ValueOrContainer::Container(c) => c.into_map().ok(),
            _ => None,
        }
    }

    /// The record, only while the list is live. Every write goes through this,
    /// so a deleted list cannot be renamed, recolored or reordered back into
    /// existence by a surface that forgot to check.
    fn live_list_record(&self, list: ListId) -> Option<LoroMap> {
        let record = self.list_record(list)?;
        read_list(list, &record)
            .deleted_ms
            .is_none()
            .then_some(record)
    }
}

/// A mergeable map under a record, if it exists.
fn record_map(record: &LoroMap, key: &str) -> Option<LoroMap> {
    match record.get(key)? {
        ValueOrContainer::Container(c) => c.into_map().ok(),
        _ => None,
    }
}

/// One frozen name as its stored value: a whole map, one insert.
fn write_frozen(record: &LoroMap, generation: u64, frozen: &FrozenTitle) -> Result<()> {
    let (kind, value) = match frozen {
        FrozenTitle::Text(text) => (FROZEN_TEXT, text.as_str()),
        FrozenTitle::Bucket(token) => (FROZEN_BUCKET, token.as_str()),
    };
    let stored = LoroValue::from(HashMap::from([
        (F_FROZEN_KIND.to_string(), LoroValue::from(kind)),
        (F_FROZEN_VALUE.to_string(), LoroValue::from(value)),
    ]));
    record
        .ensure_mergeable_map(K_PAGE_FROZEN)?
        .insert(&generation.to_string(), stored)?;
    Ok(())
}

fn parse_event(key: &str) -> Option<EventId> {
    uuid::Uuid::parse_str(key).ok().map(EventId::from_uuid)
}

fn parse_decision(key: &str) -> Option<DecisionId> {
    uuid::Uuid::parse_str(key).ok().map(DecisionId::from_uuid)
}

fn parse_replica_event(key: &str) -> Option<(Replica, EventId)> {
    let (replica, event) = key.split_once('|')?;
    Some((
        u64::from_str_radix(replica, 16).ok()?,
        parse_event(event)?,
    ))
}

fn read_frozen(value: &LoroValue) -> Option<FrozenTitle> {
    let LoroValue::Map(map) = value else {
        return None;
    };
    let text = |key: &str| match map.get(key)? {
        LoroValue::String(s) => Some(s.to_string()),
        _ => None,
    };
    let (kind, value) = (text(F_FROZEN_KIND)?, text(F_FROZEN_VALUE)?);
    match kind.as_str() {
        FROZEN_TEXT => Some(FrozenTitle::Text(value)),
        FROZEN_BUCKET => Some(FrozenTitle::Bucket(value)),
        _ => None,
    }
}

/// A raw binary value under `key`, if present.
fn as_binary(map: &LoroMap, key: &str) -> Option<Vec<u8>> {
    match map.get(key)?.as_value()? {
        LoroValue::Binary(b) => Some(b.to_vec()),
        _ => None,
    }
}

fn read_vv(map: &LoroMap, key: &str) -> Option<loro::VersionVector> {
    loro::VersionVector::decode(&as_binary(map, key)?).ok()
}

/// The join key a replica writes a per-event record under.
fn replica_event_key(replica: Replica, event: EventId) -> String {
    format!("{replica:016x}|{event}")
}

fn read_tag(id: TagId, record: &LoroMap) -> TagEntry {
    TagEntry {
        id,
        name: record
            .get(K_TAG_NAME)
            .and_then(crate::schema::as_string)
            .unwrap_or_default(),
        color: record.get(K_TAG_COLOR).and_then(crate::schema::as_string),
        created_ms: record.get(K_TAG_CREATED).and_then(crate::schema::as_i64),
        alias_of: record
            .get(K_TAG_ALIAS)
            .and_then(crate::schema::as_string)
            .as_deref()
            .and_then(TagId::parse),
    }
}

fn read_list(id: ListId, record: &LoroMap) -> ListEntry {
    ListEntry {
        id,
        name: record
            .get(K_LIST_NAME)
            .and_then(crate::schema::as_string)
            .unwrap_or_default(),
        color: record.get(K_LIST_COLOR).and_then(crate::schema::as_string),
        order: record.get(K_LIST_ORDER).and_then(crate::schema::as_i64),
        created_ms: record.get(K_LIST_CREATED).and_then(crate::schema::as_i64),
        deleted_ms: record.get(K_LIST_DELETED).and_then(crate::schema::as_i64),
    }
}

fn read_definition(id: &str, record: &LoroMap) -> Option<AttributeDefinition> {
    let value_type = ValueType::parse(&record.get(K_DEF_TYPE).and_then(crate::schema::as_string)?)?;
    let options = match record.get(K_DEF_OPTIONS) {
        Some(ValueOrContainer::Value(LoroValue::List(items))) => items
            .iter()
            .filter_map(|v| match v {
                LoroValue::String(s) => Some(s.to_string()),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    };
    Some(AttributeDefinition {
        id: id.to_owned(),
        key: record.get(K_DEF_KEY).and_then(crate::schema::as_string)?,
        label: record
            .get(K_DEF_LABEL)
            .and_then(crate::schema::as_string)
            .unwrap_or_default(),
        value_type,
        cardinality: record
            .get(K_DEF_CARDINALITY)
            .and_then(crate::schema::as_string)
            .as_deref()
            .and_then(Cardinality::parse)
            .unwrap_or(Cardinality::One),
        scope: record
            .get(K_DEF_SCOPE)
            .and_then(crate::schema::as_string)
            .as_deref()
            .and_then(AttributeScope::parse)
            .unwrap_or(AttributeScope::Shared),
        system: false,
        options,
        filterable: record
            .get(K_DEF_FILTERABLE)
            .and_then(crate::schema::as_bool)
            .unwrap_or(true),
        sortable: record
            .get(K_DEF_SORTABLE)
            .and_then(crate::schema::as_bool)
            .unwrap_or(false),
        groupable: record
            .get(K_DEF_GROUPABLE)
            .and_then(crate::schema::as_bool)
            .unwrap_or(false),
    })
}

#[cfg(test)]
mod tests;
