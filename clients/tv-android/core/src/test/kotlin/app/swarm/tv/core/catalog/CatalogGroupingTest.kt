package app.swarm.tv.core.catalog

import app.swarm.tv.core.peer.CatalogEntry
import app.swarm.tv.core.peer.MediaKind
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import kotlin.random.Random

class CatalogGroupingTest {

    private fun track(fp: String, artist: String?, album: String?, trackNumber: Int?, title: String = fp) = MergedEntry(
        fingerprint = fp,
        sources = listOf("server-a"),
        entry = CatalogEntry(entryKey = fp, fingerprint = fp, kind = MediaKind.TRACK, title = title, size = 1000, artist = artist, album = album, trackNumber = trackNumber),
    )

    private fun episode(fp: String, show: String?, season: Int?, episode: Int?, title: String = fp, scrapedTitle: String? = null) = MergedEntry(
        fingerprint = fp,
        sources = listOf("server-a"),
        entry = CatalogEntry(entryKey = fp, fingerprint = fp, kind = MediaKind.EPISODE, title = title, size = 1000, showTitle = show, season = season, episode = episode, scrapedTitle = scrapedTitle),
    )

    private fun movie(fp: String, parent: String? = null, extraType: String? = null, extraTitle: String? = null) = MergedEntry(
        fingerprint = fp,
        sources = listOf("server-a"),
        entry = CatalogEntry(
            entryKey = fp,
            fingerprint = fp,
            kind = MediaKind.MOVIE,
            title = fp,
            size = 1000,
            parentEntryKey = parent,
            extraType = extraType,
            extraTitle = extraTitle,
        ),
    )

    @Test
    fun `movie extras are excluded from shelves and attached to their parent details`() {
        val feature = movie("movie")
        val otherFeature = movie("other-movie")
        val featurette = movie("making-of", parent = "movie", extraType = "featurette", extraTitle = "Making Of")
        val deleted = movie("deleted", parent = "movie", extraType = "deletedScene", extraTitle = "Dorm Room Extended")
        val entries = listOf(feature, otherFeature, featurette, deleted)

        assertEquals(listOf("movie", "other-movie"), CatalogGrouping.movies(entries).map { it.fingerprint })
        assertEquals(listOf("deleted", "making-of"), CatalogGrouping.movieExtras(feature, entries).map { it.fingerprint })
        assertTrue(CatalogGrouping.movieExtras(otherFeature, entries).isEmpty())
    }

    @Test
    fun `tracks group by artist then album, sorted by track number`() {
        val entries = listOf(
            track("t3", "Air", "Moon Safari", 3),
            track("t1", "Air", "Moon Safari", 1),
            track("t2", "Air", "Moon Safari", 2),
            track("b1", "Air", "Talkie Walkie", 1),
        )
        val artists = CatalogGrouping.groupTracksByArtistAlbum(entries)
        assertEquals(1, artists.size)
        assertEquals("Air", artists[0].artist)
        assertEquals(listOf("Moon Safari", "Talkie Walkie"), artists[0].albums.map { it.album })
        assertEquals(listOf("t1", "t2", "t3"), artists[0].albums[0].tracks.map { it.fingerprint })
    }

    @Test
    fun `artist with multiple albums groups both under one artist`() {
        val entries = listOf(track("a1", "Boards of Canada", "Music Has the Right to Children", 1), track("a2", "Boards of Canada", "Geogaddi", 1))
        val artists = CatalogGrouping.groupTracksByArtistAlbum(entries)
        assertEquals(1, artists.size)
        assertEquals(2, artists[0].albums.size)
    }

    @Test
    fun `tracks missing artist or album bucket under Unknown without being dropped`() {
        val entries = listOf(track("x1", null, null, null), track("x2", "Air", null, 1))
        val artists = CatalogGrouping.groupTracksByArtistAlbum(entries)
        val total = artists.sumOf { it.albums.sumOf { a -> a.tracks.size } }
        assertEquals(2, total)
        assertTrue(artists.any { it.artist == UNKNOWN_ARTIST })
        assertTrue(artists.first { it.artist == "Air" }.albums.any { it.album == UNKNOWN_ALBUM })
    }

    @Test
    fun `episodes group by show then season, sorted by episode number, multi-season ordering correct`() {
        val entries = listOf(
            episode("s2e2", "Dexter", 2, 2),
            episode("s1e2", "Dexter", 1, 2),
            episode("s1e1", "Dexter", 1, 1),
            episode("s2e1", "Dexter", 2, 1),
        )
        val shows = CatalogGrouping.groupEpisodesByShowSeason(entries)
        assertEquals(1, shows.size)
        assertEquals(listOf(1, 2), shows[0].seasons.map { it.season })
        assertEquals(listOf("s1e1", "s1e2"), shows[0].seasons[0].episodes.map { it.fingerprint })
        assertEquals(listOf("s2e1", "s2e2"), shows[0].seasons[1].episodes.map { it.fingerprint })
    }

