// SPDX-License-Identifier: MPL-2.0

//! Turning one projected document into matchable text.
//!
//! This is the half of the old app search index's `materialize.rs` that has a
//! future.
//! The other half — `partition_for_node`, `members`, `dependencies`,
//! `lifecycle_map` — answered *which partition owns this node* and *which must
//! I rewrite now it has moved*, and neither question exists any more: one
//! `LoroDoc` is one document, so a partition **is** a document and the store
//! names it. What is left is the part that decided what a person can search
//! for, and it now reads a [`DocumentProjection`] instead of a `WorkspaceDoc`.
//!
//! ## Two sources, one writer each
//!
//! **A title comes from the listing; body text comes from the node walk.** They
//! normally agree — the app pushes the document's row-0 text into the manifest —
//! so indexing both would double every title hit. The listing wins for two
//! reasons: it is the only one that can answer for a document **created but
//! never opened**, which has a title and no nodes at all; and a rename changes
//! it without changing a single node, so a title fragment built from the node
//! walk would keep the old name matchable until somebody typed into the
//! document again.
//!
//! So the title fragment is written by [`title_fragment`], on the listing path,
//! and this function writes only what the walk knows. **Each row has exactly one
//! writer**, which is what keeps them from fighting over the same key.
//!
//! When the two disagree, the root node gets its own fragment as well. Two
//! different strings somebody might search for, both matchable; when they agree,
//! one row.
//!
//! ## A tag is indexed by its name, not its id
//!
//! The projection hands a `koru.tag` attribute a `TagId`, because that is what
//! the node stores and what makes a rename one record rather than a rewrite of
//! every document ([ADR-176]). An id is not a word, so it is resolved here
//! through the [`TagTable`] the indexer keeps beside it. See [`crate::tags`]
//! and ADR-182.
//!
//! ## No `title_case(kind)` fallback
//!
//! The old builder gave an empty paragraph the text `"Paragraph"` and an empty
//! table `"Table"` — which is why its preview query carried a
//! `NOT ((kind='paragraph' AND text='Paragraph') OR …)` filter to hide them
//! again, and why searching for *table* matched every empty table in the
//! workspace. A node with no text and no name produces **no fragment**, and the
//! filter went with it.

use koru_projection::model::{DocumentProjection, ProjectedAttr, ProjectedNode};

use crate::model::{AttrRow, Fragment, field};
use crate::tags::{TAG_ROLE, TagTable};

/// Everything the **node walk** contributes. The title is not here; see
/// [`title_fragment`].
///
/// `title` is still needed: it is what the root node's own text is compared
/// against, so that the two are one row when they agree and two when they do
/// not.
///
/// `tags` is this workspace's tag table, or `None` when the indexer has none to
/// hand — see [`attr_text`] for what each answer costs.
pub fn fragments(
    projection: &DocumentProjection,
    title: &str,
    tags: Option<&TagTable>,
) -> Vec<Fragment> {
    let workspace = projection.workspace.to_storage();
    let document = projection.document.to_storage();
    let mut out = Vec::with_capacity(projection.nodes.len());

    let root = projection.nodes.first().filter(|n| n.kind == "document");
    for (seq, node) in projection.nodes.iter().enumerate() {
        if node.in_trash {
            continue;
        }
        let seq = seq as i64 + 1;
        let is_root = root.is_some_and(|r| r.id == node.id);
        // The root's own text is already carried by the listing — unless the
        // two have drifted apart, in which case both are things somebody typed.
        let skip_primary = is_root && node.text.trim() == title.trim();

        if !skip_primary && let Some(text) = primary_text(node) {
            out.push(Fragment {
                key: format!("node:{document}:{}:primary", node.id),
                workspace: workspace.clone(),
                document: Some(document.clone()),
                node: Some(node.id.clone()),
                seq,
                kind: node.kind.clone(),
                field: primary_field(node),
                text: text.to_owned(),
            });
        }

        if let Some(level) = node.level_name.as_deref()
            && !level.trim().is_empty()
            && Some(level) != primary_text(node)
        {
            out.push(Fragment {
                key: format!("node:{document}:{}:level", node.id),
                workspace: workspace.clone(),
                document: Some(document.clone()),
                node: Some(node.id.clone()),
                seq,
                kind: node.kind.clone(),
                field: field::LEVEL_NAME.into(),
                text: level.to_owned(),
            });
        }

        for attr in &node.attrs {
            // **A stable node id is routing data, not something a person read.**
            // A labelled reference stays searchable by the text they saw.
            if attr.kind == "ref" && attr.label.is_none() {
                continue;
            }
            let Some(text) = attr_text(attr, tags) else {
                continue;
            };
            if text.trim().is_empty() {
                continue;
            }
            out.push(Fragment {
                key: format!("node:{document}:{}:attr:{}", node.id, attr.id),
                workspace: workspace.clone(),
                document: Some(document.clone()),
                node: Some(node.id.clone()),
                seq,
                kind: node.kind.clone(),
                field: field::attr(&attr.role),
                text: text.to_owned(),
            });
        }
    }
    out
}

