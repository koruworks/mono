// SPDX-License-Identifier: MPL-2.0

package app.koru.mobile.services

import app.koru.contract.api.v1.*
import app.koru.mobile.domain.ProfileId
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.launch
import kotlinx.coroutines.test.runCurrent
import org.junit.Assert.*
import org.junit.Test

@OptIn(kotlinx.coroutines.ExperimentalCoroutinesApi::class)
class AccountPresentationTest {
    private val id = ProfileId("10000000-0000-4000-8000-000000000001")
    private class Port {
        val calls = mutableListOf<Triple<AccountIntent, Long, String>>()
        var next = 2L
        var refuse: String? = null
        var loseAcknowledgement = false
        val presentation = AccountPresentation { _, intent, generation, _, profile, profiles ->
            calls += Triple(intent, generation, profiles.singleOrNull() ?: profile)
            if (loseAcknowledgement) { next++; throw IllegalStateException("fixture acknowledgement lost") }
            AccountCommandReply.newBuilder().setGeneration(if (intent == AccountIntent.Cancel) generation else next++)
                .apply { refuse?.let(::setErrorCode) }.build()
        }
    }
    private fun status(stage: String = "signed_in", generation: Long = 1, gap: Boolean = false, queue: Int = 0,
        enrollment: String = "idle", target: String? = null,
        active: Boolean? = stage in setOf("exchanging", "waiting_for_provider", "restoring", "refreshing") || queue > 0) = AndroidAccountStatus(
        worker = AccountDiagnostics(stage = stage, enrollment = enrollment, profileId = target,
            enrollments = target?.let { listOf(ProfileEnrollmentDiagnostics(it, enrollment)) } ?: emptyList()),
        generation = generation, capability = true, resultsLost = gap, queueDepth = queue, requesterActive = active)
    private fun choices(generation: Long = 2, sequence: Long = 1, rows: Boolean = true) = MobileAccountResult.newBuilder()
        .setRequester(2).setGeneration(generation).setSequence(sequence).setKind("profiles")
        .setAccount(MobileAccountSummary.newBuilder().setDisplayName("Existing account"))
        .apply { if (rows) addProfiles(MobileAccountProfile.newBuilder().setProfileId(id.value)
            .setName("Work").setAvailableLocally(true).setAvailableRemotely(false)) }.build()
    private fun result(kind: String, generation: Long, sequence: Long) = MobileAccountResult.newBuilder()
        .setRequester(2).setGeneration(generation).setSequence(sequence).setKind(kind).build()

    @Test fun emailSignInIsNotOfferedWhileGoogleAndAppleAre() = runTest {
        for (offered in listOf(AccountIntent.SignIn, AccountIntent.SignInWithApple)) {
            val port = Port()
            port.presentation.observe(status("signed_out"), emptyList())
            assertTrue(port.presentation.state.value.canSignIn)
            port.presentation.act(AccountIntent.SignInWithEmail)
            assertTrue(port.calls.isEmpty())
            assertNull(port.presentation.state.value.pending)
            port.presentation.act(offered)
            assertEquals(offered, port.calls.single().first)
        }
    }

    @Test fun missingConfigurationLeavesOfflineEntryAndDoesNotSubmit() = runTest {
        val port = Port()
        port.presentation.observe(AndroidAccountStatus(adapterFailure = "account_configuration_missing"), emptyList())
        port.presentation.act(AccountIntent.SignIn)
        assertFalse(port.presentation.state.value.canSignIn)
        assertEquals("account_configuration_missing", port.presentation.state.value.failure)
        assertTrue(port.calls.isEmpty())
    }

