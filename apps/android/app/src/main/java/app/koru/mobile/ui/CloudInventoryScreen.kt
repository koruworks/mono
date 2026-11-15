// SPDX-License-Identifier: MPL-2.0

package app.koru.mobile.ui

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalConfiguration
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.*
import app.koru.mobile.R
import app.koru.mobile.services.*
import app.koru.mobile.ui.theme.KoruSpacing

/** A bounded authoritative page. Cloud copy inspection carries the entire
 * source selection; it never opens the local editor by DocumentId alone. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun CloudInventoryScreen(
    state: CloudInventoryState, profileNames: Map<String, String>, back: () -> Unit,
    first: () -> Unit, next: () -> Unit, previous: () -> Unit, workspaces: () -> Unit,
    documents: (CloudWorkspaceRow) -> Unit,
    inspect: (CloudDocumentRow, CloudDocumentEpoch) -> Unit,
) {
    val locale = LocalConfiguration.current.locales[0]
    Scaffold(topBar = { TopAppBar(title = { Text(stringResource(R.string.cloud_inventory_title)) },
        navigationIcon = { TextButton(onClick = back) { Text(stringResource(R.string.back)) } }) }) { padding ->
        LazyColumn(Modifier.fillMaxSize().padding(padding), contentPadding = PaddingValues(KoruSpacing.md),
            verticalArrangement = Arrangement.spacedBy(KoruSpacing.md)) {
            if (state.selection is CloudInventorySelection.Documents) item {
                TextButton(onClick = workspaces) { Text(stringResource(R.string.workspaces)) }
            }
            if (state.restarted) item { Text(stringResource(R.string.cloud_inventory_restarted),
                modifier = Modifier.semantics { liveRegion = LiveRegionMode.Polite }) }
            if (state.loading || state.page == null) item {
                Text(stringResource(if (state.failed || state.accountId == null) R.string.cloud_inventory_unavailable else R.string.cloud_inventory_loading),
                    modifier = Modifier.semantics { liveRegion = LiveRegionMode.Polite })
            }
            if (state.failed) item { TextButton(onClick = first, enabled = !state.loading) { Text(stringResource(R.string.account_retry)) } }
            when (val page = state.page) {
                is CloudInventoryPage.Workspaces -> {
                    if (page.rows.isEmpty()) item { Text(stringResource(R.string.cloud_inventory_empty)) }
                    val groups = page.rows.groupBy { it.placement to it.profile }
                    groups.forEach { (key, rows) ->
                        item(key = "group:${key.first}:${key.second}") {
                            val label = when (key.first) {
                                CloudPlacement.Assigned -> key.second?.let(profileNames::get)?.takeIf { it.isNotBlank() }
                                    ?: stringResource(R.string.cloud_inventory_profile)
                                CloudPlacement.Unassigned -> stringResource(R.string.cloud_inventory_unassigned)
                                CloudPlacement.Unknown -> stringResource(R.string.cloud_inventory_placement_unknown)
                            }
                            Text(label, style = MaterialTheme.typography.titleMedium, modifier = Modifier.semantics { heading() })
                            key.second?.let { profile ->
                                page.profileSubtotal(profile, state.firstPage)?.let { bytes ->
                                    Text(stringResource(R.string.cloud_inventory_profile_total, cloudStorageInteger(bytes, locale)))
                                }
                                if (profileNames[profile].isNullOrBlank()) IdentifierDetails(profile)
                            }
                        }
                        items(rows, key = { "workspace:${it.workspace}:${it.incarnation}" }) { row ->
                            Column(verticalArrangement = Arrangement.spacedBy(KoruSpacing.sm)) {
                                TextButton(onClick = { documents(row) }, enabled = !state.loading) {
                                    Text(row.name?.takeIf { it.isNotBlank() } ?: stringResource(R.string.workspace_subtitle))
                                }
                                InventoryAmount(row.amount)
                                if (row.name.isNullOrBlank()) IdentifierDetails(row.workspace)
                            }
                        }
                    }
                }
                is CloudInventoryPage.Documents -> {
                    item { Text(stringResource(R.string.cloud_inventory_non_additive), style = MaterialTheme.typography.bodySmall) }
                    if (page.rows.isEmpty()) item { Text(stringResource(R.string.cloud_inventory_empty)) }
                    val groups = page.rows.groupBy { it.membership }
                    CloudDocumentMembership.entries.forEach { membership ->
                        groups[membership]?.let { rows ->
                            item(key = "membership:$membership") {
                                Text(stringResource(when (membership) {
                                    CloudDocumentMembership.Listed -> R.string.documents
                                    CloudDocumentMembership.Trash -> R.string.cloud_inventory_trash
                                    CloudDocumentMembership.Unfiled -> R.string.cloud_inventory_unfiled
                                    CloudDocumentMembership.Structural -> R.string.cloud_inventory_structure
                                    CloudDocumentMembership.Unknown -> R.string.cloud_inventory_documents_unavailable
                                }), style = MaterialTheme.typography.titleMedium, modifier = Modifier.semantics { heading() })
                                if (membership == CloudDocumentMembership.Unfiled) Text(stringResource(R.string.cloud_inventory_recovery),
                                    style = MaterialTheme.typography.bodySmall)
                            }
                            items(rows, key = { "unit:${it.unit}" }) { row ->
                                Column(verticalArrangement = Arrangement.spacedBy(KoruSpacing.sm)) {
                                    Text(row.name?.takeIf { it.isNotBlank() } ?: stringResource(R.string.document_subtitle))
                                    InventoryAmount(row.amount)
                                    if (row.name.isNullOrBlank()) IdentifierDetails(row.document ?: row.unit)
                                    if (row.document != null && row.head != null && row.membership != CloudDocumentMembership.Structural) {
                                        TextButton(onClick = { inspect(row, page.epoch) }, enabled = !state.loading) {
                                            Text(stringResource(R.string.cloud_copy_title))
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                null -> Unit
            }
            if (state.page != null) item {
                Column {
                    TextButton(onClick = previous, enabled = state.canPrevious && !state.loading) { Text(stringResource(R.string.cloud_inventory_previous)) }
                    TextButton(onClick = next, enabled = state.page.nextCursor != null && !state.loading) { Text(stringResource(R.string.cloud_inventory_next)) }
                    TextButton(onClick = first, enabled = !state.loading) { Text(stringResource(R.string.cloud_inventory_first)) }
                }
            }
        }
    }
}

@Composable
private fun InventoryAmount(amount: CloudInventoryAmount) {
    val locale = LocalConfiguration.current.locales[0]
    Text(when (amount.state) {
        CloudStorageUsage.Known -> stringResource(R.string.cloud_storage_used, cloudStorageInteger(amount.bytes!!, locale))
        CloudStorageUsage.Building -> stringResource(R.string.cloud_storage_calculating)
        CloudStorageUsage.Unknown -> stringResource(R.string.cloud_storage_unknown)
        CloudStorageUsage.NewerSchema -> stringResource(R.string.cloud_storage_schema)
    })
}

@Composable
private fun IdentifierDetails(identifier: String) {
    var expanded by remember(identifier) { mutableStateOf(false) }
    TextButton(onClick = { expanded = !expanded }) { Text(stringResource(R.string.cloud_storage_details)) }
    if (expanded) Text(stringResource(R.string.cloud_inventory_identifier, identifier), style = MaterialTheme.typography.bodySmall)
}
