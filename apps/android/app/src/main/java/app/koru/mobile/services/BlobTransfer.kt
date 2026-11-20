// SPDX-License-Identifier: MPL-2.0

package app.koru.mobile.services

import app.koru.mobile.R
import app.koru.contract.api.v1.BlobPolicy
import app.koru.contract.api.v1.BlobTransferStatus
import app.koru.contract.api.v1.ReleasedBlobs

/**
 * One Workspace's blob transfer state, as this phone reads it.
 *
 * Everything here comes from `cloud-blob-object-v1` §10's message — counts and
 * **stable codes**, never prose. The codes are branched on and the sentence a
 * person reads is this app's own, in their language; the code itself never
 * reaches the screen.
 */
data class BlobTransferSnapshot(
    val workspaceId: String,
    val referenced: Int = 0,
    val present: Int = 0,
    val durable: Int = 0,
    val uploading: Int = 0,
    val downloading: Int = 0,
    val missing: Int = 0,
    val bytesComplete: Long = 0,
    val bytesTotal: Long = 0,
    val pendingReason: String = "",
    val lastFailureCode: String = "",
    /** The engine's own coverage claim: `complete`, `incomplete`, `superseded`,
     *  or empty where this device has not computed one — which is not the same
     *  as nothing being owed. */
    val coverageState: String = "",
    /** The one field a surface may turn into a tick (§10, tag 13). */
    val current: Boolean = false,
) {
    /** Referenced minus present: the files this phone does not hold. Not a
     *  wait — since D260 they come down only when something asks. */
    val outstanding: Int get() = (referenced - present).coerceAtLeast(0)

    /**
     * Files whose bytes are moving right now, in either direction.
     *
     * **[outstanding] does not cover this, and that gap is the D259 defect.**
     * A file this device holds and is uploading has `referenced == present`,
     * so `outstanding` is zero for the whole of a 1.9 GiB upload — which is
     * how a surface came to report "up to date" over something mid-flight.
     */
    val moving: Int get() = uploading + downloading

    val phase: BlobTransferPhase get() = phaseOf(pendingReason, outstanding, moving)

    companion object {
        fun from(status: BlobTransferStatus) = BlobTransferSnapshot(
            workspaceId = status.workspaceId,
            referenced = status.referenced,
            present = status.present,
            durable = status.durable,
            uploading = status.uploading,
            downloading = status.downloading,
            missing = status.missing,
            bytesComplete = status.bytesComplete,
            bytesTotal = status.bytesTotal,
            pendingReason = status.pendingReason,
            lastFailureCode = status.lastFailureCode,
            coverageState = status.coverageState,
            current = status.current,
        )
    }
}

/**
 * The four things a person needs told apart.
 *
 * **[Paused] is not [Refused], and that distinction is the point.** §13 of the
 * blob plan: a silent pause is indistinguishable from being refused, and that
 * has already cost this project a debugging session. A pause clears when the
 * world changes — a different network, or the person allowing cellular
 * transfers — and a refusal does not.
 */
enum class BlobTransferPhase {
    /** Nothing outstanding and nothing moving. */
    Idle,

    /** Moving bytes, or about to. */
    Working,

    /** Held. [BlobHold] says by what, and whether anything a person can do
     *  clears it. */
    Paused,

    /** This Workspace moves no bytes at all: its transport mode says so. Not a
     *  pause, and no request overrides it. */
    Refused,

    /** Something went wrong and a person may be able to act on it — a device
     *  with no room is the one this build produces. */
    Failed,

    /**
     * Cloud does not hold these bytes yet, because the device that has them
     * has not uploaded them.
     *
     * **Its own state, and emphatically not [Failed].** Nothing is wrong and
     * nothing on this phone can fix it: the file arrives when the other
     * device comes online. Reported as `object_missing`, which fell through
     * to [Failed] on the first hardware run and painted a normal state in
     * [androidx.compose.material3.ColorScheme.error] — D259's lesson again,
     * one code along, where the code was right and the sentence was wrong.
     *
     * Not [Paused] either: a pause is a fact about *this* device's
     * connection and every [BlobHold] has something that clears it here.
     * This one is a fact about a machine somebody else has switched off.
     */
    Waiting,
}

