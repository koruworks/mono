// SPDX-License-Identifier: MPL-2.0

package app.koru.mobile.services

import app.koru.mobile.domain.*
import app.koru.mobile.logging.AppLogger
import java.security.MessageDigest
import java.util.UUID
import kotlinx.coroutines.flow.*
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock

private const val OBJECT_REPLACEMENT = '\uFFFC'

class MockKoruStore(private val logger: AppLogger) {
    private val lock = Mutex()
    private val profiles = MutableStateFlow<List<Profile>>(emptyList())
    private val workspaces = MutableStateFlow<List<Workspace>>(emptyList())
    private val summaries = MutableStateFlow<List<DocumentSummary>>(emptyList())
    private val documents = mutableMapOf<DocumentId, MutableStateFlow<DocumentSnapshot>>()
    private val documentEvents = mutableMapOf<DocumentId, MutableSharedFlow<DocumentEvent>>()
    private val blobs = mutableMapOf<BlobId, ByteArray>()
    private val blobAvailability = mutableMapOf<BlobId, MutableStateFlow<Boolean>>()
    private val appliedOperations = mutableMapOf<DocumentId, MutableMap<String, DocumentCommand>>()

    init { seed() }

    fun observeProfiles(): Flow<List<Profile>> = profiles
    fun observeWorkspaces(profileId: ProfileId): Flow<List<Workspace>> =
        workspaces.map { rows -> rows.filter { it.profileId == profileId } }.distinctUntilChanged()
    fun observeDocuments(workspaceId: WorkspaceId): Flow<List<DocumentSummary>> =
        summaries.map { rows -> rows.filter { it.workspaceId == workspaceId } }.distinctUntilChanged()

    suspend fun profile(id: ProfileId) = profiles.value.firstOrNull { it.id == id }
        ?: throw AppFailure(AppErrorCode.NotFound, "profile not found")
    suspend fun workspace(id: WorkspaceId) = workspaces.value.firstOrNull { it.id == id }
        ?: throw AppFailure(AppErrorCode.NotFound, "workspace not found")
    suspend fun summary(id: DocumentId) = summaries.value.firstOrNull { it.id == id }
        ?: throw AppFailure(AppErrorCode.NotFound, "document not found")

    suspend fun createProfile(displayName: String, locale: String): Profile = lock.withLock {
        requireLabel(displayName)
        if (locale.isBlank()) throw AppFailure(AppErrorCode.InvalidData, "locale is required")
        Profile(ProfileId.new(), displayName.trim(), locale).also { profiles.value += it }
    }

    suspend fun renameProfile(id: ProfileId, displayName: String): Profile = lock.withLock {
        requireLabel(displayName)
        profile(id)
        val trimmed = displayName.trim()
        profiles.value = profiles.value.map {
            if (it.id == id) it.copy(displayName = trimmed, displayLabel = trimmed) else it
        }
        profile(id)
    }

    suspend fun createWorkspace(profileId: ProfileId, title: String): Workspace = lock.withLock {
        profile(profileId)
        requireLabel(title)
        Workspace(
            WorkspaceId.new(),
            profileId,
            title.trim(),
            modifiedAtMillis = System.currentTimeMillis(),
        ).also { workspaces.value += it }
    }

    suspend fun renameWorkspace(id: WorkspaceId, title: String): Workspace = lock.withLock {
        requireLabel(title)
        updateWorkspace(id) { it.copy(title = title.trim(), modifiedAtMillis = System.currentTimeMillis()) }
    }

    suspend fun setWorkspaceArchived(id: WorkspaceId, archived: Boolean): Workspace = lock.withLock {
        updateWorkspace(id) { it.copy(archived = archived, modifiedAtMillis = System.currentTimeMillis()) }
    }

    suspend fun deleteWorkspace(id: WorkspaceId): Workspace = lock.withLock {
        updateWorkspace(id) { it.copy(deletedAtMillis = System.currentTimeMillis()) }
    }

    suspend fun restoreWorkspace(id: WorkspaceId): Workspace = lock.withLock {
        updateWorkspace(id) { it.copy(deletedAtMillis = null, modifiedAtMillis = System.currentTimeMillis()) }
    }

