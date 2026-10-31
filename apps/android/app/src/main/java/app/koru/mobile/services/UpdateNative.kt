// SPDX-License-Identifier: MPL-2.0

package app.koru.mobile.services

/**
 * The nightly update notice's JNI edge (`koru-mobile-native/src/update.rs`).
 *
 * Marshalling only: fetching the feed, validating the manifest, downloading
 * the APK and verifying its size, SHA-256 and Minisign signature are the
 * desktop updater's code in `koru-update`, so the phone has no second parser
 * or verifier (TECHNICAL.final §5.5). Outside [KoruApiNative]'s operation
 * catalog on purpose, as account configuration is: that catalog is every
 * platform's, and only the sideloaded Android nightly updates itself.
 *
 * Both calls block on the network and throw `RuntimeException(code)` on any
 * failure; callers run them off the main thread.
 */
internal class UpdateNative {
    /** The channel's latest validated manifest as JSON, or "" for none. */
    external fun nativeCheck(baseUrl: String, channel: String, userAgent: String): String

    /** The verified APK at [destination], or a throw and nothing there. */
    external fun nativeDownload(
        channel: String,
        manifestJson: String,
        publicKey: String,
        destination: String,
        userAgent: String,
    )

    companion object {
        val loaded: Boolean by lazy { runCatching { System.loadLibrary("koru_mobile") }.isSuccess }
    }
}
