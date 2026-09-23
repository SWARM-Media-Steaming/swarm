package app.swarm.tv.core.watch

import app.swarm.tv.core.peer.CatalogEntry
import app.swarm.tv.core.peer.MediaKind
import app.swarm.tv.core.rest.SwarmJson
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.decodeFromString
import kotlinx.serialization.encodeToString
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

class WatchStateTest {
    private fun episode(fingerprint: String, season: Int = 2, number: Int = 4) = CatalogEntry(
        entryKey = "entry-$fingerprint",
        fingerprint = fingerprint,
        kind = MediaKind.EPISODE,
        title = "Episode $number",
        size = 1,
        showTitle = "The Expanse",
        season = season,
        episode = number,
    )

    @Test
    fun `ninety five percent is watched but just under is not`() {
        assertTrue(WatchState.fromPlayback(95.0, 100.0, 1).watched)
        assertFalse(WatchState.fromPlayback(94.9, 100.0, 1).watched)
    }

    @Test
    fun `wire shape uses snake_case fields`() {
        val state = WatchState(positionSecs = 125.5, durationSecs = 5400.0, watched = false, updatedAt = 1_700_000_000_000)
        val json = SwarmJson.encodeToString(state)
        assertEquals(
            """{"position_secs":125.5,"duration_secs":5400.0,"watched":false,"updated_at":1700000000000}""",
            json,
        )
        assertEquals(state, SwarmJson.decodeFromString<WatchState>(json))
    }

    @Test
    fun `episode wire shape captures show season and episode while legacy state still decodes`() {
        val state = WatchState.fromPlayback(
            positionSecs = 125.5,
            durationSecs = 5400.0,
            updatedAt = 1_700_000_000_000,
            showTitle = "The Expanse",
            season = 2,
            episode = 4,
        )
        val json = SwarmJson.encodeToString(state)
        assertEquals(
            """{"position_secs":125.5,"duration_secs":5400.0,"watched":false,"updated_at":1700000000000,"show_title":"The Expanse","season":2,"episode":4}""",
            json,
        )

        val legacy = SwarmJson.decodeFromString<WatchState>(
            """{"position_secs":42.0,"duration_secs":100.0,"watched":false,"updated_at":1}""",
        )
        assertNull(legacy.showTitle)
        assertNull(legacy.season)
        assertNull(legacy.episode)
    }

    @Test
    fun `episode state survives a changed content fingerprint by logical identity`() {
        val saved = WatchState.fromPlayback(
            900.0,
            3600.0,
            10,
            showTitle = "  THE EXPANSE ",
            season = 2,
            episode = 4,
        )
        val states = mapOf("old-file-fingerprint" to saved)

        assertEquals(saved, states.stateFor(episode("replacement-file-fingerprint")))
        assertNull(states.stateFor(episode("different-episode", number = 5)))
    }

    @Test
    fun `exact fingerprint state wins before episode identity fallback`() {
        val direct = WatchState.fromPlayback(300.0, 3600.0, 10)
        val fallback = WatchState.fromPlayback(
            900.0,
            3600.0,
            20,
            showTitle = "The Expanse",
            season = 2,
            episode = 4,
        )
        val states = mapOf("current" to direct, "old" to fallback)

        assertEquals(direct, states.stateFor(episode("current")))
    }

    @Test
    fun `unnumbered episode states never collide through the logical fallback`() {
        val saved = WatchState.fromPlayback(
            900.0,
            3600.0,
            20,
            showTitle = "The Expanse",
        )

        assertNull(mapOf("old" to saved).stateFor(episode("current").copy(season = null, episode = null)))
    }

    @Test
    fun `in-memory store roundtrips per fingerprint`() = runBlocking {
        val store = InMemoryWatchStateStore()
        assertNull(store.get("fp-1"))

        val state = WatchState(positionSecs = 42.0, durationSecs = 100.0, watched = false, updatedAt = 1)
        store.set("fp-1", state)
        assertEquals(state, store.get("fp-1"))
        assertEquals(mapOf("fp-1" to state), store.all())
        assertNull(store.get("fp-2")) // a different fingerprint is unaffected

        store.clear("fp-1")
        assertNull(store.get("fp-1"))
    }

    @Test
    fun `setting again overwrites rather than accumulates`() = runBlocking {
        val store = InMemoryWatchStateStore()
        store.set("fp-1", WatchState(10.0, 100.0, false, 1))
        store.set("fp-1", WatchState(90.0, 100.0, true, 2))
        assertEquals(WatchState(90.0, 100.0, true, 2), store.get("fp-1"))
    }

    @Test
    fun `an older asynchronous save cannot overwrite newer progress`() = runBlocking {
        val store = InMemoryWatchStateStore()
        val newer = WatchState(90.0, 100.0, false, 2)
        store.set("fp-1", newer)
        store.set("fp-1", WatchState(10.0, 100.0, false, 1))

        assertEquals(newer, store.get("fp-1"))
    }
}
