import app.swarm.tv.core.catalog.CatalogGrouping
import app.swarm.tv.core.catalog.MergedEntry
import app.swarm.tv.core.peer.CatalogEntry
import app.swarm.tv.core.peer.MediaKind

/**
 * Device-UI search must hide show cards that would render as "0 seasons".
 * Mirrors CatalogScreen: group then keep only groups with previewSeasons.
 */
fun searchableShowCards(entries: List<MergedEntry>): List<String> =
    CatalogGrouping.groupEpisodesByShowSeason(entries)
        .filter { CatalogGrouping.previewSeasons(it).isNotEmpty() }
        .map { it.show }

/** Same identifying fields CatalogScreen uses for the device search box. */
fun matchesSearch(entry: MergedEntry, query: String): Boolean {
    val q = query.trim().lowercase()
    if (q.isEmpty()) return true
    val e = entry.entry
    return listOfNotNull(e.scrapedTitle, e.title, e.artist, e.album, e.showTitle)
        .any { it.lowercase().contains(q) }
}

fun searchShowCards(entries: List<MergedEntry>, query: String): List<String> =
    searchableShowCards(entries.filter { matchesSearch(it, query) })

fun searchSeasonLabels(entries: List<MergedEntry>, query: String): List<String> =
    CatalogGrouping.groupEpisodesByShowSeason(entries.filter { matchesSearch(it, query) })
        .filter { CatalogGrouping.previewSeasons(it).isNotEmpty() }
        .map { show ->
            val n = CatalogGrouping.previewSeasons(show).size
            "$n season" + if (n == 1) "" else "s"
        }

fun episode(
    fp: String,
    show: String?,
    season: Int?,
    episode: Int?,
    title: String = fp,
    scrapedTitle: String? = null,
): MergedEntry = MergedEntry(
    fingerprint = fp,
    sources = listOf("server-a"),
    entry = CatalogEntry(
        entryKey = fp,
        fingerprint = fp,
        kind = MediaKind.EPISODE,
        title = title,
        size = 1000,
        showTitle = show,
        season = season,
        episode = episode,
        scrapedTitle = scrapedTitle,
    ),
)

fun main() {
    fun check(name: String, got: List<String>, expected: List<String>) {
        if (got != expected) {
            System.err.println("FAIL $name: got=$got expected=$expected")
            kotlin.system.exitProcess(1)
        }
        println("ok $name")
    }

    // Search hits only specials / extras / unnumbered episodes of a show.
    check(
        "specials-only-hidden",
        searchableShowCards(
            listOf(
                episode("s0e1", "Dexter", 0, 1, title = "Christmas Special"),
                episode("feat", "Dexter", 2, null, title = "Making Of"),
                episode("loose", "Dexter", null, 1, title = "Interview"),
                episode("ep0", "Dexter", 1, 0, title = "Pilot Recap"),
            ),
        ),
        emptyList(),
    )

    // A real numbered season still surfaces; extras do not create a second card.
    check(
        "mixed-real-season-kept",
        searchableShowCards(
            listOf(
                episode("s0e1", "Dexter", 0, 1, title = "Christmas Special"),
                episode("s1e1", "Dexter", 1, 1, title = "Dexter"),
                episode("feat", "Dexter", 2, null, title = "Making Of"),
            ),
        ),
        listOf("Dexter"),
    )

    // Query matching extras of one show and a real season of another.
    check(
        "only-shows-with-real-seasons",
        searchableShowCards(
            listOf(
                episode("ghost-special", "Ghost Show", 0, 1, title = "needle special"),
                episode("real", "Real Show", 1, 1, title = "needle episode"),
            ),
        ),
        listOf("Real Show"),
    )

    // Season 0 is never a preview season even with episode numbers.
    check(
        "season-zero-alone-hidden",
        searchableShowCards(listOf(episode("s0e1", "Lost", 0, 1))),
        emptyList(),
    )

    // Negative / zero episode numbers in a numbered season do not count.
    check(
        "unnumbered-episodes-in-s1-hidden",
        searchableShowCards(
            listOf(
                episode("e0", "Wire", 1, 0),
                episode("enull", "Wire", 1, null),
                episode("eneg", "Wire", 1, -1),
            ),
        ),
        emptyList(),
    )

    // Two real seasons stay visible; count is at least 1.
    val two = CatalogGrouping.groupEpisodesByShowSeason(
        listOf(
            episode("s1e1", "Wire", 1, 1),
            episode("s2e1", "Wire", 2, 1),
        ),
    ).single()
    val n = CatalogGrouping.previewSeasons(two).size
    if (n != 2) {
        System.err.println("FAIL two-seasons count=$n")
        kotlin.system.exitProcess(1)
    }
    println("ok two-seasons")

    // Empty catalog.
    check("empty", searchableShowCards(emptyList()), emptyList())

    val library = listOf(
        episode("s1e1", "Dexter", 1, 1, title = "Pilot"),
        episode("s0e1", "Dexter", 0, 1, title = "Christmas Special"),
        episode("feat", "Dexter", 2, null, title = "Making Of"),
        episode("ghost", "Ghost Show", 0, 1, title = "Holiday Special"),
        episode("ghost-feat", "Ghost Show", null, null, title = "Behind the Scenes"),
        episode("wire", "The Wire", 1, 1, title = "The Target"),
        episode("wire-s2", "The Wire", 2, 1, title = "Ebb Tide"),
        MergedEntry(
            fingerprint = "movie-making",
            sources = listOf("server-a"),
            entry = CatalogEntry(
                entryKey = "movie-making",
                fingerprint = "movie-making",
                kind = MediaKind.MOVIE,
                title = "Making Movies",
                size = 1000,
            ),
        ),
    )

    // Query hits only extras/specials of a show that also has real seasons:
    // the filtered group has no preview season, so the card must vanish.
    check("query-extra-title-hides", searchShowCards(library, "Making"), emptyList())
    check("query-special-title-hides-ghost", searchShowCards(library, "Holiday"), emptyList())

    // Query matching the show name of an extras-only series.
    check("query-show-name-extras-only", searchShowCards(library, "Ghost"), emptyList())

    // Query matching a real episode or a show that still has a numbered season.
    check("query-real-episode", searchShowCards(library, "Pilot"), listOf("Dexter"))
    check("query-show-name-with-seasons", searchShowCards(library, "dexter"), listOf("Dexter"))
    check("query-trim-case", searchShowCards(library, "  The Wire  "), listOf("The Wire"))

    // Visible cards never use the "0 seasons" label the bug reported.
    val labels = searchSeasonLabels(library, "wire")
    if (labels != listOf("2 seasons")) {
        System.err.println("FAIL wire-label: $labels")
        kotlin.system.exitProcess(1)
    }
    println("ok wire-label")
    val dexterLabels = searchSeasonLabels(library, "dexter")
    if (dexterLabels != listOf("1 season") || dexterLabels.any { it.startsWith("0 ") }) {
        System.err.println("FAIL dexter-label: $dexterLabels")
        kotlin.system.exitProcess(1)
    }
    println("ok dexter-label")

    // Movies that match the query are not show cards.
    check("movie-not-a-show", searchShowCards(library, "Making Movies"), emptyList())

    println("ALL_OK")
}
