// SPDX-License-Identifier: MPL-2.0

package app.koru.mobile.services

import app.koru.mobile.domain.*
import kotlinx.coroutines.async
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.test.*
import org.junit.Assert.*
import org.junit.Test

@OptIn(kotlinx.coroutines.ExperimentalCoroutinesApi::class)
class DocumentEditsTest {
    private val document = DocumentId("10000000-0000-4000-8000-000000000001")
    private val block = BlockId("20000000-0000-4000-8000-000000000001")
    private val key = BlockEditKey(document, block)
    private fun snapshot(text: String) = DocumentSnapshot(document,
        WorkspaceId("30000000-0000-4000-8000-000000000001"), "Test", 1,
        listOf(Block(block, content = BlockContent.Paragraph(RichText(text)))))

    @Test fun queuedEnterAndSplitAfterPurgeFreezeDoNotCreateDraftsOrThrow() = runTest {
        var writes = 0
        val edits = DocumentEdits(this) { _, _ -> writes++ }
        val paragraph = Block(block, content = BlockContent.Paragraph(RichText("Before")))
        assertTrue(edits.quiesce(document).isEmpty())
        assertEquals(block, edits.enter(key, paragraph, "Before", "Before\nafter", false, false))
        assertEquals(block, edits.split(key, paragraph, "Before", "Before after", 6, false, false))
        runCurrent()
        assertTrue(edits.drafts.value.isEmpty())
        assertTrue(edits.paragraphs.value.isEmpty())
        assertEquals(0, writes)
    }

    @Test fun purgeQuiescenceJoinsAnAdmittedWriteAndRetainsRejectedText() = runTest {
        val gate = CompletableDeferred<Unit>()
        var calls = 0
        val edits = DocumentEdits(this) { _, _ -> calls++; gate.await(); error("document_removed") }
        edits.change(key, RichText("Before"), "Typed here")
        edits.flush(document)
        runCurrent()
        val pending = async { edits.quiesce(document) }
        runCurrent()
        assertFalse(pending.isCompleted)
        edits.change(key, RichText("Before"), "Too late")
        edits.flush(document)
        gate.complete(Unit)
        runCurrent()
        assertEquals(1, calls)
        assertEquals("Typed here", pending.await().single().after)
        assertEquals("Typed here", edits.drafts.value.getValue(key).text)
        edits.transferred(document)
        assertTrue(edits.drafts.value.isEmpty())
    }

    @Test fun purgeQuiescenceKeepsAnUnwrittenEnterChainInDependencyOrder() = runTest {
        val edits = DocumentEdits(this) { _, _ -> error("must not write a frozen source") }
        val paragraph = Block(block, content = BlockContent.Paragraph(RichText("Before")))
        val child = edits.split(key, paragraph, "Before", "First tail", 6, false, false)
        edits.change(BlockEditKey(document, child), RichText("tail"), "Retained tail")
        val pending = edits.quiesce(document)
        assertEquals(listOf(block, child), pending.map { it.blockId })
        assertEquals(child, pending[0].split?.blockId)
        assertEquals("Retained tail", pending[1].after)
    }

    @Test fun typingCoalescesBehindDurabilityWithoutChangingItsBase() = runTest {
        val gate = CompletableDeferred<Unit>()
        val calls = mutableListOf<DocumentCommand.EditBlockText>()
        val edits = DocumentEdits(this) { _, edit -> calls += edit; if (calls.size == 1) gate.await() }
        edits.change(key, RichText("a"), "ab")
        edits.flush(document)
        runCurrent()
        edits.change(key, RichText("a"), "abc")
        edits.change(key, RichText("incoming peer text"), "abcd")
        assertEquals("abcd", edits.drafts.value.getValue(key).text)
        assertTrue(edits.drafts.value.getValue(key).saving)
        edits.flush(document)
        gate.complete(Unit)
        runCurrent()
        assertEquals(listOf("a" to "ab", "ab" to "abcd"), calls.map { it.before to it.after })
        assertTrue(edits.drafts.value.isEmpty())
    }