    private suspend fun updateWorkspace(id: WorkspaceId, change: (Workspace) -> Workspace): Workspace {
        val current = workspace(id)
        val updated = change(current)
        workspaces.value = workspaces.value.map { if (it.id == id) updated else it }
        return updated
    }

    suspend fun createDocument(workspaceId: WorkspaceId, title: String): DocumentSummary = lock.withLock {
        workspace(workspaceId)
        requireLabel(title)
        val summary = DocumentSummary(DocumentId.new(), workspaceId, title.trim())
        val paragraph = Block(BlockId.new(), content = BlockContent.Paragraph(RichText("")))
        addDocument(DocumentSnapshot(summary.id, workspaceId, summary.title, 0, listOf(paragraph)))
        summary
    }

    suspend fun renameDocument(id: DocumentId, title: String): DocumentSummary {
        requireLabel(title)
        execute(id, DocumentCommand.RenameDocument(title.trim()), UUID.randomUUID().toString())
        return summary(id)
    }

    suspend fun trashDocument(id: DocumentId): Unit = lock.withLock {
        summary(id)
        summaries.value = summaries.value.filterNot { it.id == id }
        documents.remove(id)
        documentEvents.remove(id)
        appliedOperations.remove(id)
        Unit
    }

    suspend fun open(id: DocumentId): DocumentSession {
        summary(id)
        return MockDocumentSession(this, id, logger)
    }

    fun snapshotFlow(id: DocumentId): StateFlow<DocumentSnapshot> =
        documents[id]?.asStateFlow() ?: throw AppFailure(AppErrorCode.NotFound, "document not found")
    fun eventFlow(id: DocumentId): SharedFlow<DocumentEvent> =
        documentEvents[id]?.asSharedFlow() ?: throw AppFailure(AppErrorCode.NotFound, "document not found")

    suspend fun execute(id: DocumentId, command: DocumentCommand, operationId: String) {
        if (runCatching { UUID.fromString(operationId) }.isFailure) {
            throw AppFailure(AppErrorCode.InvalidData, "operation ID is not a UUID")
        }
        lock.withLock {
            val seen = appliedOperations.getOrPut(id) { mutableMapOf() }
            seen[operationId]?.let { prior ->
                if (prior == command) return
                throw AppFailure(AppErrorCode.InvalidData, "operation ID was reused")
            }
            val state = documents[id] ?: throw AppFailure(AppErrorCode.NotFound, "document not found")
            val (next, changed, event) = apply(state.value, command)
            state.value = next
            summaries.value = summaries.value.map {
                if (it.id == id) it.copy(title = next.title) else it
            }
            seen[operationId] = command
            documentEvents.getValue(id).tryEmit(event ?: DocumentEvent.Changed(next.revision, changed))
            logger.event("document_command", mapOf("document_id" to id.value, "command" to command::class.simpleName, "revision" to next.revision))
        }
    }

    suspend fun simulateExternal(id: DocumentId, command: DocumentCommand) =
        execute(id, command, UUID.randomUUID().toString())

    fun observeBlob(id: BlobId): Flow<Boolean> = blobAvailability.getOrPut(id) { MutableStateFlow(blobs.containsKey(id)) }

    suspend fun putBlob(bytes: ByteArray, mimeType: String, width: Int?, height: Int?): BlobReference = lock.withLock {
        if (bytes.isEmpty() || mimeType.isBlank()) throw AppFailure(AppErrorCode.InvalidData, "blob data and MIME type are required")
        val digest = MessageDigest.getInstance("SHA-256").digest(bytes).joinToString("") { "%02x".format(it) }
        val id = BlobId("sha256:$digest")
        blobs[id] = bytes.copyOf()
        blobAvailability.getOrPut(id) { MutableStateFlow(false) }.value = true
        BlobReference(id, mimeType, bytes.size.toLong(), width, height)
    }

    suspend fun loadBlob(id: BlobId): ByteArray = lock.withLock {
        blobs[id]?.copyOf() ?: throw AppFailure(AppErrorCode.NotFound, "blob not found")
    }

    private fun addDocument(document: DocumentSnapshot) {
        documents[document.id] = MutableStateFlow(document)
        documentEvents[document.id] = MutableSharedFlow(extraBufferCapacity = 32)
        summaries.value += DocumentSummary(document.id, document.workspaceId, document.title)
    }

