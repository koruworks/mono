// SPDX-License-Identifier: MPL-2.0

package app.koru.mobile.ui

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.KeyboardArrowRight
import androidx.compose.material.icons.outlined.KeyboardArrowDown
import androidx.compose.material3.*
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalConfiguration
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import app.koru.mobile.R
import app.koru.mobile.services.BLOB_MODE_REFUSED
import app.koru.mobile.services.BLOB_NOT_UPLOADED_YET
import app.koru.mobile.services.BLOB_NO_ROOM
import app.koru.mobile.services.BlobHold
import app.koru.mobile.services.CloudDetailPart
import app.koru.mobile.services.CloudHold
import app.koru.mobile.services.CloudPath
import app.koru.mobile.services.CloudProgressRow
import app.koru.mobile.services.CloudProgressScreenState
import app.koru.mobile.services.CloudSummary
import app.koru.mobile.services.CloudWaitingLine
import app.koru.mobile.domain.WorkspaceId
import app.koru.mobile.ui.theme.KoruSpacing
import java.text.NumberFormat

/**
 * **One Profile's Cloud sync, across every Workspace** — the desktop's Manage
 * Profiles Cloud sync section, on a phone. Draws [state] and nothing else: the
 * poll, the rules and the mapping live in `CloudProgressPresenter`.
 *
 * A phone scrolls, so every Workspace is listed rather than capped under
 * *and N more* as the desktop's fixed-height pane must; the cap under an open
 * row is the desktop's own twenty lines.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun CloudProgressScreen(
    profileName: String,
    state: CloudProgressScreenState,
    onToggle: (WorkspaceId) -> Unit,
    onBack: () -> Unit,
) {
    Scaffold(topBar = {
        TopAppBar(
            title = {
                Column {
                    Text(stringResource(R.string.cloud_progress_row), maxLines = 1, overflow = TextOverflow.Ellipsis)
                    if (profileName.isNotBlank()) {
                        Text(
                            profileName,
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                        )
                    }
                }
            },
            navigationIcon = { TextButton(onClick = onBack) { Text(stringResource(R.string.back)) } },
        )
    }) { padding ->
        when (state) {
            is CloudProgressScreenState.Off -> Message(
                padding,
                heading = stringResource(R.string.account_cloud_sync),
                body = stringResource(
                    if (state.unattached) R.string.account_cloud_sync_unattached else R.string.cloud_progress_sync_off,
                ),
            )
            CloudProgressScreenState.Loading -> Message(padding, body = stringResource(R.string.cloud_progress_loading))
            CloudProgressScreenState.Unavailable ->
                Message(padding, body = stringResource(R.string.cloud_progress_unavailable))
            is CloudProgressScreenState.Ready -> LazyColumn(
                Modifier.fillMaxSize().padding(padding),
                contentPadding = PaddingValues(vertical = KoruSpacing.sm),
            ) {
                item {
                    Text(
                        summaryText(state.summary),
                        modifier = Modifier.padding(horizontal = KoruSpacing.lg, vertical = KoruSpacing.sm),
                        style = MaterialTheme.typography.titleMedium,
                    )
                }
                items(state.rows, key = { it.id.value }) { row -> WorkspaceRow(row, onToggle) }
            }
        }
    }
}

@Composable
private fun Message(padding: PaddingValues, body: String, heading: String? = null) {
    Column(Modifier.fillMaxSize().padding(padding).padding(KoruSpacing.lg)) {
        heading?.let { Text(it, style = MaterialTheme.typography.titleMedium) }
        Text(body, style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
    }
}

@Composable
private fun WorkspaceRow(row: CloudProgressRow, onToggle: (WorkspaceId) -> Unit) {
    val open = row.waiting != null
    Column(Modifier.fillMaxWidth()) {
        Row(
            Modifier
                .fillMaxWidth()
                .then(
                    if (row.openable) Modifier.clickable(role = Role.Button) { onToggle(row.id) }
                    else Modifier,
                )
                .padding(horizontal = KoruSpacing.lg, vertical = KoruSpacing.sm),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Column(Modifier.weight(1f)) {
                Text(row.name, style = MaterialTheme.typography.bodyLarge, maxLines = 1, overflow = TextOverflow.Ellipsis)
                Text(
                    detailText(row.detail),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            if (row.openable) {
                Icon(
                    if (open) Icons.Outlined.KeyboardArrowDown else Icons.AutoMirrored.Outlined.KeyboardArrowRight,
                    contentDescription = null,
                    tint = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
        row.waiting?.forEach { line ->
            Text(
                waitingText(line),
                modifier = Modifier.padding(start = KoruSpacing.lg + 16.dp, end = KoruSpacing.lg, bottom = KoruSpacing.xs),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}

/** The reader's grouping: *1,208*, *1.208*, *١٬٢٠٨*. */
@Composable
private fun count(value: Long): String =
    NumberFormat.getIntegerInstance(LocalConfiguration.current.locales[0]).format(value)

