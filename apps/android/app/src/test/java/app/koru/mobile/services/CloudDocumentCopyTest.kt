// SPDX-License-Identifier: MPL-2.0
package app.koru.mobile.services

import app.koru.mobile.domain.BlockId
import app.koru.mobile.ui.editor.CloudCopyPhase
import app.koru.mobile.ui.editor.CloudCopyViewKey
import app.koru.mobile.ui.editor.CloudCopyConfirmation
import app.koru.mobile.ui.editor.cloudCopyRemovableNodes
import app.koru.mobile.ui.editor.cloudCopyRemovalEnabled
import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Test
import javax.xml.parsers.DocumentBuilderFactory

class CloudDocumentCopyTest {
    private fun id(n: Int) = "00000000-0000-0000-0000-${n.toString().padStart(12, '0')}"
    private val selection = CloudCopySelection(id(1), id(2), id(3), id(4), id(5), "a".repeat(64))
    private fun block(n: Int, kind: String = "file") = JSONObject().apply {
        put("id", id(n)); put("parent_id", JSONObject.NULL); put("kind", kind)
        put("in_trash", kind == "file")
        put("properties", JSONObject().apply {
            listOf("name", "icon", "color", "heading_level", "list_style", "image_width", "image_height", "language").forEach { put(it, JSONObject.NULL) }
            put("alignment", "start"); put("done", false)
        })
        if (kind == "file") put("blob", JSONObject().apply {
            put("id", "hash"); put("mime_type", "application/octet-stream")
            put("size", JSONObject.NULL); put("width", JSONObject.NULL); put("height", JSONObject.NULL)
        }) else put("rich_text", JSONObject().apply {
            put("text", "é😀"); put("marks", JSONArray()); put("inline_objects", JSONArray())
        })
    }
    private fun fixture() = JSONObject().apply {
        put("selection", JSONObject(selection.json()))
        put("document", JSONObject().apply {
            put("id", selection.documentId); put("workspace_id", selection.workspaceId)
            put("title", "Cloud title"); put("revision", 9_007_199_254_740_993L)
            put("blocks", JSONArray().put(block(6)).put(block(7, "paragraph")))
        })
        put("can_request_removal", true)
        put("removable_files", JSONArray().put(JSONObject().put("node_id", id(6)).put("in_trash", true)))
    }
    private fun decode(value: JSONObject) = CloudDocumentCopyDecoder.decode(value.toString(), selection)

    @Test fun exactCopyPreservesUnknownFileSizeAndNeverCreatesGlobalBlobHandle() {
        val copy = decode(fixture())!!
        assertEquals(selection, copy.selection)
        assertEquals(9_007_199_254_740_993L, copy.document.revision)
        assertNull(copy.files[BlockId(id(6))]!!.sizeDecimal)
        assertEquals(setOf(BlockId(id(6))), copy.permittedNodes)
        assertEquals(setOf(BlockId(id(6))), copy.trashedNodes)
        assertTrue(copy.document.blocks[0].content is app.koru.mobile.domain.BlockContent.Unsupported)
        assertFalse(copy.toString().contains("Cloud title"))
        val exact = fixture()
        exact.getJSONObject("document").getJSONArray("blocks").getJSONObject(0).getJSONObject("blob").put("size", 9_007_199_254_740_993L)
        assertEquals("9007199254740993", decode(exact)!!.files[BlockId(id(6))]!!.sizeDecimal)
        val canvas = fixture(); canvas.getJSONObject("document").getJSONArray("blocks").getJSONObject(0).put("kind", "canvas")
        assertEquals(setOf(BlockId(id(6))), decode(canvas)!!.permittedNodes)
    }

    @Test fun everySelectionDimensionAndDocumentScopeAreBound() {
        for (key in listOf("account_id", "workspace_id", "incarnation_id", "cloud_unit_id", "document_id", "source_head_digest")) {
            val changed = fixture()
            changed.getJSONObject("selection").put(key, if (key == "source_head_digest") "b".repeat(64) else id(99))
            assertNull(key, decode(changed))
        }
        val changed = fixture(); changed.getJSONObject("document").put("workspace_id", id(99))
        assertNull(decode(changed))
        assertNull(CloudDocumentCopyDecoder.decode(fixture().toString(), selection.copy(accountId = "bad")))
    }

    @Test fun onlyNativePermittedLeavesOfferRemovalAndUnknownOutcomesCannotRetryWrite() {
        val copy = decode(fixture())!!
        val removable = cloudCopyRemovableNodes(copy.document, copy.permittedNodes)
        assertTrue(cloudCopyRemovalEnabled(CloudCopyPhase.Verified, removable, BlockId(id(6))))
        for (phase in CloudCopyPhase.entries.filter { it != CloudCopyPhase.Verified })
            assertFalse(cloudCopyRemovalEnabled(phase, removable, BlockId(id(6))))
        assertFalse(cloudCopyRemovalEnabled(CloudCopyPhase.Verified, removable, BlockId(id(7))))
        val readOnly = fixture().put("can_request_removal", false)
        assertTrue(decode(readOnly)!!.permittedNodes.isEmpty())
        val subtree = fixture()
        subtree.getJSONObject("document").getJSONArray("blocks").getJSONObject(1).put("parent_id", id(6))
        assertNull(decode(subtree))
        val note = fixture(); note.getJSONArray("removable_files").getJSONObject(0).put("node_id", id(7))
        assertNull(decode(note))
    }

