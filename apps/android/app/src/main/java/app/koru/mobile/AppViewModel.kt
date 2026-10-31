// SPDX-License-Identifier: MPL-2.0

package app.koru.mobile

import android.content.Intent
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import app.koru.mobile.domain.*
import app.koru.mobile.services.ReleasedFiles
import app.koru.mobile.services.DocumentSession
import app.koru.mobile.services.IncomingShare
import app.koru.mobile.services.DeviceLinkState
import app.koru.mobile.sharing.RecentDestination
import java.util.Base64
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.*
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

data class AppUiState(
    val loading: Boolean = true,
    val error: AppFailure? = null,
    val activeProfileId: ProfileId? = null,
    val activeWorkspaceId: WorkspaceId? = null,
    val activeDocumentId: DocumentId? = null,
)

data class ShareUiState(
    val receiving: Boolean = false,
    val pending: IncomingShare? = null,
    val selectedProfileId: ProfileId? = null,
    val saving: Boolean = false,
    val savedFiles: List<DocumentSummary> = emptyList(),
    val savedWorkspaceId: WorkspaceId? = null,
    /** The destinations the last shares on this device went to, most recent
     *  first — the screen's "Recent" rows. */
    val recents: List<RecentDestination> = emptyList(),
    /** The name each incoming file saves under, editable on the share screen;
     *  seeded with the reader's default names. */
    val names: List<String> = emptyList(),
    /** The note each incoming file saves with, editable on the share screen;
     *  empty means no note. */
    val notes: List<String> = emptyList(),
)

