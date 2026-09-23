/**
 * Resume + watched state for one catalog entry, keyed by its cross-server
 * fingerprint (not a per-server entry key — the plan's "single profile,
 * resume + watched" scope, and a fingerprint is what stays stable across
 * which source a merged entry happens to stream from). The real
 * implementation (`SharedPreferences`-backed) lives in `:app` since it
 * needs Android framework APIs unavailable here; this interface is what
 * the rest of `:core` — and any test — programs against.
 *
 * Deliberately not Room, despite the project plan naming a "client-local
 * Room table": every other piece of local state in this app
 * (`TokenStore`) already uses a plain key/value store, not a database, for
 * data this simple (one JSON blob per fingerprint) — adding another Room
 * entity is not a useful trade. The Android implementation can snapshot its
 * small dedicated preference file for Continue Watching, while lookup by
 * fingerprint remains the playback hot path.
 */
package app.swarm.tv.core.watch

import app.swarm.tv.core.peer.CatalogEntry
import app.swarm.tv.core.peer.MediaKind
import kotlinx.serialization.Serializable

@Serializable
data class WatchState(
    val positionSecs: Double,
    val durationSecs: Double,
    val watched: Boolean,
    val updatedAt: Long,
    /**
     * Episode identity is snapshotted alongside the fingerprint so a media
     * replacement/rescan that changes the file fingerprint does not send a
     * show back to an older episode. Movies and tracks leave these null.
     */
    val showTitle: String? = null,
    val season: Int? = null,
    val episode: Int? = null,
) {
    companion object {
        /** Credits commonly start before the media timeline reaches 100%. */
        const val WATCHED_FRACTION = 0.95

        fun fromPlayback(
            positionSecs: Double,
            durationSecs: Double,
            updatedAt: Long,
            showTitle: String? = null,
            season: Int? = null,
            episode: Int? = null,
        ): WatchState {
            val watched = durationSecs > 0 && positionSecs / durationSecs >= WATCHED_FRACTION
            return WatchState(positionSecs, durationSecs, watched, updatedAt, showTitle, season, episode)
        }
    }
}

private data class EpisodeIdentity(val show: String, val season: Int, val episode: Int)

private fun CatalogEntry.episodeIdentity(): EpisodeIdentity? {
    if (kind != MediaKind.EPISODE) return null
    val show = showTitle?.trim()?.lowercase()?.takeIf { it.isNotEmpty() } ?: return null
    return EpisodeIdentity(show, season ?: return null, episode ?: return null)
}

private fun WatchState.episodeIdentity(): EpisodeIdentity? {
    val show = showTitle?.trim()?.lowercase()?.takeIf { it.isNotEmpty() } ?: return null
    return EpisodeIdentity(show, season ?: return null, episode ?: return null)
}

/**
 * Resolves state by fingerprint first, then by a captured show/season/episode
 * identity. The fallback preserves resume state when the same logical episode
 * is replaced by a differently encoded file with a new content fingerprint.
 */
fun Map<String, WatchState>.stateFor(entry: CatalogEntry): WatchState? {
    this[entry.fingerprint]?.let { return it }
    val identity = entry.episodeIdentity() ?: return null
    return values.asSequence()
        .filter { it.episodeIdentity() == identity }
        .maxByOrNull { it.updatedAt }
}

/** Store-backed counterpart to [stateFor], keeping fingerprint lookup hot. */
suspend fun WatchStateStore.getForEntry(entry: CatalogEntry): WatchState? =
    get(entry.fingerprint) ?: entry.episodeIdentity()?.let { identity ->
        all().values.asSequence()
            .filter { it.episodeIdentity() == identity }
            .maxByOrNull { it.updatedAt }
    }

interface WatchStateStore {
    suspend fun get(fingerprint: String): WatchState?
    suspend fun all(): Map<String, WatchState>
    /** An older [WatchState.updatedAt] must not replace a newer record. */
    suspend fun set(fingerprint: String, state: WatchState)
    suspend fun clear(fingerprint: String)
}

/** Test double / placeholder — never used for a real device's watch state. */
class InMemoryWatchStateStore : WatchStateStore {
    private val states = mutableMapOf<String, WatchState>()

    override suspend fun get(fingerprint: String): WatchState? = states[fingerprint]

    override suspend fun all(): Map<String, WatchState> = states.toMap()

    override suspend fun set(fingerprint: String, state: WatchState) {
        if ((states[fingerprint]?.updatedAt ?: Long.MIN_VALUE) > state.updatedAt) return
        states[fingerprint] = state
    }

    override suspend fun clear(fingerprint: String) {
        states.remove(fingerprint)
    }
}