    @Test
    fun `episode missing show or season does not crash and lands in fallback bucket`() {
        val entries = listOf(episode("u1", null, null, null), episode("u2", "Dexter", null, 3))
        val shows = CatalogGrouping.groupEpisodesByShowSeason(entries)
        val total = shows.sumOf { it.seasons.sumOf { s -> s.episodes.size } }
        assertEquals(2, total)
        assertTrue(shows.any { it.show == UNKNOWN_SHOW })
        assertTrue(shows.first { it.show == "Dexter" }.seasons.any { it.season == null })
    }

    @Test
    fun `nextEpisode returns the following episode in the same season`() {
        val entries = listOf(episode("s1e1", "Dexter", 1, 1), episode("s1e2", "Dexter", 1, 2), episode("s1e3", "Dexter", 1, 3))
        val shows = CatalogGrouping.groupEpisodesByShowSeason(entries)
        val next = CatalogGrouping.nextEpisode(entries[0], shows)
        assertEquals("s1e2", next?.fingerprint)
    }

    @Test
    fun `nextEpisode crosses a season boundary to episode 1 of the next season`() {
        val entries = listOf(episode("s1e1", "Dexter", 1, 1), episode("s1e2", "Dexter", 1, 2), episode("s2e1", "Dexter", 2, 1))
        val shows = CatalogGrouping.groupEpisodesByShowSeason(entries)
        val last = entries.first { it.fingerprint == "s1e2" }
        val next = CatalogGrouping.nextEpisode(last, shows)
        assertEquals("s2e1", next?.fingerprint)
    }

    @Test
    fun `nextEpisode returns null at the true end of a show`() {
        val entries = listOf(episode("s1e1", "Dexter", 1, 1), episode("s1e2", "Dexter", 1, 2))
        val shows = CatalogGrouping.groupEpisodesByShowSeason(entries)
        val last = entries.first { it.fingerprint == "s1e2" }
        assertNull(CatalogGrouping.nextEpisode(last, shows))
    }

    @Test
    fun `nextEpisode returns null when the entry is not found in the show groups`() {
        val entries = listOf(episode("s1e1", "Dexter", 1, 1))
        val shows = CatalogGrouping.groupEpisodesByShowSeason(entries)
        val stranger = episode("gone", "Some Other Show", 1, 1)
        assertNull(CatalogGrouping.nextEpisode(stranger, shows))
    }

    @Test
    fun `two differently-named show folders merge when their episodes agree on a scraped title`() {
        // Real bug: "Law & Order SVU" and "Law & Order Special Victims
        // Unit" are the same series stored under two different on-disk
        // show folder names — the path-derived show title alone can't tell
        // they're the same, but a matching TMDb scrape can.
        val entries = listOf(
            episode("a1", "Law & Order SVU", 1, 1, scrapedTitle = "Law & Order: Special Victims Unit"),
            episode("a2", "Law & Order SVU", 1, 2, scrapedTitle = "Law & Order: Special Victims Unit"),
            episode("b1", "Law & Order Special Victims Unit", 2, 1, scrapedTitle = "Law & Order: Special Victims Unit"),
        )
        val shows = CatalogGrouping.groupEpisodesByShowSeason(entries)
        assertEquals(1, shows.size)
        assertEquals("Law & Order: Special Victims Unit", shows[0].show)
        assertEquals(listOf(1, 2), shows[0].seasons.map { it.season })
    }

    @Test
    fun `show folders with no scraped title consensus stay separate`() {
        val entries = listOf(episode("a1", "Show A", 1, 1), episode("b1", "Show B", 1, 1))
        val shows = CatalogGrouping.groupEpisodesByShowSeason(entries)
        assertEquals(2, shows.size)
    }

    @Test
    fun `show previews exclude specials unnumbered seasons and featurettes`() {
        val entries = listOf(
            episode("special", "Dexter", 0, 1),
            episode("loose", "Dexter", null, 1),
            episode("featurette", "Dexter", 2, null),
            episode("interview", "Dexter", 2, 0),
            episode("real", "Dexter", 2, 3),
        )
        val show = CatalogGrouping.groupEpisodesByShowSeason(entries).single()

        assertEquals(listOf(2), CatalogGrouping.previewSeasons(show).map { it.season })
        assertEquals("real", CatalogGrouping.randomPreviewEpisode(show, Random(7))?.fingerprint)
    }

