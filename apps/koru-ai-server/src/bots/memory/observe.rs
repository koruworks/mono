// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_core::schema::{DocumentReadValidationError, NodeReadEvidence, SCHEMA_VERSION};
use koru_core::{Heading, Kind, Parent, WorkspaceDoc};
use std::collections::HashSet;

pub(in crate::bots) fn observe(
    page: BotPage,
    version: DocumentVersionId,
    snapshot: &[u8],
) -> Result<MemoryIndexSource> {
    let doc = bounded_import(snapshot)?;
    observe_document(page, version, &doc)
}

pub(in crate::bots) fn observe_document(
    page: BotPage,
    version: DocumentVersionId,
    doc: &WorkspaceDoc,
) -> Result<MemoryIndexSource> {
    if doc.effective_schema_version() > SCHEMA_VERSION {
        return Err(MemoryCodecError::UnsupportedSchema);
    }
    observe_doc(page, version, doc)
}

fn evidence_error(error: DocumentReadValidationError) -> MemoryCodecError {
    match error {
        DocumentReadValidationError::NodeLimit { .. } => {
            MemoryCodecError::TooLarge(MemoryBudget::RawNodes)
        }
        DocumentReadValidationError::TextLimit { .. } => {
            MemoryCodecError::TooLarge(MemoryBudget::Text)
        }
        _ => MemoryCodecError::InvalidSnapshot,
    }
}

fn read(doc: &WorkspaceDoc, node: NodeId, remaining: &mut usize) -> Result<NodeReadEvidence> {
    let value = doc
        .read_node_evidence(node, *remaining)
        .map_err(evidence_error)?;
    *remaining -= value.text.as_ref().map_or(0, String::len);
    Ok(value)
}

fn visible(value: &NodeReadEvidence) -> bool {
    value.kind != Kind::Paragraph
        || value.has_marks
        || value.has_attribute_data
        || value
            .text
            .as_ref()
            .is_none_or(|text| !text.trim().is_empty())
}

fn children(doc: &WorkspaceDoc, node: NodeId) -> Result<Vec<NodeId>> {
    doc.children(Parent::Node(node))
        .map_err(|_| MemoryCodecError::InvalidSnapshot)
}

fn walk_descendants(
    doc: &WorkspaceDoc,
    parent: NodeId,
    parent_depth: usize,
    remaining: &mut usize,
    visited: &mut HashSet<NodeId>,
    visible_tree: bool,
) -> Result<bool> {
    let mut any_visible = false;
    let mut stack: Vec<_> = children(doc, parent)?
        .into_iter()
        .rev()
        .map(|node| (node, parent_depth + 1))
        .collect();
    while let Some((node, depth)) = stack.pop() {
        if depth > DEPTH_LIMIT {
            return Err(MemoryCodecError::TooLarge(MemoryBudget::Depth));
        }
        if !visited.insert(node) || visited.len() > NODE_LIMIT {
            return Err(MemoryCodecError::InvalidSnapshot);
        }
        let evidence = read(doc, node, remaining)?;
        if visible_tree {
            if evidence.kind == Kind::Trash {
                return Err(MemoryCodecError::InvalidSnapshot);
            }
            any_visible |= visible(&evidence);
        }
        stack.extend(
            children(doc, node)?
                .into_iter()
                .rev()
                .map(|child| (child, depth + 1)),
        );
    }
    Ok(any_visible)
}

fn observe_doc(
    page: BotPage,
    version: DocumentVersionId,
    doc: &WorkspaceDoc,
) -> Result<MemoryIndexSource> {
    let identity = doc
        .validate_node_identities(NODE_LIMIT)
        .map_err(evidence_error)?;
    if doc.id().map(|id| id.as_uuid()) != Some(page.workspace.as_uuid())
        || doc.own_document_id().map(|id| id.as_uuid()) != Some(page.document.as_uuid())
    {
        return Err(MemoryCodecError::InvalidSnapshot);
    }
    let mut remaining = TEXT_LIMIT;
    let mut visible_root = None;
    let mut trash_roots = Vec::new();
    let mut visited = HashSet::new();
    for root in identity.root_nodes {
        if !visited.insert(root) {
            return Err(MemoryCodecError::InvalidSnapshot);
        }
        let evidence = read(doc, root, &mut remaining)?;
        if evidence.text.is_none() {
            return Err(MemoryCodecError::InvalidSnapshot);
        }
        match evidence.kind {
            Kind::Trash => trash_roots.push(root),
            Kind::Document
                if visible_root.is_none()
                    && doc.document_id(root).as_uuid() == page.document.as_uuid() =>
            {
                visible_root = Some(root)
            }
            _ => return Err(MemoryCodecError::InvalidSnapshot),
        }
    }
    let root = visible_root.ok_or(MemoryCodecError::InvalidSnapshot)?;
    for trash in trash_roots {
        // Validate removed content, but never classify it into output or counts.
        walk_descendants(doc, trash, 0, &mut remaining, &mut visited, false)?;
    }
    let mut source = MemoryIndexSource {
        page,
        version,
        facts: Vec::new(),
        diagnostics: Vec::new(),
        invalid_count: 0,
        not_indexed_count: 0,
    };
    for top in children(doc, root)? {
        if !visited.insert(top) {
            return Err(MemoryCodecError::InvalidSnapshot);
        }
        let first = read(doc, top, &mut remaining)?;
        if first.kind == Kind::Trash {
            return Err(MemoryCodecError::InvalidSnapshot);
        }
        let fact = first.heading == Some(Heading::H2) && first.kind != Kind::Document;
        let mut subtree_visible = visible(&first);
        if fact {
            let summary = first.text.as_deref().map(str::trim).filter(|s| {
                !s.is_empty()
                    && s.len() <= SUMMARY_LIMIT
                    && s.chars()
                        .any(|c| !c.is_whitespace() && c != koru_core::ANCHOR)
            });
            if let Some(summary) = summary {
                source.facts.push(MemorySummary {
                    node: top,
                    summary: summary.to_owned(),
                });
            } else {
                source.invalid_count += 1;
                source.diagnostics.push(MemoryDiagnostic {
                    node: top,
                    code: MemoryDiagnosticCode::InvalidSummary,
                });
            }
        }
        subtree_visible |= walk_descendants(doc, top, 1, &mut remaining, &mut visited, true)?;
        if !fact && subtree_visible {
            source.not_indexed_count += 1;
            source.diagnostics.push(MemoryDiagnostic {
                node: top,
                code: MemoryDiagnosticCode::NotIndexed,
            });
        }
    }
    if visited.len() != identity.live_nodes {
        return Err(MemoryCodecError::InvalidSnapshot);
    }
    Ok(source)
}
