import app.swarm.tv.core.catalog.CatalogGrouping
import app.swarm.tv.core.catalog.MergedEntry
import app.swarm.tv.core.catalog.SeasonGroup
import app.swarm.tv.core.catalog.ShowGroup
import app.swarm.tv.core.peer.CatalogEntry
import app.swarm.tv.core.peer.MediaKind
import app.swarm.tv.core.rest.SwarmJson
import app.swarm.tv.core.watch.InMemoryWatchStateStore
import app.swarm.tv.core.watch.WatchState
import app.swarm.tv.core.watch.getForEntry
import app.swarm.tv.core.watch.stateFor
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.decodeFromString
import kotlinx.serialization.encodeToString
import kotlin.math.max

/**
 * Executable UAT for issue #356: overnight show resume must keep season,
 * episode, and playback progress. Production [stateFor]/[getForEntry] and
 * [WatchState.fromPlayback] are the source of truth — this driver does not
 * reimplement identity matching.
 *
 * Continue-watching / season-list resume predicates are the same rules
 * CatalogScreen and SeasonScreen apply on top of [stateFor].
 */
private val failures = mutableListOf<String>()

private fun check(name: String, ok: Boolean, detail: String = "") {
    if (ok) {
        println("ok $name")
    } else {
        val message = if (detail.isEmpty()) name else "$name: $detail"
        failures += message
        println("FAIL $message")
    }
}

private fun <T> checkEq(name: String, got: T, expected: T) {
    check(name, got == expected, "got=$got expected=$expected")
}

private fun episode(
    fingerprint: String,
    show: String? = "The Wire",
    season: Int? = 1,
    number: Int? = 2,
    title: String = "Episode $number",
): CatalogEntry = CatalogEntry(
    entryKey = "entry-$fingerprint",
    fingerprint = fingerprint,
    kind = MediaKind.EPISODE,
    title = title,
    size = 1,
    showTitle = show,
    season = season,
    episode = number,
)

private fun movie(
    fingerprint: String,
    title: String = "Movie",
    extraType: String? = null,
): CatalogEntry = CatalogEntry(
    entryKey = "entry-$fingerprint",
    fingerprint = fingerprint,
    kind = MediaKind.MOVIE,
    title = title,
    size = 1,
    extraType = extraType,
)

private fun track(fingerprint: String): CatalogEntry = CatalogEntry(
    entryKey = "entry-$fingerprint",
    fingerprint = fingerprint,
    kind = MediaKind.TRACK,
    title = "Track",
    size = 1,
    artist = "Artist",
    album = "Album",
    trackNumber = 1,
)

private fun merged(entry: CatalogEntry) = MergedEntry(entry.fingerprint, listOf("server-a"), entry)

/** Mirrors SwarmViewModel.savePlaybackPosition's identity snapshot. */
private fun snapshot(
    entry: CatalogEntry,
    positionSecs: Double,
    durationSecs: Double,
    updatedAt: Long,
): WatchState = WatchState.fromPlayback(
    positionSecs = positionSecs,
    durationSecs = durationSecs,
    updatedAt = updatedAt,
    showTitle = entry.showTitle.takeIf { entry.kind == MediaKind.EPISODE },
    season = entry.season.takeIf { entry.kind == MediaKind.EPISODE },
    episode = entry.episode.takeIf { entry.kind == MediaKind.EPISODE },
)

/** Mirrors play()/next-episode resume: watched items restart at 0. */
private fun resumeSecs(state: WatchState?): Double =
    state?.takeUnless { it.watched }?.positionSecs ?: 0.0

/**
 * Season-list Resume (#152 / #356): most recently touched unfinished
 * episode, including after a replacement encode changes the fingerprint.
 */
private fun resumeEpisode(show: ShowGroup, watchStates: Map<String, WatchState>): MergedEntry? =
    show.seasons.asSequence()
        .flatMap { it.episodes.asSequence() }
        .mapNotNull { episode ->
            val saved = watchStates.stateFor(episode.entry)
            if (saved == null || saved.watched || saved.positionSecs <= 0.0) null else episode to saved
        }
        .maxByOrNull { it.second.updatedAt }
        ?.first