    @Test
    fun `show preview randomizes both real seasons and their episodes`() {
        val entries = listOf(
            episode("s1e1", "Dexter", 1, 1),
            episode("s1e2", "Dexter", 1, 2),
            episode("s2e1", "Dexter", 2, 1),
            episode("s2e2", "Dexter", 2, 2),
        )
        val show = CatalogGrouping.groupEpisodesByShowSeason(entries).single()
        val selections = (0..100).mapNotNull { seed ->
            CatalogGrouping.randomPreviewEpisode(show, Random(seed))
        }

        assertEquals(setOf(1, 2), selections.mapNotNull { it.entry.season }.toSet())
        assertEquals(setOf(1, 2), selections.mapNotNull { it.entry.episode }.toSet())
    }

    @Test
    fun `nextEpisode still works across a season boundary for a merged show`() {
        val entries = listOf(
            episode("a1", "Law & Order SVU", 1, 1, scrapedTitle = "Law & Order: SVU"),
            episode("a2", "Law & Order SVU", 1, 2, scrapedTitle = "Law & Order: SVU"),
            episode("b1", "Law & Order Special Victims Unit", 2, 1, scrapedTitle = "Law & Order: SVU"),
        )
        val shows = CatalogGrouping.groupEpisodesByShowSeason(entries)
        val last = entries.first { it.fingerprint == "a2" }
        val next = CatalogGrouping.nextEpisode(last, shows)
        assertEquals("b1", next?.fingerprint)
    }

    private fun artists(vararg entries: MergedEntry) = CatalogGrouping.groupTracksByArtistAlbum(entries.toList())

    @Test
    fun `nextTrack OFF plays the next track on the album then rolls to the next album`() {
        val a1 = track("a1", "Air", "Moon Safari", 1)
        val a2 = track("a2", "Air", "Moon Safari", 2)
        val b1 = track("b1", "Air", "Talkie Walkie", 1)
        val grouped = artists(a1, a2, b1)
        assertEquals("a2", CatalogGrouping.nextTrack(a1, grouped, ShuffleMode.OFF)?.fingerprint)
        assertEquals("b1", CatalogGrouping.nextTrack(a2, grouped, ShuffleMode.OFF)?.fingerprint)
        assertNull(CatalogGrouping.nextTrack(b1, grouped, ShuffleMode.OFF))
    }

    @Test
    fun `nextTrack ALBUM stays within the current album`() {
        val a1 = track("a1", "Air", "Moon Safari", 1)
        val a2 = track("a2", "Air", "Moon Safari", 2)
        val a3 = track("a3", "Air", "Moon Safari", 3)
        val b1 = track("b1", "Air", "Talkie Walkie", 1)
        val grouped = artists(a1, a2, a3, b1)
        val picks = (0..50).map {
            CatalogGrouping.nextTrack(a1, grouped, ShuffleMode.ALBUM, random = Random(it))?.fingerprint
        }.toSet()
        assertEquals(setOf("a2", "a3"), picks)
    }

    @Test
    fun `nextTrack ALBUM with a single-track album returns the same track rather than stopping`() {
        val only = track("only", "Air", "Single", 1)
        val other = track("other", "Zero 7", "Other", 1)
        val grouped = artists(only, other)
        assertEquals("only", CatalogGrouping.nextTrack(only, grouped, ShuffleMode.ALBUM)?.fingerprint)
    }

    @Test
    fun `nextTrack ALL_SONGS can leave the current artist and album`() {
        val a1 = track("a1", "Air", "Moon Safari", 1)
        val a2 = track("a2", "Air", "Moon Safari", 2)
        val z1 = track("z1", "Zero 7", "Simple Things", 1)
        val z2 = track("z2", "Zero 7", "Simple Things", 2)
        val grouped = artists(a1, a2, z1, z2)
        val picks = (0..80).map {
            CatalogGrouping.nextTrack(a1, grouped, ShuffleMode.ALL_SONGS, random = Random(it))?.fingerprint
        }.toSet()
        assertEquals(setOf("a2", "z1", "z2"), picks)
        assertTrue(picks.any { it?.startsWith("z") == true }, "shuffle-all must be able to cross to another artist")
    }

    @Test
    fun `nextTrack ALL_SONGS never repeats the current track`() {
        val a1 = track("a1", "Air", "Moon Safari", 1)
        val z1 = track("z1", "Zero 7", "Simple Things", 1)
        val grouped = artists(a1, z1)
        repeat(50) { seed ->
            assertEquals("z1", CatalogGrouping.nextTrack(a1, grouped, ShuffleMode.ALL_SONGS, random = Random(seed))?.fingerprint)
        }
    }

