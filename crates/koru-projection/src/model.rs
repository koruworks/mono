// SPDX-License-Identifier: MPL-2.0

//! What a document looks like to everything that is not the editor.
//!
//! ## Strings and primitives, on purpose
//!
//! Nothing here names a type from `koru-core`. A kind is the token
//! `Kind::as_str()` already stores in Loro, a heading is a number, an attribute
//! value is a tagged string. That is not laziness about types — it is the
//! boundary: **a consumer of the projection links no node model at all.** The
//! search index, a retrieval pipeline and a kanban view each get everything they
//! need from this crate and `rusqlite`, and none of them acquires an opinion
//! about what a `Kind::ListBlock` is.
//!
//! The tokens are the same ones the document stores, so they are stable for the
//! same reason the storage keys are: changing one orphans what is written with
//! it.
//!
//! ## What is here, and the test that decided it
//!
//! **Can a consumer recover this without reopening the document?** Carry what
//! fails that test; resolve everything else by joining.
//!
//! - Structure, kind and the block properties a view branches on — nothing else
//!   has them.
//! - Plain text, which is the matchable truth.
//! - **Runs**, so emphasis, links and pill anchors survive. A rendering consumer
//!   — an export, a preview, a context window — would otherwise have to open the
//!   `LoroDoc` itself, which is the thing this exists to prevent.
//! - **Typed attributes.** A date stays a date. Flattening them into text is
//!   what makes a calendar impossible to build on this.
//!
//! What is deliberately absent from a **node**: the document's title path.
//! Denormalizing it there would make a rename rewrite every descendant —
//! turning a one-document edit into an N-document write, which is the one cost
//! this layer must never have. It is carried instead as [`ListedDocument`], one
//! row per document, where a rename is one row.
//!
//! Search vocabulary — fragments, breadcrumbs, normalized terms — is absent.
//! That is an index's business, and putting it here would make every other
//! consumer inherit search's shape.

use koru_store_core::ids::{DocumentId, WorkspaceId};
use std::collections::HashMap;

/// The shape of everything in this module.
///
/// **A change here is a rebuild, never a migration.** The projection is derived
/// and disposable, so the cheap correct answer is to throw it away and walk the
/// documents again — which is also why nothing in this crate has a migration
/// engine. `koru-store-sqlite` has one because its data is authoritative; this
/// data is not.
///
/// **An index counts as a shape.** The 7 -> 8 bump adds no column and changes
/// no row: it adds `attrs_probe`, the covering index the tasks query is built
/// on. `SCHEMA`'s `CREATE INDEX IF NOT EXISTS` would put it on an existing
/// database at the next open regardless — so the bump is not what makes the
/// index appear, and pretending otherwise would be the wrong reason to make it.
/// The reason is that `user_version` is the one answer to *is this database the
/// shape this build expects*, and without a bump two different shapes both
/// answer 7: one where a scoped tasks page costs 14 ms and one where it costs
/// 220 ms, told apart only by `EXPLAIN QUERY PLAN`. A number that cannot
/// distinguish those is not doing its job.
pub const PROJECTION_SCHEMA_VERSION: i64 = 8;

/// Manifest kinds the projection does not follow: a delegated run and a
/// schedule (`docs/architecture/DATAMODEL.md`, ADR-343).
///
/// Neither has a node tree — a run is a `run` map and a `timeline` list —
/// so walking one yields zero rows and costs a fork that is linear in the
/// run's history, on every step, on the projection thread: 18.8 ms at ten
/// thousand entries. A commit to one of these is skipped before the fork,
/// a sweep never compares it, and [`crate::worker::Source::record_activity`]
/// never sees it — which is what keeps it out of automatic history too. The
/// tokens are `koru_core::explore`'s, the one place that decides what a
/// person browses, so the hiding and the skipping name the same literal.
///
/// **The fallback still holds.** A run whose kind the mirror does not yet
/// know (a listing refresh that failed) is projected like any other: it
/// adopts, is writable, and projects to no rows and no error —
/// `walk::tests::a_run_document_projects_to_no_rows_and_no_error`.
///
/// **A reading document** (OCR TECHNICAL D14) is the fifth: a `readings` map
/// of words per page, no node tree, written by the reader rather than by a
/// person. Because a commit to one is dropped here before any consumer hears
/// of it, nothing that wants to know a reading changed may learn it from this
/// crate; the Store daemon carries that change itself (TECHNICAL §7).
pub const NOT_PROJECTED: [&str; 5] = [
    koru_core::explore::KIND_RUN,
    koru_core::explore::KIND_SCHEDULE,
    koru_core::explore::KIND_SOCIAL_POST,
    koru_core::explore::KIND_SOCIAL_CHANNEL,
    koru_core::explore::KIND_READING,
];