private const val MAX_CONTINUE_WATCHING = 6

/** Home Continue Watching: one card per show, cap 6, exclude watched/zero. */
private fun continueWatching(
    entries: List<MergedEntry>,
    watchStates: Map<String, WatchState>,
): List<MergedEntry> {
    val shows = CatalogGrouping.groupEpisodesByShowSeason(entries)
    val showByEpisode = buildMap {
        for (show in shows) {
            for (season in show.seasons) {
                for (ep in season.episodes) put(ep.entry.fingerprint, show)
            }
        }
    }
    val inProgress = entries.mapNotNull { entry ->
        if (entry.entry.kind == MediaKind.TRACK) return@mapNotNull null
        val saved = watchStates.stateFor(entry.entry) ?: return@mapNotNull null
        if (saved.watched || saved.positionSecs <= 0.0) null else entry to saved
    }
    val movieItems = inProgress.filter {
        it.first.entry.kind == MediaKind.MOVIE && it.first.entry.extraType == null
    }
    val episodeItems = inProgress
        .filter { it.first.entry.kind == MediaKind.EPISODE }
        .groupBy { (entry, _) -> showByEpisode[entry.entry.fingerprint]?.show ?: entry.entry.showTitle.orEmpty() }
        .values
        .mapNotNull { candidates -> candidates.maxByOrNull { it.second.updatedAt } }
    return (movieItems + episodeItems)
        .sortedByDescending { it.second.updatedAt }
        .take(MAX_CONTINUE_WATCHING)
        .map { it.first }
}

/** Mirrors SwarmViewModel's strictly increasing save clock. */
private fun nextUpdatedAt(nowMs: Long, lastUpdatedAt: Long): Long = max(nowMs, lastUpdatedAt + 1)

/** Mirrors SwarmViewModel's startup merge of disk vs live progress. */
private fun mergeLoaded(
    loaded: Map<String, WatchState>,
    live: Map<String, WatchState>,
): Map<String, WatchState> =
    (loaded.keys + live.keys).associateWith { fingerprint ->
        listOfNotNull(loaded[fingerprint], live[fingerprint]).maxBy { it.updatedAt }
    }

fun main() = runBlocking {
    captureSeasonEpisodeAndProgress()
    overnightFingerprintReplacement()
    fingerprintWinsOverIdentity()
    watchedRestartsAtZero()
    completionThresholdBoundaries()
    malformedAndPartialIdentity()
    timestampOrdering()
    sessionRestoreMerge()
    continueWatchingInvariants()
    seasonResumeInvariants()
    wireShapeAndLegacy()
    moviesAndTracksStayFingerprintOnly()

    if (failures.isNotEmpty()) {
        throw IllegalStateException("FAILED ${failures.size}: ${failures.joinToString("; ")}")
    }
    println("ALL_OK")
}

private suspend fun captureSeasonEpisodeAndProgress() {
    val entry = episode("file-v1", show = "The Wire", season = 3, number = 9)
    val saved = snapshot(entry, positionSecs = 1234.5, durationSecs = 3600.0, updatedAt = 10)

    checkEq("captured-show-title", saved.showTitle, "The Wire")
    checkEq("captured-season", saved.season, 3)
    checkEq("captured-episode", saved.episode, 9)
    checkEq("captured-progress", saved.positionSecs, 1234.5)
    check("in-progress-is-not-watched", !saved.watched, "watched=${saved.watched}")

    val store = InMemoryWatchStateStore()
    store.set(entry.fingerprint, saved)
    val loaded = store.getForEntry(entry)
    checkEq("store-roundtrip-season", loaded?.season, 3)
    checkEq("store-roundtrip-episode", loaded?.episode, 9)
    checkEq("store-roundtrip-progress", loaded?.positionSecs, 1234.5)
    checkEq("resume-uses-saved-progress", resumeSecs(loaded), 1234.5)
}

