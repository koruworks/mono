// SPDX-License-Identifier: MPL-2.0

package app.koru.mobile.services

import app.koru.contract.api.v1.AccountAction
import app.koru.contract.api.v1.AccountCommandRequest
import app.koru.contract.api.v1.MobileAccountStatus
import java.util.UUID

/** Pure validation mirrors koru-contract; generated messages stop here. */
internal object MobileAccountContract {
    fun validate(command: AccountCommandRequest) {
        require(command.version == 1 && command.requester != 0L)
        require(command.action != AccountAction.ACCOUNT_ACTION_UNSPECIFIED && command.action != AccountAction.UNRECOGNIZED)
        if (command.action !in setOf(AccountAction.ACCOUNT_ACTION_ADD_ACCOUNT, AccountAction.ACCOUNT_ACTION_SWITCH_ACCOUNT)) require(command.hasExpectedOwnerScope())
        require(command.locale.toByteArray(Charsets.UTF_8).size <= 64 && command.profileId.length <= 36 && command.accountId.length <= 36)
        if (command.action == AccountAction.ACCOUNT_ACTION_INSPECT_CLOUD_USAGE_DOCUMENTS) {
            require(command.hasCloudWorkspaceId() && command.hasCloudIncarnationId())
            listOf(command.cloudWorkspaceId, command.cloudIncarnationId).forEach {
                require(UUID.fromString(it).toString() == it)
            }
        } else require(!command.hasCloudWorkspaceId() && !command.hasCloudIncarnationId())
        if (command.action in setOf(AccountAction.ACCOUNT_ACTION_INSPECT_CLOUD_USAGE_WORKSPACES,
                AccountAction.ACCOUNT_ACTION_INSPECT_CLOUD_USAGE_DOCUMENTS)) {
            if (command.hasCloudCursor()) CloudInventoryDecoder.requireCursor(command.cloudCursor)
        } else require(!command.hasCloudCursor())
        if (command.action == AccountAction.ACCOUNT_ACTION_CANCEL) require(command.generation != 0L)
        if (command.action in setOf(AccountAction.ACCOUNT_ACTION_INSPECT_CLOUD_DOCUMENT, AccountAction.ACCOUNT_ACTION_REMOVE_CLOUD_FILE)) {
            require(command.hasCloudDocumentSelectionJson() && CloudCopySelection.decode(command.cloudDocumentSelectionJson) != null)
            if (command.action == AccountAction.ACCOUNT_ACTION_REMOVE_CLOUD_FILE) {
                require(command.hasCloudFileNodeId() && UUID.fromString(command.cloudFileNodeId).toString() == command.cloudFileNodeId)
            } else require(!command.hasCloudFileNodeId())
        } else require(!command.hasCloudDocumentSelectionJson() && !command.hasCloudFileNodeId())
        if (command.action == AccountAction.ACCOUNT_ACTION_SELECT_PROFILES) {
            require(command.profileIdsList == command.profileIdsList.distinct().sorted())
            command.profileIdsList.forEach(::requireProfileId)
        } else require(command.profileIdsCount == 0)
        // The switch names exactly one Profile, so its id and its new position
        // are both required; a set with neither would read as "every Profile".
        if (command.action == AccountAction.ACCOUNT_ACTION_SET_PROFILE_CLOUD_SYNC) {
            require(command.profileId.isNotEmpty() && command.hasCloudSyncEnabled())
            requireProfileId(command.profileId)
        } else {
            require(!command.hasCloudSyncEnabled())
            if (command.action == AccountAction.ACCOUNT_ACTION_RETRY_ENROLLMENT && command.profileId.isNotEmpty()) {
                requireProfileId(command.profileId)
            } else require(command.profileId.isEmpty())
        }
        if (command.action == AccountAction.ACCOUNT_ACTION_SWITCH_ACCOUNT) {
            val id = UUID.fromString(command.accountId)
            require(id.version() == 4 && id.variant() == 2 && id.toString() == command.accountId)
        } else require(command.accountId.isEmpty())
    }
    internal fun requireProfileId(value: String) {
        val id = UUID.fromString(value)
        require(id.version() == 4 && id.variant() == 2 && id.toString() == value)
    }
}

data class ProfileEnrollmentDiagnostics(
    val profileId: String,
    val stage: String,
    val failure: String? = null,
    val retryAtMillis: Long? = null,
) {
    companion object {
        internal fun from(value: app.koru.contract.api.v1.MobileProfileEnrollment) = ProfileEnrollmentDiagnostics(
            value.profileId, value.stage,
            value.failureCode.takeIf { value.hasFailureCode() },
            value.retryAtMillis.takeIf { value.hasRetryAtMillis() },
        )
    }
}

/** Content-free values only: safe for the developer row, never a DTO dump. */
data class AccountDiagnostics(
    val stage: String = "unavailable",
    val enrollment: String = "idle",
    val profileId: String? = null,
    val failure: String? = null,
    val retryAtMillis: Long? = null,
    val enrollmentFailure: String? = null,
    val enrollmentRetryAtMillis: Long? = null,
    val enrollments: List<ProfileEnrollmentDiagnostics> = emptyList(),
) {
    companion object {
        internal fun from(value: MobileAccountStatus) = AccountDiagnostics(
            value.stage, value.enrollmentStage,
            value.enrollmentProfileId.takeIf { value.hasEnrollmentProfileId() },
            value.failureCode.takeIf { value.hasFailureCode() },
            value.retryAtMillis.takeIf { value.hasRetryAtMillis() },
            value.enrollmentFailureCode.takeIf { value.hasEnrollmentFailureCode() },
            value.enrollmentRetryAtMillis.takeIf { value.hasEnrollmentRetryAtMillis() },
            value.enrollmentsList.map(ProfileEnrollmentDiagnostics::from),
        )
    }
}
