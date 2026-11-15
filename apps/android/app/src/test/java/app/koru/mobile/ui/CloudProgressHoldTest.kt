// SPDX-License-Identifier: MPL-2.0

package app.koru.mobile.ui

import app.koru.mobile.R
import app.koru.mobile.services.CloudHold
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/**
 * **A hold is said in the words the file transfer panel already uses** for
 * the same code, so the Sync status screen and the Documents screen never
 * describe one hold two ways; a code with no sentence is left for *Waiting:*.
 */
class CloudProgressHoldTest {
    @Test
    fun upload_holds_keep_the_same_message_in_progress() {
        assertEquals(R.string.cloud_upload_file_allowance, holdSentence(CloudHold.Code("file_exceeds_plan_allowance")))
        assertEquals(R.string.cloud_upload_account_quota, holdSentence(CloudHold.Code("account_over_quota")))
        assertEquals(R.string.cloud_upload_usage_unknown, holdSentence(CloudHold.Code("usage_unknown")))
    }

    @Test fun everyKnownHoldIsTheTransferPanelsOwnSentence() {
        assertEquals(R.string.blob_state_paused_person, holdSentence(CloudHold.PausedByYou))
        assertEquals(R.string.blob_state_paused_person, holdSentence(CloudHold.Code("paused_by_person")))
        assertEquals(R.string.blob_state_paused_metered, holdSentence(CloudHold.Code("network_metered")))
        assertEquals(R.string.blob_state_paused_low_data, holdSentence(CloudHold.Code("network_low_data")))
        assertEquals(R.string.blob_state_paused_offline, holdSentence(CloudHold.Code("network_offline")))
        assertEquals(R.string.blob_state_waiting_holder, holdSentence(CloudHold.Code("object_missing")))
        assertEquals(R.string.blob_state_refused, holdSentence(CloudHold.Code("mode_refuses_transport")))
        assertEquals(R.string.blob_state_no_cloud, holdSentence(CloudHold.Code("cloud_access_unavailable")))
        assertEquals(R.string.blob_state_no_room, holdSentence(CloudHold.Code("insufficient_storage")))
    }

    @Test fun anUnknownCodeHasNoSentenceOfItsOwn() {
        assertNull(holdSentence(CloudHold.Code("cloud_unit_unavailable")))
    }
}