    private fun apply(source: DocumentSnapshot, command: DocumentCommand): Triple<DocumentSnapshot, Set<BlockId>, DocumentEvent?> {
        var blocks = source.blocks
        var title = source.title
        var changed = emptySet<BlockId>()
        var event: DocumentEvent? = null
        when (command) {
            is DocumentCommand.EditBlockText -> {
                val split = command.split
                val replay = split != null && blocks.any { it.id == split.blockId }
                if (replay) {
                    val at = utf16Index(command.after, split!!.atUtf8)
                    if (requireBlock(blocks, command.blockId).content.editableText()?.text != command.after.substring(0, at) ||
                        requireBlock(blocks, split.blockId).content.editableText()?.text != command.after.substring(at)) invalid("text_edit_conflict")
                } else if (blocks.none { it.id == command.blockId } && command.createParagraph) {
                    blocks = blocks + Block(command.blockId, content = BlockContent.Paragraph(RichText(command.after)))
                } else {
                    blocks = updateRichText(blocks, command.blockId) { rich ->
                        if (rich.text == command.after) rich else {
                            if (rich.text != command.before) invalid("text_edit_conflict")
                            val splice = textSplice(command.before, command.after)
                            val start = command.before.substring(0, splice.start).encodeToByteArray().size
                            val end = command.before.substring(0, splice.end).encodeToByteArray().size
                            insertText(deleteText(rich, TextRange(start, end), true), start, splice.inserted, false)
                        }
                    }
                }
                changed = setOf(command.blockId)
                if (split != null && !replay) {
                    val original = requireBlock(blocks, command.blockId)
                    val rich = original.content.editableText() ?: invalid("block_read_only")
                    val at = utf16Index(rich.text, split.atUtf8)
                    val child = original.copy(id = split.blockId,
                        parentId = if (blocks.any { it.parentId == original.id }) original.id else original.parentId,
                        properties = BlockProperties(headingLevel = original.properties.headingLevel.takeIf { at < rich.text.length }),
                        content = original.content.withText(rich.slice(at, rich.text.length)))
                    blocks = blocks.toMutableList().apply {
                        val index = indexOfFirst { it.id == original.id }
                        set(index, original.copy(content = original.content.withText(rich.slice(0, at))))
                        add(index + 1, child)
                    }
                    changed = changed + split.blockId
                }
            }
            is DocumentCommand.CreateBlock -> {
                if (blocks.any { it.id == command.block.id }) invalid("duplicate block ID")
                command.block.parentId?.let { parent -> if (blocks.none { it.id == parent }) notFound("parent") }
                val insertion = insertionIndex(blocks, command.block.parentId, command.siblingIndex)
                blocks = blocks.toMutableList().apply { add(insertion, command.block) }
                changed = setOf(command.block.id)
            }
            is DocumentCommand.DeleteBlock -> {
                requireBlock(blocks, command.blockId)
                val removed = descendantsIncluding(blocks, command.blockId)
                blocks = blocks.filterNot { it.id in removed }
                changed = removed
            }
            is DocumentCommand.MoveBlock -> {
                blocks = move(blocks, command.blockId, command.parentId, command.siblingIndex)
                changed = setOf(command.blockId)
            }
            is DocumentCommand.IndentBlock -> {
                val target = requireBlock(blocks, command.blockId)
                val siblings = blocks.filter { it.parentId == target.parentId }
                val at = siblings.indexOfFirst { it.id == target.id }
                if (at <= 0) invalid("block has no previous sibling")
                val parent = siblings[at - 1]
                blocks = move(blocks, target.id, parent.id, Int.MAX_VALUE)
                changed = setOf(target.id)
            }
            is DocumentCommand.OutdentBlock -> {
                val target = requireBlock(blocks, command.blockId)
                val parent = target.parentId?.let { requireBlock(blocks, it) } ?: invalid("block is already top-level")
                val grandParent = parent.parentId
                val parentSiblings = blocks.filter { it.parentId == grandParent }
                blocks = move(blocks, target.id, grandParent, parentSiblings.indexOfFirst { it.id == parent.id } + 1)
                changed = setOf(target.id)
            }
            is DocumentCommand.InsertText -> {
                blocks = updateRichText(blocks, command.blockId) { rich -> insertText(rich, command.atUtf8, command.text, false) }
                changed = setOf(command.blockId)
            }
            is DocumentCommand.DeleteText -> {
                blocks = updateRichText(blocks, command.blockId) { rich -> deleteText(rich, command.range, false) }
                changed = setOf(command.blockId)
            }
            is DocumentCommand.AddMark -> {
                blocks = updateRichText(blocks, command.blockId) { rich ->
                    validateRange(rich.text, command.mark.range)
                    if (rich.inlineObjects.any { it.anchorUtf8 in command.mark.range.startUtf8 until command.mark.range.endUtf8 }) invalid("marks cannot cover inline objects")
                    rich.copy(marks = rich.marks + command.mark)
                }
                changed = setOf(command.blockId)
            }
            is DocumentCommand.RemoveMark -> {
                blocks = updateRichText(blocks, command.blockId) { rich ->
                    validateRange(rich.text, command.range)
                    rich.copy(marks = rich.marks.flatMap { subtractMark(it, command.range, command.kind) })
                }
                changed = setOf(command.blockId)
            }
            is DocumentCommand.ToggleTask -> {
                val block = requireBlock(blocks, command.blockId)
                if (block.content !is BlockContent.Task && block.parentId?.let { id -> (blocks.find { it.id == id }?.content as? BlockContent.ListContainer)?.style } != ListStyle.Checkbox) {
                    invalid("block is not task-like")
                }
                blocks = replace(blocks, block.copy(properties = block.properties.copy(done = command.done)))
                changed = setOf(block.id)
            }
            is DocumentCommand.InsertInlineObject -> {
                blocks = updateRichText(blocks, command.blockId) { rich ->
                    if (rich.inlineObjects.any { it.id == command.value.id }) invalid("duplicate inline object")
                    val inserted = insertText(rich, command.value.anchorUtf8, OBJECT_REPLACEMENT.toString(), true)
                    inserted.copy(inlineObjects = inserted.inlineObjects + command.value)
                }
                changed = setOf(command.blockId)
            }
            is DocumentCommand.RemoveInlineObject -> {
                blocks = updateRichText(blocks, command.blockId) { rich ->
                    val value = rich.inlineObjects.firstOrNull { it.id == command.objectId } ?: notFound("inline object")
                    val without = rich.copy(inlineObjects = rich.inlineObjects - value)
                    deleteText(without, TextRange(value.anchorUtf8, value.anchorUtf8 + 3), true)
                }
                changed = setOf(command.blockId)
            }
            is DocumentCommand.AttachBlob -> {
                val block = requireBlock(blocks, command.blockId)
                if (block.content !is BlockContent.Image) invalid("blob target is not an image")
                blocks = replace(blocks, block.copy(content = BlockContent.Image(command.blob)))
                changed = setOf(block.id)
                event = DocumentEvent.BlobAvailable(command.blob.id)
            }
            is DocumentCommand.RenameDocument -> {
                requireLabel(command.title)
                title = command.title.trim()
                event = DocumentEvent.Renamed(title)
            }
        }
        return Triple(source.copy(title = title, revision = source.revision + 1, blocks = blocks), changed, event)
    }

