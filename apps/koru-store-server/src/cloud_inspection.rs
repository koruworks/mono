// SPDX-License-Identifier: MPL-2.0

//! Native inspection/edit of an exact existing Cloud copy. No Store, manifest,
//! global location, blob hydration, or normal editor handle participates here.
use koru_core::schema::file_references::{ReferenceBudget, inspect_file_references};
use koru_core::{Kind, NodeId, Parent, WorkspaceDoc};
use koru_device::cloud::{CloudId, CloudReplicaPrincipalV1};
use koru_network_iroh::cloud::client_io::inspection::{EditError, InspectionCarrier};
use koru_replication::cloud::transfer::{Error, Scope, Unit};
use koru_store_core::DocumentId;
use koru_store_wire::cloud_document::*;
use koru_store_wire::cloud_document::{
    BlobReference as ViewBlob, BlockProjection as ViewBlock,
    BlockProperties as ViewBlockProperties, EntityReference as ViewReference,
    InlineObject as ViewInlineObject, RichText as ViewRichText, TextMark as ViewMark,
};
use std::collections::BTreeSet;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncWrite};
const RUN_PILL_ROLE: &str = "koru.run";

pub use koru_store_wire::cloud_document::CloudDocumentOutcome as InspectionResult;

/// Called only after the platform admitted the exact current account/session
/// and the idle reconciler transferred its carrier. The platform retains its
/// one-slot admission through this job and rechecks its account/permission fence
/// before delivering any result. `retention` owns its admission permit and is
/// moved into blocking work, so even caller task abortion cannot release the
/// slot while replay continues. `can_write` is the current host permission,
/// never inferred from inventory metadata. A removal result is a remote durable Ack,
/// never bytes-freed evidence; refresh authoritative inventory and usage.
pub async fn run<'a, R, W, T>(
    carrier: InspectionCarrier<'a, R, W>,
    scope: Scope,
    current_account: CloudId,
    selection: CloudDocumentSelection,
    action: CloudDocumentAction,
    at_ms: i64,
    can_write: bool,
    retention: Arc<T>,
) -> Result<InspectionResult, CloudDocumentFailure>
where
    R: AsyncRead + Unpin + Send + 'a,
    W: AsyncWrite + Unpin + Send + 'a,
    T: Send + Sync + 'static,
{
    let (unit, head) = target(scope, current_account, &selection, &action)?;
    let blocking_retention = Arc::clone(&retention);
    let _retention = retention;
    match action {
        CloudDocumentAction::Inspect => {
            let source = carrier
                .download(unit)
                .await
                .map_err(|_| CloudDocumentFailure::Unavailable)?;
            if source.observed().head() != head {
                return Err(CloudDocumentFailure::SourceChanged);
            }
            tokio::task::spawn_blocking(move || {
                let _retention = blocking_retention;
                let doc = source
                    .replay()
                    .map_err(|_| CloudDocumentFailure::Unavailable)?;
                if !source
                    .observed()
                    .matches(scope, unit, head, &doc.oplog_vv().encode())
                {
                    return Err(CloudDocumentFailure::Unavailable);
                }
                let mut view = project(doc, selection)?;
                view.can_request_removal &= can_write;
                if !view.can_request_removal {
                    view.removable_files.clear();
                }
                Ok(InspectionResult::View(view))
            })
            .await
            .map_err(|_| CloudDocumentFailure::Unavailable)?
        }
        CloudDocumentAction::RemoveFile { node_id } => {
            if !can_write {
                return Err(CloudDocumentFailure::Unavailable);
            }
            carrier
                .edit(unit, head, move |source| {
                    let _retention = blocking_retention;
                    let doc = source.replay()?;
                    if !source
                        .observed()
                        .matches(scope, unit, head, &doc.oplog_vv().encode())
                    {
                        return Err(Error::Invalid);
                    }
                    let delta = remove_file(doc, &selection, &node_id, at_ms)
                        .map_err(|_| Error::Invalid)?;
                    Ok((delta, source))
                })
                .await
                .map_err(|error| match error {
                    EditError::SourceChanged => CloudDocumentFailure::SourceChanged,
                    EditError::Unavailable => CloudDocumentFailure::Unavailable,
                    EditError::OutcomeUnknown => CloudDocumentFailure::OutcomeUnknown,
                })?;
            Ok(InspectionResult::Removed)
        }
    }
}
fn target(
    scope: Scope,
    account: CloudId,
    selection: &CloudDocumentSelection,
    action: &CloudDocumentAction,
) -> Result<(Unit, [u8; 32]), CloudDocumentFailure> {
    let bad = CloudDocumentFailure::Unavailable;
    if !selection.validate() || !action.validate() || selection.account_id != account.to_string() {
        return Err(bad);
    }
    let CloudReplicaPrincipalV1::Workspace {
        workspace_id,
        incarnation_id,
        ..
    } = scope.principal
    else {
        return Err(bad);
    };
    if selection.workspace_id != workspace_id.to_string()
        || selection.incarnation_id != incarnation_id.to_string()
        || selection.workspace_id != scope.eligibility_workspace.to_storage()
        || selection.document_id == scope.manifest.to_storage()
    {
        return Err(bad);
    }
    let document = uuid::Uuid::parse_str(&selection.document_id).map_err(|_| bad)?;
    let unit = Unit::Document {
        workspace_id,
        document_id: CloudId::from_bytes(document.into_bytes()),
    };
    scope.object_unit(unit).map_err(|_| bad)?;
    let mut head = [0; 32];
    for (i, byte) in head.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&selection.source_head_digest[i * 2..i * 2 + 2], 16)
            .map_err(|_| bad)?;
    }
    Ok((unit, head))
}
fn budget() -> ReferenceBudget {
    ReferenceBudget {
        nodes: MAX_INSPECTION_BLOCKS,
        fields: 40_000,
        depth: 256,
        value_bytes: MAX_INSPECTION_BYTES,
    }
}
fn models(
    doc: loro::LoroDoc,
    selection: &CloudDocumentSelection,
) -> Result<Vec<WorkspaceDoc>, CloudDocumentFailure> {
    let bad = CloudDocumentFailure::Unavailable;
    let proof = inspect_file_references(&doc, budget()).map_err(|_| bad)?;
    for hash in proof.blobs() {
        koru_store_core::model::BlobHash::parse(hash).map_err(|_| bad)?;
    }
    let meta = doc.get_map("meta");
    for (key, expected) in [
        ("id", selection.workspace_id.as_str()),
        ("document", selection.document_id.as_str()),
    ] {
        if let Some(value) = meta.get(key) {
            if !matches!(value.as_value(),Some(loro::LoroValue::String(actual)) if actual.as_str()==expected)
            {
                return Err(bad);
            }
        }
    }
    let expected = proof.node_ids().to_vec();
    let mut models = Vec::new();
    let mut actual = Vec::new();
    for tree in koru_core::schema::DocumentTree::ALL {
        if !tree.exists_in(&doc) {
            continue;
        }
        let model =
            WorkspaceDoc::adopt_detached_tree(doc.clone(), tree, budget()).map_err(|_| bad)?;
        let (walk, repeats) = model.walk_with_trash_checked().map_err(|_| bad)?;
        if !repeats.is_empty() {
            return Err(bad);
        }
        for (id, _) in walk {
            actual.push(id);
            let node = model.read(id).map_err(|_| bad)?;
            koru_core::Node::new(node.kind, node.content.clone()).map_err(|_| bad)?;
        }
        models.push(model);
    }
    actual.sort_unstable();
    if actual != expected {
        return Err(bad);
    }
    Ok(models)
}

