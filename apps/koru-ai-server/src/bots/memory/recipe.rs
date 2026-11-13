// SPDX-License-Identifier: MPL-2.0
//! Pure bounded edit preparation. The imported history never leaves this lane.
use super::{history::VerifiedMemory, *};
use koru_core::schema::NodeContentReadError;
use koru_core::{Content, Kind, Parent};
use koru_mcp_server::native::memory::{
    MemoryEditRecipe, MemoryExistingChild, MemoryRecipeChange, MemoryRecipeOperation,
    validate_memory_text,
};
use std::collections::HashSet;

fn invalid() -> MemoryCodecError {
    MemoryCodecError::InvalidCapture
}
fn strict_error(error: NodeContentReadError) -> MemoryCodecError {
    match error {
        NodeContentReadError::TooLarge => MemoryCodecError::DetailTooLarge,
        NodeContentReadError::UnsupportedDetail => MemoryCodecError::UnsupportedDetail,
        NodeContentReadError::InvalidContent => MemoryCodecError::InvalidSnapshot,
    }
}

pub(in crate::bots) fn prepare_recipe(
    memory: &VerifiedMemory,
    operation: &MemoryRecipeOperation,
) -> Result<MemoryEditRecipe> {
    match operation {
        MemoryRecipeOperation::Remember { summary, detail }
        | MemoryRecipeOperation::Correct {
            summary, detail, ..
        } => {
            validate_memory_text(summary, detail)
                .map_err(|_| MemoryCodecError::InvalidWriteInput)?;
        }
        MemoryRecipeOperation::Forget { .. } => {}
    }
    let doc = &memory.doc;
    let roots = doc.children(Parent::Root).map_err(|_| invalid())?;
    let mut documents = roots
        .into_iter()
        .filter(|id| doc.kind_of(*id) == Some(Kind::Document));
    let root = documents.next().ok_or_else(invalid)?;
    if documents.next().is_some() {
        return Err(invalid());
    }
    let change = match operation {
        MemoryRecipeOperation::Remember { summary, detail } => MemoryRecipeChange::Remember {
            after: doc
                .children(Parent::Node(root))
                .map_err(|_| invalid())?
                .last()
                .map(|id| id.as_uuid()),
            summary: summary.clone(),
            detail: detail.clone(),
        },
        MemoryRecipeOperation::Correct { fact, .. } | MemoryRecipeOperation::Forget { fact } => {
            let fact_id = NodeId::from_uuid(*fact);
            if !memory
                .source
                .facts
                .iter()
                .any(|entry| entry.node == fact_id)
                || doc.parent(fact_id).map_err(|_| invalid())? != Parent::Node(root)
            {
                return Err(invalid());
            }
            // The index's display summary is trimmed. Expected is the original
            // exact text, with a remaining budget applied before materializing.
            let is_correct = matches!(operation, MemoryRecipeOperation::Correct { .. });
            let limit = if is_correct { 32 * 1024 } else { 65_536 };
            let evidence = doc
                .read_node_evidence(fact_id, limit)
                .map_err(|_| MemoryCodecError::DetailTooLarge)?;
            let expected = evidence.text.ok_or_else(invalid)?;
            if let MemoryRecipeOperation::Correct {
                summary, detail, ..
            } = operation
            {
                let direct = doc.children(Parent::Node(fact_id)).map_err(|_| invalid())?;
                if direct.len() > 128 {
                    return Err(MemoryCodecError::DetailTooLarge);
                }
                let mut remaining = 32 * 1024;
                let mut stack = vec![(fact_id, 0)];
                let mut visited = HashSet::new();
                let direct_ids: HashSet<_> = direct.iter().copied().collect();
                let mut text = std::collections::HashMap::new();
                while let Some((node, depth)) = stack.pop() {
                    if depth > DEPTH_LIMIT || !visited.insert(node) || visited.len() > NODE_LIMIT {
                        return Err(invalid());
                    }
                    let read = doc
                        .read_node_content_strict(node, remaining)
                        .map_err(strict_error)?;
                    let Content::Text { text: raw, .. } = read.content else {
                        return Err(MemoryCodecError::UnsupportedDetail);
                    };
                    remaining = remaining
                        .checked_sub(raw.len())
                        .ok_or(MemoryCodecError::DetailTooLarge)?;
                    if direct_ids.contains(&node) {
                        text.insert(node, raw);
                    }
                    stack.extend(
                        doc.children(Parent::Node(node))
                            .map_err(|_| invalid())?
                            .into_iter()
                            .rev()
                            .map(|child| (child, depth + 1)),
                    );
                }
                let children = direct
                    .into_iter()
                    .map(|node| {
                        Ok(MemoryExistingChild {
                            node: node.as_uuid(),
                            expected: text.remove(&node).ok_or_else(invalid)?,
                        })
                    })
                    .collect::<Result<Vec<_>>>()?;
                MemoryRecipeChange::Correct {
                    fact: *fact,
                    expected,
                    children,
                    summary: summary.clone(),
                    detail: detail.clone(),
                }
            } else {
                MemoryRecipeChange::Forget {
                    fact: *fact,
                    expected,
                }
            }
        }
    };
    let recipe = MemoryEditRecipe {
        workspace: koru_store_core::WorkspaceId::from_uuid(memory.source.page.workspace.as_uuid()),
        document: koru_store_core::DocumentId::from_uuid(memory.source.page.document.as_uuid()),
        history: memory.source.version.as_uuid(),
        root: root.as_uuid(),
        change,
    };
    recipe
        .validate()
        .map_err(|_| MemoryCodecError::DetailTooLarge)?;
    Ok(recipe)
}

#[cfg(test)]
mod tests;
