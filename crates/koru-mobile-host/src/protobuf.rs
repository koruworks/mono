// SPDX-License-Identifier: MPL-2.0

//! Generated-message mobile operation adapter. JNI and C decode/encode only.

#[cfg(test)]
mod account_tests;
#[cfg(test)]
mod cloud_progress_tests;

use std::path::PathBuf;
use std::sync::Arc;

use koru_contract::mobile_proto as wire;

use crate::run_view::{RunView, TimelineWindow};
use crate::{
    DevicesResult, DocumentView, InventoryResult, LinkResult, MobileHost, MobileStateEvent,
    PairingOfferResult, ViewReference, get_host,
};

fn host(context: Option<wire::HostContext>, operation: &str) -> Result<Arc<MobileHost>, String> {
    let context = context.ok_or_else(|| "missing_host_context".to_owned())?;
    let secrets: [u8; 64] = context
        .secrets
        .try_into()
        .map_err(|_| "invalid_device_seeds".to_owned())?;
    let host = get_host(
        PathBuf::from(context.root),
        secrets,
        &context.device_name,
        // Absent means a shim from before the field existed: the shipped
        // default, relays on (decision №6).
        context.relay_enabled.unwrap_or(true),
    )?;
    host.admit(operation)?;
    Ok(host)
}

fn account_host_error(code: String) -> String {
    match code.as_str() {
        "missing_host_context" | "invalid_device_seeds" | "operation_not_catalogued" => code,
        _ => "account_host_unavailable".into(),
    }
}

pub fn account_command(mut request: wire::AccountCommandRequest) -> wire::AccountCommandReply {
    let result = koru_contract::mobile_account::command(&request)
        .map_err(str::to_owned)
        .and_then(|_| {
            let host = host(request.host.take(), "account_command").map_err(account_host_error)?;
            let account = host
                .account
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
                .ok_or("account_trust_not_ready")?;
            account.command_owned(&request).map_err(str::to_owned)
        });
    wire::AccountCommandReply {
        requester: request.requester,
        generation: result.as_ref().map_or(0, |a| a.generation),
        owner_scope: result.as_ref().map_or(0, |a| a.owner_scope),
        error_code: result.err(),
    }
}

pub fn account_poll(request: wire::AccountPollRequest) -> wire::AccountPollReply {
    let result = koru_contract::mobile_account::envelope(request.version, request.requester)
        .map_err(str::to_owned)
        .and_then(|_| {
            let host = host(request.host, "account_poll").map_err(account_host_error)?;
            // The app polls while a person is looking at it, and that is the
            // phone's only regular tick: a device that never writes would
            // otherwise never ask the Cloud for another device's work.
            host.wake_cloud_periodically();
            let account = host
                .account
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            Ok(account
                .map(|account| {
                    account.poll(
                        request.requester,
                        request.after_sequence,
                        request.include_platform,
                    )
                })
                .unwrap_or_else(crate::account::unavailable))
        });
    result.unwrap_or_else(|code| wire::AccountPollReply {
        error_code: Some(code),
        ..Default::default()
    })
}

/// Shims call this after encoding the private platform response. Generated
/// DTOs deliberately have no persistence role and must not retain proof copies.
pub fn zeroize_account_poll(reply: &mut wire::AccountPollReply) {
    use zeroize::Zeroize;
    if let Some(prompt) = &mut reply.sign_in {
        prompt.nonce.zeroize();
    }
    if let Some(prompt) = &mut reply.vault {
        if let Some(credential) = &mut prompt.credential {
            credential.zeroize();
        }
    }
}

/// Decode into an owned value so a malformed trailing field still lets the
/// adapter wipe any proof decoded before the parser rejected the envelope.
pub fn decode_account_platform(bytes: &[u8]) -> Result<wire::AccountPlatformRequest, &'static str> {
    use prost::Message;
    let mut request = wire::AccountPlatformRequest::default();
    if bytes.len() > 96 * 1024 || request.merge(bytes).is_err() {
        crate::account::zeroize_response(&mut request);
        return Err("invalid_protobuf");
    }
    Ok(request)
}

pub fn account_platform(mut request: wire::AccountPlatformRequest) -> wire::MutationResult {
    let result = koru_contract::mobile_account::platform(&request)
        .map_err(str::to_owned)
        .and_then(|_| {
            let host = host(request.host.take(), "account_platform").map_err(account_host_error)?;
            let account = host
                .account
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
                .ok_or("account_trust_not_ready")?;
            // **Foreground wakes the peer pass too** (root-catalog Plan 2.2,
            // TECHNICAL.final §7.2, §9): the Cloud coordinator hears
            // `Foreground(true)` through the account; a peer connection that
            // survived the background gets no connect event, so the data
            // plane is asked for one pass with every live peer here. The
            // pass is coalesced and idempotent.
            let foregrounded = matches!(
                request.response,
                Some(wire::account_platform_request::Response::Foreground(true))
            );
            account.platform(&mut request).map_err(str::to_owned)?;
            if foregrounded {
                host.network().resync_peers();
            }
            Ok(())
        });
    crate::account::zeroize_response(&mut request);
    mutation(result, "accepted")
}

