// SPDX-License-Identifier: MPL-2.0

package app.koru.mobile.services

import app.koru.contract.api.v1.*
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.*
import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Test
import javax.xml.parsers.DocumentBuilderFactory

@OptIn(ExperimentalCoroutinesApi::class)
class CloudInventoryPresentationTest {
    private fun uuid(n: Int) = "00000000-0000-4000-8000-${n.toString().padStart(12, '0')}"
    private val digest = "a".repeat(64)
    private fun amount(state: String = "known", bytes: String = "7") = JSONObject().put("state", state)
        .put("reason", if (state == "unknown") "usage_unknown" else JSONObject.NULL)
        .put("files_bytes", if (state == "known") bytes else JSONObject.NULL)
    private fun workspace(n: Int = 1, state: String = "known", placement: String = "assigned") = JSONObject()
        .put("workspace_id", uuid(n)).put("incarnation_id", uuid(n + 1000)).put("source_revision", "1")
        .put("display_name", JSONObject.NULL).put("usage", amount(state).put("last_known", JSONObject.NULL))
        .put("placement", JSONObject().put("state", placement).put("profile_id", if (placement == "assigned") uuid(5000) else JSONObject.NULL))
    private fun workspaces(next: String? = null, revision: String = "1", rows: List<JSONObject> = listOf(workspace())) = JSONObject()
        .put("contract", "cloud-usage-workspaces-v1").put("revision", revision).put("generation", uuid(7000))
        .put("catalog_revision_digest", digest).put("items", JSONArray(rows)).put("next_cursor", next ?: JSONObject.NULL).toString()
    private fun document(n: Int, membership: String, bytes: String = "7") = JSONObject()
        .put("cloud_unit_id", uuid(n)).put("document_id", uuid(n + 2000)).put("unit_role", if (membership == "structural") "structure" else "content")
        .put("source_head_digest", digest).put("display_name", if (membership in setOf("listed", "trash")) "Document $n" else JSONObject.NULL)
        .put("membership", JSONObject().put("state", membership).put("reason", if (membership == "unknown") "manifest_unavailable" else JSONObject.NULL))
        .put("usage", amount(bytes = bytes))
    private fun documents(next: String? = null) = JSONObject().put("contract", "cloud-usage-documents-v1")
        .put("algorithm", "files-plain-v0").put("row_totals", "non_additive").put("revision", "1").put("generation", uuid(7000))
        .put("evaluated_at_millis", "1000").put("workspace_id", uuid(1)).put("incarnation_id", uuid(1001)).put("source_revision", "1")
        .put("inventory_digest", digest).put("manifest", JSONObject().put("state", "verified").put("reason", JSONObject.NULL)
            .put("source_head_digest", digest).put("proof_digest", digest))
        .put("items", JSONArray(listOf(document(1, "structural", "0"), document(2, "listed"), document(3, "trash"),
            document(4, "unfiled"), document(5, "unknown"))))
        .put("next_cursor", next ?: JSONObject.NULL)
    private fun status(owner: Long = 1, account: String? = "account") = AndroidAccountStatus(activeScope = owner,
        activeAccountId = account, worker = AccountDiagnostics(stage = if (account == null) "signed_out" else "signed_in"))
    private class Calls {
        data class Read(val owner: Long, val selection: CloudInventorySelection, val cursor: String?)
        var generation = 10L
        val reads = mutableListOf<Read>()
        val cancelled = mutableListOf<Long>()
        suspend fun read(owner: Long, selection: CloudInventorySelection, cursor: String?): AccountCommandReply {
            reads += Read(owner, selection, cursor)
            return AccountCommandReply.newBuilder().setGeneration(generation++).build()
        }
    }
    private fun TestScope.presenter(calls: Calls) = CloudInventoryPresentation(backgroundScope, calls::read,
        { _, generation -> calls.cancelled += generation })
    private suspend fun CloudInventoryPresentation.enter() { visibility(true, status()); foreground(true, status()) }
    private fun result(sequence: Long, generation: Long, payload: String? = workspaces(), failure: String? = null,
                       docs: Boolean = false, owner: Long = 1) = MobileAccountResult.newBuilder()
        .setRequester(AndroidAccountAdapter.REQUESTER).setOwnerScope(owner).setSequence(sequence).setGeneration(generation)
        .setKind(if (docs) "cloud_usage_documents" else "cloud_usage_workspaces")
        .apply {
            payload?.let(::setCloudInventoryJson); failure?.let(::setErrorCode)
            if (docs) { setCloudWorkspaceId(uuid(1)); setCloudIncarnationId(uuid(1001)) }
        }.build()

