// SPDX-License-Identifier: MPL-2.0

package app.koru.mobile.services

import org.json.JSONObject
import java.util.UUID

data class CloudInventoryAmount(val state: CloudStorageUsage, val bytes: Long?)
enum class CloudPlacement { Assigned, Unassigned, Unknown }
data class CloudWorkspaceRow(
    val workspace: String, val incarnation: String, val sourceRevision: String,
    val name: String?, val placement: CloudPlacement, val profile: String?,
    val amount: CloudInventoryAmount,
)
data class CloudWorkspaceEpoch(val revision: String, val generation: String?, val catalog: String)
data class CloudDocumentEpoch(
    val revision: String, val generation: String?, val workspace: String, val incarnation: String,
    val sourceRevision: String, val inventory: String, val manifestState: String,
    val manifestReason: String?, val manifestHead: String?, val manifestProof: String?,
)
enum class CloudDocumentMembership { Listed, Trash, Unfiled, Structural, Unknown }
data class CloudDocumentRow(
    val unit: String, val document: String?, val head: String?, val name: String?,
    val membership: CloudDocumentMembership, val amount: CloudInventoryAmount,
)
sealed interface CloudInventoryPage {
    val nextCursor: String?
    data class Workspaces(val epoch: CloudWorkspaceEpoch, val rows: List<CloudWorkspaceRow>,
                          override val nextCursor: String?) : CloudInventoryPage
    data class Documents(val epoch: CloudDocumentEpoch, val rows: List<CloudDocumentRow>,
                         override val nextCursor: String?) : CloudInventoryPage
}

/** Only projections of the bounded canonical DTO emitted by Rust. Epochs are
 * retained intact even though revisions and proof digests are never UI text. */
internal object CloudInventoryDecoder {
    fun decode(raw: String): CloudInventoryPage? = runCatching {
        require(raw.length <= 1_048_576 && raw.toByteArray(Charsets.UTF_8).size <= 1_048_576)
        val root = JSONObject(raw)
        val cursor = root.nullableString("next_cursor")?.also { requireCursor(it) }
        val revision = root.decimalString("revision")
        val generation = root.uuid("generation", true)
        val rows = root.getJSONArray("items")
        require(rows.length() <= 100)
        when (root.getString("contract")) {
            "cloud-usage-workspaces-v1" -> {
                val epoch = CloudWorkspaceEpoch(revision, generation, root.digest("catalog_revision_digest")!!)
                val items = (0 until rows.length()).map { index ->
                    val row = rows.getJSONObject(index)
                    val placement = row.getJSONObject("placement")
                    val state = when (placement.getString("state")) {
                        "assigned" -> CloudPlacement.Assigned
                        "unassigned" -> CloudPlacement.Unassigned
                        "unknown" -> CloudPlacement.Unknown
                        else -> error("placement unavailable")
                    }
                    val profile = placement.uuid("profile_id", true)
                    require((state == CloudPlacement.Assigned) == (profile != null))
                    CloudWorkspaceRow(row.uuid("workspace_id")!!, row.uuid("incarnation_id")!!,
                        row.decimalString("source_revision"), row.label("display_name", 1024), state, profile,
                        amount(row.getJSONObject("usage")))
                }
                require(items.map { it.workspace }.distinct().size == items.size)
                CloudInventoryPage.Workspaces(epoch, items, cursor)
            }
            "cloud-usage-documents-v1" -> {
                require(root.getString("algorithm") == "files-plain-v0" && root.getString("row_totals") == "non_additive")
                root.decimalString("evaluated_at_millis")
                val manifest = root.getJSONObject("manifest")
                val state = manifest.getString("state")
                val reason = manifest.nullableString("reason")
                val head = manifest.digest("source_head_digest", true)
                val proof = manifest.digest("proof_digest", true)
                when (state) {
                    "verified" -> require(reason == null && head != null && proof != null)
                    "unknown" -> require(reason in setOf("manifest_unavailable", "cloud_schema_update_required") && proof == null)
                    else -> error("manifest unavailable")
                }
                val epoch = CloudDocumentEpoch(revision, generation, root.uuid("workspace_id")!!,
                    root.uuid("incarnation_id")!!, root.decimalString("source_revision"),
                    root.digest("inventory_digest")!!, state, reason, head, proof)
                val items = (0 until rows.length()).map { index ->
                    val row = rows.getJSONObject(index)
                    val membership = row.getJSONObject("membership")
                    val member = when (membership.getString("state")) {
                        "listed" -> CloudDocumentMembership.Listed
                        "trash" -> CloudDocumentMembership.Trash
                        "unfiled" -> CloudDocumentMembership.Unfiled
                        "structural" -> CloudDocumentMembership.Structural
                        "unknown" -> CloudDocumentMembership.Unknown
                        else -> error("membership unavailable")
                    }
                    val memberReason = membership.nullableString("reason")
                    require(if (member == CloudDocumentMembership.Unknown)
                        memberReason in setOf("manifest_unavailable", "cloud_schema_update_required", "unit_identity_unknown")
                        else memberReason == null)
                    require(state == "verified" || member in setOf(CloudDocumentMembership.Unknown, CloudDocumentMembership.Structural))
                    val document = row.uuid("document_id", true)
                    val name = row.label("display_name", 128)
                    require(name == null || member in setOf(CloudDocumentMembership.Listed, CloudDocumentMembership.Trash))
                    val amount = amount(row.getJSONObject("usage"))
                    when (row.getString("unit_role")) {
                        "unknown" -> require(document == null && member == CloudDocumentMembership.Unknown &&
                            memberReason == "unit_identity_unknown" && amount.state != CloudStorageUsage.Known)
                        "content" -> require(document != null && member != CloudDocumentMembership.Structural)
                        "structure" -> require(document != null && member == CloudDocumentMembership.Structural &&
                            (amount.state != CloudStorageUsage.Known || amount.bytes == 0L))
                        else -> error("unit unavailable")
                    }
                    val sourceHead = row.digest("source_head_digest", true)
                    require(amount.state != CloudStorageUsage.Known || (sourceHead != null && generation != null))
                    require(amount.state != CloudStorageUsage.Building || generation == null)
                    CloudDocumentRow(row.uuid("cloud_unit_id")!!, document, sourceHead, name, member, amount)
                }
                require(items.zipWithNext().all { (a, b) -> a.unit < b.unit })
                CloudInventoryPage.Documents(epoch, items, cursor)
            }
            else -> error("inventory unavailable")
        }
    }.getOrNull()

