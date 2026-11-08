// SPDX-License-Identifier: MPL-2.0

package app.koru.mobile.services

import app.koru.contract.api.v1.AccountAction
import app.koru.contract.api.v1.AccountCommandReply
import app.koru.contract.api.v1.MobileAccountResult
import app.koru.mobile.domain.ProfileId
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.CancellationException

enum class AccountIntent { SignIn, SignInWithApple, SignInWithEmail, AddAccount, SwitchAccount, ConfirmCreation, CancelCreation, Cancel, Retry, Logout, RefreshProfiles, SelectProfiles, RelinkDevices, RetryEnrollment, CancelEnrollment }
enum class AccountPhase { Unavailable, SignedOut, Restoring, Provider, AwaitingCreation, Exchanging, SignedIn, Refreshing, Cancelled, Failed }
enum class ProfileEnrollment { Idle, Waiting, Enrolling, Complete, Failed }

private fun String?.isCredentialRepairFailure() = this in setOf(
    "account_credential_missing", "account_credential_rejected", "account_credential_invalid",
)

data class AuthorizedProfile(val id: ProfileId, val name: String, val local: Boolean, val remote: Boolean)
data class ProfileEnrollmentProgress(val id: ProfileId, val state: ProfileEnrollment,
    val failure: String? = null, val retryAtMillis: Long? = null) {
    companion object {
        internal fun from(row: ProfileEnrollmentDiagnostics) = ProfileEnrollmentProgress(ProfileId(row.profileId),
            when (row.stage) {
                "waiting" -> ProfileEnrollment.Waiting
                "enrolling" -> ProfileEnrollment.Enrolling
                "complete" -> ProfileEnrollment.Complete
                "failed" -> ProfileEnrollment.Failed
                else -> ProfileEnrollment.Idle
            }, row.failure, row.retryAtMillis)
    }
}
data class AccountScreenState(
    val phase: AccountPhase = AccountPhase.Unavailable,
    val capable: Boolean = false,
    val generation: Long = 0,
    val displayName: String? = null,
    val profiles: List<AuthorizedProfile> = emptyList(),
    val profilesLoaded: Boolean = false,
    val pending: AccountIntent? = null,
    val commandUncertain: Boolean = false,
    val failure: String? = null,
    val retryAtMillis: Long? = null,
    val enrollment: ProfileEnrollment = ProfileEnrollment.Idle,
    val enrollmentProfile: ProfileId? = null,
    val enrollmentFailure: String? = null,
    val enrollmentRetryAtMillis: Long? = null,
    val enrollments: List<ProfileEnrollmentProgress> = emptyList(),
    val activeScope: Long = 0,
    val activeAccountId: String? = null,
    val heldAccounts: List<HeldAccount> = emptyList(),
    val switchPending: Boolean = false,
    val switchRetryAvailable: Boolean = false,
) {
    val working get() = phase in setOf(AccountPhase.Restoring, AccountPhase.Provider, AccountPhase.Exchanging, AccountPhase.Refreshing)
    val signedIn get() = phase == AccountPhase.SignedIn || phase == AccountPhase.Refreshing || displayName != null
    val canSignIn get() = capable && !switchPending && pending == null && !commandUncertain && !signedIn &&
        (phase in setOf(AccountPhase.SignedOut, AccountPhase.Cancelled) ||
            (phase == AccountPhase.Failed && failure.isCredentialRepairFailure()))
    val canDecideCreation get() = capable && !switchPending && pending == null && phase == AccountPhase.AwaitingCreation
    val retryAllowedForFailure get() = failure !in setOf("profile_lifecycle_owner_unproven", "account_repair_identity_mismatch")
    val canRetry get() = capable && !switchPending && retryAllowedForFailure && phase == AccountPhase.Failed && pending !in setOf(AccountIntent.Cancel, AccountIntent.Retry) && (pending == null || retryAtMillis != null)
    val canCancel get() = !switchPending && phase != AccountPhase.AwaitingCreation && !commandUncertain && generation != 0L && pending != AccountIntent.Cancel &&
        (phase in setOf(AccountPhase.Provider, AccountPhase.Exchanging) ||
            pending in setOf(AccountIntent.SignIn, AccountIntent.SignInWithApple, AccountIntent.SignInWithEmail))
    val canChoose get() = capable && !switchPending && !commandUncertain && phase == AccountPhase.SignedIn && pending == null && profilesLoaded
    val relinkFailed get() = enrollments.any { it.state == ProfileEnrollment.Failed }
    fun showRelinkDevices(available: Boolean?) = (signedIn || phase == AccountPhase.Restoring) &&
        (available != false || relinkWaiting || relinkFailed)
    fun relinkLoading(available: Boolean?) = showRelinkDevices(available) && !relinkWaiting &&
        (available == null || working || !profilesLoaded)
    fun canRelinkDevices(available: Boolean?) = available == true && canChoose &&
        profiles.isNotEmpty() && !relinkWaiting
    // Command admission also checks the durable device signal at the UI
    // boundary. This is the account-only portion used after that admission.
    val canRelinkDevices get() = canChoose && profiles.isNotEmpty() && !relinkWaiting
    // The selected enrollment targets are durable account state. A screen or
    // process recreation must not offer a second command while they retry.
    val relinkWaiting get() = pending == AccountIntent.RelinkDevices || enrollments.any {
        it.state in setOf(ProfileEnrollment.Waiting, ProfileEnrollment.Enrolling) || it.retryAtMillis != null
    }
    val canSwitchAccounts get() = capable && !switchPending
    val canAddAccount get() = capable && (!switchPending || switchRetryAvailable)
}

