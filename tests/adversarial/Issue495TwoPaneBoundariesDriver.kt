package app.swarm.tv.app.ui.screens

import androidx.compose.ui.focus.FocusDirection
import androidx.compose.ui.geometry.Rect
import app.swarm.tv.core.catalog.AlbumGroup
import app.swarm.tv.core.catalog.ArtistGroup
import app.swarm.tv.core.catalog.CatalogGrouping
import app.swarm.tv.core.catalog.MergedEntry
import app.swarm.tv.core.catalog.SeasonGroup
import app.swarm.tv.core.catalog.ShowGroup
import app.swarm.tv.core.catalog.UNKNOWN_ALBUM
import app.swarm.tv.core.peer.CatalogEntry
import app.swarm.tv.core.peer.MediaKind
import kotlin.system.exitProcess

/**
 * Executable JVM driver for issue #495 and its trusted amendments:
 * - "For the TV apps i want the episode and shows to view differently. I want it
 *    to view and navigate like Netflix does. See attachment. Seasons and specials
 *    list on left and episodes show on right."
 * - "Good work.  Now I want to now update music UI to look and feel the same way."
 * - "I also want to update the shows UI so that as the user scrolls down on the
 *    episodes, and the end of the season is in focus, the user can keep scrolling
 *    down which moves to next season.  Keep the seasons on the left and ability
 *    to select a season but scrolling should navigate through all seasons not
 *    just the selected season."
 *
 * Compiled against the real `:core` catalog/grouping models, real helpers/types
 * extracted from `SeasonScreen.kt` and `AlbumScreen.kt`, and Jetpack Compose UI's
 * production `TwoDimensionalFocusSearchKt` (`isCandidate` + `isBetterCandidate`).
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
    showTitle: String = "Severance",
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
        durationSecs = 2700.0,
        showTitle = showTitle,
        season = season,
        episode = number,
        extraType = extraType,
        extraTitle = extraTitle,
    ),
)

private fun makeTrack(
    key: String,
    artist: String = "Air",
    album: String? = "Moon Safari",
    trackNumber: Int? = 1,
): MergedEntry = MergedEntry(
    fingerprint = key,
    sources = listOf("server-a"),
    entry = CatalogEntry(
        entryKey = key,
        fingerprint = key,
        kind = MediaKind.TRACK,
        title = "Track $key",
        size = 1L,
        durationSecs = 215.0,
        artist = artist,
        album = album,
        trackNumber = trackNumber,
    ),
)

/**
 * Runs Compose UI's production `TwoDimensionalFocusSearchKt` eligibility and
 * candidate-ranking functions (`isBetterCandidate_I7lrPNg$isCandidate` and
 * `isBetterCandidate-...`) over candidate focus rectangles outside a pane when
 * that pane does not cancel directional exit.
 */
private fun composeTwoDimensionalEscapeWinner(
    candidates: List<Pair<String, Rect>>,
    source: Rect,
    direction: FocusDirection,
): String? {
    val searchClass = Class.forName("androidx.compose.ui.focus.TwoDimensionalFocusSearchKt")
    val isCandidate = searchClass.declaredMethods.first {
        it.name.endsWith("\$isCandidate") && it.parameterCount == 3
    }.apply { isAccessible = true }
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
        val eligible = isCandidate.invoke(null, rect, dirInt, source) as Boolean
        if (!eligible) continue
        if (bestRect == null || isBetter.invoke(null, rect, bestRect, source, dirInt) as Boolean) {
            bestRect = rect
            bestName = name
        }
    }
    return bestName
}

// Geometry derived from AlbumScreen.kt:
// Outer Row: padding(horizontal = 40.dp, vertical = 28.dp), spacedBy(28.dp).
// Left Column (width = 300.dp -> x = 40..340):
//   72.dp square cover + 18.dp Spacer + 4.dp LazyColumn top contentPadding -> first Album row at y = 122.dp,
//   ~44.dp tall, spacedBy(6.dp) -> stride = 50.dp.
// Right TrackList Column (x = 368..1880):
//   ~27.dp album header + 12.dp Spacer + 4.dp LazyColumn top contentPadding -> first TrackRow at y = 71.dp,
//   ~48.dp tall, spacedBy(8.dp) -> stride = 56.dp.
private fun albumRowRect(slot: Int): Rect {
    val top = 122f + slot * 50f
    return Rect(40f, top, 340f, top + 44f)
}

