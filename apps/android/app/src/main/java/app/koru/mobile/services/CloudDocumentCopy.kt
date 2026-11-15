// SPDX-License-Identifier: MPL-2.0
package app.koru.mobile.services

import app.koru.mobile.domain.*
import org.json.JSONArray
import org.json.JSONObject
import java.math.BigInteger
import java.util.UUID

/** Never a DocumentId-only editor/cache key. Account scope remains an outer delivery fence. */
internal data class CloudCopySelection(
    val accountId: String, val workspaceId: String, val incarnationId: String,
    val cloudUnitId: String, val documentId: String, val sourceHeadDigest: String,
) {
    fun valid(): Boolean = listOf(accountId, workspaceId, incarnationId, cloudUnitId, documentId).all(::copyUuid) &&
        sourceHeadDigest.matches(Regex("[0-9a-f]{64}"))
    fun json(): String {
        require(valid())
        return JSONObject().put("account_id", accountId).put("workspace_id", workspaceId)
            .put("incarnation_id", incarnationId).put("cloud_unit_id", cloudUnitId)
            .put("document_id", documentId).put("source_head_digest", sourceHeadDigest).toString()
    }
    override fun toString() = "CloudCopySelection(<redacted>)"
}

/** Metadata only: deliberately not a global BlobReference usable by hydration. */
internal data class CloudCopyFile(val mimeType: String, val sizeDecimal: String?, val reference: EntityReference? = null)
internal enum class CloudCopyStructure { Document, Trash }
internal class CloudDocumentCopy(
    val selection: CloudCopySelection,
    val document: DocumentSnapshot,
    val files: Map<BlockId, CloudCopyFile>,
    val trashedNodes: Set<BlockId>,
    val structure: Map<BlockId, CloudCopyStructure>,
    val canRequestRemoval: Boolean,
    val removableFiles: Map<BlockId, Boolean>,
) {
    val permittedNodes: Set<BlockId> get() = if (canRequestRemoval) removableFiles.keys else emptySet()
    override fun toString() = "CloudDocumentCopy(<redacted>)"
}