private suspend fun overnightFingerprintReplacement() {
    val original = episode("encode-v1", show = "  THE WIRE ", season = 1, number = 2)
    val replacement = episode("encode-v2", show = "The Wire", season = 1, number = 2)
    val saved = snapshot(original, 1812.0, 3600.0, 50)

    val store = InMemoryWatchStateStore()
    store.set(original.fingerprint, saved)

    val recovered = store.getForEntry(replacement)
    checkEq("overnight-identity-progress", recovered?.positionSecs, 1812.0)
    checkEq("overnight-identity-season", recovered?.season, 1)
    checkEq("overnight-identity-episode", recovered?.episode, 2)
    checkEq("map-stateFor-progress", mapOf(original.fingerprint to saved).stateFor(replacement)?.positionSecs, 1812.0)

    val otherEpisode = episode("encode-v2-e3", show = "The Wire", season = 1, number = 3)
    check("replacement-does-not-bleed-to-next-episode", store.getForEntry(otherEpisode) == null)

    val otherShow = episode("encode-v2", show = "The Sopranos", season = 1, number = 2)
    check("replacement-does-not-bleed-across-shows", store.getForEntry(otherShow) == null)

    val twoOld = mapOf(
        "older-encode" to snapshot(original, 100.0, 3600.0, 1),
        "newer-encode" to snapshot(original, 1812.0, 3600.0, 9),
    )
    checkEq("newest-identity-match-wins", twoOld.stateFor(replacement)?.positionSecs, 1812.0)
}

private suspend fun fingerprintWinsOverIdentity() {
    val current = episode("current")
    val direct = WatchState.fromPlayback(300.0, 3600.0, 10)
    val fallback = snapshot(episode("old", season = 1, number = 2), 900.0, 3600.0, 99)
    val states = mapOf(current.fingerprint to direct, "old" to fallback)

    checkEq("fingerprint-beats-newer-identity", states.stateFor(current)?.positionSecs, 300.0)

    val store = InMemoryWatchStateStore()
    store.set(current.fingerprint, direct)
    store.set("old", fallback)
    checkEq("store-fingerprint-beats-identity", store.getForEntry(current)?.positionSecs, 300.0)
}

private fun watchedRestartsAtZero() {
    val finished = WatchState.fromPlayback(3420.0, 3600.0, 1, "The Wire", 1, 2)
    check("ninety-five-percent-is-watched", finished.watched)
    checkEq("watched-resume-is-zero", resumeSecs(finished), 0.0)

    val replacement = episode("new-file")
    val states = mapOf("old-file" to finished)
    val recovered = states.stateFor(replacement)
    check("watched-identity-still-resolves", recovered?.watched == true)
    checkEq("watched-identity-resume-is-zero", resumeSecs(recovered), 0.0)
}

private fun completionThresholdBoundaries() {
    check("exactly-95-is-watched", WatchState.fromPlayback(95.0, 100.0, 1).watched)
    check("just-under-95-is-not-watched", !WatchState.fromPlayback(94.9, 100.0, 1).watched)
    check("zero-duration-is-never-watched", !WatchState.fromPlayback(50.0, 0.0, 1).watched)
    check("negative-duration-is-never-watched", !WatchState.fromPlayback(95.0, -100.0, 1).watched)
    check("zero-position-is-not-watched", !WatchState.fromPlayback(0.0, 100.0, 1).watched)
    check("past-end-is-watched", WatchState.fromPlayback(200.0, 100.0, 1).watched)

    val nan = WatchState.fromPlayback(Double.NaN, 100.0, 1)
    check("nan-position-is-not-watched", !nan.watched)

    val unsetDuration = WatchState.fromPlayback(1234.0, 0.0, 1, "The Wire", 1, 2)
    checkEq("unset-duration-keeps-position", unsetDuration.positionSecs, 1234.0)
    check("unset-duration-keeps-identity", unsetDuration.season == 1 && unsetDuration.episode == 2)
}