/// Manifest kinds whose history the Store never captures: no version is
/// recorded for one when its checkpoint is taken
/// (`koru_store_core::Store::with_unversioned_kinds`).
///
/// **The host decides, because the store cannot.** `koru-store-core` treats
/// documents as opaque and links nothing that names a kind, so the daemon
/// hands it this set when it opens the Store. Automatic history is already
/// kept off these by [`NOT_PROJECTED`] — the projector never hands one to
/// `record_activity` — and this closes the other door, the checkpoint's own
/// history row.
///
/// Only the reading document (OCR TECHNICAL D14): a person never writes into
/// one, so a version of it is a version nobody can want, and a large archive
/// re-read would otherwise fill `document_versions` with snapshots of words.
/// Whether a run and a schedule belong here too is the same question for
/// kinds that are not that feature's, and it is noted there, not changed.
pub const UNVERSIONED: [&str; 1] = [koru_core::explore::KIND_READING];

/// Whether a document of this manifest kind is walked at all.
pub fn projected(kind: &str) -> bool {
    !NOT_PROJECTED.contains(&kind)
}

/// Whether the current projection may describe the authoritative source.
/// Stored beside derived rows, never in the Store document itself.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ProjectionSourceStatus {
    Current,
    SchemaTooNew {
        found: i64,
        understood: i64,
        source_revision: Vec<u8>,
    },
}

/// One document as the **workspace's listing** has it, rather than as its own
/// content has it.
///
/// The manifest is a document to the store and is never walked here, but what
/// it holds — a title, a parent, whether it is in the trash — is what every
/// consumer joins against. A search result that cannot say which document it is
/// in is not a search result.
///
/// **Mirrored rather than joined live.** The store's own copy sits behind one
/// `Mutex` covering the whole store, so a query touching it would block every
/// commit. Mirroring it here also means a consumer that was stopped can recover
/// it by comparison — a listing it could only be *told* about would be a stream
/// it must not miss, which is the thing this layer exists not to have.
///
/// Plain types, not `koru_store_core::model::DocumentEntry`: that one carries a
/// `Timestamp` and `serde`, and a consumer of the projection should need
/// neither.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ListedDocument {
    pub document: DocumentId,
    /// The document this one sits under, if any. The whole of the title path,
    /// resolved by following it.
    pub parent: Option<DocumentId>,
    pub ordinal: u32,
    pub title: String,
    /// The application's own token — `page`, `task-list`. The store never
    /// interprets it and neither does this.
    pub kind: String,
    /// MIME type recorded by the manifest for opaque files. `None` for notes
    /// and for older file entries whose extension remains the fallback.
    pub mime: Option<String>,
    /// **Binned, not deleted** ([ADR-95](../../../docs/decisions/ADR-076-100.md#adr-95-trash--a-document-in-the-trash-is-a-flag-on-its-manifest-entry)).
    /// A document in the trash keeps its content and its projection, because the
    /// bin is a place somebody searches and restores from; it is absent from a
    /// listing only because whoever draws one filters on this.
    ///
    /// **Effective, not raw.** The manifest stamps only the entry that was
    /// binned and lets the subtree inherit; a consumer of this listing filters
    /// row by row, so the stamp is carried down the tree before mirroring —
    /// see [`apply_inherited_trash`].
    pub trashed_at: Option<i64>,
    pub trashed_by: Option<String>,
}

