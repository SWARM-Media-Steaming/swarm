import app.swarm.tv.app.data.BROWSE_ALL_MUSIC_TITLE
import app.swarm.tv.app.data.BROWSE_ALL_SHOWS_TITLE
import app.swarm.tv.app.data.entriesForGenreScope
import app.swarm.tv.core.catalog.CatalogGrouping
import app.swarm.tv.core.catalog.MergedEntry
import app.swarm.tv.core.catalog.RepeatMode
import app.swarm.tv.core.catalog.ShuffleMode
import app.swarm.tv.core.peer.CatalogEntry
import app.swarm.tv.core.peer.MediaKind
import kotlin.random.Random
import kotlin.system.exitProcess

/**
 * Executable queue-membership UAT for #398.
 *
 * A genre Browse All screen is a membership boundary, including when its
 * nested artist/show contains entries from more than one genre.  Ordering a
 * whole-kind group before filtering would put the foreign entry between the
 * selected entries and make next, previous, or shuffle leave that boundary.
 */
private fun track(
    key: String,
    number: Int,
    vararg genres: String,
) = MergedEntry(
    fingerprint = key,
    sources = listOf("server-a"),
    entry = CatalogEntry(
        entryKey = key,
        fingerprint = key,
        kind = MediaKind.TRACK,
        title = key,
        size = 1,
        artist = "Boundary Artist",
        album = "Mixed Genre Album",
        trackNumber = number,
        genres = genres.toList(),
    ),
)

private fun episode(
    key: String,
    number: Int,
    vararg genres: String,
    season: Int = 1,
) = MergedEntry(
    fingerprint = key,
    sources = listOf("server-a"),
    entry = CatalogEntry(
        entryKey = key,
        fingerprint = key,
        kind = MediaKind.EPISODE,
        title = key,
        size = 1,
        showTitle = "Boundary Show",
        season = season,
        episode = number,
        genres = genres.toList(),
    ),
)

private val failures = mutableListOf<String>()

private fun <T> check(name: String, got: T, expected: T) {
    if (got != expected) {
        failures += "$name: got=$got expected=$expected"
        println("FAIL ${failures.last()}")
    } else {
        println("ok $name")
    }
}

private fun checkInScope(name: String, got: MergedEntry?, scope: String) {
    if (got == null || scope !in got.entry.genres) {
        failures += "$name: selected=${got?.fingerprint} genres=${got?.entry?.genres} outside=$scope"
        println("FAIL ${failures.last()}")
    } else {
        println("ok $name")
    }
}

fun main() {
    // The foreign track is ordered between the two Jazz tracks. This catches
    // both a whole-kind sequential queue and a previous queue that was left
    // unscoped while next was fixed.
    val jazzOne = track("jazz-1", 1, "Jazz")
    val rockMiddle = track("rock-2", 2, "Rock")
    val jazzThree = track("jazz-3", 3, "Jazz")
    val musicEntries = listOf(jazzOne, rockMiddle, jazzThree)
    val jazzArtists = CatalogGrouping.groupTracksByArtistAlbum(entriesForGenreScope(musicEntries, "Jazz"))

    check(
        "sequential-next-skips-foreign-track-in-same-album",
        CatalogGrouping.nextTrack(jazzOne, jazzArtists, ShuffleMode.OFF)?.fingerprint,
        "jazz-3",
    )
    check(
        "previous-skips-foreign-track-in-same-album",
        CatalogGrouping.previousTrack(jazzThree, jazzArtists)?.fingerprint,
        "jazz-1",
    )
    check(
        "repeat-album-wraps-within-genre-membership",
        CatalogGrouping.nextTrack(jazzThree, jazzArtists, ShuffleMode.OFF, RepeatMode.ALBUM)?.fingerprint,
        "jazz-1",
    )
    for (seed in 0..32) {
        checkInScope(
            "all-songs-shuffle-$seed-stays-in-genre",
            CatalogGrouping.nextTrack(jazzOne, jazzArtists, ShuffleMode.ALL_SONGS, random = Random(seed)),
            "Jazz",
        )
        checkInScope(
            "album-shuffle-$seed-stays-in-genre",
            CatalogGrouping.nextTrack(jazzOne, jazzArtists, ShuffleMode.ALBUM, random = Random(seed)),
            "Jazz",
        )
    }

    // The foreign episode sits between selected episodes in one season. The
    // second assertion represents the successor recomputed during episode
    // preload/autoplay, not merely the initial play selection.
    val crimeOne = episode("crime-1", 1, "Crime")
    val dramaMiddle = episode("drama-2", 2, "Drama")
    val crimeThree = episode("crime-3", 3, "Crime")
    val crimeSeasonTwo = episode("crime-s2e1", 1, "Crime", season = 2)
    val crimeShows = CatalogGrouping.groupEpisodesByShowSeason(
        entriesForGenreScope(listOf(crimeOne, dramaMiddle, crimeThree, crimeSeasonTwo), "Crime"),
    )
    check(
        "episode-next-skips-foreign-episode-in-same-season",
        CatalogGrouping.nextEpisode(crimeOne, crimeShows)?.fingerprint,
        "crime-3",
    )
    check(
        "episode-autoplay-crosses-to-next-in-scope-season",
        CatalogGrouping.nextEpisode(crimeThree, crimeShows)?.fingerprint,
        "crime-s2e1",
    )

    // Null means top-level/unscoped navigation. Headings are ordinary valid
    // genre values, not scope sentinels.
    check(
        "null-scope-keeps-whole-catalog",
        entriesForGenreScope(musicEntries, null).map { it.fingerprint },
        listOf("jazz-1", "rock-2", "jazz-3"),
    )
    check(
        "genre-named-Music-is-still-a-real-scope",
        entriesForGenreScope(listOf(track("music", 1, BROWSE_ALL_MUSIC_TITLE), jazzOne), BROWSE_ALL_MUSIC_TITLE)
            .map { it.fingerprint },
        listOf("music"),
    )
    check(
        "genre-named-Shows-is-still-a-real-scope",
        entriesForGenreScope(listOf(episode("shows", 1, BROWSE_ALL_SHOWS_TITLE), crimeOne), BROWSE_ALL_SHOWS_TITLE)
            .map { it.fingerprint },
        listOf("shows"),
    )
    check("unknown-genre-produces-an-empty-queue", entriesForGenreScope(musicEntries, "Unknown"), emptyList())

    if (failures.isNotEmpty()) exitProcess(1)
    println("ALL_OK")
}
