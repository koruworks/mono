// SPDX-License-Identifier: MPL-2.0

package app.koru.mobile.services

import android.app.Application
import android.content.ActivityNotFoundException
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.provider.Settings
import androidx.core.content.FileProvider
import app.koru.mobile.BuildConfig
import app.koru.mobile.logging.AppLogger
import java.io.File
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch

/** What the update banner shows. */
sealed interface UpdateNotice {
    data object None : UpdateNotice
    data class Available(val offer: UpdateOffer) : UpdateNotice
    data class Downloading(val offer: UpdateOffer) : UpdateNotice
    /** Verified and waiting for the person to start the system installer. */
    data class Ready(val offer: UpdateOffer, val apk: File) : UpdateNotice
    data class Failed(val offer: UpdateOffer) : UpdateNotice
}

/**
 * **The nightly app's update notice** (TECHNICAL.final §5.5, D21).
 *
 * Only `do.koru.app.nightly` built with a feed and a key checks at all: the
 * stable app, a Play install and a contributor's build have neither, and an
 * app Play installed leaves updating to Play even if it has them. A check
 * runs when the app comes to the foreground and at most every
 * [UPDATE_CHECK_INTERVAL_MS]; it asks the release's public feed and nothing
 * else, sends nothing about the person or the device beyond an HTTP request's
 * user agent, and a failure — offline, a 404, a manifest `koru-update`
 * refuses — shows nothing and is logged on this device only.
 *
 * Installing is always the person's: the banner downloads the APK and
 * `koru-update` verifies its size, SHA-256 and Minisign signature against the
 * key this build carries, then the system package installer — which also
 * refuses an APK not signed with this app's key — asks before it installs.
 */