/// Carry inherited trash down a listing, in place.
///
/// **The manifest stamps only the entry that was binned**; a trashed document
/// takes its subtree by inheritance ([ADR-95](../../../docs/decisions/ADR-076-100.md#adr-95-trash--a-document-in-the-trash-is-a-flag-on-its-manifest-entry)),
/// so a raw listing row cannot answer "is this document in the trash" on its
/// own. The projection is where that answer becomes row-local — a consumer
/// filters on the stamp without walking parents — so a row with no stamp of
/// its own takes the nearest trashed ancestor's, stamper included. A row with
/// its own stamp keeps it: binned directly before its parent went, it stays
/// binned when the parent comes back.
pub fn apply_inherited_trash(documents: &mut [ListedDocument]) {
    let index_of: HashMap<DocumentId, usize> = documents
        .iter()
        .enumerate()
        .map(|(i, document)| (document.document, i))
        .collect();
    let mut effective = Vec::with_capacity(documents.len());
    for start in 0..documents.len() {
        let mut stamp = (
            documents[start].trashed_at,
            documents[start].trashed_by.clone(),
        );
        let mut ancestor = documents[start].parent;
        // The hop limit is a cycle guard: a listing is a tree, but a corrupt
        // mirror must not hang the projector.
        for _ in 0..documents.len() {
            if stamp.0.is_some() {
                break;
            }
            let Some(&up) = ancestor.and_then(|id| index_of.get(&id)) else {
                break;
            };
            stamp = (documents[up].trashed_at, documents[up].trashed_by.clone());
            ancestor = documents[up].parent;
        }
        effective.push(stamp);
    }
    for (document, (trashed_at, trashed_by)) in documents.iter_mut().zip(effective) {
        document.trashed_at = trashed_at;
        document.trashed_by = trashed_by;
    }
}

/// A document the read model holds that no workspace listing names.
///
/// **A third state, and not the same as trashed.** A trashed document is *in*
/// the listing with a `trashed_at`; this is a document the listing does not
/// mention at all, so nothing in the app can reach it: Explore walks the
/// manifest, and search refuses to index a document it cannot name a listing
/// row for. The content is intact and the read model has all of it — which is
/// why this type carries a title and a preview rather than just an id. A
/// reviewer deciding what to do with one needs to see what it is.
///
/// Found by the search indexer, which used to hand these to `index_one`
/// forever; see `koru_search::worker`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnfiledDocument {
    pub workspace: WorkspaceId,
    pub workspace_name: String,
    pub document: DocumentId,
    /// The document's own first row, which is its title — a document keeps a
    /// root node for it and the manifest *mirrors* that, so the title survives
    /// the manifest entry going missing.
    pub title: String,
    /// The next few rows, joined, so a title alone is not the whole basis of a
    /// decision.
    pub preview: String,
    pub nodes: usize,
    pub projected_at: i64,
    /// A document the listing *does* name, in the same workspace, has this
    /// title.
    ///
    /// **The flag any purge decision would rest on.** It separates "a stray
    /// second copy" from "the only copy", and those want opposite answers.
    /// Deliberately a weak signal — same title, not same content — so it
    /// prompts a look rather than deciding anything.
    pub same_title_as_listed: bool,
}

/// One workspace's listing, and what the workspace is called.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct WorkspaceListing {
    pub workspace: WorkspaceId,
    pub name: String,
    /// What the listing hashes to.
    ///
    /// **The comparison, in the same idiom as `source_revision`.** A consumer
    /// records the digest it consumed and re-reads when it differs, rather than
    /// trusting that it saw every change.
    pub digest: Vec<u8>,
    pub documents: Vec<ListedDocument>,
}

/// One document, as a read model.
#[derive(Clone, PartialEq, Debug)]
pub struct DocumentProjection {
    pub workspace: WorkspaceId,
    pub document: DocumentId,
    /// The store revision this was built from — `VersionVector::encode()`.
    ///
    /// **Compared, never trusted as an ordering.** Two revisions can be
    /// incomparable, and a recovered document's can go *backwards*; see
    /// `Revision` in `store.rs` for the rule that follows from it.
    pub source_revision: Vec<u8>,
    /// In document order, parents before children.
    pub nodes: Vec<ProjectedNode>,
}

