package app.swarm.tv.app.data

import app.swarm.tv.core.catalog.CatalogGrouping
import app.swarm.tv.core.catalog.MergedEntry
import app.swarm.tv.core.catalog.ShuffleMode
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
    fun `genre named Movies still filters to that genre`() {
        val entries = listOf(movie("a", "Movies"), movie("b", "Comedy"))
        assertEquals(listOf("a"), moviesForBrowseAll(entries, BROWSE_ALL_MOVIES_TITLE).map { it.fingerprint })
    }

    @Test
    fun `genre movies title keeps only that genre`() {
        val entries = listOf(movie("a", "Action"), movie("b", "Comedy"), movie("c", "Action", "Drama"))
        assertEquals(listOf("a", "c"), moviesForBrowseAll(entries, "Action").map { it.fingerprint })
    }

    @Test
    fun `genre named Shows still filters to that genre`() {
        val entries = listOf(episode("e1", "Wire", "Shows"), episode("e2", "Friends", "Comedy"))
        assertEquals(listOf("Wire"), showsForBrowseAll(entries, BROWSE_ALL_SHOWS_TITLE).map { it.show })
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
    fun `genre shows omit groups without preview seasons`() {
        val valid = episode("e1", "Wire", "Crime")
        val specialsOnly = episode("e2", "Behind the Scenes", "Crime").copy(
            entry = episode("e2", "Behind the Scenes", "Crime").entry.copy(season = 0, episode = null),
        )
        val unnumberedOnly = episode("e3", "Interview", "Crime").copy(
            entry = episode("e3", "Interview", "Crime").entry.copy(season = null, episode = null),
        )

        assertEquals(listOf("Wire"), showsForBrowseAll(listOf(valid, specialsOnly, unnumberedOnly), "Crime").map { it.show })
    }

    @Test
    fun `genre named Music still filters to that genre`() {
        val entries = listOf(track("t1", "A", "Music"), track("t2", "B", "Jazz"))
        assertEquals(listOf("A"), artistsForBrowseAll(entries, BROWSE_ALL_MUSIC_TITLE).map { it.artist })
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

    @Test
    fun `blank genre title never creates a browse all result`() {
        val entries = listOf(movie("blank", ""), movie("spaces", "   "), movie("action", "Action"))

        assertEquals(emptyList<String>(), moviesForBrowseAll(entries, "").map { it.fingerprint })
        assertEquals(emptyList<String>(), moviesForBrowseAll(entries, "   ").map { it.fingerprint })
    }

    @Test
    fun `blank genre scope leaves queue unscoped`() {
        val entries = listOf(movie("blank", ""), movie("action", "Action"))

        assertEquals(entries, entriesForGenreScope(entries, ""))
        assertEquals(entries, entriesForGenreScope(entries, "   "))
    }

    @Test
    fun `genre-scoped track queue cannot select another genre`() {
        val jazz1 = track("j1", "Artist", "Jazz")
        val jazz2Base = track("j2", "Artist", "Jazz")
        val jazz2 = jazz2Base.copy(entry = jazz2Base.entry.copy(trackNumber = 2))
        val rock = track("r1", "Artist", "Rock")

        val grouped = CatalogGrouping.groupTracksByArtistAlbum(entriesForGenreScope(listOf(jazz1, rock, jazz2), "Jazz"))
        assertEquals("j2", CatalogGrouping.nextTrack(jazz1, grouped, ShuffleMode.OFF)?.fingerprint)
    }

    @Test
    fun `genre-scoped episode queue cannot select another genre`() {
        val crime1 = episode("c1", "Case Files", "Crime")
        val crime2Base = episode("c2", "Case Files", "Crime")
        val crime2 = crime2Base.copy(entry = crime2Base.entry.copy(episode = 2))
        val drama = episode("d1", "Case Files", "Drama")

        val grouped = CatalogGrouping.groupEpisodesByShowSeason(entriesForGenreScope(listOf(crime1, drama, crime2), "Crime"))
        assertEquals("c2", CatalogGrouping.nextEpisode(crime1, grouped)?.fingerprint)
    }
}
