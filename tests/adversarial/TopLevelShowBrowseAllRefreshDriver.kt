import app.swarm.tv.core.catalog.CatalogGrouping
import app.swarm.tv.core.catalog.MergedEntry
import app.swarm.tv.core.catalog.ShowGroup
import app.swarm.tv.core.peer.CatalogEntry
import app.swarm.tv.core.peer.MediaKind
import kotlin.system.exitProcess

/**
 * Executable membership for #394: the *top-level* (un-scoped, scopedToGenre =
 * false) Shows Browse All page — reached from `CatalogScreen`'s own `val
 * shows` grid, `openShowShelf()`, `openShowSeasons()`, and every
 * `replaceEmbeddedCatalog` branch that rebuilds those states on a catalog
 * delta — must all agree on the same "browsable" membership:
 * [CatalogGrouping.groupEpisodesByShowSeason] filtered to groups with a
 * non-empty [CatalogGrouping.previewSeasons]. A group that is only extras
 * (season 0, null/negative season, or a season with no numbered episode)
 * must never render as a "0 seasons" card, on first load or after a delta.
 *
 * The production fix routes every one of those call sites through the new
 * [CatalogGrouping.browsableShows] helper. This driver does not trust that
 * routing (the source-level checks in the Python harness do); it re-derives
 * the domain invariant directly against [CatalogGrouping.browsableShows]
 * itself, independent of how any call site is wired, so a future refactor
 * that reintroduces an unfiltered call site is caught by the source checks
 * while a future bug in the filter's domain logic is caught here.
 */
private fun episode(
    key: String,
    show: String,
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
        genres = emptyList(),
    ),
)