@OptIn(ExperimentalCoroutinesApi::class)
class AppViewModel(private val container: AppContainer) : ViewModel() {
    private val mutableState = MutableStateFlow(AppUiState())
    val state = mutableState.asStateFlow()
    val deviceLink = container.deviceLink.state
    val linkedDevices = container.deviceLink.devices
    val thisDevice = container.deviceLink.thisDevice
    val relinkAvailable = container.deviceLink.relinkAvailable
    val relayEnabled = container.deviceLink.relayEnabled
    val relayStatus = container.deviceLink.relayStatus
    val accountDiagnostics = container.deviceLink.accountDiagnostics
    val updateNotice = container.updates.notice
    fun downloadUpdate() = container.updates.download()
    fun installUpdate(context: android.content.Context) = container.updates.install(context)
    fun dismissUpdate() = container.updates.dismiss()
    val account = container.account.screen
    fun accountAction(intent: app.koru.mobile.services.AccountIntent, locale: String, ownerScope: Long, profile: ProfileId? = null, accountId: String? = null, profiles: List<ProfileId> = emptyList()) =
        container.account.act(intent, ownerScope, locale, profile, accountId, profiles)
    val cloudSync = container.account.cloudSyncState
    fun setProfileCloudSync(profile: ProfileId, enabled: Boolean, ownerScope: Long) =
        container.account.setCloudSync(profile, enabled, ownerScope)
    /** **How far one Profile's Cloud sync has got**, for the Sync status
     *  screen. One poll for the app, pointed at whichever Profile the screen
     *  shows; it reads only while that screen is visible, and an answer about
     *  a Profile the person has left is dropped. */
    internal val cloudProgress = app.koru.mobile.services.CloudProgressPresenter(container.cloudProgress)
    val contentSyncStatus = container.contentSyncStatus
    val profiles = container.profiles.observeProfiles().stateIn(viewModelScope, SharingStarted.WhileSubscribed(5_000), emptyList())
    val visibleProfiles = kotlinx.coroutines.flow.combine(profiles, account) { rows, state ->
        app.koru.mobile.domain.visibleProfiles(rows, state.activeAccountId)
    }.stateIn(viewModelScope, SharingStarted.WhileSubscribed(5_000), app.koru.mobile.domain.VisibleProfiles(emptyList(), 0))
    private val activeProfile = MutableStateFlow<ProfileId?>(null)
    private val activeWorkspace = MutableStateFlow<WorkspaceId?>(null)
    private val activeDocument = MutableStateFlow<DocumentId?>(null)
    val workspaces = activeProfile.flatMapLatest { id -> id?.let(container.workspaces::observeWorkspaces) ?: flowOf(emptyList()) }
        .stateIn(viewModelScope, SharingStarted.WhileSubscribed(5_000), emptyList())
    val documents = activeWorkspace.flatMapLatest { id -> id?.let(container.documents::observeDocuments) ?: flowOf(emptyList()) }
        .stateIn(viewModelScope, SharingStarted.WhileSubscribed(5_000), emptyList())
    private val mutableShare = MutableStateFlow(ShareUiState())
    val share = mutableShare.asStateFlow()
    val shareWorkspaces = mutableShare
        .map { it.selectedProfileId }
        .distinctUntilChanged()
        .flatMapLatest { id -> id?.let(container.workspaces::observeWorkspaces) ?: flowOf(emptyList()) }
        .stateIn(viewModelScope, SharingStarted.WhileSubscribed(5_000), emptyList())
    /** The workspaces of every profile a recent destination names — the share
     *  screen validates its "Recent" rows against this live inventory. */
    val shareRecentWorkspaces = mutableShare
        .map { share -> share.recents.map { it.profileId }.distinct() }
        .distinctUntilChanged()
        .flatMapLatest { profileIds ->
            if (profileIds.isEmpty()) {
                flowOf(emptyList())
            } else {
                combine(profileIds.map(container.workspaces::observeWorkspaces)) { groups ->
                    groups.flatMap { rows -> rows }
                }
            }
        }
        .stateIn(viewModelScope, SharingStarted.WhileSubscribed(5_000), emptyList())
    private val mutableDocument = MutableStateFlow<DocumentSnapshot?>(null)
    val document = mutableDocument.asStateFlow()
    private val mutableRun = MutableStateFlow<RunSnapshot?>(null)
    /** The delegated run on screen, refreshed by its observer (P1.18). */
    val run = mutableRun.asStateFlow()
    private var runJob: Job? = null
    private var session: DocumentSession? = null
    val documentEdits = app.koru.mobile.services.DocumentEdits(viewModelScope) { id, command ->
        // Keep an already admitted write bound to its document even if navigation changes.
        val writer = container.documents.open(id)
        try {
            writer.execute(command)
            writer.snapshots.first().also { snapshot ->
                if (activeDocument.value == id) acceptDocumentSnapshot(snapshot)
            }
        } finally { writer.close() }
    }
    private var snapshotJob: Job? = null
    private var eventJob: Job? = null
    private val commands = Channel<Pair<DocumentId, DocumentCommand>>(Channel.UNLIMITED)

    init {
        mutableState.update { it.copy(loading = false) }
        viewModelScope.launch {
            container.pendingLink.filterNotNull().collect { link ->
                container.pendingLink.value = null
                runCatching { container.deviceLink.link(link) }.onFailure(::showError)
            }
        }
        viewModelScope.launch {
            for ((id, command) in commands) {
                val current = session
                if (current?.documentId != id) continue
                runCatching { current.execute(command) }.onFailure(::showError)
            }
        }
    }

    fun selectProfile(id: ProfileId) {
        activeProfile.value = id
        activeWorkspace.value = null
        activeDocument.value = null
        updateActive()
        container.logger.event("navigate_workspaces", mapOf("profile_id" to id.value))
    }

    fun selectWorkspace(id: WorkspaceId) {
        activeWorkspace.value = id
        activeDocument.value = null
        updateActive()
        container.logger.event("navigate_documents", mapOf("workspace_id" to id.value))
    }

