// SPDX-License-Identifier: MPL-2.0

package app.koru.mobile

import android.app.Application
import app.koru.mobile.logging.AppLogger
import app.koru.mobile.services.*
import app.koru.mobile.sharing.AndroidShareIntentReader
import app.koru.mobile.sharing.RecentDestinations
import android.net.Uri
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.async
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import app.koru.mobile.domain.DocumentId
import app.koru.mobile.domain.DocumentCommand

class AppContainer(application: Application) {
    val logger = AppLogger()
    val deviceLink = NativeDeviceLinkService(application, logger)
    val account = NativeAndroidAccountService(application, deviceLink)
    /** This device's connection, told to the shared transfer engine (D50). A
     *  `ConnectivityManager` callback, never a poll. */
    val blobPolicy = AndroidBlobPolicy(application, deviceLink, logger)
    private val content =
        NativeContentServices(deviceLink, logger, application.cacheDir.resolve("shared-content"))
    val contentSyncStatus = content.syncStatus
    val removalDrafts = PurgeDraftJournal(application.filesDir.resolve("document-purge-drafts"))
    val documentRemovals = content.removalPrompts
    val purgedDocumentIds = content.purgedDocumentIds
    val profiles: ProfileService = content
    val workspaces: WorkspaceService = content
    val documents: DocumentService = content
    val blobs: BlobService = content
    val files: WorkspaceFileService = content
    val runs: RunService = NativeRunService(deviceLink, logger)
    /** The Cloud sync progress screen's reads: the desktop's two Manage
     *  Profiles questions, answered by the embedded host. */
    internal val cloudProgress: CloudProgressSource = NativeCloudProgressSource(deviceLink, content)
    val shareIntents = AndroidShareIntentReader(application.contentResolver)
    val recents = RecentDestinations(application)
    /** The nightly's update notice; inert in every other build (§5.5). */
    val updates = AppUpdates(application, logger)
    val serviceImplementation = "native-link + replicated-store"
    internal val pendingLink = MutableStateFlow<String?>(null)
    private val removalPreparation = Mutex()
    private val removalScope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
    private data class RemovalEditor(
        val quiesce: suspend (DocumentId) -> List<DocumentCommand.EditBlockText>,
        val transferred: (DocumentId) -> Unit,
        val flush: suspend (DocumentId) -> Unit,
        val resume: (DocumentId) -> Unit,
    )
    private val removalEditors = mutableMapOf<Any, RemovalEditor>()

    internal fun registerRemovalEditor(
        owner: Any,
        quiesce: suspend (DocumentId) -> List<DocumentCommand.EditBlockText>,
        transferred: (DocumentId) -> Unit,
        flush: suspend (DocumentId) -> Unit,
        resume: (DocumentId) -> Unit,
    ) { removalEditors[owner] = RemovalEditor(quiesce, transferred, flush, resume) }

    internal fun unregisterRemovalEditor(owner: Any) { removalEditors.remove(owner) }

    /** All Activity/ViewModel editors share one native source. None may leave
     *  buffered text behind another window's quiescence acknowledgement. */
    internal suspend fun prepareDocumentRemoval(prompt: DocumentRemovalPrompt) = removalScope.async {
        removalPreparation.withLock {
            // Application-owned until native acknowledgement: Activity teardown
            // may stop awaiting this handoff, but cannot cancel its journal write.
            val editors = removalEditors.values.toList()
            val pending = mutableListOf<DocumentCommand.EditBlockText>()
            for (editor in editors) pending += editor.quiesce(prompt.documentId)
            val retained = removalDrafts.retain(prompt.documentId, pending)
            documents.quiesceRemoval(prompt, retained)
            editors.forEach { it.transferred(prompt.documentId) }
            removalDrafts.transferred(prompt.documentId)
        }
    }.await()

    internal suspend fun deleteDocumentPermanently(id: DocumentId) = removalPreparation.withLock {
        val editors = removalEditors.values.toList()
        try {
            editors.forEach { it.flush(id) }
            for (editor in editors) {
                // A second Activity could type while the first write awaited IO.
                // Preserve that new draft and retry instead of deleting over it.
                check(editor.quiesce(id).isEmpty()) { "document_edits_pending" }
            }
            documents.deletePermanently(id)
        } catch (error: Throwable) {
            editors.forEach { it.resume(id) }
            throw error
        }
    }

    fun acceptLinkIntent(uri: Uri?) {
        if (uri?.scheme == "https" && uri.host == "koru.do" && uri.path == "/link-device") {
            pendingLink.value = uri.toString()
        }
    }
}

class KoruApplication : Application() {
    val container by lazy { AppContainer(this) }
    override fun onCreate() {
        super.onCreate()
        container.logger.event("application_start", mapOf("platform" to "android", "contract" to "koru.v1"))
        // **Resume, not poll.** A transfer Android interrupted by killing this
        // process finishes on its own once there is a network again; the
        // staging file on disk is the resume authority, so nothing had to be
        // remembered here. D244's account poll remains the only tick.
        BlobRecoveryWorker.enqueue(this)
            .onFailure {
                container.logger.event(
                    "blob_recovery_unscheduled",
                    mapOf("message" to (it.message ?: "?")),
                )
            }
    }
}
