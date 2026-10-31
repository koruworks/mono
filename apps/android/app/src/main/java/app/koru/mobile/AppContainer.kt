// SPDX-License-Identifier: MPL-2.0

package app.koru.mobile

import android.app.Application
import app.koru.mobile.logging.AppLogger
import app.koru.mobile.services.*
import app.koru.mobile.sharing.AndroidShareIntentReader
import app.koru.mobile.sharing.RecentDestinations
import android.net.Uri
import kotlinx.coroutines.flow.MutableStateFlow

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
