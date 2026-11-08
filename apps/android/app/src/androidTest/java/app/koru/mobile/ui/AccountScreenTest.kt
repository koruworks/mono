// SPDX-License-Identifier: MPL-2.0

package app.koru.mobile.ui

import android.app.Application
import androidx.activity.ComponentActivity
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.mutableStateOf
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.semantics.SemanticsNode
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.StateRestorationTester
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.test.ext.junit.runners.AndroidJUnit4
import app.koru.mobile.R
import app.koru.mobile.domain.Profile
import app.koru.mobile.domain.ProfileId
import app.koru.mobile.services.AccountIntent
import app.koru.mobile.services.AccountPhase
import app.koru.mobile.services.AccountScreenState
import app.koru.mobile.services.AuthorizedProfile
import app.koru.mobile.services.ProfileEnrollment
import app.koru.mobile.services.ProfileEnrollmentProgress
import app.koru.mobile.services.HeldAccount
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Assert.assertNotEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

/** Injected projections only: no application container, JNI, Store, provider or vault. */
@RunWith(AndroidJUnit4::class)
class AccountScreenTest {
    @get:Rule val compose = createAndroidComposeRule<ComponentActivity>()
    private val state = mutableStateOf(AccountScreenState())
    private val local = mutableStateOf<List<Profile>>(emptyList())
    private val actions = mutableListOf<Pair<AccountIntent, ProfileId?>>()
    private val opened = mutableListOf<ProfileId>()
    private val selections = mutableListOf<List<ProfileId>>()
    private var offline = 0
    private var back = 0
    private val first = ProfileId("10000000-0000-4000-8000-000000000001")
    private val second = ProfileId("20000000-0000-4000-8000-000000000002")
    private val third = ProfileId("30000000-0000-4000-8000-000000000003")

    private fun label(id: Int) = compose.activity.getString(id)

    @Test fun creationExplainsSeparateAccountAndOffersExplicitDecisionWithOfflineNavigation() {
        show(AccountScreenState(phase = AccountPhase.AwaitingCreation, capable = true))
        compose.onNodeWithText(label(R.string.account_creation_title)).assertExists()
        compose.onNodeWithText(label(R.string.account_creation_body)).assertExists()
        compose.onNodeWithText(label(R.string.account_creation_confirm)).performScrollTo().performClick()
        assertEquals(AccountIntent.ConfirmCreation, actions.single().first)
        compose.onNodeWithText(label(R.string.account_creation_cancel)).performScrollTo().performClick()
        assertEquals(AccountIntent.CancelCreation, actions.last().first)
        compose.onNodeWithText(label(R.string.account_offline)).performScrollTo().performClick()
        assertEquals(1, offline)
        compose.onNodeWithText(label(R.string.account_google)).assertDoesNotExist()
    }

    @Test fun expiredCreationExplainsPossibleAccountAndOffersSignInAgain() {
        show(AccountScreenState(phase = AccountPhase.SignedOut, capable = true, failure = "account_confirmation_expired"))
        compose.onNodeWithText(label(R.string.account_creation_expired_body)).assertExists()
        compose.onNodeWithText(label(R.string.account_google)).performClick()
        assertEquals(AccountIntent.SignIn, actions.single().first)
    }
    private fun show(initial: AccountScreenState = AccountScreenState()): StateRestorationTester {
        // Also catch an accidentally restored production runner configuration.
        assertEquals(Application::class.java, compose.activity.application.javaClass)
        state.value = initial
        return StateRestorationTester(compose).also { restoration ->
            restoration.setContent {
                MaterialTheme {
                    AccountScreen(state.value, local.value,
                        onAction = { intent, profile -> actions += intent to profile },
                        onOffline = { offline++ }, onBack = { back++ },
                        onOpenLocal = { opened += it },
                        onSelectProfiles = { selections += it })
                }
            }
        }
    }

    private fun text(id: Int): SemanticsNodeInteraction {
        compose.onNode(hasScrollAction()).performScrollToNode(hasText(label(id)))
        return compose.onNodeWithText(label(id))
    }

    private fun radio(name: String): SemanticsNodeInteraction {
        compose.onNode(hasScrollAction()).performScrollToNode(hasText(name))
        return compose.onNode(hasText(name) and SemanticsMatcher.expectValue(SemanticsProperties.Role, Role.RadioButton))
    }

