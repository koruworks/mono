// SPDX-License-Identifier: MPL-2.0

package app.koru.mobile.services

import java.time.Instant
import java.time.LocalDateTime
import java.time.ZoneOffset
import org.json.JSONObject

/**
 * **The nightly's update notice, as pure rules** (TECHNICAL.final §5.5). The
 * manifest was validated by `koru-update` before it reached Kotlin; this
 * reads the few fields the banner needs and decides whether to show it. The
 * JVM tests pin every rule here; [AppUpdates] is the Android glue.
 */
data class UpdateOffer(
    val version: String,
    /** The release's `yyyymmddHHMM` build number, which the feed carries. */
    val buildNumber: Long,
    val fileName: String,
    val notesUrl: String?,
    /** The validated manifest as served, handed back to Rust to download;
     *  Rust validates it again rather than trusting the app's copy. */
    val manifestJson: String,
)

/** How often a foregrounded nightly asks the feed: at most every few hours. */
const val UPDATE_CHECK_INTERVAL_MS: Long = 3 * 60 * 60 * 1000L

/** The package Play installs as. An app it installed leaves updating to it (D21). */
const val PLAY_STORE_INSTALLER = "com.android.vending"

fun parseUpdateOffer(json: String): UpdateOffer? = runCatching {
    val root = JSONObject(json)
    val build = root.getLong("build_number")
    val pkg = root.getJSONObject("package")
    val notes = root.optJSONObject("release_notes")?.optString("url")?.takeIf { it.isNotBlank() }
    UpdateOffer(
        version = root.getString("version"),
        buildNumber = build,
        fileName = pkg.getString("file_name"),
        notesUrl = notes?.let(::safeNotesUrl),
        manifestJson = json,
    ).takeIf { it.version.isNotBlank() && build > 0 && safeFileName(it.fileName) }
}.getOrNull()

/**
 * The `versionCode` a build number becomes: the minutes since 2026-01-01
 * 00:00 UTC, the rule `koru-release android-version-code` stamps every APK
 * with. The feed carries the build number, the installed app knows its code,
 * and this is the one conversion between them. Null for anything that is not
 * a build time after that instant.
 */
fun androidVersionCode(buildNumber: Long): Long? = runCatching {
    val digits = buildNumber.toString()
    require(digits.length == 12)
    fun part(from: Int, to: Int) = digits.substring(from, to).toInt()
    val time = LocalDateTime.of(part(0, 4), part(4, 6), part(6, 8), part(8, 10), part(10, 12))
        .toInstant(ZoneOffset.UTC)
    val minutes = (time.epochSecond - VERSION_CODE_EPOCH.epochSecond) / 60
    minutes.takeIf { it in 1..2_100_000_000L }
}.getOrNull()

private val VERSION_CODE_EPOCH = Instant.parse("2026-01-01T00:00:00Z")

/** The offer is for a newer build than the one installed. */
fun isNewerThanInstalled(offer: UpdateOffer, installedVersionCode: Long): Boolean =
    androidVersionCode(offer.buildNumber)?.let { it > installedVersionCode } == true

/** Shown while it is newer and the person has not dismissed this build or a
 *  later one; a newer build than the dismissed one shows again. */
fun shouldShowUpdate(offer: UpdateOffer, installedVersionCode: Long, dismissedBuild: Long): Boolean =
    isNewerThanInstalled(offer, installedVersionCode) && offer.buildNumber > dismissedBuild

/** A check is due on the first start, after the interval, and after the clock
 *  moved backwards (the stored time would otherwise silence it). */
fun updateCheckIsDue(lastCheckMs: Long, nowMs: Long, intervalMs: Long = UPDATE_CHECK_INTERVAL_MS): Boolean =
    lastCheckMs <= 0 || nowMs < lastCheckMs || nowMs - lastCheckMs >= intervalMs

/** Play owns an app it installed; anything else, unknown included, is ours. */
fun updatesBelongToPlay(installer: String?): Boolean = installer == PLAY_STORE_INSTALLER

/** The release notes link, only on HTTPS at the two hosts that publish Koru. */
fun safeNotesUrl(url: String): String? = runCatching {
    val uri = java.net.URI(url)
    url.takeIf { uri.scheme == "https" && uri.host in setOf("github.com", "updates.koru.do") }
}.getOrNull()

/** A file name with no path in it, so the APK lands in the updates directory. */
private fun safeFileName(name: String): Boolean =
    name.isNotBlank() && name.endsWith(".apk") && name.none { it == '/' || it == '\\' } && name != ".." && !name.startsWith(".")