/** Presentation only. Rust owns retries, authorization and durable enrollment.
 * Application lifetime retains observations across recreation, never process death.
 * This reads the adapter's existing bounded result ring, not another native cursor. */
internal class AccountPresentation(
    private val command: suspend (Long, AccountIntent, Long, String, String, List<String>) -> AccountCommandReply,
) {
    private val lock = Mutex()
    private val mutableState = MutableStateFlow(AccountScreenState())
    val state = mutableState.asStateFlow()
    private val publicationLock = Any()
    private var queuedRelinkScope: Long? = null
    private var published = AccountScreenState()
    private var current = AccountScreenState()
    private var after = 0L
    private var pendingGeneration: Long? = null
    private var loadAttempted = false
    private var commandFailure: String? = null
    private var gapPending = false
    private var awaitingAdmission = false
    private data class Scoped(
        val state: AccountScreenState,
        val after: Long,
        val pendingGeneration: Long?,
        val loadAttempted: Boolean,
        val commandFailure: String?,
        val gapPending: Boolean,
        val awaitingAdmission: Boolean,
    )
    private val held = mutableMapOf<Long, Scoped>()
    private var transitionOrigin: Long? = null
    private var transitionScope: Long? = null
    private var transitionGeneration: Long? = null
    private var transitionAfter = 0L
    private var observedSequence = 0L
    private var transitionRetry = false

    /** Reserve the existing bounded handoff before any poll/JNI suspension.
     * This is only pending command feedback; restored enrollment owns the wait. */
    fun queueRelink(ownerScope: Long): Boolean = synchronized(publicationLock) {
        val visible = mutableState.value
        if (queuedRelinkScope != null || visible.activeScope != ownerScope || !visible.canRelinkDevices) {
            return@synchronized false
        }
        queuedRelinkScope = ownerScope
        mutableState.value = visible.copy(pending = AccountIntent.RelinkDevices)
        true
    }

    fun releaseQueuedRelink(ownerScope: Long) = synchronized(publicationLock) {
        if (queuedRelinkScope == ownerScope) queuedRelinkScope = null
        publishVisible()
    }

    private fun publish() = synchronized(publicationLock) {
        published = current
        publishVisible()
    }

    private fun publishVisible() {
        mutableState.value = if (queuedRelinkScope == published.activeScope) {
            published.copy(pending = AccountIntent.RelinkDevices)
        } else published
    }

    private fun activate(scope: Long) {
        val previous = current
        if (previous.activeScope == scope) return
        held[previous.activeScope] = Scoped(previous, after, pendingGeneration, loadAttempted, commandFailure, gapPending, awaitingAdmission)
        val restored = held.remove(scope)
        current = restored?.state ?: AccountScreenState(activeScope = scope)
        after = restored?.after ?: 0
        pendingGeneration = restored?.pendingGeneration
        loadAttempted = restored?.loadAttempted ?: false
        commandFailure = restored?.commandFailure
        gapPending = restored?.gapPending ?: false
        awaitingAdmission = restored?.awaitingAdmission ?: false
    }

    suspend fun pollFailed() = lock.withLock {
        current = current.copy(failure = "account_adapter_unavailable")
        publish()
    }

    /** Profiles are account-server state, so a process that survived in the
     * background must not keep presenting its old list after returning. Keep
     * the rows visible while the next observation refreshes their authority. */
    suspend fun foregrounded() = lock.withLock {
        current = current.copy(profilesLoaded = false)
        loadAttempted = false
        held.replaceAll { _, scoped ->
            scoped.copy(state = scoped.state.copy(profilesLoaded = false), loadAttempted = false)
        }
        publish()
    }

    suspend fun observe(status: AndroidAccountStatus, results: List<MobileAccountResult>) = lock.withLock {
        try {
        val displayScope = status.activeScope
        observedSequence = maxOf(observedSequence, results.maxOfOrNull { it.sequence } ?: 0)
        var switchFailure: String? = null
        if (transitionOrigin != null) {
            val terminal = results.firstOrNull { result -> result.sequence > transitionAfter &&
                (transitionGeneration?.let { result.generation == it && result.ownerScope == transitionScope }
                    ?: (result.kind == "account_selected" && result.hasSelectedScope())) }
            val owner = status.heldAccounts.firstOrNull { it.scopeId == transitionScope }
            val settled = transitionGeneration?.let { owner != null && it !in owner.activeGenerations } == true
            val allIdle = status.requesterActive == false && status.heldAccounts.all { it.requesterActive == false }
            if (terminal != null || settled || (transitionGeneration == null && (allIdle || owner?.requesterActive == false))) {
                switchFailure = terminal?.errorCode?.takeIf { terminal.hasErrorCode() }
                    ?: if (terminal == null && displayScope == transitionOrigin) "account_command_unavailable" else null
                transitionOrigin = null
                transitionScope = null
                transitionGeneration = null
            }
        }
        // Settle parked acknowledgements and cache their targeted projections,
        // without issuing a follow-up against the currently visible account.
        val anonymous = (held.keys + current.activeScope).filter { scope ->
            scope != displayScope && status.heldAccounts.none { it.scopeId == scope } &&
                results.any { it.ownerScope == scope }
        }
        for (scope in anonymous) {
            activate(scope)
            val scopedResults = results.filter { it.ownerScope == scope }
            observeScope(status.copy(activeScope = scope, activeAccountId = current.activeAccountId,
                worker = AccountDiagnostics(stage = current.phase.wireStage()), requesterActive = null,
                generation = maxOf(current.generation, scopedResults.maxOfOrNull { it.generation } ?: 0)), scopedResults, false)
        }
        for (row in status.heldAccounts.filter { it.scopeId != displayScope }) {
            val scopedResults = results.filter { it.ownerScope == row.scopeId }
            if (row.scopeId !in held && current.activeScope != row.scopeId && scopedResults.isEmpty()) continue
            activate(row.scopeId)
            val previous = current
            observeScope(status.copy(
                activeScope = row.scopeId, activeAccountId = row.accountId,
                generation = maxOf(previous.generation, row.activeGenerations.maxOrNull() ?: 0, scopedResults.maxOfOrNull { it.generation } ?: 0),
                worker = AccountDiagnostics(stage = row.stage, failure = row.failureCode),
                requesterActive = row.requesterActive,
            ), scopedResults, false)
        }
        activate(displayScope)
        if (transitionOrigin != null &&
            ((transitionScope != null && transitionScope == displayScope) ||
                (transitionScope == null && displayScope != transitionOrigin))) {
            transitionOrigin = null
            transitionScope = null
            transitionGeneration = null
        }
        if (switchFailure != null) commandFailure = switchFailure
        observeScope(status, results.filter { it.ownerScope == displayScope }, true)
        val living = status.heldAccounts.map { it.scopeId }.toSet() + displayScope
        // The one anonymous attempt remains available through AddAccount.
        held.entries.removeAll { (scope, value) -> scope !in living && value.state.activeAccountId != null && value.state.pending == null }
        } finally { publish() }
    }

    private suspend fun observeScope(status: AndroidAccountStatus, results: List<MobileAccountResult>, allowFollowUp: Boolean) {
        val previous = current
        val worker = status.worker
        val phase = when (worker.stage) {
            "signed_out" -> AccountPhase.SignedOut
            "restoring" -> AccountPhase.Restoring
            "waiting_for_provider" -> AccountPhase.Provider
            "awaiting_account_creation" -> AccountPhase.AwaitingCreation
            "exchanging" -> AccountPhase.Exchanging
            "signed_in" -> AccountPhase.SignedIn
            "refreshing" -> AccountPhase.Refreshing
            "cancelled" -> AccountPhase.Cancelled
            "failed" -> AccountPhase.Failed
            else -> AccountPhase.Unavailable
        }
        var next = previous.copy(phase = phase, capable = status.capability,
            activeScope = status.activeScope, activeAccountId = status.activeAccountId,
            heldAccounts = status.heldAccounts, switchPending = transitionOrigin != null,
            switchRetryAvailable = transitionOrigin != null && transitionRetry,
            generation = status.generation, retryAtMillis = worker.retryAtMillis,
            enrollment = when (worker.enrollment) {
                "waiting" -> ProfileEnrollment.Waiting
                "enrolling" -> ProfileEnrollment.Enrolling
                "complete" -> ProfileEnrollment.Complete
                "failed" -> ProfileEnrollment.Failed
                else -> ProfileEnrollment.Idle
            }, enrollmentProfile = worker.profileId?.let(::ProfileId),
            enrollmentFailure = worker.enrollmentFailure,
            enrollmentRetryAtMillis = worker.enrollmentRetryAtMillis,
            enrollments = if (allowFollowUp) worker.enrollments.map(ProfileEnrollmentProgress::from) else previous.enrollments)
        if (status.generation != previous.generation || status.resultsLost) {
            next = next.copy(profiles = emptyList(), profilesLoaded = false)
            loadAttempted = false
        }
        if (phase in setOf(AccountPhase.SignedOut, AccountPhase.Cancelled, AccountPhase.Unavailable, AccountPhase.Restoring, AccountPhase.Provider, AccountPhase.AwaitingCreation, AccountPhase.Exchanging) ||
            (phase == AccountPhase.Failed && worker.failure.isCredentialRepairFailure())) {
            next = next.copy(displayName = null, profiles = emptyList(), profilesLoaded = false)
        }
        if (awaitingAdmission && status.requesterActive == true && status.generation > previous.generation) {
            pendingGeneration = status.generation
            awaitingAdmission = false
        }
        val floor = maxOf(status.generation, pendingGeneration ?: 0)
        if (status.resultsLost) {
            gapPending = true
            after = maxOf(after, results.maxOfOrNull { it.sequence } ?: 0)
        }
        results.filter { it.sequence > after }.forEach { result ->
            after = maxOf(after, result.sequence)
            if (result.generation < floor && result.generation != pendingGeneration) return@forEach
            if (awaitingAdmission && result.generation > previous.generation) {
                pendingGeneration = result.generation
                awaitingAdmission = false
            }
            if (result.generation == pendingGeneration) {
                pendingGeneration = null
                next = next.copy(pending = null)
                commandFailure = result.errorCode.takeIf { result.hasErrorCode() }
            }
            if (result.kind == "profiles" && result.hasAccount() && phase == AccountPhase.SignedIn) {
                next = next.copy(displayName = result.account.displayName, profilesLoaded = true,
                    profiles = result.profilesList.filter { runCatching { MobileAccountContract.requireProfileId(it.profileId) }.isSuccess }
                        .distinctBy { it.profileId }.map { AuthorizedProfile(ProfileId(it.profileId), it.name, it.availableLocally, it.availableRemotely) })
                loadAttempted = true
            }
            if (result.kind == "profiles_selected" && (!allowFollowUp || result.generation > status.generation)) {
                next = next.copy(enrollments = result.enrollmentsList.map {
                    ProfileEnrollmentProgress.from(ProfileEnrollmentDiagnostics.from(it))
                })
            }
        }
        // Some terminal paths finish without Result. Only shared ownership can
        // settle a missing acknowledgement; queue depth/stage are not liveness.
        val activeGenerations = status.heldAccounts.firstOrNull { it.scopeId == status.activeScope }?.activeGenerations
        val generationSettled = pendingGeneration != null && activeGenerations != null && pendingGeneration !in activeGenerations
        if (generationSettled || (status.requesterActive == false && pendingGeneration != null && status.generation >= pendingGeneration!!)) {
            pendingGeneration = null
            next = next.copy(pending = null)
        }
        if (awaitingAdmission && status.requesterActive == false) {
            awaitingAdmission = false
            next = next.copy(pending = null)
        }
        // A ring gap does not assert success. Once the shared worker is idle,
        // release the missing UI acknowledgement and request fresh authority.
        if (gapPending && status.requesterActive == false) {
            gapPending = false
            pendingGeneration = null
            next = next.copy(pending = null)
            commandFailure = "account_result_gap"
        }
        next = next.copy(failure = status.adapterFailure ?: commandFailure ?: worker.failure, commandUncertain = awaitingAdmission)
        current = next
        if (allowFollowUp && phase == AccountPhase.SignedIn && next.pending == null && !next.profilesLoaded && !loadAttempted) {
            loadAttempted = true
            submit(AccountIntent.RefreshProfiles, "", "")
        }
    }

    suspend fun act(intent: AccountIntent, locale: String = "", profileId: ProfileId? = null, accountId: String? = null, expectedOwnerScope: Long? = null, profileIds: List<ProfileId> = emptyList()) = lock.withLock {
        try {
        val snapshot = current
        if (intent !in setOf(AccountIntent.AddAccount, AccountIntent.SwitchAccount) && expectedOwnerScope != null && expectedOwnerScope != snapshot.activeScope) {
            held[expectedOwnerScope]?.let { held[expectedOwnerScope] = it.copy(commandFailure = "account_scope_changed", state = it.state.copy(failure = "account_scope_changed")) }
            return@withLock
        }
        if (intent in setOf(AccountIntent.SwitchAccount, AccountIntent.AddAccount)) {
            if (!(if(intent==AccountIntent.AddAccount) snapshot.canAddAccount else snapshot.canSwitchAccounts) ||
                (intent == AccountIntent.SwitchAccount && snapshot.heldAccounts.none { it.accountId == accountId && it.scopeId != snapshot.activeScope })) return@withLock
            transitionOrigin = snapshot.activeScope
            transitionScope = snapshot.heldAccounts.firstOrNull { it.accountId == accountId }?.scopeId
            transitionGeneration = null
            transitionAfter = observedSequence
            transitionRetry = false
            current = snapshot.copy(switchPending = true)
            try {
                val reply = send(snapshot.activeScope, intent, 0, locale, accountId.orEmpty())
                if (reply.hasErrorCode()) {
                    transitionOrigin = null
                    current = snapshot.copy(failure = reply.errorCode, switchPending = false)
                } else {
                    transitionScope = reply.ownerScope
                    transitionGeneration = reply.generation
                }
            } catch (cancelled: CancellationException) { throw cancelled }
            catch (_: Exception) {
                // Shared AddAccount resumes its retained anonymous intent; a
                // lost reply does not replace A's own pending acknowledgement.
                transitionRetry = intent == AccountIntent.AddAccount
                current = snapshot.copy(failure = "account_command_unavailable", switchPending = true, switchRetryAvailable = transitionRetry)
            }
            return@withLock
        }
        if (snapshot.switchPending) return@withLock
        val allowed = when (intent) {
            AccountIntent.AddAccount, AccountIntent.SwitchAccount -> false
            // Email has no Android journey yet: the host refuses START_EMAIL,
            // so the screen does not offer it and a stray intent is not sent.
            AccountIntent.SignIn, AccountIntent.SignInWithApple -> snapshot.canSignIn
            AccountIntent.SignInWithEmail -> false
            AccountIntent.ConfirmCreation, AccountIntent.CancelCreation -> snapshot.canDecideCreation
            AccountIntent.Cancel -> snapshot.canCancel
            AccountIntent.SelectProfiles -> snapshot.canChoose && profileId == null &&
                profileIds.distinct().size == profileIds.size && profileIds.all { id -> snapshot.profiles.any { it.id == id } }
            AccountIntent.RelinkDevices -> snapshot.canRelinkDevices && profileId == null &&
                profileIds.isNotEmpty() && profileIds.distinct().size == profileIds.size &&
                profileIds.all { id -> snapshot.profiles.any { it.id == id } }
            AccountIntent.RefreshProfiles -> snapshot.signedIn && snapshot.pending == null
            AccountIntent.Logout -> snapshot.signedIn && snapshot.pending == null
            AccountIntent.Retry -> snapshot.canRetry
            AccountIntent.RetryEnrollment -> snapshot.signedIn && snapshot.pending == null &&
                snapshot.enrollments.any { (profileId == null || it.id == profileId) && it.state != ProfileEnrollment.Complete }
            AccountIntent.CancelEnrollment -> snapshot.signedIn && snapshot.pending == null &&
                snapshot.enrollments.any { it.state != ProfileEnrollment.Complete }
        }
        if (allowed) submit(intent, locale, profileId?.value.orEmpty(), profileIds.map { it.value }.sorted())
        } finally { publish() }
    }

    private suspend fun submit(intent: AccountIntent, locale: String, profileId: String, profileIds: List<String> = emptyList()) {
        val snapshot = current
        commandFailure = null
        if (intent == AccountIntent.RelinkDevices) {
            current = snapshot.copy(pending = intent)
            publish()
        }
        try {
            val reply = send(snapshot.activeScope, intent, if (intent == AccountIntent.Cancel) pendingGeneration ?: snapshot.generation else 0, locale, profileId, profileIds)
            if (reply.hasErrorCode()) {
                commandFailure = reply.errorCode
                current = snapshot.copy(failure = commandFailure)
            } else {
                pendingGeneration = reply.generation
                current = snapshot.copy(
                    pending = intent,
                    failure = null,
                    commandUncertain = false,
                )
            }
        } catch (cancelled: CancellationException) {
            throw cancelled
        } catch (_: Exception) {
            // JNI acknowledgement may be lost; do not automatically repeat a
            // command that could already have reached the shared worker.
            commandFailure = "account_command_unavailable"
            awaitingAdmission = true
            current = snapshot.copy(
                pending = intent,
                failure = commandFailure,
                commandUncertain = true,
            )
        }
    }

    private suspend fun send(owner: Long, intent: AccountIntent, generation: Long, locale: String, target: String, profiles: List<String> = emptyList()) =
        command(owner, intent, generation, locale, target, profiles)
}

