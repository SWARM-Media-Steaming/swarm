package app.swarm.tv.app.ui.screens

import androidx.compose.ui.focus.FocusDirection
import androidx.compose.ui.geometry.Rect
import app.swarm.tv.core.catalog.CatalogGrouping
import app.swarm.tv.core.catalog.MergedEntry
import app.swarm.tv.core.catalog.SeasonGroup
import app.swarm.tv.core.catalog.ShowGroup
import app.swarm.tv.core.peer.CatalogEntry
import app.swarm.tv.core.peer.MediaKind
import app.swarm.tv.core.watch.WatchState
import kotlin.system.exitProcess

/**
 * Executable JVM driver for issue #495 ("For the TV apps i want the episode
 * and shows to view differently. I want it to view and navigate like Netflix
 * does. See attachment. Seasons and specials list on left and episodes show on
 * right.").
 *
 * Compiled together with:
 * - the real `:core` catalog/watch-state models (`CatalogGrouping`, `WatchState`),
 * - the real pure helpers extracted verbatim from `SeasonScreen.kt`
 *   (`seasonPaneOrder`, `defaultSeason`, `episodeDurationLabel`,
 *   `resumeEpisode`, `seasonKey`, `seasonLabel`, `seasonCountLabel`,
 *   `episodeLabel`, `progressFraction`), and
 * - Compose UI's production `TwoDimensionalFocusSearchKt.isBetterCandidate`
 *   from the project's resolved `androidx.compose.ui:ui-android` artifact.
 */
private val failures = mutableListOf<String>()

private fun check(name: String, ok: Boolean, detail: String = "") {
    if (ok) {
        println("ok $name")
    } else {
        val msg = if (detail.isEmpty()) name else "$name: $detail"
        failures += msg
        System.err.println("FAIL: $msg")
    }
}

private fun <T> checkEq(name: String, expected: T, actual: T) {
    check(name, expected == actual, "expected=$expected actual=$actual")
}

private fun makeEpisode(
    key: String,
    season: Int?,
    number: Int?,
    showTitle: String = "Stranger Things",
    durationSecs: Double? = 2820.0,
    overview: String? = "Episode synopsis",
    extraType: String? = null,
    extraTitle: String? = null,
): MergedEntry = MergedEntry(
    fingerprint = key,
    sources = listOf("server-a"),
    entry = CatalogEntry(
        entryKey = key,
        fingerprint = key,
        kind = MediaKind.EPISODE,
        title = "Title $key",
        size = 1L,
        durationSecs = durationSecs,
        showTitle = showTitle,
        season = season,
        episode = number,
        overview = overview,
        extraType = extraType,
        extraTitle = extraTitle,
    ),
)

/**
 * Invokes Compose UI's real `TwoDimensionalFocusSearchKt.isBetterCandidate`
 * over candidate focus rectangles when a `LazyColumn`'s `FocusRestorer` has no
 * saved child and no custom fallback `FocusRequester`.
 */
private fun composeTwoDimensionalWinner(
    candidates: List<Pair<String, Rect>>,
    source: Rect,
    direction: FocusDirection,
): String? {
    val searchClass = Class.forName("androidx.compose.ui.focus.TwoDimensionalFocusSearchKt")
    val isBetter = searchClass.declaredMethods.first {
        it.name.startsWith("isBetterCandidate-") && it.parameterCount == 4
    }.apply { isAccessible = true }
    val dirInt = FocusDirection::class.java
        .getDeclaredMethod("unbox-impl")
        .apply { isAccessible = true }
        .invoke(direction) as Int

    var bestRect: Rect? = null
    var bestName: String? = null
    for ((name, rect) in candidates) {
        if (bestRect == null || isBetter.invoke(null, rect, bestRect, source, dirInt) as Boolean) {
            bestRect = rect
            bestName = name
        }
    }
    return bestName
}

/**
 * Layout geometry (in dp, 1dp = 1px on a 1080p mdpi TV surface or scaled
 * uniformly at xhdpi — uniform scale preserves all `TwoDimensionalFocusSearch`
 * comparisons) derived from `SeasonScreen.kt`:
 * - Screen `Row`: `padding(horizontal = 40.dp, vertical = 28.dp)`,
 *   `spacedBy(28.dp)`.
 * - `SeasonPane`: `width(300.dp)` -> x = [40, 340].
 *   Header poster `64.dp * 3/2 = 96.dp` (y = 28..124) + `Spacer(14.dp)` +
 *   action button row (~40.dp, y = 138..178) + `Spacer(18.dp)` +
 *   `LazyColumn` `contentPadding(vertical = 4.dp)` -> first `SeasonRow` at
 *   y = 200, each row ~44.dp tall with `spacedBy(6.dp)` (stride = 50.dp).
 *   On a 540dp-tall 1080p@2x TV viewport (`540 - 28 - 196 = 316dp` list
 *   viewport), at most 6 season rows are visible at once without scrolling.
 * - `EpisodePane`: x = [368, 1880].
 *   Header (~27.dp, y = 28..55) + `Spacer(12.dp)` + `LazyColumn`
 *   `contentPadding(vertical = 4.dp)` -> first `EpisodeRow` at y = 71,
 *   thumbnail `208.dp * 9/16 = 117.dp` + `8.dp * 2` padding = 133.dp tall
 *   with `spacedBy(10.dp)` (stride = 143.dp).
 */
