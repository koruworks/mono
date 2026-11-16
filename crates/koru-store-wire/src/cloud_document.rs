// SPDX-License-Identifier: MPL-2.0

//! Bounded, immutable native inspection of one exact Cloud copy. These values
//! never grant read/write authority or identify a global editor/cache entry.
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

pub const MAX_INSPECTION_BYTES: usize = 1024 * 1024;
pub const MAX_INSPECTION_BLOCKS: usize = 2000;
pub const MAX_INSPECTION_TEXT_BYTES: usize = 256 * 1024;
pub const MAX_INSPECTION_MARKS: usize = 8000;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloudDocumentSelection {
    pub account_id: String,
    pub workspace_id: String,
    pub incarnation_id: String,
    pub cloud_unit_id: String,
    pub document_id: String,
    pub source_head_digest: String,
}
impl std::fmt::Debug for CloudDocumentSelection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CloudDocumentSelection(<redacted>)")
    }
}
fn id(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(i, b)| {
            if [8, 13, 18, 23].contains(&i) {
                b == b'-'
            } else {
                b.is_ascii_digit() || (b'a'..=b'f').contains(&b)
            }
        })
}
impl CloudDocumentSelection {
    pub fn validate(&self) -> bool {
        [
            &self.account_id,
            &self.workspace_id,
            &self.incarnation_id,
            &self.cloud_unit_id,
            &self.document_id,
        ]
        .into_iter()
        .all(|v| id(v))
            && self.source_head_digest.len() == 64
            && self
                .source_head_digest
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    }
    pub fn from_json(json: &str) -> Result<Self, &'static str> {
        if json.len() > 1024 {
            return Err("invalid inspection selection");
        }
        let value: Self = serde_json::from_str(json).map_err(|_| "invalid inspection selection")?;
        if !value.validate() {
            return Err("invalid inspection selection");
        }
        Ok(value)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileRemovalMode {
    AttachmentLeaf,
    RetainedSlot,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileRemovalControl {
    pub node_id: String,
    pub mode: FileRemovalMode,
    pub in_trash: bool,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloudDocumentInspectionV1 {
    pub selection: CloudDocumentSelection,
    pub document: DocumentProjection,
    /// Presentation hint only; the worker rechecks current write permission.
    pub can_request_removal: bool,
    /// Native-proven leaf or attachment-slot operations. Never a note/page purge.
    pub removable_files: Vec<FileRemovalControl>,
}
impl std::fmt::Debug for CloudDocumentInspectionV1 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CloudDocumentInspectionV1(<redacted>)")
    }
}
impl CloudDocumentInspectionV1 {
    pub fn from_json(json: &str) -> Result<Self, &'static str> {
        if json.is_empty() || json.len() > MAX_INSPECTION_BYTES {
            return Err("invalid inspection size");
        }
        let value: Self = serde_json::from_str(json).map_err(|_| "invalid inspection payload")?;
        value.validate()?;
        Ok(value)
    }
    pub fn to_json(&self) -> Result<String, &'static str> {
        self.validate()?;
        let json = serde_json::to_string(self).map_err(|_| "invalid inspection payload")?;
        if json.len() > MAX_INSPECTION_BYTES {
            return Err("invalid inspection size");
        }
        Ok(json)
    }
    pub fn validate(&self) -> Result<(), &'static str> {
        if !self.selection.validate()
            || self.document.id != self.selection.document_id
            || self.document.workspace_id != self.selection.workspace_id
            || self.document.revision > i64::MAX as u64
            || self.document.blocks.len() > MAX_INSPECTION_BLOCKS
            || self.removable_files.len() > MAX_INSPECTION_BLOCKS
            || (!self.can_request_removal && !self.removable_files.is_empty())
        {
            return Err("invalid inspection scope");
        }
        let mut text = TextBudget(MAX_INSPECTION_TEXT_BYTES);
        text.add(&self.document.title)?;
        let mut ids = HashSet::new();
        let mut parents = HashSet::new();
        let mut eligible = HashMap::new();
        let mut marks = 0usize;
        for block in &self.document.blocks {
            if !id(&block.id)
                || !ids.insert(block.id.as_str())
                || block
                    .parent_id
                    .as_ref()
                    .is_some_and(|p| !ids.contains(p.as_str()) || p == &block.id)
            {
                return Err("invalid inspection hierarchy");
            }
            if let Some(parent) = &block.parent_id {
                parents.insert(parent.as_str());
            }
            if block.blob.is_some() {
                let mode = if matches!(block.kind.as_str(), "file" | "image" | "canvas") {
                    FileRemovalMode::AttachmentLeaf
                } else if matches!(
                    block.kind.as_str(),
                    "task"
                        | "paragraph"
                        | "quote"
                        | "callout"
                        | "document"
                        | "listblock"
                        | "table"
                        | "code"
                        | "contact"
                        | "divider"
                        | "trash"
                ) {
                    FileRemovalMode::RetainedSlot
                } else {
                    return Err("invalid attachment kind");
                };
                eligible.insert(block.id.as_str(), (block.in_trash, mode));
            }
            text.add(&block.kind)?;
            let p = &block.properties;
            for s in [&p.name, &p.icon, &p.color, &p.list_style, &p.language]
                .into_iter()
                .flatten()
            {
                text.add(s)?;
            }
            text.add(&p.alignment)?;
            if let Some(blob) = &block.blob {
                text.add(&blob.id)?;
                text.add(&blob.mime_type)?;
            }
            if let Some(reference) = &block.reference {
                text.reference(reference)?;
            }
            if let Some(rich) = &block.rich_text {
                text.add(&rich.text)?;
                marks = marks
                    .checked_add(rich.marks.len())
                    .and_then(|n| n.checked_add(rich.inline_objects.len()))
                    .ok_or("invalid inspection marks")?;
                if marks > MAX_INSPECTION_MARKS {
                    return Err("invalid inspection marks");
                }
                for mark in &rich.marks {
                    if mark.start_utf8 > mark.end_utf8
                        || mark.end_utf8 > rich.text.len()
                        || !rich.text.is_char_boundary(mark.start_utf8)
                        || !rich.text.is_char_boundary(mark.end_utf8)
                    {
                        return Err("invalid inspection text range");
                    }
                    text.add(&mark.kind)?;
                    if let Some(value) = &mark.value {
                        text.add(value)?;
                    }
                }
                for object in &rich.inline_objects {
                    if !id(&object.id) || !rich.text.is_char_boundary(object.anchor_utf8) {
                        return Err("invalid inspection inline object");
                    }
                    text.add(&object.role)?;
                    if let Some(label) = &object.label {
                        text.add(label)?;
                    }
                    text.reference(&object.reference)?;
                }
            }
        }
        let mut controls = HashSet::new();
        for file in &self.removable_files {
            if !controls.insert(file.node_id.as_str())
                || (file.mode == FileRemovalMode::AttachmentLeaf
                    && parents.contains(file.node_id.as_str()))
                || eligible.get(file.node_id.as_str()) != Some(&(file.in_trash, file.mode))
            {
                return Err("invalid inspection removal target");
            }
        }
        Ok(())
    }
}
struct TextBudget(usize);
impl TextBudget {
    fn add(&mut self, value: &str) -> Result<(), &'static str> {
        self.0 = self
            .0
            .checked_sub(value.len())
            .ok_or("invalid inspection text size")?;
        Ok(())
    }
    fn reference(&mut self, r: &EntityReference) -> Result<(), &'static str> {
        match r {
            EntityReference::Document {
                document_id,
                node_id,
            } => {
                if !id(document_id) || node_id.as_ref().is_some_and(|v| !id(v)) {
                    return Err("invalid inspection reference");
                }
            }
            EntityReference::Block {
                block_id,
                document_id,
            } => {
                if !id(block_id) || document_id.as_ref().is_some_and(|v| !id(v)) {
                    return Err("invalid inspection reference");
                }
            }
            EntityReference::Run { document_id } => {
                if !id(document_id) {
                    return Err("invalid inspection reference");
                }
            }
            EntityReference::Url { url } => self.add(url)?,
            EntityReference::Label { value } => self.add(value)?,
        }
        Ok(())
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentProjection {
    pub id: String,
    pub workspace_id: String,
    pub title: String,
    pub revision: u64,
    pub blocks: Vec<BlockProjection>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlockProjection {
    /// Inherited as well as direct in-document Trash membership.
    pub in_trash: bool,
    pub id: String,
    pub parent_id: Option<String>,
    pub kind: String,
    pub properties: BlockProperties,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rich_text: Option<RichText>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blob: Option<BlobReference>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reference: Option<EntityReference>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlockProperties {
    pub name: Option<String>,
    pub icon: Option<String>,
    pub color: Option<String>,
    pub done: bool,
    pub alignment: String,
    pub heading_level: Option<u8>,
    pub list_style: Option<String>,
    pub image_width: Option<u32>,
    pub image_height: Option<u32>,
    pub language: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RichText {
    pub text: String,
    pub marks: Vec<TextMark>,
    pub inline_objects: Vec<InlineObject>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TextMark {
    pub start_utf8: usize,
    pub end_utf8: usize,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InlineObject {
    pub id: String,
    pub anchor_utf8: usize,
    pub role: String,
    pub label: Option<String>,
    pub reference: EntityReference,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlobReference {
    pub id: String,
    pub mime_type: String,
    pub size: Option<u64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum EntityReference {
    Document {
        document_id: String,
        /// Passive label context only; this never authorizes navigation.
        #[serde(skip_serializing_if = "Option::is_none")]
        node_id: Option<String>,
    },
    Block {
        block_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        document_id: Option<String>,
    },
    Url {
        url: String,
    },
    Label {
        value: String,
    },
    /// Passive run identity; never a navigation or execution grant.
    Run {
        document_id: String,
    },
}

#[cfg(test)]
mod tests;

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CloudDocumentAction {
    Inspect,
    RemoveFile { node_id: String },
}
impl CloudDocumentAction {
    pub fn validate(&self) -> bool {
        match self {
            Self::Inspect => true,
            Self::RemoveFile { node_id } => id(node_id),
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloudDocumentFailure {
    Unavailable,
    SourceChanged,
    OutcomeUnknown,
}
impl CloudDocumentFailure {
    pub fn code(self) -> &'static str {
        match self {
            Self::Unavailable => "cloud_document_unavailable",
            Self::SourceChanged => "cloud_document_source_changed",
            Self::OutcomeUnknown => "cloud_edit_outcome_unknown",
        }
    }
}

/// A detached view or a completed ordinary remote publication. `Removed` does
/// not assert released bytes; authoritative inventory and usage must be read.
#[derive(Serialize, Deserialize)]
#[serde(
    tag = "status",
    content = "view",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum CloudDocumentOutcome {
    View(CloudDocumentInspectionV1),
    Removed,
}
