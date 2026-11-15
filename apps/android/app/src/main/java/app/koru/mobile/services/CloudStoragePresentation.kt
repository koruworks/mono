// SPDX-License-Identifier: MPL-2.0

package app.koru.mobile.services

import app.koru.contract.api.v1.AccountCommandReply
import app.koru.contract.api.v1.MobileAccountResult
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock

data class CloudStorageState(
    val ownerScope: Long = 0,
    val accountId: String? = null,
    val snapshot: CloudStorageSnapshot? = null,
    val loading: Boolean = false,
    val stale: Boolean = false,
    val failed: Boolean = false,
    val deadlineMillis: Long? = null,
) {
    /** A paint-time fence also covers the frame before the service sees an
     * account switch or Android delivers the foreground callback. */
    fun forAccount(owner: Long, account: String?, now: Long): CloudStorageState =
        if (owner != ownerScope || account == null || account != accountId) CloudStorageState()
        else if (deadlineMillis?.let { now >= it } == true) copy(stale = true)
        else this
}

/** Visibility-driven reads and two one-shot deadlines (request and validity).
 * The existing account poll carries results; there is no storage polling loop. */
internal class CloudStoragePresentation(
    private val scope: CoroutineScope,
    private val nowMillis: () -> Long,
    private val command: suspend (Long) -> AccountCommandReply,
    private val cancel: suspend (Long, Long) -> Unit,
) {
    private val lock = Mutex()
    private val mutableState = MutableStateFlow(CloudStorageState())
    val state = mutableState.asStateFlow()
    private var status = AndroidAccountStatus()
    private var visible = false
    private var foreground = false
    private var lastSequence = 0L
    private var pending: Long? = null
    private var started = 0L
    private var deadline: Job? = null
    private var requestDeadline: Job? = null
    private var transitRetryUsed = false

    private suspend fun align(next: AndroidAccountStatus) {
        val previous = status
        status = next
        if (next.activeScope != previous.activeScope || next.activeAccountId != previous.activeAccountId ||
            next.worker.stage == "signed_out") {
            retire()
            transitRetryUsed = false
            mutableState.value = CloudStorageState(next.activeScope, next.activeAccountId)
        }
    }

    private suspend fun retire() {
        deadline?.cancel(); deadline = null
        requestDeadline?.cancel(); requestDeadline = null
        val generation = pending
        pending = null
        if (generation != null) runCatching { cancel(mutableState.value.ownerScope, generation) }
            .onFailure { if (it is CancellationException) throw it }
    }

    suspend fun observe(next: AndroidAccountStatus, results: List<MobileAccountResult>) = lock.withLock {
        val changed = next.activeScope != status.activeScope || next.activeAccountId != status.activeAccountId
        val becameReady = next.worker.stage == "signed_in" && status.worker.stage != "signed_in"
        align(next)
        for (result in results) {
            if (result.sequence <= lastSequence) continue
            lastSequence = maxOf(lastSequence, result.sequence)
            if (result.requester != AndroidAccountAdapter.REQUESTER || result.ownerScope != status.activeScope ||
                result.generation != pending || status.activeAccountId == null) continue
            if (result.kind != "cloud_plan" && result.kind != "failed") continue
            pending = null
            requestDeadline?.cancel(); requestDeadline = null
            val snapshot = if (result.kind == "cloud_plan" && result.hasCloudPlanJson())
                CloudStorageSnapshot.decode(result.cloudPlanJson) else null
            if (snapshot == null) { failed(); continue }
            deadline?.cancel(); deadline = null
            val now = nowMillis()
            // Includes queue time and result delivery, and treats a uint64
            // duration outside signed range as expired rather than extending it.
            val nativeElapsed = result.cloudPlanRequestElapsedMillis.takeIf { it >= 0 } ?: Long.MAX_VALUE
            val elapsed = maxOf(nativeElapsed, (now - started).coerceAtLeast(0))
            val remaining = snapshot.validUntilMillis?.let {
                (it - snapshot.evaluatedAtMillis - elapsed).coerceAtLeast(0)
            }
            val expires = remaining?.let { if (it > Long.MAX_VALUE - now) Long.MAX_VALUE else now + it }
            mutableState.value = CloudStorageState(status.activeScope, status.activeAccountId, snapshot,
                stale = remaining == 0L, deadlineMillis = expires)
            if (remaining == 0L) {
                if (!transitRetryUsed && visible && foreground) {
                    transitRetryUsed = true
                    request()
                }
            } else {
                transitRetryUsed = false
                scheduleExpiry()
            }
        }
        if (next.resultsLost && pending != null) {
            retire(); failed()
        }
        if ((changed || becameReady) && visible && foreground) request()
    }

    suspend fun visibility(value: Boolean, next: AndroidAccountStatus) = lock.withLock {
        align(next)
        val entering = value && !visible
        visible = value
        expireBeforePaint()
        if (!value) { retire(); mutableState.value = mutableState.value.copy(loading = false) }
        else if (entering && foreground) { transitRetryUsed = false; request() }
    }

    suspend fun foreground(value: Boolean, next: AndroidAccountStatus) = lock.withLock {
        align(next)
        val returning = value && !foreground
        foreground = value
        expireBeforePaint()
        if (!value) { retire(); mutableState.value = mutableState.value.copy(loading = false) }
        else if (visible && returning) request()
    }

    suspend fun retry(next: AndroidAccountStatus) = lock.withLock {
        align(next)
        if (pending == null) transitRetryUsed = false
        request()
    }

    /** A remote Cloud acknowledgement invalidates the whole cached snapshot.
     * One event-driven read also runs when the meter is behind the files page. */
    suspend fun usageChanged(next: AndroidAccountStatus) = lock.withLock {
        align(next); retire(); transitRetryUsed = false
        mutableState.value = CloudStorageState(next.activeScope, next.activeAccountId)
        request(afterAcknowledgement = true)
    }

    private fun expireBeforePaint() {
        mutableState.value = mutableState.value.forAccount(status.activeScope, status.activeAccountId, nowMillis())
    }

    private fun failed() {
        mutableState.value = mutableState.value.copy(loading = false, failed = true,
            stale = mutableState.value.snapshot != null)
    }

    private suspend fun request(afterAcknowledgement: Boolean = false) {
        if ((!visible && !afterAcknowledgement) || !foreground || pending != null || status.activeAccountId == null ||
            status.worker.stage != "signed_in") return
        expireBeforePaint()
        mutableState.value = mutableState.value.copy(loading = true, failed = false)
        started = nowMillis()
        val reply = try { command(status.activeScope) }
        catch (error: CancellationException) { throw error }
        catch (_: Exception) { failed(); return }
        if (reply.hasErrorCode() || reply.generation == 0L) { failed(); return }
        pending = reply.generation
        val generation = reply.generation
        requestDeadline?.cancel()
        requestDeadline = scope.launch {
            delay(30_000)
            lock.withLock {
                if (pending == generation) {
                    pending = null
                    failed()
                    runCatching { cancel(status.activeScope, generation) }
                        .onFailure { if (it is CancellationException) throw it }
                }
            }
        }
    }

    private fun scheduleExpiry() {
        val value = mutableState.value
        val expires = value.deadlineMillis ?: return
        if (!visible || !foreground || value.stale) return
        deadline = scope.launch {
            delay((expires - nowMillis()).coerceAtLeast(0))
            lock.withLock {
                if (mutableState.value.deadlineMillis == expires && mutableState.value.accountId == value.accountId &&
                    mutableState.value.ownerScope == value.ownerScope) {
                    mutableState.value = mutableState.value.copy(stale = true)
                    request()
                }
            }
        }
    }
}