fn mutation(result: Result<(), String>, state: &str) -> wire::MutationResult {
    match result {
        Ok(()) => wire::MutationResult {
            ok: true,
            state: state.into(),
            error_code: None,
        },
        Err(code) => wire::MutationResult {
            ok: false,
            state: "failed".into(),
            error_code: Some(code),
        },
    }
}

fn link(value: LinkResult<'_>) -> wire::LinkDeviceResult {
    wire::LinkDeviceResult {
        state: value.state.into(),
        peer_name: value.peer_name,
        error_code: value.error_code,
    }
}

pub fn status(request: wire::HostStatusRequest) -> wire::HostStatus {
    let value = host(request.host, "status")
        .map(|host| host.status())
        .unwrap_or_else(crate::failed_owned);
    wire::HostStatus {
        state: value.state.into(),
        peer_name: value.peer_name,
        error_code: value.error_code,
    }
}

pub fn link_device(request: wire::LinkDeviceRequest) -> wire::LinkDeviceResult {
    link(
        host(request.host, "link_device")
            .map(|host| host.link(&request.code))
            .unwrap_or_else(crate::failed_owned),
    )
}

pub fn advertise_build(request: wire::BuildPayload) -> wire::MutationResult {
    let payload = koru_device::BuildPayload {
        app_version: request.app_version,
        channel: request.channel,
        branch: request.branch,
        commit: request.commit,
        os: request.os,
        os_version: request.os_version,
        peer_protocol: None,
    };
    mutation(
        host(request.host, "advertise_build")
            .and_then(|host| host.advertise_build_payload(payload)),
        "advertised",
    )
}

pub fn inventory(request: wire::InventoryRequest) -> wire::Inventory {
    match host(request.host, "store.inventory").and_then(|host| host.inventory()) {
        Ok(value) => inventory_value(value),
        Err(code) => wire::Inventory {
            error_code: Some(code),
            ..Default::default()
        },
    }
}

fn inventory_value(value: InventoryResult) -> wire::Inventory {
    wire::Inventory {
        profiles: value
            .profiles
            .into_iter()
            .map(|r| wire::InventoryProfile {
                id: r.id,
                name: r.name,
                locale: r.locale,
                icon: r.icon,
                leading_emoji: r.leading_emoji,
                display_name: r.display_name,
                color: r.color,
                account_binding: r.account_binding,
            })
            .collect(),
        workspaces: value
            .workspaces
            .into_iter()
            .map(|r| wire::InventoryWorkspace {
                id: r.id,
                profile_id: r.profile_id,
                name: r.name,
                icon: r.icon,
                color: r.color,
                leading_emoji: r.leading_emoji,
                display_name: r.display_name,
                archived: r.archived,
                deleted_at_millis: r.deleted_at_millis,
                modified_at_millis: r.modified_at_millis,
            })
            .collect(),
        documents: value
            .documents
            .into_iter()
            .map(|r| wire::InventoryDocument {
                id: r.id,
                workspace_id: r.workspace_id,
                title: r.title,
                display_title: r.display_title,
                leading_emoji: r.leading_emoji,
                kind: r.kind,
                mime: r.mime,
                note_id: r.note_id,
                updated_at_millis: r.updated_at_millis,
            })
            .collect(),
        deleted_workspaces: value.deleted_workspaces.min(u32::MAX as usize) as u32,
        trashed_documents: value.trashed_documents.min(u32::MAX as usize) as u32,
        failed_workspaces: value.failed_workspaces.min(u32::MAX as usize) as u32,
        hidden_runs: value.hidden_runs.min(u32::MAX as usize) as u32,
        error_code: value.error_code,
    }
}

pub fn document(request: wire::DocumentRequest) -> wire::DocumentProjection {
    match host(request.host, "store.document")
        .and_then(|host| host.view_document(&request.document_id))
    {
        Ok(value) => document_value(value),
        Err(code) => wire::DocumentProjection {
            error_code: Some(code),
            ..Default::default()
        },
    }
}

