// SPDX-License-Identifier: MPL-2.0

package app.koru.mobile.services

import app.koru.contract.api.v1.*
import com.google.protobuf.ByteString
import com.google.protobuf.MessageLite

/** Canonical generated-shape Protobuf binding over the one JNI byte edge. */
internal class KoruApiNative(
    root: String,
    secrets: ByteArray,
    deviceName: String = "Android",
    /** The device-local relay preference; applied when the host first opens.
     *  A later change goes through [setRelay], which applies live. */
    relayEnabled: Boolean = true,
) {
    private val host = HostContext.newBuilder()
        .setRoot(root).setSecrets(ByteString.copyFrom(secrets)).setDeviceName(deviceName)
        .setRelayEnabled(relayEnabled).build()

    private external fun nativeInvoke(operation: String, request: ByteArray): ByteArray
    private external fun nativeObserveDocument(request: ByteArray, onChange: Runnable): Long
    private external fun nativeCloseDocumentObserver(observer: Long)

    fun observeDocument(id: String, onChange: Runnable): Long {
        val input = DocumentRequest.newBuilder().setHost(host).setDocumentId(id).build().toByteArray()
        try { return nativeObserveDocument(input, onChange) }
        finally { input.fill(0) }
    }

    fun closeDocumentObserver(observer: Long) = nativeCloseDocumentObserver(observer)
    private external fun nativeConfigureAccount(host: ByteArray, origin: String, issuer: String, client: String, channel: String, credentialRollout: String, blobByteOrigins: String)
    fun configureAccount(config: AndroidAccountConfig) {
        val input = host.toByteArray()
        try { nativeConfigureAccount(input, config.origin, config.issuer, config.serverClientId, config.channel, config.credentialRollout, config.blobByteOrigins) }
        finally { input.fill(0) }
    }
    private fun <T : MessageLite> call(operation: String, request: MessageLite, decode: (ByteArray) -> T): T {
        val input = request.toByteArray()
        try {
            val output = nativeInvoke(operation, input)
            try { return decode(output) } finally { output.fill(0) }
        } finally { input.fill(0) }
    }

    fun status() = call("status", HostStatusRequest.newBuilder().setHost(host).build(), HostStatus::parseFrom)
    fun link(code: String) = call("link_device", LinkDeviceRequest.newBuilder().setHost(host).setCode(code).build(), LinkDeviceResult::parseFrom)
    fun advertise(request: BuildPayload) = call("advertise_build", request.toBuilder().setHost(host).build(), MutationResult::parseFrom)
    fun inventory() = call("store.inventory", InventoryRequest.newBuilder().setHost(host).build(), Inventory::parseFrom)
    fun document(id: String) = call("store.document", DocumentRequest.newBuilder().setHost(host).setDocumentId(id).build(), DocumentProjection::parseFrom)
    fun blob(workspace: String, hash: String) = call("store.blob", BlobRequest.newBuilder().setHost(host).setWorkspaceId(workspace).setHash(hash).build(), BlobBytes::parseFrom)
    // `cloud-blob-object-v1` §10, plus the two a phone needs around it:
    // WorkManager's recovery entry point and the arrival window Android has to
    // ask for because it has no room registry.
    fun blobStatus(workspace: String) = call("store.blob_status", BlobStatusRequest.newBuilder().setHost(host).setWorkspaceId(workspace).build(), BlobTransferStatus::parseFrom)
    // The Cloud sync progress screen's two reads: the desktop's `cloud-sync`
    // answer for one Workspace (asked of its manifest by the host), and the
    // documents it is waiting on another device for.
    fun cloudSyncStatus(workspace: String) = call("store.cloud_sync_status", CloudSyncStatusRequest.newBuilder().setHost(host).setWorkspaceId(workspace).build(), CloudSyncStatus::parseFrom)
    fun cloudWaiting(workspace: String) = call("store.cloud_waiting", CloudWaitingRequest.newBuilder().setHost(host).setWorkspaceId(workspace).build(), CloudWaitingDocuments::parseFrom)
    fun setBlobPolicy(policy: BlobPolicy) = call("store.set_blob_policy", SetBlobPolicyRequest.newBuilder().setHost(host).setPolicy(policy).build(), MutationResult::parseFrom)
    fun requestBlob(workspace: String, hash: String) = call("store.request_blob", RequestBlobRequest.newBuilder().setHost(host).setRequest(BlobDownloadRequest.newBuilder().setWorkspaceId(workspace).setBlobHash(hash)).build(), MutationResult::parseFrom)
    /** What blob a `kind == "file"` document is. Its own call because
     *  opening a file as a document is refused: `document_is_file`. */
    fun fileBlob(documentId: String) =
        call("store.file_blob", FileBlobRequest.newBuilder().setHost(host)
            .setDocumentId(documentId).build(), FileBlob::parseFrom)

    /** Write one blob to [destination]. A path rather than bytes because a
     *  file can be a video: `blob` returns the whole payload through JNI and
     *  into the JVM heap, which is what the import side already refuses. */
    fun exportBlob(workspace: String, hash: String, destination: String) =
        call("store.export_blob", ExportBlobRequest.newBuilder().setHost(host)
            .setWorkspaceId(workspace).setHash(hash).setDestinationPath(destination).build(),
            ExportedBlob::parseFrom)
    /** Drop this device's copies of the files Koru Cloud holds, keep every
     *  reference, and read back the numbers. The engine checks each file
     *  against the control plane before touching it. */
    fun releaseBlobs(workspace: String) = call("store.release_blobs", ReleaseBlobsRequest.newBuilder().setHost(host).setWorkspaceId(workspace).build(), ReleasedBlobs::parseFrom)
    fun recoverBlobs(workspace: String) = call("store.recover_blobs", BlobRecoverRequest.newBuilder().setHost(host).setWorkspaceId(workspace).build(), MutationResult::parseFrom)
    fun blobArrivals(workspace: String, afterSequence: Long) = call("store.blob_arrivals", BlobArrivalsRequest.newBuilder().setHost(host).setWorkspaceId(workspace).setAfterSequence(afterSequence).build(), BlobArrivals::parseFrom)
    fun run(id: String) = call("store.run", RunRequest.newBuilder().setHost(host).setDocumentId(id).build(), RunView::parseFrom)
    fun runTimeline(id: String, newestFirstFrom: Int, len: Int) = call("store.run_timeline", RunTimelineRequest.newBuilder().setHost(host).setDocumentId(id).setNewestFirstFrom(newestFirstFrom).setLen(len).build(), RunTimelineWindow::parseFrom)
    fun mutateWorkspace(id: String, action: String, title: String) = call("store.mutate_workspace", WorkspaceMutation.newBuilder().setHost(host).setWorkspaceId(id).setAction(action).setTitle(title).build(), MutationResult::parseFrom)
    fun mutateProfile(id: String, action: String, value: String) = call("store.mutate_profile", ProfileMutation.newBuilder().setHost(host).setProfileId(id).setAction(action).setValue(value).build(), MutationResult::parseFrom)
    fun mutateDocument(id: String, action: String, title: String, expectedVersion: ByteString = ByteString.EMPTY) = call("store.mutate_document", DocumentMutation.newBuilder().setHost(host).setDocumentId(id).setAction(action).setTitle(title).setExpectedVersion(expectedVersion).build(), MutationResult::parseFrom)
    fun editBlockText(id: String, edit: app.koru.mobile.domain.DocumentCommand.EditBlockText, action: String = "edit_text") = call(
        "store.mutate_document", DocumentMutation.newBuilder().setHost(host).setDocumentId(id)
            .setAction(action).setTextEdit(edit.toWireEdit()).build(), MutationResult::parseFrom)
    fun importFile(workspace: String, name: String, mime: String, path: String, note: String? = null) = call("store.import_file", ImportFileRequest.newBuilder().setHost(host).setWorkspaceId(workspace).setName(name).setMime(mime).setPath(path).apply { note?.let(::setNote) }.build(), DocumentIdResult::parseFrom)
    fun ensureFileNote(document: String) = call("store.ensure_file_note", FileNoteRequest.newBuilder().setHost(host).setDocumentId(document).build(), DocumentIdResult::parseFrom)
    fun startPairing() = call(
        "start_pairing",
        StartPairingRequest.newBuilder().setHost(host).build(),
        app.koru.contract.api.v1.PairingOffer::parseFrom,
    )
    fun cancelPairing() = call("cancel_pairing", CancelPairingRequest.newBuilder().setHost(host).build(), MutationResult::parseFrom)
    fun devices() = call("devices", DevicesRequest.newBuilder().setHost(host).build(), DeviceList::parseFrom)
    fun history(device: String, before: Long?) = call("device_history", DeviceHistoryRequest.newBuilder().setHost(host).setDeviceId(device).apply { before?.let(::setBefore) }.build(), DeviceHistory::parseFrom)
    fun unlink(device: String) = call("unlink_device", UnlinkDeviceRequest.newBuilder().setHost(host).setDeviceId(device).build(), MutationResult::parseFrom)
    fun rename(name: String) = call("rename_device", RenameDeviceRequest.newBuilder().setHost(host).setName(name).build(), MutationResult::parseFrom)
    fun resetIdentity() = call("reset_identity", ResetIdentityRequest.newBuilder().setHost(host).build(), MutationResult::parseFrom)
    fun setRelay(enabled: Boolean) = call("set_relay", SetRelayRequest.newBuilder().setHost(host).setEnabled(enabled).build(), MutationResult::parseFrom)
    fun accountCommand(request: AccountCommandRequest) = call("account_command", request.toBuilder().setHost(host).build(), AccountCommandReply::parseFrom)
    fun accountPoll(request: AccountPollRequest) = call("account_poll", request.toBuilder().setHost(host).build(), AccountPollReply::parseFrom)
    fun accountPlatform(request: AccountPlatformRequest) = call("account_platform", request.toBuilder().setHost(host).build(), MutationResult::parseFrom)
}