    private fun move(source: List<Block>, id: BlockId, parentId: BlockId?, siblingIndex: Int): List<Block> {
        requireBlock(source, id)
        parentId?.let {
            requireBlock(source, it)
            if (it in descendantsIncluding(source, id)) invalid("block cannot move beneath itself")
        }
        val movedIds = descendantsIncluding(source, id)
        val segment = source.filter { it.id in movedIds }.map { if (it.id == id) it.copy(parentId = parentId) else it }
        val remainder = source.filterNot { it.id in movedIds }
        val insertion = insertionIndex(remainder, parentId, siblingIndex)
        return remainder.toMutableList().apply { addAll(insertion, segment) }
    }

    private fun insertionIndex(source: List<Block>, parentId: BlockId?, siblingIndex: Int): Int {
        val siblings = source.withIndex().filter { it.value.parentId == parentId }
        if (siblings.isEmpty()) {
            return parentId?.let { parent -> source.indexOfFirst { it.id == parent } + 1 } ?: source.size
        }
        if (siblingIndex < siblings.size) return siblings[siblingIndex.coerceAtLeast(0)].index
        val last = siblings.last().value.id
        val descendants = descendantsIncluding(source, last)
        return source.indexOfLast { it.id in descendants } + 1
    }

    private fun descendantsIncluding(source: List<Block>, root: BlockId): Set<BlockId> {
        val result = linkedSetOf(root)
        var grew = true
        while (grew) {
            val before = result.size
            source.filter { it.parentId in result }.forEach { result += it.id }
            grew = result.size != before
        }
        return result
    }

