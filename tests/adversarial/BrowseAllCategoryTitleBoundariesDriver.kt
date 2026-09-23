import app.swarm.tv.app.data.artistsForBrowseAll
import app.swarm.tv.app.data.moviesForBrowseAll
import app.swarm.tv.app.data.showsForBrowseAll
import app.swarm.tv.core.catalog.MergedEntry
import app.swarm.tv.core.peer.CatalogEntry
import app.swarm.tv.core.peer.MediaKind
import kotlin.system.exitProcess

/**
 * Boundary membership for a Browse All page whose heading is a category
 * name (#353). The heading is only honest if the grid it labels contains
 * that category's assets — extras, other kinds, substring genre hits, and
 * empty catalogs must not quietly change what the title means.
 */
private fun movie(
    key: String,
    vararg genres: String,
    extraType: String? = null,
    parentEntryKey: String? = null,
) = MergedEntry(
    fingerprint = key,
    sources = listOf("server-a"),
    entry = CatalogEntry(
        entryKey = key, fingerprint = key, kind = MediaKind.MOVIE,
        title = key, size = 1, genres = genres.toList(),
        extraType = extraType, parentEntryKey = parentEntryKey,
    ),
)

private fun episode(
    key: String,
    show: String,
    vararg genres: String,
    season: Int? = 1,
    episode: Int? = 1,
) = MergedEntry(
    fingerprint = key,
    sources = listOf("server-a"),
    entry = CatalogEntry(
        entryKey = key, fingerprint = key, kind = MediaKind.EPISODE,
        title = key, size = 1, showTitle = show, season = season,
        episode = episode, genres = genres.toList(),
    ),
)

private fun track(key: String, artist: String, vararg genres: String) = MergedEntry(
    fingerprint = key,
    sources = listOf("server-a"),
    entry = CatalogEntry(
        entryKey = key, fingerprint = key, kind = MediaKind.TRACK,
        title = key, size = 1, artist = artist, album = "Album",
        trackNumber = 1, genres = genres.toList(),
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
    val mixed = listOf(
        movie("action-movie", "Action"),
        movie("drama-movie", "Drama"),
        movie("trailer", "Action", extraType = "trailer", parentEntryKey = "action-movie"),
        episode("action-ep", "Wire", "Action"),
        track("action-track", "Band", "Action"),
        movie("no-genre"),
        movie("empty-genre", ""),
        movie("action-adventure-label", "Action Adventure"),
        movie("sci-fi", "Sci-Fi"),
        movie("anime", "アニメ"),
        movie("rnb", "R&B"),
        movie("action-and-drama", "Action", "Drama"),
    )

    check(
        "genre-grid-excludes-other-kinds-and-extras",
        moviesForBrowseAll(mixed, "Action").map { it.fingerprint },
        listOf("action-movie", "action-and-drama"),
    )
    check(
        "substring-genre-is-not-the-selected-category",
        moviesForBrowseAll(mixed, "Action Adventure").map { it.fingerprint },
        listOf("action-adventure-label"),
    )
    check(
        "unicode-genre-title-selects-only-that-label",
        moviesForBrowseAll(mixed, "アニメ").map { it.fingerprint },
        listOf("anime"),
    )
    check(
        "punctuation-genre-title-is-opaque",
        moviesForBrowseAll(mixed, "R&B").map { it.fingerprint },
        listOf("rnb"),
    )
    check(
        "hyphenated-genre-title",
        moviesForBrowseAll(mixed, "Sci-Fi").map { it.fingerprint },
        listOf("sci-fi"),
    )
    check(
        "unknown-category-is-empty-not-the-whole-library",
        moviesForBrowseAll(mixed, "Western").map { it.fingerprint },
        emptyList(),
    )
    check(
        "empty-catalog-stays-empty",
        moviesForBrowseAll(emptyList(), "Action").map { it.fingerprint },
        emptyList(),
    )
    check(
        "blank-genre-string-is-not-Action",
        moviesForBrowseAll(listOf(movie("blank", ""), movie("action", "Action")), "Action")
            .map { it.fingerprint },
        listOf("action"),
    )
    check(
        "shows-genre-does-not-pull-movies-or-tracks",
        showsForBrowseAll(mixed, "Action").map { it.show },
        listOf("Wire"),
    )
    check(
        "music-genre-does-not-pull-movies-or-episodes",
        artistsForBrowseAll(mixed, "Action").map { it.artist },
        listOf("Band"),
    )
    check(
        "show-partial-genre-keeps-only-matching-episodes",
        showsForBrowseAll(
            listOf(
                episode("crime", "Wire", "Crime"),
                episode("comedy", "Wire", "Comedy"),
                episode("other-show", "Friends", "Comedy"),
            ),
            "Crime",
        ).flatMap { show -> show.seasons.flatMap { season -> season.episodes.map { it.fingerprint } } },
        listOf("crime"),
    )

    if (failures.isNotEmpty()) {
        System.err.println(failures.joinToString("; "))
        exitProcess(1)
    }
    println("ALL_OK")
}
