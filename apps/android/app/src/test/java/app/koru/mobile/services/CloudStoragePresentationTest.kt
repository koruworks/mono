// SPDX-License-Identifier: MPL-2.0

package app.koru.mobile.services

import app.koru.contract.api.v1.AccountCommandReply
import app.koru.contract.api.v1.MobileAccountResult
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.*
import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Test
import java.util.Locale
import javax.xml.parsers.DocumentBuilderFactory

@OptIn(ExperimentalCoroutinesApi::class)
class CloudStoragePresentationTest {
    private fun status(account: String? = "account-a", owner: Long = 1) = AndroidAccountStatus(
        worker = AccountDiagnostics(stage = if (account == null) "signed_out" else "signed_in"),
        activeAccountId = account, activeScope = owner, requesterActive = true)

    private fun payload(used: String = "7", total: String = "100", until: String? = "11000") = """
        {"contract":"cloud-plan-v1","revision":"2","policy_revision":"3","evaluated_at_millis":"1000",
         "valid_until_millis":${until?.let { "\"$it\"" } ?: "null"},
         "policy":{"allowance":{"state":"finite","base_bytes":"$total","effective_bytes":"$total"}},
         "usage":{"state":"known","reason":null,"algorithm":"files-plain-v0","generation":"fixture",
          "used_bytes":"$used","files_bytes":"$used","total_bytes":"$total",
          "available_bytes":"${(total.toLong() - used.toLong()).coerceAtLeast(0)}","reserved_bytes":"0",
          "upload_headroom_bytes":"${(total.toLong() - used.toLong()).coerceAtLeast(0)}"}}
    """.trimIndent()

    private fun result(sequence: Long = 1, generation: Long = 10, owner: Long = 1,
                       json: String? = payload(), elapsed: Long = 0) = MobileAccountResult.newBuilder()
        .setSequence(sequence).setRequester(AndroidAccountAdapter.REQUESTER).setGeneration(generation)
        .setOwnerScope(owner).setKind("cloud_plan").setCloudPlanRequestElapsedMillis(elapsed)
        .apply { json?.let(::setCloudPlanJson) }.build()

    private class Calls {
        var next = 10L
        val reads = mutableListOf<Long>()
        val cancellations = mutableListOf<Pair<Long, Long>>()
        suspend fun read(owner: Long): AccountCommandReply {
            reads += owner
            return AccountCommandReply.newBuilder().setGeneration(next++).build()
        }
    }
    private fun TestScope.presenter(calls: Calls) = CloudStoragePresentation(backgroundScope,
        { testScheduler.currentTime }, calls::read, { owner, generation -> calls.cancellations += owner to generation })

    private suspend fun CloudStoragePresentation.enter() {
        visibility(true, status())
        foreground(true, status())
    }

    @Test fun exactIntegersAndOverageNeverUseFloatingPoint() {
        val value = CloudStorageSnapshot.decode(payload("9007199254740993", "9007199254740992"))!!
        assertEquals(9007199254740993L, value.used)
        assertEquals(0L, value.available)
        assertEquals("9,007,199,254,740,993", cloudStorageInteger(value.used!!, Locale.US))
        assertEquals("9.007.199.254.740.993", cloudStorageInteger(value.used, Locale.GERMANY))
        assertTrue(cloudStorageInteger(value.used, Locale.forLanguageTag("ar")).contains('٩'))
        assertEquals(0L, CloudStorageSnapshot.decode(payload("0"))!!.used)
    }

    @Test fun malformedAndFutureShapesNeverBecomeZero() {
        assertNull(CloudStorageSnapshot.decode(payload().replace("\"used_bytes\":\"7\"", "\"used_bytes\":7")))
        assertNull(CloudStorageSnapshot.decode(payload().replace("\"7\"", "\"07\"")))
        assertNull(CloudStorageSnapshot.decode(payload().replace("\"7\"", "\"9223372036854775808\"")))
        assertNull(CloudStorageSnapshot.decode(payload().replace("files-plain-v0", "future")))
        assertNull(CloudStorageSnapshot.decode(" ".repeat(32_769)))
        assertNull(CloudStorageSnapshot.decode(payload(until = "1000")))
        assertNull(CloudStorageSnapshot.decode(payload().replace("\"available_bytes\":\"93\"", "\"available_bytes\":\"94\"")))
    }