/// One node of a document.
#[derive(Clone, PartialEq, Debug, Hash)]
pub struct ProjectedNode {
    /// The node's stable id, as text. **Only meaningful inside its document** —
    /// which is why every row that names one is keyed by the document too.
    pub id: String,
    pub parent: Option<String>,
    /// Position among its siblings.
    pub ordinal: usize,
    /// Distance from the document root, so a consumer can shape a tree without
    /// walking parents.
    pub depth: usize,
    /// `Kind::as_str()` — the token the document itself stores.
    pub kind: String,
    pub text: String,
    /// 1–6, or `None` for body text.
    pub heading: Option<u8>,
    /// The list style a container carries: `bullet`, `checkbox`, `numbered`…
    pub list_style: Option<String>,
    /// A task's checkbox.
    pub done: bool,
    /// A person's own name for this level of the hierarchy.
    pub level_name: Option<String>,
    /// An authored display name for content with no text of its own — an image.
    pub name: Option<String>,
    pub icon: Option<String>,
    pub colour: Option<String>,
    /// The grammar a code block highlights as.
    pub language: Option<String>,
    /// The content hash an image's bytes live at, and their type. A consumer
    /// cannot find the picture without them.
    pub blob: Option<String>,
    pub mime: Option<String>,
    /// A block-level reference — the node this one stands in for. The
    /// counterpart of a pill, and the other half of the edges between nodes.
    pub embed: Option<String>,
    /// **This node is in the document's own bin** — it is the hidden
    /// `Kind::Trash` root, or it sits under one.
    ///
    /// **Not the same trash as [`ListedDocument::trashed_at`]**, and the two
    /// are deliberately spelled differently. That one is a *document* the
    /// workspace's manifest binned, inherited down the document tree; this one
    /// is a *node* somebody binned inside a document, which is an ordinary move
    /// into a hidden root and keeps the node's text, marks and pills
    /// ([ADR-69](../../../docs/decisions/ADR-051-075.md#adr-69--trashing-a-document-is-a-move-into-a-hidden-library-node-and-nothing-ever-purges-it)).
    /// A node can be either, both or neither.
    ///
    /// **Flagged, never hidden.** The bin is projected precisely so a search
    /// can find what is in it and offer to put it back; a query that means
    /// *live content* filters on this rather than the projection dropping the
    /// rows — see `store.rs`, "two notions of trash", for which query answers
    /// which way.
    pub in_trash: bool,
    pub runs: Vec<ProjectedRun>,
    pub attrs: Vec<ProjectedAttr>,
}

/// One heading of a document, in reading order.
///
/// The shape a deep-link picker needs and nothing more: no runs, no attrs, no
/// Loro decode. A heading is a *block property* rather than a `Kind` — a
/// heading node's `kind` reads `paragraph` — so the only correct predicate is
/// `heading IS NOT NULL`, never a kind filter.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ProjectedHeading {
    /// The node's stable id, as text. Only meaningful inside its document.
    pub node: String,
    /// 1–6. Clamped on read: a corrupt row must not panic a `u8` cast.
    pub level: u8,
    pub text: String,
    /// Document order, so a consumer can present headings as they are read.
    pub seq: usize,
}

/// A stretch of one node's text that is formatted the same way throughout.
///
/// Byte ranges, matching the document's own coordinate system — Loro indexes in
/// unicode scalars and everything above it in UTF-8 bytes, and these are the
/// bytes, so a consumer slicing `text` with them cannot land mid-character.
#[derive(Clone, PartialEq, Eq, Debug, Default, Hash)]
pub struct ProjectedRun {
    pub start: usize,
    pub end: usize,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    pub code: bool,
    /// The highlight colour's stored name, if any. A *name*, not RGB, because
    /// that is what the document stores — it is resolved by the theme so a
    /// highlight stays legible in dark mode.
    pub highlight: Option<String>,
    /// `sup` or `sub`. Carried because `x²` and `x₂` are different words.
    pub script: Option<String>,
    /// Set when this run is a pill anchor, naming the attribute it draws — the
    /// only sight of the edges between nodes.
    pub pill: Option<String>,
}

impl ProjectedRun {
    /// Whether this run carries no formatting at all, which is what most prose
    /// is. Worth asking before writing a row for it.
    pub fn is_plain(&self) -> bool {
        !self.bold
            && !self.italic
            && !self.underline
            && !self.strike
            && !self.code
            && self.highlight.is_none()
            && self.script.is_none()
            && self.pill.is_none()
    }
}