    private fun checkbox(name: String): SemanticsNodeInteraction {
        compose.onNode(hasScrollAction()).performScrollToNode(hasText(name))
        return compose.onNode(hasText(name) and SemanticsMatcher.expectValue(SemanticsProperties.Role, Role.Checkbox))
    }

    private fun nearestSelectionGroup(node: SemanticsNode): Int {
        var parent = node.parent
        while (parent != null) {
            if (parent.config.contains(SemanticsProperties.SelectableGroup)) return parent.id
            parent = parent.parent
        }
        error("selectable row must belong to a group")
    }

    private fun signedIn(vararg profiles: AuthorizedProfile) = AccountScreenState(
        phase = AccountPhase.SignedIn, capable = true, displayName = "Fixture account",
        profilesLoaded = true, profiles = profiles.toList(), generation = 7,
    )

    @Test fun unavailableEntryKeepsOfflineAndBackWithoutAccountEffects() {
        show()
        text(R.string.account_unavailable).assertIsDisplayed()
        compose.onNodeWithText(label(R.string.account_google)).assertDoesNotExist()
        text(R.string.account_offline).performClick()
        compose.onNodeWithText(label(R.string.back)).performClick()
        compose.runOnIdle {
            assertEquals(1, offline)
            assertEquals(1, back)
            assertTrue(actions.isEmpty())
            assertTrue(opened.isEmpty())
        }
    }

    @Test fun signedOutEntryEmitsExplicitSignInAndStillOffersOffline() {
        show(AccountScreenState(phase = AccountPhase.SignedOut, capable = true))
        text(R.string.account_google).performClick()
        text(R.string.account_apple).performClick()
        compose.onNodeWithText(label(R.string.account_email)).assertDoesNotExist()
        text(R.string.account_offline).performClick()
        compose.runOnIdle {
            assertEquals(
                listOf(AccountIntent.SignIn to null, AccountIntent.SignInWithApple to null),
                actions,
            )
            assertEquals(1, offline)
        }
    }

    @Test fun zeroAuthorizedProfilesShowsEmptyStateWithoutInventingLocalContent() {
        show(signedIn().copy(displayName = ""))
        text(R.string.account_unnamed).assertIsDisplayed()
        text(R.string.account_empty).assertIsDisplayed()
        compose.onAllNodes(SemanticsMatcher.expectValue(SemanticsProperties.Role, Role.RadioButton)).assertCountEquals(0)
        compose.onNodeWithText(label(R.string.account_apply_selection)).assertDoesNotExist()
        compose.onNodeWithText(label(R.string.account_open_profile)).assertDoesNotExist()
        text(R.string.account_refresh_profiles).performClick()
        compose.runOnIdle {
            assertEquals(listOf(AccountIntent.RefreshProfiles to null), actions)
            assertTrue(opened.isEmpty())
        }
    }

    @Test fun profileSelectionIsExclusiveAndRestoresWithoutSubmitting() {
        val restoration = show(signedIn(
            AuthorizedProfile(first, "First fixture", false, true),
            AuthorizedProfile(second, "Second fixture", true, false),
        ))
        text(R.string.account_apply_selection).assertIsNotEnabled()
        radio("First fixture").performClick().assertIsSelected()
        radio("Second fixture").performClick().assertIsSelected()
        radio("First fixture").assertIsNotSelected()
        restoration.emulateSavedInstanceStateRestore()
        radio("Second fixture").assertIsSelected()
        radio("First fixture").assertIsNotSelected()
        compose.runOnIdle { assertTrue(actions.isEmpty()) }
        text(R.string.account_apply_selection).assertIsEnabled().performClick()
        compose.runOnIdle { assertEquals(listOf(listOf(second)), selections); assertTrue(actions.isEmpty()) }
    }

    @Test fun severalSubmitsExactlyTheCheckedSetIncludingUnavailableFirstProfile() {
        show(signedIn(AuthorizedProfile(first, "Unavailable first", false, false),
            AuthorizedProfile(second, "Available second", false, true), AuthorizedProfile(third, "Third", true, true)))
        text(R.string.account_choose_several).performClick()
        checkbox("Unavailable first").performClick().assertIsOn()
        checkbox("Third").performClick().assertIsOn()
        checkbox("Available second").assertIsOff()
        text(R.string.account_apply_selection).performClick()
        compose.runOnIdle { assertEquals(listOf(listOf(first, third)), selections); assertTrue(actions.isEmpty()) }
    }