/// A platform observer uses the same document admission and generated request
/// as a read. Its callback carries no data and must only enqueue a UI wake.
/// Subscribe before the first read, including for a referenced run whose bytes
/// have not arrived yet. Dropping the returned owner removes the subscription.
pub fn observe_document(
    request: wire::DocumentRequest,
    notify: impl Fn() -> bool + Send + 'static,
) -> Result<crate::document_observer::DocumentObserver, String> {
    let host = host(request.host, "store.document")?;
    let document = koru_store_core::DocumentId::parse(&request.document_id)
        .ok_or_else(|| "invalid_document_id".to_owned())?;
    let runtime = host.tokio();
    let watch = runtime
        .block_on(
            host.store
                .watch_cloud(koru_store_api::cloud_changes::Dataset::DocumentView(
                    document,
                )),
        )
        .map_err(|_| "document_observer_unavailable".to_owned())?;
    // Metadata lives in the manifest, not the body's revision. Watching only
    // the exact document would leave a remotely trashed open document visible.
    // Missing run references still keep their exact-ID subscription so arrival
    // triggers a read; no provisional document or location is manufactured.
    let manifest = host.with_store(move |store| {
        let workspace = store
            .locate(document)
            .map_err(|error| error.code().as_str().to_owned())?;
        workspace
            .map(|workspace| {
                store
                    .manifest_id(workspace)
                    .map_err(|error| error.code().as_str().to_owned())
            })
            .transpose()
    })?;
    let manifest = manifest
        .map(|manifest| {
            runtime
                .block_on(host.store.watch_cloud(
                    koru_store_api::cloud_changes::Dataset::DocumentView(manifest),
                ))
                .map_err(|_| "document_observer_unavailable".to_owned())
        })
        .transpose()?;
    Ok(crate::document_observer::DocumentObserver::start(
        &runtime, watch, manifest, notify,
    ))
}

fn document_value(value: DocumentView) -> wire::DocumentProjection {
    wire::DocumentProjection {
        id: value.id,
        workspace_id: value.workspace_id,
        title: value.title,
        revision: value.revision,
        blocks: value
            .blocks
            .into_iter()
            .map(|b| wire::Block {
                id: b.id,
                parent_id: b.parent_id,
                kind: b.kind,
                properties: Some(wire::BlockProperties {
                    name: b.properties.name,
                    icon: b.properties.icon,
                    color: b.properties.color,
                    done: b.properties.done,
                    alignment: b.properties.alignment.into(),
                    heading_level: b.properties.heading_level.map(u32::from),
                    list_style: b.properties.list_style.map(str::to_owned),
                    image_width: b.properties.image_width,
                    image_height: b.properties.image_height,
                    language: b.properties.language,
                }),
                rich_text: b.rich_text.map(|r| wire::RichText {
                    text: r.text,
                    marks: r
                        .marks
                        .into_iter()
                        .map(|m| wire::TextMark {
                            start_utf8: m.start_utf8.min(u32::MAX as usize) as u32,
                            end_utf8: m.end_utf8.min(u32::MAX as usize) as u32,
                            kind: m.kind.into(),
                            value: m.value,
                        })
                        .collect(),
                    inline_objects: r
                        .inline_objects
                        .into_iter()
                        .map(|o| wire::InlineObject {
                            id: o.id,
                            anchor_utf8: o.anchor_utf8.min(u32::MAX as usize) as u32,
                            role: o.role,
                            label: o.label,
                            reference: Some(reference(o.reference)),
                        })
                        .collect(),
                }),
                blob: b.blob.map(|v| wire::BlobReference {
                    id: v.id,
                    mime_type: v.mime_type,
                    size: v.size,
                    width: v.width,
                    height: v.height,
                }),
                reference: b.reference.map(reference),
            })
            .collect(),
        error_code: None,
    }
}

fn reference(value: ViewReference) -> wire::EntityReference {
    let mut r = wire::EntityReference::default();
    match value {
        ViewReference::Document {
            document_id,
            node_id,
        } => {
            r.kind = "document".into();
            r.document_id = Some(document_id);
            r.node_id = node_id;
        }
        ViewReference::Block {
            block_id,
            document_id,
        } => {
            r.kind = "block".into();
            r.block_id = Some(block_id);
            r.document_id = document_id;
        }
        ViewReference::Url { url } => {
            r.kind = "url".into();
            r.url = Some(url);
        }
        ViewReference::Label { value } => {
            r.kind = "label".into();
            r.value = Some(value);
        }
        ViewReference::Run { document_id } => {
            r.kind = "run".into();
            r.document_id = Some(document_id);
        }
    }
    r
}

pub fn run(request: wire::RunRequest) -> wire::RunView {
    match host(request.host, "store.run").and_then(|host| host.view_run(&request.document_id)) {
        Ok(value) => run_value(value),
        Err(code) => wire::RunView {
            error_code: Some(code),
            ..Default::default()
        },
    }
}

