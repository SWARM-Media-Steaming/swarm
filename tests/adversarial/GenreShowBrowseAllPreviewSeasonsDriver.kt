import app.swarm.tv.app.data.BROWSE_ALL_SHOWS_TITLE
import app.swarm.tv.app.data.showsForBrowseAll
import app.swarm.tv.core.catalog.CatalogGrouping
import app.swarm.tv.core.catalog.MergedEntry
import app.swarm.tv.core.catalog.ShowGroup
import app.swarm.tv.core.peer.CatalogEntry
import app.swarm.tv.core.peer.MediaKind
import kotlin.system.exitProcess

/**
 * Executable membership for #368: a catalog delta on a genre Shows Browse All
 * page must rebuild the same groups the originating genre shelf would show.
 *
 * CatalogScreen's genre shelves group matching episodes, then drop groups
 * whose [CatalogGrouping.previewSeasons] is empty. Season 0, null/negative
 * seasons, and seasons whose episodes are unnumbered are extras, not
 * browseable shows. Membership is decided after the genre filter, so a
 * numbered season in a different genre cannot keep an extras-only group
 * alive on this page.
 */
private fun episode(
    key: String,
    show: String,
    vararg genres: String,
    season: Int? = 1,
    episode: Int? = 1,
    scrapedTitle: String? = null,
    kind: MediaKind = MediaKind.EPISODE,
) = MergedEntry(
    fingerprint = key,
    sources = listOf("server-a"),
    entry = CatalogEntry(
        entryKey = key,
        fingerprint = key,
        kind = kind,
        title = key,
        size = 1,
        showTitle = show,
        season = season,
        episode = episode,
        scrapedTitle = scrapedTitle,
        genres = genres.toList(),
    ),
)

/** Same grouping the Crime (etc.) show genre shelf uses before Browse All. */
private fun genreShelfShows(entries: List<MergedEntry>, genre: String): List<ShowGroup> =
    CatalogGrouping.groupEpisodesByShowSeason(entries.filter { it.entry.genres.contains(genre) })
        .filter { CatalogGrouping.previewSeasons(it).isNotEmpty() }

private val failures = mutableListOf<String>()

private fun check(name: String, got: List<String>, expected: List<String>) {
    if (got != expected) {
        failures += "$name: got=$got expected=$expected"
        println("FAIL ${failures.last()}")
    } else {
        println("ok $name")
    }
}

private fun names(groups: List<ShowGroup>): List<String> = groups.map { it.show }