private fun trackRowRect(slot: Int): Rect {
    val top = 71f + slot * 56f
    return Rect(368f, top, 1880f, top + 48f)
}

// Geometry derived from SeasonScreen.kt:
// Outer Row: padding(horizontal = 40.dp, vertical = 28.dp), spacedBy(28.dp).
// Left SeasonPane (width = 300.dp -> x = 40..340):
//   64.dp * 3/2 = 96.dp poster (y = 28..124) + 14.dp Spacer + ~40.dp Watchlist/Resume button row (y = 138..178)
//   + 18.dp Spacer + 4.dp LazyColumn top contentPadding -> first Season row at y = 200.dp, ~44.dp tall, stride = 50.dp.
// Right EpisodePane (x = 368..1880):
//   4.dp top contentPadding + ~27.dp season header + 10.dp spacing -> first EpisodeRow at y = 69.dp,
//   208.dp * 9/16 = 117.dp + 16.dp padding = 133.dp tall, spacedBy(10.dp) -> stride = 143.dp.
private fun seasonRowRect(slot: Int): Rect {
    val top = 200f + slot * 50f
    return Rect(40f, top, 340f, top + 44f)
}

private fun episodeRowRect(slot: Int): Rect {
    val top = 69f + slot * 143f
    return Rect(368f, top, 1880f, top + 133f)
}

