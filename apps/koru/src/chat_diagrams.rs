// SPDX-License-Identifier: MPL-2.0

//! Connect both chat hosts to the same diagram worker documents use.

use koru_ai_ui::ChatSurface;
use koru_workspace_host::diagram::{DiagramClient, client};

#[derive(Default)]
pub(crate) struct ChatDiagrams(Option<DiagramClient>);

impl ChatDiagrams {
    pub fn maintain(&mut self, surface: &mut ChatSurface) -> bool {
        // Restored views can exist before the worker starts.
        if self.0.is_none() {
            self.0 = client();
        }
        let Some(client) = &self.0 else {
            return false;
        };
        for request in surface.take_diagram_requests() {
            client.request(request.node, request.text_hash, request.text);
        }
        let mut changed = false;
        for reply in client.drain() {
            changed |= surface.diagram_result(reply.node, reply.text_hash, reply.result);
        }
        changed
    }
}
