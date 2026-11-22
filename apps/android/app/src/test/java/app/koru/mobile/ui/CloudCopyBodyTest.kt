// SPDX-License-Identifier: MPL-2.0
package app.koru.mobile.ui

import androidx.compose.ui.graphics.Color
import app.koru.mobile.domain.*
import app.koru.mobile.ui.editor.annotatedRichText
import app.koru.mobile.ui.editor.passiveBlockLayouts
import org.junit.Assert.*
import org.junit.Test

class CloudCopyBodyTest {
    private fun block(id: String, parent: String? = null, content: BlockContent = BlockContent.Paragraph(RichText(id))) =
        Block(BlockId(id), parent?.let(::BlockId), content = content)

    @Test fun indexesNestedListsAndCalloutsWithoutChangingContent() {
        val blocks = listOf(
            block("box", content = BlockContent.Callout),
            block("list", "box", BlockContent.ListContainer(ListStyle.Numbered)),
            block("first", "list"), block("second", "list"),
            block("nested", "second"), block("outside"),
        )
        val rows = passiveBlockLayouts(blocks)!!
        assertEquals(listOf("first", "second", "nested", "outside"), rows.map { it.block.id.value })
        assertEquals(listOf(1, 1, 2, 0), rows.map { it.depth })
        assertEquals(listOf(1, 2, 1, 2), rows.map { it.ordinal })
        assertEquals(listOf(true, false, false, false), rows.map { it.calloutHead })
        assertEquals(BlockId("box"), rows[2].callout?.id)
        assertEquals(ListStyle.Numbered, rows[0].listParent?.style)
        assertSame(blocks[2], rows[0].block)
    }

    @Test fun malformedOrOversizedHierarchyRefusesWholeBody() {
        assertNull(passiveBlockLayouts(listOf(block("a", "missing"))))
        assertNull(passiveBlockLayouts(listOf(block("a"), block("a"))))
        assertNull(passiveBlockLayouts(listOf(block("a", "b"), block("b", "a"))))
        assertNull(passiveBlockLayouts((0..2000).map { block("$it") }))
        assertEquals(2000, passiveBlockLayouts((0 until 2000).map { block("$it", if (it == 0) null else "${it - 1}") })!!.size)
    }

    @Test fun passiveRichTextPreservesLabelsAndMarksWithoutNavigationAnnotations() {
        val rich = RichText(
            "A\uFFFCB",
            marks = listOf(TextMark(TextRange(0, 1), MarkKind.Bold)),
            inlineObjects = listOf(InlineObject(InlineObjectId("link"), 1, "link", EntityReference.Url("https://example.test"), "stored label")),
        )
        val passive = annotatedRichText(rich, Color.Blue, { it.label!! }, activeReferences = false)
        assertEquals("Astored labelB", passive.text)
        assertTrue(passive.getStringAnnotations(0, passive.length).isEmpty())
        assertEquals(1, passive.spanStyles.size)
        val normal = annotatedRichText(rich, Color.Blue, { it.label!! })
        assertEquals(1, normal.getStringAnnotations(0, normal.length).size)
        assertEquals(passive.text, normal.text)
    }
}