private fun runMusicTwoPaneChecks(
    leftPaneBlocksVerticalExit: Boolean,
    rightPaneBlocksVerticalExit: Boolean,
    rightPaneContinuousAcrossAlbums: Boolean,
    trackFocusSyncsInTracks: Boolean,
    leftReturnsToAlbumExplicitly: Boolean,
) {
    val grouped = CatalogGrouping.groupTracksByArtistAlbum(
        listOf(
            makeTrack("m2", "Air", "Moon Safari", 2),
            makeTrack("m1", "Air", "Moon Safari", 1),
            makeTrack("t1", "Air", "Talkie Walkie", 1),
            makeTrack("u1", "Air", null, 1),
        ),
    )
    checkEq("artist-grouped-count", 1, grouped.size)
    val artist: ArtistGroup = grouped.single()
    checkEq(
        "artist-album-order-real-before-unknown",
        listOf("Moon Safari", "Talkie Walkie", UNKNOWN_ALBUM),
        artist.albums.map { it.album },
    )
    checkEq("album-for-key-null-is-null", null, albumForKey(artist, null))
    checkEq("album-for-key-known", "Talkie Walkie", albumForKey(artist, "Talkie Walkie")?.album)
    checkEq("album-for-key-unknown-bucket", UNKNOWN_ALBUM, albumForKey(artist, UNKNOWN_ALBUM)?.album)
    checkEq("album-for-key-missing-is-null", null, albumForKey(artist, "Nonexistent"))
    checkEq("track-count-singular", "1 track", trackCount(1))
    checkEq("track-count-plural", "2 tracks", trackCount(2))
    checkEq("track-count-zero", "0 tracks", trackCount(0))

    val albums3 = (0 until 3).map { i -> "album${i + 1}" to albumRowRect(i) }
    val albums1 = albums3.take(1)
    val tracks5 = (0 until 5).map { j -> "track${j + 1}" to trackRowRect(j) }
    val tracks1 = tracks5.take(1)
    val tracks2 = tracks5.take(2)

    // 1. Left album pane top boundary: D-pad Up on Album 1 must remain on Album 1
    // instead of escaping upward onto Track 1 (at y = 71..119dp < Album 1 top 122dp).
    val afterUpOnFirstAlbum = if (leftPaneBlocksVerticalExit) {
        "album1"
    } else {
        composeTwoDimensionalEscapeWinner(albums3.drop(1) + tracks5, albums3[0].second, FocusDirection.Up) ?: "album1"
    }
    checkEq("music-dpad-up-at-first-album-stays-on-album1", "album1", afterUpOnFirstAlbum)

    // 2. Left album pane bottom boundary: D-pad Down on the last album must remain on
    // that album instead of escaping diagonally into mid-album tracks on the right.
    val afterDownOnSingleAlbumArtist = if (leftPaneBlocksVerticalExit) {
        "album1"
    } else {
        composeTwoDimensionalEscapeWinner(tracks5, albums1[0].second, FocusDirection.Down) ?: "album1"
    }
    checkEq("music-dpad-down-at-last-album-single-album-artist-stays-on-album1", "album1", afterDownOnSingleAlbumArtist)

    val afterDownOnThreeAlbumArtist = if (leftPaneBlocksVerticalExit) {
        "album3"
    } else {
        composeTwoDimensionalEscapeWinner(albums3.take(2) + tracks5, albums3[2].second, FocusDirection.Down) ?: "album3"
    }
    checkEq("music-dpad-down-at-last-album-three-album-artist-stays-on-album3", "album3", afterDownOnThreeAlbumArtist)

    // 3. Right track pane bottom boundary: D-pad Down on the last track of a 1-track or
    // 2-track album must NEVER jump into the left album pane (which pulls focus out of
    // tracks and overwrites the displayed album).
    val afterDownOnOneTrackAlbum = when {
        rightPaneContinuousAcrossAlbums -> "next-album-track1"
        rightPaneBlocksVerticalExit -> "track1"
        else -> composeTwoDimensionalEscapeWinner(albums3, tracks1[0].second, FocusDirection.Down) ?: "track1"
    }
    check(
        "music-dpad-down-at-end-of-one-track-album-never-escapes-to-album-pane",
        !afterDownOnOneTrackAlbum.startsWith("album"),
        "focus escaped from track1 into left pane `$afterDownOnOneTrackAlbum`",
    )

    val afterDownOnTwoTrackAlbum = when {
        rightPaneContinuousAcrossAlbums -> "next-album-track1"
        rightPaneBlocksVerticalExit -> "track2"
        else -> composeTwoDimensionalEscapeWinner(albums3 + tracks2.take(1), tracks2[1].second, FocusDirection.Down) ?: "track2"
    }
    check(
        "music-dpad-down-at-end-of-two-track-album-never-escapes-to-album-pane",
        !afterDownOnTwoTrackAlbum.startsWith("album"),
        "focus escaped from track2 into left pane `$afterDownOnTwoTrackAlbum` (switching displayed album)",
    )

    // 4. Track-pane focus state sync: if a TrackRow gains focus directly (e.g. via
    // RequestFocus semantics or any focus transfer that did not go through `onOpen`),
    // `inTracks` must synchronize so D-pad Left and Back return to the active album
    // rather than swallowing Left as a no-op and exiting the entire artist screen on Back.
    var inTracks = false
    var albumPaneFocused = true
    var focusedNode = "album1"
    var onBackInvoked = false

    // Focus moves onto track1 in TrackList without `onOpen` having run first.
    focusedNode = "track1"
    albumPaneFocused = false
    if (trackFocusSyncsInTracks) {
        inTracks = true
    }

    // User presses D-pad Left on track1: `.onPaneDirection(Key.DirectionLeft)` consumes the key event.
    val previousInTracks = inTracks
    inTracks = false
    if ((previousInTracks != inTracks && !inTracks && !albumPaneFocused) || leftReturnsToAlbumExplicitly) {
        focusedNode = "album1"
        albumPaneFocused = true
    }
    checkEq(
        "music-dpad-left-from-directly-focused-track-returns-to-active-album",
        "album1",
        focusedNode,
    )

    // Re-focus track1 directly and press Back: first Back must return to album1, NOT exit artist screen.
    inTracks = false
    albumPaneFocused = false
    focusedNode = "track1"
    if (trackFocusSyncsInTracks) {
        inTracks = true
    }
    if (inTracks) {
        inTracks = false
        if (!albumPaneFocused) {
            focusedNode = "album1"
            albumPaneFocused = true
        }
    } else {
        onBackInvoked = true
    }
    check(
        "music-back-from-directly-focused-track-returns-to-album-before-exiting-artist",
        !onBackInvoked && focusedNode == "album1",
        "onBackInvoked=$onBackInvoked focusedNode=$focusedNode",
    )
}