    private fun updateRichText(source: List<Block>, id: BlockId, update: (RichText) -> RichText): List<Block> {
        val block = requireBlock(source, id)
        val next = when (val content = block.content) {
            is BlockContent.Paragraph -> block.copy(content = BlockContent.Paragraph(update(content.richText)))
            is BlockContent.Quote -> block.copy(content = BlockContent.Quote(update(content.richText)))
            is BlockContent.Task -> block.copy(content = BlockContent.Task(update(content.richText)))
            is BlockContent.Code -> block.copy(content = BlockContent.Code(update(content.richText)))
            is BlockContent.Table -> block.copy(content = BlockContent.Table(update(content.richText)))
            is BlockContent.Contact -> block.copy(content = BlockContent.Contact(update(content.name)))
            else -> invalid("block has no editable text")
        }
        return replace(source, next)
    }

    private fun insertText(rich: RichText, atUtf8: Int, value: String, allowObject: Boolean): RichText {
        if (!allowObject && value.contains(OBJECT_REPLACEMENT)) invalid("inline objects require a semantic command")
        val at = utf16Index(rich.text, atUtf8)
        val delta = value.toByteArray(Charsets.UTF_8).size
        return rich.copy(
            text = rich.text.substring(0, at) + value + rich.text.substring(at),
            marks = rich.marks.map { mark ->
                when {
                    mark.range.startUtf8 >= atUtf8 -> mark.copy(range = TextRange(mark.range.startUtf8 + delta, mark.range.endUtf8 + delta))
                    mark.range.endUtf8 > atUtf8 -> mark.copy(range = mark.range.copy(endUtf8 = mark.range.endUtf8 + delta))
                    else -> mark
                }
            },
            inlineObjects = rich.inlineObjects.map { if (it.anchorUtf8 >= atUtf8) it.copy(anchorUtf8 = it.anchorUtf8 + delta) else it },
        )
    }

    private fun deleteText(rich: RichText, range: TextRange, allowObject: Boolean): RichText {
        validateRange(rich.text, range)
        if (!allowObject && rich.inlineObjects.any { it.anchorUtf8 in range.startUtf8 until range.endUtf8 }) invalid("inline objects require a semantic command")
        val start = utf16Index(rich.text, range.startUtf8)
        val end = utf16Index(rich.text, range.endUtf8)
        val delta = range.endUtf8 - range.startUtf8
        return rich.copy(
            text = rich.text.removeRange(start, end),
            marks = rich.marks.mapNotNull { clampAfterDelete(it, range) },
            inlineObjects = rich.inlineObjects.mapNotNull {
                when {
                    it.anchorUtf8 in range.startUtf8 until range.endUtf8 -> if (allowObject) null else it
                    it.anchorUtf8 >= range.endUtf8 -> it.copy(anchorUtf8 = it.anchorUtf8 - delta)
                    else -> it
                }
            },
        )
    }

    private fun clampAfterDelete(mark: TextMark, deleted: TextRange): TextMark? {
        val start = when {
            mark.range.startUtf8 >= deleted.endUtf8 -> mark.range.startUtf8 - (deleted.endUtf8 - deleted.startUtf8)
            mark.range.startUtf8 >= deleted.startUtf8 -> deleted.startUtf8
            else -> mark.range.startUtf8
        }
        val end = when {
            mark.range.endUtf8 >= deleted.endUtf8 -> mark.range.endUtf8 - (deleted.endUtf8 - deleted.startUtf8)
            mark.range.endUtf8 > deleted.startUtf8 -> deleted.startUtf8
            else -> mark.range.endUtf8
        }
        return if (start < end) mark.copy(range = TextRange(start, end)) else null
    }