internal data class AccountObservation(val status: AndroidAccountStatus, val results: List<MobileAccountResult>)

/** Serializes acquisition AND projection with admission, so an observation
 * captured before a lost command ACK cannot settle that later command. */
internal class AccountPresentationCycle(
    private val presentation: AccountPresentation,
    private val poll: suspend () -> AccountObservation?,
) {
    private val lock = Mutex()
    suspend fun tick() = lock.withLock {
        val observation = poll()
        if (observation == null) presentation.pollFailed()
        else presentation.observe(observation.status, observation.results)
    }
    suspend fun foregrounded() = lock.withLock {
        presentation.foregrounded()
    }
    suspend fun act(intent: AccountIntent, locale: String, profile: ProfileId?, accountId: String? = null, expectedOwnerScope: Long? = null, profiles: List<ProfileId> = emptyList()) = lock.withLock {
        presentation.act(intent, locale, profile, accountId, expectedOwnerScope, profiles)
    }
}

private fun AccountPhase.wireStage(): String = when (this) {
    AccountPhase.SignedOut -> "signed_out"
    AccountPhase.Restoring -> "restoring"
    AccountPhase.Provider -> "waiting_for_provider"
    AccountPhase.AwaitingCreation -> "awaiting_account_creation"
    AccountPhase.Exchanging -> "exchanging"
    AccountPhase.SignedIn -> "signed_in"
    AccountPhase.Refreshing -> "refreshing"
    AccountPhase.Cancelled -> "cancelled"
    AccountPhase.Failed -> "failed"
    AccountPhase.Unavailable -> "unavailable"
}