    @Test fun conclusiveCredentialRepairAdmitsOneFreshSignInAndReleasesStalePresentation() = runTest {
        for (code in listOf("account_credential_missing", "account_credential_rejected", "account_credential_invalid")) {
            val port = Port()
            port.presentation.observe(status(), emptyList())
            port.presentation.observe(status(generation = 2), listOf(choices()))
            assertTrue(port.presentation.state.value.signedIn)
            port.presentation.observe(status("failed", 3).copy(worker = AccountDiagnostics(stage = "failed", failure = code)), emptyList())
            assertFalse(port.presentation.state.value.signedIn)
            assertTrue(port.presentation.state.value.canSignIn)
            val count = port.calls.size
            port.presentation.act(AccountIntent.SignIn)
            port.presentation.act(AccountIntent.SignIn)
            assertEquals(count + 1, port.calls.size)
            assertEquals(AccountIntent.SignIn, port.calls.last().first)
        }
    }

    @Test fun inactiveCredentialRepairNeverInterruptsTheSignedInAccount() = runTest {
        for (code in listOf("account_credential_missing", "account_credential_rejected", "account_credential_invalid")) {
            val port = Port()
            port.presentation.observe(status(), emptyList())
            port.presentation.observe(status(generation = 2), listOf(choices()))
            port.presentation.observe(status(generation = 2).copy(worker = AccountDiagnostics(stage = "signed_in", failure = code)), emptyList())
            assertFalse(port.presentation.state.value.canSignIn)
            assertTrue(port.presentation.state.value.canChoose)
            assertEquals(id, port.presentation.state.value.profiles.single().id)
            val count = port.calls.size
            port.presentation.act(AccountIntent.SignIn)
            assertEquals(count, port.calls.size)
        }
    }

    @Test fun repairRequiresSettledAdmissionAndGenericCredentialFailuresRemainRetryOwned() {
        val repair = AccountScreenState(phase = AccountPhase.Failed, capable = true, failure = "account_credential_missing")
        assertTrue(repair.canSignIn)
        assertFalse(repair.copy(pending = AccountIntent.ConfirmCreation).canSignIn)
        assertFalse(repair.copy(commandUncertain = true).canSignIn)
        assertFalse(repair.copy(displayName = "Active account").canSignIn)
        assertFalse(repair.copy(capable = false).canSignIn)
        for (code in listOf("credential_missing", "credential_rejected", "credential_invalid", "credential_unavailable", "credential_migration_pending")) {
            val state = repair.copy(failure = code)
            assertFalse(state.canSignIn)
            assertTrue(state.canRetry)
        }
    }

    @Test fun repairIdentityMismatchAllowsAccountChoiceAndNeverSubmitsRetry() = runTest {
        val port = Port()
        port.presentation.observe(status("failed").copy(worker = AccountDiagnostics(
            stage = "failed", failure = "account_repair_identity_mismatch")), emptyList())
        val state = port.presentation.state.value
        assertFalse(state.retryAllowedForFailure)
        assertFalse(state.canRetry)
        assertTrue(state.canSwitchAccounts)
        assertTrue(state.canAddAccount)
        port.presentation.act(AccountIntent.Retry)
        assertTrue(port.calls.isEmpty())
    }

    @Test fun creationPauseAdmitsOneExplicitDecisionAndNeverAnotherGoogleRequest() = runTest {
        for (intent in listOf(AccountIntent.ConfirmCreation, AccountIntent.CancelCreation)) {
            val port = Port()
            port.presentation.observe(status("awaiting_account_creation"), emptyList())
            assertTrue(port.presentation.state.value.canDecideCreation)
            assertFalse(port.presentation.state.value.canSignIn)
            assertFalse(port.presentation.state.value.canCancel)
            port.presentation.act(AccountIntent.SignIn)
            port.presentation.act(AccountIntent.Cancel)
            port.presentation.act(intent)
            port.presentation.act(intent)
            port.presentation.act(if (intent == AccountIntent.ConfirmCreation) AccountIntent.CancelCreation else AccountIntent.ConfirmCreation)
            assertEquals(listOf(intent), port.calls.map { it.first })
            assertFalse(port.presentation.state.value.canDecideCreation)
            port.presentation.observe(status("failed", 2).copy(worker = AccountDiagnostics(stage = "failed", retryAtMillis = 200)), emptyList())
            assertTrue(port.presentation.state.value.canRetry)
            assertFalse(port.presentation.state.value.canSignIn)
            assertFalse(port.presentation.state.value.canDecideCreation)
        }
    }

