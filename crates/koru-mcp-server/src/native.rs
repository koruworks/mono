// SPDX-License-Identifier: MPL-2.0
//! Semantic page writes for a trusted owner. This module does not authorize,
//! schedule or register Run results; the caller holds those responsibilities.
//! Ordinary MCP requests retain their existing execution path and vocabulary.
use super::*;
use koru_store_core::{CreationOrigin, CreationOriginLookup};

pub mod memory;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeWriteKind {
    Created,
    Edited,
}

/// Provider arguments never supply this identity. The authenticated owner
/// supplies its persisted candidate and scoped Store operation key.
#[derive(Clone)]
pub struct NativeStoreIdentity {
    pub candidate: StoreDocumentId,
    pub scope: String,
    pub request_key: String,
}
impl NativeStoreIdentity {
    /// The trusted adapter supplies its vouched local owner, never tool input.
    pub fn binding(
        &self,
        owner: koru_store_core::UserScopeId,
        workspace: StoreWorkspaceId,
        kind: koru_store_core::NativeEffectKind,
        request_fingerprint: [u8; 32],
    ) -> Result<koru_store_core::NativeEffectBinding, ToolError> {
        validate_identity(self)?;
        let binding = koru_store_core::NativeEffectBinding {
            user_scope: owner,
            scope: self.scope.clone(),
            request_key: uuid::Uuid::parse_str(&self.request_key)
                .map_err(|_| ToolError::invalid("invalid native Store identity"))?,
            workspace,
            document: self.candidate,
            kind,
            request_fingerprint,
        };
        if !binding.valid() {
            return Err(ToolError::invalid("invalid native Store binding"));
        }
        Ok(binding)
    }
}
#[derive(Clone)]
pub struct NativeWriteDescription {
    pub kind: NativeWriteKind,
    pub workspace: StoreWorkspaceId,
    pub document: Option<StoreDocumentId>,
    pub fingerprint: String,
    pub expected_revision: Option<Vec<u8>>,
    pub title: String,
}