fn run_value(value: RunView) -> wire::RunView {
    let economics = value.economics;
    wire::RunView {
        document_id: value.document_id,
        workspace_id: value.workspace_id,
        run_id: value.run_id,
        revision: value.revision,
        status: value.status,
        attention: value.attention,
        control: value.control,
        failure_stage: value.failure_stage,
        prior_attempt: value.prior_attempt,
        agent: value.agent,
        objective: value.objective,
        scope_kind: value.scope_kind,
        context_count: value.context_count,
        executing_device_id: value.executing_device_id,
        executing_device_name: value.executing_device_name,
        executing_here: value.executing_here,
        created_at_millis: value.created_at_millis,
        results: value
            .results
            .into_iter()
            .map(|r| wire::RunResult {
                artifact_id: r.artifact_id,
                held_by_device_id: r.held_by_device_id,
                held_by_device_name: r.held_by_device_name,
                held_here: r.held_here,
                title: r.title,
                size_bytes: r.size_bytes,
                registered_at_millis: r.registered_at_millis,
            })
            .collect(),
        economics: Some(wire::RunEconomics {
            orchestrator_turns: economics.orchestrator_turns,
            human_turns: economics.human_turns,
            turns_with_usage: economics.turns_with_usage,
            input_tokens: economics.input_tokens,
            input_unavailable_turns: economics.input_unavailable_turns,
            output_tokens: economics.output_tokens,
            output_unavailable_turns: economics.output_unavailable_turns,
            cached_input_tokens: economics.cached_input_tokens,
            cached_input_unavailable_turns: economics.cached_input_unavailable_turns,
            tool_calls: economics.tool_calls,
            interventions: economics.interventions,
            elapsed_ms: economics.elapsed_ms,
            models: economics.models,
            complete: economics.complete,
        }),
        rating: value.rating,
        rating_note: value.rating_note,
        rated_at_millis: value.rated_at_millis,
        timeline_len: value.timeline_len,
        needs_newer_koru: value.needs_newer_koru,
        error_code: None,
    }
}

pub fn run_timeline(request: wire::RunTimelineRequest) -> wire::RunTimelineWindow {
    match host(request.host, "store.run_timeline").and_then(|host| {
        host.run_timeline_window(
            &request.document_id,
            request.newest_first_from as usize,
            request.len as usize,
        )
    }) {
        Ok(value) => window_value(value),
        Err(code) => wire::RunTimelineWindow {
            error_code: Some(code),
            ..Default::default()
        },
    }
}

fn window_value(value: TimelineWindow) -> wire::RunTimelineWindow {
    wire::RunTimelineWindow {
        document_id: value.document_id,
        newest_first_from: value.newest_first_from,
        rows: value
            .rows
            .into_iter()
            .map(|row| wire::RunTimelineRow {
                kind: row.kind.into(),
                id: row.id,
                at_millis: row.at_millis,
                step: row.step,
                originator: row.originator,
                calls: row.calls,
                failed: row.failed,
                outcome: row.outcome,
                artifacts_registered: row.artifacts_registered,
                reason: row.reason,
                stage: row.stage,
                prior_attempt: row.prior_attempt,
                via: row.via,
                discovered: row.discovered,
                rating: row.rating,
                events_lost: row.events_lost,
                refs: row.refs,
                input_tokens: row.input_tokens,
                output_tokens: row.output_tokens,
                cached_input_tokens: row.cached_input_tokens,
                revision: row.revision,
                completeness: row.completeness,
            })
            .collect(),
        timeline_len: value.timeline_len,
        revision: value.revision,
        error_code: None,
    }
}

pub fn blob(request: wire::BlobRequest) -> wire::BlobBytes {
    match host(request.host, "store.blob")
        .and_then(|host| host.load_blob(&request.workspace_id, &request.hash))
    {
        Ok(value) => wire::BlobBytes {
            value,
            error_code: None,
        },
        Err(code) => wire::BlobBytes {
            value: Vec::new(),
            error_code: Some(code),
        },
    }
}
/// `cloud-blob-object-v1` §10's three methods, plus the two a phone needs
/// around them: WorkManager's recovery entry point and the §11 announcement
/// Android has to be asked for because it has no room registry.
///
/// **Argument marshalling only.** Every one of these is a parse and a call;
/// the decisions — what a pause means, what an explicit request overrides,
/// whether a download fits — are the engine's, on both platforms.
pub fn blob_status(request: wire::BlobStatusRequest) -> wire::BlobTransferStatus {
    host(request.host, "store.blob_status")
        .and_then(|host| host.blob_status(&request.workspace_id))
        .unwrap_or_else(|code| wire::BlobTransferStatus {
            workspace_id: request.workspace_id,
            // A host that could not answer says so in the one field that
            // means "what is this waiting on", rather than reporting zero
            // counts as though it had looked.
            pending_reason: code,
            ..wire::BlobTransferStatus::default()
        })
}

