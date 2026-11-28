// SPDX-License-Identifier: MPL-2.0

package app.koru.mobile.ui.theme

import android.content.Context
import android.graphics.Typeface as AndroidTypeface
import androidx.annotation.StringRes
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.*
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.*
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import app.koru.mobile.R
import java.util.Locale

object KoruSpacing {
    val xs = 4.dp
    val sm = 8.dp
    val md = 16.dp
    val lg = 24.dp
    val xl = 32.dp
}

/**
 * The three appearance choices. `key` is what is stored — a semantic value,
 * never the drawn word — and `labelRes` is what is drawn; the picker used to
 * print `name`, which is a Kotlin identifier and English by accident.
 */
enum class KoruThemePreference(val key: String, @StringRes val labelRes: Int) {
    System("system", R.string.appearance_system),
    Light("light", R.string.appearance_light),
    Dark("dark", R.string.appearance_dark);
    companion object { fun fromKey(value: String?) = entries.firstOrNull { it.key == value } ?: System }
}

/**
 * The font picker's six choices.
 *
 * `label` is the name **as it is written everywhere**: Inter and Manrope are
 * product names and are not translated, so they carry their own literal. The
 * four system families are described rather than named — "System serif" is a
 * sentence about the phone, not a typeface anybody calls that — so they carry
 * a resource id instead and `labelRes` is what the picker draws when it is
 * there. Two fields rather than one so the difference is visible: a null
 * `labelRes` says *this name is the same in every language*, which is a claim,
 * not an omission.
 */
enum class KoruFontChoice(
    val key: String,
    val label: String,
    @StringRes val labelRes: Int?,
    private val androidFamily: String,
) {
    Inter("inter", "Inter", null, "sans-serif"),
    Manrope("manrope", "Manrope", null, "sans-serif"),
    Sans("sans", "System sans", R.string.font_system_sans, "sans-serif"),
    Serif("serif", "System serif", R.string.font_system_serif, "serif"),
    Rounded("rounded", "System rounded", R.string.font_system_rounded, "sans-serif-rounded"),
    Mono("mono", "System mono", R.string.font_system_mono, "monospace");

    companion object { fun fromKey(value: String?) = entries.firstOrNull { it.key == value } ?: Inter }

    internal fun composeFamily(context: Context): FontFamily = when (this) {
        Inter -> FontFamily(
            Font("Inter-Regular.ttf", context.assets, FontWeight.Normal),
            Font("Inter-Medium.ttf", context.assets, FontWeight.Medium),
        )
        Manrope -> FontFamily(Font("Manrope-SemiBold.ttf", context.assets, FontWeight.SemiBold))
        else -> FontFamily(AndroidTypeface.create(androidFamily, AndroidTypeface.NORMAL))
    }
}

data class AppearancePreferences(
    val theme: KoruThemePreference = KoruThemePreference.System,
    val font: KoruFontChoice = KoruFontChoice.Inter,
    val language: String = Locale.getDefault().toLanguageTag(),
)

object AppearanceStore {
    private const val StoreName = "appearance"
    fun load(context: Context): AppearancePreferences {
        val store = context.getSharedPreferences(StoreName, Context.MODE_PRIVATE)
        return AppearancePreferences(
            theme = KoruThemePreference.fromKey(store.getString("theme", null)),
            font = KoruFontChoice.fromKey(store.getString("font", store.getString("app_font", null))),
            language = store.getString("language", null) ?: Locale.getDefault().toLanguageTag(),
        )
    }

    fun save(context: Context, value: AppearancePreferences) {
        context.getSharedPreferences(StoreName, Context.MODE_PRIVATE).edit()
            .putString("theme", value.theme.key)
            .putString("font", value.font.key)
            .putString("language", value.language)
            .apply()
    }

    @Suppress("DEPRECATION")
    fun applyLanguage(context: Context, language: String) {
        val locale = Locale.forLanguageTag(language)
        Locale.setDefault(locale)
        val configuration = context.resources.configuration
        if (configuration.locales[0] != locale) {
            configuration.setLocale(locale)
            context.resources.updateConfiguration(configuration, context.resources.displayMetrics)
        }
    }
}

@Composable fun rememberKoruFontFamily(choice: KoruFontChoice): FontFamily {
    val context = LocalContext.current
    return remember(choice, context.assets) { choice.composeFamily(context) }
}

private val Light = lightColorScheme(
    background = Color.White, surface = Color.White,
    onBackground = Color(0xFF000000), onSurface = Color(0xFF000000),
    primary = Color(0xFF3B6BB5), onPrimary = Color.White,
    secondaryContainer = Color(0xFFF2F2F2), outline = Color(0xFFE0E0E0),
    surfaceVariant = Color(0xFFF1F1F3),
)
private val Dark = darkColorScheme(
    background = Color(0xFF191919), surface = Color(0xFF1E1E1E),
    onBackground = Color(0xFFF5F5F5), onSurface = Color(0xFFF5F5F5),
    primary = Color(0xFF6FA3D8), onPrimary = Color(0xFF121212),
    secondaryContainer = Color(0xFF2C2C2C), outline = Color(0xFF333333),
    surfaceVariant = Color(0xFF232323),
)

@Composable fun KoruTheme(
    appearance: AppearancePreferences = AppearancePreferences(),
    content: @Composable () -> Unit,
) {
    val dark = when (appearance.theme) {
        KoruThemePreference.System -> isSystemInDarkTheme()
        KoruThemePreference.Light -> false
        KoruThemePreference.Dark -> true
    }
    val family = rememberKoruFontFamily(appearance.font)
    val base = Typography()
    val typography = base.copy(
        headlineSmall = TextStyle(fontFamily = family, fontWeight = FontWeight.SemiBold, fontSize = 24.sp, lineHeight = 32.sp),
        bodyLarge = TextStyle(fontFamily = family, fontWeight = FontWeight.Normal, fontSize = 16.sp, lineHeight = 24.sp),
        bodyMedium = TextStyle(fontFamily = family, fontWeight = FontWeight.Medium, fontSize = 14.sp, lineHeight = 20.sp),
        titleLarge = base.titleLarge.copy(fontFamily = family),
        titleMedium = base.titleMedium.copy(fontFamily = family),
        titleSmall = base.titleSmall.copy(fontFamily = family),
        labelLarge = base.labelLarge.copy(fontFamily = family),
        labelMedium = base.labelMedium.copy(fontFamily = family),
        labelSmall = base.labelSmall.copy(fontFamily = family),
    )
    MaterialTheme(
        colorScheme = if (dark) Dark else Light,
        typography = typography,
        content = content,
    )
}