    @Test fun profileRadiosAndChoiceModeRadiosHaveSeparateAccessibleGroups() {
        show(signedIn(AuthorizedProfile(first, "First", false, true)))
        val modeGroup = nearestSelectionGroup(radio(label(R.string.account_choose_one)).fetchSemanticsNode())
        val profileGroup = nearestSelectionGroup(radio("First").fetchSemanticsNode())
        assertNotEquals(modeGroup, profileGroup)
    }

    @Test fun allCapturesCurrentGrantsAndFreshRemovalNeverSubmitsAnOldGrant() {
        val restoration = show(signedIn(AuthorizedProfile(first, "First", false, false), AuthorizedProfile(second, "Second", false, true)))
        text(R.string.account_choose_all).performClick()
        text(R.string.account_apply_selection).performClick()
        compose.runOnIdle { assertEquals(listOf(first, second), selections.single()) }
        restoration.emulateSavedInstanceStateRestore()
        compose.runOnIdle { state.value = state.value.copy(profiles = listOf(
            AuthorizedProfile(second, "Second", false, true), AuthorizedProfile(third, "New third", false, true))) }
        radio(label(R.string.account_choose_several)).assertIsSelected()
        checkbox("Second").assertIsOn()
        checkbox("New third").assertIsOff()
        text(R.string.account_apply_selection).performClick()
        compose.runOnIdle { assertEquals(listOf(second), selections.last()) }
        text(R.string.account_choose_all).performClick()
        text(R.string.account_apply_selection).performClick()
        compose.runOnIdle { assertEquals(listOf(second, third), selections.last()) }
    }

    @Test fun switchingAccountsKeepsDraftsSeparateAndUnrelatedProgressDoesNotResetThem() {
        val accounts = listOf(HeldAccount(1, "account-a", null, "signed_in", true, null, emptyList(), false),
            HeldAccount(2, "account-b", null, "signed_in", true, null, emptyList(), false))
        val a = signedIn(AuthorizedProfile(first, "A first", false, true), AuthorizedProfile(second, "A second", false, true))
            .copy(activeScope = 1, activeAccountId = "account-a", heldAccounts = accounts)
        show(a)
        radio("A second").performClick()
        compose.runOnIdle { state.value = state.value.copy(enrollments = listOf(ProfileEnrollmentProgress(first, ProfileEnrollment.Waiting))) }
        radio("A second").assertIsSelected()
        compose.runOnIdle { state.value = a.copy(activeScope = 2, activeAccountId = "account-b", profiles = listOf(AuthorizedProfile(third, "B third", false, true))) }
        text(R.string.account_apply_selection).assertIsNotEnabled()
        radio("B third").performClick()
        compose.runOnIdle { state.value = a }
        radio("A second").assertIsSelected()
        radio("A first").assertIsNotSelected()
        compose.runOnIdle { assertTrue(selections.isEmpty()) }
        text(R.string.account_apply_selection).performClick()
        compose.runOnIdle { assertEquals(listOf(listOf(second)), selections) }
    }

    @Test fun completedLocalProfileOpensWhileEarlierSelectionWaitsAndAdmissionIsUncertain() {
        show(signedIn().copy(commandUncertain = true, pending = AccountIntent.SelectProfiles,
            enrollments = listOf(ProfileEnrollmentProgress(first, ProfileEnrollment.Waiting, "no_peer_online"),
                ProfileEnrollmentProgress(second, ProfileEnrollment.Complete))))
        compose.runOnIdle { local.value = listOf(Profile(second, "Local complete", "en")) }
        text(R.string.account_enrollment_wait).assertIsDisplayed()
        text(R.string.account_retry_enrollment).assertIsNotEnabled()
        text(R.string.account_open_profile).assertIsEnabled().performClick()
        compose.runOnIdle { assertEquals(listOf(second), opened); assertTrue(selections.isEmpty()); assertTrue(actions.isEmpty()) }
    }