private fun runShowsContinuousAndBoundsChecks(
    episodePaneIsContinuous: Boolean,
    episodeFocusSyncsSeasonWithoutRefocus: Boolean,
    seasonPaneBlocksVerticalExit: Boolean,
) {
    check(
        "episode-pane-uses-single-continuous-list-across-seasons",
        episodePaneIsContinuous,
        "EpisodePane still isolates seasons instead of rendering all sections in one continuous list",
    )
    check(
        "episode-focus-updates-highlighted-and-selected-season-without-yanking-focus",
        episodeFocusSyncsSeasonWithoutRefocus,
        "Focusing an episode across a season boundary does not cleanly update highlighted/selected season",
    )

    val s1e1 = makeEpisode("s1e1", 1, 1)
    val s1e2 = makeEpisode("s1e2", 1, 2)
    val s1x1 = makeEpisode("s1x1", 1, null, extraType = "featurette", extraTitle = "Behind S1")
    val s2e1 = makeEpisode("s2e1", 2, 1)
    val s2e2 = makeEpisode("s2e2", 2, 2)
    val spx1 = makeEpisode("spx1", 0, null, extraType = "deletedScene", extraTitle = "Cut Special")
    val un1 = makeEpisode("un1", null, null)

    val show = ShowGroup(
        show = "Severance",
        seasons = listOf(
            SeasonGroup(null, listOf(un1)),
            SeasonGroup(0, listOf(spx1)),
            SeasonGroup(2, listOf(s2e1, s2e2)),
            SeasonGroup(1, listOf(s1e1, s1e2, s1x1)),
        ),
    )
    val ordered = seasonPaneOrder(show.seasons)
    checkEq("shows-season-order-real-specials-unnumbered", listOf(1, 2, 0, null), ordered.map { it.season })

    var nextHeader = 0
    val sections = ordered.map { season ->
        EpisodeSection(season, nextHeader).also { nextHeader += it.rowCount }
    }
    // Section 0 (Season 1): header(0), s1e1(1), s1e2(2), extras-header(3), s1x1(4) -> rowCount = 5
    // Section 1 (Season 2): header(5), s2e1(6), s2e2(7) -> rowCount = 3
    // Section 2 (Specials, extras-only): header(8), extras-header(9), spx1(10) -> rowCount = 3
    // Section 3 (Unnumbered): header(11), un1(12) -> rowCount = 2
    checkEq("section-header-indices-contiguous", listOf(0, 5, 8, 11), sections.map { it.headerIndex })
    checkEq("section-row-counts", listOf(5, 3, 3, 2), sections.map { it.rowCount })
    checkEq("section0-default-focus-first-ep", 1, sections[0].focusIndex(null))
    checkEq("section0-focus-second-ep", 2, sections[0].focusIndex("s1e2"))
    checkEq("section0-focus-extra", 4, sections[0].focusIndex("s1x1"))
    checkEq("section1-default-focus-after-crossing-boundary", 6, sections[1].focusIndex(null))
    checkEq("section1-focus-specific-ep", 7, sections[1].focusIndex("s2e2"))
    checkEq("section2-extras-only-focus-skips-both-headers", 10, sections[2].focusIndex(null))
    checkEq("section3-unnumbered-focus", 12, sections[3].focusIndex(null))

    // SeasonPane vertical D-pad boundaries:
    // Pressing Down on the bottom season/specials row in SeasonPane must stay on that season row
    // instead of escaping diagonally into mid-list episodes (ep2/ep3/ep4) in EpisodePane.
    val watchlist = "watchlist" to Rect(40f, 138f, 155f, 178f)
    val seasons4 = listOf(
        "season1" to seasonRowRect(0),
        "season2" to seasonRowRect(1),
        "season3" to seasonRowRect(2),
        "specials" to seasonRowRect(3),
    )
    val episodes4 = (0 until 4).map { j -> "ep${j + 1}" to episodeRowRect(j) }

    val downOnOneSeasonShow = if (seasonPaneBlocksVerticalExit) {
        "season1"
    } else {
        composeTwoDimensionalEscapeWinner(listOf(watchlist) + episodes4, seasons4[0].second, FocusDirection.Down) ?: "season1"
    }
    checkEq("shows-dpad-down-at-bottom-of-1-season-list-stays-on-season1", "season1", downOnOneSeasonShow)

    val downOnTwoSeasonShow = if (seasonPaneBlocksVerticalExit) {
        "season2"
    } else {
        composeTwoDimensionalEscapeWinner(
            listOf(watchlist, seasons4[0]) + episodes4,
            seasons4[1].second,
            FocusDirection.Down,
        ) ?: "season2"
    }
    checkEq("shows-dpad-down-at-bottom-of-2-season-list-stays-on-season2", "season2", downOnTwoSeasonShow)

    val downOnSpecialsRow = if (seasonPaneBlocksVerticalExit) {
        "specials"
    } else {
        composeTwoDimensionalEscapeWinner(
            listOf(watchlist) + seasons4.take(3) + episodes4,
            seasons4[3].second,
            FocusDirection.Down,
        ) ?: "specials"
    }
    checkEq("shows-dpad-down-at-specials-end-of-season-list-stays-on-specials", "specials", downOnSpecialsRow)

    // When EpisodePane is clamped near the end of the continuous list (e.g. previewing a 1-episode
    // final season/Specials so the preceding season's episode sits partially clipped at the top of
    // EpisodePane at y = 28..130dp, above Watchlist's y = 138..178dp), pressing Up on Watchlist must
    // stay on Watchlist rather than escaping upward into EpisodePane.
    val clampedTopEpisode = "ep-prev-season" to Rect(368f, 28f, 1880f, 130f)
    val upOnWatchlist = if (seasonPaneBlocksVerticalExit) {
        "watchlist"
    } else {
        composeTwoDimensionalEscapeWinner(
            seasons4 + listOf(clampedTopEpisode) + episodes4.drop(1),
            watchlist.second,
            FocusDirection.Up,
        ) ?: "watchlist"
    }
    checkEq("shows-dpad-up-at-top-watchlist-action-stays-in-season-pane", "watchlist", upOnWatchlist)
}

