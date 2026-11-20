// SPDX-License-Identifier: MPL-2.0

package app.koru.mobile.services

import app.koru.mobile.R
import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * A file a person asked to open that did not arrive: the sentence is chosen
 * from the host's stable code, and an engine hold reaches the screen as the
 * same "held" sentence the file sheet already shows, never as "unexpected".
 */
class BlobHeldMessageTest {
    @Test
    fun upload_admission_holds_use_specific_localized_messages() {
        assertEquals(R.string.cloud_upload_file_allowance, blobHeldMessage("file_exceeds_plan_allowance"))
        assertEquals(R.string.cloud_upload_account_quota, blobHeldMessage("account_over_quota"))
        assertEquals(R.string.cloud_upload_usage_unknown, blobHeldMessage("usage_unknown"))
        assertEquals(R.string.cloud_upload_policy_unavailable, blobHeldMessage("cloud_policy_unavailable"))
        assertEquals(R.string.cloud_upload_plan_required, blobHeldMessage("cloud_plan_required"))
        assertEquals(R.string.cloud_upload_document_count, blobHeldMessage("cloud_document_count_limit"))
        assertEquals(R.string.cloud_upload_document_bytes, blobHeldMessage("cloud_stored_document_bytes_limit"))
        assertEquals(R.string.cloud_upload_physical_bytes, blobHeldMessage("cloud_physical_bytes_limit"))
        assertEquals(R.string.cloud_upload_staging_bytes, blobHeldMessage("cloud_staging_bytes_limit"))
        assertEquals(R.string.cloud_upload_removal_capacity, blobHeldMessage("cloud_removal_capacity_limit"))
    }

    @Test
    fun nobody_reachable_has_it() {
        assertEquals(R.string.image_waiting_no_holder, blobHeldMessage("blob_waiting_for_peer"))
        assertEquals(R.string.image_waiting_no_holder, blobHeldMessage("blob_not_found"))
    }

    @Test
    fun nothing_is_linked() {
        assertEquals(R.string.image_link_device, blobHeldMessage("no_linked_devices"))
    }

    @Test
    fun an_engine_hold_is_the_held_sentence() {
        assertEquals(R.string.file_transfers_held, blobHeldMessage("cloud_access_unavailable"))
        assertEquals(R.string.file_transfers_held, blobHeldMessage("metered_connection"))
    }
}