fn hashes(model: &WorkspaceDoc) -> Result<BTreeSet<String>, CloudDocumentFailure> {
    let proof = inspect_file_references(model.doc(), budget())
        .map_err(|_| CloudDocumentFailure::Unavailable)?;
    proof
        .blobs()
        .iter()
        .map(|raw| {
            koru_store_core::model::BlobHash::parse(raw)
                .map(|h| h.to_string())
                .map_err(|_| CloudDocumentFailure::Unavailable)
        })
        .collect()
}
fn eligible(model: &WorkspaceDoc, id: NodeId) -> bool {
    model.read(id).is_ok_and(|node| {
        matches!(node.kind, Kind::File | Kind::Image | Kind::Canvas)
            && matches!(node.content, koru_core::Content::Blob { .. })
    }) && model
        .children(Parent::Node(id))
        .is_ok_and(|children| children.is_empty())
}
fn remove_file(
    doc: loro::LoroDoc,
    selection: &CloudDocumentSelection,
    node: &str,
    at_ms: i64,
) -> Result<Vec<u8>, CloudDocumentFailure> {
    let bad = CloudDocumentFailure::Unavailable;
    let target = NodeId::parse(node).ok_or(bad)?;
    let mut model = models(doc, selection)?
        .into_iter()
        .find(|model| model.read(target).is_ok())
        .ok_or(bad)?;
    if !eligible(&model, target) {
        return Err(bad);
    }
    let before = model.version();
    let before_refs = hashes(&model)?;
    if model.in_trash(target) && !model.is_trashed(target) {
        // Only this strict-validated leaf moves under its existing bin. The
        // containing note, its descendants, and its recovery stamps stay put.
        let trash = model
            .ancestors(target)
            .map_err(|_| bad)?
            .into_iter()
            .find(|id| model.kind_of(*id) == Some(Kind::Trash))
            .ok_or(bad)?;
        let end = model.children(Parent::Node(trash)).map_err(|_| bad)?.len();
        model
            .move_node(target, Parent::Node(trash), end)
            .map_err(|_| bad)?;
    } else if !model.is_trashed(target) {
        model.trash(target, at_ms, None).map_err(|_| bad)?;
    }
    model.delete_permanently(target).map_err(|_| bad)?;
    let after_refs = hashes(&model)?;
    if !after_refs.is_subset(&before_refs) {
        return Err(bad);
    }
    model.export_updates_since(&before).map_err(|_| bad)
}
fn project(
    doc: loro::LoroDoc,
    selection: CloudDocumentSelection,
) -> Result<CloudDocumentInspectionV1, CloudDocumentFailure> {
    let bad = CloudDocumentFailure::Unavailable;
    let revision = revision_number(&doc.oplog_vv().encode());
    let models = models(doc, &selection)?;
    let document =
        DocumentId::from_uuid(uuid::Uuid::parse_str(&selection.document_id).map_err(|_| bad)?);
    let mut title = None;
    let mut blocks = Vec::new();
    let mut removable_files = Vec::new();
    for model in models {
        let order = model.walk_with_trash().map_err(|_| bad)?;
        let root = order
            .iter()
            .map(|(id, _)| *id)
            .find(|id| model.kind_of(*id) == Some(Kind::Document))
            .unwrap_or_else(|| NodeId::from_uuid(document.as_uuid()));
        if title.is_none() {
            title = model
                .read(root)
                .ok()
                .and_then(|n| n.content.as_text().map(str::to_owned))
                .filter(|s| !s.is_empty())
                .or_else(|| model.name());
        }
        for (id, _) in order {
            let node = model.read(id).map_err(|_| bad)?;
            let parent = match model.parent(id).map_err(|_| bad)? {
                Parent::Node(parent) => Some(parent.to_storage()),
                Parent::Root => None,
            };
            blocks.push(view_block(&model, &node, parent, root, document).map_err(|_| bad)?);
            if eligible(&model, id) {
                removable_files.push(FileRemovalControl {
                    node_id: id.to_storage(),
                    in_trash: model.in_trash(id),
                });
            }
        }
    }
    let value = CloudDocumentInspectionV1 {
        document: DocumentProjection {
            id: selection.document_id.clone(),
            workspace_id: selection.workspace_id.clone(),
            title: title.unwrap_or_default(),
            revision,
            blocks,
        },
        selection,
        can_request_removal: true,
        removable_files,
    };
    value.to_json().map_err(|_| bad)?;
    Ok(value)
}

