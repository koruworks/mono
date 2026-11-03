// SPDX-License-Identifier: MPL-2.0

//! The document window's half of private toggle folds: hand the editor's
//! fold intents to `koru_workspace_host::folds`, which saves them off this
//! thread, and give the editor back the current answer whenever it moves.
//!
//! Free when nothing changed: one integer compare and an empty drain.

use std::collections::HashSet;

use koru_core::NodeId;
use koru_core::schema::WorkspaceDoc;
use koru_editor::editor::Editor;
use koru_workspace_host::folds;

#[derive(Default)]
pub struct FoldSync {
    /// The document whose folds the editor holds, by its own id.
    document: Option<koru_core::DocumentId>,
    seen: Option<u64>,
    /// The save-failure notice is shown once per document per window.
    failure_shown: bool,
}

impl FoldSync {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns whether the rows changed and the view needs a frame.
    pub fn sync(&mut self, editor: &mut Editor, ws: &WorkspaceDoc) -> bool {
        let Some(document) = ws.own_document_id() else {
            return false;
        };
        let store = koru_workspace::explore::store_document(document);
        if self.document != Some(document) {
            self.document = Some(document);
            self.seen = None;
            self.failure_shown = false;
            folds::ensure(store);
        }
        let intents = editor.body.take_fold_intents();
        if !intents.is_empty() {
            folds::set(
                store,
                intents
                    .into_iter()
                    .map(|intent| (intent.node.as_uuid(), intent.closed))
                    .collect(),
            );
        }
        let version = folds::version();
        if self.seen == Some(version) {
            return false;
        }
        self.seen = Some(version);
        if folds::save_failed(store) && !self.failure_shown {
            self.failure_shown = true;
            editor.view.notice = Some(
                koru_i18n::t(koru_i18n::current(), koru_i18n::Key::ToggleSaveFailed).into(),
            );
        }
        let Some((_, closed)) = folds::closed(store) else {
            return false;
        };
        let closed: HashSet<NodeId> = closed.into_iter().map(NodeId::from_uuid).collect();
        editor.body.adopt_folds(ws, closed)
    }
}
