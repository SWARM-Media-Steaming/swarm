import app.swarm.tv.app.data.BROWSE_ALL_MUSIC_TITLE
import app.swarm.tv.app.data.BROWSE_ALL_SHOWS_TITLE
import app.swarm.tv.app.data.artistsForBrowseAll
import app.swarm.tv.app.data.showsForBrowseAll
import app.swarm.tv.core.catalog.ArtistGroup
import app.swarm.tv.core.catalog.CatalogGrouping
import app.swarm.tv.core.catalog.MergedEntry
import app.swarm.tv.core.catalog.ShowGroup
import app.swarm.tv.core.peer.CatalogEntry
import app.swarm.tv.core.peer.MediaKind
import kotlin.system.exitProcess

/**
 * Open-time genre-scope derivation for #397: a genre-row artist/show card
 * tapped directly on Catalog (not via that genre's Browse All shelf) must
 * come up already scoped to that genre, and the top-level Music/Shows rows
 * (and the watchlist) must stay unscoped even though they also open
 * straight from Catalog.
 *
 * This mirrors, deliberately verbatim, the scope-selection expression in
 * SwarmViewModel.openArtistAlbums/openShowSeasons:
 *
 *   val scope = (previous as? ArtistShelf)?.takeIf { it.scopedToGenre }?.title
 *       ?: genreScope.takeIf { previous is Catalog }
 *
 * A stand-in [Previous] sealed type replaces UiState so this driver does
 * not need to pull in the whole SwarmViewModel/UiState dependency graph —
 * the same approach NestedGenreCatalogDeltaDriver.kt (#369) takes.
 */
private sealed class Previous {
    object Catalog : Previous()
    data class ArtistShelf(val title: String, val scopedToGenre: Boolean) : Previous()
    data class ShowShelf(val title: String, val scopedToGenre: Boolean) : Previous()
}

private fun resolveArtistScope(previous: Previous, genreScopeParam: String?): String? =
    (previous as? Previous.ArtistShelf)?.takeIf { it.scopedToGenre }?.title
        ?: genreScopeParam.takeIf { previous is Previous.Catalog }

private fun resolveShowScope(previous: Previous, genreScopeParam: String?): String? =
    (previous as? Previous.ShowShelf)?.takeIf { it.scopedToGenre }?.title
        ?: genreScopeParam.takeIf { previous is Previous.Catalog }

/** Mirrors the `artists =` line in openArtistAlbums: only a direct Catalog
 * open with a resolved scope narrows immediately; everything else (a Browse
 * All open, or no scope) keeps the whole-kind grouping already computed. */
private fun resolveArtistList(
    previous: Previous,
    entries: List<MergedEntry>,
    wholeKind: List<ArtistGroup>,
    scope: String?,
): List<ArtistGroup> =
    if (previous is Previous.Catalog && scope != null) artistsForBrowseAll(entries, scope) else wholeKind

private fun resolveShowList(
    previous: Previous,
    entries: List<MergedEntry>,
    wholeKind: List<ShowGroup>,
    scope: String?,
): List<ShowGroup> =
    if (previous is Previous.Catalog && scope != null) showsForBrowseAll(entries, scope) else wholeKind

private fun track(key: String, artist: String, album: String, vararg genres: String, trackNumber: Int = 1) =
    MergedEntry(
        fingerprint = key,
        sources = listOf("server-a"),
        entry = CatalogEntry(
            entryKey = key,
            fingerprint = key,
            kind = MediaKind.TRACK,
            title = key,
            size = 1,
            artist = artist,
            album = album,
            trackNumber = trackNumber,
            genres = genres.toList(),
        ),
    )

private fun episode(key: String, show: String, vararg genres: String, season: Int? = 1, episode: Int? = 1) =
    MergedEntry(
        fingerprint = key,
        sources = listOf("server-a"),
        entry = CatalogEntry(
            entryKey = key,
            fingerprint = key,
            kind = MediaKind.EPISODE,
            title = key,
            size = 1,
            showTitle = show,
            season = season,
            episode = episode,
            genres = genres.toList(),
        ),
    )

private fun albumsOf(artists: List<ArtistGroup>, artist: String): List<String> =
    artists.find { it.artist == artist }?.albums?.map { it.album } ?: emptyList()

private fun episodesOf(shows: List<ShowGroup>, show: String): List<String> =
    shows.find { it.show == show }?.seasons?.flatMap { s -> s.episodes.map { it.fingerprint } } ?: emptyList()

private val failures = mutableListOf<String>()

private fun <T> check(name: String, got: T, expected: T) {
    if (got != expected) {
        failures += "$name: got=$got expected=$expected"
        println("FAIL ${failures.last()}")
    } else {
        println("ok $name")
    }
}