/// A labelled property on a node — a due date, a link, a reference.
#[derive(Clone, PartialEq, Eq, Debug, Hash)]
pub struct ProjectedAttr {
    pub id: String,
    /// What the property means to the person: `due`, `owner`, `link`.
    pub role: String,
    /// Which shape `value` is: `date`, `datetime_floating`, `datetime_zoned`,
    /// `number`, `bool`, `ref`, `url`, `text`.
    ///
    /// **Kept apart from the value** so a consumer can ask for dates without
    /// parsing every string and hoping. This is the whole reason attributes are
    /// not flattened into text.
    ///
    /// **The three temporal types keep three tokens** ([ADR-183]). There is no
    /// `datetime` that might be either, so a query never has to guess whether
    /// the row it is comparing carries a zone, and a consumer that only
    /// understands calendar days can filter to `date` and be right.
    ///
    /// [ADR-183]: ../../../docs/decisions/ADR-201-225.md#adr-206-timemodel--koru-has-three-temporal-types-not-one-with-a-nullable-timezone
    pub kind: String,
    /// The value in its canonical form — a calendar date as `YYYY-MM-DD`, a
    /// floating wall clock as `YYYY-MM-DDTHH:MM:SS`, a zoned instant as
    /// decimal epoch-ms, a reference as the target's id, a url or text
    /// verbatim.
    pub value: String,
    /// The IANA zone a `datetime_zoned` value was meant in — and `None` for
    /// every other kind.
    ///
    /// It rides beside the instant rather than being dropped as presentation,
    /// because an instant with no zone is exactly the ambiguous shape the
    /// three types abolish: a consumer reading this row must be able to tell
    /// *what it meant*, not just when it was. The zone a reader *displays* it
    /// in is a separate decision the consumer makes.
    pub zone: Option<String>,
    /// Display text when it cannot be derived from the value.
    pub label: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listed(parent: Option<DocumentId>, trashed_at: Option<i64>) -> ListedDocument {
        ListedDocument {
            document: DocumentId::new(),
            parent,
            ordinal: 0,
            title: "t".into(),
            kind: "page".into(),
            mime: None,
            trashed_at,
            trashed_by: trashed_at.map(|_| "Tester".to_owned()),
        }
    }

    #[test]
    fn a_child_inherits_its_trashed_parents_stamp() {
        let mut parent = listed(None, Some(10));
        parent.trashed_by = Some("Parent's trasher".into());
        let parent_id = parent.document;
        let child = listed(Some(parent_id), None);
        let child_id = child.document;
        let mut documents = vec![parent, child];

        apply_inherited_trash(&mut documents);

        let child = documents.iter().find(|d| d.document == child_id).unwrap();
        assert_eq!(child.trashed_at, Some(10));
        assert_eq!(child.trashed_by.as_deref(), Some("Parent's trasher"));
    }

    #[test]
    fn a_grandchild_inherits_through_a_live_parent() {
        let root = listed(None, Some(5));
        let root_id = root.document;
        let middle = listed(Some(root_id), None);
        let middle_id = middle.document;
        let leaf = listed(Some(middle_id), None);
        let leaf_id = leaf.document;
        let mut documents = vec![root, middle, leaf];

        apply_inherited_trash(&mut documents);

        assert_eq!(
            documents
                .iter()
                .find(|d| d.document == leaf_id)
                .unwrap()
                .trashed_at,
            Some(5)
        );
    }

    /// Binned directly before its parent went, it keeps its own stamp — and
    /// stays binned when the parent comes back.
    #[test]
    fn a_directly_trashed_child_keeps_its_own_stamp() {
        let parent = listed(None, Some(10));
        let parent_id = parent.document;
        let mut child = listed(Some(parent_id), Some(3));
        child.trashed_by = Some("Child's trasher".into());
        let child_id = child.document;
        let mut documents = vec![parent, child];

        apply_inherited_trash(&mut documents);

        let child = documents.iter().find(|d| d.document == child_id).unwrap();
        assert_eq!(child.trashed_at, Some(3));
        assert_eq!(child.trashed_by.as_deref(), Some("Child's trasher"));
    }

    #[test]
    fn a_live_sibling_of_a_trashed_document_stays_live() {
        let trashed = listed(None, Some(1));
        let live = listed(None, None);
        let live_id = live.document;
        let mut documents = vec![trashed, live];

        apply_inherited_trash(&mut documents);

        assert_eq!(
            documents
                .iter()
                .find(|d| d.document == live_id)
                .unwrap()
                .trashed_at,
            None
        );
    }

    /// A parent id the listing does not contain is not a reason to stamp or
    /// to fail: the row keeps its own state.
    #[test]
    fn a_missing_parent_reference_is_left_alone() {
        let orphan = listed(Some(DocumentId::new()), None);
        let orphan_id = orphan.document;
        let mut documents = vec![orphan];

        apply_inherited_trash(&mut documents);

        assert_eq!(documents[0].document, orphan_id);
        assert_eq!(documents[0].trashed_at, None);
    }

    /// A listing is a tree, but a corrupt mirror must not hang the projector.
    #[test]
    fn a_parent_cycle_terminates() {
        let mut a = listed(None, None);
        let mut b = listed(None, None);
        a.parent = Some(b.document);
        b.parent = Some(a.document);
        let mut documents = vec![a, b];

        apply_inherited_trash(&mut documents);

        assert!(documents.iter().all(|d| d.trashed_at.is_none()));
    }
}
