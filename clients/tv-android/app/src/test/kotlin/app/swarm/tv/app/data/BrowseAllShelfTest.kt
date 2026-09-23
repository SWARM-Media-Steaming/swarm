package app.swarm.tv.app.data

import app.swarm.tv.core.catalog.MergedEntry
import app.swarm.tv.core.peer.CatalogEntry
import app.swarm.tv.core.peer.MediaKind
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Test

/**
 * Browse All grids show the originating shelf title (#353). A live catalog
 * delta must rebuild the same subset the title names, not the whole kind.
 */
class BrowseAllShelfTest {
    private fun movie(key: String, vararg genres: String) = MergedEntry(
        fingerprint = key,
        sources = listOf("server-a"),
        entry = CatalogEntry(
            entryKey = key,
            fingerprint = key,
            kind = MediaKind.MOVIE,
            title = key,
            size = 1,
            genres = genres.toList(),
        ),
    )

    private fun episode(key: String, show: String, vararg genres: String) = MergedEntry(
        fingerprint = key,
        sources = listOf("server-a"),
        entry = CatalogEntry(
            entryKey = key,
            fingerprint = key,
            kind = MediaKind.EPISODE,
            title = key,
            size = 1,
            showTitle = show,
            season = 1,
            episode = 1,
            genres = genres.toList(),
        ),
    )

    private fun track(key: String, artist: String, vararg genres: String) = MergedEntry(
        fingerprint = key,
        sources = listOf("server-a"),
        entry = CatalogEntry(
            entryKey = key,
            fingerprint = key,
            kind = MediaKind.TRACK,
            title = key,
            size = 1,
            artist = artist,
            album = "Album",
            trackNumber = 1,
            genres = genres.toList(),
        ),
    )

    @Test
    fun `top-level movies title keeps every movie`() {
        val entries = listOf(movie("a", "Action"), movie("b", "Comedy"))
        assertEquals(listOf("a", "b"), moviesForBrowseAll(entries, BROWSE_ALL_MOVIES_TITLE).map { it.fingerprint })
    }

    @Test
    fun `genre movies title keeps only that genre`() {
        val entries = listOf(movie("a", "Action"), movie("b", "Comedy"), movie("c", "Action", "Drama"))
        assertEquals(listOf("a", "c"), moviesForBrowseAll(entries, "Action").map { it.fingerprint })
    }

    @Test
    fun `top-level shows title keeps every show`() {
        val entries = listOf(episode("e1", "Wire", "Crime"), episode("e2", "Friends", "Comedy"))
        assertEquals(listOf("Friends", "Wire"), showsForBrowseAll(entries, BROWSE_ALL_SHOWS_TITLE).map { it.show })
    }

    @Test
    fun `genre shows title groups only matching episodes`() {
        val entries = listOf(
            episode("e1", "Wire", "Crime"),
            episode("e2", "Friends", "Comedy"),
            episode("e3", "Wire", "Drama"),
        )
        val shows = showsForBrowseAll(entries, "Crime")
        assertEquals(listOf("Wire"), shows.map { it.show })
        assertEquals(listOf("e1"), shows.single().seasons.single().episodes.map { it.fingerprint })
    }

    @Test
    fun `top-level music title keeps every artist`() {
        val entries = listOf(track("t1", "A", "Rock"), track("t2", "B", "Jazz"))
        assertEquals(listOf("A", "B"), artistsForBrowseAll(entries, BROWSE_ALL_MUSIC_TITLE).map { it.artist })
    }

    @Test
    fun `genre music title groups only matching tracks`() {
        val entries = listOf(
            track("t1", "A", "Rock"),
            track("t2", "B", "Jazz"),
            track("t3", "A", "Pop"),
        )
        val artists = artistsForBrowseAll(entries, "Rock")
        assertEquals(listOf("A"), artists.map { it.artist })
        assertEquals(listOf("t1"), artists.single().albums.single().tracks.map { it.fingerprint })
    }
}
