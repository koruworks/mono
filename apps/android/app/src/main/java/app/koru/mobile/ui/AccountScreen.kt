// SPDX-License-Identifier: MPL-2.0

package app.koru.mobile.ui

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.selectableGroup
import androidx.compose.foundation.selection.toggleable
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.saveable.mapSaver
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.luminance
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.*
import app.koru.mobile.R
import app.koru.mobile.domain.Profile
import app.koru.mobile.domain.ProfileId
import app.koru.mobile.domain.boundAccountId
import app.koru.mobile.services.*
import app.koru.mobile.ui.theme.KoruSpacing

private val ProfileDraftSaver = mapSaver<Map<String, ProfileSelectionDraft>>(
    save = { drafts -> drafts.mapValues { (_, draft) -> arrayListOf(draft.mode.name).apply {
        addAll(draft.ids.map { it.value }.sorted())
    } } },
    restore = { saved -> saved.mapValues { (_, value) ->
        val fields = (value as? List<*>)?.filterIsInstance<String>().orEmpty()
        ProfileSelectionDraft(runCatching { ProfileSelectionMode.valueOf(fields.first()) }.getOrDefault(ProfileSelectionMode.One),
            fields.drop(1).map(::ProfileId).toSet())
    } },
)

/** A projection of the application-owned worker, with no effect or retry timer.
 * Only unsubmitted, account-scoped choices survive Activity recreation. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun AccountScreen(
    state: AccountScreenState,
    localProfiles: List<Profile>,
    onAction: (AccountIntent, ProfileId?) -> Unit,
    onOffline: () -> Unit,
    onBack: () -> Unit,
    onOpenLocal: (ProfileId) -> Unit,
    onSwitchAccount: (String) -> Unit = {},
    hiddenProfileCount: Int = 0,
    onSelectProfiles: (List<ProfileId>) -> Unit = {},
) {
    var drafts by rememberSaveable(stateSaver = ProfileDraftSaver) { mutableStateOf(emptyMap<String, ProfileSelectionDraft>()) }
    val scope = "${state.activeScope}:${state.activeAccountId.orEmpty()}"
    val authorized = state.profiles.map { it.id }.toSet()
    val retained = drafts[scope] ?: ProfileSelectionDraft()
    val draft = if (state.profilesLoaded) retained.reconcile(authorized) else retained
    SideEffect {
        val living = state.heldAccounts.map { "${it.scopeId}:${it.accountId}" }.toSet() + scope
        val next = drafts.filterKeys { it in living } + (scope to draft)
        if (next != drafts) drafts = next
    }
    val progressLabel = stringResource(when (state.phase) {
        AccountPhase.Restoring -> R.string.account_wait_restore
        AccountPhase.Provider -> if (state.pending == AccountIntent.SignInWithApple) R.string.account_wait_provider_apple else R.string.account_wait_provider
        AccountPhase.Exchanging -> R.string.account_wait_exchange
        AccountPhase.Refreshing -> R.string.account_wait_refresh
        else -> if (state.pending == AccountIntent.RefreshProfiles) R.string.account_loading_profiles else R.string.account_operation
    })
    Scaffold(topBar = { TopAppBar(
        title = { Text(stringResource(R.string.account_title)) },
        navigationIcon = { TextButton(onClick = onBack) { Text(stringResource(R.string.back)) } },
    ) }) { padding ->
        LazyColumn(Modifier.fillMaxSize().padding(padding).selectableGroup(),
            contentPadding = PaddingValues(KoruSpacing.md),
            verticalArrangement = Arrangement.spacedBy(KoruSpacing.md)) {
            item {
                Text(stringResource(R.string.account_intro))
                TextButton(onClick = onOffline) { Text(stringResource(R.string.account_offline)) }
            }
            item { Text(stringResource(R.string.account_switcher_title), style = MaterialTheme.typography.titleLarge) }
            items(state.heldAccounts, key = { "account:${it.scopeId}" }) { row ->
                OutlinedButton(
                    onClick = { onSwitchAccount(row.accountId) },
                    enabled = state.canSwitchAccounts && row.scopeId != state.activeScope,
                    modifier = Modifier.fillMaxWidth(),
                ) {
                    Column(Modifier.fillMaxWidth()) {
                        Text(stringResource(R.string.account_switcher_label, row.accountId.takeLast(8)))
                        Text(stringResource(when {
                            row.scopeId == state.activeScope -> R.string.account_switcher_active
                            row.activeGenerations.isNotEmpty() -> R.string.account_switcher_pending
                            !row.credentialReady -> R.string.account_switcher_repair
                            else -> R.string.account_switcher_title
                        }), style = MaterialTheme.typography.bodySmall)
                    }
                }
            }
            item {
                TextButton(enabled = state.canAddAccount, onClick = { onAction(AccountIntent.AddAccount, null) }) {
                    Text(stringResource(R.string.account_switcher_add))
                }
                if (hiddenProfileCount > 0) Text(stringResource(R.string.account_switcher_hidden, hiddenProfileCount))
            }
            if (!state.capable) item { Text(stringResource(R.string.account_unavailable)) }
            if (state.displayName != null) item {
                Text(state.displayName.ifBlank { stringResource(R.string.account_unnamed) }, style = MaterialTheme.typography.headlineSmall)
            }
            if (state.canSignIn) item {
                Text(stringResource(R.string.account_sign_in_again))
                Button(onClick = { onAction(AccountIntent.SignIn, null) }) { Text(stringResource(R.string.account_google)) }
                SignInWithAppleButton(onClick = { onAction(AccountIntent.SignInWithApple, null) })
            }
            if (state.phase == AccountPhase.AwaitingCreation) item {
                Column(verticalArrangement = Arrangement.spacedBy(KoruSpacing.sm),
                    modifier = Modifier.semantics { liveRegion = LiveRegionMode.Polite }) {
                    Text(stringResource(R.string.account_creation_title), style = MaterialTheme.typography.titleLarge)
                    Text(stringResource(R.string.account_creation_body))
                    Button(enabled = state.canDecideCreation,
                        onClick = { onAction(AccountIntent.ConfirmCreation, null) }) {
                        Text(stringResource(R.string.account_creation_confirm))
                    }
                    TextButton(enabled = state.canDecideCreation,
                        onClick = { onAction(AccountIntent.CancelCreation, null) }) {
                        Text(stringResource(R.string.account_creation_cancel))
                    }
                }
            }
            if (state.working || state.pending != null) item {
                Column(Modifier.semantics { liveRegion = LiveRegionMode.Polite }) {
                    Text(if (state.pending == AccountIntent.Cancel) stringResource(R.string.account_cancel_pending) else progressLabel)
                    LinearProgressIndicator(Modifier.fillMaxWidth().semantics { contentDescription = progressLabel })
                }
            }
            if (state.failure != null || state.phase == AccountPhase.Failed) item {
                Text(stringResource(when (state.failure) {
                    "provider_session_clear_failed" -> R.string.account_provider_clear_failed
                    "account_confirmation_expired" -> R.string.account_creation_expired_body
                    "profile_lifecycle_owner_unproven" -> R.string.account_removal_owner_unproven
                    "account_repair_identity_mismatch" -> R.string.account_repair_identity_mismatch
                    else -> R.string.account_failed
                }),
                    color = MaterialTheme.colorScheme.error, modifier = Modifier.semantics { liveRegion = LiveRegionMode.Polite })
                if (state.phase == AccountPhase.Failed && state.retryAllowedForFailure) TextButton(
                    enabled = state.canRetry,
                    onClick = { onAction(AccountIntent.Retry, null) },
                ) { Text(stringResource(R.string.account_retry)) }
            }
            if (state.retryAtMillis != null) item { Text(stringResource(R.string.account_retry_scheduled)) }
            if (state.canCancel) item {
                TextButton(onClick = { onAction(AccountIntent.Cancel, null) }) { Text(stringResource(R.string.account_cancel)) }
            }
            if (state.signedIn) {
                item {
                    Text(stringResource(R.string.account_choose), style = MaterialTheme.typography.titleLarge)
                    TextButton(enabled = state.pending == null && !state.switchPending, onClick = { onAction(AccountIntent.RefreshProfiles, null) }) {
                        Text(stringResource(R.string.account_refresh_profiles))
                    }
                }
                if (state.profilesLoaded && state.profiles.isEmpty()) item { Text(stringResource(R.string.account_empty)) }
                if (state.profilesLoaded && state.profiles.isNotEmpty()) item {
                    Column(Modifier.selectableGroup()) {
                        ProfileSelectionMode.entries.forEach { mode ->
                            Row(Modifier.fillMaxWidth().selectable(selected = draft.mode == mode,
                                enabled = state.canChoose, role = Role.RadioButton,
                                onClick = { drafts = drafts + (scope to draft.choose(mode, authorized)) })
                                .padding(vertical = KoruSpacing.sm), verticalAlignment = Alignment.CenterVertically) {
                                RadioButton(selected = draft.mode == mode, onClick = null, enabled = state.canChoose)
                                Text(stringResource(when (mode) {
                                    ProfileSelectionMode.One -> R.string.account_choose_one
                                    ProfileSelectionMode.Several -> R.string.account_choose_several
                                    ProfileSelectionMode.All -> R.string.account_choose_all
                                }), Modifier.padding(start = KoruSpacing.sm))
                            }
                        }
                    }
                    Text(stringResource(R.string.account_all_snapshot), style = MaterialTheme.typography.bodySmall)
                    Text(stringResource(R.string.account_selection_count, draft.ids.size),
                        modifier = Modifier.semantics { liveRegion = LiveRegionMode.Polite })
                }
                items(state.profiles, key = { it.id.value }) { profile ->
                            val choose = { drafts = drafts + (scope to draft.toggle(profile.id, authorized)) }
                            val selection = if (draft.mode == ProfileSelectionMode.One)
                                Modifier.selectable(selected = profile.id in draft.ids, enabled = state.canChoose, role = Role.RadioButton, onClick = choose)
                            else Modifier.toggleable(value = profile.id in draft.ids, enabled = state.canChoose, role = Role.Checkbox, onValueChange = { choose() })
                            Row(Modifier.fillMaxWidth().then(selection).padding(vertical = KoruSpacing.sm),
                                verticalAlignment = Alignment.CenterVertically) {
                                if (draft.mode == ProfileSelectionMode.One) RadioButton(selected = profile.id in draft.ids, onClick = null, enabled = state.canChoose)
                                else Checkbox(checked = profile.id in draft.ids, onCheckedChange = null, enabled = state.canChoose)
                                Column(Modifier.padding(start = KoruSpacing.sm)) {
                                    Text(profile.name.ifBlank { stringResource(R.string.account_profile_unnamed) })
                                    Text(stringResource(if (profile.local) R.string.account_local_yes else R.string.account_local_no), style = MaterialTheme.typography.bodySmall)
                                    Text(stringResource(if (profile.remote) R.string.account_remote_yes else R.string.account_remote_no), style = MaterialTheme.typography.bodySmall)
                                }
                            }
                }
                if (state.profilesLoaded && state.profiles.isNotEmpty()) item {
                    Button(enabled = state.canChoose && draft.ids.isNotEmpty(),
                        onClick = { onSelectProfiles(draft.submission(authorized)) }) {
                        Text(stringResource(R.string.account_apply_selection))
                    }
                }
            }
            items(state.enrollments, key = { "enrollment-${it.id.value}" }) { enrollment ->
                    val profileId = enrollment.id
                    val throttled = enrollment.failure == "enrollment_rate_limited"
                    val enrollmentLabel = stringResource(if (!state.signedIn) R.string.account_enrollment_account else if (throttled) R.string.account_enrollment_throttled else when (enrollment.state) {
                        ProfileEnrollment.Waiting -> R.string.account_enrollment_wait
                        ProfileEnrollment.Enrolling -> R.string.account_enrollment_work
                        ProfileEnrollment.Complete -> R.string.account_enrollment_complete
                        ProfileEnrollment.Failed -> R.string.account_enrollment_failed
                        ProfileEnrollment.Idle -> R.string.account_enrollment_wait
                    })
                    Column(Modifier.semantics { liveRegion = LiveRegionMode.Polite }, verticalArrangement = Arrangement.spacedBy(KoruSpacing.sm)) {
                        Text(state.profiles.firstOrNull { it.id == profileId }?.name?.takeIf { it.isNotBlank() }
                            ?: stringResource(R.string.account_profile_unnamed), style = MaterialTheme.typography.titleMedium)
                        Text(enrollmentLabel)
                        Text(stringResource(if (localProfiles.any { it.id == profileId }) R.string.account_local_yes else R.string.account_local_no),
                            style = MaterialTheme.typography.bodySmall)
                        if (state.signedIn && !throttled && enrollment.state in setOf(ProfileEnrollment.Waiting, ProfileEnrollment.Enrolling)) {
                            LinearProgressIndicator(Modifier.fillMaxWidth().semantics { contentDescription = enrollmentLabel })
                        }
                        if (enrollment.retryAtMillis != null) Text(stringResource(R.string.account_retry_scheduled))
                        if (enrollment.state in setOf(ProfileEnrollment.Waiting, ProfileEnrollment.Failed)) {
                            TextButton(enabled = state.signedIn && state.pending == null && !throttled, onClick = { onAction(AccountIntent.RetryEnrollment, profileId) }) {
                                Text(stringResource(R.string.account_retry_enrollment))
                            }
                        }
                        // Enrollment receipt is trust, not proof content has
                        // arrived. Navigation uses only observed Store rows.
                        if (localProfiles.any { it.id == profileId }) {
                            Button(onClick = { onOpenLocal(profileId) }) { Text(stringResource(R.string.account_open_profile)) }
                        } else if (enrollment.state == ProfileEnrollment.Complete) Text(stringResource(R.string.account_local_pending))
                    }
            }
            if (state.enrollments.any { it.state != ProfileEnrollment.Complete }) item {
                TextButton(enabled = state.signedIn && state.pending == null, onClick = { onAction(AccountIntent.CancelEnrollment, null) }) {
                    Text(stringResource(R.string.account_cancel_enrollment))
                }
            }
            if (state.signedIn) item {
                HorizontalDivider()
                Text(stringResource(R.string.account_logout_note))
                TextButton(enabled = state.pending == null && !state.switchPending, onClick = { onAction(AccountIntent.Logout, null) }) { Text(stringResource(R.string.account_logout)) }
            }
            if (localProfiles.isNotEmpty()) item { Text(stringResource(R.string.account_local_yes), style = MaterialTheme.typography.titleLarge) }
            items(localProfiles, key = { "local:${it.id.value}" }) { profile ->
                TextButton(onClick = { onOpenLocal(profile.id) }, modifier = Modifier.fillMaxWidth()) {
                    Column(Modifier.fillMaxWidth()) {
                        Text(profile.displayLabel)
                        Text(localProfileAccountLabel(profile), style = MaterialTheme.typography.bodySmall)
                    }
                }
            }
        }
    }
}

/** Apple's Human Interface Guidelines button: black with a white mark and the
 * title "Sign in with Apple", or white with a black mark on a dark screen. The
 * same Material shape and padding as the Google button beside it. */
@Composable
private fun SignInWithAppleButton(onClick: () -> Unit) {
    val dark = MaterialTheme.colorScheme.background.luminance() < 0.5f
    val container = if (dark) Color.White else Color.Black
    val content = if (dark) Color.Black else Color.White
    Button(onClick = onClick, colors = ButtonDefaults.buttonColors(containerColor = container, contentColor = content)) {
        Icon(painterResource(R.drawable.ic_apple_logo), contentDescription = null,
            modifier = Modifier.size(ButtonDefaults.IconSize))
        Spacer(Modifier.size(ButtonDefaults.IconSpacing))
        Text(stringResource(R.string.account_apple))
    }
}

@Composable
internal fun localProfileAccountLabel(profile: Profile): String = when {
    profile.accountBinding.isEmpty() -> stringResource(R.string.account_switcher_local_only)
    profile.boundAccountId() != null -> stringResource(R.string.account_switcher_label, profile.boundAccountId()!!.takeLast(8))
    else -> stringResource(R.string.account_switcher_unknown)
}