fun main() {
    val crimePilot = episode("wire-s1e1", "The Wire", "Crime")
    val crimeSpecial = episode("bts", "Behind the Scenes", "Crime", season = 0, episode = 1)
    val crimeUnnumbered = episode("interview", "Interview", "Crime", season = null, episode = null)
    val crimeNegSeason = episode("neg", "Negative Season", "Crime", season = -1, episode = 1)
    val crimeS1E0 = episode("recap", "Recap Dump", "Crime", season = 1, episode = 0)
    val crimeS1NullEp = episode("featurette", "Featurette Dump", "Crime", season = 1, episode = null)
    val crimeS1NegEp = episode("neg-ep", "Neg Episode", "Crime", season = 1, episode = -3)
    val comedyPilot = episode("friends-s1e1", "Friends", "Comedy")
    val movie = episode("movie", "Not A Show", "Crime", kind = MediaKind.MOVIE)
    val track = episode("track", "Not A Show", "Crime", kind = MediaKind.TRACK)

    // Domain: extras-only groups never become genre Browse All cards.
    check(
        "season-zero-with-numbered-episode-is-not-a-show",
        names(showsForBrowseAll(listOf(crimePilot, crimeSpecial), "Crime")),
        listOf("The Wire"),
    )
    check(
        "null-season-unnumbered-episode-is-not-a-show",
        names(showsForBrowseAll(listOf(crimePilot, crimeUnnumbered), "Crime")),
        listOf("The Wire"),
    )
    check(
        "negative-season-is-not-a-show",
        names(showsForBrowseAll(listOf(crimePilot, crimeNegSeason), "Crime")),
        listOf("The Wire"),
    )
    check(
        "season-one-episode-zero-is-not-a-show",
        names(showsForBrowseAll(listOf(crimePilot, crimeS1E0), "Crime")),
        listOf("The Wire"),
    )
    check(
        "season-one-null-episode-is-not-a-show",
        names(showsForBrowseAll(listOf(crimePilot, crimeS1NullEp), "Crime")),
        listOf("The Wire"),
    )
    check(
        "season-one-negative-episode-is-not-a-show",
        names(showsForBrowseAll(listOf(crimePilot, crimeS1NegEp), "Crime")),
        listOf("The Wire"),
    )
    check(
        "genre-of-only-extras-is-empty",
        names(showsForBrowseAll(listOf(crimeSpecial, crimeUnnumbered, crimeNegSeason), "Crime")),
        emptyList(),
    )
    check(
        "empty-catalog",
        names(showsForBrowseAll(emptyList(), "Crime")),
        emptyList(),
    )
    check(
        "unknown-genre",
        names(showsForBrowseAll(listOf(crimePilot, crimeSpecial), "Western")),
        emptyList(),
    )

    // Mixed groups stay: extras live under the show, they just cannot be the
    // only reason the card exists.
    val mixed = listOf(
        crimePilot,
        episode("wire-s0e1", "The Wire", "Crime", season = 0, episode = 1),
        episode("wire-feat", "The Wire", "Crime", season = 2, episode = null),
    )
    val mixedGroups = showsForBrowseAll(mixed, "Crime")
    check("mixed-show-kept", names(mixedGroups), listOf("The Wire"))
    val mixedSeasons = mixedGroups.single().seasons.map { it.season }
    if (0 !in mixedSeasons) {
        failures += "mixed-show-keeps-extras-seasons: seasons=$mixedSeasons"
        println("FAIL ${failures.last()}")
    } else {
        println("ok mixed-show-keeps-extras-seasons")
    }
    if (CatalogGrouping.previewSeasons(mixedGroups.single()).isEmpty()) {
        failures += "mixed-show-has-preview-season"
        println("FAIL ${failures.last()}")
    } else {
        println("ok mixed-show-has-preview-season")
    }

    // Genre filter happens before grouping: a Drama numbered season does not
    // rescue Crime extras of the same show on the Crime page.
    val splitGenre = listOf(
        episode("wire-crime-extra", "The Wire", "Crime", season = 0, episode = 1),
        episode("wire-drama-pilot", "The Wire", "Drama", season = 1, episode = 1),
        crimeSpecial,
    )
    check(
        "other-genre-numbered-season-does-not-keep-extras-on-this-page",
        names(showsForBrowseAll(splitGenre, "Crime")),
        emptyList(),
    )
    check(
        "drama-page-keeps-the-numbered-season",
        names(showsForBrowseAll(splitGenre, "Drama")),
        listOf("The Wire"),
    )

    // Refresh sequence: extras-only title appears in a later catalog delta
    // while the viewer is still on Crime Browse All.
    val beforeDelta = listOf(crimePilot, comedyPilot)
    check("before-delta", names(showsForBrowseAll(beforeDelta, "Crime")), listOf("The Wire"))
    val afterExtrasArrive = beforeDelta + crimeSpecial + crimeUnnumbered
    check(
        "delta-adds-extras-only-groups-they-stay-hidden",
        names(showsForBrowseAll(afterExtrasArrive, "Crime")),
        listOf("The Wire"),
    )
    val afterPilotBecomesSpecial = listOf(
        episode("wire-s1e1", "The Wire", "Crime", season = 0, episode = 1),
        crimeSpecial,
    )
    check(
        "delta-removes-last-preview-season",
        names(showsForBrowseAll(afterPilotBecomesSpecial, "Crime")),
        emptyList(),
    )
    val afterSpecialGainsSeason = listOf(
        episode("bts", "Behind the Scenes", "Crime", season = 1, episode = 1),
    )
    check(
        "delta-gives-extras-title-a-preview-season",
        names(showsForBrowseAll(afterSpecialGainsSeason, "Crime")),
        listOf("Behind the Scenes"),
    )

    // A user genre can be spelled "Shows"; extras still are not shows.
    check(
        "genre-named-Shows-still-drops-extras",
        names(
            showsForBrowseAll(
                listOf(
                    episode("in", "Selected", "Shows"),
                    episode("extra", "Bonus", "Shows", season = 0, episode = 1),
                    episode("out", "Other", "Drama"),
                ),
                BROWSE_ALL_SHOWS_TITLE,
            ),
        ),
        listOf("Selected"),
    )

    // Other kinds tagged Crime are not show cards.
    check(
        "movies-and-tracks-are-not-show-groups",
        names(showsForBrowseAll(listOf(crimePilot, movie, track), "Crime")),
        listOf("The Wire"),
    )

    // Canonical scrape merge: extras folder + numbered folder of the same
    // series stay one card; two extras-only folders stay hidden.
    val mergedReal = listOf(
        episode(
            "svu-s1",
            "Law & Order SVU",
            "Crime",
            scrapedTitle = "Law & Order: Special Victims Unit",
        ),
        episode(
            "svu-extra",
            "Law & Order Special Victims Unit",
            "Crime",
            season = 0,
            episode = 1,
            scrapedTitle = "Law & Order: Special Victims Unit",
        ),
    )
    check(
        "canonical-merge-with-preview-season-kept",
        names(showsForBrowseAll(mergedReal, "Crime")),
        listOf("Law & Order: Special Victims Unit"),
    )
    val mergedExtrasOnly = listOf(
        episode(
            "ghost-a",
            "Ghost Show",
            "Crime",
            season = 0,
            episode = 1,
            scrapedTitle = "Ghost Show",
        ),
        episode(
            "ghost-b",
            "Ghost Show Folder",
            "Crime",
            season = null,
            episode = null,
            scrapedTitle = "Ghost Show",
        ),
    )
    check(
        "canonical-merge-extras-only-hidden",
        names(showsForBrowseAll(mergedExtrasOnly, "Crime")),
        emptyList(),
    )

    // Two real seasons: the group is kept (count is a previewSeasons concern
    // for the card subtitle, but an empty preview list is the 0-seasons bug).
    val twoSeasons = showsForBrowseAll(
        listOf(
            episode("s1", "The Wire", "Crime", season = 1, episode = 1),
            episode("s2", "The Wire", "Crime", season = 2, episode = 1),
        ),
        "Crime",
    ).single()
    val previewCount = CatalogGrouping.previewSeasons(twoSeasons).size
    if (previewCount != 2) {
        failures += "two-preview-seasons: count=$previewCount"
        println("FAIL ${failures.last()}")
    } else {
        println("ok two-preview-seasons")
    }

    // Refresh helper must match the genre shelf for every fixture, including
    // the ones that would have leaked extras-only groups.
    val catalog = listOf(
        crimePilot,
        crimeSpecial,
        crimeUnnumbered,
        crimeNegSeason,
        crimeS1E0,
        comedyPilot,
        episode("wire-s0", "The Wire", "Crime", season = 0, episode = 4),
        episode("split-extra", "Split", "Crime", season = 0, episode = 1),
        episode("split-real", "Split", "Drama", season = 1, episode = 1),
        movie,
        track,
    )
    for (genre in listOf("Crime", "Comedy", "Drama", "Shows", "Western")) {
        check(
            "refresh-matches-genre-shelf[$genre]",
            names(showsForBrowseAll(catalog, genre)),
            names(genreShelfShows(catalog, genre)),
        )
    }

    if (failures.isNotEmpty()) {
        System.err.println(failures.joinToString("; "))
        exitProcess(1)
    }
    println("ALL_OK")
}
