// SPDX-License-Identifier: MPL-2.0

//! What a workspace holds, as a tree: the **Explore** projection.
//!
//! This is the question the explore pane has always asked, given a name and a
//! shape that outlive the way it is answered today. Right now a workspace is one
//! `LoroDoc` and a document is a `Kind::Document` node inside it, so the answer
//! is computed from the tree. Under the store a workspace *is* a manifest and
//! the same question is one call
//! ([ADR-80](../../../docs/decisions/ADR-076-100.md#adr-80-manifest--a-workspace-is-a-manifest-document-plus-opaque-documents)).
//! [`ExploreEntry`] is field-for-field the store's `DocumentEntry`, so that swap
//! is a change of *source* rather than a redesign.
//!
//! ## Why this is derived rather than maintained
//!
//! The obvious alternative is a second structure kept in step: a manifest beside
//! the tree, updated on create, rename, move and trash. It was rejected because
//! **rename has no choke point**. A document's title is its node's text, that
//! node is row 0 of its own body, and every ordinary editing path — typing,
//! backspace, paste, cut, autoformat, the pill surgery — reaches it through
//! `set_text` / `insert_text` / `delete_text`. Roughly twenty call sites, each
//! of which would have to remember. The first one that forgot would leave a
//! title in Explore that no longer matched the document, and nothing would
//! fail — it would simply be wrong.
//!
//! Deriving cannot drift, because there is nothing to drift *from*. That is the
//! same conclusion the store reached for its own SQL mirror, where the test is
//! named `reindexing_replaces_rather_than_accumulates`: the projection is
//! replaced wholesale, never patched.
//!
//! The cost is that each call walks the tree. It is one `kind_of` per node —
//! one key off the meta map, not a `read`, which would materialise every
//! paragraph's text — and a `read` only for the documents that earn a row. That
//! is the same budget `ExploreView::rebuild` already spends.

use crate::NodeId;
use crate::ids::DocumentId;
use crate::node::Kind;
use crate::schema::{Parent, Result, WorkspaceDoc};

/// One thing a workspace holds.
///
/// Field-for-field `koru_store_core::model::DocumentEntry`, deliberately:
/// documents today, and whatever else earns an entry later. `kind` is an
/// app-defined token the store never interprets, which is what makes a new sort
/// of entry a new token and a renderer rather than a schema change.
///
/// **There is no `NodeId` here**, though the caller usually wants one. Adding it
/// would put a field in every UI site that has to disappear when a document
/// stops being a node; [`WorkspaceDoc::node_of`] is the bridge instead, so the
/// conversion happens at the one place a document is actually opened.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ExploreEntry {
    pub id: DocumentId,
    /// The nearest *document* ancestor, not the tree parent.
    ///
    /// A document may sit under a task, and the explore pane draws it under the
    /// nearest document above it rather than inventing a row for the task. So a
    /// document three tree-levels down whose ancestors are all paragraphs is a
    /// child of whatever document contains them.
    pub parent: Option<DocumentId>,
    /// Position among the entries sharing this `parent`, from 0.
    ///
    /// **Not the tree index.** A document's children are mostly its body — the
    /// paragraphs — and those earn no entry, so the two coordinate systems
    /// disagree the moment a document has any content at all.
    /// [`WorkspaceDoc::move_entry`] translates.
    pub ordinal: u32,
    pub title: String,
    pub kind: String,
    /// MIME type for file entries, when the manifest knows it. Optional for
    /// snapshots created before file metadata was mirrored into listings.
    pub mime: Option<String>,
    /// The one note attached to a file, if it has one. `None` for anything
    /// that is not a file, and for a file nobody has annotated yet.
    pub note: Option<DocumentId>,
    /// When it was put in the trash, and by whom. `None` for a document that is
    /// not in it — which is every document in a listing that filtered them out.
    ///
    /// **The label is not an identity.** Nothing resolves it or compares it; it
    /// is what the Trash screen prints under *Deleted by*, so that saying who
    /// does not mean looking up a profile that may live on another machine.
    pub trashed_at: Option<i64>,
    pub trashed_by: Option<String>,
}

