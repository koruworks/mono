// SPDX-License-Identifier: MPL-2.0

package app.koru.mobile.services

import app.koru.contract.api.v1.*
import com.google.protobuf.ByteString
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import java.security.MessageDigest

/** Public build coordinates; never read from intents or account responses. */
internal data class AndroidAccountConfig(
    val origin: String,
    val issuer: String,
    val serverClientId: String,
    val channel: String,
    val credentialRollout: String = "",
    /** A local object worker this build may send blob bytes to, beside the
     *  compiled-in per-region set. Empty on every build that is not pointed at
     *  a development stack, and **not** part of [present]: a phone with no
     *  local stack to name is a correctly configured phone. */
    val blobByteOrigins: String = "",
) {
    val present: Boolean get() = listOf(origin, issuer, serverClientId, channel).all { it.isNotBlank() }
}

internal interface AccountNativePort {
    suspend fun command(request: AccountCommandRequest): AccountCommandReply
    suspend fun poll(request: AccountPollRequest): AccountPollReply
    suspend fun platform(request: AccountPlatformRequest): MutationResult
}

internal interface GoogleAccountDialog {
    suspend fun signIn(prompt: NativeSignInPrompt): GoogleAccountResult
    suspend fun clearSession()
}

/** Owned temporary proof. Never a UI value and never a data-class toString. */
internal class GoogleAccountResult(val token: ByteArray? = null, val cancelled: Boolean = false) {
    fun wipe() { token?.fill(0) }
    override fun toString() = "GoogleAccountResult(REDACTED)"
}

internal const val MAX_GOOGLE_TOKEN_BYTES = 16 * 1024

data class AndroidAccountStatus(
    val worker: AccountDiagnostics = AccountDiagnostics(),
    val generation: Long = 0,
    val adapterFailure: String? = null,
    val signInAgain: Boolean = false,
    val resultsLost: Boolean = false,
    val capability: Boolean = false,
    val queueDepth: Int = 0,
    val requesterActive: Boolean? = null,
    val activeScope: Long = 0,
    val activeAccountId: String? = null,
    val heldAccounts: List<HeldAccount> = emptyList(),
    /** Local poll-start ordering only; never a Cloud account generation. */
    val pollSequence: Long = 0,
)

data class HeldAccount(
    val scopeId: Long,
    val accountId: String,
    val authIdentityId: String?,
    val stage: String,
    val credentialReady: Boolean,
    val failureCode: String?,
    val activeGenerations: List<Long>,
    val requesterActive: Boolean?,
)

/** Application-lifetime effects over Rust's worker. This owns no account policy
 * or durable continuation. A rotation changes only the dialog's Activity. */
