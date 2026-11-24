// SPDX-License-Identifier: MPL-2.0
package app.koru.mobile.services

import android.util.AtomicFile
import android.system.Os
import android.system.OsConstants
import java.io.ByteArrayOutputStream
import app.koru.contract.api.v1.DocumentTextEdit
import app.koru.contract.api.v1.DocumentTextSplit
import app.koru.mobile.domain.*
import java.io.File
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

internal fun DocumentCommand.EditBlockText.toWireEdit(): DocumentTextEdit = DocumentTextEdit.newBuilder()
    .setBlockId(blockId.value).setBefore(before).setAfter(after).setCreateParagraph(createParagraph)
    .apply { this@toWireEdit.split?.let { setSplit(DocumentTextSplit.newBuilder().setBlockId(it.blockId.value).setAtUtf8(it.atUtf8)) } }.build()

private fun DocumentTextEdit.toEdit() = DocumentCommand.EditBlockText(BlockId(blockId), before, after,
    createParagraph, if (hasSplit()) BlockTextSplit(BlockId(split.blockId), split.atUtf8) else null)

/** Only the purge handoff is journaled here. Native owns the retained CRDT
 *  snapshot; this ordered protobuf chain covers a crash partway through staging.
 *  Every filesystem operation runs on IO, never on the input/main thread. */
class PurgeDraftJournal(private val directory: File) {
    private fun file(id: DocumentId): AtomicFile {
        val name = java.util.UUID.fromString(id.value).toString()
        return AtomicFile(File(directory, "$name.pb"))
    }

    suspend fun retain(id: DocumentId, pending: List<DocumentCommand.EditBlockText>): List<DocumentCommand.EditBlockText> = withContext(Dispatchers.IO) {
        check(directory.mkdirs() || directory.isDirectory) { "document_draft_journal_unavailable" }
        val file = file(id)
        val previous = if (file.baseFile.exists() || File(file.baseFile.path + ".bak").exists()) {
            file.openRead().use { input ->
                buildList {
                    while (true) {
                        val edit = DocumentTextEdit.parseDelimitedFrom(input) ?: break
                        check(size < 16_384) { "document_draft_journal_too_large" }
                        add(edit)
                    }
                }
            }
        } else emptyList()
        val ordered = (previous + pending.map { it.toWireEdit() }).distinct()
        check(ordered.size <= 16_384) { "document_draft_journal_too_large" }
        val bytes = ByteArrayOutputStream().use { output ->
            ordered.forEach { it.writeDelimitedTo(output) }
            output.toByteArray()
        }
        val output = file.startWrite()
        var finished = false
        try {
            output.write(bytes)
            // AtomicFile logs a failed sync instead of throwing; the explicit
            // sync is the acknowledgement boundary for the unstaged tail.
            output.fd.sync()
            file.finishWrite(output)
            finished = true
            check(file.openRead().use { it.readBytes().contentEquals(bytes) }) { "document_draft_journal_unavailable" }
            syncDirectory(directory)
            directory.parentFile?.let(::syncDirectory)
        } catch (error: Throwable) {
            if (!finished) file.failWrite(output)
            throw error
        }
        ordered.map { it.toEdit() }
    }

    private fun syncDirectory(path: File) {
        // O_DIRECTORY is not in Android's public SDK; validate the opened fd.
        val descriptor = Os.open(path.absolutePath, OsConstants.O_RDONLY, 0)
        try {
            check(OsConstants.S_ISDIR(Os.fstat(descriptor).st_mode)) { "document_draft_journal_unavailable" }
            Os.fsync(descriptor)
        } finally { Os.close(descriptor) }
    }

    suspend fun transferred(id: DocumentId) = withContext(Dispatchers.IO) { file(id).delete() }
}