    @Test fun profileTotalsRequireCompletePlacementAndCurrentKnownMembers() {
        fun page(rows: List<JSONObject>, next: String? = null) = CloudInventoryDecoder.decode(workspaces(next, rows = rows)) as CloudInventoryPage.Workspaces
        val known = page(listOf(workspace(), workspace(2)))
        assertEquals(14L, known.profileSubtotal(uuid(5000), true))
        assertNull(known.profileSubtotal(uuid(5000), false))
        assertNull(page(listOf(workspace()), "next").profileSubtotal(uuid(5000), true))
        assertNull(page(listOf(workspace(), workspace(2, "unknown"))).profileSubtotal(uuid(5000), true))
        assertNull(page(listOf(workspace(), workspace(2, placement = "unknown"))).profileSubtotal(uuid(5000), true))
        assertNull(known.rows.first().name)
        assertEquals(uuid(1), known.rows.first().workspace)
    }

    @Test fun censusRetainsEveryMembershipAndNeverAddsDocumentAmounts() {
        val page = CloudInventoryDecoder.decode(documents().toString()) as CloudInventoryPage.Documents
        assertEquals(CloudDocumentMembership.entries.toSet(), page.rows.map { it.membership }.toSet())
        assertEquals(5, page.rows.size)
        assertNull(page.rows.single { it.membership == CloudDocumentMembership.Unfiled }.name)
        assertEquals(0L, page.rows.single { it.membership == CloudDocumentMembership.Structural }.amount.bytes)
        assertNull(CloudInventoryDecoder.decode(documents().put("row_totals", "additive").toString()))
        assertNull(CloudInventoryDecoder.decode(workspaces(rows = (1..101).map { workspace(it) })))
        assertNull(CloudInventoryDecoder.decode(workspaces(rows = listOf(workspace(), workspace()))))
    }

    @Test fun nextPageEpochChangeRestartsOnceAndNeverConcatenates() = runTest {
        val calls = Calls(); val presenter = presenter(calls); presenter.enter()
        presenter.observe(status(), listOf(result(1, 10, workspaces("next"))))
        presenter.next(1)
        presenter.observe(status(), listOf(result(2, 11, workspaces(revision = "2", rows = listOf(workspace(2))))))
        assertTrue(presenter.state.value.restarted)
        assertNull(presenter.state.value.page)
        assertNull(calls.reads.last().cursor)
        presenter.observe(status(), listOf(result(3, 12, null, "usage_revision_changed")))
        assertTrue(presenter.state.value.failed)
        assertEquals(3, calls.reads.size)
        advanceTimeBy(60_000); runCurrent()
        assertEquals(3, calls.reads.size)
    }

    @Test fun boundedBackHistoryKeepsFirstAndAllForwardPagesReachable() = runTest {
        val calls = Calls(); val presenter = presenter(calls); presenter.enter()
        for (i in 0..40) {
            presenter.observe(status(), listOf(result(i + 1L, i + 10L, workspaces("cursor$i", rows = listOf(workspace(i + 1))))))
            assertEquals(1, (presenter.state.value.page as CloudInventoryPage.Workspaces).rows.size)
            if (i < 40) presenter.next(1)
        }
        repeat(32) { i ->
            presenter.previous(1)
            presenter.observe(status(), listOf(result(42L + i, 51L + i, workspaces("again"))))
        }
        assertFalse(presenter.state.value.canPrevious)
        assertFalse(presenter.state.value.firstPage)
        val readCount = calls.reads.size
        presenter.previous(1)
        assertEquals(readCount, calls.reads.size)
        presenter.first(1)
        assertNull(calls.reads.last().cursor)
    }

    @Test fun manifestProofAndSourceArePartOfDocumentPageEpoch() = runTest {
        val calls = Calls(); val presenter = presenter(calls); presenter.enter()
        presenter.observe(status(), listOf(result(1, 10)))
        val row = (presenter.state.value.page as CloudInventoryPage.Workspaces).rows.single()
        presenter.documents(row, 1)
        presenter.observe(status(), listOf(result(2, 11, documents("next").toString(), docs = true)))
        presenter.next(1)
        val changed = documents().apply { getJSONObject("manifest").put("proof_digest", "b".repeat(64)) }
        presenter.observe(status(), listOf(result(3, 12, changed.toString(), docs = true)))
        assertTrue(presenter.state.value.restarted)
        assertNull(presenter.state.value.page)
        assertEquals(4, calls.reads.size)
    }

