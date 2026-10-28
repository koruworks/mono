// SPDX-License-Identifier: MPL-2.0

package app.koru.mobile.services

import app.koru.mobile.domain.*
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow

data class BlockEditKey(val document: DocumentId, val block: BlockId)
data class BlockDraft(
    val base: RichText,
    val text: String,
    val createParagraph: Boolean = false,
    val saving: Boolean = false,
    val error: String? = null,
    val split: BlockTextSplit? = null,
) {
    val displayedText: String get() = split?.let { text.encodeToByteArray().copyOfRange(0, it.atUtf8).decodeToString() } ?: text
}

data class PendingParagraph(val after: BlockId, val block: Block)

/** Main-thread owned drafts survive screen/keyboard changes. Typing resets a
 * document's idle timer, but cannot postpone its maximum batching deadline.
 * A failed write retains the draft and stops the lane, never rebasing stale
 * byte offsets onto a newer peer edit. */
class DocumentEdits(
    private val scope: CoroutineScope,
    // The caller publishes a fresh, ordered post-commit read before returning.
    private val execute: suspend (DocumentId, DocumentCommand.EditBlockText) -> Unit,
) {
    private val mutableDrafts = MutableStateFlow<Map<BlockEditKey, BlockDraft>>(emptyMap())
    val drafts = mutableDrafts.asStateFlow()
    private val writers = mutableSetOf<BlockEditKey>()
    private val idleSaves = mutableMapOf<DocumentId, Job>()
    private val maximumSaves = mutableMapOf<DocumentId, Job>()
    private val ready = mutableSetOf<BlockEditKey>()
    private val mutableParagraphs = MutableStateFlow<Map<BlockEditKey, PendingParagraph>>(emptyMap())
    val paragraphs = mutableParagraphs.asStateFlow()
    private val blockedBy = mutableMapOf<BlockEditKey, BlockEditKey>()

    fun enter(key: BlockEditKey, block: Block, before: String, text: String, hasChildren: Boolean, createParagraph: Boolean): BlockId {
        val lines = text.split('\n')
        require(lines.size > 1)
        var currentKey = key
        var currentBlock = block
        var currentBefore = before
        var next = block.id
        lines.dropLast(1).forEachIndexed { index, line ->
            val joined = lines.drop(index).joinToString("")
            next = split(currentKey, currentBlock, currentBefore, joined, line.length,
                index == 0 && hasChildren, index == 0 && createParagraph)
            currentKey = BlockEditKey(key.document, next)
            currentBlock = mutableParagraphs.value.getValue(currentKey).block
            currentBefore = lines.drop(index + 1).joinToString("")
        }
        return next
    }

    /** Enter stages the next row immediately; its writes wait for the split
     * that creates it. Repeated Enter can safely build a chain while JNI runs. */
    fun split(key: BlockEditKey, block: Block, before: String, text: String, at: Int, hasChildren: Boolean, createParagraph: Boolean): BlockId {
        val rich = block.content.editableText()!!.copy(text = before)
        change(key, rich, text, createParagraph)
        val current = mutableDrafts.value[key] ?: BlockDraft(rich, text, createParagraph)
        val id = BlockId.new()
        val childKey = BlockEditKey(key.document, id)
        val tail = current.base.withDraftText(text).slice(at, text.length)
        val next = block.copy(id = id, parentId = if (hasChildren) block.id else block.parentId,
            properties = BlockProperties(headingLevel = block.properties.headingLevel.takeIf { at < text.length }),
            content = block.content.withText(tail))
        mutableParagraphs.value += childKey to PendingParagraph(block.id, next)
        blockedBy[childKey] = key
        put(childKey, BlockDraft(tail, tail.text, saving = true))
        put(key, current.copy(text = text, saving = current.error == null,
            split = BlockTextSplit(id, text.substring(0, at).encodeToByteArray().size)))
        if (current.error == null) { ready.add(key); save(key) }
        return id
    }

    fun change(key: BlockEditKey, rich: RichText, text: String, createParagraph: Boolean = false) {
        val draft = mutableDrafts.value[key] ?: BlockDraft(rich, rich.text, createParagraph)
        if (draft.text == text) return
        put(key, draft.copy(text = text, saving = draft.error == null))
        if (draft.error == null) {
            // Cancel only waiting timers, never a write already admitted to JNI.
            // Once a deadline/explicit flush admits a pending save, later
            // typing must not withdraw it while the previous write finishes.
            idleSaves.remove(key.document)?.cancel()
            idleSaves[key.document] = scope.launch {
                delay(IDLE_SAVE_MILLIS)
                idleSaves.remove(key.document)
                flush(key.document)
            }
            maximumSaves.getOrPut(key.document) {
                scope.launch {
                    delay(MAX_SAVE_WAIT_MILLIS)
                    maximumSaves.remove(key.document)
                    flush(key.document)
                }
            }
        }
    }

    private fun cancelTimers(document: DocumentId) {
        idleSaves.remove(document)?.cancel()
        maximumSaves.remove(document)?.cancel()
    }

    /** Done, navigation and backgrounding spend pending work without waiting. */
    fun flush(document: DocumentId) {
        cancelTimers(document)
        mutableDrafts.value.filter { (key, draft) -> key.document == document && draft.error == null }
            .keys.forEach { key -> ready.add(key); save(key) }
    }

    fun retry(key: BlockEditKey) {
        val draft = mutableDrafts.value[key] ?: return
        put(key, draft.copy(error = null, saving = true))
        ready.add(key)
        save(key)
    }

    fun discard(key: BlockEditKey) {
        if (key !in writers) {
            val removed = mutableSetOf(key)
            while (removed.addAll(blockedBy.filterValues { it in removed }.keys)) { }
            removed.forEach { ready.remove(it); blockedBy.remove(it) }
            mutableDrafts.value -= removed
            mutableParagraphs.value -= removed
            if (mutableDrafts.value.keys.none { it.document == key.document }) cancelTimers(key.document)
        }
    }

    fun copyDraft(key: BlockEditKey): String {
        val draft = mutableDrafts.value[key] ?: return ""
        return draft.displayedText + (draft.split?.let { "\n" + copyDraft(BlockEditKey(key.document, it.blockId)) } ?: "")
    }

    private fun put(key: BlockEditKey, draft: BlockDraft) { mutableDrafts.value += key to draft }

    private fun save(key: BlockEditKey) {
        if (key in blockedBy || key !in ready || !writers.add(key)) return
        put(key, mutableDrafts.value.getValue(key).copy(saving = true))
        scope.launch {
            try {
                // Finish staging a multi-paragraph input before publishing any acknowledgement.
                yield()
                while (ready.remove(key)) {
                    val sending = mutableDrafts.value[key] ?: break
                    if (sending.split == null && !sending.createParagraph && sending.base.text == sending.text) {
                        mutableDrafts.value -= key
                        ready.remove(key)
                        break
                    }
                    execute(key.document, DocumentCommand.EditBlockText(key.block,
                        sending.base.text, sending.text, sending.createParagraph, sending.split))
                    val latest = mutableDrafts.value.getValue(key)
                    if (sending.split != null) {
                        val child = BlockEditKey(key.document, sending.split.blockId)
                        mutableDrafts.value -= key
                        ready.remove(key)
                        blockedBy.remove(child)
                        mutableParagraphs.value -= child
                        val childDraft = mutableDrafts.value.getValue(child)
                        if (childDraft.split == null && childDraft.text == childDraft.base.text) {
                            mutableDrafts.value -= child
                            ready.remove(child)
                        } else {
                            if (childDraft.split != null) ready.add(child)
                            save(child)
                        }
                        break
                    }
                    if (latest.text == sending.text && latest.split == null) {
                        // The caller publishes this post-commit read before
                        // releasing the draft. A peer edit that already followed
                        // our commit is now the visible document too.
                        mutableDrafts.value -= key
                        ready.remove(key)
                        break
                    }
                    put(key, latest.copy(base = sending.base.copy(text = sending.text), createParagraph = false))
                }
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (failure: Exception) {
                ready.remove(key)
                mutableDrafts.value[key]?.let { put(key, it.copy(error = failure.message ?: "edit_failed")) }
            } finally {
                writers.remove(key)
                // A newer draft may still be waiting for its batching deadline.
                mutableDrafts.value[key]?.let { put(key, it.copy(saving = it.error == null)) }
                if (mutableDrafts.value.keys.none { it.document == key.document }) cancelTimers(key.document)
            }
        }
    }

    companion object {
        const val IDLE_SAVE_MILLIS = 250L
        // A scheduling bound, not a disk/network deadline. Slow writes remain
        // owned and serialized; the latest draft follows their acknowledgement.
        const val MAX_SAVE_WAIT_MILLIS = 500L
    }
}

fun BlockContent.editableText(): RichText? = when (this) {
    is BlockContent.Paragraph -> richText
    is BlockContent.Quote -> richText
    is BlockContent.Task -> richText
    is BlockContent.Code -> richText
    else -> null
}

fun BlockContent.withText(text: RichText): BlockContent = when (this) {
    is BlockContent.Paragraph -> BlockContent.Paragraph(text)
    is BlockContent.Quote -> BlockContent.Quote(text)
    is BlockContent.Task -> BlockContent.Task(text)
    is BlockContent.Code -> BlockContent.Code(text)
    else -> error("block has no editable text")
}

fun withPendingParagraphs(blocks: List<Block>, pending: List<PendingParagraph>): List<Block> {
    val result = blocks.toMutableList()
    pending.forEach { paragraph ->
        if (result.none { it.id == paragraph.block.id }) {
            val at = result.indexOfFirst { it.id == paragraph.after }
            if (at >= 0) result.add(at + 1, paragraph.block)
        }
    }
    return result
}
