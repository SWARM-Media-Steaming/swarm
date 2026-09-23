package app.swarm.tv.app.ui.screens

import app.swarm.tv.core.catalog.MergedEntry
import app.swarm.tv.core.catalog.SeasonGroup
import app.swarm.tv.core.catalog.ShowGroup
import app.swarm.tv.core.peer.CatalogEntry
import app.swarm.tv.core.peer.MediaKind
import app.swarm.tv.core.watch.WatchState
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Test

class SeasonScreenTest {
    private fun episode(key: String, season: Int, number: Int) = MergedEntry(
        fingerprint = key,
        sources = listOf("server-a"),
        entry = CatalogEntry(
            entryKey = key,
            fingerprint = key,
            kind = MediaKind.EPISODE,
            title = "Episode $number",
            size = 1,
            showTitle = "The Wire",
            season = season,
            episode = number,
        ),
    )

    private val s1e1 = episode("s1e1", 1, 1)
    private val s1e2 = episode("s1e2", 1, 2)
    private val s2e1 = episode("s2e1", 2, 1)

    private val show = ShowGroup(
        show = "The Wire",
        seasons = listOf(
            SeasonGroup(1, listOf(s1e1, s1e2)),
            SeasonGroup(2, listOf(s2e1)),
        ),
    )

    private fun inProgress(updatedAt: Long) =
        WatchState(positionSecs = 300.0, durationSecs = 3000.0, watched = false, updatedAt = updatedAt)

    private fun finished(updatedAt: Long) =
        WatchState(positionSecs = 2900.0, durationSecs = 3000.0, watched = true, updatedAt = updatedAt)

    @Test
    fun `no resume target when nothing has been started`() {
        assertNull(resumeEpisode(show, emptyMap()))
    }

    @Test
    fun `no resume target when every started episode is finished`() {
        val states = mapOf("s1e1" to finished(10L), "s1e2" to finished(20L))
        assertNull(resumeEpisode(show, states))
    }

    @Test
    fun `no resume target for a zero-position saved state`() {
        val states = mapOf("s1e1" to inProgress(10L).copy(positionSecs = 0.0))
        assertNull(resumeEpisode(show, states))
    }

    @Test
    fun `resumes the most recently touched unfinished episode`() {
        val states = mapOf(
            "s1e1" to finished(50L),
            "s1e2" to inProgress(10L),
            "s2e1" to inProgress(40L),
        )
        assertEquals("s2e1", resumeEpisode(show, states)?.entry?.fingerprint)
    }

    @Test
    fun `an unfinished episode still resumes even if a later one was watched`() {
        // The show has aged out of Continue Watching but a mid-season
        // episode was left unfinished — Resume must still offer it (#152).
        val states = mapOf(
            "s1e2" to inProgress(5L),
            "s2e1" to finished(9L),
        )
        assertEquals("s1e2", resumeEpisode(show, states)?.entry?.fingerprint)
    }

    @Test
    fun `resume finds the same episode after its file fingerprint changes`() {
        val states = mapOf(
            "replaced-file" to inProgress(10L).copy(
                showTitle = "the wire",
                season = 1,
                episode = 2,
            ),
        )

        assertEquals("s1e2", resumeEpisode(show, states)?.entry?.fingerprint)
    }
}
