// SPDX-License-Identifier: MPL-2.0

package app.koru.mobile.services

import app.koru.contract.api.v1.AccountCommandReply
import app.koru.contract.api.v1.MobileAccountResult
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock

sealed interface CloudInventorySelection {
    data object Workspaces : CloudInventorySelection
    data class Documents(val workspace: String, val incarnation: String, val name: String?) : CloudInventorySelection
}
data class CloudInventoryState(
    val ownerScope: Long = 0, val accountId: String? = null,
    val selection: CloudInventorySelection = CloudInventorySelection.Workspaces,
    val page: CloudInventoryPage? = null,
    val loading: Boolean = false, val failed: Boolean = false, val restarted: Boolean = false,
    val firstPage: Boolean = true, val canPrevious: Boolean = false,
) {
    fun forAccount(owner: Long, account: String?): CloudInventoryState =
        if (owner == ownerScope && account != null && account == accountId) this else CloudInventoryState()
}

/** Keeps one page and at most 32 back cursors. Every cursor is opaque to UI;
 * an epoch mismatch restarts once, never joins two different inventories. */
internal class CloudInventoryPresentation(
    private val scope: CoroutineScope,
    private val read: suspend (Long, CloudInventorySelection, String?) -> AccountCommandReply,
    private val cancel: suspend (Long, Long) -> Unit,
) {
    private val lock = Mutex()
    private val mutableState = MutableStateFlow(CloudInventoryState())
    val state = mutableState.asStateFlow()
    private var status = AndroidAccountStatus()
    private var visible = false
    private var foreground = false
    private var sequence = 0L
    private var pending: Long? = null
    private var timeout: Job? = null
    private var cursor: String? = null
    private val history = mutableListOf<String?>()
    private var workspaceEpoch: CloudWorkspaceEpoch? = null
    private var documentEpoch: CloudDocumentEpoch? = null
    private var restarted = false

    private suspend fun retire() {
        timeout?.cancel(); timeout = null
        val generation = pending; pending = null
        if (generation != null) try { cancel(mutableState.value.ownerScope, generation) }
        catch (error: CancellationException) { throw error }
        catch (_: Exception) { /* Rust also owns its bounded read deadline. */ }
    }
    private fun resetPage() {
        cursor = null; history.clear(); workspaceEpoch = null; documentEpoch = null
        mutableState.value = mutableState.value.copy(page = null, firstPage = true, canPrevious = false,
            loading = false, failed = false)
    }
    private suspend fun align(next: AndroidAccountStatus): Boolean {
        val changed = status.activeScope != next.activeScope || status.activeAccountId != next.activeAccountId
        if (changed || next.worker.stage == "signed_out") {
            retire(); resetPage(); restarted = false
            mutableState.value = CloudInventoryState(next.activeScope, next.activeAccountId)
        }
        status = next
        return changed
    }
    suspend fun observe(next: AndroidAccountStatus, results: List<MobileAccountResult>) = lock.withLock {
        val ready = next.worker.stage == "signed_in" && status.worker.stage != "signed_in"
        val changed = align(next)
        for (result in results) {
            if (result.sequence <= sequence) continue
            sequence = maxOf(sequence, result.sequence)
            if (result.requester != AndroidAccountAdapter.REQUESTER || result.ownerScope != status.activeScope ||
                result.generation != pending || status.activeAccountId == null) continue
            pending = null; timeout?.cancel(); timeout = null
            val selection = mutableState.value.selection
            val expectedKind = if (selection is CloudInventorySelection.Workspaces) "cloud_usage_workspaces" else "cloud_usage_documents"
            val selectorsMatch = when (selection) {
                CloudInventorySelection.Workspaces -> !result.hasCloudWorkspaceId() && !result.hasCloudIncarnationId()
                is CloudInventorySelection.Documents -> result.hasCloudWorkspaceId() && result.hasCloudIncarnationId() &&
                    result.cloudWorkspaceId == selection.workspace && result.cloudIncarnationId == selection.incarnation
            }
            if (result.kind != expectedKind || !selectorsMatch ||
                (result.hasErrorCode() && result.hasCloudInventoryJson())) { fail(); continue }
            if (result.hasErrorCode()) {
                if (result.errorCode == "usage_revision_changed") restart() else fail()
                continue
            }
            val page = if (result.hasCloudInventoryJson()) CloudInventoryDecoder.decode(result.cloudInventoryJson) else null
            val valid = when {
                page is CloudInventoryPage.Workspaces && selection is CloudInventorySelection.Workspaces -> true
                page is CloudInventoryPage.Documents && selection is CloudInventorySelection.Documents ->
                    page.epoch.workspace == selection.workspace && page.epoch.incarnation == selection.incarnation
                else -> false
            }
            if (!valid) { fail(); continue }
            val changedEpoch = when (page) {
                is CloudInventoryPage.Workspaces -> workspaceEpoch?.let { it != page.epoch } ?: false
                is CloudInventoryPage.Documents -> documentEpoch?.let { it != page.epoch } ?: false
                else -> false
            }
            if (changedEpoch) { restart(); continue }
            when (page) {
                is CloudInventoryPage.Workspaces -> workspaceEpoch = page.epoch
                is CloudInventoryPage.Documents -> documentEpoch = page.epoch
                else -> Unit
            }
            mutableState.value = mutableState.value.copy(page = page, loading = false, failed = false,
                restarted = restarted, firstPage = cursor == null, canPrevious = history.isNotEmpty())
        }
        if (next.resultsLost && pending != null) { retire(); fail() }
        if ((changed || ready) && visible && foreground) request()
    }
    private fun fail() { mutableState.value = mutableState.value.copy(page = null, loading = false, failed = true) }
    private suspend fun restart() {
        resetPage()
        if (restarted) { fail(); return }
        restarted = true
        mutableState.value = mutableState.value.copy(restarted = true)
        request()
    }
    suspend fun visibility(value: Boolean, next: AndroidAccountStatus) = lock.withLock {
        align(next)
        val entering = value && !visible; visible = value
        if (!value) { retire(); mutableState.value = mutableState.value.copy(loading = false) }
        else if (entering && foreground) { resetPage(); restarted = false; request() }
    }
    suspend fun foreground(value: Boolean, next: AndroidAccountStatus) = lock.withLock {
        align(next)
        val returning = value && !foreground; foreground = value
        if (!value) { retire(); resetPage() }
        else if (returning && visible) { restarted = false; request() }
    }
    suspend fun first(owner: Long) = lock.withLock {
        if (owner != status.activeScope) return@withLock
        retire(); resetPage(); restarted = false; request()
    }
    suspend fun workspaces(owner: Long) = lock.withLock {
        if (owner != status.activeScope) return@withLock
        retire(); resetPage(); restarted = false
        mutableState.value = mutableState.value.copy(selection = CloudInventorySelection.Workspaces)
        request()
    }
    suspend fun documents(row: CloudWorkspaceRow, owner: Long) = lock.withLock {
        if (owner != status.activeScope || row !in (mutableState.value.page as? CloudInventoryPage.Workspaces)?.rows.orEmpty()) return@withLock
        retire(); resetPage(); restarted = false
        mutableState.value = mutableState.value.copy(selection = CloudInventorySelection.Documents(row.workspace, row.incarnation, row.name))
        request()
    }
    suspend fun next(owner: Long) = lock.withLock {
        if (owner != status.activeScope || pending != null) return@withLock
        val next = mutableState.value.page?.nextCursor ?: return@withLock
        history += cursor
        if (history.size > 32) history.removeAt(0)
        cursor = next; request()
    }
    suspend fun previous(owner: Long) = lock.withLock {
        if (owner != status.activeScope || pending != null || history.isEmpty()) return@withLock
        cursor = history.removeAt(history.lastIndex); request()
    }
    private suspend fun request() {
        if (!visible || !foreground || pending != null || status.activeAccountId == null || status.worker.stage != "signed_in") return
        mutableState.value = mutableState.value.copy(loading = true, failed = false, restarted = restarted)
        val reply = try { read(status.activeScope, mutableState.value.selection, cursor) }
        catch (error: CancellationException) { throw error }
        catch (_: Exception) { fail(); return }
        if (reply.hasErrorCode() || reply.generation == 0L) { fail(); return }
        pending = reply.generation
        val generation = reply.generation
        timeout = scope.launch {
            delay(30_000)
            lock.withLock {
                if (pending == generation) {
                    pending = null; fail()
                    try { cancel(status.activeScope, generation) }
                    catch (error: CancellationException) { throw error }
                    catch (_: Exception) { /* The read is still bounded by Rust. */ }
                }
            }
        }
    }
}
