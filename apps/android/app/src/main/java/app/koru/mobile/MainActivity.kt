// SPDX-License-Identifier: MPL-2.0

package app.koru.mobile

import android.os.Bundle
import android.content.Intent
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.runtime.getValue
import androidx.compose.runtime.setValue
import androidx.lifecycle.viewmodel.compose.viewModel
import app.koru.mobile.ui.KoruApp
import app.koru.mobile.ui.theme.KoruTheme
import app.koru.mobile.ui.theme.AppearanceStore
import app.koru.mobile.logging.AppLogger

class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        AppLogger.peerObjectsEnabled = intent?.getBooleanExtra("koru_peer_object_logs", false) == true
        AppLogger.accountEnabled = intent?.getBooleanExtra("koru_account_logs", false) == true
        val container = (application as KoruApplication).container
        val initialAppearance = AppearanceStore.load(applicationContext)
        AppearanceStore.applyLanguage(this, initialAppearance.language)
        container.acceptLinkIntent(intent?.data)
        setContent {
            val model: AppViewModel = viewModel { AppViewModel(container) }
            var appearance by androidx.compose.runtime.remember {
                androidx.compose.runtime.mutableStateOf(initialAppearance)
            }
            KoruTheme(appearance) {
                KoruApp(model, container.serviceImplementation, appearance) { updated ->
                    val languageChanged = updated.language != appearance.language
                    appearance = updated
                    AppearanceStore.save(applicationContext, updated)
                    if (languageChanged) {
                        AppearanceStore.applyLanguage(this, updated.language)
                        recreate()
                    }
                }
            }
        }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        AppLogger.peerObjectsEnabled = intent.getBooleanExtra("koru_peer_object_logs", false)
        AppLogger.accountEnabled = intent.getBooleanExtra("koru_account_logs", false)
        (application as KoruApplication).container.acceptLinkIntent(intent.data)
    }

    override fun onStart() {
        super.onStart()
        (application as KoruApplication).container.account.foreground(this)
        (application as KoruApplication).container.updates.checkIfDue()
    }

    override fun onStop() {
        (application as KoruApplication).container.account.background(this)
        super.onStop()
    }
}