    private fun subtractMark(mark: TextMark, removed: TextRange, kind: MarkKind): List<TextMark> {
        if (mark.kind != kind || mark.range.endUtf8 <= removed.startUtf8 || mark.range.startUtf8 >= removed.endUtf8) return listOf(mark)
        return buildList {
            if (mark.range.startUtf8 < removed.startUtf8) add(mark.copy(range = TextRange(mark.range.startUtf8, removed.startUtf8)))
            if (mark.range.endUtf8 > removed.endUtf8) add(mark.copy(range = TextRange(removed.endUtf8, mark.range.endUtf8)))
        }
    }

    private fun validateRange(text: String, range: TextRange) {
        if (range.startUtf8 < 0 || range.endUtf8 < range.startUtf8) invalid("invalid text range")
        utf16Index(text, range.startUtf8)
        utf16Index(text, range.endUtf8)
    }

    private fun utf16Index(text: String, utf8Offset: Int): Int {
        if (utf8Offset < 0) invalid("invalid UTF-8 offset")
        var bytes = 0
        var index = 0
        while (index < text.length) {
            if (bytes == utf8Offset) return index
            val codePoint = text.codePointAt(index)
            bytes += String(Character.toChars(codePoint)).toByteArray(Charsets.UTF_8).size
            index += Character.charCount(codePoint)
        }
        if (bytes == utf8Offset) return index
        invalid("offset is not a UTF-8 scalar boundary")
    }

    private fun replace(source: List<Block>, block: Block) = source.map { if (it.id == block.id) block else it }
    private fun requireBlock(source: List<Block>, id: BlockId) = source.firstOrNull { it.id == id } ?: notFound("block")
    private fun requireLabel(value: String) { if (value.isBlank()) invalid("value is required") }
    private fun invalid(detail: String): Nothing = throw AppFailure(AppErrorCode.InvalidData, detail)
    private fun notFound(value: String): Nothing = throw AppFailure(AppErrorCode.NotFound, "$value not found")

    private fun seed() {
        val profile = Profile(ProfileId("10000000-0000-4000-8000-000000000001"), "Koru Demo", "en-US")
        val workspace = Workspace(WorkspaceId("10000000-0000-4000-8000-000000000002"), profile.id, "Mobile foundation")
        val documentId = DocumentId("10000000-0000-4000-8000-000000000003")
        val referencedId = DocumentId("10000000-0000-4000-8000-000000000004")
        val heading = Block(
            BlockId("10000000-0000-4000-8000-000000000010"),
            properties = BlockProperties(headingLevel = 1),
            content = BlockContent.Paragraph(RichText("A semantic Koru document", listOf(TextMark(TextRange(0, 10), MarkKind.Bold)))),
        )
        val multilingual = "English, العربية, 日本語, and emoji 👋 stay one document."
        val paragraph = Block(BlockId("10000000-0000-4000-8000-000000000011"), content = BlockContent.Paragraph(RichText(multilingual)))
        val bullets = Block(BlockId("10000000-0000-4000-8000-000000000012"), properties = BlockProperties(listStyle = ListStyle.Bullet), content = BlockContent.ListContainer(ListStyle.Bullet))
        val bulletItem = Block(BlockId("10000000-0000-4000-8000-000000000013"), bullets.id, content = BlockContent.Paragraph(RichText("Blocks carry structure")))
        val nested = Block(BlockId("10000000-0000-4000-8000-000000000014"), bulletItem.id, content = BlockContent.Paragraph(RichText("Nested without spaces pretending to be data")))
        val numbers = Block(BlockId("10000000-0000-4000-8000-000000000015"), properties = BlockProperties(listStyle = ListStyle.Numbered), content = BlockContent.ListContainer(ListStyle.Numbered))
        val numbered = Block(BlockId("10000000-0000-4000-8000-000000000016"), numbers.id, content = BlockContent.Paragraph(RichText("Commands express intent")))
        val task = Block(BlockId("10000000-0000-4000-8000-000000000017"), properties = BlockProperties(done = false), content = BlockContent.Task(RichText("Toggle this native task")))
        val png = java.util.Base64.getDecoder().decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=")
        val digest = MessageDigest.getInstance("SHA-256").digest(png).joinToString("") { "%02x".format(it) }
        val blob = BlobReference(BlobId("sha256:$digest"), "image/png", png.size.toLong(), 1, 1)
        blobs[blob.id] = png
        blobAvailability[blob.id] = MutableStateFlow(true)
        val image = Block(BlockId("10000000-0000-4000-8000-000000000018"), properties = BlockProperties(name = "Koru sample image"), content = BlockContent.Image(blob))
        val referenceText = "Open  for the referenced document"
        val anchor = "Open ".toByteArray().size
        val pill = InlineObject(InlineObjectId("10000000-0000-4000-8000-000000000019"), anchor, "document", EntityReference.Document(referencedId), "Project brief")
        val reference = Block(BlockId("10000000-0000-4000-8000-000000000020"), content = BlockContent.Paragraph(RichText(referenceText.substring(0, 5) + OBJECT_REPLACEMENT + referenceText.substring(5), inlineObjects = listOf(pill))))

        profiles.value = listOf(profile)
        workspaces.value = listOf(workspace)
        addDocument(DocumentSnapshot(documentId, workspace.id, "Mobile block model", 1, listOf(heading, paragraph, bullets, bulletItem, nested, numbers, numbered, task, image, reference)))
        addDocument(DocumentSnapshot(referencedId, workspace.id, "Project brief", 1, listOf(Block(BlockId("10000000-0000-4000-8000-000000000021"), content = BlockContent.Paragraph(RichText("Referenced by identity, not title."))))))
    }
}

