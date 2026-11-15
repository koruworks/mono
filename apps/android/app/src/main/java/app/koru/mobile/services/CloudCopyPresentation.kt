// SPDX-License-Identifier: MPL-2.0
package app.koru.mobile.services

import app.koru.contract.api.v1.AccountCommandReply
import app.koru.contract.api.v1.MobileAccountResult
import app.koru.mobile.domain.BlockId
import app.koru.mobile.ui.editor.CloudCopyPhase
import app.koru.mobile.ui.editor.CloudCopyViewKey
import kotlinx.coroutines.*
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
) {
    private val lock = Mutex()
    private val mutableState = MutableStateFlow(CloudCopyState())
    val state = mutableState.asStateFlow()
    private var status = AndroidAccountStatus()
    private var foreground = false
    private var sequence = 0L
    private var epoch = 0L
    private data class Pending(val key: CloudCopyViewKey, val generation: Long, val node: BlockId?)
    private var pending: Pending? = null
    private var timeout: Job? = null

    private suspend fun retire() {
        timeout?.cancel(); timeout = null
        val old = pending; pending = null
        if (old != null) try { cancel(old.key.ownerScope, old.generation) }
        catch (error: CancellationException) { throw error }
        catch (_: Exception) { /* Native owns its bounded cancellation and retention. */ }
    }
    private suspend fun align(next: AndroidAccountStatus) {
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
    private suspend fun request(key: CloudCopyViewKey, node: BlockId?) {
        val reply = try { command(key.ownerScope, key.selection, node) }
        catch (error: CancellationException) { throw error }
        catch (_: Exception) { fail(node); return }
        if (reply.hasErrorCode() || reply.generation == 0L) { fail(node); return }
        val operation = Pending(key, reply.generation, node)
        pending = operation
        timeout = scope.launch {
            delay(30_000)
            lock.withLock {
                if (pending == operation) { timeout = null; retire(); fail(node) }
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
    suspend fun observe(next: AndroidAccountStatus, results: List<MobileAccountResult>) = lock.withLock {
        align(next)
        for (result in results) {
            if (result.sequence <= sequence) continue
            sequence = maxOf(sequence, result.sequence)
            val operation = pending ?: continue
            if (result.requester != AndroidAccountAdapter.REQUESTER || result.ownerScope != operation.key.ownerScope ||
                result.generation != operation.generation) continue
            val removal = operation.node != null
            if (result.kind != if (removal) "cloud_file_removal" else "cloud_document_inspection") continue
            timeout?.cancel(); timeout = null; pending = null
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
                refreshUsage()
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