    @Test fun malformedHierarchyNumbersTypesAndRangesRefuseWholeCopy() {
        val duplicate = fixture(); duplicate.getJSONObject("document").getJSONArray("blocks").getJSONObject(1).put("id", id(6))
        assertNull(decode(duplicate))
        val orphan = fixture(); orphan.getJSONObject("document").getJSONArray("blocks").getJSONObject(1).put("parent_id", id(99))
        assertNull(decode(orphan))
        assertNull(decode(fixture().put("can_request_removal", "true")))
        assertNull(decode(fixture().put("unexpected", true)))
        val number = fixture(); number.getJSONObject("document").put("revision", 1.5)
        assertNull(decode(number))
        val range = fixture()
        range.getJSONObject("document").getJSONArray("blocks").getJSONObject(1).getJSONObject("rich_text")
            .getJSONArray("marks").put(JSONObject().put("start_utf8", 1).put("end_utf8", 2).put("kind", "bold"))
        assertNull(decode(range))
    }

    @Test fun resourceBoundsRejectInsteadOfTruncatingOrShowingPartialContent() {
        val text = fixture(); text.getJSONObject("document").put("title", "x".repeat(262_145))
        assertNull(decode(text))
        val many = fixture(); many.getJSONObject("document").put("blocks", JSONArray().apply { repeat(2_001) { put(block(it + 100, "paragraph")) } })
        assertNull(decode(many))
        val marks = fixture()
        marks.getJSONObject("document").getJSONArray("blocks").getJSONObject(1).getJSONObject("rich_text")
            .put("marks", JSONArray().apply { repeat(8_001) { put(JSONObject().put("start_utf8", 0).put("end_utf8", 2).put("kind", "bold")) } })
        assertNull(decode(marks))
        assertNull(CloudDocumentCopyDecoder.decode(" ".repeat(1_048_577), selection))
    }

    @Test fun structuralRootsAndTrashDescendantsRemainExplicit() {
        val value = fixture()
        val root = block(8, "document")
        val trash = block(9, "trash").put("in_trash", true)
        val file = block(6).put("parent_id", id(9))
        value.getJSONObject("document").put("blocks", JSONArray().put(root).put(trash).put(file))
        val copy = decode(value)!!
        assertEquals(3, copy.document.blocks.size)
        assertEquals(CloudCopyStructure.Document, copy.structure[BlockId(id(8))])
        assertEquals(CloudCopyStructure.Trash, copy.structure[BlockId(id(9))])
        assertTrue(BlockId(id(6)) in copy.trashedNodes)
        val rows = app.koru.mobile.ui.editor.passiveBlockLayouts(copy.document.blocks, copy.structure.keys)!!
        assertEquals(0, rows.last().depth)
        value.getJSONArray("removable_files").getJSONObject(0).put("in_trash", false)
        assertNull(decode(value))
    }

    @Test fun passiveCopyResourcesCoverEveryLocaleWithNeutralReferenceLabels() {
        val locales = listOf("values", "values-ar", "values-de", "values-es", "values-fr", "values-ja", "values-pt-rBR", "values-zh-rCN")
        val rows = locales.map { locale ->
            val nodes = javaClass.classLoader!!.getResourceAsStream("$locale/cloud_copy.xml")!!.use {
                DocumentBuilderFactory.newInstance().newDocumentBuilder().parse(it)
            }.getElementsByTagName("string")
            (0 until nodes.length).associate { nodes.item(it).attributes.getNamedItem("name").nodeValue to nodes.item(it).textContent }
        }
        for (row in rows) {
            assertEquals(rows.first().keys, row.keys)
            assertTrue(row.values.all(String::isNotBlank))
            assertFalse(row.getValue("cloud_copy_block_reference").contains("Insert"))
        }
        assertEquals("Block reference", rows.first().getValue("cloud_copy_block_reference"))
    }

    @Test fun confirmationNeverRetargetsIdenticalContentAcrossScopeSessionOrSource() {
        val key = CloudCopyViewKey(1, 7, selection)
        val pending = CloudCopyConfirmation(key, BlockId(id(6)))
        val allowed = setOf(BlockId(id(6)))
        assertTrue(pending.validFor(key, CloudCopyPhase.Verified, allowed))
        val changed = listOf(key.copy(ownerScope = 2), key.copy(accountGeneration = 8),
            key.copy(selection = selection.copy(accountId = id(99))),
            key.copy(selection = selection.copy(workspaceId = id(99))),
            key.copy(selection = selection.copy(incarnationId = id(99))),
            key.copy(selection = selection.copy(cloudUnitId = id(99))),
            key.copy(selection = selection.copy(documentId = id(99))),
            key.copy(selection = selection.copy(sourceHeadDigest = "b".repeat(64))))
        changed.forEach { assertFalse(pending.validFor(it, CloudCopyPhase.Verified, allowed)) }
        assertFalse(pending.validFor(key, CloudCopyPhase.Removing, allowed))
        assertFalse(pending.validFor(key, CloudCopyPhase.Verified, emptySet()))
    }
}
