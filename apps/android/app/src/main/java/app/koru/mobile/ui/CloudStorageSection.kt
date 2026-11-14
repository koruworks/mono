// SPDX-License-Identifier: MPL-2.0

package app.koru.mobile.ui

import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalConfiguration
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import app.koru.mobile.R
import app.koru.mobile.services.*
import app.koru.mobile.ui.theme.KoruSpacing
import java.text.DateFormat
import java.util.Date

/** Native account-screen composition: text carries every meaning, exact values
 * wrap with enlarged fonts, and refresh never changes keyboard/TalkBack focus. */
@Composable
internal fun CloudStorageSection(state: CloudStorageState, refresh: () -> Unit, files: () -> Unit) {
    val locale = LocalConfiguration.current.locales[0]
    val snapshot = state.snapshot
    var details by remember(state.ownerScope, state.accountId) { mutableStateOf(false) }
    Column(Modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(KoruSpacing.sm)) {
        Text(stringResource(R.string.cloud_storage_title), style = MaterialTheme.typography.titleLarge,
            modifier = Modifier.semantics { heading() })
        val status = when {
            state.stale && snapshot != null -> stringResource(R.string.cloud_storage_stale,
                DateFormat.getDateTimeInstance(DateFormat.SHORT, DateFormat.SHORT, locale).format(Date(snapshot.evaluatedAtMillis)))
            snapshot == null && state.failed -> stringResource(R.string.cloud_storage_failed)
            snapshot == null || state.loading -> stringResource(R.string.cloud_storage_checking)
            snapshot.usage == CloudStorageUsage.Building -> stringResource(R.string.cloud_storage_calculating)
            snapshot.usage == CloudStorageUsage.Unknown -> stringResource(R.string.cloud_storage_unknown)
            snapshot.usage == CloudStorageUsage.NewerSchema -> stringResource(R.string.cloud_storage_schema)
            else -> null
        }
        if (status != null) Text(status, modifier = Modifier.semantics { liveRegion = LiveRegionMode.Polite })
        if (snapshot != null) {
            when (snapshot.allowance) {
                CloudStorageAllowance.Unbounded -> Text(stringResource(R.string.cloud_storage_no_limit))
                CloudStorageAllowance.Unavailable -> Text(stringResource(R.string.cloud_storage_allowance_unknown))
                CloudStorageAllowance.Finite -> if (snapshot.available != null) {
                    Text(stringResource(R.string.cloud_storage_available,
                        cloudStorageInteger(snapshot.available, locale), cloudStorageInteger(snapshot.total!!, locale)))
                } else Text(stringResource(R.string.cloud_storage_total, cloudStorageInteger(snapshot.total!!, locale)))
            }
            snapshot.used?.let { Text(stringResource(R.string.cloud_storage_used, cloudStorageInteger(it, locale))) }
        }
        Text(stringResource(R.string.cloud_storage_explanation), style = MaterialTheme.typography.bodySmall)
        TextButton(onClick = files) { Text(stringResource(R.string.cloud_inventory_view)) }
        TextButton(onClick = refresh, enabled = !state.loading) {
            Text(stringResource(if (state.failed || state.stale || snapshot?.usage != CloudStorageUsage.Known ||
                snapshot.allowance == CloudStorageAllowance.Unavailable) R.string.account_retry else R.string.cloud_storage_refresh))
        }
        if (snapshot != null) {
            TextButton(onClick = { details = !details }) { Text(stringResource(R.string.cloud_storage_details)) }
            if (details) {
                snapshot.reserved?.let { Text(stringResource(R.string.cloud_storage_reserved, cloudStorageInteger(it, locale))) }
                Text(stringResource(R.string.cloud_storage_checked,
                    DateFormat.getDateTimeInstance(DateFormat.SHORT, DateFormat.SHORT, locale).format(Date(snapshot.evaluatedAtMillis))))
            }
        }
    }
}