internal fun AccountIntent.wire(): AccountAction = when (this) {
    AccountIntent.AddAccount -> AccountAction.ACCOUNT_ACTION_ADD_ACCOUNT
    AccountIntent.SwitchAccount -> AccountAction.ACCOUNT_ACTION_SWITCH_ACCOUNT
    AccountIntent.ConfirmCreation -> AccountAction.ACCOUNT_ACTION_CONFIRM_ACCOUNT_CREATION
    AccountIntent.CancelCreation -> AccountAction.ACCOUNT_ACTION_CANCEL_ACCOUNT_CREATION
    AccountIntent.SignIn -> AccountAction.ACCOUNT_ACTION_START_GOOGLE
    // Apple has no Android sheet: the host asks for a browser page and takes
    // back the verified App Link (BrowserSignInPrompt / BrowserSignInReply).
    AccountIntent.SignInWithApple -> AccountAction.ACCOUNT_ACTION_START_APPLE
    // Retained for the wire mapping. The screen does not offer it, and the
    // mobile host refuses the action until the email journey has its fields.
    AccountIntent.SignInWithEmail -> AccountAction.ACCOUNT_ACTION_START_EMAIL
    AccountIntent.Cancel -> AccountAction.ACCOUNT_ACTION_CANCEL
    AccountIntent.Retry -> AccountAction.ACCOUNT_ACTION_RETRY
    AccountIntent.Logout -> AccountAction.ACCOUNT_ACTION_LOGOUT
    AccountIntent.RefreshProfiles -> AccountAction.ACCOUNT_ACTION_LIST_PROFILES
    AccountIntent.SelectProfiles -> AccountAction.ACCOUNT_ACTION_SELECT_PROFILES
    // Relinking deliberately reuses explicit Profile enrollment. Rust re-arms
    // completed targets and reconciles only currently effective trust, so a
    // revoked relationship cannot be resurrected by this presentation alias.
    AccountIntent.RelinkDevices -> AccountAction.ACCOUNT_ACTION_SELECT_PROFILES
    AccountIntent.RetryEnrollment -> AccountAction.ACCOUNT_ACTION_RETRY_ENROLLMENT
    AccountIntent.CancelEnrollment -> AccountAction.ACCOUNT_ACTION_CANCEL_ENROLLMENT
}
