// SPDX-License-Identifier: MPL-2.0
package app.koru.mobile.ui.editor

import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import app.koru.mobile.R
import app.koru.mobile.domain.BlockId
import app.koru.mobile.domain.DocumentSnapshot
import app.koru.mobile.services.CloudCopyFile
import app.koru.mobile.services.CloudCopyStructure
import app.koru.mobile.ui.theme.KoruSpacing

/** Presentation states; only the authenticated native adapter can supply content/actions. */
internal enum class CloudCopyPhase {
    Loading, Verified, Unavailable, SourceChanged, Removing, OutcomeUnknown, Acknowledged,
}

internal fun cloudCopyRemovableNodes(document: DocumentSnapshot?, permittedNodes: Set<BlockId>): Set<BlockId> {
    if (document == null || passiveBlockLayouts(document.blocks) == null) return emptySet()
    val parents = document.blocks.mapNotNullTo(HashSet()) { it.parentId }
    return document.blocks.mapNotNullTo(HashSet()) { it.id.takeIf { id -> id in permittedNodes && id !in parents } }
}

internal fun cloudCopyRemovalEnabled(phase: CloudCopyPhase, removable: Set<BlockId>, node: BlockId): Boolean =
    phase == CloudCopyPhase.Verified && node in removable

/**
 * Pure composition. No navigation to a local document, image fetch, or local
 * editing lifecycle. The caller owns the full authenticated selection and must
 * revalidate it on every supplied callback. Acknowledgement never subtracts bytes.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun CloudCopyScreen(
    document: DocumentSnapshot?,
    phase: CloudCopyPhase,
    permittedNodes: Set<BlockId>,
    files: Map<BlockId, CloudCopyFile>,
    trashedNodes: Set<BlockId>,
    structure: Map<BlockId, CloudCopyStructure>,
    onBack: () -> Unit,
    onReload: () -> Unit,
    onRemove: (BlockId) -> Unit,
) {
    var confirmation by remember(document) { mutableStateOf<BlockId?>(null) }
    val removable = remember(document, permittedNodes) { cloudCopyRemovableNodes(document, permittedNodes) }
    LaunchedEffect(phase, removable) {
        if (confirmation?.let { !cloudCopyRemovalEnabled(phase, removable, it) } == true)
            confirmation = null
    }
    Scaffold(topBar = {
        TopAppBar(
            title = { Text(stringResource(R.string.cloud_copy_title)) },
            navigationIcon = { TextButton(onClick = onBack) { Text(stringResource(R.string.back)) } },
        )
    }) { padding ->
        Column(Modifier.padding(padding).fillMaxSize()) {
            Column(Modifier.fillMaxWidth().padding(KoruSpacing.md), verticalArrangement = Arrangement.spacedBy(KoruSpacing.sm)) {
                document?.title?.takeIf(String::isNotBlank)?.let { Text(it, style = MaterialTheme.typography.titleMedium) }
                Text(stringResource(R.string.cloud_copy_passive), style = MaterialTheme.typography.bodySmall)
                val status = when (phase) {
                    CloudCopyPhase.Loading -> R.string.cloud_copy_loading
                    CloudCopyPhase.Unavailable -> R.string.cloud_copy_unavailable
                    CloudCopyPhase.SourceChanged -> R.string.cloud_copy_changed
                    CloudCopyPhase.Removing -> R.string.cloud_copy_removing
                    CloudCopyPhase.OutcomeUnknown -> R.string.cloud_copy_unknown
                    CloudCopyPhase.Acknowledged -> R.string.cloud_copy_acknowledged
                    CloudCopyPhase.Verified -> if (document == null) R.string.cloud_copy_unavailable
                        else if (removable.isEmpty()) R.string.cloud_copy_no_actions else null
                }
                status?.let { Text(stringResource(it)) }
                if (phase == CloudCopyPhase.Loading || phase == CloudCopyPhase.Removing) LinearProgressIndicator(Modifier.fillMaxWidth())
                if (phase != CloudCopyPhase.Loading && phase != CloudCopyPhase.Removing) {
                    TextButton(onClick = { confirmation = null; onReload() }) { Text(stringResource(R.string.cloud_copy_reload)) }
                }
            }
            if (document != null && phase in setOf(CloudCopyPhase.Verified, CloudCopyPhase.Removing)) {
                CloudCopyBody(document, Modifier.weight(1f), files, trashedNodes, structure) { block ->
                    if (block.id in removable) {
                        TextButton(
                            enabled = cloudCopyRemovalEnabled(phase, removable, block.id),
                            onClick = { confirmation = block.id },
                        ) { Text(stringResource(R.string.cloud_copy_remove)) }
                    }
                }
            }
        }
    }
    confirmation?.let { node ->
        AlertDialog(
            onDismissRequest = { confirmation = null },
            title = { Text(stringResource(R.string.cloud_copy_remove_question)) },
            text = { Text(stringResource(R.string.cloud_copy_remove_explanation)) },
            confirmButton = {
                TextButton(
                    enabled = cloudCopyRemovalEnabled(phase, removable, node),
                    onClick = {
                        if (cloudCopyRemovalEnabled(phase, removable, node)) onRemove(node)
                        confirmation = null
                    },
                ) { Text(stringResource(R.string.cloud_copy_remove)) }
            },
            dismissButton = { TextButton(onClick = { confirmation = null }) { Text(stringResource(R.string.cancel)) } },
        )
    }
}
