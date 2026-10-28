// SPDX-License-Identifier: MPL-2.0

package app.koru.mobile.services

import android.util.Log
import app.koru.mobile.domain.*
import app.koru.mobile.logging.AppLogger
import app.koru.mobile.sharing.safeDisplayName
import app.koru.mobile.sharing.safeMimeType
import app.koru.mobile.sharing.stageSharedFile
import java.io.File
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.delay
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.buffer
import kotlinx.coroutines.flow.collect
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.flatMapLatest
import kotlinx.coroutines.flow.flowOf
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asSharedFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.channelFlow
import kotlinx.coroutines.flow.emptyFlow
import kotlinx.coroutines.flow.flow
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch

data class ContentSyncStatus(
    val syncing: Boolean = false,
    /** True only after the Cloud document transport reports live readiness.
     * Peer activity, account sign-in and subscription state do not set it. */
    val cloudActive: Boolean = false,
    val profileCount: Int = 0,
    val workspaceCount: Int = 0,
    val documentCount: Int = 0,
)

/** Mobile projection of the embedded, replicated Store. */
class NativeContentServices(
    private val native: NativeDeviceLinkService,
    private val logger: AppLogger,
    private val scratch: File,
) : ProfileService, WorkspaceService, DocumentService, BlobService, WorkspaceFileService {
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    private val inventory = MutableStateFlow(NativeInventory())

    /**
     * The §11 announcement as this app reads it: the hash of every picture the
     * host has made durable since the last time anybody looked.
     *
     * **Drained on the inventory loop rather than on a loop of its own.** The
     * host's arrival window is a few integers in memory, and reading it is one
     * call across the shim; adding a second interval to this app would be the
     * timer D244 says a phone must not hold. `replay = 0` because an arrival
     * is news, not state: what is durable is the Store's answer, and a screen
     * that opens afterwards reads the bytes rather than an old notification.
     */
    private val mutableArrivals = kotlinx.coroutines.flow.MutableSharedFlow<String>(
        extraBufferCapacity = 64,
        onBufferOverflow = kotlinx.coroutines.channels.BufferOverflow.DROP_OLDEST,
    )
    internal val arrivals: kotlinx.coroutines.flow.SharedFlow<String> =
        mutableArrivals.asSharedFlow()
    private var arrivalSequence = 0L
    private val mutableSyncStatus = MutableStateFlow(ContentSyncStatus())
    val syncStatus: StateFlow<ContentSyncStatus> = mutableSyncStatus.asStateFlow()

    init {
        scope.launch {
            native.syncEvent.collectLatest { event ->
                val syncing = when (event?.stage) {
                    null -> mutableSyncStatus.value.syncing
                    "sync_complete", "catalog_failed" -> false
                    else -> true
                }
                mutableSyncStatus.value = mutableSyncStatus.value.copy(syncing = syncing)
            }
        }
        scope.launch {
            native.state.collectLatest { linkState ->
                if (linkState is DeviceLinkState.Linked) {
                    mutableSyncStatus.value = mutableSyncStatus.value.copy(syncing = true)
                    logger.event("sync_started", mapOf("direction" to "peer_to_local"))
                } else {
                    mutableSyncStatus.value = mutableSyncStatus.value.copy(syncing = false)
                }
            }
        }
        scope.launch {
            while (isActive) {
                runCatching { native.inventory() }
                    .onSuccess { next ->
                        val previous = inventory.value
                        inventory.value = next
                        mutableSyncStatus.value = ContentSyncStatus(
                            syncing = mutableSyncStatus.value.syncing,
                            cloudActive = mutableSyncStatus.value.cloudActive,
                            profileCount = next.profiles.size,
                            workspaceCount = next.workspaces.size,
                            documentCount = next.documents.size,
                        )
                        if (next != previous) {
                            logPeerObjects(previous, next)
                            logger.event("local_inventory", mapOf(
                                "profiles" to next.profiles.size.toString(),
                                "workspaces" to next.workspaces.size.toString(),
                                "documents" to next.documents.size.toString(),
                                "deleted_workspaces" to next.deletedWorkspaces.toString(),
                                "trashed_documents" to next.trashedDocuments.toString(),
                                "failed_workspaces" to next.failedWorkspaces.toString(),
                                "hidden_runs" to next.hiddenRuns.toString(),
                            ))
                        }
                    }
                    .onFailure {
                        logger.event(
                            "native_inventory_error",
                            mapOf("message" to (it.message ?: "unknown")),
                        )
                    }
                drainArrivals()
                delay(INVENTORY_POLL_MILLIS)
            }
        }
    }

    /** Ask the host what became durable since the last drain, and tell
     *  whatever is showing a placeholder for it. */
    private suspend fun drainArrivals() {
        val reply = runCatching { native.blobArrivals("", arrivalSequence) }.getOrNull() ?: return
        if (reply.sequence == arrivalSequence) return
        arrivalSequence = reply.sequence
        for (arrival in reply.arrivalsList) {
            // Unconditional and small: a picture arriving is news about
            // somebody's content, and it is the line that explains a
            // placeholder turning into an image.
            logger.event(
                "blob_arrived",
                mapOf("workspace_id" to arrival.workspaceId, "blob_id" to arrival.blobHash),
            )
            mutableArrivals.tryEmit(arrival.blobHash)
        }
    }

    override fun observeProfiles(): Flow<List<Profile>> = inventory
        .map { state ->
            state.profiles.map { it.toProfile() }
        }
        .distinctUntilChanged()

    override suspend fun get(id: ProfileId): Profile = observeProfilesSnapshot()
        .firstOrNull { it.id == id }
        ?: notFound("profile", id.value)

    override suspend fun create(displayName: String, locale: String): Profile = unsupported()

    override suspend fun rename(id: ProfileId, displayName: String): Profile {
        native.mutateProfile(id.value, "rename", displayName)
        inventory.value = native.inventory()
        logger.event("profile_change", mapOf(
            "direction" to "local",
            "action" to "rename",
            "profile_id" to id.value,
        ))
        return get(id)
    }

    override fun observeWorkspaces(profileId: ProfileId): Flow<List<Workspace>> = inventory
        .map { state ->
            state.workspaces
                .filter { it.profileId == profileId.value }
                .map {
                    Workspace(
                        WorkspaceId(it.id), profileId, it.name, it.archived,
                        it.deletedAtMillis, it.modifiedAtMillis, it.icon, it.color,
                        it.leadingEmoji, it.displayName,
                    )
                }
        }
        .distinctUntilChanged()

    override suspend fun get(id: WorkspaceId): Workspace {
        val row = inventory.value.workspaces.firstOrNull { it.id == id.value }
            ?: notFound("workspace", id.value)
        return Workspace(
            WorkspaceId(row.id), ProfileId(row.profileId), row.name, row.archived,
            row.deletedAtMillis, row.modifiedAtMillis, row.icon, row.color,
            row.leadingEmoji, row.displayName,
        )
    }

    override suspend fun create(profileId: ProfileId, title: String): Workspace = unsupported()

    override suspend fun rename(id: WorkspaceId, title: String): Workspace =
        mutateWorkspace(id, "rename", title)

    override suspend fun setArchived(id: WorkspaceId, archived: Boolean): Workspace =
        mutateWorkspace(id, if (archived) "archive" else "unarchive")

    override suspend fun delete(id: WorkspaceId): Workspace = mutateWorkspace(id, "delete")

    override suspend fun restore(id: WorkspaceId): Workspace = mutateWorkspace(id, "restore")

    override fun observeDocuments(workspaceId: WorkspaceId): Flow<List<DocumentSummary>> = inventory
        .map { state ->
            state.documents
                .filter { it.workspaceId == workspaceId.value }
                .map {
                    DocumentSummary(
                        DocumentId(it.id), workspaceId, it.title, it.displayTitle,
                        it.leadingEmoji, it.kind, it.mime, it.noteId?.let(::DocumentId),
                        it.updatedAtMillis,
                    )
                }
        }
        .distinctUntilChanged()

    override suspend fun get(id: DocumentId): DocumentSummary {
        val row = inventory.value.documents.firstOrNull { it.id == id.value }
            ?: return resolve(id)
        return DocumentSummary(
            DocumentId(row.id), WorkspaceId(row.workspaceId), row.title,
            row.displayTitle, row.leadingEmoji, row.kind, row.mime,
            row.noteId?.let(::DocumentId), row.updatedAtMillis,
        )
    }

    /**
     * The store catalog, not the polled inventory, says where a document
     * lives. The inventory is a cache refreshed every half second and only
     * as complete as the last sync pass, so a link into a workspace this
     * device has not finished pulling must still resolve — the native layer
     * locates the workspace itself and names the layer that failed.
     */
    private suspend fun resolve(id: DocumentId): DocumentSummary {
        val snapshot = try {
            native.document(id)
        } catch (cancelled: kotlinx.coroutines.CancellationException) {
            // First, because it *is* an IllegalStateException: a person who
            // left the screen has not had a document fail to open.
            throw cancelled
        } catch (error: IllegalStateException) {
            val code = error.message ?: "unknown"
            // **Every failure leaves a line**, ids and the code only. A
            // document that would not open used to become "An unexpected
            // error occurred" on screen and nothing at all in the log, so the
            // next log pull could not say which layer refused it.
            Log.w("KoruMobile", "document_open_failed document_id=${id.value} code=$code")
            throw when (code) {
                "document_unlocated", "document_not_listed" ->
                    AppFailure(AppErrorCode.NotFound, "document ${id.value} was not found ($code)")
                // The Store worker did not answer. That is not "unexpected" —
                // it is the one failure a person can act on, by relaunching —
                // and it must not read as a bug in the document. The native
                // layer names which of the four it was; all four mean the same
                // thing here, which is that storage is not answering.
                "store_worker_timeout", "store_worker_busy", "store_worker_stopped",
                "store_worker_poisoned", "store_worker_unavailable" ->
                    AppFailure(AppErrorCode.Unavailable, "document ${id.value} failed to open ($code)")
                else ->
                    AppFailure(AppErrorCode.Unknown, "document ${id.value} failed to open ($code)")
            }
        } catch (error: Exception) {
            // Anything else — a reply that would not decode, a JNI fault — is
            // passed on as it was, but not silently: the class names the layer.
            Log.w(
                "KoruMobile",
                "document_open_failed document_id=${id.value} code=${error.javaClass.simpleName}",
            )
            throw error
        }
        return DocumentSummary(snapshot.id, snapshot.workspaceId, snapshot.title)
    }

    override suspend fun create(workspaceId: WorkspaceId, title: String): DocumentSummary = unsupported()

    override suspend fun rename(id: DocumentId, title: String): DocumentSummary {
        native.mutateDocument(id.value, "rename", title)
        inventory.value = native.inventory()
        logger.event("document_change", mapOf(
            "direction" to "local", "action" to "rename", "document_id" to id.value,
        ))
        return get(id)
    }

    override suspend fun moveToTrash(id: DocumentId) {
        native.mutateDocument(id.value, "trash")
        inventory.value = native.inventory()
        logger.event("document_change", mapOf(
            "direction" to "local", "action" to "trash", "document_id" to id.value,
        ))
    }

    override suspend fun open(id: DocumentId): DocumentSession {
        resolve(id)
        return NativeDocumentSession(native, id, logger)
    }

    override fun observeLoad(reference: BlobReference): Flow<BlobLoadState> = channelFlow {
        send(BlobLoadState.CheckingLocal)
        val peerState = launch {
            native.devices.collectLatest { devices ->
                val state = when {
                    devices.isEmpty() -> BlobLoadState.NoLinkedDevices
                    devices.none { it.connected } -> {
                        BlobLoadState.WaitingForPeer(listOfNotNull(devices.firstOrNull()?.displayName))
                    }
                    else -> BlobLoadState.Fetching(0, reference.size)
                }
                logger.event(
                    "blob_hydration",
                    mapOf(
                        "workspace_id" to (reference.workspaceId?.value ?: "-"),
                        "state" to when (state) {
                            BlobLoadState.NoLinkedDevices -> "no_linked_devices"
                            is BlobLoadState.WaitingForPeer -> "peer_offline"
                            else -> "fetching"
                        },
                    ),
                )
                send(state)
            }
        }
        val first = attempt(reference)
        peerState.cancel()
        send(first)
        if (first is BlobLoadState.Available) return@channelFlow

        // **The announcement (§11), on the path a phone has.** A desktop wakes
        // the document's room; Android has no room registry (D242), so the
        // host keeps an arrival window and this waits on it. Without it a
        // correct download sits on the disk behind a placeholder that never
        // changes — which is the defect D242 was written about, and §18's
        // case 8 is the test for it being forgotten on this platform.
        arrivals.collect { hash ->
            if (hash == reference.id.value) send(attempt(reference))
        }
    }

    override suspend fun save(
        workspaceId: WorkspaceId,
        share: IncomingShare,
    ): List<DocumentSummary> {
        if (share.files.isEmpty()) throw AppFailure(AppErrorCode.InvalidData, "shared content is empty")
        val imported = mutableListOf<DocumentId>()
        try {
            for (incoming in share.files) {
                val staged = stageSharedFile(scratch, incoming)
                try {
                    imported += DocumentId(
                        native.importFile(
                            workspaceId.value,
                            safeDisplayName(incoming.name),
                            safeMimeType(incoming.mimeType),
                            staged.absolutePath,
                            incoming.note,
                        ),
                    )
                } finally {
                    staged.delete()
                }
            }
        } catch (error: Throwable) {
            // A Store import is committed the moment it returns, so a later
            // item failing cannot be undone by deleting a temporary file. The
            // batch is still all-or-nothing from the sharer's side: what
            // landed is binned again, best effort, and the failure is what
            // they are told about.
            imported.forEach { id -> runCatching { native.mutateDocument(id.value, "trash") } }
            inventory.value = native.inventory()
            throw error
        }
        inventory.value = native.inventory()
        logger.event(
            "shared_content_saved",
            mapOf("workspace_id" to workspaceId.value, "file_count" to imported.size),
        )
        return imported.mapNotNull { id -> runCatching { get(id) }.getOrNull() }
    }

    override suspend fun ensureNote(file: DocumentId): DocumentId {
        val note = DocumentId(native.ensureFileNote(file.value))
        inventory.value = native.inventory()
        logger.event("document_change", mapOf(
            "direction" to "local", "action" to "ensure_note", "document_id" to file.value,
        ))
        return note
    }

    override suspend fun put(
        bytes: ByteArray,
        mimeType: String,
        width: Int?,
        height: Int?,
    ): BlobReference = unsupported()

    /** The workspace comes from the row that was tapped rather than a second
     *  lookup: a file lives in exactly one Workspace and the list already
     *  knows which. */
    override suspend fun fileBlob(documentId: DocumentId, workspaceId: WorkspaceId): BlobReference? {
        val (hash, mime, size) = native.fileBlob(documentId)
        return BlobReference(BlobId(hash), mime, size, workspaceId = workspaceId)
    }

    /**
     * Stream one blob into this app's own storage and return the file.
     *
     * **Named by content, not by title.** Two files can share a name and a
     * title can be renamed; the blob id cannot, so the export is addressed by
     * it and the person's name rides only on the extension. That also makes
     * the re-open below sound: a file already on disk under that id is the
     * same bytes, so opening a video twice does not copy it twice.
     */
    override suspend fun export(reference: BlobReference, fileName: String): File {
        val workspace = reference.workspaceId
            ?: throw AppFailure(AppErrorCode.InvalidData, "file has no workspace")
        val directory = File(scratch, "exports").apply { mkdirs() }
        val target = File(directory, exportName(reference, fileName))
        // A complete export is reusable: same blob id, same bytes, and the
        // host already verified them on the way out.
        if (target.isFile && target.length() == reference.size) return target
        // The host answers a stable code when the bytes are nowhere
        // reachable; the dialog turns that into a sentence, not "unexpected".
        val written = try {
            native.exportBlob(workspace, reference.id, target.absolutePath)
        } catch (failure: IllegalStateException) {
            throw AppFailure(AppErrorCode.BlobHeld, failure.message ?: "blob_export_failed")
        }
        if (reference.size > 0 && written != reference.size) {
            target.delete()
            throw AppFailure(AppErrorCode.InvalidData, "blob_export_short")
        }
        return target
    }

    override suspend fun load(reference: BlobReference): ByteArray {
        val workspace = reference.workspaceId
            ?: throw AppFailure(AppErrorCode.InvalidData, "image has no workspace")
        return native.blob(workspace, reference.id)
    }

    /**
     * One reading of a picture: the bytes, a pause, or a failure.
     *
     * The pause is read from the engine's own status and **only when the load
     * produced no bytes** — never guessed from the error. A picture that did
     * not arrive on a metered connection is waiting for a network this device
     * may use, a person may ask for that one anyway (D50), and calling it
     * "unavailable" is the §13 defect this separates.
     */
    private suspend fun attempt(reference: BlobReference): BlobLoadState {
        val workspace = reference.workspaceId?.value ?: "-"
        val result = runCatching { load(reference) }
        result.getOrNull()?.let { bytes ->
            logger.event(
                "blob_hydration",
                mapOf(
                    "workspace_id" to workspace,
                    "state" to "available",
                    "bytes" to bytes.size.toString(),
                ),
            )
            return BlobLoadState.Available(bytes)
        }
        val paused = reference.workspaceId
            ?.let { runCatching { native.blobStatus(it.value) }.getOrNull() }
            ?.takeIf { it.phase == BlobTransferPhase.Paused }
        if (paused != null) {
            logger.event(
                "blob_hydration",
                mapOf(
                    "workspace_id" to workspace,
                    "state" to "paused",
                    "pending_reason" to paused.pendingReason,
                ),
            )
            // **Only the holds a request can carry through are requestable.**
            // D50's two paid-connection pauses are; no network, a pause
            // somebody chose and an account that has not connected this
            // Workspace to Koru Cloud are not, and offering "Download now"
            // against one of those is a button that does nothing.
            return BlobLoadState.Paused(
                paused.pendingReason,
                requestable = BlobHold.of(paused.pendingReason)
                    ?.clearedByAllowingCellular == true,
            )
        }
        val code = result.exceptionOrNull()?.message ?: "blob_load_failed"
        // A peer that lacks the bytes is a wait, not a failure, and the trace
        // says which: `state=failed error_code=blob_not_found` read as damage.
        // `store.blob` carries no `waiting_on` (its wait is `blocking_wait`,
        // which never returns a park), so nobody is named here.
        val state = blobLoadFailure(code)
        logger.event(
            "blob_hydration",
            mapOf(
                "workspace_id" to workspace,
                "state" to if (state is BlobLoadState.Failed) "failed" else "waiting",
                "error_code" to code,
            ),
        )
        return state
    }

    /** §10's `request_blob`: somebody is looking at a placeholder. */
    override suspend fun request(reference: BlobReference) {
        val workspace = reference.workspaceId
            ?: throw AppFailure(AppErrorCode.InvalidData, "image has no workspace")
        native.requestBlob(workspace.value, reference.id.value)
        logger.event(
            "blob_requested",
            mapOf("workspace_id" to workspace.value, "blob_id" to reference.id.value),
        )
    }

    override suspend fun transferStatus(workspaceId: WorkspaceId): BlobTransferSnapshot? =
        runCatching { native.blobStatus(workspaceId.value) }.getOrNull()

    override suspend fun releaseLocal(workspaceId: WorkspaceId): ReleasedFiles {
        val released = native.releaseBlobs(workspaceId.value)
        // News: a person pressed this and bytes left the disk. Counts only.
        logger.event(
            "blobs_released",
            mapOf(
                "workspace_id" to workspaceId.value,
                "released" to released.released.toString(),
                "kept" to released.kept.toString(),
                "bytes_freed" to released.bytesFreed.toString(),
                "error_code" to released.failureCode,
            ),
        )
        if (released.failureCode.isNotEmpty() && released.released == 0 && released.kept == 0) {
            throw AppFailure(AppErrorCode.InvalidData, released.failureCode)
        }
        return released
    }

    private suspend fun mutateWorkspace(id: WorkspaceId, action: String, title: String = ""): Workspace {
        native.mutateWorkspace(id.value, action, title)
        inventory.value = native.inventory()
        logger.event("workspace_change", mapOf(
            "direction" to "local",
            "action" to action,
            "workspace_id" to id.value,
        ))
        return get(id)
    }

    private fun observeProfilesSnapshot() = inventory.value.profiles.map {
        it.toProfile()
    }

    private fun logPeerObjects(previous: NativeInventory, next: NativeInventory) {
        val oldProfiles = previous.profiles.associateBy { it.id }
        val oldWorkspaces = previous.workspaces.associateBy { it.id }
        val oldDocuments = previous.documents.associateBy { it.id }
        next.profiles.filter { it.id !in oldProfiles }.forEach { profile ->
            logger.peerObject("peer_object_received", mapOf(
                "object" to "profile", "profile_id" to profile.id,
            ))
        }
        next.workspaces.forEach { workspace ->
            val old = oldWorkspaces[workspace.id]
            if (old != null) {
                if (old.name != workspace.name) logWorkspaceChange(workspace.id, "rename")
                if (old.archived != workspace.archived) {
                    logWorkspaceChange(workspace.id, if (workspace.archived) "archive" else "unarchive")
                }
                if (old.deletedAtMillis != workspace.deletedAtMillis) {
                    logWorkspaceChange(workspace.id, if (workspace.deletedAtMillis == null) "restore" else "delete")
                }
            }
            if (old == null || old != workspace) logger.peerObject("peer_object_received", mapOf(
                "object" to "workspace",
                "workspace_id" to workspace.id,
                "archived" to workspace.archived,
                "deleted" to (workspace.deletedAtMillis != null),
            ))
        }
        next.documents.filter { it.id !in oldDocuments }.forEach { document ->
            logger.peerObject("peer_object_received", mapOf(
                "object" to "document",
                "workspace_id" to document.workspaceId,
                "document_id" to document.id,
            ))
        }
    }

    private fun logWorkspaceChange(workspaceId: String, action: String) {
        logger.event("workspace_change", mapOf(
            "direction" to "peer_to_local",
            "action" to action,
            "workspace_id" to workspaceId,
        ))
    }

    private fun notFound(kind: String, id: String): Nothing =
        throw AppFailure(AppErrorCode.NotFound, "$kind $id was not found")

    private fun unsupported(): Nothing =
        throw AppFailure(AppErrorCode.Unsupported, "mobile document editing is not part of this MVP slice")

    private companion object {
        const val INVENTORY_POLL_MILLIS = 500L
    }
}