    @Test fun localAndRemoteAvailabilityAreIndependentLabels() {
        show(signedIn(
            AuthorizedProfile(first, "Remote fixture", false, true),
            AuthorizedProfile(second, "Local fixture", true, false),
        ))
        radio("Remote fixture").assert(hasText(label(R.string.account_local_no)))
            .assert(hasText(label(R.string.account_remote_yes)))
        radio("Local fixture").assert(hasText(label(R.string.account_local_yes)))
            .assert(hasText(label(R.string.account_remote_no)))
    }

    @Test fun signInProgressCancellationAndRetryReflectInjectedWorkerState() {
        show(AccountScreenState(phase = AccountPhase.Provider, capable = true, generation = 7))
        text(R.string.account_wait_provider).assertIsDisplayed()
        text(R.string.account_cancel).performClick()
        compose.runOnIdle { state.value = state.value.copy(pending = AccountIntent.Cancel) }
        text(R.string.account_cancel_pending).assertIsDisplayed()
        compose.onNodeWithText(label(R.string.account_cancel)).assertDoesNotExist()
        compose.runOnIdle {
            state.value = state.value.copy(phase = AccountPhase.Failed, pending = null, failure = "fixture_failure", retryAtMillis = 10)
        }
        text(R.string.account_failed).assertIsDisplayed()
        text(R.string.account_retry_scheduled).assertIsDisplayed()
        text(R.string.account_retry).assertIsEnabled().performClick()
        compose.runOnIdle { assertEquals(listOf(AccountIntent.Cancel to null, AccountIntent.Retry to null), actions) }
    }

    @Test fun waitingForSourceExplainsRetryAndCancellationWithoutOpening() {
        show(signedIn().copy(enrollments = listOf(ProfileEnrollmentProgress(first, ProfileEnrollment.Waiting,
            retryAtMillis = 10))))
        text(R.string.account_enrollment_wait).assertIsDisplayed()
        text(R.string.account_retry_scheduled).assertIsDisplayed()
        text(R.string.account_retry_enrollment).performClick()
        text(R.string.account_cancel_enrollment).performClick()
        compose.onNodeWithText(label(R.string.account_open_profile)).assertDoesNotExist()
        compose.runOnIdle {
            assertEquals(listOf(AccountIntent.RetryEnrollment to first, AccountIntent.CancelEnrollment to null), actions)
            assertTrue(opened.isEmpty())
        }
    }

    @Test fun enrollmentReceiptAndAvailabilityNeverReplaceObservedStoreInventory() {
        show(signedIn(AuthorizedProfile(first, "Fixture profile", true, true))
            .copy(enrollments = listOf(ProfileEnrollmentProgress(first, ProfileEnrollment.Complete))))
        text(R.string.account_local_pending).assertIsDisplayed()
        compose.onNodeWithText(label(R.string.account_open_profile)).assertDoesNotExist()
        compose.runOnIdle { local.value = listOf(Profile(second, "Different local profile", "en")) }
        text(R.string.account_local_pending).assertIsDisplayed()
        compose.onNodeWithText(label(R.string.account_open_profile)).assertDoesNotExist()
        compose.runOnIdle { local.value = listOf(Profile(first, "Observed local profile", "en")) }
        text(R.string.account_open_profile).assertIsEnabled().performClick()
        compose.runOnIdle {
            assertEquals(listOf(first), opened)
            assertTrue(actions.isEmpty())
            local.value = emptyList()
        }
        text(R.string.account_local_pending).assertIsDisplayed()
        compose.onNodeWithText(label(R.string.account_open_profile)).assertDoesNotExist()
    }

    @Test fun restoredTargetWithoutAccountExplainsSignInAndKeepsLocalOpen() {
        show(AccountScreenState(phase = AccountPhase.SignedOut, capable = true,
            enrollments = listOf(ProfileEnrollmentProgress(first, ProfileEnrollment.Waiting))))
        compose.runOnIdle { local.value = listOf(Profile(first, "Retained local profile", "en")) }
        text(R.string.account_enrollment_account).assertIsDisplayed()
        compose.onNodeWithText(label(R.string.account_enrollment_wait)).assertDoesNotExist()
        text(R.string.account_open_profile).performClick()
        compose.runOnIdle { assertEquals(listOf(first), opened); assertTrue(actions.isEmpty()) }
    }
}
