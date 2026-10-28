// SPDX-License-Identifier: MPL-2.0

//! Android's bounded body-edit command. The Store worker serializes validation,
//! application and flush with replication; only durable changes are announced.
use super::*;
use koru_contract::mobile_proto::DocumentTextEdit;
use koru_core::{Kind, Node, NodeId, WorkspaceDoc, schema::Parent};

impl MobileHost {
    pub(super) fn edit_document_text(
        &self,
        id: &str,
        edit: DocumentTextEdit,
    ) -> Result<(), String> {
        let document = DocumentId::parse(id).ok_or("invalid_document_id")?;
        let block = NodeId::parse(&edit.block_id).ok_or("invalid_block_id")?;
        if edit.before.len() > 1024 * 1024 || edit.after.len() > 1024 * 1024 {
            return Err("text_edit_too_large".into());
        }
        let at = Timestamp::from_millis(now_millis());
        let peer = self.document_edit_peer;
        self.with_store(move |store| {
            let workspace = store
                .locate(document)
                .map_err(store_error)?
                .ok_or("document_not_found")?;
            let entry = store
                .documents(workspace)
                .map_err(store_error)?
                .into_iter()
                .find(|entry| entry.id == document && entry.trashed_at.is_none())
                .ok_or("document_not_found")?;
            if entry.kind == "file" {
                return Err("document_is_file".into());
            }
            let fork = store
                .fork_document(workspace, document)
                .map_err(store_error)?;
            fork.set_peer_id(peer).map_err(|_| "document_peer_failed")?;
            let mut model = WorkspaceDoc::adopt(fork).map_err(|_| "invalid_document_snapshot")?;
            let before = model.version();
            apply_text_edit(&mut model, block, &edit)?;
            let update = model
                .export_updates_since(&before)
                .map_err(|_| "document_export_failed")?;
            store
                .apply_update(
                    workspace,
                    document,
                    update,
                    at,
                    koru_store_core::ports::UpdateOrigin::Store,
                )
                .map_err(store_error)?;
            store.flush(workspace, document, at).map_err(store_error)?;
            Ok(())
        })?;
        self.network.notify_local_change();
        Ok(())
    }
}

fn store_error(error: koru_store_core::Error) -> String {
    error.code().as_str().to_owned()
}

fn apply_text_edit(
    model: &mut WorkspaceDoc,
    block: NodeId,
    edit: &DocumentTextEdit,
) -> Result<(), String> {
    let split = edit
        .split
        .as_ref()
        .map(|split| {
            let tail = NodeId::parse(&split.block_id).ok_or("invalid_block_id")?;
            let at = split.at_utf8 as usize;
            if tail == block || at > edit.after.len() || !edit.after.is_char_boundary(at) {
                return Err("invalid_block_split");
            }
            Ok((tail, at))
        })
        .transpose()?;
    if let Some((tail, at)) = split {
        if model.kind_of(tail).is_some() {
            if model
                .read(tail)
                .ok()
                .and_then(|n| n.content.as_text().map(str::to_owned))
                .as_deref()
                != Some(&edit.after[at..])
            {
                return Err("text_edit_conflict".into());
            }
            // A replay can validate an already committed split, never perform
            // a second split or rewrite either half.
            let head = &edit.after[..at];
            if model
                .read(block)
                .ok()
                .and_then(|n| n.content.as_text().map(str::to_owned))
                .as_deref()
                != Some(head)
            {
                return Err("text_edit_conflict".into());
            }
            let mut replay = edit.clone();
            replay.before = head.into();
            replay.after = head.into();
            replay.create_paragraph = false;
            return apply_block_text(model, block, &replay);
        }
    }
    apply_block_text(model, block, edit)?;
    if let Some((tail, at)) = split {
        model
            .split_with_id(block, at, tail)
            .map_err(|_| "invalid_block_split")?;
    }
    Ok(())
}

fn apply_block_text(
    model: &mut WorkspaceDoc,
    block: NodeId,
    edit: &DocumentTextEdit,
) -> Result<(), String> {
    if !model.compatibility().writable() {
        return Err("document_read_only".into());
    }
    let root = model
        .children(Parent::Root)
        .map_err(|_| "invalid_document_snapshot")?
        .into_iter()
        .find(|id| model.kind_of(*id) == Some(Kind::Document))
        .ok_or("document_title_not_found")?;
    if model.kind_of(block).is_none() {
        if !edit.create_paragraph
            || !edit.before.is_empty()
            || edit.after.contains(['\n', '\r', '\u{fffc}'])
        {
            return Err("invalid_block_edit".into());
        }
        let mut node = Node::paragraph(&edit.after).map_err(|_| "invalid_block_edit")?;
        node.id = block;
        return model
            .insert(Parent::Node(root), node)
            .map(|_| ())
            .map_err(|_| "invalid_block_edit".into());
    }
    if !matches!(
        model.kind_of(block),
        Some(Kind::Paragraph | Kind::Quote | Kind::Task | Kind::Code)
    ) {
        return Err("block_read_only".into());
    }
    // A caller cannot edit a title, a detached node, or a node inside Trash.
    let mut parent = model.parent(block).map_err(|_| "invalid_block_edit")?;
    while parent != Parent::Node(root) {
        let Parent::Node(id) = parent else {
            return Err("invalid_block_edit".into());
        };
        if model.kind_of(id) == Some(Kind::Trash) {
            return Err("invalid_block_edit".into());
        }
        parent = model.parent(id).map_err(|_| "invalid_block_edit")?;
    }
    let current = model
        .read(block)
        .map_err(|_| "invalid_block_edit")?
        .content
        .as_text()
        .ok_or("invalid_block_edit")?
        .to_owned();
    // Also makes a retry after an uncertain acknowledgement harmless.
    if current == edit.after {
        return Ok(());
    }
    if current != edit.before {
        return Err("text_edit_conflict".into());
    }
    let (range, inserted) = splice(&edit.before, &edit.after);
    // Existing anchors may be removed atomically by delete_text, but native
    // keyboard/clipboard input must never manufacture references.
    if inserted.contains(['\u{fffc}', '\r']) {
        return Err("invalid_inline_text".into());
    }
    if inserted.contains('\n') {
        if model.kind_of(block) != Some(Kind::Code) {
            return Err("invalid_block_edit".into());
        }
        model
            .delete_text(block, range.clone())
            .map_err(|_| "invalid_block_edit")?;
        model
            .insert_text(block, range.start, inserted)
            .map_err(|_| "invalid_block_edit")?;
    } else {
        model
            .replace_text(block, range, inserted)
            .map_err(|_| "invalid_block_edit")?;
    }
    Ok(())
}

/// Byte-addressed minimal splice, with boundaries on whole Unicode scalars.
fn splice<'a>(before: &str, after: &'a str) -> (std::ops::Range<usize>, &'a str) {
    let prefix = before
        .chars()
        .zip(after.chars())
        .take_while(|(a, b)| a == b)
        .map(|(c, _)| c.len_utf8())
        .sum::<usize>();
    let suffix = before[prefix..]
        .chars()
        .rev()
        .zip(after[prefix..].chars().rev())
        .take_while(|(a, b)| a == b)
        .map(|(c, _)| c.len_utf8())
        .sum::<usize>();
    (
        prefix..before.len() - suffix,
        &after[prefix..after.len() - suffix],
    )
}

#[cfg(test)]
mod tests;