/**
 * Every hold the engine can report, kept apart because each is its own
 * sentence (plan §13).
 *
 * "Waiting for an unmetered connection", "data saver is on", "there is no
 * network", "you paused this", "this workspace moves no file bytes at all" and
 * "no account has connected this to Koru Cloud" are six different things to be
 * told, and they do not clear the same way — or, for one of them, at all.
 * Collapsing any two into one sentence is the defect this enum makes hard to
 * reach by accident. §13 named the first five; the sixth is the code a build
 * with no Cloud attached reports on every pass.
 */
enum class BlobHold(val code: String) {
    /** A paid connection. D50's allowance clears it. */
    Metered("network_metered"),

    /** Android's data saver. The same allowance answers it (D50). */
    LowData("network_low_data"),

    /** No usable network. Not a permission question, so no switch helps. */
    Offline("network_offline"),

    /** This person paused this Workspace's transfers (D256). Nothing about the
     *  world clears it. */
    Person("paused_by_person"),

    /** The Workspace's transport mode moves no bytes (D245). **Not a pause**:
     *  no request overrides it and no change of network clears it. */
    Mode("mode_refuses_transport"),

    /**
     * No account has handed this process a Cloud token — either the build has
     * no Cloud attached at all (D244) or the token has aged out.
     *
     * **A sentence of its own, and not a failure sentence.** The engine files
     * this code as a failure because to a pass it is one; to a person it is
     * "this workspace is not connected to Koru Cloud", which is the ordinary
     * state of most builds and reads as alarming nonsense when it is rendered
     * as "a transfer could not finish". D259's lesson, one code along: the
     * code was right and the sentence was wrong.
     */
    Cloud("cloud_access_unavailable");

    /** The two paid-connection holds D50 gives a person a switch for. */
    val clearedByAllowingCellular: Boolean get() = this == Metered || this == LowData

    companion object {
        fun of(code: String): BlobHold? = entries.firstOrNull { it.code == code }
    }
}

/** The pauses: a fact about the connection, cleared by the world changing. */
val BLOB_PAUSE_CODES = setOf(BlobHold.Metered.code, BlobHold.LowData.code, BlobHold.Offline.code)

/** The Workspace's transport mode refusing. Never a pause (D245). */
const val BLOB_MODE_REFUSED = "mode_refuses_transport"

/** This device cannot fit the download. Bounded and visible, never silent. */
const val BLOB_NO_ROOM = "insufficient_storage"

/**
 * Cloud holds no bytes for a file this Workspace references.
 *
 * **The ordinary answer while the device holding them is shut**, not damage —
 * see [BlobTransferPhase.Waiting]. A constant rather than a [BlobHold] because
 * it is not a hold: no switch on this phone clears it and no request overrides
 * it, so putting it in that enum would offer a person a remedy that is not
 * theirs.
 */
const val BLOB_NOT_UPLOADED_YET = "object_missing"

/**
 * The one mapping from a stable code to what a person is shown.
 *
 * Kept as a function over the code rather than spread across the screens that
 * render it, so "which of these is a pause" is answered in one place and is
 * testable without a `Context`.
 *
 * **`inFlight` is neither optional nor the same as `outstanding`.** A file
 * being uploaded is already on this device, so `outstanding` is zero while it
 * moves; without this argument the answer is `Idle` — the exact shape of D259,
 * where a surface said "up to date" over a 1.9 GiB archive mid-flight.
 *
 * **`outstanding` does not make the phase `Working`** (D260). A file this
 * phone does not hold is not on its way: since D260 files come down only when
 * something asks — a picture on screen, a file being opened — so a Workspace
 * with forty files elsewhere and nothing moving is idle, and a card that said
 * "Moving files…" over it would describe a transfer that is not happening.
 * `outstanding` is kept for the count a person reads, not for the phase.
 */
fun phaseOf(pendingReason: String, outstanding: Int, inFlight: Int): BlobTransferPhase =
    when (BlobHold.of(pendingReason)) {
        BlobHold.Mode -> BlobTransferPhase.Refused
        null -> when {
            pendingReason == BLOB_NOT_UPLOADED_YET -> BlobTransferPhase.Waiting
            pendingReason.isNotEmpty() -> BlobTransferPhase.Failed
            inFlight > 0 -> BlobTransferPhase.Working
            else -> BlobTransferPhase.Idle
        }
        else -> BlobTransferPhase.Paused
    }