    @Test fun conflictRetainsTextAndStopsFurtherWritesUntilRetry() = runTest {
        var calls = 0
        var fail = true
        val edits = DocumentEdits(this) { _, _ -> calls++; if (fail) error("text_edit_conflict") }
        edits.change(key, RichText("old"), "draft")
        edits.flush(document)
        runCurrent()
        edits.change(key, RichText("peer edit"), "longer draft")
        runCurrent()
        assertEquals(1, calls)
        assertEquals("old", edits.drafts.value.getValue(key).base.text)
        assertEquals("longer draft", edits.drafts.value.getValue(key).text)
        assertEquals("text_edit_conflict", edits.drafts.value.getValue(key).error)
        fail = false
        edits.retry(key)
        runCurrent()
        assertEquals(2, calls)
        assertTrue(edits.drafts.value.isEmpty())
    }

    @Test fun admittedWriteKeepsItsDocumentAndCreationIsSentOnlyOnce() = runTest {
        val gate = CompletableDeferred<Unit>()
        val calls = mutableListOf<Pair<DocumentId, DocumentCommand.EditBlockText>>()
        val edits = DocumentEdits(this) { id, edit -> calls += id to edit; if (calls.size == 1) gate.await() }
        edits.change(key, RichText(""), "Hello", createParagraph = true)
        edits.flush(document)
        runCurrent()
        edits.change(key, RichText(""), "Hello tablet", createParagraph = true)
        edits.flush(document)
        gate.complete(Unit)
        runCurrent()
        assertTrue(calls.all { it.first == document })
        assertEquals(listOf(true, false), calls.map { it.second.createParagraph })
        assertTrue(edits.drafts.value.isEmpty())
    }

    @Test fun explicitDiscardDoesNotCancelAnInFlightSave() = runTest {
        val gate = CompletableDeferred<Unit>()
        val edits = DocumentEdits(this) { _, _ -> gate.await(); error("disk_full") }
        edits.change(key, RichText("a"), "b")
        edits.flush(document)
        runCurrent()
        edits.discard(key)
        assertTrue(edits.drafts.value.containsKey(key))
        gate.complete(Unit)
        runCurrent()
        edits.discard(key)
        assertTrue(edits.drafts.value.isEmpty())
    }

    @Test fun nativeSpliceDoesNotSplitEmojiOrSurrogateSuffixes() {
        for ((before, after) in listOf("a😀z" to "a😄z", "😀" to "😁", "é你好" to "é!你好", "" to "👋")) {
            val splice = textSplice(before, after)
            assertEquals(after, before.replaceRange(splice.start, splice.end, splice.inserted))
            assertFalse(splice.inserted.firstOrNull()?.isLowSurrogate() == true)
        }
    }

    @Test fun postCommitReadReleasesDraftEvenWhenAPeerHasAlreadyEditedAgain() = runTest {
        var published: DocumentSnapshot? = null
        val edits = DocumentEdits(this) { _, _ -> snapshot("peer followed our edit").also { published = it } }
        edits.change(key, RichText("before"), "our edit")
        edits.flush(document)
        runCurrent()
        assertEquals("peer followed our edit", published!!.blocks.single().content.editableText()!!.text)
        assertTrue(edits.drafts.value.isEmpty())
    }

    @Test fun aTypingBurstWritesOnlyAfterTheLastKeystrokesIdleDelay() = runTest {
        val calls = mutableListOf<DocumentCommand.EditBlockText>()
        val edits = DocumentEdits(this) { _, edit -> calls += edit }
        repeat(3) { index ->
            edits.change(key, RichText(""), "x".repeat(index + 1))
            advanceTimeBy(100)
            runCurrent()
            assertTrue(calls.isEmpty())
        }
        advanceTimeBy(DocumentEdits.IDLE_SAVE_MILLIS - 101)
        runCurrent()
        assertTrue(calls.isEmpty())
        advanceTimeBy(1)
        runCurrent()
        assertEquals(listOf("" to "xxx"), calls.map { it.before to it.after })
        assertTrue(edits.drafts.value.isEmpty())
    }