/// How many documents a gesture on `roots` actually moves.
///
/// **Roots plus every document beneath them, at any depth, counted once.**
/// Trashing or moving a section takes its whole subtree, so a count of the
/// selection alone under-reports it and a count that also counted a selected
/// *child* of a selected parent reports the same subtree twice. Both were real:
/// the drop label said "Move 4 items" while binning one, and said three where
/// the answer was more.
///
/// One function because two answers to "how many" is how a confirm comes to
/// quote a different number than the label that offered it.
pub fn documents_moved(roots: &[DocumentId], entries: &[ExploreEntry]) -> usize {
    let chosen: std::collections::HashSet<DocumentId> = roots.iter().copied().collect();
    let parent_of: std::collections::HashMap<DocumentId, Option<DocumentId>> =
        entries.iter().map(|e| (e.id, e.parent)).collect();

    // A selected document whose ancestor is also selected is not a root: it is
    // already travelling inside that one.
    let is_root = |id: &DocumentId| {
        let mut at = parent_of.get(id).copied().flatten();
        // Bounded by the list, so a parent chain that somehow cycles cannot spin.
        for _ in 0..entries.len().saturating_add(1) {
            match at {
                Some(parent) if chosen.contains(&parent) => return false,
                Some(parent) => at = parent_of.get(&parent).copied().flatten(),
                None => return true,
            }
        }
        true
    };

    let mut moving: std::collections::HashSet<DocumentId> = std::collections::HashSet::new();
    let mut frontier: Vec<DocumentId> = roots.iter().copied().filter(is_root).collect();
    for id in &frontier {
        moving.insert(*id);
    }
    while let Some(parent) = frontier.pop() {
        for child in entries.iter().filter(|e| e.parent == Some(parent)) {
            if moving.insert(child.id) {
                frontier.push(child.id);
            }
        }
    }
    moving.len()
}