/**
 * One Workspace's file transfers, as a screen needs them: the counts, the one
 * hold, and whether either thing a person can do would help.
 *
 * **Pure, and it names no noun.** Every sentence is a string resource chosen
 * from these fields, so the screen carries the vocabulary and this carries the
 * arithmetic — and it is testable on a JVM with no `Context`, which is how the
 * "up to date" rule below is pinned rather than reviewed.
 */
data class BlobTransferReport(
    val phase: BlobTransferPhase,
    /** Which hold, when there is one. Five codes, five sentences. */
    val hold: BlobHold?,
    /** Files this Workspace's documents refer to — **every** file, whatever
     *  its media type. The engine's walk never consulted one (D259). */
    val referenced: Int,
    /** Of those, the ones whose bytes are on this device. */
    val onThisDevice: Int,
    /** Of those, the ones Koru Cloud has confirmed it holds. */
    val onCloud: Int,
    /** Files moving right now, in either direction. */
    val moving: Int,
    val bytesComplete: Long,
    val bytesTotal: Long,
    /**
     * **Never true while anything is in flight.**
     *
     * Three conditions, and the engine's own claim is only one of them: a
     * claim is computed per pass, and a file that began moving after it was
     * taken is a file that claim cannot know about. D259's defect was a tick
     * that survived a transfer.
     */
    val upToDate: Boolean,
    /** D50's first override — allow transfers on cellular — clears this hold. */
    val cellularWouldHelp: Boolean,
    /**
     * Whether [onThisDevice], [onCloud] and [moving] mean anything yet.
     *
     * **They are a pass's tally, not a census.** The engine resets them at the
     * start of every pass and fills them in as it resolves each file, so a
     * pass that stopped at a hold — before it asked the Store about a single
     * one — leaves all three at zero. Rendering that as "0 on this device"
     * over a Workspace whose pictures are all sitting on the phone would be a
     * worse lie than the one D259 caught, so a held pass shows them as unknown.
     */
    val countsResolved: Boolean,
    /** A stable code, shown verbatim when there is nothing better to say: it is
     *  what a person reads back to us, so prettifying it loses the only thing
     *  it is for. */
    val failureCode: String,
) {
    /** A Workspace that refers to no file and is doing nothing has nothing to
     *  report, and a card saying so is a card in the way. */
    val worthShowing: Boolean get() = referenced > 0 || phase != BlobTransferPhase.Idle
}

/**
 * **What clearing local files did**, in numbers a person can read.
 *
 * [kept] is the count that matters: every one of those is a file this phone
 * may be the only place for, as far as Cloud has confirmed, and a sentence
 * that folded it into "freed" would report a deletion that did not happen.
 */
data class ReleasedFiles(
    val referenced: Int,
    val released: Int,
    val bytesFreed: Long,
    val kept: Int,
    val notHeld: Int,
    /** The stable code the control plane stopped on, or empty. */
    val failureCode: String,
) {
    companion object {
        fun from(released: ReleasedBlobs) = ReleasedFiles(
            referenced = released.referenced,
            released = released.released,
            bytesFreed = released.bytesFreed,
            kept = released.kept,
            notHeld = released.notHeld,
            failureCode = if (released.hasErrorCode()) released.errorCode else "",
        )
    }
}

/**
 * **Whether *Free up space* is worth offering.** While this phone holds any of
 * the Workspace's files and the counts are a real tally, not a held pass's
 * zeros: a button over "— on this device" would be a promise about numbers
 * nobody has measured.
 */
fun canClearLocal(report: BlobTransferReport): Boolean =
    report.countsResolved && report.onThisDevice > 0

/** The one reading of a §10 status. */
fun blobTransferReport(snapshot: BlobTransferSnapshot): BlobTransferReport {
    val phase = snapshot.phase
    val hold = BlobHold.of(snapshot.pendingReason)
    return BlobTransferReport(
        phase = phase,
        hold = hold,
        referenced = snapshot.referenced,
        onThisDevice = snapshot.present,
        onCloud = snapshot.durable,
        moving = snapshot.moving,
        bytesComplete = snapshot.bytesComplete,
        bytesTotal = snapshot.bytesTotal,
        // The engine's claim, and then the two facts a claim taken a pass ago
        // cannot have known.
        upToDate = snapshot.current && phase == BlobTransferPhase.Idle && snapshot.moving == 0,
        cellularWouldHelp = hold?.clearedByAllowingCellular == true,
        // A waiting pass walked the Workspace and resolved every count — what
        // it could not do is fetch bytes another device has not sent. The
        // counts are the honest part of that answer and are shown.
        countsResolved = phase == BlobTransferPhase.Working ||
            phase == BlobTransferPhase.Idle ||
            phase == BlobTransferPhase.Waiting,
        failureCode = if (phase == BlobTransferPhase.Failed) snapshot.pendingReason else "",
    )
}