private fun malformedAndPartialIdentity() {
    val replacement = episode("new")
    check(
        "blank-show-title-cannot-fallback",
        mapOf("old" to WatchState.fromPlayback(100.0, 1000.0, 1, "   ", 1, 2)).stateFor(replacement) == null,
    )
    check(
        "empty-show-title-cannot-fallback",
        mapOf("old" to WatchState.fromPlayback(100.0, 1000.0, 1, "", 1, 2)).stateFor(replacement) == null,
    )
    check(
        "null-show-title-cannot-fallback",
        mapOf("old" to WatchState.fromPlayback(100.0, 1000.0, 1, null, 1, 2)).stateFor(replacement) == null,
    )
    check(
        "missing-season-cannot-fallback",
        mapOf("old" to WatchState.fromPlayback(100.0, 1000.0, 1, "The Wire", null, 2)).stateFor(replacement) == null,
    )
    check(
        "missing-episode-cannot-fallback",
        mapOf("old" to WatchState.fromPlayback(100.0, 1000.0, 1, "The Wire", 1, null)).stateFor(replacement) == null,
    )
    check(
        "unnumbered-catalog-episode-cannot-fallback",
        mapOf("old" to snapshot(episode("old"), 100.0, 1000.0, 1))
            .stateFor(episode("new", season = null, number = null)) == null,
    )
    check(
        "whitespace-and-case-normalize-show-title",
        mapOf("old" to WatchState.fromPlayback(77.0, 1000.0, 1, "  THE WIRE ", 1, 2))
            .stateFor(episode("new", show = "the wire"))?.positionSecs == 77.0,
    )

    val s0e1 = WatchState.fromPlayback(10.0, 100.0, 1, "The Wire", 0, 1)
    val s0e2 = episode("special-2", season = 0, number = 2)
    check("season-zero-specials-are-distinct", mapOf("s0e1" to s0e1).stateFor(s0e2) == null)
    check(
        "season-zero-same-number-matches",
        mapOf("s0e1" to s0e1).stateFor(episode("special-1b", season = 0, number = 1))?.positionSecs == 10.0,
    )
}

private suspend fun timestampOrdering() {
    val store = InMemoryWatchStateStore()
    val newer = WatchState.fromPlayback(900.0, 3600.0, 20, "The Wire", 1, 2)
    store.set("fp", newer)
    store.set("fp", WatchState.fromPlayback(12.0, 3600.0, 19, "The Wire", 1, 2))
    checkEq("older-heartbeat-cannot-roll-progress-back", store.get("fp")?.positionSecs, 900.0)

    store.set("fp", WatchState.fromPlayback(2400.0, 3600.0, 5, "The Wire", 1, 2))
    checkEq("older-timestamp-with-larger-position-is-still-rejected", store.get("fp")?.positionSecs, 900.0)

    val rewind = WatchState.fromPlayback(30.0, 3600.0, 21, "The Wire", 1, 2)
    store.set("fp", rewind)
    checkEq("newer-intentional-rewind-is-kept", store.get("fp")?.positionSecs, 30.0)

    checkEq("same-ms-saves-stay-ordered", nextUpdatedAt(100, 100), 101L)
    checkEq("clock-behind-last-save-still-advances", nextUpdatedAt(50, 100), 101L)
    checkEq("wall-clock-ahead-is-used", nextUpdatedAt(200, 100), 200L)
}

private fun sessionRestoreMerge() {
    val live = mapOf("fp" to WatchState.fromPlayback(400.0, 3600.0, 10, "The Wire", 1, 2))
    val loaded = mapOf(
        "fp" to WatchState.fromPlayback(40.0, 3600.0, 5, "The Wire", 1, 2),
        "other" to WatchState.fromPlayback(90.0, 3600.0, 8, "The Wire", 2, 1),
    )
    val merged = mergeLoaded(loaded, live)
    checkEq("live-progress-survives-stale-disk-snapshot", merged["fp"]?.positionSecs, 400.0)
    checkEq("disk-only-fingerprints-are-kept", merged["other"]?.positionSecs, 90.0)

    val diskNewer = mergeLoaded(
        mapOf("fp" to WatchState.fromPlayback(800.0, 3600.0, 20, "The Wire", 1, 2)),
        mapOf("fp" to WatchState.fromPlayback(100.0, 3600.0, 2, "The Wire", 1, 2)),
    )
    checkEq("newer-disk-snapshot-wins-over-stale-live", diskNewer["fp"]?.positionSecs, 800.0)
}