    fun openDocument(id: DocumentId) {
        activeDocument.value = id
        updateActive()
        viewModelScope.launch {
            runCatching {
                snapshotJob?.cancel()
                eventJob?.cancel()
                session?.close()
                mutableDocument.value = null
                val opened = container.documents.open(id)
                session = opened
                snapshotJob = launch {
                    try {
                        opened.snapshots.collect { snapshot ->
                            if (activeDocument.value != id) return@collect
                            acceptDocumentSnapshot(snapshot)
                            landIn(snapshot.workspaceId)
                        }
                    } catch (cancelled: CancellationException) {
                        throw cancelled
                    } catch (error: Exception) {
                        showError(error)
                    }
                }
                eventJob = launch { opened.events.collect { event -> container.logger.event("document_event", mapOf("document_id" to id.value, "event" to event::class.simpleName)) } }
            }.onFailure(::showError)
        }
    }

    private fun acceptDocumentSnapshot(snapshot: DocumentSnapshot) {
        val current = mutableDocument.value
        if (current?.id == snapshot.id && current.observation > snapshot.observation) return
        mutableDocument.value = snapshot
    }

    /**
     * A followed link may land in another workspace — the catalog, not the
     * list the user came from, says where a document lives. Keep the active
     * workspace and profile truthful about what is on screen; the route a
     * screen re-selects on entry is unaffected, so Back still returns where
     * the user came from.
     */
    private suspend fun landIn(workspaceId: WorkspaceId) {
        if (activeWorkspace.value == workspaceId) return
        activeWorkspace.value = workspaceId
        runCatching { container.workspaces.get(workspaceId) }
            .onSuccess { workspace -> activeProfile.value = workspace.profileId }
        updateActive()
    }