    @Test fun typingInAnotherBlockResetsTheWholeDocumentsIdleDelay() = runTest {
        val calls = mutableListOf<DocumentCommand.EditBlockText>()
        val edits = DocumentEdits(this) { _, edit -> calls += edit }
        val second = BlockEditKey(document, BlockId("20000000-0000-4000-8000-000000000002"))
        edits.change(key, RichText("a"), "ab")
        advanceTimeBy(100)
        edits.change(second, RichText("c"), "cd")
        advanceTimeBy(DocumentEdits.IDLE_SAVE_MILLIS - 1)
        runCurrent()
        assertTrue(calls.isEmpty())
        advanceTimeBy(1)
        runCurrent()
        assertEquals(setOf(block, second.block), calls.map { it.blockId }.toSet())
        assertTrue(edits.drafts.value.isEmpty())
    }

    @Test fun continuousTypingSavesEveryMaximumWindowWithoutWaitingForAPause() = runTest {
        val calls = mutableListOf<Pair<Long, DocumentCommand.EditBlockText>>()
        val edits = DocumentEdits(this) { _, edit -> calls += testScheduler.currentTime to edit }
        repeat(20) { index ->
            edits.change(key, RichText(calls.lastOrNull()?.second?.after.orEmpty()), "x".repeat(index + 1))
            advanceTimeBy(100)
            runCurrent()
        }
        assertEquals(listOf(500L, 1000L, 1500L, 2000L), calls.map { it.first })
        assertEquals(listOf(5, 10, 15, 20), calls.map { it.second.after.length })
        assertEquals(listOf(0, 5, 10, 15), calls.map { it.second.before.length })
        assertTrue(edits.drafts.value.isEmpty())
    }

    @Test fun laterTypingCannotWithdrawADeadlineSaveBehindASlowWrite() = runTest {
        val gate = CompletableDeferred<Unit>()
        val calls = mutableListOf<DocumentCommand.EditBlockText>()
        val edits = DocumentEdits(this) { _, edit -> calls += edit; if (calls.size == 1) gate.await() }
        edits.change(key, RichText("a"), "ab")
        edits.flush(document)
        runCurrent()
        repeat(5) { index ->
            edits.change(key, RichText("a"), "abc" + "x".repeat(index))
            advanceTimeBy(100)
            runCurrent()
        }
        assertEquals(1, calls.size)
        edits.change(key, RichText("a"), "latest")
        gate.complete(Unit)
        runCurrent()
        assertEquals(listOf("a" to "ab", "ab" to "latest"), calls.map { it.before to it.after })
        advanceTimeBy(1000)
        runCurrent()
        assertEquals(2, calls.size)
    }

    @Test fun documentsHaveIndependentBatchingDeadlines() = runTest {
        val other = DocumentId("10000000-0000-4000-8000-000000000002")
        val calls = mutableListOf<Pair<Long, DocumentId>>()
        val edits = DocumentEdits(this) { id, _ -> calls += testScheduler.currentTime to id }
        edits.change(key, RichText("a"), "ab")
        advanceTimeBy(100)
        edits.change(BlockEditKey(other, block), RichText("c"), "cd")
        advanceTimeBy(150)
        runCurrent()
        assertEquals(listOf(250L to document), calls)
        advanceTimeBy(100)
        runCurrent()
        assertEquals(listOf(250L to document, 350L to other), calls)
    }

    @Test fun aDeadlineFailureKeepsTheLatestDraftAndDoesNotRetryOnATimer() = runTest {
        var calls = 0
        val edits = DocumentEdits(this) { _, _ -> calls++; error("disk_full") }
        repeat(5) { index ->
            edits.change(key, RichText("a"), "a" + "x".repeat(index + 1))
            advanceTimeBy(100)
            runCurrent()
        }
        edits.change(key, RichText("a"), "retained")
        advanceTimeBy(2000)
        runCurrent()
        assertEquals(1, calls)
        assertEquals("retained", edits.drafts.value.getValue(key).text)
        assertEquals("a", edits.drafts.value.getValue(key).base.text)
        assertEquals("disk_full", edits.drafts.value.getValue(key).error)
    }