class MockProfileService(private val store: MockKoruStore) : ProfileService {
    override fun observeProfiles() = store.observeProfiles()
    override suspend fun get(id: ProfileId) = store.profile(id)
    override suspend fun create(displayName: String, locale: String) = store.createProfile(displayName, locale)
    override suspend fun rename(id: ProfileId, displayName: String) = store.renameProfile(id, displayName)
}

class MockWorkspaceService(private val store: MockKoruStore) : WorkspaceService {
    override fun observeWorkspaces(profileId: ProfileId) = store.observeWorkspaces(profileId)
    override suspend fun get(id: WorkspaceId) = store.workspace(id)
    override suspend fun create(profileId: ProfileId, title: String) = store.createWorkspace(profileId, title)
    override suspend fun rename(id: WorkspaceId, title: String) = store.renameWorkspace(id, title)
    override suspend fun setArchived(id: WorkspaceId, archived: Boolean) = store.setWorkspaceArchived(id, archived)
    override suspend fun delete(id: WorkspaceId) = store.deleteWorkspace(id)
    override suspend fun restore(id: WorkspaceId) = store.restoreWorkspace(id)
}

class MockDocumentService(private val store: MockKoruStore) : DocumentService {
    override fun observeDocuments(workspaceId: WorkspaceId) = store.observeDocuments(workspaceId)
    override suspend fun get(id: DocumentId) = store.summary(id)
    override suspend fun create(workspaceId: WorkspaceId, title: String) = store.createDocument(workspaceId, title)
    override suspend fun rename(id: DocumentId, title: String) = store.renameDocument(id, title)
    override suspend fun moveToTrash(id: DocumentId) = store.trashDocument(id)
    override suspend fun open(id: DocumentId) = store.open(id)
}

class MockBlobService(private val store: MockKoruStore) : BlobService {
    override suspend fun put(bytes: ByteArray, mimeType: String, width: Int?, height: Int?) = store.putBlob(bytes, mimeType, width, height)
    override suspend fun load(reference: BlobReference) = store.loadBlob(reference.id)
}

class MockDocumentSession(
    private val store: MockKoruStore,
    override val documentId: DocumentId,
    private val logger: AppLogger,
) : DocumentSession {
    override val snapshots = store.snapshotFlow(documentId)
    override val events = store.eventFlow(documentId)
    override suspend fun execute(command: DocumentCommand, operationId: String) =
        logger.timed("document_execute", mapOf("document_id" to documentId.value)) { store.execute(documentId, command, operationId) }
    override suspend fun close() { logger.event("document_session_close", mapOf("document_id" to documentId.value)) }
}
