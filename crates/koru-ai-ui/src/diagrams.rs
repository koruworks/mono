// SPDX-License-Identifier: MPL-2.0

//! Chat's view-only diagram state. The host uses the document layout worker;
//! this surface only requests work and paints prepared answers.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use koru_core::NodeId;
use koru_draw::diagram::PreparedDiagram;
use koru_editor::diagram::{DiagramRequest, Status, text_hash};

use crate::{ChatSurface, MessageLayout};

type Outcome = Result<Arc<PreparedDiagram>, Status>;

pub(crate) struct Diagram {
    source: Arc<str>,
    hash: u64,
    pub prepared: Option<Arc<PreparedDiagram>>,
}

impl Diagram {
    pub fn new(source: &str) -> Self {
        Self {
            source: source.into(),
            hash: text_hash(source),
            prepared: None,
        }
    }

    pub fn scale(&self, width: f32) -> f64 {
        self.prepared.as_ref().map_or(1.0, |p| {
            (f64::from(width.max(1.0)) / p.natural_size().width.max(1.0)).min(1.0)
        })
    }

    pub fn size(&self, width: f32) -> Option<(f32, f32)> {
        let size = self.prepared.as_ref()?.natural_size();
        let scale = self.scale(width);
        Some(((size.width * scale) as f32, (size.height * scale) as f32))
    }
}

struct Entry {
    node: NodeId,
    result: Option<Outcome>,
}

#[derive(Default)]
pub(crate) struct Diagrams {
    entries: HashMap<u64, Entry>,
    pending: Vec<DiagramRequest>,
}

impl Diagrams {
    /// Called only when the transcript changes or is remeasured. Retain just
    /// this transcript, including failures, across resizing and font changes.
    pub fn sync(&mut self, messages: &mut [MessageLayout]) {
        let mut wanted = HashSet::new();
        for diagram in messages
            .iter_mut()
            .flat_map(|m| &mut m.blocks)
            .filter_map(|b| b.diagram.as_mut())
        {
            wanted.insert(diagram.hash);
            let entry = self.entries.entry(diagram.hash).or_insert_with(|| {
                let node = NodeId::new();
                self.pending.push(DiagramRequest {
                    node,
                    text_hash: diagram.hash,
                    text: diagram.source.clone(),
                });
                Entry { node, result: None }
            });
            diagram.prepared = entry
                .result
                .as_ref()
                .and_then(|result| result.as_ref().ok())
                .cloned();
        }
        self.entries.retain(|hash, _| wanted.contains(hash));
        self.pending
            .retain(|request| wanted.contains(&request.text_hash));
    }
}

impl ChatSurface {
    /// Pending closed Mermaid blocks, for the host's existing diagram worker.
    /// A host without that worker leaves the source visible.
    pub fn take_diagram_requests(&mut self) -> Vec<DiagramRequest> {
        std::mem::take(&mut self.diagrams.pending)
    }

    /// Ignore replies for a removed/replaced transcript, including a previous
    /// request for identical text after leaving and reopening a chat.
    pub fn diagram_result(&mut self, node: NodeId, hash: u64, result: Outcome) -> bool {
        let Some(entry) = self
            .diagrams
            .entries
            .get_mut(&hash)
            .filter(|entry| entry.node == node && entry.result.is_none())
        else {
            return false;
        };
        entry.result = Some(result);
        // Reconcile reuses the text layouts; only block geometry changes.
        self.messages_dirty = true;
        true
    }
}

#[cfg(test)]
mod tests;