fun main() {
    val jazzMiles = track("jazz-a", "Miles", "Kind of Blue", "Jazz")
    val rockMiles = track("rock-a", "Miles", "On the Corner", "Rock")
    val entries = listOf(jazzMiles, rockMiles)
    val wholeArtists = CatalogGrouping.groupTracksByArtistAlbum(entries)

    // Core #397 repro: tapping Miles straight off the Jazz genre row on
    // Catalog (no Browse All) must resolve scope="Jazz" and immediately
    // exclude the Rock album — not wait for a catalog delta to fix it.
    val jazzRowScope = resolveArtistScope(Previous.Catalog, "Jazz")
    check("direct-genre-row-open-resolves-that-genre", jazzRowScope, "Jazz")
    check(
        "direct-genre-row-open-excludes-other-genre-album-immediately",
        albumsOf(resolveArtistList(Previous.Catalog, entries, wholeArtists, jazzRowScope), "Miles"),
        listOf("Kind of Blue"),
    )

    // Top-level Music row and the watchlist open straight from Catalog too,
    // but pass no genre — must stay unscoped, not silently inherit a genre.
    val topLevelScope = resolveArtistScope(Previous.Catalog, null)
    check("top-level-row-open-stays-unscoped", topLevelScope, null)
    check(
        "top-level-row-open-keeps-every-album",
        albumsOf(resolveArtistList(Previous.Catalog, entries, wholeArtists, topLevelScope), "Miles"),
        listOf("Kind of Blue", "On the Corner"),
    )

    // A user genre literally named "Music" must still scope, not be treated
    // as the unscoped top-level row (heading text is not the discriminator).
    val namedMusicGenre = track("music-genre", "Miles", "Music Genre Album", BROWSE_ALL_MUSIC_TITLE)
    val namedMusicEntries = entries + namedMusicGenre
    val namedMusicScope = resolveArtistScope(Previous.Catalog, BROWSE_ALL_MUSIC_TITLE)
    check("genre-named-Music-still-resolves-as-a-genre", namedMusicScope, BROWSE_ALL_MUSIC_TITLE)
    check(
        "genre-named-Music-does-not-widen-to-whole-kind",
        albumsOf(
            resolveArtistList(Previous.Catalog, namedMusicEntries, CatalogGrouping.groupTracksByArtistAlbum(namedMusicEntries), namedMusicScope),
            "Miles",
        ),
        listOf("Music Genre Album"),
    )

    // Browse All (#369) precedence: opening from within a genre-scoped
    // ArtistShelf must keep using the shelf's own title, never a stray
    // caller-supplied genreScope param (defends the ?: ordering).
    val fromGenreShelf = Previous.ArtistShelf(title = "Jazz", scopedToGenre = true)
    check(
        "genre-shelf-scope-wins-over-a-mismatched-param",
        resolveArtistScope(fromGenreShelf, "SomethingElse"),
        "Jazz",
    )
    check(
        "genre-shelf-scope-used-even-when-param-is-null",
        resolveArtistScope(fromGenreShelf, null),
        "Jazz",
    )

    // Unscoped Browse All (top-level Music grid) must stay unscoped even if
    // a genre string somehow arrived in the param — the param only applies
    // to a *direct* Catalog open.
    val fromUnscopedShelf = Previous.ArtistShelf(title = "Music", scopedToGenre = false)
    check(
        "unscoped-shelf-ignores-a-leaked-param",
        resolveArtistScope(fromUnscopedShelf, "Jazz"),
        null,
    )

    // Show side: same core repro and same precedence rules.
    val crimeWire = episode("wire-s1e1", "The Wire", "Crime")
    val dramaWire = episode("wire-drama", "The Wire", "Drama", season = 2)
    val showEntries = listOf(crimeWire, dramaWire)
    val wholeShows = CatalogGrouping.browsableShows(showEntries)

    val crimeRowScope = resolveShowScope(Previous.Catalog, "Crime")
    check("direct-genre-row-show-open-resolves-that-genre", crimeRowScope, "Crime")
    check(
        "direct-genre-row-show-open-excludes-other-genre-episode-immediately",
        episodesOf(resolveShowList(Previous.Catalog, showEntries, wholeShows, crimeRowScope), "The Wire"),
        listOf("wire-s1e1"),
    )

    val topLevelShowScope = resolveShowScope(Previous.Catalog, null)
    check("top-level-show-row-open-stays-unscoped", topLevelShowScope, null)
    check(
        "top-level-show-row-open-keeps-every-episode",
        episodesOf(resolveShowList(Previous.Catalog, showEntries, wholeShows, topLevelShowScope), "The Wire"),
        listOf("wire-s1e1", "wire-drama"),
    )

    val namedShowsGenre = episode("shows-genre-ep", "Selected", BROWSE_ALL_SHOWS_TITLE)
    val namedShowsScope = resolveShowScope(Previous.Catalog, BROWSE_ALL_SHOWS_TITLE)
    check("genre-named-Shows-still-resolves-as-a-genre", namedShowsScope, BROWSE_ALL_SHOWS_TITLE)
    check(
        "genre-named-Shows-does-not-widen-to-whole-kind",
        resolveShowList(Previous.Catalog, listOf(namedShowsGenre) + showEntries, CatalogGrouping.browsableShows(listOf(namedShowsGenre) + showEntries), namedShowsScope)
            .map { it.show },
        listOf("Selected"),
    )

    val fromGenreShowShelf = Previous.ShowShelf(title = "Crime", scopedToGenre = true)
    check(
        "show-genre-shelf-scope-wins-over-a-mismatched-param",
        resolveShowScope(fromGenreShowShelf, "SomethingElse"),
        "Crime",
    )
    val fromUnscopedShowShelf = Previous.ShowShelf(title = "Shows", scopedToGenre = false)
    check(
        "unscoped-show-shelf-ignores-a-leaked-param",
        resolveShowScope(fromUnscopedShowShelf, "Crime"),
        null,
    )

    if (failures.isNotEmpty()) {
        System.err.println(failures.joinToString("; "))
        exitProcess(1)
    }
    println("ALL_OK")
}
