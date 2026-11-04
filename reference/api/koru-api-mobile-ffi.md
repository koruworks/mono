# Koru API mobile FFI reference

Generated from the public mobile operation catalog. Do not edit.

| Operation | Rust client | HTTP | Mobile symbol | Scope | Request | Response |
| --- | --- | --- | --- | --- | --- | --- |
| `status` | `client.status` | GET /v1/status | `koru_api_status` | platform principal | `HostStatusRequest` | `HostStatus` |
| `link_device` | `client.link_device` | not HTTP-exposed | `koru_api_link_device` | platform principal | `LinkDeviceRequest` | `LinkDeviceResult` |
| `advertise_build` | `client.advertise_build` | not HTTP-exposed | `koru_api_advertise_build` | platform principal | `BuildPayload` | `MutationResult` |
| `store.inventory` | `client.store().inventory` | not HTTP-exposed | `koru_api_store_inventory` | store.read_documents | `InventoryRequest` | `Inventory` |
| `store.document` | `client.store().document` | not HTTP-exposed | `koru_api_store_document` | store.read_documents | `DocumentRequest` | `DocumentProjection` |
| `store.blob` | `client.store().blob` | GET /v1/workspaces/{id}/blobs/{hash} | `koru_api_store_blob` | store.read_documents | `BlobRequest` | `BlobBytes` |
| `store.blob_status` | `client.store().blob_status` | not HTTP-exposed | `koru_api_store_blob_status` | store.read_documents | `BlobStatusRequest` | `BlobTransferStatus` |
| `store.cloud_sync_status` | `client.store().cloud_sync_status` | not HTTP-exposed | `koru_api_store_cloud_sync_status` | store.read_documents | `CloudSyncStatusRequest` | `CloudSyncStatus` |
| `store.cloud_waiting` | `client.store().cloud_waiting` | not HTTP-exposed | `koru_api_store_cloud_waiting` | store.read_documents | `CloudWaitingRequest` | `CloudWaitingDocuments` |
| `store.set_blob_policy` | `client.store().set_blob_policy` | not HTTP-exposed | `koru_api_store_set_blob_policy` | store.read_documents | `SetBlobPolicyRequest` | `MutationResult` |
| `store.request_blob` | `client.store().request_blob` | not HTTP-exposed | `koru_api_store_request_blob` | store.read_documents | `RequestBlobRequest` | `MutationResult` |
| `store.file_blob` | `client.store().file_blob` | not HTTP-exposed | `koru_api_store_file_blob` | store.read_documents | `FileBlobRequest` | `FileBlob` |
| `store.export_blob` | `client.store().export_blob` | not HTTP-exposed | `koru_api_store_export_blob` | store.read_documents | `ExportBlobRequest` | `ExportedBlob` |
| `store.release_blobs` | `client.store().release_blobs` | not HTTP-exposed | `koru_api_store_release_blobs` | store.read_documents | `ReleaseBlobsRequest` | `ReleasedBlobs` |
| `store.recover_blobs` | `client.store().recover_blobs` | not HTTP-exposed | `koru_api_store_recover_blobs` | store.read_documents | `BlobRecoverRequest` | `MutationResult` |
| `store.blob_arrivals` | `client.store().blob_arrivals` | not HTTP-exposed | `koru_api_store_blob_arrivals` | store.read_documents | `BlobArrivalsRequest` | `BlobArrivals` |
| `store.run` | `client.store().run` | not HTTP-exposed | `koru_api_store_run` | store.read_documents | `RunRequest` | `RunView` |
| `store.run_timeline` | `client.store().run_timeline` | not HTTP-exposed | `koru_api_store_run_timeline` | store.read_documents | `RunTimelineRequest` | `RunTimelineWindow` |
| `store.mutate_workspace` | `client.store().mutate_workspace` | not HTTP-exposed | `koru_api_store_mutate_workspace` | store.manage_workspaces | `WorkspaceMutation` | `MutationResult` |
| `store.mutate_profile` | `client.store().mutate_profile` | not HTTP-exposed | `koru_api_store_mutate_profile` | store.manage_workspaces | `ProfileMutation` | `MutationResult` |
| `store.mutate_document` | `client.store().mutate_document` | not HTTP-exposed | `koru_api_store_mutate_document` | store.edit_documents | `DocumentMutation` | `MutationResult` |
| `store.import_file` | `client.store().import_file` | not HTTP-exposed | `koru_api_store_import_file` | store.edit_documents | `ImportFileRequest` | `DocumentIdResult` |
| `store.ensure_file_note` | `client.store().ensure_file_note` | not HTTP-exposed | `koru_api_store_ensure_file_note` | store.edit_documents | `FileNoteRequest` | `DocumentIdResult` |
| `start_pairing` | `client.start_pairing` | not HTTP-exposed | `koru_api_start_pairing` | platform principal | `StartPairingRequest` | `PairingOffer` |
| `cancel_pairing` | `client.cancel_pairing` | not HTTP-exposed | `koru_api_cancel_pairing` | platform principal | `CancelPairingRequest` | `MutationResult` |
| `devices` | `client.devices` | not HTTP-exposed | `koru_api_devices` | platform principal | `DevicesRequest` | `DeviceList` |
| `device_history` | `client.device_history` | not HTTP-exposed | `koru_api_device_history` | platform principal | `DeviceHistoryRequest` | `DeviceHistory` |
| `unlink_device` | `client.unlink_device` | not HTTP-exposed | `koru_api_unlink_device` | platform principal | `UnlinkDeviceRequest` | `MutationResult` |
| `rename_device` | `client.rename_device` | not HTTP-exposed | `koru_api_rename_device` | platform principal | `RenameDeviceRequest` | `MutationResult` |
| `reset_identity` | `client.reset_identity` | not HTTP-exposed | `koru_api_reset_identity` | platform principal | `ResetIdentityRequest` | `MutationResult` |
| `set_relay` | `client.set_relay` | not HTTP-exposed | `koru_api_set_relay` | platform principal | `SetRelayRequest` | `MutationResult` |
| `account_command` | `client.account_command` | not HTTP-exposed | `koru_api_account_command` | platform principal | `AccountCommandRequest` | `AccountCommandReply` |
| `account_poll` | `client.account_poll` | not HTTP-exposed | `koru_api_account_poll` | platform principal | `AccountPollRequest` | `AccountPollReply` |
| `account_platform` | `client.account_platform` | not HTTP-exposed | `koru_api_account_platform` | platform principal | `AccountPlatformRequest` | `MutationResult` |

