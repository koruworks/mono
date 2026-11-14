// SPDX-License-Identifier: MPL-2.0

package app.koru.mobile.services

import android.app.Activity
import android.content.Context
import app.koru.contract.api.v1.*
import app.koru.mobile.BuildConfig
import kotlinx.coroutines.*
import app.koru.mobile.domain.ProfileId

/** One application-owned adapter; background/rotation never destroys the Rust
 * worker or persists an unfinished sign-in. JNI and vault work run on IO. */
class NativeAndroidAccountService(context: Context, deviceLink: NativeDeviceLinkService) {
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    private val dialog = AndroidGoogleAccountDialog(context)
    private val browser = AndroidBrowserSignInLauncher()
    internal val adapter = AndroidAccountAdapter(object : AccountNativePort {
        override suspend fun command(request: AccountCommandRequest) = deviceLink.accountCommand(request)
        override suspend fun poll(request: AccountPollRequest) = deviceLink.accountPoll(request)
        override suspend fun platform(request: AccountPlatformRequest) = deviceLink.accountPlatform(request)
    }, dialog, AndroidAccountCredentialVault(context), scope, browser)
    val status = adapter.status
    private val presentation = AccountPresentation(
        command = { owner, intent, generation, locale, target, profiles ->
            adapter.command(intent.wire(), generation, locale,
                profileId = target.takeIf { intent == AccountIntent.RetryEnrollment }.orEmpty(),
                accountId = target.takeIf { intent == AccountIntent.SwitchAccount }.orEmpty(),
                expectedOwnerScope = owner,
                profileIds = profiles)
        },
    )
    val screen = presentation.state
    // The Cloud sync switch reads the same poll as the account screen but
    // keeps its own pending set: a change in flight must not stop the sheet's
    // other rows, and the account's single pending intent would do exactly that.
    private val cloudSync = CloudSyncPresentation(
        attached = BuildConfig.ACCOUNT_CREDENTIAL_ROLLOUT.isNotBlank(),
        command = { owner, profile, enabled ->
            adapter.command(
                cloudSyncAction(profile),
                profileId = profile?.value.orEmpty(),
                expectedOwnerScope = owner,
                cloudSyncEnabled = enabled,
            )
        },
    )
    val cloudSyncState = cloudSync.state
    private val cloudStorage = CloudStoragePresentation(scope, android.os.SystemClock::elapsedRealtime,
        command = { owner -> adapter.command(AccountAction.ACCOUNT_ACTION_INSPECT_CLOUD_PLAN, expectedOwnerScope = owner) },
        cancel = { owner, generation -> adapter.command(AccountAction.ACCOUNT_ACTION_CANCEL, generation,
            expectedOwnerScope = owner) })
    val cloudStorageState = cloudStorage.state
    private val cycle = AccountPresentationCycle(presentation) {
        try {
            if (adapter.tick()) AccountObservation(adapter.status.value, adapter.results.value) else null
        } catch (cancelled: CancellationException) { throw cancelled }
        catch (_: Exception) { adapter.failure("account_adapter_unavailable"); null }
    }
    // One application-owned command handoff; taps do not create an unbounded
    // queue of coroutines waiting for JNI. Rust retains its own bounded queue.
    private data class Action(val intent: AccountIntent, val ownerScope: Long, val locale: String, val profile: ProfileId?, val accountId: String?, val profiles: List<ProfileId>)
    private val actions = kotlinx.coroutines.channels.Channel<Action>(1)
    private var activity: Activity? = null

    init {
        scope.launch {
            for ((intent, ownerScope, locale, profile, accountId, profiles) in actions) {
                try { cycle.act(intent, locale, profile, accountId, ownerScope, profiles) }
                finally {
                    if (intent == AccountIntent.RelinkDevices) presentation.releaseQueuedRelink(ownerScope)
                }
            }
        }
        scope.launch {
            val configuration = AndroidAccountConfig(BuildConfig.ACCOUNT_ORIGIN, BuildConfig.ACCOUNT_ISSUER,
                BuildConfig.GOOGLE_SERVER_CLIENT_ID, BuildConfig.ACCOUNT_CHANNEL, BuildConfig.ACCOUNT_CREDENTIAL_ROLLOUT,
                BuildConfig.BLOB_BYTE_ORIGINS)
            if (!configuration.present) {
                adapter.failure("account_configuration_missing")
                presentation.observe(adapter.status.value, adapter.results.value)
                return@launch
            }
            try { deviceLink.configureAccount(configuration) }
            catch (_: Exception) {
                adapter.failure("account_configuration_unavailable")
                presentation.observe(adapter.status.value, adapter.results.value)
                return@launch
            }
            while (isActive) {
                cycle.tick()
                cloudSync.observe(adapter.status.value, adapter.results.value)
                cloudStorage.observe(adapter.status.value, adapter.results.value)
                cloudSync.inspect(adapter.status.value)
                delay(200)
            }
        }
    }

    /** Turn Koru Cloud on or off for one Profile. Runs on the service's own
     *  scope so the sheet stays responsive and the account's command handoff
     *  is left free for what only it can do. */
    fun setCloudSync(profile: ProfileId, enabled: Boolean, ownerScope: Long) {
        scope.launch { cloudSync.set(profile, enabled, ownerScope) }
    }

    fun cloudStorageVisible(visible: Boolean) {
        scope.launch { cloudStorage.visibility(visible, adapter.status.value) }
    }
    fun refreshCloudStorage() {
        scope.launch { cloudStorage.retry(adapter.status.value) }
    }

    fun act(intent: AccountIntent, ownerScope: Long, locale: String = "", profile: ProfileId? = null, accountId: String? = null, profiles: List<ProfileId> = emptyList()) {
        if (intent == AccountIntent.RelinkDevices && !presentation.queueRelink(ownerScope)) return
        val admitted = actions.trySend(Action(intent, ownerScope, locale, profile, accountId, profiles.toList())).isSuccess
        if (!admitted && intent == AccountIntent.RelinkDevices) presentation.releaseQueuedRelink(ownerScope)
    }

    /** The App Link a browser sign-in returned through. Ignored unless a page
     *  this service opened is still waiting for it; never logged. */
    fun browserReturned(url: String) { adapter.browserReturned(url) }

    fun foreground(value: Activity) {
        val returning = activity == null
        activity = value
        dialog.foreground(value)
        browser.foreground(value)
        adapter.foreground(true)
        scope.launch { cloudStorage.foreground(true, adapter.status.value) }
        if (returning) scope.launch { cycle.foregrounded() }
    }

    fun background(value: Activity) {
        dialog.background(value)
        browser.background(value)
        if (activity === value) {
            activity = null
            adapter.foreground(false)
            scope.launch { cloudStorage.foreground(false, adapter.status.value) }
        }
    }
}
