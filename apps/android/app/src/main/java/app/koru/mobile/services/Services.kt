// SPDX-License-Identifier: MPL-2.0

package app.koru.mobile.services

import app.koru.mobile.domain.*
import java.io.File
import java.io.InputStream
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.flow

sealed interface DeviceLinkState {
    data object Unlinked : DeviceLinkState
    data class Linking(val peerName: String? = null) : DeviceLinkState
    data class Linked(val peerName: String?) : DeviceLinkState
    data class Failed(val code: String) : DeviceLinkState
    /** The seeds this device holds no longer produce the keys `device.json`
     *  names — content moved or restored onto another phone, or a stored
     *  identity that no longer verifies. The content is intact and the native
     *  library is fine; the old seeds cannot be recovered from a public key,
     *  so only [DeviceLinkService.resetIdentity] leads out of it. It is its
     *  own state rather than an [Unavailable] reason string because it is the
     *  one status code a person can act on. */
    data object IdentityMismatch : DeviceLinkState
    data class Unavailable(val reason: String) : DeviceLinkState
}

/**
 * Whether asking to link right now can only fail. Linking is a signature by
 * the key this device no longer has ([DeviceLinkState.IdentityMismatch]), or
 * a call into a library that did not load ([DeviceLinkState.Unavailable]), or
 * a second attempt over one already in flight — and offering a button for any
 * of the three is the same lie about the cause this screen exists to stop
 * telling.
 */
val DeviceLinkState.linkingBlocked: Boolean
    get() = this is DeviceLinkState.IdentityMismatch ||
        this is DeviceLinkState.Unavailable ||
        this is DeviceLinkState.Linking

interface DeviceLinkService {
    val accountDiagnostics: StateFlow<AccountDiagnostics>
    val state: StateFlow<DeviceLinkState>
    val devices: StateFlow<List<LinkedDevice>>
    /** This device itself — the row a rename gesture lands on. */
    val thisDevice: StateFlow<LinkedDevice?>
    /** Null until the native Store has answered. True with no effective peer,
     *  or while signed local revocation history lacks a fresh direct relationship. */
    val relinkAvailable: StateFlow<Boolean?>
    /** This device's relay on/off preference — device-local, default on.
     *  Off stops relayed connections only: same-network and direct sync
     *  continue, and work on this device is unaffected. */
    val relayEnabled: StateFlow<Boolean>
    /** The relay connection as the host's network runtime reads it now —
     *  the mobile mirror of the desktop tray's status row, carried on the
     *  same devices query this service already polls. Null until the host
     *  has answered (or from a host too old to say). */
    val relayStatus: StateFlow<RelayStatus?>
    /** Persist the relay preference and apply it to the live host without an
     *  app restart (iroh-relay roadmap, decision №6). */
    suspend fun setRelayEnabled(enabled: Boolean)
    suspend fun link(code: String)
    suspend fun startPairing(): PairingOffer
    suspend fun cancelPairing()
    suspend fun unlink(deviceId: String)
    suspend fun rename(name: String)
    /** Start over as a new device after [DeviceLinkState.IdentityMismatch],
     *  keeping every workspace. The old identity is set aside on disk, never
     *  deleted; it stays listed offline on the other devices until somebody
     *  unlinks it there, because there is no key left to sign a revocation
     *  with. Refused by the native host on a healthy device. */
    suspend fun resetIdentity()
    suspend fun history(deviceId: String, before: Long? = null): DeviceHistoryPage
}

/** The four readings of the relay connection. [Off] is this device's own
 *  switch; [Disconnected] covers both a relay that died after connecting and
 *  one never reached, which is why the UI words it "disconnected"/"not
 *  connected" rather than "lost". */
enum class RelayState { Off, Connecting, Connected, Disconnected }

/** One relay reading: the state, and the relay's host — empty for [RelayState.Off]
 *  and when no relay has been named yet. */
data class RelayStatus(val state: RelayState, val host: String)