    /** Open a run read-only (D15): observe its header until [closeRun]. */
    fun openRun(id: DocumentId) {
        runJob?.cancel()
        mutableRun.value = null
        runJob = viewModelScope.launch {
            try {
                container.runs.observe(id).collect { snapshot ->
                    mutableRun.value = snapshot
                    landIn(snapshot.workspaceId)
                }
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (error: Exception) {
                showError(error)
            }
        }
    }

    fun closeRun() {
        runJob?.cancel()
        runJob = null
        mutableRun.value = null
    }

    /** One window of a run's activity, newest first from `newestFirstFrom`. */
    suspend fun runTimeline(id: DocumentId, newestFirstFrom: Int, len: Int): RunTimelinePage =
        container.runs.timeline(id, newestFirstFrom, len)

    fun createProfile(name: String, locale: String, onCreated: (ProfileId) -> Unit) = viewModelScope.launch {
        runCatching { container.profiles.create(name, locale) }
            .onSuccess { selectProfile(it.id); onCreated(it.id) }.onFailure(::showError)
    }

    fun createWorkspace(title: String, onCreated: (WorkspaceId) -> Unit) = viewModelScope.launch {
        val profileId = activeProfile.value ?: return@launch showError(AppFailure(AppErrorCode.InvalidData, "profile is not selected"))
        runCatching { container.workspaces.create(profileId, title) }
            .onSuccess { selectWorkspace(it.id); onCreated(it.id) }.onFailure(::showError)
    }

    fun renameProfile(id: ProfileId, displayName: String) = viewModelScope.launch {
        runCatching { container.profiles.rename(id, displayName) }.onFailure(::showError)
    }

    fun renameWorkspace(id: WorkspaceId, title: String) = viewModelScope.launch {
        runCatching { container.workspaces.rename(id, title) }.onFailure(::showError)
    }

    fun setWorkspaceArchived(id: WorkspaceId, archived: Boolean) = viewModelScope.launch {
        runCatching { container.workspaces.setArchived(id, archived) }.onFailure(::showError)
    }

    fun deleteWorkspace(id: WorkspaceId) = viewModelScope.launch {
        runCatching { container.workspaces.delete(id) }.onFailure(::showError)
    }

    fun restoreWorkspace(id: WorkspaceId) = viewModelScope.launch {
        runCatching { container.workspaces.restore(id) }.onFailure(::showError)
    }

    fun createDocument(title: String, onCreated: (DocumentId) -> Unit) = viewModelScope.launch {
        val workspaceId = activeWorkspace.value ?: return@launch showError(AppFailure(AppErrorCode.InvalidData, "workspace is not selected"))
        runCatching { container.documents.create(workspaceId, title) }
            .onSuccess { openDocument(it.id); onCreated(it.id) }.onFailure(::showError)
    }

    fun receiveShare(intent: Intent) {
        mutableShare.value = ShareUiState(receiving = true)
        viewModelScope.launch {
            runCatching { withContext(Dispatchers.IO) { container.shareIntents.read(intent) } }
                .onSuccess { incoming ->
                    mutableShare.value = ShareUiState(
                        pending = incoming,
                        recents = container.recents.load(),
                        names = incoming?.files?.map { it.name }.orEmpty(),
                        notes = incoming?.files?.map { it.note.orEmpty() }.orEmpty(),
                    )
                }
                .onFailure { error ->
                    mutableShare.value = ShareUiState()
                    showError(error)
                }
        }
    }

    fun selectShareProfile(id: ProfileId) {
        mutableShare.update { it.copy(selectedProfileId = id) }
    }

    fun setShareFileName(index: Int, name: String) {
        mutableShare.update { state ->
            if (index !in state.names.indices) return@update state
            state.copy(names = state.names.toMutableList().also { it[index] = name })
        }
    }

    fun setShareFileNote(index: Int, note: String) {
        mutableShare.update { state ->
            if (index !in state.notes.indices) return@update state
            state.copy(notes = state.notes.toMutableList().also { it[index] = note })
        }
    }

    fun saveSharedContent(workspaceId: WorkspaceId) {
        val current = mutableShare.value
        val incoming = current.pending ?: return
        val profileId = current.selectedProfileId ?: return showError(
            AppFailure(AppErrorCode.InvalidData, "profile is not selected"),
        )
        if (current.saving) return
        mutableShare.update { it.copy(saving = true) }
        viewModelScope.launch {
            runCatching {
                val workspace = container.workspaces.get(workspaceId)
                if (workspace.profileId != profileId) {
                    throw AppFailure(AppErrorCode.InvalidData, "workspace does not belong to the selected profile")
                }
                container.files.save(workspaceId, applyShareEdits(incoming, current.names, current.notes))
            }.onSuccess { saved ->
                container.recents.record(profileId, workspaceId)
                selectProfile(profileId)
                selectWorkspace(workspaceId)
                mutableShare.update {
                    it.copy(saving = false, savedFiles = saved, savedWorkspaceId = workspaceId)
                }
            }.onFailure { error ->
                mutableShare.update { it.copy(saving = false) }
                showError(error)
            }
        }
    }

    fun clearShare() {
        mutableShare.value = ShareUiState()
    }

    /** The share with the screen's edited names and notes applied. A blank
     *  name edit keeps the default; the save path's own `safeDisplayName` is
     *  the sanitizer. A blank note means no note. */
    private fun applyShareEdits(share: IncomingShare, names: List<String>, notes: List<String>): IncomingShare {
        if (names.size != share.files.size || notes.size != share.files.size) return share
        return IncomingShare(
            share.files.mapIndexed { index, file ->
                val renamed = names[index].takeIf { it.isNotBlank() }?.let { file.copy(name = it) } ?: file
                renamed.copy(note = notes[index].trim().takeIf { it.isNotEmpty() })
            },
        )
    }

    fun renameDocument(id: DocumentId, title: String) = viewModelScope.launch {
        runCatching { container.documents.rename(id, title) }.onFailure(::showError)
    }

    /** Open the file's note — its existing one, or *Add note*'s ensured one.
     *  A note opens like any other document; back returns to the list. */
    fun openFileNote(file: DocumentSummary, onOpen: (DocumentId) -> Unit) = viewModelScope.launch {
        runCatching { file.noteId ?: container.files.ensureNote(file.id) }
            .onSuccess { openDocument(it); onOpen(it) }
            .onFailure(::showError)
    }

    fun moveDocumentToTrash(id: DocumentId) = viewModelScope.launch {
        runCatching { container.documents.moveToTrash(id) }.onFailure(::showError)
    }

    fun send(command: DocumentCommand) {
        val id = activeDocument.value ?: return
        commands.trySend(id to command)
    }

    fun addBlock(kind: String) {
        val snapshot = document.value ?: return
        val block = when (kind) {
            "task" -> Block(BlockId.new(), content = BlockContent.Task(RichText("")))
            "list" -> Block(BlockId.new(), properties = BlockProperties(listStyle = ListStyle.Bullet), content = BlockContent.ListContainer(ListStyle.Bullet))
            else -> Block(BlockId.new(), content = BlockContent.Paragraph(RichText("")))
        }
        send(DocumentCommand.CreateBlock(block, snapshot.blocks.count { it.parentId == null }))
        if (kind == "list") {
            send(DocumentCommand.CreateBlock(Block(BlockId.new(), block.id, content = BlockContent.Paragraph(RichText(""))), 0))
        }
    }

    fun addSampleImage() = viewModelScope.launch {
        val bytes = Base64.getDecoder().decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=")
        runCatching {
            val blob = container.blobs.put(bytes, "image/png", 1, 1)
            val block = Block(BlockId.new(), properties = BlockProperties(name = "Sample image"), content = BlockContent.Image(blob))
            send(DocumentCommand.CreateBlock(block, Int.MAX_VALUE))
        }.onFailure(::showError)
    }

    fun insertReference(blockId: BlockId, atUtf8: Int) {
        val target = documents.value.firstOrNull { it.id != activeDocument.value } ?: return
        send(DocumentCommand.InsertInlineObject(
            blockId,
            InlineObject(InlineObjectId.new(), atUtf8, "document", EntityReference.Document(target.id), target.title),
        ))
    }

    suspend fun loadBlob(reference: BlobReference) = container.blobs.load(reference)

    fun observeBlob(reference: BlobReference) = container.blobs.observeLoad(reference)

    /**
     * The blob a `kind == "file"` document is, read on demand.
     *
     * **Asked for rather than carried on every row.** `DocumentSummary` is
     * what a list draws, and widening it with a size and a blob id would make
     * every Documents screen pay for a detail only one sheet ever shows.
     *
     * It cannot be read by opening the document: a file is not a node tree, so
     * the host refuses that with `document_is_file`. `store.file_blob` reads
     * the manifest and the file's one node instead.
     */
    suspend fun fileBlob(document: DocumentSummary): BlobReference? =
        if (document.kind != "file") null
        else container.blobs.fileBlob(document.id, document.workspaceId)

    /**
     * Put a file on disk so another application can open it, and hand back
     * where it landed.
     *
     * The export waits for the bytes when this device does not hold them, so
     * `onReady` means a complete file — never a placeholder another app would
     * fail to play. A refusal surfaces as the app's ordinary error.
     */
    fun exportFile(document: DocumentSummary, onReady: (java.io.File, String) -> Unit) =
        viewModelScope.launch {
            runCatching {
                val reference = fileBlob(document)
                    ?: throw AppFailure(AppErrorCode.InvalidData, "file_has_no_blob")
                // The type Android is handed, which is not always the type
                // the sender declared — see `resolveMimeType`.
                container.blobs.export(reference, document.title) to
                    app.koru.mobile.services.resolveMimeType(reference.mimeType, document.title)
            }.fold(
                onSuccess = { (file, mime) -> onReady(file, mime) },
                onFailure = ::showError,
            )
        }

    /** No installed application handles this file's type. The export
     *  succeeded; there is simply nothing on the phone that opens it. */
    fun reportNoHandler() =
        showError(AppFailure(AppErrorCode.InvalidData, "no_app_for_file"))

    /** **The explicit download override** (D50): somebody is looking at a
     *  picture held back by a metered connection and asked for that one. It
     *  goes through the pause and to the front of the queue; the load this
     *  screen is already observing reports the arrival. */
    fun requestBlob(reference: BlobReference) = viewModelScope.launch {
        runCatching { container.blobs.request(reference) }.onFailure(::showError)
    }

    /** Whether this person has allowed file and picture transfers on a paid
     *  connection. Off by default — a phone that quietly spent somebody's
     *  data allowance on holiday is what D50 exists to prevent. */
    val picturesOnCellular = container.blobPolicy.allowOnMetered

    fun setPicturesOnCellular(allowed: Boolean) = viewModelScope.launch {
        runCatching { container.blobPolicy.setAllowOnMetered(allowed) }.onFailure(::showError)
    }

    /** **What this Workspace's file transfers are doing**, for as long as one
     *  screen is looking. Counts and one stable code (§10, §15); the sentence
     *  a person reads is the screen's own, in their language. */
    fun observeTransfers(workspace: WorkspaceId) = container.blobs.observeTransfers(workspace)

    /** What the last *Free up space* did, until the person dismisses it. An
     *  acknowledgement rather than a state: it never survives a dismissal. */
    val lastRelease = MutableStateFlow<ReleasedFiles?>(null)

    /** **Free up space, confirmed.** Every copy Koru Cloud holds leaves this
     *  phone; every reference stays; the engine checks each file first. */
    fun clearLocalFiles(workspace: WorkspaceId) = viewModelScope.launch {
        runCatching { container.blobs.releaseLocal(workspace) }
            .onSuccess { lastRelease.value = it }
            .onFailure(::showError)
    }

    fun dismissRelease() { lastRelease.value = null }

    fun clearError() { mutableState.update { it.copy(error = null) } }

    fun linkDevice(code: String) = viewModelScope.launch {
        runCatching { container.deviceLink.link(code.trim()) }.onFailure(::showError)
    }

    suspend fun startPairing() = container.deviceLink.startPairing()
    fun cancelPairing() = viewModelScope.launch { container.deviceLink.cancelPairing() }
    fun unlinkDevice(deviceId: String) = viewModelScope.launch {
        runCatching { container.deviceLink.unlink(deviceId) }.onFailure(::showError)
    }

    suspend fun deviceHistory(deviceId: String, before: Long? = null) =
        container.deviceLink.history(deviceId, before)
    fun renameThisDevice(name: String) = viewModelScope.launch {
        runCatching { container.deviceLink.rename(name) }.onFailure(::showError)
    }
    fun setRelayEnabled(enabled: Boolean) = viewModelScope.launch {
        runCatching { container.deviceLink.setRelayEnabled(enabled) }.onFailure(::showError)
    }

    /** Set this device's unusable identity aside and mint a new one, keeping
     *  every workspace. Only reachable from the identity-mismatch banner, and
     *  refused by the native host on a healthy device. */
    fun resetDeviceIdentity() = viewModelScope.launch {
        runCatching { container.deviceLink.resetIdentity() }.onFailure(::showError)
    }

    private fun showError(error: Throwable) {
        val failure = error as? AppFailure ?: AppFailure(AppErrorCode.Unknown, error.message ?: "unknown error")
        mutableState.update { it.copy(error = failure) }
        container.logger.event("application_error", mapOf(
            "code" to failure.code.name,
            "message" to (failure.message ?: "-"),
        ))
    }

    private fun updateActive() {
        mutableState.update {
            it.copy(activeProfileId = activeProfile.value, activeWorkspaceId = activeWorkspace.value, activeDocumentId = activeDocument.value)
        }
    }

    override fun onCleared() {
        snapshotJob?.cancel()
        eventJob?.cancel()
        runJob?.cancel()
        commands.close()
        super.onCleared()
    }
}