private class NativeDocumentSession(
    private val native: NativeDeviceLinkService,
    override val documentId: DocumentId,
    private val logger: AppLogger,
) : DocumentSession {
    private val closed = MutableStateFlow(false)

    @OptIn(kotlinx.coroutines.ExperimentalCoroutinesApi::class)
    override val snapshots: Flow<DocumentSnapshot> = channelFlow {
        if (closed.value) return@channelFlow
        val observer = launch {
            native.documentChanges(documentId)
                .map { native.document(documentId) }
                .distinctUntilChanged()
                .flatMapLatest { snapshot ->
                    val references = snapshot.runReferences()
                    if (references.isEmpty()) flowOf(snapshot)
                    else combine(references.map { native.documentChanges(it) }) { Unit }
                        .buffer(Channel.CONFLATED)
                        .map { withRuns(snapshot) }
                }
                .collect { send(it) }
        }
        closed.first { it }
        observer.cancelAndJoin()
    }.buffer(Channel.CONFLATED).distinctUntilChanged().map { snapshot ->
        logger.event(
            "document_snapshot_observed",
            mapOf(
                "document_id" to documentId.value,
                "revision" to snapshot.revision,
                "blocks" to snapshot.blocks.size.toString(),
                "marks" to snapshot.blocks.sumOf {
                    it.content.diagnosticRichText()?.marks?.size ?: 0
                }.toString(),
                "bold_marks" to snapshot.blocks.sumOf { block ->
                    block.content.diagnosticRichText()?.marks?.count {
                        it.kind == MarkKind.Bold
                    } ?: 0
                }.toString(),
            ),
        )
        snapshot
    }

    /**
     * The runs the document's pills point at, refreshed by their own durable
     * commit wakes, even across workspaces or when first arriving on this
     * phone. The host keeps a fork per
     * open run and reuses it while the version stands, so an unchanged run
     * costs catalog reads and no fork. A run that has not reached this
     * phone yet is simply absent: the pill shows its stored label.
     */
    private suspend fun withRuns(snapshot: DocumentSnapshot): DocumentSnapshot {
        val referenced = snapshot.runReferences()
        if (referenced.isEmpty()) return snapshot
        val runs = referenced.mapNotNull { id ->
            runCatching { native.run(id) }.getOrNull()?.let { id to it }
        }.toMap()
        return snapshot.copy(runs = runs)
    }

    override val events: Flow<DocumentEvent> = emptyFlow()

    override suspend fun execute(command: DocumentCommand, operationId: String) {
        check(!closed.value) { "document_session_closed" }
        when (command) {
            is DocumentCommand.EditBlockText -> native.editBlockText(documentId, command)
            else -> throw AppFailure(AppErrorCode.Unsupported, "unsupported mobile body command")
        }
    }

    override suspend fun close() {
        closed.value = true
    }
}

