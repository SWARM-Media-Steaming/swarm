package app.swarm.tv.app.data

import android.content.Context

/** Device local Audio Boost choice. Values are the supported UI levels in dB. */
class AndroidAudioBoostStore(context: Context) {
    private val prefs = context.applicationContext.getSharedPreferences("swarm_audio_boost", Context.MODE_PRIVATE)

    fun get(): Int = prefs.getInt(KEY, 0).takeIf { it in LEVELS } ?: 0

    fun set(levelDb: Int) {
        prefs.edit().putInt(KEY, levelDb.takeIf { it in LEVELS } ?: 0).apply()
    }

    companion object {
        val LEVELS = listOf(0, 3, 6, 10)
        private const val KEY = "level_db"
    }
}