/// Constructed only by shared semantic validation, before any effects or random
/// node identities. The semantic request cannot be replaced after admission.
#[derive(Clone)]
pub struct PreparedNativeWrite {
    description: NativeWriteDescription,
    request: Request,
}
#[derive(Clone)]
enum Request {
    Create(CreateDocumentRequest),
    Edit(EditDocumentRequest),
    Memory(memory::PreparedMemory),
}
impl PreparedNativeWrite {
    pub fn description(&self) -> &NativeWriteDescription {
        &self.description
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct NativeWriteEvidence {
    pub kind: NativeWriteKind,
    pub workspace: StoreWorkspaceId,
    pub document: StoreDocumentId,
    pub version: Vec<u8>,
    pub title: String,
    pub confirmed_at_ms: i64,
    /// The immutable Store version this save produced (M6). Every save this
    /// build makes carries it. It is absent only on evidence rebuilt from a
    /// result reference retained before the id was kept, so that such a
    /// reference is confirmed again exactly as it was first sent.
    pub version_id: Option<koru_store_core::ids::DocumentVersionId>,
}
/// Unconfirmed is deliberately not NotApplied and never permits blind replay.
pub enum NativeReconciliation {
    Verified(NativeWriteEvidence),
    Unconfirmed,
}

/// Exact phase of a semantic failure; gateways must not infer it from an
/// error code which might also occur after a successful content write.
#[derive(Debug)]
pub enum NativeWriteError {
    Refused(ToolError),
    Unconfirmed(ToolError),
}
impl From<ToolError> for NativeWriteError {
    fn from(error: ToolError) -> Self {
        Self::Refused(error)
    }
}
impl From<koru_core::StoreError> for NativeWriteError {
    fn from(error: koru_core::StoreError) -> Self {
        Self::Refused(error.into())
    }
}
impl From<koru_core::ShapeError> for NativeWriteError {
    fn from(error: koru_core::ShapeError) -> Self {
        Self::Refused(error.into())
    }
}
impl NativeWriteError {
    pub fn error(&self) -> &ToolError {
        match self {
            Self::Refused(error) | Self::Unconfirmed(error) => error,
        }
    }
}
struct ArgumentCounter(usize);
impl std::io::Write for ArgumentCounter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 = self
            .0
            .checked_add(bytes.len())
            .filter(|size| *size <= 4 * 1024 * 1024)
            .ok_or_else(|| std::io::Error::other("native request size exceeded"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub(super) fn unavailable() -> ToolError {
    ToolError {
        code: "native_store_unavailable".into(),
        message: "the Store lacks trusted native write support".into(),
        details: None,
    }
}
fn conflict() -> ToolError {
    ToolError {
        code: "revision_conflict".into(),
        message: "the exact document revision or creation binding changed".into(),
        details: None,
    }
}
fn strict_version(bytes: &[u8]) -> Result<loro::VersionVector, ToolError> {
    if bytes.is_empty() || bytes.len() > 65_536 {
        return Err(ToolError::invalid("invalid revision bounds"));
    }
    loro::VersionVector::decode(bytes).map_err(|_| ToolError::invalid("invalid revision encoding"))
}
fn validate_identity(identity: &NativeStoreIdentity) -> Result<(), ToolError> {
    if identity.scope.is_empty()
        || identity.scope.len() > 256
        || koru_store_core::ids::HistoryEntryId::parse(&identity.request_key).is_none_or(|id| {
            id.to_storage() != identity.request_key
                || id.as_uuid().get_version_num() != 4
                || id.as_uuid().get_variant() != uuid::Variant::RFC4122
        })
        || (identity.candidate.as_uuid().get_version_num() != 4
            || identity.candidate.as_uuid().get_variant() != uuid::Variant::RFC4122)
    {
        return Err(ToolError::invalid("invalid native Store identity"));
    }
    Ok(())
}
fn fingerprint_bytes(raw: &str) -> Result<[u8; 32], ToolError> {
    if raw.len() != 64 || !raw.is_ascii() {
        return Err(ToolError::invalid("invalid native fingerprint"));
    }
    let mut bytes = [0; 32];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&raw[index * 2..index * 2 + 2], 16)
            .map_err(|_| ToolError::invalid("invalid native fingerprint"))?;
    }
    Ok(bytes)
}
fn origin_matches(origin: &CreationOrigin, identity: &NativeStoreIdentity, hash: &str) -> bool {
    origin.document == identity.candidate
        && origin.scope == identity.scope
        && origin.request_key == identity.request_key
        && origin.request_hash == hash
        && !origin.initial_update.is_empty()
        && origin.initial_update.len() <= 16 * 1024 * 1024
}
fn edit_allowed(operation: &EditOperation) -> bool {
    matches!(
        operation,
        EditOperation::ReplaceText { .. }
            | EditOperation::Insert {
                kind: InsertKind::Paragraph
                    | InsertKind::Quote
                    | InsertKind::ListBlock
                    | InsertKind::Table
                    | InsertKind::Code
                    | InsertKind::Divider
                    | InsertKind::Callout,
                ..
            }
            | EditOperation::Move { .. }
            | EditOperation::Remove { .. }
            | EditOperation::SetLanguage { .. }
            | EditOperation::SetAlign { .. }
            | EditOperation::SetMark { .. }
            | EditOperation::SetHeading { .. }
            | EditOperation::SetHighlight { .. }
            | EditOperation::SetCalloutLook { .. }
    )
}
fn initial_allowed(operation: &InitialOperation) -> bool {
    matches!(
        operation,
        InitialOperation::Insert {
            kind: InsertKind::Paragraph
                | InsertKind::Quote
                | InsertKind::ListBlock
                | InsertKind::Table
                | InsertKind::Code
                | InsertKind::Divider
                | InsertKind::Callout,
            ..
        } | InitialOperation::SetLanguage { .. }
            | InitialOperation::SetAlign { .. }
            | InitialOperation::SetMark { .. }
            | InitialOperation::SetCalloutLook { .. }
    )
}

impl LocalBackend {
    /// Parse the ordinary shared request types, narrowed to page-contained
    /// effects. Does not discover a Store, read content or write anything.
    pub fn prepare_native_write(
        &self,
        tool: &str,
        arguments: Value,
    ) -> Result<PreparedNativeWrite, ToolError> {
        serde_json::to_writer(ArgumentCounter(0), &arguments)
            .map_err(|_| ToolError::invalid("native arguments exceed bound"))?;
        match tool {
            "create_document" => {
                let mut a: CreateDocumentRequest = serde_json::from_value(arguments)
                    .map_err(|_| ToolError::invalid("invalid create arguments"))?;
                let ws = workspace(&a.workspace_id)?;
                if a.kind.as_deref().unwrap_or("page") != "page"
                    || a.parent_document_id.is_some()
                    || a.idempotency_key.is_some()
                    || a.title.trim().is_empty()
                    || a.title.len() > 256
                    || a.initial_operations.as_ref().is_some_and(|ops| {
                        ops.is_empty() || ops.iter().any(|op| !initial_allowed(op))
                    })
                {
                    return Err(ToolError::invalid(
                        "native creation requires a root page and supported body operations",
                    ));
                }
                a.workspace_id = ws.to_storage();
                a.kind = Some("page".into());
                let description = NativeWriteDescription {
                    kind: NativeWriteKind::Created,
                    workspace: ws,
                    document: None,
                    fingerprint: create_request_hash(&a, ws)?,
                    expected_revision: None,
                    title: a.title.clone(),
                };
                Ok(PreparedNativeWrite {
                    description,
                    request: Request::Create(a),
                })
            }
            "edit_document" => {
                let mut a: EditDocumentRequest = serde_json::from_value(arguments)
                    .map_err(|_| ToolError::invalid("invalid edit arguments"))?;
                let ws = workspace(&a.workspace_id)?;
                let target = document(&a.document_id)?;
                if a.operations.is_empty()
                    || a.operations.iter().any(|op| !edit_allowed(op))
                    || !a.base_revision.is_ascii()
                    || a.base_revision.len() > 131_072
                {
                    return Err(ToolError::invalid(
                        "native edit requires supported body operations and an exact revision",
                    ));
                }
                let expected = strict_version(&unhex(&a.base_revision)?)?.encode();
                a.workspace_id = ws.to_storage();
                a.document_id = target.to_storage();
                a.base_revision = hex(&expected);
                let mut semantic = serde_json::to_value(&a)
                    .map_err(|_| ToolError::invalid("invalid edit request"))?;
                normalize_request_uuids(&mut semantic);
                let vector = strict_version(&expected)?;
                let mut entries = vector
                    .iter()
                    .map(|(peer, counter)| (*peer, *counter))
                    .collect::<Vec<_>>();
                entries.sort_unstable();
                semantic["base_revision"] = serde_json::to_value(entries)
                    .map_err(|_| ToolError::invalid("invalid revision"))?;
                let fingerprint = koru_store_core::checksum(
                    &serde_json::to_vec(&semantic)
                        .map_err(|_| ToolError::invalid("invalid edit request"))?,
                );
                Ok(PreparedNativeWrite {
                    description: NativeWriteDescription {
                        kind: NativeWriteKind::Edited,
                        workspace: ws,
                        document: Some(target),
                        fingerprint,
                        expected_revision: Some(expected),
                        title: String::new(),
                    },
                    request: Request::Edit(a),
                })
            }
            _ => Err(ToolError::invalid("unsupported native write")),
        }
    }

    fn native_store(&self) -> Result<&dyn EmbeddedStore, ToolError> {
        self.embedded.as_deref().ok_or_else(unavailable)
    }
    fn native_page(
        &self,
        ws: StoreWorkspaceId,
        id: StoreDocumentId,
    ) -> Result<DocumentEntry, ToolError> {
        self.native_store()?
            .document_entry(ws, id)?
            .filter(|entry| entry.id == id && entry.kind == "page" && entry.trashed_at.is_none())
            .ok_or_else(|| ToolError::permission_denied("the target is not a current page"))
    }

    fn native_context(&self, ws: StoreWorkspaceId) -> Result<InlineContext, ToolError> {
        let workspace = self.native_store()?.workspace(ws)?;
        if workspace.id != ws {
            return Err(conflict());
        }
        Ok(InlineContext {
            profile: CoreProfileId::from_uuid(workspace.owner.as_uuid()),
            workspace: CoreWorkspaceId::from_uuid(ws.as_uuid()),
            documents: HashMap::new(),
            files: HashMap::new(),
            attrs: AttrContext::default(),
        })
    }

    /// The caller has already acquired the exact effect lease. No callback to
    /// its policy owner occurs here. Success is Store evidence, not Run Saved.
    pub fn execute_native_write(
        &self,
        prepared: &PreparedNativeWrite,
        identity: NativeStoreIdentity,
    ) -> Result<NativeWriteEvidence, NativeWriteError> {
        validate_identity(&identity)?;
        let store = self.native_store()?;
        let ws = prepared.description.workspace;
        match &prepared.request {
            Request::Create(request) => {
                let mut a = request.clone();
                local_ids::resolve_seed(&mut a.root_node_id, a.initial_operations.as_deref_mut())?;
                let root_id = a
                    .root_node_id
                    .as_deref()
                    .map(node)
                    .transpose()?
                    .unwrap_or_default();
                let context = self.native_context(ws)?;
                let mut doc = WorkspaceDoc::create_on(
                    loro::LoroDoc::new(),
                    CoreWorkspaceId::from_uuid(ws.as_uuid()),
                    &a.title,
                )?;
                let mut value = Node::document(&a.title)?;
                value.id = root_id;
                let root = doc.insert(Parent::Root, value)?;
                if let Some(ops) = a.initial_operations.as_deref() {
                    apply_initial_ops(&mut doc, root, ops, &context)?;
                } else {
                    doc.insert(Parent::Node(root), Node::paragraph("")?)?;
                }
                let result = store.seed_document_with_origin(
                    EmbeddedSeedDocument {
                        workspace: ws,
                        title: a.title.clone(),
                        kind: "page".into(),
                        parent: None,
                        index: None,
                        idempotency_key: identity.request_key.clone(),
                        request_hash: prepared.description.fingerprint.clone(),
                        initial_update: doc.export_all_updates()?,
                        attribution: None,
                    },
                    identity.clone(),
                )?;
                if result.creation.document != identity.candidate
                    || !origin_matches(&result.origin, &identity, &prepared.description.fingerprint)
                {
                    return Err(NativeWriteError::Unconfirmed(conflict()));
                }
                self.native_page(ws, identity.candidate)
                    .map_err(NativeWriteError::Unconfirmed)?;
                let retained = store
                    .retain_creation_version(
                        ws,
                        identity.clone(),
                        prepared.description.fingerprint.clone(),
                    )
                    .map_err(NativeWriteError::Unconfirmed)?;
                evidence(
                    prepared,
                    identity.candidate,
                    retained,
                    &result.origin.initial_update,
                )
                .map_err(NativeWriteError::Unconfirmed)
            }
            Request::Edit(_) | Request::Memory(_) => {
                let id = prepared.description.document.ok_or_else(conflict)?;
                if identity.candidate != id {
                    return Err(conflict().into());
                }
                let page = self.native_page(ws, id)?;
                let title = bounded_title(&page.title);
                if title.trim().is_empty() {
                    return Err(ToolError::invalid("the page has no bounded result title").into());
                }
                let snapshot = store.snapshot(ws, id)?;
                if matches!(&prepared.request, Request::Memory(_)) {
                    memory::validate_snapshot(&snapshot)?;
                }
                let mut doc = WorkspaceDoc::from_snapshot(&snapshot)?;
                let expected = prepared
                    .description
                    .expected_revision
                    .as_ref()
                    .ok_or_else(conflict)?;
                if doc.version() != strict_version(expected)? {
                    return Err(conflict().into());
                }
                let before = doc.version();
                match &prepared.request {
                    Request::Edit(request) => {
                        let mut ops = request.operations.clone();
                        local_ids::resolve_edit(&mut ops)?;
                        let context = self.native_context(ws)?;
                        let _ = apply_ops(&mut doc, &ops, &context)?;
                    }
                    Request::Memory(request) => request.apply(self, ws, &mut doc)?,
                    Request::Create(_) => unreachable!(),
                }
                if doc.version() == before {
                    return Err(NativeWriteError::Refused(ToolError {
                        code: "native_no_change".into(),
                        message: "the edit changes no document state".into(),
                        details: None,
                    }));
                }
                let retained = store.persist_update_at_version(
                    ws,
                    id,
                    expected.clone(),
                    doc.export_updates_since(&before)?,
                    identity,
                    fingerprint_bytes(&prepared.description.fingerprint)?,
                )?;
                if retained.document != id
                    || strict_version(&retained.version_vector)
                        .map_err(NativeWriteError::Unconfirmed)?
                        != doc.version()
                {
                    return Err(NativeWriteError::Unconfirmed(conflict()));
                }
                Ok(NativeWriteEvidence {
                    kind: NativeWriteKind::Edited,
                    workspace: ws,
                    document: id,
                    version: retained.version_vector,
                    title,
                    confirmed_at_ms: retained.captured_at.millis(),
                    version_id: Some(retained.id),
                })
            }
        }
    }

    /// Read original proof and retain only that immutable history state. Never
    /// resubmit page bytes or repair a missing manifest after an uncertain call.
    pub fn reconcile_native_write(
        &self,
        prepared: &PreparedNativeWrite,
        identity: NativeStoreIdentity,
    ) -> Result<NativeReconciliation, ToolError> {
        validate_identity(&identity)?;
        if prepared.description.kind == NativeWriteKind::Edited {
            return Ok(NativeReconciliation::Unconfirmed);
        }
        let ws = prepared.description.workspace;
        let store = self.native_store()?;
        let CreationOriginLookup::Known(origin) = store.creation_origin(ws, identity.candidate)?
        else {
            return Ok(NativeReconciliation::Unconfirmed);
        };
        if !origin_matches(&origin, &identity, &prepared.description.fingerprint) {
            return Err(conflict());
        }
        if !store
            .document_entry(ws, identity.candidate)?
            .is_some_and(|entry| entry.kind == "page" && entry.trashed_at.is_none())
        {
            return Ok(NativeReconciliation::Unconfirmed);
        }
        let retained = store.retain_creation_version(
            ws,
            identity.clone(),
            prepared.description.fingerprint.clone(),
        )?;
        Ok(NativeReconciliation::Verified(evidence(
            prepared,
            identity.candidate,
            retained,
            &origin.initial_update,
        )?))
    }
}
fn evidence(
    prepared: &PreparedNativeWrite,
    id: StoreDocumentId,
    version: koru_store_core::ports::DocumentVersion,
    original: &[u8],
) -> Result<NativeWriteEvidence, ToolError> {
    let doc = WorkspaceDoc::from_snapshot(original)?;
    if version.document != id || strict_version(&version.version_vector)? != doc.version() {
        return Err(conflict());
    }
    Ok(NativeWriteEvidence {
        kind: NativeWriteKind::Created,
        workspace: prepared.description.workspace,
        document: id,
        version: version.version_vector,
        title: prepared.description.title.clone(),
        confirmed_at_ms: version.captured_at.millis(),
        version_id: Some(version.id),
    })
}

/// A page at one immutable Store version, rendered exactly as `read_document`
/// renders the live page: the same typed-node tree, from the version's own
/// snapshot. The caller has read the bytes by version id under its own read
/// authority and verified their checksum; nothing here reads a Store, and the
/// live page is never consulted. Bytes that do not open as a page are refused.
pub fn render_pinned_document(
    document: StoreDocumentId,
    snapshot: &[u8],
) -> Result<Value, ToolError> {
    let doc = WorkspaceDoc::from_snapshot(snapshot)?;
    crate::document_json(document, &doc)
}

#[cfg(test)]
mod tests;

fn bounded_title(value: &str) -> String {
    let mut end = value.len().min(256);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

/// The shared four-tool catalog narrowed before compaction, so discovery never
/// advertises fields/operations this native semantic owner refuses. Transport
/// owners may add their operation correlation field; they do not own this list.
pub fn native_tool_catalog() -> Vec<WorkspaceTool> {
    let mut tools = workspace_tool_catalog()
        .into_iter()
        .filter(|tool| {
            matches!(
                tool.name.as_str(),
                "search_documents" | "read_document" | "create_document" | "edit_document"
            )
        })
        .collect::<Vec<_>>();
    for tool in &mut tools {
        let create = tool.name == "create_document";
        if create || tool.name == "edit_document" {
            let mut schema = if create {
                serde_json::to_value(schemars::schema_for!(CreateDocumentRequest)).expect("schema")
            } else {
                serde_json::to_value(schemars::schema_for!(EditDocumentRequest)).expect("schema")
            };
            narrow_schema(&mut schema, create);
            if create {
                let fields = schema["properties"]
                    .as_object_mut()
                    .expect("create properties");
                fields.remove("parent_document_id");
                fields.remove("idempotency_key");
                fields.insert("kind".into(), json!({"type":"string","const":"page"}));
                tool.description="Create a new root page with text, structure and formatting. The owner supplies retry identity. Unsupported secondary effects are refused.".into();
            } else {
                tool.description="Edit one current page at its exact read revision using text, structure and formatting operations. Concurrent revision changes and secondary effects are refused.".into();
            }
            tool.input_schema = Value::Object(published_schema::compact_input_schema(
                schema.as_object().expect("schema object"),
            ));
        }
    }
    tools
}
fn narrow_schema(schema: &mut Value, initial: bool) {
    match schema {
        Value::Object(map) => {
            if let Some(Value::Array(branches)) = map.get_mut("oneOf") {
                branches.retain(|branch| {
                    branch
                        .pointer("/properties/op/const")
                        .and_then(Value::as_str)
                        .is_none_or(|op| {
                            if initial {
                                matches!(
                                    op,
                                    "insert"
                                        | "set_language"
                                        | "set_align"
                                        | "set_mark"
                                        | "set_callout_look"
                                )
                            } else {
                                matches!(
                                    op,
                                    "replace_text"
                                        | "insert"
                                        | "move"
                                        | "remove"
                                        | "set_language"
                                        | "set_align"
                                        | "set_mark"
                                        | "set_heading"
                                        | "set_highlight"
                                        | "set_callout_look"
                                )
                            }
                        })
                });
            }
            if let Some(Value::Array(values)) = map.get_mut("enum") {
                values.retain(|value| value.as_str() != Some("task"));
            }
            for value in map.values_mut() {
                narrow_schema(value, initial);
            }
        }
        Value::Array(values) => {
            for value in values {
                narrow_schema(value, initial);
            }
        }
        _ => {}
    }
}
