// SPDX-License-Identifier: MPL-2.0
package app.koru.mobile.services

import app.koru.contract.api.v1.*
import app.koru.mobile.domain.BlockId
import app.koru.mobile.ui.editor.CloudCopyPhase
import kotlinx.coroutines.*
import kotlinx.coroutines.test.*
import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Test

@OptIn(ExperimentalCoroutinesApi::class)
class CloudCopyPresentationTest {
    private fun id(n: Int) = "00000000-0000-0000-0000-${n.toString().padStart(12, '0')}"
    private val selection = CloudCopySelection(id(1), id(2), id(3), id(4), id(5), "a".repeat(64))
    private val node = BlockId(id(6))
    private fun status(owner: Long = 1, account: String = id(1)) = AndroidAccountStatus(
        activeScope = owner, activeAccountId = account, worker = AccountDiagnostics(stage = "signed_in"))
    private fun payload(): String = JSONObject().put("selection", JSONObject(selection.json()))
        .put("document", JSONObject().put("id", selection.documentId).put("workspace_id", selection.workspaceId)
            .put("title", "Cloud body").put("revision", 1).put("blocks", JSONArray().put(JSONObject()
                .put("id", node.value).put("parent_id", JSONObject.NULL).put("kind", "file").put("in_trash", false)
                .put("properties", JSONObject().apply {
                    listOf("name", "icon", "color", "heading_level", "list_style", "image_width", "image_height", "language").forEach { put(it, JSONObject.NULL) }
                    put("alignment", "start"); put("done", false)
                }).put("blob", JSONObject().put("id", "hash").put("mime_type", "image/png")
                    .put("size", JSONObject.NULL).put("width", JSONObject.NULL).put("height", JSONObject.NULL)))))
        .put("can_request_removal", true).put("removable_files", JSONArray().put(JSONObject()
            .put("node_id", node.value).put("in_trash", false))).toString()
    private class Calls {
        var next = 10L
        val nodes = mutableListOf<BlockId?>()
        val cancellations = mutableListOf<Pair<Long, Long>>()
        var refreshes = 0
        var admission: CompletableDeferred<Unit>? = null
    }
    private fun TestScope.presenter(calls: Calls) = CloudCopyPresentation(backgroundScope,
        command = { _, _, node -> calls.nodes += node; calls.admission?.await(); AccountCommandReply.newBuilder().setGeneration(calls.next++).build() },
        cancel = { owner, generation -> calls.cancellations += owner to generation }, refreshUsage = { calls.refreshes++ })
    private suspend fun CloudCopyPresentation.enter() { foreground(true, status()); open(selection, status()) }
    private fun result(sequence: Long, generation: Long, removal: Boolean = false,
                       selected: CloudCopySelection = selection, owner: Long = 1, failure: String? = null,
                       removed: Boolean = true) = MobileAccountResult.newBuilder()
        .setRequester(AndroidAccountAdapter.REQUESTER).setOwnerScope(owner).setGeneration(generation).setSequence(sequence)
        .setKind(if (removal) "cloud_file_removal" else "cloud_document_inspection")
        .setCloudDocumentSelectionJson(selected.json()).apply {
            if (removal) { setCloudFileNodeId(node.value); setCloudFileRemoved(removed && failure == null) }
            else if (failure == null) setCloudDocumentInspectionJson(payload())
            failure?.let(::setErrorCode)
        }.build()