fn view_block(
    model: &koru_core::WorkspaceDoc,
    node: &koru_core::Node,
    parent_id: Option<String>,
    root: koru_core::NodeId,
    document: DocumentId,
) -> Result<ViewBlock, String> {
    use koru_core::Content;

    let (rich_text, blob, reference, kind) = match &node.content {
        Content::Text { .. } => {
            let rich = Some(view_rich_text(model, node, root, document)?);
            (rich, None, None, node.kind.as_str().to_owned())
        }
        Content::Blob {
            hash,
            mime,
            width,
            height,
            ..
        } => (
            None,
            Some(ViewBlob {
                id: hash.0.clone(),
                mime_type: mime.clone(),
                size: None,
                width: *width,
                height: *height,
            }),
            None,
            node.kind.as_str().to_owned(),
        ),
        Content::Embed { target } => (
            None,
            None,
            Some(view_reference(*target, "embed", root, document)),
            "embed".to_owned(),
        ),
    };

    let meta = &node.meta;
    let alignment = match meta.align {
        Some(koru_core::Align::Center) => "center",
        Some(koru_core::Align::End) => "end",
        Some(koru_core::Align::Start) | None => "start",
    };
    let (image_width, image_height) = meta.image_size.unzip();
    Ok(ViewBlock {
        in_trash: model.in_trash(node.id),
        id: node.id.to_storage(),
        parent_id,
        kind,
        properties: ViewBlockProperties {
            name: meta.name.clone(),
            icon: meta.icon.clone(),
            color: meta.colour.clone(),
            done: meta.done,
            alignment: alignment.into(),
            heading_level: meta.heading.map(|heading| heading.level()),
            list_style: meta.list_style.map(|style| style.as_str().to_owned()),
            image_width,
            image_height,
            language: meta.language.clone(),
        },
        rich_text,
        blob,
        reference,
    })
}

