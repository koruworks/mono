// SPDX-License-Identifier: MPL-2.0
package app.koru.mobile.ui.editor

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedCard
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import app.koru.mobile.R
import app.koru.mobile.domain.*
import app.koru.mobile.services.CloudCopyFile
import app.koru.mobile.services.CloudCopyStructure
import app.koru.mobile.ui.theme.KoruSpacing

/** Display metadata only. No account authority, Store handle or draft editor. */
internal data class PassiveBlockLayout(
    val block: Block,
    val depth: Int,
    val listParent: BlockContent.ListContainer?,
    val ordinal: Int,
    val callout: Block?,
    val calloutHead: Boolean,
)

/** Parent-before-child input is checked once, then every row lookup is constant time. */
internal fun passiveBlockLayouts(blocks: List<Block>, structuralContainers: Set<BlockId> = emptySet()): List<PassiveBlockLayout>? {
    if (blocks.size > 2_000) return null
    val indexed = HashMap<BlockId, PassiveBlockLayout>(blocks.size)
    val ordinals = HashMap<BlockId?, Int>()
    val calloutHeads = HashSet<BlockId>()
    val visible = ArrayList<PassiveBlockLayout>(blocks.size)
    for (block in blocks) {
        if (indexed.containsKey(block.id)) return null
        val parent = block.parentId?.let { indexed[it] ?: return null }
        val ordinal = (ordinals[block.parentId] ?: 0) + 1
        ordinals[block.parentId] = ordinal
        val callout = parent?.block?.takeIf { it.content is BlockContent.Callout } ?: parent?.callout
        val shown = block.content !is BlockContent.Callout && block.content !is BlockContent.ListContainer
        val row = PassiveBlockLayout(
            block = block,
            depth = parent?.let { it.depth + if (it.block.content is BlockContent.Callout || it.block.id in structuralContainers) 0 else 1 } ?: 0,
            listParent = parent?.block?.content as? BlockContent.ListContainer,
            ordinal = ordinal,
            callout = callout,
            calloutHead = shown && callout != null && calloutHeads.add(callout.id),
        )
        indexed[block.id] = row
        if (shown) visible.add(row)
    }
    return visible
}

/**
 * The authenticated inspection adapter supplies a detached snapshot. This body
 * cannot load blobs, navigate references, edit drafts, or flush on disposal.
 * Explicit file controls are a caller slot, populated only from native proof.
 */
@Composable
internal fun CloudCopyBody(
    document: DocumentSnapshot,
    modifier: Modifier = Modifier,
    files: Map<BlockId, CloudCopyFile> = emptyMap(),
    trashedNodes: Set<BlockId> = emptySet(),
    structure: Map<BlockId, CloudCopyStructure> = emptyMap(),
    fileControls: @Composable (Block) -> Unit = {},
) {
    val rows = remember(document.blocks, structure) { passiveBlockLayouts(document.blocks, structure.keys) }
    if (rows == null) {
        Text(stringResource(R.string.cloud_inventory_unavailable), modifier)
        return
    }
    LazyColumn(
        modifier = modifier.fillMaxSize(),
        contentPadding = PaddingValues(KoruSpacing.md),
        verticalArrangement = Arrangement.spacedBy(KoruSpacing.sm),
    ) {
        items(rows, key = { it.block.id.value }) { row ->
            Column {
                val trashRoot = structure[row.block.id] == CloudCopyStructure.Trash
                if (row.block.id in trashedNodes && !trashRoot) Text(stringResource(R.string.cloud_inventory_trash), style = MaterialTheme.typography.labelSmall)
                val file = files[row.block.id]
                if (trashRoot) {
                    Text(stringResource(R.string.cloud_inventory_trash), style = MaterialTheme.typography.titleMedium)
                } else if (file != null) {
                    OutlinedCard {
                        Column(Modifier.padding(KoruSpacing.md), verticalArrangement = Arrangement.spacedBy(KoruSpacing.sm)) {
                            row.block.properties.name?.takeIf(String::isNotBlank)?.let { Text(it) }
                            file.mimeType.takeIf(String::isNotBlank)?.let { Text(it, style = MaterialTheme.typography.bodySmall) }
                            Text(stringResource(R.string.cloud_copy_preview_unavailable), style = MaterialTheme.typography.bodySmall)
                        }
                    }
                } else DocumentBlockRow(
                    block = row.block,
                    all = emptyList(),
                    depth = row.depth,
                    calloutHead = row.calloutHead,
                    runs = document.runs,
                    image = { _, label ->
                        OutlinedCard {
                            Column(Modifier.padding(KoruSpacing.md)) {
                                label?.takeIf(String::isNotBlank)?.let { Text(it) }
                                Text(stringResource(R.string.image_unavailable), style = MaterialTheme.typography.bodySmall)
                            }
                        }
                    },
                    follow = null,
                    editing = false,
                    draft = null,
                    onText = { _, _ -> },
                    onDone = {},
                    onSplit = { _, _ -> },
                    requestFocus = false,
                    onFocused = {},
                    indexed = row,
                )
                fileControls(row.block)
            }
        }
    }
}