/**
 * This device's network, as D50 needs it stated.
 *
 * A pure function over what `ConnectivityManager` reports plus the person's
 * own allowance, so the ordering is testable on a JVM with no emulator — the
 * Android half is one callback that calls this and hands the result across the
 * shim.
 */
fun blobPolicyOf(
    connected: Boolean,
    metered: Boolean,
    dataSaver: Boolean,
    allowOnMetered: Boolean,
): BlobPolicy = BlobPolicy.newBuilder()
    .setOffline(!connected)
    .setMetered(metered)
    .setLowData(dataSaver)
    .setAllowOnMetered(allowOnMetered)
    .build()

/**
 * Peer hydration codes that mean **nobody reachable holds the bytes yet**,
 * from `koru-network-iroh`'s hydration module:
 *
 * - `blob_not_found` — every connected peer answered that it lacks the bytes
 *   and no offline device is worth waiting for. Not damage: the file exists
 *   on a device this one cannot reach, or has not reached yet.
 * - `blob_waiting_for_peer` — the hydration parked because the device that
 *   holds the bytes is offline; it carries that device's name when known.
 *
 * `no_linked_devices` is the same absence with a different remedy (link a
 * device), so it keeps its own state. Everything else — a hash, length or
 * metadata mismatch, an oversized blob, a refused path or Workspace, an
 * unreadable store — is a genuine failure and stays one.
 */
val BLOB_NOT_HELD_CODES = setOf("blob_not_found", "blob_waiting_for_peer")

/** The transient failures a person's Retry can reasonably clear. */
private val RETRYABLE_BLOB_CODES = setOf(
    "runtime_closed",
    "store_unavailable",
    "internal",
    "open",
    "read_length",
    "read_body",
    "write_length",
    "write_body",
    "finish",
    "timeout",
)

/**
 * What a picture load that produced no bytes (and no pause) shows.
 *
 * [waitingOn] is the display names of the devices known to hold the bytes, as
 * the hydration's `waiting_on` names them; empty when nobody is known, which
 * the placeholder says as "no device online has it" rather than as a failure.
 */
fun blobLoadFailure(code: String, waitingOn: List<String> = emptyList()): BlobLoadState = when (code) {
    in BLOB_NOT_HELD_CODES -> BlobLoadState.WaitingForPeer(waitingOn.distinct())
    "no_linked_devices" -> BlobLoadState.NoLinkedDevices
    else -> BlobLoadState.Failed(code, code in RETRYABLE_BLOB_CODES)
}

/**
 * The sentence for a file a person asked to open that did not arrive, chosen
 * from the host's stable code. Nobody reachable has it, nothing is linked,
 * or the engine names the hold it is under (the resource takes the code as
 * its one argument; the others ignore it).
 */
fun blobHeldMessage(code: String): Int = when (code) {
    "cloud_policy_unavailable" -> R.string.cloud_upload_policy_unavailable
    "cloud_plan_required" -> R.string.cloud_upload_plan_required
    "file_exceeds_plan_allowance" -> R.string.cloud_upload_file_allowance
    "usage_unknown" -> R.string.cloud_upload_usage_unknown
    "account_over_quota" -> R.string.cloud_upload_account_quota
    "cloud_document_count_limit" -> R.string.cloud_upload_document_count
    "cloud_stored_document_bytes_limit" -> R.string.cloud_upload_document_bytes
    "cloud_physical_bytes_limit" -> R.string.cloud_upload_physical_bytes
    "cloud_staging_bytes_limit" -> R.string.cloud_upload_staging_bytes
    "cloud_removal_capacity_limit" -> R.string.cloud_upload_removal_capacity

    in BLOB_NOT_HELD_CODES -> R.string.image_waiting_no_holder
    "no_linked_devices" -> R.string.image_link_device
    else -> R.string.file_transfers_held
}