/** Bounded projection of koru-store-wire::cloud_document::CloudDocumentInspectionV1. */
internal object CloudDocumentCopyDecoder {
    fun decode(raw: String, expected: CloudCopySelection): CloudDocumentCopy? = runCatching {
        require(expected.valid() && raw.length in 1..1_048_576 && raw.toByteArray(Charsets.UTF_8).size <= 1_048_576)
        val root = JSONObject(raw).fields("selection document can_request_removal removable_files")
        val selected = root.getJSONObject("selection").fields("account_id workspace_id incarnation_id cloud_unit_id document_id source_head_digest")
        val selection = CloudCopySelection(selected.string("account_id"), selected.string("workspace_id"),
            selected.string("incarnation_id"), selected.string("cloud_unit_id"), selected.string("document_id"), selected.string("source_head_digest"))
        require(selection == expected)
        val document = root.getJSONObject("document").fields("id workspace_id title revision blocks")
        require(document.string("id") == selection.documentId && document.string("workspace_id") == selection.workspaceId)
        val budget = Budget()
        val title = budget.text(document.string("title"))
        val revision = document.integer("revision", Long.MAX_VALUE.toString()).toLong()
        val rows = document.getJSONArray("blocks").bounded(2_000)
        val ids = HashSet<String>()
        val parents = HashSet<String>()
        val eligible = HashSet<String>()
        val files = HashMap<BlockId, CloudCopyFile>()
        val trashed = HashSet<BlockId>()
        val structure = HashMap<BlockId, CloudCopyStructure>()
        val blocks = List(rows.length()) { index ->
            val row = rows.getJSONObject(index).fields("id parent_id kind in_trash properties", "rich_text blob reference")
            val id = row.string("id").also { require(copyUuid(it) && ids.add(it)) }
            if (row.bool("in_trash")) trashed.add(BlockId(id))
            val parent = row.nullString("parent_id")?.also { require(it != id && it in ids); parents.add(it) }
            val kind = budget.text(row.string("kind"))
            var properties = properties(row.getJSONObject("properties"), budget)
            val rich = row.optionalObject("rich_text")?.let { rich(it, budget) }
            val reference = row.optionalObject("reference")?.let { reference(it, budget) }
            val blob = row.optionalObject("blob")?.let {
                it.fields("id mime_type size width height")
                budget.text(it.string("id"))
                val mime = budget.text(it.string("mime_type"))
                val size = it.nullInteger("size", "18446744073709551615")
                it.nullInteger("width", "4294967295"); it.nullInteger("height", "4294967295")
                CloudCopyFile(mime, size, reference)
            }
            if (blob != null && kind in setOf("file", "image", "canvas")) files[BlockId(id)] = blob
            if (blob != null && kind in setOf("file", "image", "canvas")) eligible.add(id)
            val content = when (kind) {
                "document" -> {
                    structure[BlockId(id)] = CloudCopyStructure.Document
                    properties = properties.copy(headingLevel = properties.headingLevel ?: 1)
                    BlockContent.Paragraph(rich ?: RichText(properties.name ?: title))
                }
                "trash" -> {
                    structure[BlockId(id)] = CloudCopyStructure.Trash
                    BlockContent.Unsupported(0, "koru.cloud/trash", byteArrayOf())
                }
                "paragraph" -> BlockContent.Paragraph(requireNotNull(rich))
                "quote" -> BlockContent.Quote(requireNotNull(rich))
                "task" -> BlockContent.Task(requireNotNull(rich))
                "code" -> BlockContent.Code(requireNotNull(rich))
                "table" -> BlockContent.Table(requireNotNull(rich))
                "contact" -> BlockContent.Contact(requireNotNull(rich))
                "callout" -> BlockContent.Callout
                "listblock" -> BlockContent.ListContainer(properties.listStyle ?: ListStyle.Bullet)
                "divider" -> BlockContent.Divider
                "embed" -> BlockContent.Embed(requireNotNull(reference))
                // These rows are drawn from passive file metadata, never a hydration handle.
                else -> BlockContent.Unsupported(0, "koru.cloud/$kind", byteArrayOf())
            }
            Block(BlockId(id), parent?.let(::BlockId), properties, content)
        }
        val controls = root.getJSONArray("removable_files").bounded(2_000)
        val removable = HashMap<BlockId, Boolean>()
        for (index in 0 until controls.length()) {
            val control = controls.getJSONObject(index).fields("node_id in_trash")
            val id = control.string("node_id")
            require(id in eligible && id !in parents && BlockId(id) !in removable)
            val inTrash = control.bool("in_trash")
            require(inTrash == (BlockId(id) in trashed))
            removable[BlockId(id)] = inTrash
        }
        CloudDocumentCopy(selection, DocumentSnapshot(DocumentId(selection.documentId), WorkspaceId(selection.workspaceId),
            title, revision, blocks), files, trashed, structure, root.bool("can_request_removal"), removable)
    }.getOrNull()

    private fun properties(value: JSONObject, budget: Budget): BlockProperties {
        value.fields("name icon color done alignment heading_level list_style image_width image_height language")
        fun text(key: String) = value.nullString(key)?.let(budget::text)
        val name = text("name"); val icon = text("icon"); val color = text("color")
        val alignment = budget.text(value.string("alignment"))
        val heading = value.nullInteger("heading_level", "255")?.toInt()
        val list = text("list_style")
        val width = value.nullInteger("image_width", "4294967295")?.toLong()
        val height = value.nullInteger("image_height", "4294967295")?.toLong()
        return BlockProperties(name, icon, color, value.bool("done"), when (alignment) {
            "center" -> Alignment.Center; "end" -> Alignment.End; "justify" -> Alignment.Justify; else -> Alignment.Start
        }, heading, when (list) {
            "bullet" -> ListStyle.Bullet; "checkbox" -> ListStyle.Checkbox; "number", "numbered" -> ListStyle.Numbered
            "letter" -> ListStyle.Letter; "roman" -> ListStyle.Roman; else -> null
        }, width?.takeIf { it <= Int.MAX_VALUE }?.toInt(), height?.takeIf { it <= Int.MAX_VALUE }?.toInt(), text("language"))
    }