/// One Workspace's Cloud sync state, for the phone's Cloud sync progress
/// screen. Marshalling only: the host reads the same Store services the
/// desktop's `cloud-sync` route does.
pub fn cloud_sync_status(request: wire::CloudSyncStatusRequest) -> wire::CloudSyncStatus {
    match host(request.host, "store.cloud_sync_status")
        .and_then(|host| host.cloud_sync_status(&request.workspace_id))
    {
        Ok(status) => cloud_sync_message(&request.workspace_id, &status),
        Err(code) => wire::CloudSyncStatus {
            workspace_id: request.workspace_id,
            error_code: Some(code),
            ..wire::CloudSyncStatus::default()
        },
    }
}

/// The Store's report as the contract's message: the fields a progress row
/// reads, one for one, and nothing that names content.
pub(crate) fn cloud_sync_message(
    workspace_id: &str,
    status: &koru_store_wire::internal::CloudSyncStatus,
) -> wire::CloudSyncStatus {
    wire::CloudSyncStatus {
        workspace_id: status
            .workspace
            .map_or_else(|| workspace_id.to_owned(), |id| id.to_string()),
        manifest_document_id: status.document.map(|id| id.to_string()).unwrap_or_default(),
        sync: status.sync.as_ref().map(|sync| wire::CloudSyncPermit {
            enabled: sync.enabled,
            stage: sync.stage.clone().unwrap_or_default(),
            error_code: sync.error_code.clone().unwrap_or_default(),
        }),
        files: status.blobs.as_ref().map(|blobs| wire::CloudSyncFiles {
            referenced: blobs.referenced,
            present: blobs.present,
            durable: blobs.durable,
            uploading: blobs.uploading,
            downloading: blobs.downloading,
            missing: blobs.missing,
            pending_reason: blobs.pending_reason.clone().unwrap_or_default(),
            direction: blobs.direction.clone().unwrap_or_default(),
            path: blobs.path.clone().unwrap_or_default(),
            unreadable_references: blobs.unreadable_references,
            paused: blobs.paused,
            coverage: blobs
                .coverage
                .as_ref()
                .map(|coverage| wire::CloudSyncCoverage {
                    state: coverage.state.clone(),
                    documents_required: coverage.documents_required,
                    documents_missing: coverage.documents_missing,
                }),
        }),
        error_code: None,
    }
}

/// The documents one Workspace is waiting on another device for.
pub fn cloud_waiting(request: wire::CloudWaitingRequest) -> wire::CloudWaitingDocuments {
    match host(request.host, "store.cloud_waiting")
        .and_then(|host| host.cloud_waiting(&request.workspace_id))
    {
        Ok(documents) => wire::CloudWaitingDocuments {
            workspace_id: request.workspace_id,
            documents: documents.into_iter().map(cloud_waiting_message).collect(),
            error_code: None,
        },
        Err(code) => wire::CloudWaitingDocuments {
            workspace_id: request.workspace_id,
            documents: Vec::new(),
            error_code: Some(code),
        },
    }
}

pub(crate) fn cloud_waiting_message(
    document: koru_store_wire::CloudWaitingDocument,
) -> wire::CloudWaitingDocument {
    wire::CloudWaitingDocument {
        document_id: document.document.to_string(),
        title: document.title,
        missing_files: document.missing_files,
        holders: document.holders,
    }
}

pub fn set_blob_policy(request: wire::SetBlobPolicyRequest) -> wire::MutationResult {
    mutation(
        host(request.host, "store.set_blob_policy")
            .and_then(|host| host.set_blob_policy(&request.policy.unwrap_or_default())),
        "ok",
    )
}

/// What blob a file document is, for a caller about to export or describe it.
pub fn file_blob(request: wire::FileBlobRequest) -> wire::FileBlob {
    match host(request.host, "store.file_blob")
        .and_then(|host| host.file_blob(&request.document_id))
    {
        Ok((hash, mime_type, size)) => wire::FileBlob {
            hash,
            mime_type,
            size,
            error_code: None,
        },
        Err(code) => wire::FileBlob {
            error_code: Some(code),
            ..wire::FileBlob::default()
        },
    }
}