class AppUpdates(
    private val application: Application,
    private val logger: AppLogger,
    private val scope: CoroutineScope = CoroutineScope(SupervisorJob() + Dispatchers.IO),
) {
    private val store = application.getSharedPreferences(StoreName, Context.MODE_PRIVATE)
    private val native = UpdateNative()
    private val directory = File(application.cacheDir, "updates")
    private val installed = BuildConfig.VERSION_CODE.toLong()
    private val mutableNotice = MutableStateFlow<UpdateNotice>(UpdateNotice.None)
    val notice: StateFlow<UpdateNotice> = mutableNotice.asStateFlow()

    /** Whether this build offers updates at all, before the install source. */
    private val configured: Boolean =
        BuildConfig.APPLICATION_ID == NightlyApplicationId &&
            BuildConfig.UPDATE_BASE_URL.isNotEmpty() &&
            BuildConfig.UPDATE_PUBLIC_KEY.isNotEmpty()

    init {
        if (configured) scope.launch { restore() }
    }

    /** Call when the app comes to the foreground. */
    fun checkIfDue() {
        if (!configured) return
        scope.launch {
            if (!ourSource()) return@launch
            val now = System.currentTimeMillis()
            if (!updateCheckIsDue(store.getLong(LastCheckKey, 0), now)) return@launch
            store.edit().putLong(LastCheckKey, now).apply()
            if (!UpdateNative.loaded) return@launch
            runCatching { native.nativeCheck(BuildConfig.UPDATE_BASE_URL, Channel, userAgent()) }
                .onSuccess { json -> if (json.isNotEmpty()) accept(json) }
                .onFailure { logger.event("update_check_failed", mapOf("code" to (it.message ?: "?"))) }
        }
    }

    /** "Not now": this build stays hidden; a newer one shows again. */
    fun dismiss() {
        val offer = current() ?: return
        store.edit().putLong(DismissedKey, offer.buildNumber).apply()
        mutableNotice.value = UpdateNotice.None
    }

    /** Download and verify the offered APK; the banner then offers to install it. */
    fun download() {
        val offer = current() ?: return
        if (mutableNotice.value is UpdateNotice.Downloading) return
        mutableNotice.value = UpdateNotice.Downloading(offer)
        scope.launch {
            val apk = File(directory, offer.fileName)
            val result = runCatching {
                if (!apk.isFile) {
                    directory.listFiles()?.forEach { it.delete() }
                    native.nativeDownload(Channel, offer.manifestJson, BuildConfig.UPDATE_PUBLIC_KEY, apk.path, userAgent())
                }
            }
            mutableNotice.value = result.fold(
                onSuccess = { UpdateNotice.Ready(offer, apk) },
                onFailure = {
                    logger.event("update_download_failed", mapOf("code" to (it.message ?: "?")))
                    UpdateNotice.Failed(offer)
                },
            )
        }
    }

    /**
     * Hand the verified APK to the system installer, which asks the person.
     * Android first wants this app allowed to install unknown apps; without
     * that, the person is taken to that one switch, and taps Install again.
     */
    fun install(context: Context) {
        val ready = mutableNotice.value as? UpdateNotice.Ready ?: return
        val intent = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O &&
            !application.packageManager.canRequestPackageInstalls()
        ) {
            Intent(Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES, Uri.parse("package:${application.packageName}"))
        } else {
            val uri = FileProvider.getUriForFile(application, "${application.packageName}.fileprovider", ready.apk)
            Intent(Intent.ACTION_VIEW)
                .setDataAndType(uri, ApkMimeType)
                .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
        }
        try {
            context.startActivity(intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
        } catch (error: ActivityNotFoundException) {
            logger.event("update_install_unavailable", mapOf("code" to (error.message ?: "?")))
            mutableNotice.value = UpdateNotice.Failed(ready.offer)
        }
    }

    private fun restore() {
        if (!ourSource()) return
        val offer = store.getString(OfferKey, null)?.let(::parseUpdateOffer)
        if (offer == null || !isNewerThanInstalled(offer, installed)) {
            // Installed, or never offered: the APK it downloaded is spent.
            store.edit().remove(OfferKey).apply()
            directory.listFiles()?.forEach { it.delete() }
            return
        }
        publish(offer)
    }

    private fun accept(json: String) {
        val offer = parseUpdateOffer(json) ?: return
        if (!ourSource()) return
        store.edit().putString(OfferKey, json).apply()
        if (mutableNotice.value !is UpdateNotice.Downloading && mutableNotice.value !is UpdateNotice.Ready) {
            publish(offer)
        }
    }

    private fun publish(offer: UpdateOffer) {
        mutableNotice.value =
            if (shouldShowUpdate(offer, installed, store.getLong(DismissedKey, 0))) {
                UpdateNotice.Available(offer)
            } else {
                UpdateNotice.None
            }
    }

    private fun current(): UpdateOffer? = when (val value = mutableNotice.value) {
        is UpdateNotice.Available -> value.offer
        is UpdateNotice.Downloading -> value.offer
        is UpdateNotice.Ready -> value.offer
        is UpdateNotice.Failed -> value.offer
        UpdateNotice.None -> null
    }

    /**
     * D21: read on every check, so an install Play took over stops polling
     * and drops what it stored. An exception reading the source is not Play.
     */
    private fun ourSource(): Boolean {
        val installer = runCatching {
            val manager = application.packageManager
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
                manager.getInstallSourceInfo(application.packageName).installingPackageName
            } else {
                @Suppress("DEPRECATION")
                manager.getInstallerPackageName(application.packageName)
            }
        }.getOrNull()
        if (!updatesBelongToPlay(installer)) return true
        store.edit().remove(OfferKey).apply()
        mutableNotice.value = UpdateNotice.None
        return false
    }

    private fun userAgent() = "Koru-Android/${BuildConfig.VERSION_NAME}"

    private companion object {
        const val StoreName = "updates"
        const val LastCheckKey = "last_check_ms"
        const val OfferKey = "offer_manifest"
        const val DismissedKey = "dismissed_build"
        const val Channel = "nightly"
        const val NightlyApplicationId = "do.koru.app.nightly"
        const val ApkMimeType = "application/vnd.android.package-archive"
    }
}