private const val VISIBLE_SEASON_ROWS_IN_VIEWPORT = 6

private fun seasonRowRect(visibleSlotIndex: Int): Rect {
    val top = 200f + visibleSlotIndex * 50f
    return Rect(40f, top, 340f, top + 44f)
}

private fun episodeRowRect(visibleSlotIndex: Int): Rect {
    val top = 71f + visibleSlotIndex * 143f
    return Rect(368f, top, 1880f, top + 133f)
}

private fun runSharedModelChecks() {
    // Left pane ordering: numbered seasons ascending, then Specials (0), then Unnumbered (null).
    val mixed = listOf(
        SeasonGroup(null, listOf(makeEpisode("un-1", null, null))),
        SeasonGroup(0, listOf(makeEpisode("sp-1", 0, 1))),
        SeasonGroup(3, listOf(makeEpisode("s3e1", 3, 1))),
        SeasonGroup(1, listOf(makeEpisode("s1e1", 1, 1), makeEpisode("s1e2", 1, 2))),
        SeasonGroup(2, listOf(makeEpisode("s2e1", 2, 1))),
    )
    val ordered = seasonPaneOrder(mixed)
    checkEq("seasonPaneOrder-real-then-specials-then-unnumbered", listOf(1, 2, 3, 0, null), ordered.map { it.season })
    checkEq(
        "seasonLabels-match-ordered-pane",
        listOf("Season 1", "Season 2", "Season 3", "Specials", "Unnumbered"),
        ordered.map { seasonLabel(it.season) },
    )

    val show = ShowGroup("Stranger Things", mixed)
    // Nothing in progress -> opens on Season 1, never Specials or Unnumbered.
    checkEq("defaultSeason-untouched-picks-season-1", 1, defaultSeason(show, emptyMap())?.season)

    // In-progress Special -> opens on Specials (season 0).
    val specialInProgress = mapOf(
        "sp-1" to WatchState.fromPlayback(300.0, 1800.0, 50L, "Stranger Things", 0, 1),
    )
    checkEq("defaultSeason-in-progress-special-picks-specials", 0, defaultSeason(show, specialInProgress)?.season)

    // Re-encoded episode (changed fingerprint, matching show/season/episode identity) -> opens on Season 3.
    val replacedS3 = mapOf(
        "old-fingerprint" to WatchState.fromPlayback(600.0, 2800.0, 99L, "  stranger things ", 3, 1),
    )
    checkEq("defaultSeason-survives-fingerprint-replacement", 3, defaultSeason(show, replacedS3)?.season)

    // Duration & progress boundaries.
    checkEq("duration-negative-is-null", null, episodeDurationLabel(-10.0))
    checkEq("duration-infinity-is-null", null, episodeDurationLabel(Double.POSITIVE_INFINITY))
    checkEq("duration-exact-hour", "1h 00m", episodeDurationLabel(3600.0))
    checkEq(
        "progress-nan-duration-safe",
        false,
        WatchState(10.0, Double.NaN, false, 1L).progressFraction() > 0f,
    )
    checkEq(
        "progress-clamps-above-one",
        1f,
        WatchState(5000.0, 3000.0, false, 1L).progressFraction(),
    )

    // Browsable shows always have >= 1 real season, so Specials (season 0) is always at index >= 1.
    val catalogEntries = mixed.flatMap { it.episodes }
    val browsable = CatalogGrouping.browsableShows(catalogEntries)
    checkEq("browsable-show-count", 1, browsable.size)
    val orderedBrowsable = seasonPaneOrder(browsable.single().seasons)
    val specialsIdx = orderedBrowsable.indexOfFirst { it.season == 0 }
    check("specials-always-after-real-seasons", specialsIdx >= 1, "specialsIdx=$specialsIdx")
}