internal fun NativeInventoryProfile.toProfile() = Profile(
    id = ProfileId(id),
    displayName = name,
    locale = locale,
    icon = icon,
    leadingEmoji = leadingEmoji,
    displayLabel = displayName,
    color = color,
    accountBinding = accountBinding,
)

private fun BlockContent.diagnosticRichText(): RichText? = when (this) {
    is BlockContent.Paragraph -> richText
    is BlockContent.Quote -> richText
    is BlockContent.Task -> richText
    is BlockContent.Code -> richText
    is BlockContent.Table -> richText
    is BlockContent.Contact -> name
    else -> null
}

/**
 * The type to hand Android, which is not always the type Koru stored.
 *
 * **A share arrives with whatever the sender called it.** A file manager
 * commonly says `application/octet-stream`, and Koru keeps that faithfully —
 * it is what the sender declared. But it is also the one type that tells
 * Android nothing, so the chooser offers everything and no video player:
 * `MVI_3212.mp4` opened a list of Google, Wallet and a terminal.
 *
 * So a generic label is treated as an absent one and the extension decides,
 * which is the only other evidence there is. A specific stored type always
 * wins — a sender that said `video/mp4` knows more than a suffix does.
 *
 * The small table exists so the common cases are pure and testable; anything
 * else falls through to Android's own database, which is unavailable in a
 * unit test and absent rather than wrong when it answers nothing.
 */