@Composable
private fun summaryText(summary: CloudSummary): String {
    val parts = mutableListOf(
        stringResource(
            R.string.cloud_progress_workspaces,
            count(summary.onCloud.toLong()),
            count(summary.workspaces.toLong()),
        ),
    )
    if (summary.referenced > 0) {
        parts += stringResource(R.string.cloud_progress_files, count(summary.durable), count(summary.referenced))
    }
    if (summary.held > 0) parts += stringResource(R.string.cloud_progress_held, count(summary.held.toLong()))
    return parts.joinToString(" · ")
}

@Composable
private fun detailText(parts: List<CloudDetailPart>): String = parts.map { part ->
    when (part) {
        is CloudDetailPart.Documents ->
            stringResource(R.string.cloud_progress_documents, count(part.synced.toLong()), count(part.required.toLong()))
        is CloudDetailPart.Files ->
            stringResource(R.string.cloud_progress_files, count(part.durable.toLong()), count(part.referenced.toLong()))
        is CloudDetailPart.Held -> holdText(part.hold)
        CloudDetailPart.SyncOff -> stringResource(R.string.cloud_progress_sync_off)
        is CloudDetailPart.Moving -> {
            val moving = stringResource(if (part.sending) R.string.cloud_progress_sending else R.string.cloud_progress_receiving)
            when (part.path) {
                CloudPath.Cloud -> "$moving · ${stringResource(R.string.cloud_progress_path_cloud)}"
                CloudPath.Peer -> "$moving · ${stringResource(R.string.cloud_progress_path_peer)}"
                null -> moving
            }
        }
        CloudDetailPart.SyncOn -> stringResource(R.string.cloud_progress_sync_on)
        CloudDetailPart.Unavailable -> stringResource(R.string.cloud_progress_unavailable)
    }
}.joinToString(" · ")

/**
 * **A hold in the words this app already uses for it** — the Documents
 * screen's file transfer panel says each of these codes with the same
 * resource, so the two surfaces never describe one hold two ways. A code no
 * sentence exists for is shown verbatim after *Waiting:*, as the desktop
 * does: it is what a person reads back to us.
 */
internal fun holdSentence(hold: CloudHold): Int? = when (hold) {
    CloudHold.PausedByYou -> R.string.blob_state_paused_person
    is CloudHold.Code -> when (hold.code) {
        "cloud_policy_unavailable" -> R.string.cloud_upload_policy_unavailable
        "cloud_plan_required" -> R.string.cloud_upload_plan_required
        "file_exceeds_plan_allowance" -> R.string.cloud_upload_file_allowance
        "usage_unknown" -> R.string.cloud_upload_usage_unknown
        "account_over_quota" -> R.string.cloud_upload_account_quota
        "cloud_document_count_limit" -> R.string.cloud_upload_document_count
        "cloud_stored_document_bytes_limit" -> R.string.cloud_upload_document_bytes
        "cloud_physical_bytes_limit" -> R.string.cloud_upload_physical_bytes
        "cloud_staging_bytes_limit" -> R.string.cloud_upload_staging_bytes
        "cloud_removal_capacity_limit" -> R.string.cloud_upload_removal_capacity

        BlobHold.Metered.code -> R.string.blob_state_paused_metered
        BlobHold.LowData.code -> R.string.blob_state_paused_low_data
        BlobHold.Offline.code -> R.string.blob_state_paused_offline
        BlobHold.Person.code -> R.string.blob_state_paused_person
        BlobHold.Cloud.code -> R.string.blob_state_no_cloud
        BLOB_MODE_REFUSED -> R.string.blob_state_refused
        BLOB_NOT_UPLOADED_YET -> R.string.blob_state_waiting_holder
        BLOB_NO_ROOM -> R.string.blob_state_no_room
        else -> null
    }
}

@Composable
private fun holdText(hold: CloudHold): String = holdSentence(hold)?.let { stringResource(it) }
    ?: stringResource(R.string.cloud_progress_waiting_code, (hold as CloudHold.Code).code)

@Composable
private fun waitingText(line: CloudWaitingLine): String = when (line) {
    CloudWaitingLine.Reading -> stringResource(R.string.cloud_progress_loading)
    CloudWaitingLine.Unavailable -> stringResource(R.string.cloud_progress_unavailable)
    CloudWaitingLine.None -> stringResource(R.string.cloud_progress_waiting_none)
    is CloudWaitingLine.More -> stringResource(R.string.cloud_progress_more, count(line.count.toLong()))
    is CloudWaitingLine.Document -> {
        val document = line.document
        val title = document.title.trim().ifEmpty { stringResource(R.string.untitled) }
        val files = if (document.missingFiles == 1) stringResource(R.string.cloud_progress_waiting_one_file)
        else stringResource(R.string.cloud_progress_waiting_files, count(document.missingFiles.toLong()))
        val whom = if (document.holders.isEmpty()) stringResource(R.string.cloud_progress_waiting_no_holder)
        else stringResource(R.string.cloud_progress_waiting_for, document.holders.joinToString(", "))
        listOf(title, files, whom).joinToString(" · ")
    }
}