    @Test fun creationSurvivesMissingResultAndExpiredAuthorityOffersFreshSignIn() = runTest {
        val port = Port()
        port.presentation.observe(status("awaiting_account_creation", gap = true), emptyList())
        assertTrue(port.presentation.state.value.canDecideCreation)
        port.presentation.act(AccountIntent.ConfirmCreation)
        port.presentation.observe(status("signed_out", 2).copy(worker = AccountDiagnostics(stage = "signed_out", failure = "account_confirmation_expired")),
            listOf(result("failed", 2, 1).toBuilder().setErrorCode("account_confirmation_expired").build()))
        assertTrue(port.presentation.state.value.canSignIn)
        assertEquals("account_confirmation_expired", port.presentation.state.value.failure)
        assertFalse(port.presentation.state.value.canDecideCreation)
        port.presentation.act(AccountIntent.SignIn)
        assertEquals(AccountIntent.SignIn, port.calls.last().first)
    }

    @Test fun lostCreationCommandAcknowledgementKeepsDecisionUnavailableUntilSharedObservation() = runTest {
        val port = Port()
        port.presentation.observe(status("awaiting_account_creation"), emptyList())
        port.loseAcknowledgement = true
        port.presentation.act(AccountIntent.ConfirmCreation)
        port.presentation.act(AccountIntent.CancelCreation)
        assertEquals(1, port.calls.size)
        port.presentation.pollFailed()
        assertEquals(AccountIntent.ConfirmCreation, port.presentation.state.value.pending)
        port.presentation.observe(status("failed", 2, active = false), emptyList())
        assertTrue(port.presentation.state.value.canRetry)
        assertFalse(port.presentation.state.value.canSignIn)
    }

    @Test fun lostCommandAcknowledgementWaitsForSharedOwnershipBeforeAdmittingAnotherAction() = runTest {
        val port = Port()
        port.presentation.observe(status("signed_out"), emptyList())
        port.loseAcknowledgement = true
        port.presentation.act(AccountIntent.SignIn)
        port.presentation.act(AccountIntent.SignIn)
        assertEquals(1, port.calls.size)
        assertFalse(port.presentation.state.value.canCancel) // generation ACK is still unknown
        port.presentation.observe(status("exchanging", 2), emptyList())
        assertEquals(AccountIntent.SignIn, port.presentation.state.value.pending)
        assertTrue(port.presentation.state.value.canCancel)
        port.presentation.observe(status("failed", 2, active = false), emptyList())
        assertNull(port.presentation.state.value.pending)
        assertTrue(port.presentation.state.value.canRetry)
        assertFalse(port.presentation.state.value.canSignIn)
    }

    @Test fun preCommandSnapshotCannotSettleLostAcknowledgementAndFailedPollPreservesIt() = runTest {
        val port = Port()
        val captured = CompletableDeferred<Unit>()
        val release = CompletableDeferred<Unit>()
        var first = true
        var fresh: AccountObservation? = null
        val cycle = AccountPresentationCycle(port.presentation) {
            if (first) {
                first = false
                val old = AccountObservation(status("signed_out", 1, active = false), emptyList())
                captured.complete(Unit)
                release.await()
                old
            } else fresh
        }
        val poll = launch { cycle.tick() }
        captured.await()
        port.loseAcknowledgement = true
        val action = launch { cycle.act(AccountIntent.SignIn, "en", null) }
        runCurrent()
        assertTrue(port.calls.isEmpty())
        release.complete(Unit)
        poll.join(); action.join()
        assertEquals(AccountIntent.SignIn, port.presentation.state.value.pending)
        cycle.tick() // no fresh status is not an inactive acknowledgement
        assertEquals(AccountIntent.SignIn, port.presentation.state.value.pending)
        fresh = AccountObservation(status("exchanging", 2, active = true), emptyList())
        cycle.tick()
        assertEquals(AccountIntent.SignIn, port.presentation.state.value.pending)
        fresh = AccountObservation(status("failed", 2, active = false), emptyList())
        cycle.tick()
        assertNull(port.presentation.state.value.pending)
        assertTrue(port.presentation.state.value.canRetry)
    }