internal fun resolveMimeType(stored: String, fileName: String, lookup: (String) -> String? = ::systemMimeType): String {
    val generic = stored.isBlank() ||
        stored.equals("application/octet-stream", ignoreCase = true) ||
        stored.equals("*/*", ignoreCase = true) ||
        stored.equals("content/unknown", ignoreCase = true)
    if (!generic) return stored
    val extension = fileName.substringAfterLast('.', "").lowercase()
    if (extension.isEmpty()) return FALLBACK_MIME
    return COMMON_MIME[extension] ?: lookup(extension) ?: FALLBACK_MIME
}

private const val FALLBACK_MIME = "application/octet-stream"

/** The types a person is most likely to have saved into Koru, so the common
 *  path needs neither the framework nor a device to be right. */
private val COMMON_MIME = mapOf(
    "mp4" to "video/mp4", "m4v" to "video/mp4", "mov" to "video/quicktime",
    "wmv" to "video/x-ms-wmv", "avi" to "video/x-msvideo", "mkv" to "video/x-matroska",
    "webm" to "video/webm", "3gp" to "video/3gpp",
    "mp3" to "audio/mpeg", "m4a" to "audio/mp4", "wav" to "audio/wav",
    "aac" to "audio/aac", "ogg" to "audio/ogg", "flac" to "audio/flac",
    "jpg" to "image/jpeg", "jpeg" to "image/jpeg", "png" to "image/png",
    "gif" to "image/gif", "webp" to "image/webp", "heic" to "image/heic",
    "pdf" to "application/pdf", "txt" to "text/plain", "md" to "text/markdown",
    "zip" to "application/zip",
)

private fun systemMimeType(extension: String): String? =
    runCatching {
        android.webkit.MimeTypeMap.getSingleton().getMimeTypeFromExtension(extension)
    }.getOrNull()

/**
 * The on-disk name for an exported blob: its id, plus an extension a player
 * can recognise.
 *
 * **The extension is the only part that comes from the person.** Android picks
 * a handler largely from the MIME type, but some apps still read the suffix,
 * and a file called `<uuid>` with no extension opens in fewer of them. The
 * stem stays the blob id so two files with the same title cannot collide and a
 * rename cannot orphan an export.
 */
internal fun exportName(reference: BlobReference, fileName: String): String {
    val suffix = fileName.substringAfterLast('.', "")
        .lowercase()
        .filter { it.isLetterOrDigit() }
        .take(8)
    val id = reference.id.value.filter { it.isLetterOrDigit() || it == '-' }.take(64)
    return if (suffix.isEmpty()) id else "$id.$suffix"
}
