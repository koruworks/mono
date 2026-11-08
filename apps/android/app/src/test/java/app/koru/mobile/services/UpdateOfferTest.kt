// SPDX-License-Identifier: MPL-2.0

package app.koru.mobile.services

import org.junit.Assert.*
import org.junit.Test

class UpdateOfferTest {
    private fun manifest(
        version: String = "0.1.5-nightly.20261030.abc1234",
        build: Long = 202610302235,
        fileName: String = "koru-0.1.5-nightly.20261030.abc1234-android-aarch64.apk",
        notes: String? = "\"https://github.com/koruworks/mono/releases/tag/nightly\"",
    ) = """
        {"schema_version":1,"channel":"nightly","platform":"android","architecture":"aarch64",
         "version":"$version","build_number":$build,"published_at":"2026-10-30T22:35:00Z",
         "release_notes":{"title":"Nightly","summary":["s"],"url":${notes ?: "null"}},
         "package":{"url":"https://github.com/koruworks/mono/releases/download/nightly/$fileName",
                    "file_name":"$fileName","size":1,"sha256":"00","signature":"s","format":"apk"}}
    """.trimIndent()

    @Test fun the_version_code_is_the_rule_the_apk_was_stamped_with() {
        // The same instants as koru-release's mobile tests.
        assertEquals(1L, androidVersionCode(202601010001))
        assertEquals(24L * 60, androidVersionCode(202601020000))
        assertEquals(302L * 24 * 60 + 22 * 60 + 35, androidVersionCode(202610302235))
    }

    @Test fun a_number_that_is_not_a_build_time_after_2026_has_no_code() {
        for (build in listOf(0L, 42L, 202601010000, 202512312359, 202613010000, 202610322400, 20261030223)) {
            assertNull("$build", androidVersionCode(build))
        }
    }

    @Test fun a_validated_manifest_becomes_an_offer() {
        val offer = parseUpdateOffer(manifest())!!
        assertEquals("0.1.5-nightly.20261030.abc1234", offer.version)
        assertEquals(202610302235, offer.buildNumber)
        assertEquals("koru-0.1.5-nightly.20261030.abc1234-android-aarch64.apk", offer.fileName)
        assertEquals("https://github.com/koruworks/mono/releases/tag/nightly", offer.notesUrl)
        assertEquals(manifest(), offer.manifestJson)
    }

    @Test fun a_manifest_without_what_the_banner_needs_is_no_offer() {
        assertNull(parseUpdateOffer("not json"))
        assertNull(parseUpdateOffer("{}"))
        assertNull(parseUpdateOffer(manifest(version = "")))
        assertNull(parseUpdateOffer(manifest(fileName = "../koru.apk")))
        assertNull(parseUpdateOffer(manifest(fileName = "koru.zip")))
    }

    @Test fun notes_open_only_on_https_at_koru_s_hosts() {
        assertNull(parseUpdateOffer(manifest(notes = null))!!.notesUrl)
        assertNull(parseUpdateOffer(manifest(notes = "\"http://github.com/x\""))!!.notesUrl)
        assertNull(safeNotesUrl("https://example.com/koru"))
        assertNull(safeNotesUrl("javascript:alert(1)"))
        assertEquals("https://updates.koru.do/notes", safeNotesUrl("https://updates.koru.do/notes"))
    }

    @Test fun only_a_newer_build_not_dismissed_is_shown() {
        val offer = parseUpdateOffer(manifest())!!
        val code = androidVersionCode(offer.buildNumber)!!
        assertTrue(shouldShowUpdate(offer, installedVersionCode = code - 1, dismissedBuild = 0))
        assertFalse("the installed build", shouldShowUpdate(offer, code, 0))
        assertFalse("an older offer", shouldShowUpdate(offer, code + 1, 0))
        assertFalse("dismissed", shouldShowUpdate(offer, code - 1, offer.buildNumber))
        assertTrue("a later one than dismissed", shouldShowUpdate(offer, code - 1, 202610292235))
    }

    @Test fun a_check_is_due_first_after_the_interval_and_after_the_clock_went_back() {
        val hour = 60 * 60 * 1000L
        assertTrue(updateCheckIsDue(0, 10 * hour))
        assertFalse(updateCheckIsDue(10 * hour, 11 * hour))
        assertTrue(updateCheckIsDue(10 * hour, 13 * hour))
        assertTrue(updateCheckIsDue(10 * hour, 9 * hour))
    }

    @Test fun play_owns_what_it_installed_and_nothing_else() {
        assertTrue(updatesBelongToPlay("com.android.vending"))
        assertFalse(updatesBelongToPlay(null))
        assertFalse(updatesBelongToPlay("com.google.android.packageinstaller"))
    }
}
