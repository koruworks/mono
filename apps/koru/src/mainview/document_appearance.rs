// SPDX-License-Identifier: MPL-2.0

//! Navigator decorations from the existing attribute index. Reads run on a
//! worker, never open document snapshots, and stop while Documents is hidden.

use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use koru_core::attributes::{KEY_COLOR, KEY_PRIORITY, Priority};
use koru_core::{NodeId, WorkspaceId};
use koru_store_wire::attributes::{AttributeQuery, AttributeRow};

use crate::panes::explore::DocumentAppearance;

type Appearances = HashMap<NodeId, DocumentAppearance>;
const REFRESH: Duration = Duration::from_secs(10);
const POLL: Duration = Duration::from_millis(50);

struct Answer {
    workspace: WorkspaceId,
    generation: u64,
    rows: Result<Appearances, String>,
}

#[derive(Default)]
pub(super) struct DocumentAppearances {
    client: Option<koru_api_client::Client>,
    workspace: Option<WorkspaceId>,
    generation: u64,
    visible: bool,
    receiver: Option<Receiver<Answer>>,
    next_refresh: Option<Instant>,
    pub(super) rows: Appearances,
    // Preserve local edits across answers from a projection that has not caught
    // up. Matching answers acknowledge them; the ceiling bounds stale echoes.
    pending: HashMap<NodeId, (DocumentAppearance, Instant)>,
    observed: Option<(NodeId, DocumentAppearance)>,
}

impl DocumentAppearances {
    pub(super) fn set_client(&mut self, client: Option<koru_api_client::Client>) {
        self.client = client;
    }

    pub(super) fn set_workspace(&mut self, workspace: Option<WorkspaceId>) -> bool {
        if self.workspace == workspace {
            return false;
        }
        self.workspace = workspace;
        self.generation = self.generation.wrapping_add(1);
        self.rows.clear();
        self.pending.clear();
        self.observed = None;
        self.next_refresh = Some(Instant::now());
        true
    }

    pub(super) fn observe(&mut self, id: NodeId, appearance: DocumentAppearance) {
        if self.observed.as_ref() == Some(&(id, appearance.clone())) {
            return;
        }
        self.observed = Some((id, appearance.clone()));
        self.rows.insert(id, appearance.clone());
        self.pending.insert(id, (appearance, Instant::now()));
        // Allow the committed edit to reach the derived index before asking.
        self.next_refresh = Some(Instant::now() + Duration::from_millis(500));
    }

    pub(super) fn deadline(&self) -> Option<Instant> {
        if !self.visible || self.workspace.is_none() || self.client.is_none() {
            return None;
        }
        if self.receiver.is_some() {
            Some(Instant::now() + POLL)
        } else {
            self.next_refresh
        }
    }

    pub(super) fn maintain(&mut self, visible: bool, now: Instant) -> bool {
        if visible && !self.visible {
            self.next_refresh = Some(now);
        }
        self.visible = visible;
        let mut changed = false;
        if let Some(receiver) = &self.receiver {
            match receiver.try_recv() {
                Ok(answer) => {
                    self.receiver = None;
                    if Some(answer.workspace) == self.workspace
                        && answer.generation == self.generation
                        && let Ok(mut rows) = answer.rows
                    {
                        self.pending.retain(|id, (local, since)| {
                            rows.get(id).cloned().unwrap_or_default() != *local
                                && now.saturating_duration_since(*since) < Duration::from_secs(30)
                        });
                        for (id, (local, _)) in &self.pending {
                            rows.insert(*id, local.clone());
                        }
                        changed = self.rows != rows;
                        self.rows = rows;
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => self.receiver = None,
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if visible && self.receiver.is_none() && self.next_refresh.is_some_and(|at| at <= now) {
            self.next_refresh = Some(now + REFRESH);
            if let (Some(workspace), Some(client)) = (self.workspace, self.client.clone()) {
                let generation = self.generation;
                let (send, receive) = mpsc::channel();
                if std::thread::Builder::new()
                    .name("koru-document-appearance".into())
                    .spawn(move || {
                        let rows = read(&client, workspace);
                        let _ = send.send(Answer {
                            workspace,
                            generation,
                            rows,
                        });
                    })
                    .is_ok()
                {
                    self.receiver = Some(receive);
                }
            }
        }
        changed
    }
}

fn read(client: &koru_api_client::Client, workspace: WorkspaceId) -> Result<Appearances, String> {
    let mut out = HashMap::new();
    // Query only the two decoration keys, paging by the server's actual cap.
    // A partial/failed refresh never replaces the previously complete answer.
    for key in [KEY_COLOR, KEY_PRIORITY] {
        let mut offset = 0;
        loop {
            let response = client
                .store()
                .query_attributes(&AttributeQuery {
                    workspaces: vec![koru_store_core::ids::WorkspaceId::from_uuid(
                        workspace.as_uuid(),
                    )],
                    has: Some(key.into()),
                    limit: Some(500),
                    offset: Some(offset),
                    ..Default::default()
                })
                .map_err(|error| error.to_string())?;
            if !response.projected || !response.updating_workspaces.is_empty() {
                return Err("Document appearance index is updating".into());
            }
            for row in &response.rows {
                if row.workspace.as_uuid() == workspace.as_uuid() {
                    collect(&mut out, row);
                }
            }
            if response.limit == 0 || response.rows.len() < response.limit {
                break;
            }
            offset = response.offset + response.rows.len();
        }
    }
    Ok(out)
}

fn collect(out: &mut Appearances, row: &AttributeRow) {
    if row.trashed || row.node_kind != "document" {
        return;
    }
    let appearance = DocumentAppearance {
        color: row
            .attributes
            .iter()
            .find(|attr| attr.role == KEY_COLOR && attr.kind == "text")
            .map(|attr| attr.value.clone()),
        priority: row
            .attributes
            .iter()
            .find(|attr| attr.role == KEY_PRIORITY && attr.kind == "text")
            .and_then(|attr| Priority::parse(&attr.value)),
    };
    out.insert(NodeId::from_uuid(row.document.as_uuid()), appearance);
}

#[cfg(test)]
mod tests;