    @Test fun partialKnownnessAndUnboundedAreIndependentFacts() {
        val root = JSONObject(payload())
        val usage = root.getJSONObject("usage")
        usage.put("state", "building").put("generation", JSONObject.NULL)
        listOf("used_bytes", "files_bytes", "available_bytes", "upload_headroom_bytes").forEach { usage.put(it, JSONObject.NULL) }
        assertEquals(CloudStorageUsage.Building, CloudStorageSnapshot.decode(root.toString())!!.usage)
        assertEquals(100L, CloudStorageSnapshot.decode(root.toString())!!.total)
        usage.put("state", "unknown").put("reason", "cloud_schema_update_required")
        assertEquals(CloudStorageUsage.NewerSchema, CloudStorageSnapshot.decode(root.toString())!!.usage)
        val unbounded = JSONObject(payload())
        unbounded.getJSONObject("policy").put("allowance", JSONObject("""{"state":"unbounded"}"""))
        listOf("total_bytes", "available_bytes", "upload_headroom_bytes").forEach { unbounded.getJSONObject("usage").put(it, JSONObject.NULL) }
        assertEquals(CloudStorageAllowance.Unbounded, CloudStorageSnapshot.decode(unbounded.toString())!!.allowance)
        unbounded.getJSONObject("policy").put("allowance", JSONObject("""{"state":"unavailable","reason":"policy_unavailable"}"""))
        val unknown = CloudStorageSnapshot.decode(unbounded.toString())!!
        assertEquals(7L, unknown.used)
        assertNull(unknown.total)
        assertEquals(CloudStorageAllowance.Unavailable, unknown.allowance)
    }

    @Test fun visibleTrialBoundaryInvalidatesWholeSnapshotAndFailedRefreshStaysStale() = runTest {
        val calls = Calls(); val presenter = presenter(calls)
        presenter.enter()
        advanceTimeBy(200)
        presenter.observe(status(), listOf(result(elapsed = 300)))
        assertEquals(9900L, presenter.state.value.deadlineMillis)
        advanceTimeBy(9699); runCurrent()
        assertFalse(presenter.state.value.stale)
        advanceTimeBy(1); runCurrent()
        assertTrue(presenter.state.value.stale)
        assertTrue(presenter.state.value.loading)
        assertEquals(2, calls.reads.size)
        presenter.observe(status(), listOf(result(2, 11, json = null)))
        assertTrue(presenter.state.value.failed)
        assertTrue(presenter.state.value.stale)
        assertEquals(7L, presenter.state.value.snapshot!!.used)
        advanceTimeBy(60_000); runCurrent()
        assertEquals("no idle retries", 2, calls.reads.size)
    }

    @Test fun queueAndDeliveryTimeConservativelyShortenValidity() = runTest {
        val calls = Calls(); val presenter = presenter(calls)
        presenter.enter()
        advanceTimeBy(10_001)
        presenter.observe(status(), listOf(result(elapsed = 1)))
        assertTrue(presenter.state.value.stale)
        assertEquals(1, calls.reads.size)
        assertFalse(presenter.state.value.loading)
    }

    @Test fun accountSwitchRejectsLateAndWrongOwnerResponses() = runTest {
        val calls = Calls(); val presenter = presenter(calls)
        presenter.enter()
        presenter.observe(status(), listOf(result()))
        assertNull(presenter.state.value.forAccount(2, "account-b", 0).snapshot)
        presenter.observe(status("account-b", 2), emptyList())
        assertNull(presenter.state.value.snapshot)
        presenter.observe(status("account-b", 2), listOf(result(2, 10), result(3, 11)))
        assertNull(presenter.state.value.snapshot)
        presenter.observe(status("account-b", 2), listOf(result(4, 11, 2)))
        assertEquals(7L, presenter.state.value.snapshot!!.used)
        presenter.observe(status(null, 0), emptyList())
        assertNull(presenter.state.value.snapshot)
        presenter.observe(status(null, 0), listOf(result(5, 11, 2)))
        assertNull(presenter.state.value.snapshot)
    }