    @Test fun returningAccountLoadsOnceAndKeepsAvailabilityIndependent() = runTest {
        val port = Port()
        port.presentation.observe(status(), emptyList())
        assertEquals(AccountIntent.RefreshProfiles, port.calls.single().first)
        port.presentation.observe(status(generation = 2), listOf(choices()))
        repeat(5) { port.presentation.observe(status(generation = 2), listOf(choices())) }
        val state = port.presentation.state.value
        assertEquals("Existing account", state.displayName)
        assertTrue(state.profiles.single().local)
        assertFalse(state.profiles.single().remote)
        assertTrue(state.canChoose)
        assertEquals(1, port.calls.size)
    }

    @Test fun returningToForegroundRefreshesProfilesWithoutAnotherGoogleSignIn() = runTest {
        val port = Port()
        port.presentation.observe(status(), emptyList())
        port.presentation.observe(status(generation = 2), listOf(choices()))
        assertTrue(port.presentation.state.value.profilesLoaded)

        port.presentation.foregrounded()
        assertFalse(port.presentation.state.value.profilesLoaded)
        port.presentation.observe(status(generation = 2), listOf(choices()))
        repeat(3) { port.presentation.observe(status(generation = 2), listOf(choices())) }

        assertEquals(listOf(AccountIntent.RefreshProfiles, AccountIntent.RefreshProfiles), port.calls.map { it.first })
        port.presentation.observe(status(generation = 3), listOf(choices(), choices(generation = 3, sequence = 2)))
        assertTrue(port.presentation.state.value.profilesLoaded)
        assertEquals(listOf(AccountIntent.RefreshProfiles, AccountIntent.RefreshProfiles), port.calls.map { it.first })
    }

    @Test fun zeroProfilesIsAnExistingAccountNotACreationRequest() = runTest {
        val port = Port()
        port.presentation.observe(status(), emptyList())
        port.presentation.observe(status(generation = 2), listOf(choices(rows = false)))
        assertEquals("Existing account", port.presentation.state.value.displayName)
        assertTrue(port.presentation.state.value.profilesLoaded)
        assertTrue(port.presentation.state.value.profiles.isEmpty())
        assertFalse(port.presentation.state.value.canSignIn)
    }

    @Test fun onlyOneAuthorizedProfileCanBeSubmittedAndRepeatedTapsDoNotQueue() = runTest {
        val port = Port()
        port.presentation.observe(status(), emptyList())
        port.presentation.observe(status(generation = 2), listOf(choices()))
        port.presentation.act(AccountIntent.SelectProfiles, profileIds = listOf(ProfileId("not-authorized")))
        assertEquals(1, port.calls.size)
        port.presentation.act(AccountIntent.SelectProfiles, profileIds = listOf(id))
        port.presentation.act(AccountIntent.SelectProfiles, profileIds = listOf(id))
        assertEquals(2, port.calls.size)
        assertEquals(id.value, port.calls.last().third)
    }