/// Write one blob to the path the caller named.
///
/// Its own result rather than `MutationResult` because a caller that asked for
/// a file wants to know how much of one arrived, and because "ok" is a poor
/// answer about a video.
pub fn export_blob(request: wire::ExportBlobRequest) -> wire::ExportedBlob {
    match host(request.host, "store.export_blob").and_then(|host| {
        host.export_blob(
            &request.workspace_id,
            &request.hash,
            &request.destination_path,
        )
    }) {
        Ok(size) => wire::ExportedBlob {
            size,
            error_code: None,
        },
        Err(code) => wire::ExportedBlob {
            size: 0,
            error_code: Some(code),
        },
    }
}

pub fn request_blob(request: wire::RequestBlobRequest) -> wire::MutationResult {
    let wanted = request.request.unwrap_or_default();
    mutation(
        host(request.host, "store.request_blob")
            .and_then(|host| host.request_blob(&wanted.workspace_id, &wanted.blob_hash)),
        "ok",
    )
}

/// Its own result rather than `MutationResult`, because "ok" says nothing
/// about how many copies went and how many were kept.
pub fn release_blobs(request: wire::ReleaseBlobsRequest) -> wire::ReleasedBlobs {
    host(request.host, "store.release_blobs")
        .and_then(|host| host.release_blobs(&request.workspace_id))
        .unwrap_or_else(|code| wire::ReleasedBlobs {
            error_code: Some(code),
            ..wire::ReleasedBlobs::default()
        })
}

pub fn recover_blobs(request: wire::BlobRecoverRequest) -> wire::MutationResult {
    mutation(
        host(request.host, "store.recover_blobs")
            .and_then(|host| host.recover_blobs(&request.workspace_id)),
        "ok",
    )
}

pub fn blob_arrivals(request: wire::BlobArrivalsRequest) -> wire::BlobArrivals {
    host(request.host, "store.blob_arrivals")
        .and_then(|host| host.blob_arrivals(&request.workspace_id, request.after_sequence))
        .unwrap_or_else(|code| wire::BlobArrivals {
            error_code: Some(code),
            ..wire::BlobArrivals::default()
        })
}

pub fn mutate_workspace(r: wire::WorkspaceMutation) -> wire::MutationResult {
    mutation(
        host(r.host, "store.mutate_workspace")
            .and_then(|h| h.mutate_workspace(&r.workspace_id, &r.action, &r.title)),
        "ok",
    )
}
pub fn mutate_profile(r: wire::ProfileMutation) -> wire::MutationResult {
    mutation(
        host(r.host, "store.mutate_profile")
            .and_then(|h| h.mutate_profile(&r.profile_id, &r.action, &r.value)),
        "ok",
    )
}
pub fn mutate_document(r: wire::DocumentMutation) -> wire::MutationResult {
    mutation(
        host(r.host, "store.mutate_document").and_then(|h| {
            match (r.action.as_str(), r.text_edit) {
                ("edit_text", Some(edit)) => h.edit_document_text(&r.document_id, edit),
                ("edit_text", None) | (_, Some(_)) => Err("invalid_document_action".to_owned()),
                (_, None) => h.mutate_document(&r.document_id, &r.action, &r.title),
            }
        }),
        "ok",
    )
}
pub fn import_file(r: wire::ImportFileRequest) -> wire::DocumentIdResult {
    match host(r.host, "store.import_file").and_then(|h| {
        h.import_file(
            &r.workspace_id,
            &r.name,
            &r.mime,
            &r.path,
            r.note.as_deref(),
        )
    }) {
        Ok(id) => wire::DocumentIdResult {
            document_id: Some(id),
            error_code: None,
        },
        Err(code) => wire::DocumentIdResult {
            document_id: None,
            error_code: Some(code),
        },
    }
}
pub fn file_note(r: wire::FileNoteRequest) -> wire::DocumentIdResult {
    match host(r.host, "store.ensure_file_note").and_then(|h| h.ensure_file_note(&r.document_id)) {
        Ok(id) => wire::DocumentIdResult {
            document_id: Some(id),
            error_code: None,
        },
        Err(code) => wire::DocumentIdResult {
            document_id: None,
            error_code: Some(code),
        },
    }
}
pub fn start_pairing(r: wire::StartPairingRequest) -> wire::PairingOffer {
    let v = host(r.host, "start_pairing")
        .map(|h| h.start_pairing())
        .unwrap_or_else(|code| PairingOfferResult {
            state: "failed",
            qr_payload: None,
            expires_in_seconds: 0,
            error_code: Some(code),
        });
    wire::PairingOffer {
        state: v.state.into(),
        qr_payload: v.qr_payload,
        expires_in_seconds: u32::from(v.expires_in_seconds),
        error_code: v.error_code,
    }
}
pub fn cancel_pairing(r: wire::CancelPairingRequest) -> wire::MutationResult {
    mutation(
        host(r.host, "cancel_pairing").map(|h| h.cancel_pairing()),
        "cancelled",
    )
}