    @Test fun foregroundChecksExpiryAndDisposalCancelsObsoleteWork() = runTest {
        val calls = Calls(); val presenter = presenter(calls)
        presenter.enter()
        presenter.observe(status(), listOf(result()))
        presenter.foreground(false, status())
        advanceTimeBy(20_000); runCurrent()
        assertTrue(presenter.state.value.forAccount(1, "account-a", testScheduler.currentTime).stale)
        assertEquals(1, calls.reads.size)
        presenter.foreground(true, status())
        assertTrue(presenter.state.value.stale)
        assertEquals(2, calls.reads.size)
        presenter.visibility(false, status())
        assertEquals(listOf(1L to 11L), calls.cancellations)
        presenter.observe(status(), listOf(result(2, 11)))
        assertTrue(presenter.state.value.stale)
    }

    @Test fun requestTimeoutIsCancelableAndNeverAutomaticPolling() = runTest {
        val calls = Calls(); val presenter = presenter(calls)
        presenter.enter()
        presenter.retry(status())
        assertEquals(1, calls.reads.size)
        advanceTimeBy(30_000); runCurrent()
        assertTrue(presenter.state.value.failed)
        assertNull(presenter.state.value.snapshot)
        assertEquals(listOf(1L to 10L), calls.cancellations)
        advanceTimeBy(30_000); runCurrent()
        assertEquals(1, calls.reads.size)
    }

    @Test fun aPartialResponseReplacesRatherThanMixesThePreviousCompleteResponse() = runTest {
        val calls = Calls(); val presenter = presenter(calls)
        presenter.enter()
        presenter.observe(status(), listOf(result()))
        presenter.retry(status())
        val raw = JSONObject(payload(total = "200"))
        raw.getJSONObject("usage").put("state", "building").put("generation", JSONObject.NULL)
        listOf("used_bytes", "files_bytes", "available_bytes", "upload_headroom_bytes").forEach { raw.getJSONObject("usage").put(it, JSONObject.NULL) }
        presenter.observe(status(), listOf(result(2, 11, json = raw.toString())))
        assertNull(presenter.state.value.snapshot!!.used)
        assertEquals(200L, presenter.state.value.snapshot!!.total)
        assertFalse(presenter.state.value.stale)
    }

    @Test fun resultLossAndWrongRequesterCannotSettleARead() = runTest {
        val calls = Calls(); val presenter = presenter(calls)
        presenter.enter()
        presenter.observe(status(), listOf(result().toBuilder().setRequester(77).build()))
        assertNull(presenter.state.value.snapshot)
        assertTrue(presenter.state.value.loading)
        presenter.observe(status().copy(resultsLost = true), emptyList())
        assertTrue(presenter.state.value.failed)
        assertFalse(presenter.state.value.loading)
        presenter.observe(status(), listOf(result(2)))
        assertNull(presenter.state.value.snapshot)
    }

    @Test fun storageStringsAndPlaceholdersCoverEverySupportedLocale() {
        val locales = listOf("values", "values-ar", "values-de", "values-es", "values-fr", "values-ja", "values-pt-rBR", "values-zh-rCN")
        val sets = locales.associateWith { locale ->
            val document = javaClass.classLoader!!.getResourceAsStream("$locale/cloud_storage.xml")!!.use {
                DocumentBuilderFactory.newInstance().newDocumentBuilder().parse(it)
            }
            val rows = document.getElementsByTagName("string")
            val entries = (0 until rows.length).map { i ->
                rows.item(i).attributes.getNamedItem("name").nodeValue to rows.item(i).textContent
            }
            assertEquals(entries.size, entries.toMap().size)
            entries.toMap()
        }
        val baseline = sets.getValue("values")
        assertEquals(17, baseline.size)
        val placeholders = Regex("%[0-9]+\\\$s")
        sets.forEach { (locale, rows) ->
            assertEquals(locale, baseline.keys, rows.keys)
            baseline.forEach { (key, text) ->
                assertTrue("$locale/$key", rows.getValue(key).isNotBlank())
                assertEquals("$locale/$key", placeholders.findAll(text).map { it.value }.toList(),
                    placeholders.findAll(rows.getValue(key)).map { it.value }.toList())
            }
        }
    }
}