    @Test fun onlyExactInspectionEnablesOneConfirmedFileCommandAndOnlyAckRefreshesUsage() = runTest {
        val calls = Calls(); val state = presenter(calls); state.enter()
        val key = state.state.value.key!!
        state.observe(status().copy(generation = 10), listOf(result(1, 10)))
        assertEquals(CloudCopyPhase.Verified, state.state.value.phase)
        state.remove(key, BlockId(id(99)))
        assertEquals(1, calls.nodes.size)
        state.remove(key, node); state.remove(key, node)
        assertEquals(listOf(null, node), calls.nodes)
        assertEquals(0, calls.refreshes)
        state.observe(status(), listOf(result(2, 11, removal = true)))
        assertEquals(CloudCopyPhase.Acknowledged, state.state.value.phase)
        assertNull(state.state.value.copy)
        assertEquals(1, calls.refreshes)
        state.observe(status(), listOf(result(2, 11, removal = true)))
        assertEquals(1, calls.refreshes)
    }
    @Test fun accountSwitchCancelsOriginalOwnerAndLateContentCannotReappear() = runTest {
        val calls = Calls(); val state = presenter(calls); state.enter()
        state.observe(status(2, id(20)), listOf(result(1, 10)))
        assertEquals(listOf(1L to 10L), calls.cancellations)
        assertNull(state.state.value.key)
        assertNull(state.state.value.forAccount(1, id(1)).copy)
        state.observe(status(), listOf(result(2, 10)))
        assertNull(state.state.value.key)
    }
    @Test fun lateAdmissionAfterScopeChangeIsCancelledAndCannotRetarget() = runTest {
        val calls = Calls(); calls.admission = CompletableDeferred()
        val state = presenter(calls)
        state.foreground(true, status())
        val opening = launch { state.open(selection, status()) }; runCurrent()
        val switching = launch { state.observe(status(2, id(20)), emptyList()) }; runCurrent()
        calls.admission!!.complete(Unit); opening.join(); switching.join()
        assertEquals(listOf(1L to 10L), calls.cancellations)
        assertNull(state.state.value.key)
    }
    @Test fun sameAccountLogoutAndLoginDiscardVerifiedContent() = runTest {
        val calls = Calls(); val state = presenter(calls); state.enter()
        state.observe(status(), listOf(result(1, 10)))
        val old = state.state.value.key!!
        state.observe(status().copy(worker = AccountDiagnostics(stage = "signed_out")), emptyList())
        state.observe(status(), emptyList())
        assertNull(state.state.value.key)
        state.remove(old, node)
        assertEquals(1, calls.nodes.size)
        state.open(selection, status())
        assertNotEquals(old, state.state.value.key)
    }
    @Test fun timeoutBackgroundAndResultLossNeverAutomaticallyRetryRemoval() = runTest {
        val calls = Calls(); val state = presenter(calls); state.enter()
        state.observe(status(), listOf(result(1, 10)))
        state.remove(state.state.value.key!!, node)
        advanceTimeBy(30_001); runCurrent()
        assertEquals(CloudCopyPhase.OutcomeUnknown, state.state.value.phase)
        assertEquals(listOf(1L to 11L), calls.cancellations)
        state.remove(state.state.value.key!!, node)
        state.foreground(false, status()); state.foreground(true, status())
        advanceTimeBy(60_000); runCurrent()
        assertEquals(2, calls.nodes.size)
        assertEquals(0, calls.refreshes)
        state.open(selection, status()); state.observe(status(), listOf(result(2, 12)))
        state.remove(state.state.value.key!!, node)
        state.observe(status().copy(resultsLost = true), emptyList())
        assertEquals(CloudCopyPhase.OutcomeUnknown, state.state.value.phase)
        assertNull(state.state.value.copy)
    }
    @Test fun wrongSelectorAndMixedSuccessNeverCountAsAcknowledgement() = runTest {
        val calls = Calls(); val state = presenter(calls); state.enter()
        state.observe(status(), listOf(result(1, 10, selected = selection.copy(cloudUnitId = id(99)))))
        assertEquals(CloudCopyPhase.Unavailable, state.state.value.phase)
        state.open(selection, status()); state.observe(status(), listOf(result(2, 11)))
        state.remove(state.state.value.key!!, node)
        val mixed = result(3, 12, removal = true).toBuilder().setErrorCode("cloud_document_unavailable").build()
        state.observe(status(), listOf(mixed))
        assertEquals(CloudCopyPhase.OutcomeUnknown, state.state.value.phase)
        assertEquals(0, calls.refreshes)
    }
    @Test fun actualMobileCommandsRequireBoundedFullSelectorAndRemovalNode() {
        val request = AccountCommandRequest.newBuilder().setVersion(1).setRequester(2).setExpectedOwnerScope(1)
            .setAction(AccountAction.ACCOUNT_ACTION_INSPECT_CLOUD_DOCUMENT).setCloudDocumentSelectionJson(selection.json())
        MobileAccountContract.validate(request.build())
        MobileAccountContract.validate(request.setAction(AccountAction.ACCOUNT_ACTION_REMOVE_CLOUD_FILE).setCloudFileNodeId(node.value).build())
        assertThrows(IllegalArgumentException::class.java) { MobileAccountContract.validate(request.clearCloudFileNodeId().build()) }
        assertThrows(IllegalArgumentException::class.java) { MobileAccountContract.validate(request.setCloudDocumentSelectionJson("x".repeat(1025)).build()) }
    }
}
