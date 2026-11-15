// SPDX-License-Identifier: MPL-2.0

//! Bot instructions are distinct from the editable ordinary artifact preamble.
use koru_delegate_core::RunId;
use std::path::Path;

const INSTRUCTIONS: &str = "You are working as a Bot in Koru. Follow the Role and task supplied below. Your conversation replies are delivered to the person. Use search_documents and read_document for workspace context; use create_document and edit_document for native page results. Give each write a fresh operation_id UUID and retain it with identical arguments for a retry. Only report a page as Saved when its tool result confirms status saved. A refusal or save_status_unknown is not a successful save: report the restriction or uncertainty and do not blindly repeat the write with another operation_id. Do not start child agents or sessions. Do not put credentials or secrets in results. Change only what the task requires. The supplied context is read-only; your working folder does not grant additional Koru document permissions.";

pub(in crate::orchestrator) fn compose(run: RunId, workspace: &Path, input: &str) -> String {
    format!(
        "{INSTRUCTIONS}\n\nThis run is `{run}`. Attached context is under `{}`.\n\n{input}",
        crate::orchestrator::start::context_directory(workspace, run).display(),
    )
}

#[cfg(test)]
mod tests;