fun main(args: Array<String>) {
    val flags = args.associate { arg ->
        val clean = arg.removePrefix("--")
        val eq = clean.indexOf('=')
        if (eq == -1) clean to "true" else clean.substring(0, eq) to clean.substring(eq + 1)
    }
    when (val mode = flags["mode"]) {
        "music-two-pane" -> runMusicTwoPaneChecks(
            leftPaneBlocksVerticalExit = flags["left-pane-blocks-vertical-exit"] == "true",
            rightPaneBlocksVerticalExit = flags["right-pane-blocks-vertical-exit"] == "true",
            rightPaneContinuousAcrossAlbums = flags["right-pane-continuous-across-albums"] == "true",
            trackFocusSyncsInTracks = flags["track-focus-syncs-in-tracks"] == "true",
            leftReturnsToAlbumExplicitly = flags["left-returns-to-album-explicitly"] == "true",
        )
        "shows-continuous-and-bounds" -> runShowsContinuousAndBoundsChecks(
            episodePaneIsContinuous = flags["episode-pane-is-continuous"] == "true",
            episodeFocusSyncsSeasonWithoutRefocus = flags["episode-focus-syncs-season-without-refocus"] == "true",
            seasonPaneBlocksVerticalExit = flags["season-pane-blocks-vertical-exit"] == "true",
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