/// The token `ExploreEntry::kind` carries for a document.
///
/// A `&'static str` rather than an enum, because the store stores exactly this
/// string and never looks inside it.
pub const KIND_DOCUMENT: &str = "page";
/// The token `ExploreEntry::kind` carries for an opaque workspace File.
pub const KIND_FILE: &str = "file";
/// The token `ExploreEntry::kind` carries for a **note** — a document attached
/// to a file. A note is an ordinary document in every way that matters to the
/// store: it syncs, persists and stays searchable. [`listed`] keeps it in the
/// projection; presentation layers hide it from the listings a person browses.
pub const KIND_NOTE: &str = "note";
/// The token `ExploreEntry::kind` carries for a **canvas** — an ordinary
/// node-tree document, rooted at a `Kind::Document` node whose text is its
/// title, that is drawn spatially rather than as a page. It syncs, searches,
/// trashes and moves like a page; only the view that opens it differs.
/// [`listed`] keeps it listed; Explore gives canvases a section of their own.
///
/// **A stored name.** It is the value in every workspace manifest's open
/// `kind` string on every device and never changes.
pub const KIND_CANVAS: &str = "canvas";
/// The token for the per-workspace **system document** — custom attribute
/// definitions and the tag table ([`crate::system::SystemDoc`], ADR-176).
///
/// App-defined and app-hidden: the store carries the token like any other,
/// and every listing a person sees filters it through [`listed`] — a document
/// nobody opens, names or drags earns no row.
pub const KIND_SYSTEM: &str = "koru-system";
/// The token for a workspace's **tasks document** — the home a task created in
/// the Tasks view is written into (SPEC §8, `koru_workspace::tasks`).
///
/// App-defined and app-hidden, exactly as [`KIND_SYSTEM`] is: the store carries
/// the token like any other and the listing a person sees filters it out
/// through [`listed`] ([ADR-285](../../../docs/decisions/ADR-276-300.md#adr-285-taskmodel--a-task-is-a-marked-node-and-the-marker-or-a-due-date-is-the-whole-derivation)'s 2026-09-01 amendment).
/// The Tasks view is where those tasks are seen, and it reads them through the
/// query — by the marker on the node, never by which document holds it — so
/// the document itself is an implementation detail with no reason to earn a
/// row somebody can open, rename or drag into another document.
///
/// **It lives here rather than beside the code that writes it** because this is
/// where the hiding is decided. `koru_workspace::tasks::KIND_TASK_LIST`
/// re-exports it, so the writer still names it from one place.
pub const KIND_TASK_LIST: &str = "task-list";
/// The token for a **delegated run** — one Loro document per run, holding
/// its header and timeline (`docs/architecture/DATAMODEL.md`, ADR-337). A run
/// is reached through the pill that references it and the
/// run views, never through a library row: nobody opens, renames or drags a
/// run as a page, so it earns none (ADR-337).
///
/// **A stored name.** It is the value in every workspace manifest's open
/// `kind` string on every device and never changes.
/// `koru_delegate_document::RUN_KIND` is the same literal on the side that
/// writes it; the two are pinned equal by a test in each crate rather than
/// shared, because the store side cannot link this crate (ADR-80).
pub const KIND_RUN: &str = "koru-run";
/// The token for a **schedule** (M2), the same shape and the same hiding
/// as [`KIND_RUN`]; reserved here so the rule exists before the first
/// schedule does. `koru_delegate_document::SCHEDULE_KIND` is its twin.
pub const KIND_SCHEDULE: &str = "koru-schedule";
/// Socials documents have their own surface and no ordinary node tree.
pub const KIND_SOCIAL_POST: &str = "koru-social-post";
pub const KIND_SOCIAL_CHANNEL: &str = "koru-social-channel";
/// The token for a **reading document** — the one hidden child of a file, or
/// of a page that holds pictures, that holds the words read out of it (OCR
/// TECHNICAL D9). Its id is derived from its parent's
/// ([`crate::ids::reading_document_id`]), so there is no link field anywhere.
///
/// App-defined and app-hidden: nobody opens, names or drags one, so
/// [`listed`] gives it no row. It is also never walked and never versioned
/// (D14): `koru_projection::model::NOT_PROJECTED` and the Store's unversioned
/// kinds both name it.
///
/// **A stored name.** It is the value in every workspace manifest's open
/// `kind` string on every device and never changes. `koru-store-core` spells
/// the same literal for `Store::ensure_reading`, pinned equal by a test in
/// each crate, because the store cannot link this crate (ADR-80).
pub const KIND_READING: &str = "koru-reading";

/// The kinds a person browses: [`listed`]'s whole answer.
pub const LISTED_KINDS: [&str; 4] = [KIND_DOCUMENT, KIND_FILE, KIND_NOTE, KIND_CANVAS];

/// Whether an entry of this kind belongs in a listing a person sees.
///
/// One predicate rather than a check per surface, for the same reason the
/// trash is one exclusion in `children`: a check at each surface is a check
/// somebody forgets to add to the eighth.
///
/// **An allowlist** ([`LISTED_KINDS`]): a page, a file, a note, a canvas, and
/// nothing else. It was a list of what to hide until the reading document
/// (OCR TECHNICAL §12), and an unknown token was listed then, so that a newer
/// build's documents were not silently invisible on an older one. The cost of
/// that rule was every internal kind ever added appearing on older builds as
/// a row nobody can open — the system document, the tasks document, runs and
/// now a reading document per file. This build is the last one that can show
/// a future internal kind that way: a kind it does not know is not listed. A
/// new kind a person browses is added here, in the change that makes it.
///
/// A note is listed *here* and hidden by the presentation layers (the desktop
/// Explore, the phone's inventory), which reach it through its file
/// (ADR-277-filenote): this predicate is about what a listing may hold, not
/// about which rows each surface draws.
///
/// **Hiding is a listing rule and nothing more.** Every hidden kind is an
/// ordinary document everywhere else: it is registered in the manifest,
/// synced and trashed like any other. The system and tasks documents are
/// also projected and *indexed*, so a task's text is found by search like
/// any other line — the seam a person is spared is the row in Explore, not
/// the content. A run, a schedule, a socials document and a reading document
/// are the exception to the projection half, decided one layer up
/// (`koru_projection::model::NOT_PROJECTED`): they have no node tree to walk.
/// Anything that needs to find one asks the manifest for its kind
/// (`Workspace::task_list_documents`, `ensure_system`), which is a different
/// question from what a library column draws.
pub fn listed(kind: &str) -> bool {
    LISTED_KINDS.contains(&kind)
}