fn view_rich_text(
    model: &koru_core::WorkspaceDoc,
    node: &koru_core::Node,
    root: koru_core::NodeId,
    document: DocumentId,
) -> Result<ViewRichText, String> {
    use koru_core::AttrValue;

    let runs = model
        .runs(node.id)
        .map_err(|_| "invalid_document_text".to_owned())?;
    let text = runs.iter().map(|run| run.text.as_str()).collect();
    let mut marks = Vec::new();
    let mut inline_objects = Vec::new();
    for run in &runs {
        let start = run.start.0;
        let end = run.end.0;
        let mut mark = |present: bool, kind: &'static str, value: Option<String>| {
            if present && start < end {
                marks.push(ViewMark {
                    start_utf8: start,
                    end_utf8: end,
                    kind: kind.to_owned(),
                    value,
                });
            }
        };
        mark(run.bold, "bold", None);
        mark(run.italic, "italic", None);
        mark(run.underline, "underline", None);
        mark(run.strike, "strike", None);
        mark(run.code, "code", None);
        mark(
            run.style.script.is_some(),
            match run.style.script.map(|script| script.as_str()) {
                Some("sup") => "superscript",
                _ => "subscript",
            },
            None,
        );
        mark(
            run.style.size.is_some(),
            "size",
            run.style.size.map(|size| size.to_string()),
        );
        mark(
            run.style.highlight.is_some(),
            "highlight",
            run.style.highlight.clone(),
        );

        let Some(attribute_id) = run.pill else {
            continue;
        };
        let Some(attribute) = node
            .content
            .attrs()
            .iter()
            .find(|attribute| attribute.id == attribute_id)
        else {
            continue;
        };
        let reference = match &attribute.value {
            AttrValue::Ref(target) => {
                view_reference(*target, attribute.role.as_str(), root, document)
            }
            // A stable document reference (ADR-127) names its target
            // directly — and under the run role it names a run (P1.18).
            AttrValue::Document(reference) if attribute.role == RUN_PILL_ROLE => {
                ViewReference::Run {
                    document_id: reference.document.to_storage(),
                }
            }
            AttrValue::Document(reference) => ViewReference::Document {
                document_id: reference.document.to_storage(),
                node_id: reference.node.map(|node| node.to_storage()),
            },
            AttrValue::Url(url) => ViewReference::Url { url: url.clone() },
            AttrValue::Date(date) => ViewReference::Label {
                value: date.to_string(),
            },
            AttrValue::Text(value) => ViewReference::Label {
                value: value.clone(),
            },
            // The temporal shapes (ADR-183); no pill carries one yet, and a
            // label is the honest projection when one does. A zoned value
            // names its zone rather than showing a bare number, which a reader
            // could not place.
            AttrValue::FloatingDateTime(wall) => ViewReference::Label {
                value: wall.to_storage(),
            },
            AttrValue::ZonedDateTime(zoned) => ViewReference::Label {
                value: format!("{} {}", zoned.instant_ms(), zoned.zone()),
            },
            AttrValue::Number(value) => ViewReference::Label {
                value: value.to_string(),
            },
            AttrValue::Bool(value) => ViewReference::Label {
                value: value.to_string(),
            },
        };
        inline_objects.push(ViewInlineObject {
            id: attribute.id.to_storage(),
            anchor_utf8: start,
            role: attribute.role.clone(),
            label: attribute.label.clone(),
            reference,
        });
    }
    Ok(ViewRichText {
        text,
        marks,
        inline_objects,
    })
}

fn view_reference(
    target: koru_core::Ref,
    role: &str,
    root: koru_core::NodeId,
    current_document: DocumentId,
) -> ViewReference {
    use koru_core::DocRef;

    if target.node == root {
        return ViewReference::Document {
            document_id: current_document.to_storage(),
            // A legacy `Ref` at the document root names the document itself,
            // so there is no block to open it at.
            node_id: None,
        };
    }
    let home = match target.home {
        Some(DocRef::Document(document)) => Some(document),
        Some(DocRef::DocumentAt { document, .. }) => Some(document),
        Some(DocRef::Profile(_) | DocRef::Workspace(_)) | None => None,
    };
    if let Some(document) = home.filter(|_| role == "document") {
        return ViewReference::Document {
            document_id: document.to_storage(),
            node_id: None,
        };
    }
    // A run pill reads back as a reference homed at the run (proposal §3.2,
    // F2): the home is the Run document, and the role says it is a run.
    if let Some(document) = home.filter(|_| role == RUN_PILL_ROLE) {
        return ViewReference::Run {
            document_id: document.to_storage(),
        };
    }
    ViewReference::Block {
        block_id: target.node.to_storage(),
        document_id: home.map(|document| document.to_storage()),
    }
}

fn revision_number(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100_0000_01b3)
    }) & i64::MAX as u64
}

#[cfg(test)]
mod tests;
