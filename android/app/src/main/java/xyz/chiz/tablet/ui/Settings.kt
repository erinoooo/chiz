package xyz.chiz.tablet.ui

import android.content.Context
import androidx.datastore.core.DataStore
import androidx.datastore.preferences.core.Preferences
import androidx.datastore.preferences.core.booleanPreferencesKey
import androidx.datastore.preferences.core.floatPreferencesKey
import androidx.datastore.preferences.core.intPreferencesKey
import androidx.datastore.preferences.core.stringPreferencesKey
import androidx.datastore.preferences.preferencesDataStore

/** Local settings (spec 6 table) in Jetpack DataStore. */
val Context.settingsStore: DataStore<Preferences> by preferencesDataStore(name = "chiz_settings")

object SettingsKeys {
    val STRIP_EDGE = stringPreferencesKey("strip_edge") // left|right|top|bottom
    val STRIP_SIZE = intPreferencesKey("strip_size") // % of shorter side, 10..35
    val ORIENTATION = stringPreferencesKey("orientation")
    val BUTTON_GUARD = booleanPreferencesKey("button_guard")
    val DIM_SCREEN = booleanPreferencesKey("dim_screen")
    val FEEDBACK_MODE = stringPreferencesKey("feedback_mode") // both|earcons|speech|off
    val SPEECH_RATE = floatPreferencesKey("speech_rate") // 0.5..4.0
    val FEEDBACK_VOLUME = intPreferencesKey("feedback_volume") // 0..100
    val ANNOUNCE_MODE = stringPreferencesKey("announce_mode") // rest|touch
    val REST_DELAY = intPreferencesKey("rest_delay") // ms, 150..600
    val SHOW_LABELS = booleanPreferencesKey("show_labels")
}

object SettingsDefaults {
    const val STRIP_EDGE = "left"
    const val STRIP_SIZE = 16
    const val ORIENTATION = "landscape"
    const val BUTTON_GUARD = true
    const val DIM_SCREEN = true
    const val FEEDBACK_MODE = "both"
    const val SPEECH_RATE = 2.0f
    const val FEEDBACK_VOLUME = 70
    const val ANNOUNCE_MODE = "rest"
    const val REST_DELAY = 250
    const val SHOW_LABELS = true
}
