// SPDX-License-Identifier: MPL-2.0

package app.koru.mobile.services

import org.json.JSONObject
import java.text.NumberFormat
import java.util.Locale

enum class CloudStorageUsage { Known, Building, Unknown, NewerSchema }
enum class CloudStorageAllowance { Finite, Unbounded, Unavailable }

/** One canonical Rust-validated response, replaced as a whole. Decimal values
 * stay signed 64-bit integers through native formatting; never Float/Double. */
@ConsistentCopyVisibility
data class CloudStorageSnapshot internal constructor(
    val usage: CloudStorageUsage,
    val allowance: CloudStorageAllowance,
    val used: Long?,
    val total: Long?,
    val available: Long?,
    val reserved: Long?,
    val evaluatedAtMillis: Long,
    val validUntilMillis: Long?,
    val revision: String,
    val policyRevision: String,
) {
    companion object {
        internal fun decode(raw: String): CloudStorageSnapshot? = runCatching {
            require(raw.length <= 32_768)
            require(raw.toByteArray(Charsets.UTF_8).size <= 32_768)
            val root = JSONObject(raw)
            require(root.getString("contract") == "cloud-plan-v1")
            val evaluated = root.decimal("evaluated_at_millis")!!
            val until = root.decimal("valid_until_millis", nullable = true)
            require(until == null || until > evaluated)
            val policy = root.getJSONObject("policy")
            val allowanceValue = policy.getJSONObject("allowance")
            val allowance = when (allowanceValue.getString("state")) {
                "finite" -> CloudStorageAllowance.Finite
                "unbounded" -> CloudStorageAllowance.Unbounded
                "unavailable" -> CloudStorageAllowance.Unavailable
                else -> error("unknown allowance")
            }
            val total = if (allowance == CloudStorageAllowance.Finite) {
                allowanceValue.decimal("base_bytes")
                allowanceValue.decimal("effective_bytes")!!
            } else null
            val value = root.getJSONObject("usage")
            require(value.getString("algorithm") == "files-plain-v0")
            val reason = value.requiredNullableString("reason")
            val usage = when (value.getString("state")) {
                "known" -> { require(reason == null); CloudStorageUsage.Known }
                "building" -> { require(reason == null); CloudStorageUsage.Building }
                "unknown" -> when (reason) {
                    "usage_unknown" -> CloudStorageUsage.Unknown
                    "cloud_schema_update_required" -> CloudStorageUsage.NewerSchema
                    else -> error("unknown usage reason")
                }
                else -> error("unknown usage state")
            }
            val used = value.decimal("used_bytes", true)
            require(value.decimal("files_bytes", true) == used)
            require(value.decimal("total_bytes", true) == total)
            val available = value.decimal("available_bytes", true)
            val reserved = value.decimal("reserved_bytes", true)
            val headroom = value.decimal("upload_headroom_bytes", true)
            if (usage == CloudStorageUsage.Known) {
                require(used != null && reserved != null)
                require(available == total?.let { (it - used).coerceAtLeast(0) })
                require(headroom == available?.let { (it - reserved).coerceAtLeast(0) })
            } else require(used == null && available == null && headroom == null)
            CloudStorageSnapshot(usage, allowance, used, total, available, reserved,
                evaluated, until, root.decimal("revision").toString(), root.decimal("policy_revision").toString())
        }.getOrNull()
    }
}

private fun JSONObject.requiredNullableString(key: String): String? {
    require(has(key))
    val value = get(key)
    if (value === JSONObject.NULL) return null
    require(value is String)
    return value
}
private fun JSONObject.decimal(key: String, nullable: Boolean = false): Long? {
    val value = requiredNullableString(key)
    if (value == null) { require(nullable); return null }
    require(value.matches(Regex("0|[1-9][0-9]{0,18}")))
    return value.toLong().also { require(it >= 0) }
}

/** Shared by the section and inventory rows, including RTL/local digit shapes. */
fun cloudStorageInteger(value: Long, locale: Locale): String =
    NumberFormat.getIntegerInstance(locale).format(value)
