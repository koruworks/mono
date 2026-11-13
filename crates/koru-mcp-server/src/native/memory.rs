// SPDX-License-Identifier: MPL-2.0
//! Trusted in-process Memory preparation. These inputs are not model tools or
//! authority tokens; the owner must prove the binding and acquire its lease.
use super::*;
use koru_ai_core::bots::DeviceId;
use koru_store_core::{ProfileId, UserScopeId};
use uuid::Uuid;

const BODY_LIMIT: usize = 65_536;
const SNAPSHOT_LIMIT: usize = 16 * 1024 * 1024;
/// Stable delegate request title, not the page's mutable display title. Carry
/// unchanged into PrepareMemoryEdit; actual Store evidence names the real page.
pub const MEMORY_REQUEST_TITLE: &str = "Memory";
const SINGLE_PARAGRAPH: &str = "Memory summary and detail must each be a single paragraph. Use the ordinary editor for multiline content.";

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryWriteAction {
    Remember,
    Correct,
    Forget,
}

#[derive(Clone)]
pub enum MemoryRecipeOperation {
    Remember {
        summary: String,
        detail: String,
    },
    Correct {
        fact: Uuid,
        summary: String,
        detail: String,
    },
    Forget {
        fact: Uuid,
    },
}

#[derive(Clone, Serialize)]
pub struct MemoryEditRecipe {
    pub workspace: StoreWorkspaceId,
    pub document: StoreDocumentId,
    pub history: Uuid,
    pub root: Uuid,
    pub change: MemoryRecipeChange,
}
#[derive(Clone, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum MemoryRecipeChange {
    Remember {
        after: Option<Uuid>,
        summary: String,
        detail: String,
    },
    Correct {
        fact: Uuid,
        expected: String,
        children: Vec<MemoryExistingChild>,
        summary: String,
        detail: String,
    },
    Forget {
        fact: Uuid,
        expected: String,
    },
}
#[derive(Clone, Serialize)]
pub struct MemoryExistingChild {
    pub node: Uuid,
    pub expected: String,
}

/// Scalar adapter view of immutable delegate authority. No new wire format.
#[derive(Clone, Serialize)]
pub struct MemoryWriteAuthority {
    pub version: u32,
    pub owner: UserScopeId,
    pub device: DeviceId,
    pub bot: Uuid,
    pub submission: Uuid,
    pub captured_config_revision: u64,
    pub creation_request: Uuid,
    pub workspace: StoreWorkspaceId,
    pub memory_document: StoreDocumentId,
    pub action: MemoryWriteAction,
}
#[derive(Clone)]
pub struct MemoryWriteBinding {
    pub authority: MemoryWriteAuthority,
    pub history: Uuid,
    pub expected_revision: Vec<u8>,
    pub fact: Uuid,
    pub detail: Option<Uuid>,
    pub source: Option<Uuid>,
    pub source_run_document: StoreDocumentId,
    pub created_at_ms: i64,
    pub reader: ProfileId,
}

