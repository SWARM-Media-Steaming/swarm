import app.swarm.tv.app.data.artistsForBrowseAll
import app.swarm.tv.app.data.entriesForGenreScope
import app.swarm.tv.app.data.moviesForBrowseAll
import app.swarm.tv.app.data.showsForBrowseAll
import app.swarm.tv.core.catalog.CatalogGrouping
import app.swarm.tv.core.catalog.MergedEntry
import app.swarm.tv.core.peer.CatalogEntry
import app.swarm.tv.core.peer.MediaKind
import kotlin.system.exitProcess

/**
 * Adversarial UAT for #421: a scraped library that carries literal blank or
 * whitespace-only genre tags on enough entries to pass MIN_GENRE_SHELF_SIZE
 * must never surface a "" shelf, never let a card opened from one scope
 * playback/browse to it, and must reject non-ASCII whitespace-only genres
 * (e.g. a lone U+00A0 non-breaking space) the same way as plain "" — a
 * scraper is just as likely to emit either.
 *
 * topGenreShelves() itself lives in CatalogScreen.kt, a Compose file this
 * driver cannot cheaply compile (it pulls in the whole Compose runtime), so
 * the ranking/threshold expression is mirrored verbatim here, exactly as
 * NestedGenreCatalogDeltaDriver.kt (#369) and GenreCardOpenScopeDriver.kt
 * (#397) already do for the same reason. Everything else below calls the
 * real, compiled BrowseAllShelf.kt production functions directly.
 */
private const val MIN_GENRE_SHELF_SIZE = 6
private const val MAX_GENRE_SHELVES = 3

private fun <T> topGenreShelves(entries: List<MergedEntry>, group: (List<MergedEntry>) -> List<T>): List<Pair<String, List<T>>> =
    entries.flatMap { it.entry.genres }
        .groupingBy { it }
        .eachCount()
        .entries
        .sortedByDescending { it.value }
        .filter { (genre, _) -> genre.isNotBlank() }
        .map { (genre, _) -> genre to group(entries.filter { it.entry.genres.contains(genre) }) }
        .filter { (_, grouped) -> grouped.size >= MIN_GENRE_SHELF_SIZE }
        .take(MAX_GENRE_SHELVES)

/** Mirrors the fixed scope-selection expression in
 * SwarmViewModel.openArtistAlbums/openShowSeasons: a blank genreScope
 * argument must resolve to null, exactly like a missing one. */
private fun resolveDirectOpenScope(fromCatalog: Boolean, genreScopeParam: String?): String? =
    genreScopeParam.takeIf { fromCatalog && it?.isNotBlank() == true }

private fun movie(key: String, vararg genres: String) =
    MergedEntry(
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
    // A scraper that emits "" on most entries and a real genre on only a
    // few must not surface a "" shelf even though "" alone clears the
    // MIN_GENRE_SHELF_SIZE threshold by a wide margin.
    val blankHeavyEntries = (1..8).map { movie("blank-$it", "") } +
        (1..8).map { movie("nbsp-$it", "  ") } +
        (1..6).map { movie("action-$it", "Action") }

    val shelves = topGenreShelves(blankHeavyEntries) { it }
    check("blank-genre-string-never-becomes-a-shelf", shelves.any { it.first == "" }, false)
    check("nbsp-only-genre-never-becomes-a-shelf", shelves.any { it.first.isBlank() }, false)
    check("real-genre-still-surfaces-despite-blank-noise", shelves.any { it.first == "Action" }, true)

    // Even a single whitespace-only genre tag mixed into an otherwise
    // healthy small library must not leak into ranking output at all.
    val singleWhitespaceEntry = listOf(movie("space-only", "   "))
    check(
        "lone-whitespace-only-genre-produces-no-shelf",
        topGenreShelves(singleWhitespaceEntry) { it },
        emptyList(),
    )

    // entriesForGenreScope must treat a blank/whitespace scope as "no
    // scope" so a playback queue never narrows to an empty membership set.
    val mixedEntries = listOf(movie("blank", ""), movie("action", "Action"))
    check("blank-scope-leaves-playback-queue-unfiltered", entriesForGenreScope(mixedEntries, ""), mixedEntries)
    check("whitespace-scope-leaves-playback-queue-unfiltered", entriesForGenreScope(mixedEntries, "   "), mixedEntries)
    check("nbsp-scope-leaves-playback-queue-unfiltered", entriesForGenreScope(mixedEntries, " "), mixedEntries)

    // Browse All rebuilds must yield an empty grid for a blank title rather
    // than a grid keyed to "everything with no genre," which a stray ""
    // title (e.g. a rebuild racing a catalog delta) could otherwise leak.
    check("browse-all-movies-blank-title-is-empty", moviesForBrowseAll(mixedEntries, "").map { it.fingerprint }, emptyList())
    check("browse-all-movies-whitespace-title-is-empty", moviesForBrowseAll(mixedEntries, "   ").map { it.fingerprint }, emptyList())

    val showEntries = listOf(
        MergedEntry(
            fingerprint = "ep1",
            sources = listOf("server-a"),
            entry = CatalogEntry(
                entryKey = "ep1",
                fingerprint = "ep1",
                kind = MediaKind.EPISODE,
                title = "ep1",
                size = 1,
                showTitle = "Show",
                season = 1,
                episode = 1,
                genres = listOf(""),
            ),
        ),
    )
    check("browse-all-shows-blank-title-is-empty", showsForBrowseAll(showEntries, "").map { it.show }, emptyList())

    // A card resolving its open-time genreScope from a blank param (the
    // #421 repro's second hop: a blank-genre shelf existed and got tapped)
    // must land unscoped, not silently produce an empty nested screen.
    check("direct-open-with-blank-param-resolves-unscoped", resolveDirectOpenScope(true, ""), null)
    check("direct-open-with-whitespace-param-resolves-unscoped", resolveDirectOpenScope(true, "   "), null)
    check("direct-open-with-real-genre-still-scopes", resolveDirectOpenScope(true, "Action"), "Action")

    if (failures.isNotEmpty()) {
        System.err.println(failures.joinToString("; "))
        exitProcess(1)
    }
    println("ALL_OK")
}