data class PairingOffer(val payload: String, val expiresInSeconds: Int)
data class LinkedDevice(
    val deviceId: String,
    val displayName: String,
    val linkedAtMillis: Long,
    val connected: Boolean,
    val lastSeenMillis: Long?,
    /** The directly paired device this one arrived through, when there is no
     *  local pairing with it. Null for a directly linked device. A transitive
     *  device offers no Unlink — it disappears when the introducing
     *  relationship is revoked. */
    val introducedBy: String? = null,
    /** `version · channel · branch · OS`, as the device's latest self-signed
     *  build record puts it. Null for a device that has never said. */
    val buildLabel: String? = null,
    /** Semver drift from this device — "same", "patch", "minor", "major",
     *  "incompatible" (different peer protocol numbers: the two do not sync)
     *  or "unknown". Null when either side has not said. Decided by the
     *  native host, which holds both descriptions. */
    val buildDrift: String? = null,
    /** "warning" (a minor apart) or "danger" (a major apart, or incompatible);
     *  null for nothing to mark. */
    val buildSeverity: String? = null,
    /** Only when incompatible: "peer" or "this_device" — who has to update. */
    val updateNeededOn: String? = null,
    /** "direct" or "relay" while connected and the path is known. */
    val connection: String? = null,
    val lastSyncMillis: Long? = null,
    val history: List<DeviceStateEvent> = emptyList(),
)

data class DeviceStateEvent(
    val eventId: Long? = null,
    val atMillis: Long,
    val kind: String,
    val profileId: String? = null,
    val workspaceId: String? = null,
    val documentId: String? = null,
    val documentName: String? = null,
    val blobHash: String? = null,
    val detail: String? = null,
    val items: Int? = null,
)

data class DeviceHistoryPage(
    val deviceId: String,
    val events: List<DeviceStateEvent>,
    val nextBefore: Long? = null,
)

interface ProfileService {
    fun observeProfiles(): Flow<List<Profile>>
    suspend fun get(id: ProfileId): Profile
    suspend fun create(displayName: String, locale: String): Profile
    suspend fun rename(id: ProfileId, displayName: String): Profile
}

interface WorkspaceService {
    fun observeWorkspaces(profileId: ProfileId): Flow<List<Workspace>>
    suspend fun get(id: WorkspaceId): Workspace
    suspend fun create(profileId: ProfileId, title: String): Workspace
    suspend fun rename(id: WorkspaceId, title: String): Workspace
    suspend fun setArchived(id: WorkspaceId, archived: Boolean): Workspace
    suspend fun delete(id: WorkspaceId): Workspace
    suspend fun restore(id: WorkspaceId): Workspace
}

interface DocumentService {
    fun observeDocuments(workspaceId: WorkspaceId): Flow<List<DocumentSummary>>
    suspend fun get(id: DocumentId): DocumentSummary
    suspend fun create(workspaceId: WorkspaceId, title: String): DocumentSummary
    suspend fun rename(id: DocumentId, title: String): DocumentSummary
    suspend fun moveToTrash(id: DocumentId)
    suspend fun deletePermanently(id: DocumentId) { error("document_removal_unavailable") }
    suspend fun quiesceRemoval(prompt: DocumentRemovalPrompt, edits: List<DocumentCommand.EditBlockText>) { error("document_removal_unavailable") }
    suspend fun answerRemoval(prompt: DocumentRemovalPrompt, keep: Boolean) { error("document_removal_unavailable") }
    suspend fun open(id: DocumentId): DocumentSession
}

/**
 * A delegated run, read-only on this phone (D15): its header, one window of
 * its activity at a time, and an observer that reports a new version at the
 * document observer's own rhythm.
 */
interface RunService {
    suspend fun view(id: DocumentId): RunSnapshot
    suspend fun timeline(id: DocumentId, newestFirstFrom: Int, len: Int = WINDOW): RunTimelinePage
    fun observe(id: DocumentId): Flow<RunSnapshot>

    companion object {
        /** The window the phone draws first and pages by (proposal §9). */
        const val WINDOW = 50
    }
}

interface BlobService {
    /** **The explicit download override** (D50). A person looking at a picture
     *  held back by a metered connection may ask for that one, and it goes
     *  through the pause and to the front of the queue. Answers once the
     *  request has been made, not once the bytes have landed: the load this
     *  screen is already observing is what reports the arrival. */
    suspend fun request(reference: BlobReference) = Unit

    /**
     * The blob a `kind == "file"` document *is*.
     *
     * **Not reachable by opening the document**: a file is not a node tree, so
     * the host refuses that with `document_is_file`. This reads the manifest
     * and the file's one node instead.
     */
    suspend fun fileBlob(documentId: DocumentId, workspaceId: WorkspaceId): BlobReference? = null

    /**
     * Write one blob to a file this app owns, for handing to another app.
     *
     * **A file, not a [ByteArray].** `load` answers with the whole payload in
     * memory, which is right for a picture a screen is about to draw and fatal
     * for a video. The host streams Store to disk; this returns where it put
     * it. The bytes are fetched first if this device does not hold them, so a
     * caller gets a complete file or an error, never a short one.
     */
    suspend fun export(reference: BlobReference, fileName: String): File =
        throw AppFailure(AppErrorCode.InvalidData, "blob_export_unsupported")