    private fun rich(value: JSONObject, budget: Budget): RichText {
        value.fields("text marks inline_objects")
        val text = budget.text(value.string("text"))
        val boundaries = HashSet<Int>()
        var offset = 0; var bytes = 0
        boundaries.add(0)
        while (offset < text.length) {
            val cp = text.codePointAt(offset)
            bytes += when { cp <= 0x7f -> 1; cp <= 0x7ff -> 2; cp <= 0xffff -> 3; else -> 4 }
            offset += Character.charCount(cp); boundaries.add(bytes)
        }
        val marks = value.getJSONArray("marks").bounded(8_000)
        val inline = value.getJSONArray("inline_objects").bounded(8_000)
        budget.marks(marks.length() + inline.length())
        val mappedMarks = List(marks.length()) { index ->
            val mark = marks.getJSONObject(index).fields("start_utf8 end_utf8 kind", "value")
            val start = mark.integer("start_utf8", "262144").toInt()
            val end = mark.integer("end_utf8", "262144").toInt()
            require(start <= end && start in boundaries && end in boundaries)
            val kind = budget.text(mark.string("kind"))
            val annotation = if (mark.has("value")) mark.nullString("value")?.let(budget::text) else null
            TextMark(TextRange(start, end), MarkKind.entries.firstOrNull { it.wireName == kind } ?: MarkKind.Unknown,
                annotation?.toByteArray(Charsets.UTF_8) ?: byteArrayOf(), kind)
        }
        val objects = List(inline.length()) { index ->
            val item = inline.getJSONObject(index).fields("id anchor_utf8 role label reference")
            val id = item.string("id").also { require(copyUuid(it)) }
            val anchor = item.integer("anchor_utf8", "262144").toInt().also { require(it in boundaries) }
            InlineObject(InlineObjectId(id), anchor, budget.text(item.string("role")),
                reference(item.getJSONObject("reference"), budget), item.nullString("label")?.let(budget::text))
        }
        return RichText(text, mappedMarks, objects)
    }

    private fun reference(value: JSONObject, budget: Budget): EntityReference = when (value.string("kind")) {
        "document" -> {
            value.fields("kind document_id", "node_id")
            val document = value.string("document_id").also { require(copyUuid(it)) }
            val node = if (value.has("node_id")) value.nullString("node_id")?.also { require(copyUuid(it)) } else null
            if (node == null) EntityReference.Document(DocumentId(document)) else EntityReference.Block(BlockId(node), DocumentId(document))
        }
        "block" -> {
            value.fields("kind block_id", "document_id")
            val id = value.string("block_id").also { require(copyUuid(it)) }
            val document = if (value.has("document_id")) value.nullString("document_id")?.also { require(copyUuid(it)) } else null
            EntityReference.Block(BlockId(id), document?.let(::DocumentId))
        }
        "run" -> { value.fields("kind document_id"); EntityReference.Run(DocumentId(value.string("document_id").also { require(copyUuid(it)) })) }
        "url" -> { value.fields("kind url"); EntityReference.Url(budget.text(value.string("url"))) }
        "label" -> { value.fields("kind value"); EntityReference.Label(budget.text(value.string("value"))) }
        else -> error("unavailable reference")
    }

    private class Budget {
        private var bytes = 262_144
        private var marks = 8_000
        fun text(value: String): String {
            var index = 0
            while (index < value.length) {
                val ch = value[index++]
                if (Character.isHighSurrogate(ch)) require(index < value.length && Character.isLowSurrogate(value[index++]))
                else require(!Character.isLowSurrogate(ch))
            }
            bytes -= value.toByteArray(Charsets.UTF_8).size
            require(bytes >= 0)
            return value
        }
        fun marks(count: Int) { marks -= count; require(marks >= 0) }
    }
    private fun JSONObject.fields(required: String, optional: String = ""): JSONObject {
        val requiredKeys = required.split(' ').filter(String::isNotEmpty).toSet()
        val allowed = requiredKeys + optional.split(' ').filter(String::isNotEmpty)
        require(keys().asSequence().all { it in allowed } && requiredKeys.all(::has))
        return this
    }
    private fun JSONObject.string(key: String) = (get(key) as? String) ?: error("unavailable string")
    private fun JSONObject.nullString(key: String): String? = if (get(key) === JSONObject.NULL) null else string(key)
    private fun JSONObject.bool(key: String) = (get(key) as? Boolean) ?: error("unavailable boolean")
    private fun JSONObject.optionalObject(key: String): JSONObject? = if (!has(key) || get(key) === JSONObject.NULL) null else getJSONObject(key)
    private fun JSONObject.integer(key: String, max: String): String {
        val number = get(key)
        require(number is Int || number is Long || number is BigInteger)
        return number.toString().also { require(it.matches(Regex("0|[1-9][0-9]*")) && BigInteger(it) <= BigInteger(max)) }
    }
    private fun JSONObject.nullInteger(key: String, max: String): String? = if (get(key) === JSONObject.NULL) null else integer(key, max)
    private fun JSONArray.bounded(max: Int) = also { require(length() <= max) }
}

private fun copyUuid(value: String): Boolean = runCatching { UUID.fromString(value).toString() == value }.getOrDefault(false)