    @Test
    fun `nextTrack returns null when the entry is no longer in the library`() {
        val grouped = artists(track("a1", "Air", "Moon Safari", 1))
        val stranger = track("gone", "Someone Else", "Nowhere", 1)
        assertNull(CatalogGrouping.nextTrack(stranger, grouped, ShuffleMode.OFF))
        assertNull(CatalogGrouping.nextTrack(stranger, grouped, ShuffleMode.ALL_SONGS))
    }

    @Test
    fun `ShuffleMode cycles off to album to all songs and back`() {
        assertEquals(ShuffleMode.ALBUM, ShuffleMode.OFF.next())
        assertEquals(ShuffleMode.ALL_SONGS, ShuffleMode.ALBUM.next())
        assertEquals(ShuffleMode.OFF, ShuffleMode.ALL_SONGS.next())
    }

    @Test
    fun `RepeatMode cycles off to repeat song to repeat album and back`() {
        assertEquals(RepeatMode.ONE, RepeatMode.OFF.next())
        assertEquals(RepeatMode.ALBUM, RepeatMode.ONE.next())
        assertEquals(RepeatMode.OFF, RepeatMode.ALBUM.next())
    }

    @Test
    fun `nextTrack repeat ALBUM wraps to the first track instead of rolling to the next album`() {
        val a1 = track("a1", "Air", "Moon Safari", 1)
        val a2 = track("a2", "Air", "Moon Safari", 2)
        val b1 = track("b1", "Air", "Talkie Walkie", 1)
        val grouped = artists(a1, a2, b1)
        assertEquals("a2", CatalogGrouping.nextTrack(a1, grouped, ShuffleMode.OFF, RepeatMode.ALBUM)?.fingerprint)
        assertEquals("a1", CatalogGrouping.nextTrack(a2, grouped, ShuffleMode.OFF, RepeatMode.ALBUM)?.fingerprint)
    }

    @Test
    fun `nextTrack repeat ALBUM keeps shuffle-all inside the current album`() {
        val a1 = track("a1", "Air", "Moon Safari", 1)
        val a2 = track("a2", "Air", "Moon Safari", 2)
        val z1 = track("z1", "Zero 7", "Simple Things", 1)
        val grouped = artists(a1, a2, z1)
        val picks = (0..50).map {
            CatalogGrouping.nextTrack(a1, grouped, ShuffleMode.ALL_SONGS, RepeatMode.ALBUM, Random(it))?.fingerprint
        }.toSet()
        assertEquals(setOf("a2"), picks)
    }

    @Test
    fun `nextTrack repeat ONE behaves like OFF so an explicit skip still advances`() {
        val a1 = track("a1", "Air", "Moon Safari", 1)
        val a2 = track("a2", "Air", "Moon Safari", 2)
        val grouped = artists(a1, a2)
        assertEquals("a2", CatalogGrouping.nextTrack(a1, grouped, ShuffleMode.OFF, RepeatMode.ONE)?.fingerprint)
        assertNull(CatalogGrouping.nextTrack(a2, grouped, ShuffleMode.OFF, RepeatMode.ONE))
    }

    @Test
    fun `previousTrack steps back through the album then into the previous album`() {
        val a1 = track("a1", "Air", "Moon Safari", 1)
        val a2 = track("a2", "Air", "Moon Safari", 2)
        val b1 = track("b1", "Air", "Talkie Walkie", 1)
        val b2 = track("b2", "Air", "Talkie Walkie", 2)
        val grouped = artists(a1, a2, b1, b2)
        assertEquals("a1", CatalogGrouping.previousTrack(a2, grouped)?.fingerprint)
        assertEquals("a2", CatalogGrouping.previousTrack(b1, grouped)?.fingerprint)
        assertNull(CatalogGrouping.previousTrack(a1, grouped))
    }

    @Test
    fun `previousTrack repeat ALBUM wraps to the last track of the same album`() {
        val a1 = track("a1", "Air", "Moon Safari", 1)
        val a2 = track("a2", "Air", "Moon Safari", 2)
        val grouped = artists(a1, a2)
        assertEquals("a2", CatalogGrouping.previousTrack(a1, grouped, RepeatMode.ALBUM)?.fingerprint)
    }

    @Test
    fun `previousTrack returns null when the entry is no longer in the library`() {
        val grouped = artists(track("a1", "Air", "Moon Safari", 1))
        assertNull(CatalogGrouping.previousTrack(track("gone", "Someone Else", "Nowhere", 1), grouped))
    }

    @Test
    fun `empty input produces empty groupings without crashing`() {
        assertTrue(CatalogGrouping.groupTracksByArtistAlbum(emptyList()).isEmpty())
        assertTrue(CatalogGrouping.groupEpisodesByShowSeason(emptyList()).isEmpty())
        assertNull(CatalogGrouping.nextEpisode(episode("x", "Y", 1, 1), emptyList()))
    }
}
