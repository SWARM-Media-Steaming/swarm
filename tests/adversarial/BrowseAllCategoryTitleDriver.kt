import app.swarm.tv.app.data.artistsForBrowseAll
import app.swarm.tv.app.data.moviesForBrowseAll
import app.swarm.tv.app.data.showsForBrowseAll
import app.swarm.tv.core.catalog.MergedEntry
import app.swarm.tv.core.peer.CatalogEntry
import app.swarm.tv.core.peer.MediaKind

/**
 * Executable boundary checks for #353's catalog-refresh invariant.
 *
 * A Browse All page is opened from a particular row.  Its visible heading is
 * only correct if a later catalog update rebuilds that same row, rather than
 * inferring the row from display text.  In particular, user-controlled genre
 * names can equal the built-in Movies, Shows, or Music headings.
 */
private fun movie(key: String, vararg genres: String) = MergedEntry(
    fingerprint = key,
    sources = listOf("server-a"),
    entry = CatalogEntry(
        entryKey = key, fingerprint = key, kind = MediaKind.MOVIE,
        title = key, size = 1, genres = genres.toList(),
    ),
)

private fun episode(key: String, show: String, vararg genres: String) = MergedEntry(
    fingerprint = key,
    sources = listOf("server-a"),
    entry = CatalogEntry(
        entryKey = key, fingerprint = key, kind = MediaKind.EPISODE,
        title = key, size = 1, showTitle = show, season = 1, episode = 1,
        genres = genres.toList(),
    ),
)

private fun track(key: String, artist: String, vararg genres: String) = MergedEntry(
    fingerprint = key,
    sources = listOf("server-a"),
    entry = CatalogEntry(
        entryKey = key, fingerprint = key, kind = MediaKind.TRACK,
        title = key, size = 1, artist = artist, album = "Album", trackNumber = 1,
        genres = genres.toList(),
    ),
)

private val failures = mutableListOf<String>()

private fun check(name: String, got: List<String>, expected: List<String>) {
    if (got != expected) {
        failures += "$name: got=$got expected=$expected"
        println("FAIL ${failures.last()}")
    } else println("ok $name")
}

fun main() {
    // These are genre shelves clicked by the viewer, not top-level pages.
    // The next catalog delta must preserve their membership even though the
    // heading happens to have the same spelling as a top-level heading.
    check(
        "movie-genre-named-Movies-is-not-widened-on-refresh",
        moviesForBrowseAll(
            listOf(movie("in-genre", "Movies"), movie("outside-genre", "Drama")),
            "Movies",
        ).map { it.fingerprint },
        listOf("in-genre"),
    )
    check(
        "show-genre-named-Shows-is-not-widened-on-refresh",
        showsForBrowseAll(
            listOf(episode("in-genre", "Selected", "Shows"), episode("outside-genre", "Other", "Drama")),
            "Shows",
        ).map { it.show },
        listOf("Selected"),
    )
    check(
        "music-genre-named-Music-is-not-widened-on-refresh",
        artistsForBrowseAll(
            listOf(track("in-genre", "Selected", "Music"), track("outside-genre", "Other", "Drama")),
            "Music",
        ).map { it.artist },
        listOf("Selected"),
    )
    check(failures.isEmpty()) { failures.joinToString("; ") }
    println("ALL_OK")
}