impl WorkspaceDoc {
    /// What this workspace holds, in the order Explore draws it.
    ///
    /// Pre-order depth-first: a document, then its children, then its next
    /// sibling. The same order `Manifest::entries` returns, so a test written
    /// against one reads correctly against the other.
    ///
    /// The trash is excluded, because `children(Parent::Root)` excludes the
    /// trash root and this walks through `children` — the one exclusion that
    /// keeps the bin out of every walk in the tree
    /// ([ADR-69](../../../docs/decisions/ADR-051-075.md#adr-69--trashing-a-document-is-a-move-into-a-hidden-library-node-and-nothing-ever-purges-it)) doing its job here for
    /// free.
    pub fn explore(&self) -> Result<Vec<ExploreEntry>> {
        Ok(self
            .explore_bridged()?
            .into_iter()
            .map(|(entry, _)| entry)
            .collect())
    }

    /// [`explore`](Self::explore), with the node behind each entry.
    ///
    /// **The bridge, and it is temporary** — the counterpart of
    /// [`node_of`](Self::node_of) for a caller that wants every entry rather
    /// than one. A document is a node today, so anything that opens, renames or
    /// drags one needs its `NodeId`; when a document becomes its own `LoroDoc`
    /// ([ADR-91](../../../docs/decisions/ADR-076-100.md#adr-91-perdocument--a-document-keeps-a-root-node-for-its-title-and-the-manifest-mirrors-it)) there is no
    /// node to hand back and this function goes, leaving `explore` — which is
    /// why the `NodeId` is a second tuple element rather than a field on
    /// [`ExploreEntry`]. The entry stays field-for-field the store's
    /// `DocumentEntry`, so 3c deletes a function rather than editing a type.
    ///
    /// It exists because `node_of` is a walk: calling it once per row would make
    /// building the explore column quadratic, on the one rebuild AGENTS.md
    /// singles out as having to stay cheap.
    pub fn explore_bridged(&self) -> Result<Vec<(ExploreEntry, NodeId)>> {
        let mut out = Vec::new();
        self.explore_under(Parent::Root, None, &mut out)?;
        Ok(out)
    }

    /// Walk `parent`'s children, emitting an entry per document and recursing
    /// *through* everything else at the same level.
    ///
    /// Recursing through is what makes a document inside a task visible: the
    /// task earns no row, so the document below it belongs to the nearest
    /// document above — which is what `home` already meant.
    fn explore_under(
        &self,
        parent: Parent,
        under: Option<DocumentId>,
        out: &mut Vec<(ExploreEntry, NodeId)>,
    ) -> Result<()> {
        let mut ordinal = 0u32;
        for node in self.children(parent)? {
            if self.kind_of(node) == Some(Kind::Document) {
                let id = self.document_id(node);
                out.push((
                    ExploreEntry {
                        id,
                        parent: under,
                        ordinal,
                        title: self.read(node)?.content.as_text().unwrap_or("").to_owned(),
                        kind: KIND_DOCUMENT.to_owned(),
                        mime: None,
                        // The tree projection has no manifest to carry a
                        // note link; store-backed listings fill this in.
                        note: None,
                        // The tree projection walks `Parent::Root`, which
                        // already excludes the hidden trash root — so anything
                        // it yields is by definition not binned.
                        trashed_at: None,
                        trashed_by: None,
                    },
                    node,
                ));
                ordinal += 1;
                self.explore_under(Parent::Node(node), Some(id), out)?;
            } else {
                // Not a document, so it earns no entry and does not advance the
                // ordinal — but a document may still be somewhere below it.
                self.explore_under(Parent::Node(node), under, out)?;
            }
        }
        Ok(())
    }