private fun continueWatchingInvariants() {
    val s1e1 = merged(episode("s1e1", season = 1, number = 1))
    val s1e2 = merged(episode("s1e2", season = 1, number = 2))
    val s2e1 = merged(episode("s2e1", season = 2, number = 1))
    val otherShow = merged(episode("expanse-e1", show = "The Expanse", season = 1, number = 1))
    val film = merged(movie("movie-1", "Film"))
    val extra = merged(movie("movie-extra", extraType = "trailer"))
    val song = merged(track("track-1"))

    val states = mapOf(
        "s1e1" to WatchState.fromPlayback(100.0, 1000.0, 1, "The Wire", 1, 1),
        "s1e2" to WatchState.fromPlayback(200.0, 1000.0, 5, "The Wire", 1, 2),
        "s2e1" to WatchState.fromPlayback(950.0, 1000.0, 9, "The Wire", 2, 1),
        "expanse-e1" to WatchState.fromPlayback(50.0, 1000.0, 8, "The Expanse", 1, 1),
        "movie-1" to WatchState.fromPlayback(10.0, 100.0, 7),
        "movie-extra" to WatchState.fromPlayback(10.0, 100.0, 20),
        "track-1" to WatchState.fromPlayback(10.0, 100.0, 30),
    )
    val row = continueWatching(listOf(s1e1, s1e2, s2e1, otherShow, film, extra, song), states)
    checkEq("one-card-per-show", row.map { it.fingerprint }, listOf("expanse-e1", "movie-1", "s1e2"))
    check("watched-episode-excluded", row.none { it.fingerprint == "s2e1" })
    check("movie-extras-excluded", row.none { it.fingerprint == "movie-extra" })
    check("tracks-excluded", row.none { it.fingerprint == "track-1" })

    val replaced = merged(episode("s1e2-reencode", season = 1, number = 2))
    val afterReplacement = continueWatching(
        listOf(s1e1, replaced, s2e1),
        mapOf("old-s1e2" to WatchState.fromPlayback(200.0, 1000.0, 5, "The Wire", 1, 2)),
    )
    checkEq("continue-watching-survives-reencode", afterReplacement.map { it.fingerprint }, listOf("s1e2-reencode"))

    val manyShows = (1..8).map { n ->
        merged(episode("show-$n", show = "Show $n", season = 1, number = 1))
    }
    val manyStates = (1..8).associate { n ->
        "show-$n" to WatchState.fromPlayback(10.0, 100.0, n.toLong(), "Show $n", 1, 1)
    }
    val capped = continueWatching(manyShows, manyStates)
    checkEq("continue-watching-cap-is-six", capped.size, 6)
    checkEq("continue-watching-is-most-recent", capped.map { it.fingerprint }, listOf("show-8", "show-7", "show-6", "show-5", "show-4", "show-3"))

    val zeroPos = continueWatching(
        listOf(s1e1),
        mapOf("s1e1" to WatchState.fromPlayback(0.0, 1000.0, 1, "The Wire", 1, 1)),
    )
    check("zero-position-is-not-continue-watching", zeroPos.isEmpty())

    val negativePos = continueWatching(
        listOf(s1e1),
        mapOf("s1e1" to WatchState(positionSecs = -1.0, durationSecs = 1000.0, watched = false, updatedAt = 1, showTitle = "The Wire", season = 1, episode = 1)),
    )
    check("negative-position-is-not-started", negativePos.isEmpty())
}

