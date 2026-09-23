/**
 * Resume/watched state storage for Android — implements `:core`'s
 * [WatchStateStore] using plain `SharedPreferences` (not the
 * `EncryptedSharedPreferences` `AndroidTokenStore` uses): a playback
 * position isn't a secret the way an access token is, so there's no
 * reason to pay Keystore encryption overhead for it. One JSON value per
 * fingerprint, reusing the same `SwarmJson` config every wire type in this
 * app already uses. Episode values include show/season/episode identity for
 * replacement-file recovery, and writes are serialized newest-first so an
 * older heartbeat cannot finish after a teardown save and roll it backward.
 */
package app.swarm.tv.app.data

import android.content.Context
import app.swarm.tv.core.rest.SwarmJson
import app.swarm.tv.core.watch.WatchState
import app.swarm.tv.core.watch.WatchStateStore
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import kotlinx.serialization.decodeFromString
import kotlinx.serialization.encodeToString

private const val PREFS_NAME = "swarm_watch_state"

class AndroidWatchStateStore(context: Context) : WatchStateStore {
    private val prefs = context.applicationContext.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
    private val writeMutex = Mutex()

    override suspend fun get(fingerprint: String): WatchState? = withContext(Dispatchers.IO) {
        prefs.getString(fingerprint, null)?.let { json ->
            runCatching { SwarmJson.decodeFromString<WatchState>(json).withCurrentCompletionRule() }.getOrNull()
        }
    }

    override suspend fun all(): Map<String, WatchState> = withContext(Dispatchers.IO) {
        prefs.all.mapNotNull { (fingerprint, value) ->
            val json = value as? String ?: return@mapNotNull null
            runCatching {
                fingerprint to SwarmJson.decodeFromString<WatchState>(json).withCurrentCompletionRule()
            }.getOrNull()
        }.toMap()
    }

    override suspend fun set(fingerprint: String, state: WatchState) = withContext(Dispatchers.IO) {
        writeMutex.withLock {
            val current = prefs.getString(fingerprint, null)?.let { json ->
                runCatching { SwarmJson.decodeFromString<WatchState>(json) }.getOrNull()
            }
            // Heartbeat, lifecycle, and Compose-disposal saves are launched
            // independently. An older IO task must never finish last and roll
            // a newer playback position backward.
            if (current != null && current.updatedAt > state.updatedAt) return@withLock
            prefs.edit().putString(fingerprint, SwarmJson.encodeToString(state)).apply()
        }
    }

    override suspend fun clear(fingerprint: String) = withContext(Dispatchers.IO) {
        writeMutex.withLock { prefs.edit().remove(fingerprint).apply() }
    }
}

/** Re-evaluates records written by older builds that used a 90% threshold. */
private fun WatchState.withCurrentCompletionRule(): WatchState =
    WatchState.fromPlayback(positionSecs, durationSecs, updatedAt, showTitle, season, episode)