internal class AndroidAccountAdapter(
    private val native: AccountNativePort,
    private val dialog: GoogleAccountDialog,
    private val vault: AccountCredentialVault,
    private val scope: CoroutineScope,
    private val browser: BrowserSignInLauncher = BrowserSignInLauncher.Unavailable,
    private val trace: AccountTrace = AccountTrace(),
    private val nowMillis: () -> Long = System::currentTimeMillis,
) {
    companion object { const val REQUESTER = 2L }
    private val lock = Mutex()
    private val pollSequence = java.util.concurrent.atomic.AtomicLong()
    internal fun pollBoundary(): Long = pollSequence.get()
    @Volatile private var desiredForeground = false
    private var sentForeground: Boolean? = null
    private var after = 0L
    private var nativeEffect: NativeEffect? = null
    private var vaultEffect: VaultEffect? = null
    private var completedNative: Pair<Long, Long>? = null
    private var completedVault: Long? = null
    private var observedAccountInventory = false
    private var previousHeldScopes: Set<Long> = emptySet()
    private var providerClear: Deferred<Boolean>? = null
    private var providerClearRequested = false
    // One browser sign-in page at a time. `browserHandled` is the last
    // (generation, request_id) opened or refused, so a prompt that stays in
    // every poll while the person is in the browser opens exactly once.
    // `browserAwaiting` is the page an App Link may still answer; the link
    // itself is handed over from the main thread and delivered by the tick.
    private var browserHandled: Pair<Long, Long>? = null
    @Volatile private var browserAwaiting: Pair<Long, Long>? = null
    private var browserRefusal: Pair<Long, Long>? = null
    private val browserReturn = java.util.concurrent.atomic.AtomicReference<String?>(null)
    private val mutableStatus = MutableStateFlow(AndroidAccountStatus())
    val status = mutableStatus.asStateFlow()
    // Results contain bounded account/Profile projections, never SDK/vault data.
    private val mutableResults = MutableStateFlow<List<MobileAccountResult>>(emptyList())
    val results = mutableResults.asStateFlow()

    private class NativeEffect(val generation: Long, val id: Long, val fingerprint: ByteArray) {
        lateinit var job: Job
        private var retired = false
        private var value: GoogleAccountResult? = null
        @Synchronized fun complete(result: GoogleAccountResult) {
            if (retired) result.wipe() else value = result
        }
        @Synchronized fun response(): GoogleAccountResult? = value
        @Synchronized fun wipe() { retired = true; value?.wipe(); value = null; fingerprint.fill(0) }
    }
    private class VaultResult(val success: Boolean, val credential: ByteArray? = null) {
        fun wipe() { credential?.fill(0) }
    }
    private class VaultEffect(val id: Long, val fingerprint: ByteArray, val job: Deferred<VaultResult>) {
        var response: VaultResult? = null
        fun wipe() { response?.wipe(); fingerprint.fill(0) }
    }

    fun foreground(value: Boolean) { desiredForeground = value }

    suspend fun command(action: AccountAction, generation: Long = 0, locale: String = "", profileId: String = "", accountId: String = "", expectedOwnerScope: Long? = null, profileIds: List<String> = emptyList(), cloudSyncEnabled: Boolean? = null,
                        cloudWorkspaceId: String? = null, cloudIncarnationId: String? = null, cloudCursor: String? = null,
                        cloudDocumentSelection: CloudCopySelection? = null, cloudFileNodeId: String? = null): AccountCommandReply = lock.withLock {
        val request = AccountCommandRequest.newBuilder().setVersion(1).setRequester(REQUESTER)
            .setAction(action).setGeneration(generation).setLocale(locale).setProfileId(profileId).setAccountId(accountId)
            .addAllProfileIds(profileIds).apply { expectedOwnerScope?.let(::setExpectedOwnerScope) }
            .apply { cloudSyncEnabled?.let(::setCloudSyncEnabled) }
            .apply { cloudWorkspaceId?.let(::setCloudWorkspaceId); cloudIncarnationId?.let(::setCloudIncarnationId); cloudCursor?.let(::setCloudCursor) }
            .apply { cloudDocumentSelection?.let { setCloudDocumentSelectionJson(it.json()) }; cloudFileNodeId?.let(::setCloudFileNodeId) }.build()
        MobileAccountContract.validate(request)
        if (action in setOf(AccountAction.ACCOUNT_ACTION_START_GOOGLE, AccountAction.ACCOUNT_ACTION_START_APPLE, AccountAction.ACCOUNT_ACTION_START_EMAIL, AccountAction.ACCOUNT_ACTION_ADD_ACCOUNT) && mutableStatus.value.adapterFailure == "provider_session_clear_failed") {
            providerClearRequested = true // one new attempt on explicit user intent
        }
        trace.emit(AccountTrace.Stage.Command, AccountTrace.Outcome.Started, generation, action = action)
        try {
            native.command(request).also { reply ->
                trace.emit(AccountTrace.Stage.Command,
                    if (reply.hasErrorCode()) AccountTrace.Outcome.Failed else AccountTrace.Outcome.Completed,
                    reply.generation,
                    if (reply.hasErrorCode()) AccountTrace.Reason.NativeRejected else AccountTrace.Reason.None, action = action)
            }
        } catch (error: Exception) {
            trace.emit(AccountTrace.Stage.Command, AccountTrace.Outcome.Failed, generation, AccountTrace.reason(error))
            throw error
        }
    }

    /** One bounded observation. Slow SDK/vault calls run independently so
     * lifecycle and the other outstanding operation remain responsive. */
    suspend fun tick(): Boolean = lock.withLock {
        val foreground = desiredForeground
        if (sentForeground != foreground) {
            val reply = native.platform(envelope().setForeground(foreground).build())
            if (!reply.hasErrorCode()) sentForeground = foreground
        }
        val observationSequence = pollSequence.updateAndGet { if (it == Long.MAX_VALUE) it else it + 1 }
        val reply = native.poll(AccountPollRequest.newBuilder().setVersion(1).setRequester(REQUESTER)
            .setAfterSequence(after).setIncludePlatform(true).build())
        if (!reply.hasStatus()) {
            failure("account_adapter_unavailable")
            return@withLock false // failed poll is not authoritative prompt absence
        }
        clearFailure("account_adapter_unavailable")
        if (reply.hasStatus()) {
            val worker = reply.status
            mutableStatus.value = mutableStatus.value.copy(worker = AccountDiagnostics.from(worker), pollSequence = observationSequence,
                generation = worker.generation, signInAgain = worker.stage == "signed_out",
                resultsLost = reply.errorCode == "account_result_gap", capability = worker.capability, queueDepth = worker.queueDepth,
                requesterActive = worker.requesterActive.takeIf { worker.hasRequesterActive() },
                activeScope = worker.activeScope,
                activeAccountId = worker.activeAccountId.takeIf { worker.hasActiveAccountId() },
                heldAccounts = worker.heldAccountsList.map { row -> HeldAccount(
                    row.scopeId, row.accountId, row.authIdentityId.takeIf { row.hasAuthIdentityId() },
                    row.stage, row.credentialReady, row.failureCode.takeIf { row.hasFailureCode() },
                    row.activeGenerationsList, row.requesterActive.takeIf { row.hasRequesterActive() },
                ) })
            val heldScopes = worker.heldAccountsList.map { it.scopeId }.toSet()
            // Selection can show an anonymous SignedOut scope while A still
            // owns an SDK/exchange. Only actual retirement (including a lost
            // result), an owned logout result, or empty initial setup clears it.
            if ((previousHeldScopes - heldScopes).isNotEmpty() ||
                (!observedAccountInventory && worker.stage == "signed_out" && worker.activeScope == 0L && heldScopes.isEmpty())) {
                providerClearRequested = true
            }
            previousHeldScopes = heldScopes
            observedAccountInventory = true
        }
        if (reply.errorCode == "account_result_gap") mutableResults.value = emptyList()
        if (reply.resultsCount > 0) {
            val incoming = reply.resultsList.filter { it.requester == REQUESTER && it.sequence > after }
            if (incoming.isNotEmpty()) {
                if (incoming.any { it.kind == "signed_out" }) providerClearRequested = true
                after = incoming.maxOf { it.sequence }
                mutableResults.value = (mutableResults.value + incoming).takeLast(16)
            }
        }
        providerClear?.takeIf { it.isCompleted }?.let {
            if (!it.await()) failure("provider_session_clear_failed") else clearFailure("provider_session_clear_failed")
            providerClear = null
        }
        if (providerClear == null && providerClearRequested && nativeEffect == null) {
            providerClearRequested = false
            providerClear = scope.async {
                trace.emit(AccountTrace.Stage.ProviderClear, AccountTrace.Outcome.Started)
                try {
                    withTimeout(30_000) { dialog.clearSession() }
                    trace.emit(AccountTrace.Stage.ProviderClear, AccountTrace.Outcome.Completed)
                    true
                } catch (error: Exception) {
                    trace.emit(AccountTrace.Stage.ProviderClear, AccountTrace.Outcome.Failed,
                        reason = if (error is TimeoutCancellationException) AccountTrace.Reason.Timeout else AccountTrace.reason(error))
                    false
                }
            }
        }
        serviceNative(reply.takeIf { it.hasSignIn() }?.signIn)
        serviceBrowser(reply.takeIf { it.hasBrowser() }?.browser)
        serviceVault(reply.takeIf { it.hasVault() }?.vault)
        true
    }

    private suspend fun serviceNative(prompt: NativeSignInPrompt?) {
        nativeEffect?.let { effect ->
            if (prompt?.requestId != effect.id || prompt.generation != effect.generation) {
                // Rust already consumed/cancelled this request. Never repeat the
                // sheet after a lost JNI acknowledgement. SDK is not a commit.
                if (!effect.job.isCompleted) effect.job.cancel()
                trace.emit(AccountTrace.Stage.NativeDelivery, AccountTrace.Outcome.Retired, effect.generation)
                effect.wipe(); nativeEffect = null
            } else if (!effect.fingerprint.contentEquals(fingerprint(prompt.toByteArray()))) {
                failure("account_prompt_changed")
                return
            } else effect.response()?.let { result ->
                val response = NativeSignInReply.newBuilder().setGeneration(effect.generation)
                    .setRequestId(effect.id).setCancelled(result.cancelled)
                result.token?.let { response.idToken = ByteString.copyFrom(it) }
                val ack = native.platform(envelope().setSignIn(response).build())
                if (!ack.hasErrorCode() || ack.errorCode == "account_response_stale") {
                    trace.emit(AccountTrace.Stage.NativeDelivery,
                        if (ack.hasErrorCode()) AccountTrace.Outcome.Stale else AccountTrace.Outcome.Completed,
                        effect.generation)
                    completedNative = effect.generation to effect.id
                    effect.wipe(); nativeEffect = null
                }
                return
            }
        }
        if (nativeEffect == null && prompt != null && desiredForeground && providerClear == null && !providerClearRequested && completedNative != (prompt.generation to prompt.requestId)) {
            clearFailure("account_prompt_changed")
            val effect = NativeEffect(prompt.generation, prompt.requestId, fingerprint(prompt.toByteArray()))
            effect.job = scope.launch {
                var result = GoogleAccountResult()
                val started = System.nanoTime()
                var reason = AccountTrace.Reason.None
                trace.emit(AccountTrace.Stage.GoogleDialog, AccountTrace.Outcome.Started, prompt.generation)
                val remaining = (prompt.expiresAtMillis - nowMillis()).coerceAtMost(120_000)
                try {
                    if (remaining > 0) withTimeout(remaining) { result = dialog.signIn(prompt) }
                    else reason = AccountTrace.Reason.DeadlineExpired
                    val token = result.token
                    if (token != null && (token.isEmpty() || token.size > MAX_GOOGLE_TOKEN_BYTES)) {
                        result.wipe(); result = GoogleAccountResult()
                        reason = AccountTrace.Reason.InvalidTokenSize
                    }
                } catch (_: TimeoutCancellationException) {
                    reason = AccountTrace.Reason.Timeout; result.wipe(); result = GoogleAccountResult()
                } catch (_: CancellationException) {
                    reason = AccountTrace.Reason.CoroutineCancelled; result.wipe(); result = GoogleAccountResult()
                } catch (error: Exception) {
                    reason = AccountTrace.reason(error); result.wipe(); result = GoogleAccountResult()
                } finally {
                    trace.emit(AccountTrace.Stage.GoogleDialog, when {
                        result.cancelled || reason == AccountTrace.Reason.CoroutineCancelled -> AccountTrace.Outcome.Cancelled
                        result.token != null -> AccountTrace.Outcome.Completed
                        else -> AccountTrace.Outcome.Failed
                    }, prompt.generation, reason, (System.nanoTime() - started) / 1_000_000)
                    effect.complete(result)
                }
            }
            nativeEffect = effect
        }
    }

    /** The verified App Link came back from the browser. Accepted only while a
     * page this adapter opened is still waiting; anything else is dropped
     * unread. The URL carries a one-use grant and is never logged. */
    fun browserReturned(url: String): Boolean {
        if (browserAwaiting == null) {
            trace.emit(AccountTrace.Stage.BrowserDelivery, AccountTrace.Outcome.Retired,
                reason = AccountTrace.Reason.NoOutstandingPrompt)
            return false
        }
        if (!BrowserSignIn.isCallback(url)) {
            trace.emit(AccountTrace.Stage.BrowserDelivery, AccountTrace.Outcome.Failed,
                reason = AccountTrace.Reason.CallbackRefused)
            return false
        }
        browserReturn.set(url)
        return true
    }

    private suspend fun serviceBrowser(prompt: BrowserSignInPrompt?) {
        val key = prompt?.let { it.generation to it.requestId }
        // A returned link answers the page it came back to: the most recent
        // one opened. Rust matches the flow's state itself, so a link that
        // belongs to an older page is refused there, never here by guesswork.
        browserReturn.get()?.let { url ->
            val awaiting = browserAwaiting
            if (awaiting == null) { browserReturn.compareAndSet(url, null); return@let }
            if (deliverBrowser(awaiting) { it.setCallbackUrl(url) }) {
                browserReturn.compareAndSet(url, null)
                browserAwaiting = null
            }
        }
        // Rust stops publishing a prompt once it is answered, cancelled or
        // timed out. An authoritative poll without it retires the wait, so a
        // late App Link is ignored rather than sent to a finished flow.
        if (browserAwaiting != null && browserAwaiting != key) browserAwaiting = null
        if (prompt != null && key != browserHandled && desiredForeground) {
            browserHandled = key
            if (!BrowserSignIn.isProviderPage(prompt.url)) {
                trace.emit(AccountTrace.Stage.Browser, AccountTrace.Outcome.Failed, prompt.generation,
                    AccountTrace.Reason.ProviderPageRefused)
                browserRefusal = key
            } else {
                trace.emit(AccountTrace.Stage.Browser, AccountTrace.Outcome.Started, prompt.generation)
                browserAwaiting = key
                val opened = try { browser.open(prompt.url) }
                    catch (cancelled: CancellationException) { throw cancelled }
                    catch (_: Exception) { false }
                if (!opened) {
                    trace.emit(AccountTrace.Stage.Browser, AccountTrace.Outcome.Failed, prompt.generation,
                        AccountTrace.Reason.NoBrowser)
                    browserAwaiting = null
                    browserRefusal = key
                }
            }
        }
        // A page that could not be shown is answered as cancelled at once,
        // rather than leaving the flow to its ten-minute ceiling.
        browserRefusal?.let { refused ->
            if (deliverBrowser(refused) { it.setCancelled(true) }) browserRefusal = null
        }
    }

    /** True once Rust has answered, including stale: never repeat after that. */
    private suspend fun deliverBrowser(key: Pair<Long, Long>, fill: (BrowserSignInReply.Builder) -> Unit): Boolean {
        val reply = BrowserSignInReply.newBuilder().setGeneration(key.first).setRequestId(key.second).also(fill)
        val ack = try { native.platform(envelope().setBrowser(reply).build()) }
            catch (cancelled: CancellationException) { throw cancelled }
            catch (_: Exception) {
                // A lost acknowledgement is retried on the next tick; Rust
                // answers a repeat as stale.
                trace.emit(AccountTrace.Stage.BrowserDelivery, AccountTrace.Outcome.Failed, key.first,
                    AccountTrace.Reason.OtherException)
                return false
            }
        trace.emit(AccountTrace.Stage.BrowserDelivery, when {
            !ack.hasErrorCode() -> AccountTrace.Outcome.Completed
            ack.errorCode == "account_response_stale" -> AccountTrace.Outcome.Stale
            else -> AccountTrace.Outcome.Failed
        }, key.first, if (ack.hasErrorCode() && ack.errorCode != "account_response_stale") AccountTrace.Reason.NativeRejected else AccountTrace.Reason.None)
        return true
    }

    private suspend fun serviceVault(prompt: AccountVaultPrompt?) {
        vaultEffect?.let { effect ->
            if (!effect.job.isCompleted) return // a durable write cannot be abandoned
            if (effect.response == null) effect.response = effect.job.await()
            if (prompt?.requestId != effect.id) {
                effect.wipe(); vaultEffect = null
            } else if (!effect.fingerprint.contentEquals(fingerprint(prompt.toByteArray()))) {
                failure("account_prompt_changed")
                return
            } else effect.response?.let { result ->
                val response = AccountVaultReply.newBuilder().setRequestId(effect.id).setSuccess(result.success)
                result.credential?.let { response.credential = ByteString.copyFrom(it) }
                val ack = native.platform(envelope().setVault(response).build())
                if (!ack.hasErrorCode() || ack.errorCode == "account_response_stale") {
                    trace.emit(AccountTrace.Stage.VaultDelivery,
                        if (ack.hasErrorCode()) AccountTrace.Outcome.Stale else AccountTrace.Outcome.Completed)
                    completedVault = effect.id
                    effect.wipe(); vaultEffect = null
                }
                return
            }
        }
        if (vaultEffect == null && prompt != null && completedVault != prompt.requestId) {
            val bytes = if (prompt.hasCredential()) prompt.credential.toByteArray() else null
            val job = scope.async {
                trace.emit(AccountTrace.Stage.Vault, AccountTrace.Outcome.Started, vaultAction = prompt.action)
                try {
                    when (prompt.action) {
                        AccountVaultAction.ACCOUNT_VAULT_ACTION_LOAD -> VaultResult(true, vault.load(prompt.entry))
                        AccountVaultAction.ACCOUNT_VAULT_ACTION_STORE -> {
                            require(bytes != null); vault.store(prompt.entry, bytes); VaultResult(true)
                        }
                        AccountVaultAction.ACCOUNT_VAULT_ACTION_CLEAR -> { vault.clear(prompt.entry); VaultResult(true) }
                        else -> VaultResult(false)
                    }.also { result -> trace.emit(AccountTrace.Stage.Vault,
                        if (result.success) AccountTrace.Outcome.Completed else AccountTrace.Outcome.Failed, vaultAction = prompt.action) }
                } catch (error: Exception) {
                    trace.emit(AccountTrace.Stage.Vault, AccountTrace.Outcome.Failed, reason = AccountTrace.reason(error), vaultAction = prompt.action)
                    VaultResult(false)
                }
                finally { bytes?.fill(0) }
            }
            vaultEffect = VaultEffect(prompt.requestId, fingerprint(prompt.toByteArray()), job)
        }
    }

    fun failure(code: String) { mutableStatus.value = mutableStatus.value.copy(adapterFailure = code) }
    private fun clearFailure(code: String) {
        if (mutableStatus.value.adapterFailure == code) mutableStatus.value = mutableStatus.value.copy(adapterFailure = null)
    }
    private fun envelope() = AccountPlatformRequest.newBuilder().setVersion(1).setRequester(REQUESTER)
    private fun fingerprint(bytes: ByteArray): ByteArray = try { MessageDigest.getInstance("SHA-256").digest(bytes) } finally { bytes.fill(0) }
}