    @Test fun typingDuringAWriteWaitsForAnotherPauseAndNeverCancelsTheWrite() = runTest {
        val gate = CompletableDeferred<Unit>()
        val calls = mutableListOf<DocumentCommand.EditBlockText>()
        val edits = DocumentEdits(this) { _, edit -> calls += edit; if (calls.size == 1) gate.await() }
        edits.change(key, RichText("a"), "ab")
        advanceTimeBy(DocumentEdits.IDLE_SAVE_MILLIS)
        runCurrent()
        assertEquals(1, calls.size)
        edits.change(key, RichText("a"), "abc")
        gate.complete(Unit)
        runCurrent()
        assertEquals(1, calls.size)
        assertEquals("ab", edits.drafts.value.getValue(key).base.text)
        assertTrue(edits.drafts.value.getValue(key).saving)
        advanceTimeBy(DocumentEdits.IDLE_SAVE_MILLIS)
        runCurrent()
        assertEquals(listOf("a" to "ab", "ab" to "abc"), calls.map { it.before to it.after })
        assertTrue(edits.drafts.value.isEmpty())
    }

    @Test fun doneOrLeavingFlushesImmediatelyAndDoesNotLeaveADuplicateTimerSave() = runTest {
        val calls = mutableListOf<DocumentCommand.EditBlockText>()
        val edits = DocumentEdits(this) { _, edit -> calls += edit }
        edits.change(key, RichText("a"), "ab")
        edits.flush(document)
        runCurrent()
        assertEquals(1, calls.size)
        advanceTimeBy(5_000)
        runCurrent()
        assertEquals(1, calls.size)
        assertTrue(edits.drafts.value.isEmpty())
    }

    @Test fun enterShowsTheTailImmediatelyAndItsTypingWaitsForDurableCreation() = runTest {
        val gate = CompletableDeferred<Unit>()
        val calls = mutableListOf<DocumentCommand.EditBlockText>()
        val edits = DocumentEdits(this) { _, edit -> calls += edit; if (calls.size == 1) gate.await() }
        val original = snapshot("Hello 😀tail").blocks.single()
        val id = edits.enter(key, original, "Hello 😀tail", "Hello 😀\ntail", false, false)
        val child = BlockEditKey(document, id)
        assertEquals("Hello 😀", edits.drafts.value.getValue(key).displayedText)
        assertEquals("tail", edits.drafts.value.getValue(child).text)
        assertEquals(listOf(block, id), withPendingParagraphs(listOf(original), edits.paragraphs.value.values.toList()).map { it.id })
        runCurrent()
        assertEquals("Hello 😀".encodeToByteArray().size, calls.single().split!!.atUtf8)
        edits.change(child, RichText("tail"), "tail typed")
        advanceTimeBy(DocumentEdits.IDLE_SAVE_MILLIS)
        runCurrent()
        assertEquals(1, calls.size)
        gate.complete(Unit)
        runCurrent()
        assertEquals(listOf(block, id), calls.map { it.blockId })
        assertEquals("tail" to "tail typed", calls.last().before to calls.last().after)
        assertFalse(calls.last().createParagraph)
        assertTrue(edits.drafts.value.isEmpty())
        assertTrue(edits.paragraphs.value.isEmpty())
    }

    @Test fun splitQueuedDuringAnEarlierTextWriteUsesItsAcknowledgedBase() = runTest {
        val gate = CompletableDeferred<Unit>()
        val calls = mutableListOf<DocumentCommand.EditBlockText>()
        val edits = DocumentEdits(this) { _, edit -> calls += edit; if (calls.size == 1) gate.await() }
        edits.change(key, RichText("a"), "ab")
        edits.flush(document)
        runCurrent()
        edits.enter(key, snapshot("a").blocks.single(), "abc", "abc\n", false, false)
        gate.complete(Unit)
        runCurrent()
        assertEquals(listOf("a" to "ab", "ab" to "abc"), calls.map { it.before to it.after })
        assertNotNull(calls.last().split)
        assertTrue(edits.drafts.value.isEmpty())
    }