    /** This Workspace's transfer state, or null while nothing has answered —
     *  counts and one stable code, never prose (§10, §15). */
    suspend fun transferStatus(workspaceId: WorkspaceId): BlobTransferSnapshot? = null

    /**
     * **Clear local files**: drop this device's copies of every file Koru Cloud
     * holds, keeping every reference. The engine asks the control plane about
     * each file before touching it, so a file Cloud has not confirmed stays
     * and is counted as kept. Null where this build has no engine.
     */
    suspend fun releaseLocal(workspaceId: WorkspaceId): ReleasedFiles? = null

    /**
     * The same thing, re-read while somebody is looking at it.
     *
     * **A read, not a wake-up.** `blob_status` is a handful of integers the
     * engine already holds, so asking again costs nothing and starts nothing:
     * this never runs a pass. D244's rule that a phone holds no timer of its
     * own while backgrounded is kept by the *collector* — the flow lives
     * exactly as long as the screen collecting it, and a backgrounded app
     * collects nothing.
     */
    fun observeTransfers(workspaceId: WorkspaceId): Flow<BlobTransferSnapshot?> = flow {
        while (true) {
            emit(transferStatus(workspaceId))
            delay(TRANSFER_POLL_MILLIS)
        }
    }

    fun observeLoad(reference: BlobReference): Flow<BlobLoadState> = flow {
        emit(BlobLoadState.CheckingLocal)
        runCatching { load(reference) }.fold(
            onSuccess = { emit(BlobLoadState.Available(it)) },
            onFailure = {
                emit(
                    BlobLoadState.Failed(
                        it.message ?: "blob_load_failed",
                        retryable = true,
                    ),
                )
            },
        )
    }
    suspend fun put(bytes: ByteArray, mimeType: String, width: Int? = null, height: Int? = null): BlobReference
    suspend fun load(reference: BlobReference): ByteArray

    companion object {
        /** Slow enough to cost nothing, fast enough that somebody watching a
         *  file move sees it move. */
        const val TRANSFER_POLL_MILLIS = 1_000L
    }
}

sealed interface BlobLoadState {
    data object CheckingLocal : BlobLoadState
    data object NoLinkedDevices : BlobLoadState
    /**
     * **Nobody reachable holds the bytes yet** — the same thing the Cloud
     * progress screen says in its own words. [holders] are the display names
     * of the devices known to hold them; empty when none is known, which reads
     * "no device online has it" rather than naming a guess.
     */
    data class WaitingForPeer(val holders: List<String>) : BlobLoadState
    data class Fetching(val receivedBytes: Long, val totalBytes: Long?) : BlobLoadState
    data class Available(val bytes: ByteArray) : BlobLoadState

    /**
     * **Held, not broken** (D50, plan §13). The connection this device is on
     * is one automatic picture transfers wait for. It clears by itself, and a
     * person may ask for this one picture anyway — which is why it is its own
     * state rather than a [Failed] with a friendlier sentence.
     */
    data class Paused(val code: String, val requestable: Boolean) : BlobLoadState

    data class Failed(val code: String, val retryable: Boolean) : BlobLoadState
}

fun interface SharedContentSource {
    fun open(): InputStream
}

data class IncomingSharedFile(
    val name: String,
    val mimeType: String,
    val declaredSize: Long?,
    val source: SharedContentSource,
    /** The share screen's optional note, saved as the file's one note. */
    val note: String? = null,
)

data class IncomingShare(val files: List<IncomingSharedFile>)

/**
 * Ingest for externally shared content.
 *
 * A saved item becomes an ordinary `kind == "file"` document in the Store, so
 * it lists in the Documents screen's Files tab and replicates like anything
 * else. There is no second projection to observe.
 */
interface WorkspaceFileService {
    suspend fun save(workspaceId: WorkspaceId, share: IncomingShare): List<DocumentSummary>
    /** The file's one note, created (empty, seeded) on first ask — *Add note*. */
    suspend fun ensureNote(file: DocumentId): DocumentId
}

interface DocumentSession {
    val documentId: DocumentId
    val snapshots: Flow<DocumentSnapshot>
    val events: Flow<DocumentEvent>
    suspend fun execute(command: DocumentCommand, operationId: String = java.util.UUID.randomUUID().toString())
    suspend fun close()
}