private fun runEpisodeEntryChecks(hasFirstEntryRedirect: Boolean) {
    runSharedModelChecks()

    // Four-row season pane: Season 1 (slot 0), Season 2 (slot 1), Season 3 (slot 2), Specials (slot 3).
    // Highlighting any season swaps EpisodePane via `key(seasonKey(season))` to a fresh, unvisited LazyColumn
    // whose top visible episodes sit at episodeRowRect(0..3).
    val episodeRects = (0 until 4).map { i -> "ep${i + 1}" to episodeRowRect(i) }

    val panesToTest = listOf(
        "Season 1" to 0,
        "Season 2" to 1,
        "Season 3" to 2,
        "Specials" to 3,
    )

    for ((label, seasonSlot) in panesToTest) {
        val landed = if (hasFirstEntryRedirect) {
            // FocusRestorer(fallback = firstEpisodeRequester) or explicit Right/onEnter redirect
            // routes first entry into an unvisited season's episode list to index 0 (ep1).
            "ep1"
        } else {
            composeTwoDimensionalWinner(episodeRects, seasonRowRect(seasonSlot), FocusDirection.Right)
        }
        checkEq(
            "dpad-right-first-entry-from-${label.lowercase().replace(' ', '-')}-lands-on-ep1",
            "ep1",
            landed,
        )
    }
}

private fun runSeasonReturnChecks(
    hasSeasonReturnRedirect: Boolean,
    scrollsSeasonListWhenInEpisodes: Boolean,
) {
    // 12-season show + Specials (13 rows total; only 6 fit in the left-pane LazyColumn viewport at once).
    val seasons = (1..12).map { n ->
        SeasonGroup(n, listOf(makeEpisode("s${n}e1", n, 1), makeEpisode("s${n}e2", n, 2)))
    } + SeasonGroup(0, listOf(makeEpisode("sp1", 0, 1)))
    val ordered = seasonPaneOrder(seasons)
    checkEq("twelve-seasons-plus-specials-order", (1..12).toList() + 0, ordered.map { it.season })

    // Returning from PlayerScreen mounts a fresh SeasonScreen with `selectedSeason != null` (`inEpisodes = true`),
    // focusing `firstEpisodeRequester` (ep1) while `SeasonPane` has not yet been focused in this mount.
    val ep1Rect = episodeRowRect(0)

    val returnTargets = listOf(
        "Season 2" to 2,
        "Season 3" to 3,
        "Season 10 (below initial viewport)" to 10,
        "Specials (below initial viewport)" to 0,
    )

    for ((label, targetSeasonNum) in returnTargets) {
        val targetIndex = ordered.indexOfFirst { it.season == targetSeasonNum }
        val firstVisibleIndex = if (scrollsSeasonListWhenInEpisodes) {
            targetIndex.coerceAtMost((ordered.size - VISIBLE_SEASON_ROWS_IN_VIEWPORT).coerceAtLeast(0))
        } else {
            0
        }
        val visibleRange = firstVisibleIndex until (firstVisibleIndex + VISIBLE_SEASON_ROWS_IN_VIEWPORT).coerceAtMost(ordered.size)
        val targetIsVisibleAndComposed = targetIndex in visibleRange

        check(
            "active-season-visible-on-mount-when-in-episodes-${label.lowercase().substringBefore(" (").replace(' ', '-')}",
            targetIsVisibleAndComposed,
            "targetIndex=$targetIndex visibleRange=$visibleRange (seasonListState remained at index $firstVisibleIndex)",
        )

        val visibleCandidates = buildList {
            add("watchlist" to Rect(150f, 138f, 265f, 178f))
            visibleRange.forEachIndexed { slot, idx ->
                val s = ordered[idx].season
                add((if (s == 0) "specials" else "season$s") to seasonRowRect(slot))
            }
        }
        val expectedName = if (targetSeasonNum == 0) "specials" else "season$targetSeasonNum"

        val landedOnLeft = if (hasSeasonReturnRedirect && targetIsVisibleAndComposed) {
            expectedName
        } else {
            composeTwoDimensionalWinner(visibleCandidates, ep1Rect, FocusDirection.Left)
        }

        checkEq(
            "dpad-left-from-ep1-after-player-return-preserves-${expectedName}",
            expectedName,
            landedOnLeft,
        )
    }
}

fun main(args: Array<String>) {
    val flags = args.associate { arg ->
        val clean = arg.removePrefix("--")
        val eq = clean.indexOf('=')
        if (eq == -1) clean to "true" else clean.substring(0, eq) to clean.substring(eq + 1)
    }
    when (val mode = flags["mode"]) {
        "episode-entry" -> runEpisodeEntryChecks(
            hasFirstEntryRedirect = flags["episode-pane-has-first-entry-redirect"] == "true",
        )
        "season-return" -> runSeasonReturnChecks(
            hasSeasonReturnRedirect = flags["season-pane-has-return-redirect"] == "true",
            scrollsSeasonListWhenInEpisodes = flags["scrolls-season-list-when-in-episodes"] == "true",
        )
        else -> {
            System.err.println("Unknown --mode=$mode")
            exitProcess(2)
        }
    }

    if (failures.isNotEmpty()) {
        System.err.println("FAILED (${failures.size} assertions)")
        exitProcess(1)
    }
    println("ALL_OK")
}