fn event(v: MobileStateEvent) -> wire::StateEvent {
    wire::StateEvent {
        event_id: v.event_id,
        at_millis: v.at_millis,
        kind: v.kind,
        profile_id: v.profile_id,
        workspace_id: v.workspace_id,
        document_id: v.document_id,
        document_name: v.document_name,
        blob_hash: v.blob_hash,
        detail: v.detail,
        items: v.items.map(|n| n as u64),
    }
}
fn device(v: crate::LinkedDeviceResult) -> wire::LinkedDevice {
    wire::LinkedDevice {
        device_id: v.device_id,
        display_name: v.display_name,
        linked_at_millis: v.linked_at_millis,
        connected: v.connected,
        last_seen_millis: v.last_seen_millis,
        introduced_by: v.introduced_by,
        build_label: v.build_label,
        build_drift: v.build_drift,
        build_severity: v.build_severity,
        update_needed_on: v.update_needed_on,
        connection: v.connection,
        last_sync_millis: v.last_sync_millis,
        history: v.history.into_iter().map(event).collect(),
    }
}
/// The host's state word onto the wire enum. An unknown word maps to
/// `Unspecified` — the value clients treat as "nothing to show" — rather
/// than silently claiming any particular state.
fn relay(v: crate::RelayStatusResult) -> wire::RelayStatus {
    let state = match v.state {
        "off" => wire::RelayState::Off,
        "connecting" => wire::RelayState::Connecting,
        "connected" => wire::RelayState::Connected,
        "disconnected" => wire::RelayState::Disconnected,
        _ => wire::RelayState::Unspecified,
    };
    wire::RelayStatus {
        state: state as i32,
        host: v.host,
    }
}
pub fn devices(r: wire::DevicesRequest) -> wire::DeviceList {
    let v = host(r.host, "devices")
        .map(|h| h.devices())
        .unwrap_or_else(|code| DevicesResult {
            error_code: Some(code),
            ..Default::default()
        });
    wire::DeviceList {
        devices: v.devices.into_iter().map(device).collect(),
        this_device: v.this_device.map(device),
        relay: v.relay.map(relay),
        sync_event: v.sync_event.map(|e| wire::SyncEvent {
            sequence: e.sequence,
            at_millis: e.at_millis,
            direction: e.direction.into(),
            peer: e.peer,
            stage: e.stage.into(),
            workspace_id: e.workspace_id,
            items: e.items.map(|n| n as u64),
            error_code: e.error_code,
        }),
        error_code: v.error_code,
        relink_available: v.relink_available,
    }
}
pub fn device_history(r: wire::DeviceHistoryRequest) -> wire::DeviceHistory {
    match host(r.host, "device_history").and_then(|h| h.state_history(&r.device_id, r.before)) {
        Ok(v) => wire::DeviceHistory {
            device_id: v.device_id,
            events: v.events.into_iter().map(event).collect(),
            next_before: v.next_before,
            error_code: None,
        },
        Err(code) => wire::DeviceHistory {
            error_code: Some(code),
            ..Default::default()
        },
    }
}
pub fn unlink_device(r: wire::UnlinkDeviceRequest) -> wire::MutationResult {
    mutation(
        host(r.host, "unlink_device").and_then(|h| h.unlink(&r.device_id)),
        "unlinked",
    )
}
pub fn rename_device(r: wire::RenameDeviceRequest) -> wire::MutationResult {
    mutation(
        host(r.host, "rename_device").and_then(|h| h.rename(&r.name)),
        "renamed",
    )
}
/// Apply the relay on/off change to the live runtime (decision №6: it takes
/// effect without an app restart). The shim persists the value in the
/// platform's local storage itself and hands it back through
/// `HostContext.relay_enabled` on the next open.
pub fn set_relay(r: wire::SetRelayRequest) -> wire::MutationResult {
    mutation(
        host(r.host, "set_relay").map(|h| h.set_relay_enabled(r.enabled)),
        "applied",
    )
}
pub fn reset_identity(r: wire::ResetIdentityRequest) -> wire::MutationResult {
    let name = r
        .host
        .as_ref()
        .map(|h| h.device_name.clone())
        .unwrap_or_default();
    mutation(
        host(r.host, "reset_identity").and_then(|h| h.reset_identity(&name)),
        "reset",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_account_binding_survives_json_and_protobuf_without_reinterpretation() {
        use prost::Message;
        for binding in [
            "",
            "koru:10000000-0000-4000-8000-000000000001",
            " malformed ",
        ] {
            let value = InventoryResult {
                profiles: vec![crate::InventoryProfile {
                    id: "30000000-0000-4000-8000-000000000001".into(),
                    name: "Local".into(),
                    locale: "en".into(),
                    icon: None,
                    leading_emoji: None,
                    display_name: "Local".into(),
                    color: None,
                    account_binding: binding.into(),
                }],
                ..InventoryResult::default()
            };
            let json = serde_json::to_value(&value).unwrap();
            assert_eq!(json["profiles"][0]["account_binding"], binding);
            let encoded = inventory_value(value).encode_to_vec();
            let decoded = wire::Inventory::decode(encoded.as_slice()).unwrap();
            assert_eq!(decoded.profiles[0].account_binding, binding);
        }
    }

    fn context(root: &std::path::Path, secrets: Vec<u8>) -> wire::HostContext {
        wire::HostContext {
            root: root.display().to_string(),
            secrets,
            device_name: "Test".into(),
            relay_enabled: None,
        }
    }

    #[test]
    fn a_fresh_host_answers_through_generated_messages() {
        let directory = tempfile::tempdir().unwrap();
        let result = status(wire::HostStatusRequest {
            host: Some(context(directory.path(), vec![47; 64])),
        });
        assert_eq!(result.state, "unlinked");
        assert_eq!(result.error_code, None);
    }

    /// The platform hands relay-off in through the context; the host opens
    /// and answers as usual (off leaves direct/LAN — it is not an error
    /// state), and the live switch answers through the shared mutation
    /// envelope in both directions.
    #[test]
    fn a_relay_off_open_answers_and_the_live_switch_applies() {
        let directory = tempfile::tempdir().unwrap();
        let mut ctx = context(directory.path(), vec![48; 64]);
        ctx.relay_enabled = Some(false);
        let result = status(wire::HostStatusRequest {
            host: Some(ctx.clone()),
        });
        assert_eq!(result.state, "unlinked");
        assert_eq!(result.error_code, None);
        for enabled in [true, false] {
            let result = set_relay(wire::SetRelayRequest {
                host: Some(ctx.clone()),
                enabled,
            });
            assert!(result.ok, "{:?}", result.error_code);
            assert_eq!(result.state, "applied");
        }
    }

    /// Every state word the host produces lands on its own wire enum value,
    /// with the host string carried through; a word this mapping does not
    /// know reads as `Unspecified` — "nothing to show" — never as a state.
    #[test]
    fn relay_state_words_map_onto_the_wire_enum() {
        for (word, expected) in [
            ("off", wire::RelayState::Off),
            ("connecting", wire::RelayState::Connecting),
            ("connected", wire::RelayState::Connected),
            ("disconnected", wire::RelayState::Disconnected),
            ("someday-new", wire::RelayState::Unspecified),
        ] {
            let value = relay(crate::RelayStatusResult {
                state: word,
                host: "use1-1.relay.koru.do".into(),
            });
            assert_eq!(value.state, expected as i32, "{word}");
            assert_eq!(value.host, "use1-1.relay.koru.do");
        }
    }

    /// The generated DeviceList answer carries the relay row end to end: a
    /// host opened with relays off answers `RELAY_STATE_OFF` with no host
    /// to name, and a request without a context (the error shape) carries
    /// no relay claim at all.
    #[test]
    fn relay_status_reaches_the_generated_device_list() {
        let directory = tempfile::tempdir().unwrap();
        let mut ctx = context(directory.path(), vec![49; 64]);
        ctx.relay_enabled = Some(false);
        let result = devices(wire::DevicesRequest { host: Some(ctx) });
        let value = result.relay.expect("a relay reading");
        assert_eq!(value.state, wire::RelayState::Off as i32);
        assert_eq!(value.host, "");

        let failed = devices(wire::DevicesRequest { host: None });
        assert_eq!(failed.error_code.as_deref(), Some("missing_host_context"));
        assert_eq!(failed.relay, None);
    }

    #[test]
    fn set_relay_without_a_context_fails_closed() {
        let result = set_relay(wire::SetRelayRequest {
            host: None,
            enabled: false,
        });
        assert!(!result.ok);
        assert_eq!(result.error_code.as_deref(), Some("missing_host_context"));
    }

    #[test]
    fn invalid_platform_secrets_fail_in_the_generated_response_shape() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("short-secrets");
        let result = status(wire::HostStatusRequest {
            host: Some(context(&root, vec![1, 2, 3])),
        });
        assert_eq!(result.state, "failed");
        assert_eq!(result.error_code.as_deref(), Some("invalid_device_seeds"));
        assert!(!root.exists());
    }
}