## Ownership

Requests and responses are Protobuf values at the canonical ABI. Each native call is synchronous on a platform IO executor: the caller owns request bytes for the duration of the call, then owns the returned response until `koru_buffer_free` (iOS) or JVM collection (Android). Kotlin coroutines and the Swift Store actor keep these calls off the main thread. Buffer free is lifecycle mechanics rather than a product operation.

## Explicitly unavailable operations

These public HTTP/client operations are catalogued but have no mobile FFI or Protobuf projection in this phase.

| Operation | Rust client | HTTP | Reason |
| --- | --- | --- | --- |
| `ai.provider_settings` | `client.ai().provider_settings` | GET /v1/ai/providers/settings | Phase 1 provider setup is desktop-only |
| `ai.select_provider` | `client.ai().select_provider` | PUT /v1/ai/providers/selection | Phase 1 provider setup is desktop-only |
| `ai.update_openai_settings` | `client.ai().update_openai_settings` | PUT /v1/ai/providers/openai | Phase 1 provider setup is desktop-only |
| `ai.update_ollama_settings` | `client.ai().update_ollama_settings` | PUT /v1/ai/providers/ollama | Phase 1 provider setup is desktop-only |
| `ai.validate_provider` | `client.ai().validate_provider` | POST /v1/ai/providers/{provider}/validate | Phase 1 provider setup is desktop-only |
| `ai.discover_agent_models` | `client.ai().discover_agent_models` | POST /v1/ai/providers/{provider}/models | Local agents run on the desktop that installed them |
| `ai.discover_agent_commands` | `client.ai().discover_agent_commands` | POST /v1/ai/providers/{provider}/commands | Local agents run on the desktop that installed them |
| `ai.pull_ollama_model` | `client.ai().pull_ollama_model` | POST /v1/ai/providers/ollama/pulls | Desktop-local Ollama lifecycle is unavailable on mobile |
| `ai.cancel_ollama_pull` | `client.ai().cancel_ollama_pull` | POST /v1/ai/providers/ollama/pulls/cancel | Desktop-local Ollama lifecycle is unavailable on mobile |
| `ai.create_chat` | `client.ai().create_chat` | POST /v1/ai/chats | Phase 1 ships desktop chat before a mobile chat host |
| `ai.list_chats` | `client.ai().list_chats` | GET /v1/ai/chats | Phase 1 ships desktop chat before a mobile chat host |
| `ai.read_chat` | `client.ai().read_chat` | GET /v1/ai/chats/{chat} | Phase 1 ships desktop chat before a mobile chat host |
| `ai.rename_chat` | `client.ai().rename_chat` | PATCH /v1/ai/chats/{chat} | Phase 1 ships desktop chat before a mobile chat host |
| `ai.pin_chat` | `client.ai().pin_chat` | PATCH /v1/ai/chats/{chat}/pin | Phase 1 ships desktop chat before a mobile chat host |
| `ai.update_chat_model` | `client.ai().update_chat_model` | PATCH /v1/ai/chats/{chat}/model | Phase 1 ships desktop chat before a mobile chat host |
| `ai.update_chat_privacy` | `client.ai().update_chat_privacy` | PATCH /v1/ai/chats/{chat}/privacy | Per-chat AI privacy is not yet exposed by a mobile chat host |
| `ai.update_chat_folder` | `client.ai().update_chat_folder` | PATCH /v1/ai/chats/{chat}/folder | Local agents run on the desktop that installed them |
| `ai.update_chat_agent_settings` | `client.ai().update_chat_agent_settings` | PATCH /v1/ai/chats/{chat}/agent-settings | Local agents run on the desktop that installed them |
| `ai.tool_list_chats` | `client.ai().tool_list_chats` | POST /v1/ai/tools/chats/list | Chat tools are an MCP and provider-facing surface, not a mobile chat operation |
| `ai.tool_search_chats` | `client.ai().tool_search_chats` | POST /v1/ai/tools/chats/search | Chat tools are an MCP and provider-facing surface, not a mobile chat operation |
| `ai.tool_read_chat` | `client.ai().tool_read_chat` | POST /v1/ai/tools/chats/read | Chat tools are an MCP and provider-facing surface, not a mobile chat operation |
| `ai.update_chat_tool_policy` | `client.ai().update_chat_tool_policy` | PUT /v1/ai/chats/{chat}/tool-policy | Workspace-tool chat policy is not yet exposed by a mobile chat host |
| `ai.read_chat_tool_policy` | `client.ai().read_chat_tool_policy` | GET /v1/ai/chats/{chat}/tool-policy | Workspace-tool chat policy is not yet exposed by a mobile chat host |
| `ai.search` | `client.ai().search` | POST /v1/ai/search | Phase 1 ships AI chat search before a mobile chat host |
| `ai.feedback` | `client.ai().feedback` | PUT /v1/ai/messages/{message}/feedback | Phase 1 ships desktop response actions before a mobile chat host |
| `ai.branch_chat` | `client.ai().branch_chat` | POST /v1/ai/chats/{chat}/branches | Phase 1 ships desktop response actions before a mobile chat host |
| `ai.retry_turn` | `client.ai().retry_turn` | POST /v1/ai/chats/{chat}/retries | Mobile SSE/stream projection is not yet implemented |
| `ai.export_chat` | `client.ai().export_chat` | POST /v1/ai/chats/{chat}/exports | Phase 1 ships desktop response actions before a mobile chat host |
| `ai.read_export` | `client.ai().read_export` | GET /v1/ai/exports/{export} | Phase 1 ships desktop response actions before a mobile chat host |
| `ai.send_turn` | `client.ai().send_turn` | POST /v1/ai/chats/{chat}/turns | Mobile SSE/stream projection is not yet implemented |
| `ai.read_attempt` | `client.ai().read_attempt` | GET /v1/ai/attempts/{attempt} | Phase 1 ships desktop chat before a mobile chat host |
| `ai.cancel_attempt` | `client.ai().cancel_attempt` | POST /v1/ai/attempts/{attempt}/cancel | Phase 1 ships desktop chat before a mobile chat host |
| `ai.steer_attempt` | `client.ai().steer_attempt` | POST /v1/ai/attempts/{attempt}/steer | Phase 1 ships desktop chat before a mobile chat host |
| `ai.approve_tool_call` | `client.ai().approve_tool_call` | POST /v1/ai/tool-calls/{tool}/approve | Workspace-tool approval is not yet exposed by a mobile chat host |
| `ai.deny_tool_call` | `client.ai().deny_tool_call` | POST /v1/ai/tool-calls/{tool}/deny | Workspace-tool approval is not yet exposed by a mobile chat host |
| `ai.delete_chat` | `client.ai().delete_chat` | DELETE /v1/ai/chats/{chat} | Phase 1 ships desktop history management before a mobile chat host |
| `ai.clear_history` | `client.ai().clear_history` | DELETE /v1/ai/chats | Phase 1 ships desktop history management before a mobile chat host |
| `ai.put_attachment` | `client.ai().put_attachment` | PUT /v1/ai/chats/{chat}/attachments/{attachment} | Attachments go to local agents, which run on the desktop that installed them |
| `ai.remove_attachment` | `client.ai().remove_attachment` | DELETE /v1/ai/chats/{chat}/attachments/{attachment} | Attachments go to local agents, which run on the desktop that installed them |
| `ai.read_attachment` | `client.ai().attachment_bytes` | GET /v1/ai/chats/{chat}/attachments/{attachment} | Attachments go to local agents, which run on the desktop that installed them |
| `ai.read_attachment_thumbnail` | `client.ai().attachment_thumbnail` | GET /v1/ai/chats/{chat}/attachments/{attachment}/thumbnail | Attachments go to local agents, which run on the desktop that installed them |
| `ai.usage` | `client.ai().usage` | GET /v1/ai/usage | Private local AI usage is desktop-only in Phase 1 |
| `ai.chat_usage` | `client.ai().usage(Some(chat))` | GET /v1/ai/chats/{chat}/usage | Private local AI usage is desktop-only in Phase 1 |
| `ai.clear_usage` | `client.ai().clear_usage` | DELETE /v1/ai/usage | Private local AI usage is desktop-only in Phase 1 |