    @Test fun wrongSelectorMixedFailureAndAccountSwitchCannotRestoreOldRows() = runTest {
        val calls = Calls(); val presenter = presenter(calls); presenter.enter()
        presenter.observe(status(), listOf(result(1, 10, failure = "usage_revision_changed")))
        assertTrue(presenter.state.value.failed)
        assertEquals(1, calls.reads.size)
        presenter.first(1)
        presenter.observe(status(2, "other"), listOf(result(2, 11)))
        assertNull(presenter.state.value.page)
        presenter.observe(status(2, "other"), listOf(result(3, 12, owner = 2)))
        assertNotNull(presenter.state.value.page)
        assertNull(presenter.state.value.forAccount(1, "account").page)
        presenter.observe(status(0, null), listOf(result(4, 12, owner = 2)))
        assertNull(presenter.state.value.page)
    }

    @Test fun pageRequestIsBoundedCancelableAndDoesNotPoll() = runTest {
        val calls = Calls(); val presenter = presenter(calls); presenter.enter()
        advanceTimeBy(30_000); runCurrent()
        assertTrue(presenter.state.value.failed)
        assertEquals(listOf(10L), calls.cancelled)
        advanceTimeBy(60_000); runCurrent()
        assertEquals(1, calls.reads.size)
        presenter.first(1)
        presenter.visibility(false, status())
        presenter.observe(status(), listOf(result(1, 11)))
        assertNull(presenter.state.value.page)
    }

    @Test fun documentSelectorEchoMustMatchTheSelectedIncarnation() = runTest {
        val calls = Calls(); val presenter = presenter(calls); presenter.enter()
        presenter.observe(status(), listOf(result(1, 10)))
        val row = (presenter.state.value.page as CloudInventoryPage.Workspaces).rows.single()
        presenter.documents(row, 1)
        val wrong = result(2, 11, documents().toString(), docs = true).toBuilder()
            .setCloudIncarnationId(uuid(9999)).build()
        presenter.observe(status(), listOf(wrong))
        assertTrue(presenter.state.value.failed)
        assertNull(presenter.state.value.page)
        presenter.first(1)
        presenter.observe(status(), listOf(result(3, 12, documents().toString(), docs = true)))
        assertNotNull(presenter.state.value.page)
    }

    @Test fun inventoryCommandsHaveOnlyBoundedCanonicalSelectors() {
        fun request(action: AccountAction) = AccountCommandRequest.newBuilder().setVersion(1).setRequester(2)
            .setExpectedOwnerScope(1).setAction(action)
        val workspaces = request(AccountAction.ACCOUNT_ACTION_INSPECT_CLOUD_USAGE_WORKSPACES)
        MobileAccountContract.validate(workspaces.build())
        assertThrows(IllegalArgumentException::class.java) { MobileAccountContract.validate(workspaces.setCloudCursor("!").build()) }
        assertThrows(IllegalArgumentException::class.java) { MobileAccountContract.validate(workspaces.clearCloudCursor().setCloudWorkspaceId(uuid(1)).build()) }
        val docs = request(AccountAction.ACCOUNT_ACTION_INSPECT_CLOUD_USAGE_DOCUMENTS).setCloudWorkspaceId(uuid(1)).setCloudIncarnationId(uuid(1001))
        MobileAccountContract.validate(docs.build())
        assertThrows(IllegalArgumentException::class.java) { MobileAccountContract.validate(docs.setCloudCursor("x".repeat(2049)).build()) }
    }

    @Test fun inventoryResourcesHaveAllLocalesAndMatchingArguments() {
        val locales = listOf("values", "values-ar", "values-de", "values-es", "values-fr", "values-ja", "values-pt-rBR", "values-zh-rCN")
        val rows = locales.map { locale ->
            val doc = javaClass.classLoader!!.getResourceAsStream("$locale/cloud_inventory.xml")!!.use {
                DocumentBuilderFactory.newInstance().newDocumentBuilder().parse(it)
            }.getElementsByTagName("string")
            (0 until doc.length).associate { doc.item(it).attributes.getNamedItem("name").nodeValue to doc.item(it).textContent }
        }
        val pattern = Regex("%[0-9]+\\\$s")
        rows.forEach { locale ->
            assertEquals(rows.first().keys, locale.keys)
            rows.first().forEach { (key, value) ->
                assertTrue(locale.getValue(key).isNotBlank())
                assertEquals(pattern.findAll(value).map { it.value }.toList(), pattern.findAll(locale.getValue(key)).map { it.value }.toList())
            }
        }
    }
}