/// Every attribute of every node, **in the type it was declared with**.
///
/// The typed sibling of the walk above, and a second writer over the same
/// nodes rather than a second pass over the store: the projection is already
/// in hand, and one insert per attribute is the whole cost.
///
/// **Every kind is carried, and nothing is converted.** A `date` keeps its ISO
/// text, a `datetime_zoned` becomes a number beside the zone it was meant in,
/// and the two never meet in one column — a filter names the kinds it applies
/// to and a row of another kind simply does not match. That is ADR-189's rule
/// about the three temporal types, expressed as a table shape rather than as a
/// convention somebody has to remember.
///
/// **A tag keeps its `TagId` here**, unlike its fragment, which is resolved to
/// a name. A filter on `koru.tag` goes through the fragment and its name
/// (ADR-190); this row exists so `has a tag at all` and `has this exact one`
/// are answerable without one, and so the walk has no per-role exception.
pub fn attr_values(
    workspace: &str,
    document: &str,
    projection: &DocumentProjection,
) -> Vec<AttrRow> {
    let mut out = Vec::new();
    for node in &projection.nodes {
        if node.in_trash {
            continue;
        }
        for attr in &node.attrs {
            if attr.value.trim().is_empty() {
                continue;
            }
            out.push(AttrRow {
                workspace: workspace.to_owned(),
                document: document.to_owned(),
                node: node.id.clone(),
                role: attr.role.clone(),
                kind: attr.kind.clone(),
                value: attr.value.clone(),
                num: comparable_number(attr),
                zone: attr.zone.clone(),
            });
        }
    }
    out
}

/// The comparable number for the two kinds that have one.
///
/// `number` is one, and a `datetime_zoned` is one because an instant *is* a
/// number — the projection stores it as decimal epoch milliseconds. Everything
/// else answers `None`, including `date` and `datetime_floating`: their ISO
/// text already sorts correctly within its own kind, and giving them a number
/// would invite the comparison across kinds that the type split exists to
/// prevent.
///
/// A value that will not parse answers `None` rather than zero. Zero is a
/// **time** — 1970 — and a malformed row that quietly sorts as the epoch is
/// worse than one that no range matches.
fn comparable_number(attr: &ProjectedAttr) -> Option<f64> {
    match attr.kind.as_str() {
        "number" | "datetime_zoned" => attr.value.trim().parse::<f64>().ok(),
        _ => None,
    }
}

/// The document's name, as the listing has it.
///
/// Written on the listing path, so a rename replaces it without re-indexing a
/// document — and so a document that has never been opened has one at all.
/// `node` is the document's root node when the index knows of one, so that
/// opening a title hit lands where a person expects.
pub fn title_fragment(workspace: &str, document: &str, title: &str) -> Option<Fragment> {
    (!title.trim().is_empty()).then(|| Fragment {
        key: format!("doc:{document}:title"),
        workspace: workspace.to_owned(),
        document: Some(document.to_owned()),
        node: None,
        // Ahead of every node, so a preview reading document order starts after
        // it rather than repeating it.
        seq: 0,
        kind: "document".to_owned(),
        field: field::TITLE.to_owned(),
        text: title.to_owned(),
    })
}

/// The one fragment a workspace contributes: its own name.
///
/// It belongs to no document, which is also what keeps
/// `DELETE FROM fragments WHERE document_id = ?` from ever touching it.
pub fn workspace_fragment(workspace: &str, name: &str) -> Option<Fragment> {
    (!name.trim().is_empty()).then(|| Fragment {
        key: format!("workspace:{workspace}"),
        workspace: workspace.to_owned(),
        document: None,
        node: None,
        seq: 0,
        kind: field::WORKSPACE.into(),
        field: field::WORKSPACE.into(),
        text: name.to_owned(),
    })
}

/// The matchable text behind one attribute, or `None` for one that is only an
/// id.
///
/// **A tag is the reference case again.** A `koru.tag` attribute's value is a
/// `TagId`; what a person read was its name, and the name lives in the
/// workspace's system document rather than beside the id — deliberately, so a
/// rename rewrites one record instead of every document
/// ([ADR-199](../../../docs/decisions/ADR-176-200.md#adr-199-attributes--attributes-are-typed-values-on-the-node-under-registered-keys-tags-are-entities-and-the-workspace-keeps-a-hidden-system-document)). Three answers,
/// and the middle one is the point:
///
/// - **A table that knows the id** — its name, which is what makes `amazing`
///   find a block tagged `#amazing`.
/// - **A table that does not** — no fragment. The tag was merged away and its
///   tombstone forgotten, or the record is simply gone; either way the id is
///   routing data and a person never read it. Never a blank row, and never a
///   uuid pretending to be a word.
/// - **No table at all** — the id, exactly as before this existed. A host that
///   wired no [`TagNames`](crate::tags::TagNames) source, or a workspace whose
///   system document has not been read yet, must not make every tag vanish from
///   the index while it waits.
fn attr_text<'a>(attr: &'a ProjectedAttr, tags: Option<&'a TagTable>) -> Option<&'a str> {
    if attr.role == TAG_ROLE
        && let Some(table) = tags
    {
        return table.name(&attr.value);
    }
    Some(attr.label.as_deref().unwrap_or(&attr.value))
}

/// What a node is findable by: its text, or the name given to content that has
/// none. Never a stand-in for the kind.
fn primary_text(node: &ProjectedNode) -> Option<&str> {
    if !node.text.trim().is_empty() {
        return Some(&node.text);
    }
    node.name.as_deref().filter(|name| !name.trim().is_empty())
}

fn primary_field(node: &ProjectedNode) -> String {
    if let Some(level) = node.heading {
        return field::heading(level);
    }
    match node.kind.as_str() {
        "document" => field::TITLE.into(),
        "task" => field::TASK.into(),
        "quote" => field::QUOTE.into(),
        "code" => field::CODE.into(),
        "table" => field::TABLE.into(),
        "contact" => field::CONTACT.into(),
        "image" | "canvas" => field::NAME.into(),
        "listblock" => field::LIST_NAME.into(),
        _ => field::BODY.into(),
    }
}

#[cfg(test)]
mod tests;