    @Test fun repeatedEnterQueuesChildrenInOrderWithoutPerCharacterWrites() = runTest {
        val gate = CompletableDeferred<Unit>()
        val calls = mutableListOf<DocumentCommand.EditBlockText>()
        val edits = DocumentEdits(this) { _, edit -> calls += edit; if (calls.size == 1) gate.await() }
        val first = edits.enter(key, snapshot("one").blocks.single(), "one", "one\n", false, false)
        runCurrent()
        val firstKey = BlockEditKey(document, first)
        edits.change(firstKey, RichText(""), "two")
        val second = edits.enter(firstKey, edits.paragraphs.value.getValue(firstKey).block, "two", "two\n", false, false)
        val secondKey = BlockEditKey(document, second)
        edits.change(secondKey, RichText(""), "three")
        assertEquals("one\ntwo\nthree", edits.copyDraft(key))
        gate.complete(Unit)
        runCurrent()
        assertEquals(listOf(block, first), calls.map { it.blockId })
        assertEquals("two", calls.last().after)
        assertEquals("three", edits.drafts.value.getValue(secondKey).text)
        advanceTimeBy(DocumentEdits.IDLE_SAVE_MILLIS)
        runCurrent()
        assertEquals(listOf(block, first, second), calls.map { it.blockId })
        assertTrue(edits.drafts.value.isEmpty())
    }

    @Test fun failedSplitKeepsEveryNewParagraphAndRetryReusesTheirIds() = runTest {
        var fail = true
        val calls = mutableListOf<DocumentCommand.EditBlockText>()
        val edits = DocumentEdits(this) { _, edit -> calls += edit; if (fail) error("disk_full") }
        val id = edits.enter(key, snapshot("headtail").blocks.single(), "headtail", "head\ntail", false, false)
        val child = BlockEditKey(document, id)
        edits.change(child, RichText("tail"), "tail typed")
        runCurrent()
        assertEquals("head\ntail typed", edits.copyDraft(key))
        assertEquals("disk_full", edits.drafts.value.getValue(key).error)
        fail = false
        edits.retry(key)
        edits.flush(document)
        runCurrent()
        assertEquals(id, calls[0].split!!.blockId)
        assertEquals(id, calls[1].split!!.blockId)
        assertEquals(id, calls.last().blockId)
        assertTrue(edits.drafts.value.isEmpty())
    }

    @Test fun explicitDiscardOfAFailedSplitRemovesOnlyItsDependentDrafts() = runTest {
        val edits = DocumentEdits(this) { _, _ -> error("disk_full") }
        edits.enter(key, snapshot("ab").blocks.single(), "ab", "a\nb\nc", false, false)
        runCurrent()
        assertEquals("a\nb\nc", edits.copyDraft(key))
        edits.discard(key)
        assertTrue(edits.drafts.value.isEmpty())
        assertTrue(edits.paragraphs.value.isEmpty())
    }

    @Test fun multilinePasteAndEnterOnABlankRowCreateRealOrderedParagraphs() = runTest {
        val calls = mutableListOf<DocumentCommand.EditBlockText>()
        val edits = DocumentEdits(this) { _, edit -> calls += edit }
        val last = edits.enter(key, snapshot("").blocks.single(), "", "one\n\nthree\n", false, true)
        runCurrent()
        assertEquals(3, calls.size)
        assertTrue(calls.first().createParagraph)
        assertTrue(calls.drop(1).none { it.createParagraph })
        assertEquals(listOf(3, 0, 5), calls.map { it.split!!.atUtf8 })
        assertEquals(last, calls.last().split!!.blockId)
        assertTrue(edits.drafts.value.isEmpty())
    }
}
