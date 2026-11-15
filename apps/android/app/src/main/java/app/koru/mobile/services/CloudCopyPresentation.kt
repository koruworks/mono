// SPDX-License-Identifier: MPL-2.0
package app.koru.mobile.services

import app.koru.contract.api.v1.AccountCommandReply
import app.koru.contract.api.v1.MobileAccountResult
import app.koru.mobile.domain.BlockId
import app.koru.mobile.ui.editor.CloudCopyPhase
import app.koru.mobile.ui.editor.CloudCopyViewKey
import kotlinx.coroutines.*
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock

internal data class CloudCopyState(
    val key: CloudCopyViewKey? = null,
    val phase: CloudCopyPhase = CloudCopyPhase.Unavailable,
    val copy: CloudDocumentCopy? = null,
) {
    fun forAccount(owner: Long, account: String?): CloudCopyState =
        if (key != null && key.ownerScope == owner && key.selection.accountId == account) this else CloudCopyState()
}

/** One exact Cloud copy, no local Store/editor state and no automatic write retry. */
internal class CloudCopyPresentation(
    private val scope: CoroutineScope,
    private val command: suspend (Long, CloudCopySelection, BlockId?) -> AccountCommandReply,
    private val cancel: suspend (Long, Long) -> Unit,
    private val refreshUsage: suspend () -> Unit,
    private val pollBoundary: () -> Long,
) {
    private val lock = Mutex()
    private val mutableState = MutableStateFlow(CloudCopyState())
    val state = mutableState.asStateFlow()
    private var status = AndroidAccountStatus()
    private var foreground = false
    private var sequence = 0L
    private var epoch = 0L
    // This lease survives view retirement and late admission. At most one
    // transport admission/cancellation can exist, even if JNI never returns.
    private class Operation(val key: CloudCopyViewKey, val node: BlockId?) {
        var generation: Long? = null
        var retired = false
        var cancelling = false
        var cancellationFinished = false
        var drained = false
        var pollBoundary = Long.MAX_VALUE
    }
    private var pending: Operation? = null
    private var timeout: Job? = null
    private val refreshes = Channel<Unit>(Channel.CONFLATED)
    init {
        scope.launch {
            for (ignored in refreshes) try { refreshUsage() }
            catch (error: CancellationException) { throw error }
            catch (_: Exception) { /* Authoritative presenters retain unavailable state. */ }
        }
    }

    private fun retire() {
        timeout?.cancel(); timeout = null
        pending?.let { it.retired = true; cancelRetired(it) }
    }
    private fun cancelRetired(operation: Operation) {
        val generation = operation.generation ?: return
        if (!operation.retired || operation.cancelling) return
        operation.cancelling = true
        scope.launch {
            try { cancel(operation.key.ownerScope, generation) }
            catch (error: CancellationException) { throw error }
            catch (_: Exception) { /* The terminal operation result can still establish drainage. */ }
            val boundary = pollBoundary()
            lock.withLock {
                operation.cancellationFinished = true
                operation.pollBoundary = maxOf(operation.pollBoundary, boundary)
                // A Cancel acknowledgement alone does not prove the original
                // operation drained. Keep one lease until its terminal result.
                if (operation.drained && pending === operation) pending = null
            }
        }
    }
    private fun align(next: AndroidAccountStatus) {
        if (status.activeScope != next.activeScope || status.activeAccountId != next.activeAccountId ||
            next.worker.stage != "signed_in") {
            retire(); mutableState.value = CloudCopyState()
        }
        status = next
    }
    suspend fun open(selection: CloudCopySelection, next: AndroidAccountStatus) = lock.withLock {
        align(next)
        if (!foreground || next.worker.stage != "signed_in" || next.activeScope == 0L ||
            selection.accountId != next.activeAccountId || !selection.valid()) return@withLock
        retire()
        epoch += 1
        val key = CloudCopyViewKey(next.activeScope, epoch, selection)
        if (pending != null) { mutableState.value = CloudCopyState(key); return@withLock }
        mutableState.value = CloudCopyState(key, CloudCopyPhase.Loading)
        request(key, null)
    }
    suspend fun close() = lock.withLock { retire(); mutableState.value = CloudCopyState() }
    suspend fun foreground(value: Boolean, next: AndroidAccountStatus) = lock.withLock {
        align(next); foreground = value
        if (!value) {
            val write = pending?.node != null || mutableState.value.phase == CloudCopyPhase.OutcomeUnknown
            retire()
            mutableState.value = mutableState.value.copy(copy = null,
                phase = if (write) CloudCopyPhase.OutcomeUnknown else CloudCopyPhase.Unavailable)
        }
    }
    suspend fun remove(key: CloudCopyViewKey, node: BlockId) = lock.withLock {
        val current = mutableState.value
        if (!foreground || pending != null || current.key != key || current.phase != CloudCopyPhase.Verified ||
            current.copy?.permittedNodes?.contains(node) != true || status.activeScope != key.ownerScope ||
            status.activeAccountId != key.selection.accountId || status.worker.stage != "signed_in") return@withLock
        mutableState.value = current.copy(phase = CloudCopyPhase.Removing)
        request(key, node)
    }
    private fun request(key: CloudCopyViewKey, node: BlockId?) {
        check(pending == null)
        val operation = Operation(key, node)
        pending = operation
        scope.launch {
            val reply = try { command(key.ownerScope, key.selection, node) }
            catch (error: CancellationException) { throw error }
            catch (_: Exception) { null }
            val boundary = pollBoundary()
            lock.withLock {
                if (pending !== operation) return@withLock
                if (reply == null) {
                    // Admission may have reached native without a reply. Keep
                    // the lease quarantined rather than guessing it vanished.
                    operation.retired = true
                    timeout?.cancel(); timeout = null
                    if (mutableState.value.key == key) fail(node)
                } else if (reply.hasErrorCode() || reply.generation == 0L) {
                    pending = null; timeout?.cancel(); timeout = null
                    if (!operation.retired && mutableState.value.key == key) fail(node)
                } else {
                    operation.generation = reply.generation
                    operation.pollBoundary = boundary
                    cancelRetired(operation)
                }
            }
        }
        timeout = scope.launch {
            delay(30_000)
            lock.withLock {
                if (pending === operation) {
                    timeout = null; retire()
                    if (mutableState.value.key == key) fail(node)
                }
            }
        }
    }
    private fun fail(node: BlockId?, code: String? = null) {
        mutableState.value = mutableState.value.copy(copy = null, phase = when (code) {
            "cloud_document_source_changed" -> CloudCopyPhase.SourceChanged
            "cloud_edit_outcome_unknown" -> CloudCopyPhase.OutcomeUnknown
            "cloud_document_unavailable" -> CloudCopyPhase.Unavailable
            else -> if (node != null) CloudCopyPhase.OutcomeUnknown else CloudCopyPhase.Unavailable
        })
    }
    private fun refreshRetiredRemoval(operation: Operation) {
        if (!operation.drained && operation.node != null && status.activeScope == operation.key.ownerScope &&
            status.activeAccountId == operation.key.selection.accountId) refreshes.trySend(Unit)
    }
    suspend fun observe(next: AndroidAccountStatus, results: List<MobileAccountResult>) = lock.withLock {
        align(next)
        pending?.let { operation ->
            val generation = operation.generation
            // Only an explicitly present held owner from a poll STARTED after
            // admission and cancellation can replace a lost terminal result.
            if (operation.retired && operation.cancellationFinished && generation != null &&
                next.pollSequence > operation.pollBoundary && next.heldAccounts.any {
                    it.scopeId == operation.key.ownerScope && it.accountId == operation.key.selection.accountId &&
                        it.requesterActive == false && generation !in it.activeGenerations
                }) { refreshRetiredRemoval(operation); pending = null }
        }
        for (result in results) {
            if (result.sequence <= sequence) continue
            sequence = maxOf(sequence, result.sequence)
            val operation = pending ?: continue
            if (result.requester != AndroidAccountAdapter.REQUESTER || result.ownerScope != operation.key.ownerScope ||
                result.generation != operation.generation) continue
            val removal = operation.node != null
            if (result.kind != if (removal) "cloud_file_removal" else "cloud_document_inspection") continue
            timeout?.cancel(); timeout = null
            if (operation.retired) {
                refreshRetiredRemoval(operation)
                operation.drained = true
                if (!operation.cancelling || operation.cancellationFinished) pending = null
                continue
            }
            pending = null
            val selection = result.cloudDocumentSelectionJson.takeIf { result.hasCloudDocumentSelectionJson() }
            if (selection == null || CloudCopySelection.decode(selection) != operation.key.selection ||
                (removal && (!result.hasCloudFileNodeId() || result.cloudFileNodeId != operation.node?.value))) {
                fail(operation.node); continue
            }
            if (result.hasErrorCode()) {
                // Mixed success/failure is never an acknowledgement.
                val mixed = result.hasCloudDocumentInspectionJson() || result.cloudFileRemoved
                fail(operation.node, result.errorCode.takeUnless { mixed }); continue
            }
            if (removal) {
                if (!result.cloudFileRemoved || result.hasCloudDocumentInspectionJson()) {
                    fail(operation.node); continue
                }
                mutableState.value = CloudCopyState(operation.key, CloudCopyPhase.Acknowledged)
                refreshes.trySend(Unit)
            } else {
                val copy = if (!result.cloudFileRemoved && !result.hasCloudFileNodeId() && result.hasCloudDocumentInspectionJson())
                    CloudDocumentCopyDecoder.decode(result.cloudDocumentInspectionJson, operation.key.selection) else null
                mutableState.value = CloudCopyState(operation.key,
                    if (copy == null) CloudCopyPhase.Unavailable else CloudCopyPhase.Verified, copy)
            }
        }
        if (next.resultsLost && pending != null) {
            val node = pending?.node; retire(); fail(node)
        }
    }
}