struct Counter(usize);
impl std::io::Write for Counter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 = self
            .0
            .checked_add(bytes.len())
            .filter(|n| *n <= BODY_LIMIT)
            .ok_or_else(|| std::io::Error::other("Memory body exceeds bound"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn bounded(value: &impl Serialize) -> Result<(), ToolError> {
    serde_json::to_writer(Counter(0), value)
        .map_err(|_| ToolError::invalid("Memory body exceeds 64KiB"))
}
fn v4(id: Uuid) -> bool {
    id.get_version_num() == 4 && id.get_variant() == uuid::Variant::RFC4122
}

pub fn validate_memory_text(summary: &str, detail: &str) -> Result<(), ToolError> {
    if summary.contains('\n') || detail.contains('\n') {
        return Err(ToolError::invalid(SINGLE_PARAGRAPH));
    }
    if summary.len() > 1024
        || summary.trim().is_empty()
        || !summary
            .chars()
            .any(|c| !c.is_whitespace() && c != koru_core::ANCHOR)
        || detail.is_empty()
        || detail.len() > 16 * 1024
    {
        return Err(ToolError::invalid(
            "invalid Memory summary or detail bounds",
        ));
    }
    Ok(())
}
impl MemoryEditRecipe {
    /// Independently checked by producer and consumer. This validates shape,
    /// not history or current authority; only the trusted owner proves those.
    pub fn validate(&self) -> Result<(), ToolError> {
        if self.workspace.as_uuid().is_nil()
            || self.document.as_uuid().is_nil()
            || self.history.is_nil()
        {
            return Err(ToolError::invalid("invalid Memory recipe identity"));
        }
        match &self.change {
            MemoryRecipeChange::Remember {
                after,
                summary,
                detail,
            } => {
                if *after == Some(self.root) {
                    return Err(ToolError::invalid("invalid Memory placement"));
                }
                validate_memory_text(summary, detail)?;
            }
            MemoryRecipeChange::Correct {
                fact,
                expected,
                children,
                summary,
                detail,
            } => {
                validate_memory_text(summary, detail)?;
                let mut ids = HashSet::from([self.root, *fact]);
                let mut bytes = expected.len();
                if *fact == self.root || children.len() > 128 {
                    return Err(ToolError::invalid("invalid Memory correction"));
                }
                for child in children {
                    if !ids.insert(child.node) {
                        return Err(ToolError::invalid("duplicate Memory child"));
                    }
                    bytes = bytes
                        .checked_add(child.expected.len())
                        .ok_or_else(|| ToolError::invalid("Memory text exceeds bound"))?;
                }
                if bytes > 32 * 1024 {
                    return Err(ToolError::invalid("Memory text exceeds 32KiB"));
                }
            }
            MemoryRecipeChange::Forget { fact, .. } => {
                if *fact == self.root {
                    return Err(ToolError::invalid("invalid Memory fact"));
                }
            }
        }
        bounded(self)
    }
}

#[derive(Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Operation {
    Edit {
        operation: EditOperation,
    },
    Trash {
        node: Uuid,
        expected: String,
        at_ms: i64,
    },
}
#[derive(Clone)]
pub(super) struct PreparedMemory {
    operations: Vec<Operation>,
    reader: ProfileId,
    source_run_document: StoreDocumentId,
}
impl PreparedMemory {
    pub(super) fn apply(
        &self,
        backend: &LocalBackend,
        ws: StoreWorkspaceId,
        doc: &mut WorkspaceDoc,
    ) -> Result<(), NativeWriteError> {
        let mut context = backend.native_context(ws)?;
        if context.profile.as_uuid() != self.reader.as_uuid() {
            return Err(conflict().into());
        }
        // Core insertion intentionally supports callers that already own ID
        // uniqueness. A trusted Memory recipe carries only local structure,
        // so reject collisions against every live indexed node, including Trash,
        // before applying any operation. Never overwrite an existing ID index.
        for operation in &self.operations {
            if let Operation::Edit {
                operation: EditOperation::Insert { node_id, .. },
            } = operation
            {
                let id = node(node_id.as_deref().unwrap_or_default())?;
                if !matches!(doc.parent(id), Err(koru_core::StoreError::NoSuchNode(missing)) if missing == id)
                {
                    return Err(conflict().into());
                }
            }
        }
        context
            .documents
            .insert(self.source_run_document, "Source Run".into());
        for operation in &self.operations {
            match operation {
                Operation::Edit { operation } => {
                    let _ = apply_ops(doc, std::slice::from_ref(operation), &context)?;
                }
                Operation::Trash {
                    node,
                    expected,
                    at_ms,
                } => {
                    let id = NodeId::from_uuid(*node);
                    if doc.read(id)?.content.as_text() != Some(expected.as_str()) {
                        return Err(conflict().into());
                    }
                    doc.trash(id, *at_ms, None)?;
                }
            }
        }
        Ok(())
    }
}

pub(super) fn validate_snapshot(bytes: &[u8]) -> Result<(), ToolError> {
    if bytes.is_empty() || bytes.len() > SNAPSHOT_LIMIT {
        return Err(ToolError::invalid("Memory snapshot exceeds import bounds"));
    }
    Ok(())
}

impl LocalBackend {
    pub fn prepare_memory_write(
        &self,
        recipe: MemoryEditRecipe,
        binding: MemoryWriteBinding,
    ) -> Result<PreparedNativeWrite, ToolError> {
        recipe.validate()?;
        let authority = &binding.authority;
        let vector = strict_version(&binding.expected_revision)?;
        if authority.version != 1
            || authority.owner.as_uuid().is_nil()
            || authority.device.as_uuid().is_nil()
            || !v4(authority.bot)
            || !v4(authority.submission)
            || !v4(authority.creation_request)
            || !(1..=i64::MAX as u64).contains(&authority.captured_config_revision)
            || authority.workspace != recipe.workspace
            || authority.memory_document != recipe.document
            || binding.history != recipe.history
            || binding.reader.as_uuid().is_nil()
            || binding.source_run_document.as_uuid().is_nil()
            || binding.source_run_document == recipe.document
            || binding.created_at_ms < 0
        {
            return Err(ToolError::invalid("invalid Memory write binding"));
        }
        let mut operations = Vec::new();
        let mut old_ids = HashSet::from([recipe.root]);
        let expected_action = match &recipe.change {
            MemoryRecipeChange::Remember { after, summary, .. } => {
                if !v4(binding.fact) {
                    return Err(ToolError::invalid("new Memory fact must be UUIDv4"));
                }
                if let Some(after) = after {
                    old_ids.insert(*after);
                }
                operations.push(edit(insert(
                    binding.fact,
                    recipe.root,
                    *after,
                    summary.clone(),
                )));
                operations.push(edit(EditOperation::SetHeading {
                    node_id: binding.fact.to_string(),
                    expected: summary.clone(),
                    heading: Some(2),
                }));
                MemoryWriteAction::Remember
            }
            MemoryRecipeChange::Correct {
                fact,
                expected,
                children,
                summary,
                ..
            } => {
                if binding.fact != *fact {
                    return Err(ToolError::invalid("Memory fact changed"));
                }
                old_ids.insert(*fact);
                operations.push(edit(EditOperation::ReplaceText {
                    node_id: fact.to_string(),
                    expected: expected.clone(),
                    text: summary.clone(),
                }));
                for child in children {
                    old_ids.insert(child.node);
                    operations.push(Operation::Trash {
                        node: child.node,
                        expected: child.expected.clone(),
                        at_ms: binding.created_at_ms,
                    });
                }
                MemoryWriteAction::Correct
            }
            MemoryRecipeChange::Forget { fact, expected } => {
                if binding.fact != *fact {
                    return Err(ToolError::invalid("Memory fact changed"));
                }
                operations.push(Operation::Trash {
                    node: *fact,
                    expected: expected.clone(),
                    at_ms: binding.created_at_ms,
                });
                MemoryWriteAction::Forget
            }
        };
        if authority.action != expected_action {
            return Err(ToolError::invalid("Memory action changed"));
        }
        if expected_action == MemoryWriteAction::Forget {
            if binding.detail.is_some() || binding.source.is_some() {
                return Err(ToolError::invalid("unexpected Memory child identities"));
            }
        } else {
            if expected_action == MemoryWriteAction::Remember && !old_ids.insert(binding.fact) {
                return Err(ToolError::invalid("Memory fact collides"));
            }
            let (Some(detail), Some(source)) = (binding.detail, binding.source) else {
                return Err(ToolError::invalid("missing Memory child identities"));
            };
            if !v4(detail) || !v4(source) || !old_ids.insert(detail) || !old_ids.insert(source) {
                return Err(ToolError::invalid("invalid Memory child identities"));
            }
            let text = match &recipe.change {
                MemoryRecipeChange::Remember { detail, .. }
                | MemoryRecipeChange::Correct { detail, .. } => detail.clone(),
                _ => unreachable!(),
            };
            operations.push(edit(insert(detail, binding.fact, None, text)));
            operations.push(edit(insert(
                source,
                binding.fact,
                Some(detail),
                String::new(),
            )));
            operations.push(edit(EditOperation::InsertInline {
                node_id: source.to_string(),
                expected: String::new(),
                at_utf8: 0,
                inline: InlineValue::Document {
                    document_id: binding.source_run_document.to_storage(),
                    node_id: None,
                    label: Some("Source Run".into()),
                },
            }));
        }
        if operations.len() > 256 {
            return Err(ToolError::invalid("too many Memory operations"));
        }
        bounded(&operations)?;
        let mut entries = vector
            .iter()
            .map(|(peer, counter)| (*peer, *counter))
            .collect::<Vec<_>>();
        entries.sort_unstable();
        let encoded = serde_json::to_vec(&(
            "koru.bot.memory.edit.v1",
            authority,
            binding.source_run_document,
            &operations,
            entries,
        ))
        .map_err(|_| ToolError::invalid("invalid Memory fingerprint"))?;
        Ok(PreparedNativeWrite {
            description: NativeWriteDescription {
                kind: NativeWriteKind::Edited,
                workspace: recipe.workspace,
                document: Some(recipe.document),
                fingerprint: koru_store_core::checksum(&encoded),
                expected_revision: Some(binding.expected_revision),
                title: MEMORY_REQUEST_TITLE.into(),
            },
            request: Request::Memory(PreparedMemory {
                operations,
                reader: binding.reader,
                source_run_document: binding.source_run_document,
            }),
        })
    }
}
fn edit(operation: EditOperation) -> Operation {
    Operation::Edit { operation }
}
fn insert(id: Uuid, parent: Uuid, after: Option<Uuid>, text: String) -> EditOperation {
    EditOperation::Insert {
        node_id: Some(id.to_string()),
        parent_id: parent.to_string(),
        after_node_id: after.map(|id| id.to_string()),
        kind: InsertKind::Paragraph,
        text,
    }
}