    internal fun requireCursor(cursor: String) {
        require(cursor.length in 1..2048 && cursor.all { it in 'a'..'z' || it in 'A'..'Z' || it in '0'..'9' || it == '_' || it == '-' })
    }
    private fun amount(value: JSONObject): CloudInventoryAmount {
        val reason = value.nullableString("reason")
        val bytes = value.nullableString("files_bytes")?.let(::decimal)
        val state = when (value.getString("state")) {
            "known" -> { require(reason == null && bytes != null); CloudStorageUsage.Known }
            "building" -> { require(reason == null && bytes == null); CloudStorageUsage.Building }
            "unknown" -> { require(bytes == null); when (reason) {
                "usage_unknown" -> CloudStorageUsage.Unknown
                "cloud_schema_update_required" -> CloudStorageUsage.NewerSchema
                else -> error("usage unavailable")
            } }
            else -> error("usage unavailable")
        }
        return CloudInventoryAmount(state, bytes)
    }
    private fun decimal(value: String): Long {
        require(value.matches(Regex("0|[1-9][0-9]{0,18}")))
        return value.toLong().also { require(it >= 0) }
    }
    private fun JSONObject.decimalString(key: String) = nullableString(key)!!.also { decimal(it) }
    private fun JSONObject.nullableString(key: String): String? {
        val value = get(key)
        if (value === JSONObject.NULL) return null
        require(value is String)
        return value
    }
    private fun JSONObject.uuid(key: String, nullable: Boolean = false): String? = nullableString(key).also {
        if (it == null) require(nullable) else require(UUID.fromString(it).toString() == it)
    }
    private fun JSONObject.digest(key: String, nullable: Boolean = false): String? = nullableString(key).also {
        if (it == null) require(nullable) else require(it.matches(Regex("[0-9a-f]{64}")))
    }
    private fun JSONObject.label(key: String, max: Int) = nullableString(key).also {
        require(it == null || it.codePointCount(0, it.length) <= max)
    }
}

/** Totals only for the whole inventory in one page; historical bytes and
 * unknown placement never become a current Profile subtotal. */
internal fun CloudInventoryPage.Workspaces.profileSubtotal(profile: String, firstPage: Boolean): Long? {
    if (!firstPage || nextCursor != null || rows.any { it.placement == CloudPlacement.Unknown }) return null
    val members = rows.filter { it.profile == profile && it.placement == CloudPlacement.Assigned }
    if (members.isEmpty() || members.any { it.amount.state != CloudStorageUsage.Known }) return null
    return runCatching { members.fold(0L) { sum, row -> Math.addExact(sum, row.amount.bytes!!) } }.getOrNull()
}