/** The exact definition the fix should be equivalent to, re-derived independently. */
private fun expectedBrowsable(entries: List<MergedEntry>): List<ShowGroup> =
    CatalogGrouping.groupEpisodesByShowSeason(entries).filter { CatalogGrouping.previewSeasons(it).isNotEmpty() }

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
    val pilot = episode("wire-s1e1", "The Wire")
    val special = episode("bts", "Behind the Scenes", season = 0, episode = 1)
    val unnumbered = episode("interview", "Interview", season = null, episode = null)
    val negSeason = episode("neg", "Negative Season", season = -1, episode = 1)
    val s1e0 = episode("recap", "Recap Dump", season = 1, episode = 0)
    val s1NullEp = episode("featurette", "Featurette Dump", season = 1, episode = null)
    val s1NegEp = episode("neg-ep", "Neg Episode", season = 1, episode = -3)
    val friends = episode("friends-s1e1", "Friends")
    val movie = episode("movie", "Not A Show", kind = MediaKind.MOVIE)
    val track = episode("track", "Not A Show", kind = MediaKind.TRACK)

    // Domain: extras-only groups never become top-level show cards.
    check(
        "season-zero-with-numbered-episode-is-not-a-show",
        names(CatalogGrouping.browsableShows(listOf(pilot, special))),
        listOf("The Wire"),
    )
    check(
        "null-season-unnumbered-episode-is-not-a-show",
        names(CatalogGrouping.browsableShows(listOf(pilot, unnumbered))),
        listOf("The Wire"),
    )
    check(
        "negative-season-is-not-a-show",
        names(CatalogGrouping.browsableShows(listOf(pilot, negSeason))),
        listOf("The Wire"),
    )
    check(
        "season-one-episode-zero-is-not-a-show",
        names(CatalogGrouping.browsableShows(listOf(pilot, s1e0))),
        listOf("The Wire"),
    )
    check(
        "season-one-null-episode-is-not-a-show",
        names(CatalogGrouping.browsableShows(listOf(pilot, s1NullEp))),
        listOf("The Wire"),
    )
    check(
        "season-one-negative-episode-is-not-a-show",
        names(CatalogGrouping.browsableShows(listOf(pilot, s1NegEp))),
        listOf("The Wire"),
    )
    check(
        "catalog-of-only-extras-is-empty",
        names(CatalogGrouping.browsableShows(listOf(special, unnumbered, negSeason))),
        emptyList(),
    )
    check("empty-catalog", names(CatalogGrouping.browsableShows(emptyList())), emptyList())
    check(
        "movies-and-tracks-are-not-show-groups",
        names(CatalogGrouping.browsableShows(listOf(pilot, movie, track))),
        listOf("The Wire"),
    )

    // Mixed groups stay: extras live under the show, they just cannot be the
    // only reason the card exists.
    val mixed = listOf(
        pilot,
        episode("wire-s0e1", "The Wire", season = 0, episode = 1),
        episode("wire-feat", "The Wire", season = 2, episode = null),
    )
    val mixedGroups = CatalogGrouping.browsableShows(mixed)
    check("mixed-show-kept", names(mixedGroups), listOf("The Wire"))
    val mixedSeasons = mixedGroups.single().seasons.map { it.season }
    if (0 !in mixedSeasons) {
        failures += "mixed-show-keeps-extras-seasons: seasons=$mixedSeasons"
        println("FAIL ${failures.last()}")
    } else {
        println("ok mixed-show-keeps-extras-seasons")
    }

    // Canonical scrape merge: extras folder + numbered folder of the same
    // series stay one card; two extras-only folders stay hidden.
    val mergedReal = listOf(
        episode("svu-s1", "Law & Order SVU", scrapedTitle = "Law & Order: Special Victims Unit"),
        episode(
            "svu-extra",
            "Law & Order Special Victims Unit",
            season = 0,
            episode = 1,
            scrapedTitle = "Law & Order: Special Victims Unit",
        ),
    )
    check(
        "canonical-merge-with-preview-season-kept",
        names(CatalogGrouping.browsableShows(mergedReal)),
        listOf("Law & Order: Special Victims Unit"),
    )
    val mergedExtrasOnly = listOf(
        episode("ghost-a", "Ghost Show", season = 0, episode = 1, scrapedTitle = "Ghost Show"),
        episode("ghost-b", "Ghost Show Folder", season = null, episode = null, scrapedTitle = "Ghost Show"),
    )
    check(
        "canonical-merge-extras-only-hidden",
        names(CatalogGrouping.browsableShows(mergedExtrasOnly)),
        emptyList(),
    )

    // This is exactly the reported bug's reproduction: a catalog delta lands
    // while the viewer is still on the un-scoped ShowShelf/ShowSeasons page.
    // Whatever rebuilds the grid from the new catalog must apply the same
    // preview-season drop the initial render used, every time, not just once.
    val beforeDelta = listOf(pilot, friends)
    check("before-delta", names(CatalogGrouping.browsableShows(beforeDelta)), listOf("Friends", "The Wire"))
    val afterExtrasArrive = beforeDelta + special + unnumbered
    check(
        "delta-adds-extras-only-groups-they-stay-hidden",
        names(CatalogGrouping.browsableShows(afterExtrasArrive)),
        listOf("Friends", "The Wire"),
    )
    val afterPilotBecomesSpecial = listOf(
        episode("wire-s1e1", "The Wire", season = 0, episode = 1),
        special,
        friends,
    )
    check(
        "delta-removes-last-preview-season",
        names(CatalogGrouping.browsableShows(afterPilotBecomesSpecial)),
        listOf("Friends"),
    )
    val afterSpecialGainsSeason = listOf(episode("bts", "Behind the Scenes", season = 1, episode = 1))
    check(
        "delta-gives-extras-title-a-preview-season",
        names(CatalogGrouping.browsableShows(afterSpecialGainsSeason)),
        listOf("Behind the Scenes"),
    )

    // Behavioral equivalence across a battery of fixtures: browsableShows
    // must never diverge from groupEpisodesByShowSeason + a previewSeasons
    // drop, for every shape above and every delta step. This is the
    // regression #394 reported: a rebuild path used the unfiltered grouping
    // directly instead of this composition.
    val fixtures = listOf(
        listOf(pilot),
        listOf(pilot, special),
        listOf(pilot, unnumbered),
        listOf(pilot, negSeason),
        listOf(pilot, s1e0),
        listOf(pilot, s1NullEp),
        listOf(pilot, s1NegEp),
        listOf(special, unnumbered, negSeason),
        emptyList(),
        listOf(pilot, movie, track),
        mixed,
        mergedReal,
        mergedExtrasOnly,
        beforeDelta,
        afterExtrasArrive,
        afterPilotBecomesSpecial,
        afterSpecialGainsSeason,
    )
    for ((index, fixture) in fixtures.withIndex()) {
        check(
            "equivalence[$index]",
            names(CatalogGrouping.browsableShows(fixture)),
            names(expectedBrowsable(fixture)),
        )
    }

    if (failures.isNotEmpty()) {
        System.err.println(failures.joinToString("; "))
        exitProcess(1)
    }
    println("ALL_OK")
}