private fun seasonResumeInvariants() {
    val s1e1 = merged(episode("s1e1", season = 1, number = 1))
    val s1e2 = merged(episode("s1e2", season = 1, number = 2))
    val s2e1 = merged(episode("s2e1", season = 2, number = 1))
    val show = ShowGroup(
        show = "The Wire",
        seasons = listOf(
            SeasonGroup(1, listOf(s1e1, s1e2)),
            SeasonGroup(2, listOf(s2e1)),
        ),
    )

    check("no-resume-when-untouched", resumeEpisode(show, emptyMap()) == null)
    check(
        "no-resume-when-all-watched",
        resumeEpisode(
            show,
            mapOf(
                "s1e1" to WatchState.fromPlayback(96.0, 100.0, 1, "The Wire", 1, 1),
                "s1e2" to WatchState.fromPlayback(96.0, 100.0, 2, "The Wire", 1, 2),
            ),
        ) == null,
    )
    check(
        "no-resume-at-zero-position",
        resumeEpisode(show, mapOf("s1e2" to WatchState.fromPlayback(0.0, 1000.0, 1, "The Wire", 1, 2))) == null,
    )

    val midSeason = mapOf(
        "s1e2" to WatchState.fromPlayback(200.0, 1000.0, 5, "The Wire", 1, 2),
        "s2e1" to WatchState.fromPlayback(960.0, 1000.0, 9, "The Wire", 2, 1),
    )
    checkEq("resume-unfinished-even-if-later-episode-watched", resumeEpisode(show, midSeason)?.fingerprint, "s1e2")

    val replaced = mapOf(
        "replaced-file" to WatchState.fromPlayback(300.0, 3000.0, 10, "the wire", 1, 2),
    )
    checkEq("resume-finds-reencoded-episode", resumeEpisode(show, replaced)?.fingerprint, "s1e2")
    checkEq("resume-progress-is-the-saved-offset", replaced.stateFor(s1e2.entry)?.positionSecs, 300.0)
}

private fun wireShapeAndLegacy() {
    val state = snapshot(episode("fp", show = "The Expanse", season = 2, number = 4), 125.5, 5400.0, 1_700_000_000_000)
    val json = SwarmJson.encodeToString(state)
    check(
        "wire-includes-show-season-episode-and-progress",
        json == """{"position_secs":125.5,"duration_secs":5400.0,"watched":false,"updated_at":1700000000000,"show_title":"The Expanse","season":2,"episode":4}""",
        json,
    )
    checkEq("wire-roundtrip", SwarmJson.decodeFromString<WatchState>(json), state)

    val legacy = SwarmJson.decodeFromString<WatchState>(
        """{"position_secs":42.0,"duration_secs":100.0,"watched":false,"updated_at":1}""",
    )
    check("legacy-json-has-null-identity", legacy.showTitle == null && legacy.season == null && legacy.episode == null)
    checkEq("legacy-json-keeps-progress", legacy.positionSecs, 42.0)

    val extra = SwarmJson.decodeFromString<WatchState>(
        """{"position_secs":1.0,"duration_secs":10.0,"watched":false,"updated_at":1,"nope":true}""",
    )
    checkEq("unknown-wire-fields-are-ignored", extra.positionSecs, 1.0)

    val partial = SwarmJson.decodeFromString<WatchState>(
        """{"position_secs":9.0,"duration_secs":10.0,"watched":false,"updated_at":1,"show_title":"The Wire"}""",
    )
    check("partial-identity-does-not-invent-season-episode", partial.season == null && partial.episode == null)

    var malformed = false
    try {
        SwarmJson.decodeFromString<WatchState>("{not json}")
    } catch (_: Exception) {
        malformed = true
    }
    check("malformed-json-does-not-decode-as-progress", malformed)

    var missingFields = false
    try {
        SwarmJson.decodeFromString<WatchState>("{}")
    } catch (_: Exception) {
        missingFields = true
    }
    check("empty-object-is-not-silent-zero-progress", missingFields)
}

private fun moviesAndTracksStayFingerprintOnly() {
    val identity = WatchState.fromPlayback(100.0, 1000.0, 1, "The Wire", 1, 2)
    val movieWithLabels = movie("movie-1").copy(showTitle = "The Wire", season = 1, episode = 2)
    check("movies-do-not-use-episode-identity", mapOf("old" to identity).stateFor(movieWithLabels) == null)

    val savedMovie = snapshot(movie("movie-1"), 50.0, 100.0, 1)
    check("movie-snapshot-has-no-show-title", savedMovie.showTitle == null)
    check("movie-snapshot-has-no-season", savedMovie.season == null)
    check("movie-snapshot-has-no-episode", savedMovie.episode == null)

    val savedTrack = snapshot(track("t1").copy(showTitle = "The Wire", season = 1, episode = 2), 50.0, 100.0, 1)
    check("track-snapshot-has-no-episode-identity", savedTrack.showTitle == null && savedTrack.season == null)
}