    @Test fun newStatusBeforeResultClearsAuthorizationAndWaitsForAcknowledgement() = runTest {
        val port = Port()
        port.presentation.observe(status(), emptyList())
        port.presentation.observe(status(generation = 2), listOf(choices()))
        port.presentation.act(AccountIntent.Logout)
        assertFalse(port.presentation.state.value.canCancel)
        port.presentation.observe(status("signed_out", 3, active = true), listOf(choices()))
        assertTrue(port.presentation.state.value.profiles.isEmpty())
        assertFalse(port.presentation.state.value.canSignIn)
        port.presentation.act(AccountIntent.SignIn)
        assertEquals(2, port.calls.size) // no Logout -> SignIn without observing SignedOut
        port.presentation.observe(status("signed_out", 3), listOf(choices(), result("signed_out", 3, 2)))
        assertTrue(port.presentation.state.value.canSignIn)
        assertNull(port.presentation.state.value.displayName)
    }

    @Test fun gapDiscardsEvenCurrentCachedChoicesAndFetchesFreshAuthority() = runTest {
        val port = Port()
        port.presentation.observe(status(), emptyList())
        port.presentation.observe(status(generation = 2), listOf(choices()))
        port.presentation.observe(status(generation = 2, gap = true), listOf(choices(sequence = 20)))
        assertFalse(port.presentation.state.value.profilesLoaded)
        assertEquals(2, port.calls.size)
        port.presentation.observe(status(generation = 3), listOf(choices(sequence = 20), choices(3, 21)))
        assertTrue(port.presentation.state.value.profilesLoaded)
        assertEquals(2, port.calls.size)
    }

    @Test fun gapDuringActiveWorkWaitsUntilSharedOutcomeAndNeverRetriesSignIn() = runTest {
        val port = Port()
        port.presentation.observe(status("signed_out"), emptyList())
        port.presentation.act(AccountIntent.SignIn)
        port.presentation.observe(status("exchanging", 2, gap = true, queue = 1), emptyList())
        assertEquals(AccountIntent.SignIn, port.presentation.state.value.pending)
        port.presentation.observe(status(generation = 2), emptyList())
        assertEquals(listOf(AccountIntent.SignIn, AccountIntent.RefreshProfiles), port.calls.map { it.first })
    }

    @Test fun cancellationUsesLiveGenerationAndWaitsRatherThanClearingTheWorker() = runTest {
        val port = Port()
        port.presentation.observe(status("signed_out"), emptyList())
        port.presentation.act(AccountIntent.SignIn)
        port.presentation.observe(status("exchanging", 2), emptyList())
        port.presentation.act(AccountIntent.Cancel)
        assertEquals(2L, port.calls.last().second)
        assertEquals(AccountIntent.Cancel, port.presentation.state.value.pending)
        port.presentation.observe(status("exchanging", 2), emptyList())
        assertFalse(port.presentation.state.value.canSignIn)
        port.presentation.observe(status("cancelled", 2), listOf(result("cancelled", 2, 1)))
        assertTrue(port.presentation.state.value.canSignIn)
    }

    @Test fun failedBackoffWithEmptyQueueStillOwnsUncertainExchangeAfterGap() = runTest {
        val port = Port()
        port.presentation.observe(status("signed_out"), emptyList())
        port.presentation.act(AccountIntent.SignIn)
        val backoff = status("failed", 2, gap = true, active = true).copy(worker = AccountDiagnostics(stage = "failed", retryAtMillis = 5000))
        port.presentation.observe(backoff, emptyList())
        assertEquals(AccountIntent.SignIn, port.presentation.state.value.pending)
        assertFalse(port.presentation.state.value.canSignIn)
        assertTrue(port.presentation.state.value.canRetry)
        port.presentation.act(AccountIntent.SignIn)
        assertEquals(1, port.calls.size)
        port.presentation.observe(status(generation = 2), listOf(result("signed_in", 2, 1)))
        assertEquals(AccountIntent.RefreshProfiles, port.calls.last().first)
    }

