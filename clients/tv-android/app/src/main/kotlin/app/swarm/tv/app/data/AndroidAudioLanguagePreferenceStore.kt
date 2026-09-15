/** Device-local audio-language preferences for episodic playback. */
package app.swarm.tv.app.data

import android.content.Context
import java.util.Locale

private const val AUDIO_LANGUAGE_PREFS_NAME = "swarm_audio_language_preferences"

class AndroidAudioLanguagePreferenceStore(context: Context) {
    private val prefs = context.applicationContext.getSharedPreferences(
        AUDIO_LANGUAGE_PREFS_NAME,
        Context.MODE_PRIVATE,
    )

    fun get(showTitle: String): String? = prefs.getString(showKey(showTitle), null)

    fun set(showTitle: String, audioLanguage: String) {
        // apply() updates the in-memory preferences immediately, so an
        // auto-advanced episode can read this choice without waiting for the
        // asynchronous disk write to finish.
        prefs.edit().putString(showKey(showTitle), audioLanguage).apply()
    }

    private fun showKey(showTitle: String): String =
        "show:${showTitle.trim().lowercase(Locale.ROOT)}"
}
