// SPDX-License-Identifier: MPL-2.0

package app.koru.mobile.domain

/**
 * A delegated run as this phone reads it (P1.18) — the Run document's
 * header and one window of its activity at a time, read-only (D15).
 *
 * Every token here is the run vocabulary's own snake_case spelling as the
 * native host hands it over; the screen translates a token it knows and
 * shows an honest placeholder for one it does not, so a newer desktop's
 * step never crashes an older phone.
 */

enum class RunState(val wireName: String) {
    Created("created"),
    Starting("starting"),
    Running("running"),
    Interrupted("interrupted"),
    Completed("completed"),
    Failed("failed"),
    Cancelled("cancelled"),
    AwaitingReview("awaiting_review"),
    WaitingForParticipant("waiting_for_participant"),
    /** A status this build does not know — shown as such, never hidden. */
    Unknown("unknown");

    companion object {
        fun of(token: String): RunState = entries.firstOrNull { it != Unknown && it.wireName == token } ?: Unknown
    }
}

enum class RunRating(val wireName: String) {
    Good("good"),
    NotGood("not_good");

    companion object {
        fun of(token: String?): RunRating? = entries.firstOrNull { it.wireName == token }
    }
}

/** A result, named with the device that holds its bytes (D27). */
data class RunResultSummary(
    val artifactId: String,
    val heldByDeviceId: String,
    /** The holding device's current name from this phone's catalog; null for
     *  a device this phone has never met. */
    val heldByDeviceName: String?,
    val heldHere: Boolean,
    val title: String,
    val sizeBytes: Long,
    val registeredAtMillis: Long,
)

/** The run's economics as plain numbers (D39). A null total is unavailable,
 *  never zero: `*UnavailableTurns` says how many turns did not report. */
data class RunEconomicsSummary(
    val orchestratorTurns: Int = 0,
    val humanTurns: Int = 0,
    val turnsWithUsage: Int = 0,
    val inputTokens: Long? = null,
    val inputUnavailableTurns: Int = 0,
    val outputTokens: Long? = null,
    val outputUnavailableTurns: Int = 0,
    val cachedInputTokens: Long? = null,
    val cachedInputUnavailableTurns: Int = 0,
    val toolCalls: Int = 0,
    val interventions: Int = 0,
    val elapsedMs: Long? = null,
    val models: Int = 0,
    val complete: Boolean = true,
)

data class RunSnapshot(
    val documentId: DocumentId,
    val workspaceId: WorkspaceId,
    val runId: String,
    /** Moves whenever the Run document's version does; the Activity list
     *  re-reads its windows when it moves and never on a timer of its own. */
    val revision: Long,
    val state: RunState,
    /** The status token as received, for a state this build does not know. */
    val stateToken: String,
    /** approval_required | waiting_for_input | child_session_detected |
     *  finished_needing_attention, while running. */
    val attention: List<String> = emptyList(),
    /** orchestrator | handover_pending | human, while running. */
    val control: String? = null,
    val failureStage: String? = null,
    val priorAttempt: String? = null,
    val agent: String,
    val objective: String,
    val scopeKind: String,
    val contextCount: Int,
    val executingDeviceId: String,
    val executingDeviceName: String?,
    /** Whether this phone is the executing device (D16 names it otherwise). */
    val executingHere: Boolean,
    val createdAtMillis: Long,
    val results: List<RunResultSummary> = emptyList(),
    val economics: RunEconomicsSummary = RunEconomicsSummary(),
    val rating: RunRating? = null,
    val ratingNote: String? = null,
    val ratedAtMillis: Long? = null,
    val timelineLen: Int,
    /** The run was written by a newer Koru: what decoded is shown, and the
     *  screen says a newer Koru is needed for the rest. */
    val needsNewerKoru: Boolean = false,
)

/** One entry of a run's activity (D45): ids, a token, a time and bounded
 *  counts — never content. */
sealed interface RunTimelineEntry {
    data class Step(
        val id: String,
        val atMillis: Long,
        /** The step token as `StepKind` spells it: run_created, started,
         *  turn_started, tools_completed, … */
        val step: String,
        val originator: String? = null,
        val calls: Int? = null,
        val failed: Int? = null,
        val outcome: String? = null,
        val artifactsRegistered: Int? = null,
        val reason: String? = null,
        val stage: String? = null,
        val priorAttempt: String? = null,
        val via: String? = null,
        val discovered: Boolean? = null,
        val rating: RunRating? = null,
        val eventsLost: Int? = null,
        val refs: Int = 0,
    ) : RunTimelineEntry

    data class Usage(
        val id: String,
        val inputTokens: Long?,
        val outputTokens: Long?,
        val cachedInputTokens: Long?,
        val revision: Int,
        val completeness: String?,
    ) : RunTimelineEntry

    /** An entry this build cannot decode, kept in its place. */
    data object Gap : RunTimelineEntry
}

data class RunTimelinePage(
    val documentId: DocumentId,
    val newestFirstFrom: Int,
    val entries: List<RunTimelineEntry>,
    val timelineLen: Int,
    val revision: Long,
)

/** The runs a document's pills point at — what the viewer resolves so a pill
 *  can show the run's status and executing device (D16). */
fun DocumentSnapshot.runReferences(): Set<DocumentId> = blocks.flatMap { block ->
    val inline = when (val content = block.content) {
        is BlockContent.Paragraph -> content.richText.inlineObjects
        is BlockContent.Quote -> content.richText.inlineObjects
        is BlockContent.Task -> content.richText.inlineObjects
        is BlockContent.Code -> content.richText.inlineObjects
        is BlockContent.Table -> content.richText.inlineObjects
        is BlockContent.Contact -> content.name.inlineObjects
        else -> emptyList()
    }.mapNotNull { (it.reference as? EntityReference.Run)?.documentId }
    val embedded = ((block.content as? BlockContent.Embed)?.reference as? EntityReference.Run)?.documentId
    inline + listOfNotNull(embedded)
}.toSet()