    @Test fun explicitRetryDuringBackoffIsAdmittedOnlyOnceBeforeObservation() = runTest {
        val port = Port()
        port.presentation.observe(status("signed_out"), emptyList())
        port.presentation.act(AccountIntent.SignIn)
        port.presentation.observe(status("failed", 2, active = true).copy(worker = AccountDiagnostics(stage = "failed", retryAtMillis = 5000)), emptyList())
        port.presentation.act(AccountIntent.Retry)
        port.presentation.act(AccountIntent.Retry)
        assertEquals(listOf(AccountIntent.SignIn, AccountIntent.Retry), port.calls.map { it.first })
        assertFalse(port.presentation.state.value.canRetry)
    }

    @Test fun unrelatedPendingOperationsNeverOfferSignInCancellation() {
        listOf(AccountIntent.Logout, AccountIntent.RefreshProfiles, AccountIntent.SelectProfiles,
            AccountIntent.RetryEnrollment, AccountIntent.CancelEnrollment, AccountIntent.Retry).forEach { intent ->
            assertFalse(intent.name, AccountScreenState(phase = AccountPhase.SignedIn, generation = 7, pending = intent).canCancel)
        }
    }

    @Test fun terminalVaultFailureWithoutResultLeavesRetryReachable() = runTest {
        val port = Port()
        port.presentation.observe(status(), emptyList())
        port.presentation.observe(status(generation = 2), listOf(choices()))
        port.presentation.act(AccountIntent.Logout)
        port.presentation.observe(status("failed", 3).copy(worker = AccountDiagnostics(stage = "failed", failure = "credential_unavailable")), emptyList())
        assertNull(port.presentation.state.value.pending)
        assertTrue(port.presentation.state.value.canRetry)
        assertFalse(port.presentation.state.value.canSignIn)
    }

    @Test fun missingOwnershipFieldCannotSettleAcknowledgementAndExhaustedRecoveryOnlyOffersRetry() = runTest {
        val port = Port()
        port.presentation.observe(status("signed_out"), emptyList())
        port.presentation.act(AccountIntent.SignIn)
        port.presentation.observe(status("failed", 2, gap = true, active = null), emptyList())
        assertEquals(AccountIntent.SignIn, port.presentation.state.value.pending)
        assertFalse(port.presentation.state.value.canSignIn)
        port.presentation.observe(status("failed", 2, active = false), emptyList())
        assertNull(port.presentation.state.value.pending)
        assertTrue(port.presentation.state.value.canRetry)
        assertFalse(port.presentation.state.value.canSignIn)
    }

    @Test fun recreatedPresentationStartsWithNoLoginContinuationAndRestoredStatusLoadsProfiles() = runTest {
        val beforeVault = Port()
        beforeVault.presentation.observe(status("signed_out"), emptyList())
        assertTrue(beforeVault.presentation.state.value.canSignIn)
        assertTrue(beforeVault.calls.isEmpty())
        val afterVault = Port()
        afterVault.presentation.observe(status("restoring"), emptyList())
        assertTrue(afterVault.calls.isEmpty())
        afterVault.presentation.observe(status(), emptyList())
        assertEquals(AccountIntent.RefreshProfiles, afterVault.calls.single().first)
    }

    @Test fun noPeerAndCompletionAreSharedProgressNotContentReadiness() = runTest {
        val port = Port()
        port.presentation.observe(status(enrollment = "waiting", target = id.value), emptyList())
        port.presentation.observe(status(generation = 2, enrollment = "waiting", target = id.value), listOf(choices()))
        assertEquals(ProfileEnrollment.Waiting, port.presentation.state.value.enrollment)
        port.presentation.act(AccountIntent.RetryEnrollment)
        assertEquals(AccountIntent.RetryEnrollment, port.calls.last().first)
        port.presentation.observe(status(generation = 3, enrollment = "complete", target = id.value), listOf(result("completed", 3, 2)))
        assertEquals(ProfileEnrollment.Complete, port.presentation.state.value.enrollment)
        assertEquals(id, port.presentation.state.value.enrollmentProfile)
    }
}