    /// The node behind an entry, or `None` if nothing in this workspace has that
    /// id.
    ///
    /// **The bridge, and it is temporary.** A document is a node today, so
    /// opening one means finding its `NodeId`; when a document becomes its own
    /// `LoroDoc` ([ADR-91](../../../docs/decisions/ADR-076-100.md#adr-91-perdocument--a-document-keeps-a-root-node-for-its-title-and-the-manifest-mirrors-it)) the
    /// same call site loads a document by id instead, and this goes. Keeping it
    /// to one function rather than a field on every entry is what makes that a
    /// small edit.
    pub fn node_of(&self, id: DocumentId) -> Option<NodeId> {
        self.explore_bridged()
            .ok()?
            .into_iter()
            .find_map(|(entry, node)| (entry.id == id).then_some(node))
    }

    /// Move a document among the entries under `parent`.
    ///
    /// `index` is in **entry** coordinates — the position Explore shows — which
    /// is not the tree index whenever the destination has a body. Translated
    /// here rather than at the call sites, for the same reason `move_node`
    /// translates the trash root away: the one list a caller can see should be
    /// the one list it addresses.
    ///
    /// A move that would put a document inside itself is refused by Loro with
    /// `CyclicMoveError`, and the tree is left exactly as it was — verified
    /// rather than assumed, in `a_document_cannot_be_moved_inside_itself`. So
    /// there is no guard here; the store's `move_document` has one only to
    /// answer with its own error code.
    pub fn move_entry(
        &mut self,
        id: DocumentId,
        parent: Option<DocumentId>,
        index: usize,
    ) -> Result<()> {
        let target = self
            .node_of(id)
            .ok_or(crate::StoreError::NoSuchDocument(id))?;
        let into = match parent {
            Some(p) => Parent::Node(
                self.node_of(p)
                    .ok_or(crate::StoreError::NoSuchDocument(p))?,
            ),
            None => Parent::Root,
        };
        let raw = self.raw_index_for_entry(into, index, target);
        self.move_node_raw(target, into, raw)
    }

    /// Turn a position among `into`'s document children into the raw sibling
    /// index Loro wants.
    ///
    /// Two corrections, and each was a failing test before it was a comment:
    ///
    /// **The index is read after the node is lifted out.** `mov_to` removes the
    /// node and then inserts it at the index given, so the list being addressed
    /// is the one *without* it. Counting in the list that still contains it
    /// moves a document one place further than asked, and pushes an append one
    /// past the end — `IndexOutOfBound { len: 2, index: 3 }`, which is how this
    /// was found.
    ///
    /// **Raw, not visible.** The visible list omits the trash root, so a raw
    /// index and a visible one differ at the top of the tree. This walks the raw
    /// list directly rather than handing a visible index to [`move_node`], which
    /// would translate a second time on top of the translation done here.
    ///
    /// Landing *past* the last entry means the end of the list, so a document
    /// dropped below everything lands after the body rather than between two
    /// paragraphs of it.
    ///
    /// [`move_node`]: WorkspaceDoc::move_node
    fn raw_index_for_entry(&self, into: Parent, entry_index: usize, moving: NodeId) -> usize {
        let Ok(raw) = self.children_raw(into) else {
            return entry_index;
        };
        let remaining: Vec<NodeId> = raw.into_iter().filter(|&n| n != moving).collect();
        let documents: Vec<NodeId> = remaining
            .iter()
            .copied()
            .filter(|&n| self.kind_of(n) == Some(Kind::Document))
            .collect();
        match documents.get(entry_index) {
            Some(at) => remaining
                .iter()
                .position(|n| n == at)
                .unwrap_or(remaining.len()),
            None => remaining.len(),
        }
    }
}

#[cfg(test)]
mod tests;
