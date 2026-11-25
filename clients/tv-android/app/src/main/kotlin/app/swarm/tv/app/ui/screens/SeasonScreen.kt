/**
 * One show, laid out as a two-pane season/episode browser (#495): the show
 * header, its actions and a list of seasons (then Specials/extras) on the
 * left; the highlighted season's episodes as a vertical list of
 * thumbnail + title + synopsis rows on the right.
 *
 * Navigation is D-pad only and never leaves this screen:
 *  - Moving focus over a season row swaps the right pane to that season's
 *    episodes immediately — no click needed just to look.
 *  - Selecting a season (Center) or pressing Right moves focus into its
 *    episodes; Left returns to the season list on the season it came from.
 *  - Back from the episode pane returns focus to the season list; Back
 *    from the season list leaves the screen.
 *  - Selecting an episode calls [onPlayEpisode] directly — no detail screen
 *    in between.
 *
 * `selectedSeason` (persisted in `UiState.ShowSeasons`) is non-null exactly
 * while focus is in the episode pane, so Back and returning from the player
 * land the user where they were.
 */
package app.swarm.tv.app.ui.screens

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListState
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.runtime.withFrameNanos
import androidx.compose.ui.Alignment
import androidx.compose.ui.ExperimentalComposeUiApi
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.focus.focusRestorer
import androidx.compose.ui.focus.onFocusChanged
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.tv.material3.Border
import androidx.tv.material3.Button
import androidx.tv.material3.Card
import androidx.tv.material3.CardDefaults
import app.swarm.tv.app.ui.UatTestTags
import app.swarm.tv.app.ui.components.swarmActionButtonColors
import app.swarm.tv.app.ui.theme.SwarmAccent
import app.swarm.tv.app.ui.theme.SwarmMuted
import app.swarm.tv.app.ui.theme.SwarmSurface
import app.swarm.tv.app.ui.theme.SwarmSurfaceMuted
import app.swarm.tv.app.ui.theme.SwarmText
import app.swarm.tv.core.catalog.CatalogGrouping
import app.swarm.tv.core.catalog.MergedEntry
import app.swarm.tv.core.catalog.SeasonGroup
import app.swarm.tv.core.catalog.ShowGroup
import app.swarm.tv.core.catalog.displayTitle
import app.swarm.tv.core.watch.WatchState
import app.swarm.tv.core.watch.stateFor

/** Frames to wait for a lazy row to compose before giving up on a focus request. */
private const val SEASON_SCREEN_FOCUS_ATTEMPTS = 10

@Composable
fun SeasonScreen(
    show: ShowGroup,
    seasonArtworkUrl: (MergedEntry) -> String?,
    episodeArtworkUrl: (MergedEntry) -> String?,
    onPlayEpisode: (MergedEntry) -> Unit,
    onBack: () -> Unit,
    selectedSeason: SeasonGroup?,
    onSelectSeason: (SeasonGroup?) -> Unit,
    isWatchlisted: Boolean,
    onToggleWatchlist: () -> Unit,
    /** Non-null when this show has an episode that was started but not
     * finished — see the season pane's Resume button (#152). Resumes that
     * episode from its saved position, Continue-Watching style. */
    onResume: (() -> Unit)? = null,
    /** Saved per-episode progress: drives each episode thumbnail's progress
     * bar and which season the screen opens on. */
    watchStates: Map<String, WatchState> = emptyMap(),
) {
    val seasons = remember(show) { seasonPaneOrder(show.seasons) }
    // The season whose episodes the right pane shows. Follows season-list
    // focus; seeded from the persisted selection, else the season holding
    // the episode in progress, else the first real season.
    var highlightedKey by remember(show.show) {
        mutableStateOf(
            selectedSeason?.let(::seasonKey)
                ?: defaultSeason(show, watchStates)?.let(::seasonKey),
        )
    }
    val displayed = seasons.firstOrNull { seasonKey(it) == highlightedKey } ?: seasons.firstOrNull()
    val inEpisodes = selectedSeason != null && displayed != null

    var seasonPaneFocused by remember { mutableStateOf(false) }
    var episodePaneFocused by remember { mutableStateOf(false) }
    val highlightedSeasonRequester = remember { FocusRequester() }
    val firstEpisodeRequester = remember { FocusRequester() }
    // Open the season list already scrolled to the active season — also when
    // returning from the player straight into the episode pane — so that row
    // is composed and on screen for D-pad Left to land on.
    val seasonListState = rememberLazyListState(
        initialFirstVisibleItemIndex = displayed?.let { d -> seasons.indexOfFirst { seasonKey(it) == seasonKey(d) } }
            ?.coerceAtLeast(0) ?: 0,
    )

    BackHandler { if (selectedSeason != null) onSelectSeason(null) else onBack() }

    // Place focus whenever the pane that should own it changes — first
    // entry, Back from the episode pane, or returning from the player. A
    // pane that already holds focus (the user moved there with the D-pad)
    // is left alone so the selection never yanks focus off a row.
    LaunchedEffect(inEpisodes, show.show) {
        if (seasons.isEmpty()) return@LaunchedEffect
        // Keep the active season row visible in both panes' states so Left
        // from the episodes (focusRestorer fallback) always has it to land on.
        val index = seasons.indexOfFirst { seasonKey(it) == seasonKey(displayed ?: seasons.first()) }
        val rowVisible = seasonListState.layoutInfo.visibleItemsInfo.any { it.index == index }
        if (index >= 0 && !seasonPaneFocused && !rowVisible) seasonListState.scrollToItem(index)
        repeat(SEASON_SCREEN_FOCUS_ATTEMPTS) {
            withFrameNanos {}
            if (inEpisodes && episodePaneFocused) return@LaunchedEffect
            if (!inEpisodes && seasonPaneFocused) return@LaunchedEffect
            val target = if (inEpisodes) firstEpisodeRequester else highlightedSeasonRequester
            if (runCatching { target.requestFocus() }.isSuccess) return@LaunchedEffect
        }
    }

    Row(
        modifier = Modifier.fillMaxSize().padding(horizontal = 40.dp, vertical = 28.dp),
        horizontalArrangement = Arrangement.spacedBy(28.dp),
    ) {
        SeasonPane(
            show = show,
            seasons = seasons,
            displayed = displayed,
            seasonArtworkUrl = seasonArtworkUrl,
            isWatchlisted = isWatchlisted,
            onToggleWatchlist = onToggleWatchlist,
            onResume = onResume,
            listState = seasonListState,
            highlightedSeasonRequester = highlightedSeasonRequester,
            onHighlightSeason = { highlightedKey = seasonKey(it) },
            onOpenSeason = { season ->
                highlightedKey = seasonKey(season)
                onSelectSeason(season)
            },
            modifier = Modifier.width(300.dp).fillMaxHeight().onFocusChanged { state ->
                seasonPaneFocused = state.hasFocus
                if (state.hasFocus && selectedSeason != null) onSelectSeason(null)
            },
        )
        if (displayed != null) {
            EpisodePane(
                season = displayed,
                watchStates = watchStates,
                episodeArtworkUrl = episodeArtworkUrl,
                onPlayEpisode = onPlayEpisode,
                firstEpisodeRequester = firstEpisodeRequester,
                modifier = Modifier.weight(1f).fillMaxHeight().onFocusChanged { state ->
                    episodePaneFocused = state.hasFocus
                    if (state.hasFocus && selectedSeason?.let(::seasonKey) != seasonKey(displayed)) {
                        onSelectSeason(displayed)
                    }
                },
            )
        }
    }
}

@OptIn(ExperimentalComposeUiApi::class)
@Composable
private fun SeasonPane(
    show: ShowGroup,
    seasons: List<SeasonGroup>,
    displayed: SeasonGroup?,
    seasonArtworkUrl: (MergedEntry) -> String?,
    isWatchlisted: Boolean,
    onToggleWatchlist: () -> Unit,
    onResume: (() -> Unit)?,
    listState: LazyListState,
    highlightedSeasonRequester: FocusRequester,
    onHighlightSeason: (SeasonGroup) -> Unit,
    onOpenSeason: (SeasonGroup) -> Unit,
    modifier: Modifier = Modifier,
) {
    Column(modifier = modifier) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            val posterSource = displayed?.let { season ->
                (CatalogGrouping.seasonEpisodes(season).firstOrNull() ?: season.episodes.firstOrNull())
            }
            ArtworkImage(
                label = show.show,
                placeholderType = "Show",
                primaryUrl = posterSource?.let(seasonArtworkUrl),
                modifier = Modifier.width(64.dp).aspectRatio(2f / 3f).clip(RoundedCornerShape(4.dp)),
            )
            Spacer(Modifier.width(14.dp))
            Column {
                Text(
                    show.show,
                    color = SwarmText,
                    fontSize = 22.sp,
                    fontWeight = FontWeight.Black,
                    maxLines = 2,
                    overflow = TextOverflow.Ellipsis,
                    modifier = Modifier.testTag(UatTestTags.SEASON_SCREEN_SHOW_TITLE),
                )
                Spacer(Modifier.height(4.dp))
                Text(showMetaLine(show), color = SwarmMuted, fontSize = 12.sp, maxLines = 1)
            }
        }
        Spacer(Modifier.height(14.dp))
        Row(horizontalArrangement = Arrangement.spacedBy(10.dp)) {
            if (onResume != null) {
                Button(
                    onClick = onResume,
                    colors = swarmActionButtonColors(),
                    modifier = Modifier.testTag(UatTestTags.SEASON_SCREEN_RESUME_BUTTON),
                ) {
                    Text("▶ Resume", fontSize = 13.sp)
                }
            }
            Button(
                onClick = onToggleWatchlist,
                colors = swarmActionButtonColors(),
                modifier = Modifier.testTag(UatTestTags.SEASON_SCREEN_WATCHLIST_BUTTON),
            ) {
                Text(if (isWatchlisted) "✓ Watchlisted" else "+ Watchlist", fontSize = 13.sp)
            }
        }
        Spacer(Modifier.height(18.dp))
        LazyColumn(
            state = listState,
            verticalArrangement = Arrangement.spacedBy(6.dp),
            // Left back into the list lands on the season it was left from —
            // or, on a fresh mount (e.g. back from the player), the season
            // whose episodes are showing — never whichever row happens to sit
            // level with the episode.
            modifier = Modifier.fillMaxWidth().weight(1f)
                .focusRestorer(if (displayed != null) highlightedSeasonRequester else FocusRequester.Default),
            contentPadding = PaddingValues(vertical = 4.dp),
        ) {
            itemsIndexed(
                items = seasons,
                key = { _, season -> season.season ?: -1 },
                contentType = { _, _ -> "season" },
            ) { _, season ->
                val isDisplayed = displayed != null && seasonKey(season) == seasonKey(displayed)
                SeasonRow(
                    season = season,
                    isDisplayed = isDisplayed,
                    onFocused = { onHighlightSeason(season) },
                    onClick = { onOpenSeason(season) },
                    modifier = if (isDisplayed) Modifier.focusRequester(highlightedSeasonRequester) else Modifier,
                )
            }
        }
    }
}

@Composable
private fun SeasonRow(
    season: SeasonGroup,
    isDisplayed: Boolean,
    onFocused: () -> Unit,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val realEpisodes = CatalogGrouping.seasonEpisodes(season)
    val extras = CatalogGrouping.seasonExtras(season)
    Card(
        onClick = onClick,
        colors = CardDefaults.colors(
            containerColor = if (isDisplayed) SwarmSurfaceMuted else Color.Transparent,
            contentColor = SwarmText,
            focusedContainerColor = SwarmSurfaceMuted,
            focusedContentColor = SwarmText,
            pressedContainerColor = SwarmSurfaceMuted,
            pressedContentColor = SwarmText,
        ),
        border = CardDefaults.border(
            border = if (isDisplayed) Border(BorderStroke(1.dp, SwarmMuted)) else Border.None,
            focusedBorder = Border(BorderStroke(2.dp, SwarmAccent)),
            pressedBorder = Border(BorderStroke(2.dp, SwarmAccent)),
        ),
        scale = CardDefaults.scale(scale = 1f, focusedScale = 1f, pressedScale = 0.99f),
        modifier = modifier.fillMaxWidth()
            .onFocusChanged { if (it.isFocused) onFocused() }
            .testTag(UatTestTags.SEASON_CARD_PREFIX + (season.season ?: -1)),
    ) {
        Row(
            modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 12.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text(
                seasonLabel(season.season),
                color = SwarmText,
                fontSize = 16.sp,
                fontWeight = if (isDisplayed) FontWeight.Bold else FontWeight.Medium,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.weight(1f),
            )
            Text(seasonCountLabel(realEpisodes.size, extras.size), color = SwarmMuted, fontSize = 12.sp, maxLines = 1)
        }
    }
}

@OptIn(ExperimentalComposeUiApi::class)
@Composable
private fun EpisodePane(
    season: SeasonGroup,
    watchStates: Map<String, WatchState>,
    episodeArtworkUrl: (MergedEntry) -> String?,
    onPlayEpisode: (MergedEntry) -> Unit,
    firstEpisodeRequester: FocusRequester,
    modifier: Modifier = Modifier,
) {
    val episodes = CatalogGrouping.seasonEpisodes(season)
    val extras = CatalogGrouping.seasonExtras(season)
    Column(modifier = modifier) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(seasonLabel(season.season), color = SwarmText, fontSize = 20.sp, fontWeight = FontWeight.Bold)
            Spacer(Modifier.width(10.dp))
            Text(seasonCountLabel(episodes.size, extras.size), color = SwarmMuted, fontSize = 13.sp)
        }
        Spacer(Modifier.height(12.dp))
        // A fresh list per season: switching seasons always starts at the
        // top instead of inheriting the previous season's scroll offset.
        // An empty season has nothing to focus, so no list (and no fallback
        // requester left unattached) is composed for it.
        if (episodes.isNotEmpty() || extras.isNotEmpty()) key(seasonKey(season)) {
            LazyColumn(
                state = rememberLazyListState(),
                verticalArrangement = Arrangement.spacedBy(10.dp),
                // Re-entering from the season list returns to the episode
                // the user left from; first entry into a season always lands
                // on its first episode, not whichever row sits level with the
                // highlighted season.
                modifier = Modifier.fillMaxWidth().weight(1f).focusRestorer(firstEpisodeRequester),
                contentPadding = PaddingValues(vertical = 4.dp),
            ) {
                itemsIndexed(
                    items = episodes,
                    key = { _, episode -> episode.entry.entryKey },
                    contentType = { _, _ -> "episode" },
                ) { index, episode ->
                    EpisodeRow(
                        entry = episode,
                        label = episodeLabel(episode),
                        title = episode.entry.displayTitle(),
                        progress = watchStates.stateFor(episode.entry)?.progressFraction(),
                        episodeArtworkUrl = episodeArtworkUrl,
                        onPlayEpisode = onPlayEpisode,
                        modifier = if (index == 0) Modifier.focusRequester(firstEpisodeRequester) else Modifier,
                    )
                }
                // Bonus content (featurettes/deleted scenes/etc.) stays
                // labeled and grouped exactly like a movie's extras row on
                // MovieDetailScreen — never a plain, undifferentiated episode.
                if (extras.isNotEmpty()) {
                    item(key = "extras-header", contentType = "header") {
                        Text(
                            "Extras",
                            color = SwarmText,
                            fontSize = 17.sp,
                            fontWeight = FontWeight.Bold,
                            modifier = Modifier.padding(top = 8.dp),
                        )
                    }
                    itemsIndexed(
                        items = extras,
                        key = { _, extra -> extra.entry.entryKey },
                        contentType = { _, _ -> "extra" },
                    ) { index, extra ->
                        EpisodeRow(
                            entry = extra,
                            label = CatalogGrouping.extraTypeLabel(extra.entry.extraType),
                            title = extra.entry.extraTitle ?: extra.entry.title,
                            progress = watchStates.stateFor(extra.entry)?.progressFraction(),
                            episodeArtworkUrl = episodeArtworkUrl,
                            onPlayEpisode = onPlayEpisode,
                            modifier = if (index == 0 && episodes.isEmpty()) {
                                Modifier.focusRequester(firstEpisodeRequester)
                            } else {
                                Modifier
                            },
                        )
                    }
                }
            }
        }
    }
}

@Composable
private fun EpisodeRow(
    entry: MergedEntry,
    label: String,
    title: String,
    progress: Float?,
    episodeArtworkUrl: (MergedEntry) -> String?,
    onPlayEpisode: (MergedEntry) -> Unit,
    modifier: Modifier = Modifier,
) {
    Card(
        onClick = { onPlayEpisode(entry) },
        colors = CardDefaults.colors(
            containerColor = Color.Transparent,
            contentColor = SwarmText,
            focusedContainerColor = SwarmSurface,
            focusedContentColor = SwarmText,
            pressedContainerColor = SwarmSurface,
            pressedContentColor = SwarmText,
        ),
        border = CardDefaults.border(
            focusedBorder = Border(BorderStroke(2.dp, SwarmAccent)),
            pressedBorder = Border(BorderStroke(2.dp, SwarmAccent)),
        ),
        scale = CardDefaults.scale(scale = 1f, focusedScale = 1f, pressedScale = 0.99f),
        modifier = modifier.fillMaxWidth()
            .testTag(UatTestTags.EPISODE_ITEM_PREFIX + entry.entry.entryKey),
    ) {
        Row(modifier = Modifier.fillMaxWidth().padding(8.dp)) {
            Box(modifier = Modifier.width(208.dp).aspectRatio(16f / 9f).clip(RoundedCornerShape(4.dp))) {
                ArtworkImage(
                    label = title,
                    placeholderType = "Show",
                    primaryUrl = episodeArtworkUrl(entry),
                    modifier = Modifier.fillMaxSize(),
                )
                progress?.takeIf { it > 0f }?.let { fraction ->
                    Box(
                        modifier = Modifier.align(Alignment.BottomStart).fillMaxWidth().height(4.dp)
                            .background(SwarmSurfaceMuted),
                    ) {
                        Box(
                            modifier = Modifier.fillMaxWidth(fraction.coerceIn(0f, 1f)).height(4.dp)
                                .background(SwarmAccent),
                        )
                    }
                }
            }
            Spacer(Modifier.width(16.dp))
            Column(modifier = Modifier.weight(1f).padding(vertical = 2.dp)) {
                Text(label, color = SwarmMuted, fontSize = 11.sp, maxLines = 1)
                Text(
                    title,
                    color = SwarmText,
                    fontSize = 15.sp,
                    fontWeight = FontWeight.SemiBold,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
                entry.entry.overview?.takeIf { it.isNotBlank() }?.let { overview ->
                    Spacer(Modifier.height(4.dp))
                    Text(
                        overview,
                        color = SwarmMuted,
                        fontSize = 12.sp,
                        lineHeight = 16.sp,
                        maxLines = 3,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
            }
        }
    }
}

/**
 * The most recently started-but-unfinished episode of [show], or null when
 * every episode is either untouched or already watched. Drives the
 * season-pane Resume button (#152): a show that was watched and stopped
 * offers Resume here even after it has aged out of the capped Continue
 * Watching row. "Started" means a saved position past 0s; "unfinished"
 * means the saved state is not yet flagged watched (95%+, see [WatchState]).
 */
internal fun resumeEpisode(show: ShowGroup, watchStates: Map<String, WatchState>): MergedEntry? =
    show.seasons.asSequence()
        .flatMap { it.episodes.asSequence() }
        .mapNotNull { episode ->
            val saved = watchStates.stateFor(episode.entry)
            if (saved == null || saved.watched || saved.positionSecs <= 0.0) null else episode to saved
        }
        .maxByOrNull { it.second.updatedAt }
        ?.first

/**
 * Left-pane order: real seasons ascending, then Specials (season 0), then
 * the unnumbered bucket — so the list reads Season 1…N with bonus content
 * after it, rather than Specials sorting ahead of Season 1.
 */
internal fun seasonPaneOrder(seasons: List<SeasonGroup>): List<SeasonGroup> =
    seasons.sortedWith(
        compareBy<SeasonGroup>(
            {
                when (val n = it.season) {
                    null -> 2
                    else -> if (n > 0) 0 else 1
                }
            },
            { it.season ?: 0 },
        ),
    )

/**
 * The season the screen opens on: the one holding the episode in progress
 * ([resumeEpisode]), else the first real season, else whatever comes first.
 */
internal fun defaultSeason(show: ShowGroup, watchStates: Map<String, WatchState>): SeasonGroup? {
    val resumeKey = resumeEpisode(show, watchStates)?.entry?.entryKey
    val ordered = seasonPaneOrder(show.seasons)
    return resumeKey?.let { key -> ordered.firstOrNull { s -> s.episodes.any { it.entry.entryKey == key } } }
        ?: ordered.firstOrNull()
}

/** Short duration label for an episode row: "45m", "1h 05m"; null when unknown. */
internal fun episodeDurationLabel(durationSecs: Double?): String? {
    val totalMinutes = durationSecs?.takeIf { it > 0.0 && it.isFinite() }?.let { (it / 60.0).toLong() } ?: return null
    if (totalMinutes < 1) return "<1m"
    val hours = totalMinutes / 60
    val minutes = totalMinutes % 60
    return if (hours > 0) "${hours}h ${minutes.toString().padStart(2, '0')}m" else "${minutes}m"
}

private fun seasonKey(season: SeasonGroup): Int = season.season ?: -1

private fun episodeLabel(episode: MergedEntry): String {
    val number = episode.entry.episode?.let { "Episode $it" } ?: "Episode"
    return episodeDurationLabel(episode.entry.durationSecs)?.let { "$number · $it" } ?: number
}

private fun seasonCountLabel(episodes: Int, extras: Int): String {
    val episodeCount = "$episodes episode" + if (episodes == 1) "" else "s"
    val extrasCount = if (extras > 0) ", $extras extra" + if (extras == 1) "" else "s" else ""
    return episodeCount + extrasCount
}

private fun showMetaLine(show: ShowGroup): String {
    val realSeasons = show.seasons.count { (it.season ?: 0) > 0 }
    val seasonsText = "$realSeasons Season" + if (realSeasons == 1) "" else "s"
    val year = show.seasons.asSequence().flatMap { it.episodes.asSequence() }.mapNotNull { it.entry.year }.minOrNull()
    return listOfNotNull(year?.toString(), seasonsText).joinToString("  ·  ")
}

private fun WatchState.progressFraction(): Float =
    if (durationSecs <= 0.0) 0f else (positionSecs / durationSecs).coerceIn(0.0, 1.0).toFloat()

/**
 * Season 0 is the real-world Plex/Kodi/TheTVDB convention for a show's
 * bonus/extra content (featurettes, interviews, deleted scenes...) — a
 * single show-level bucket, not tied to any one real season. `null` means
 * classify() found no season signal at all, a different, unrelated case.
 */
private fun seasonLabel(season: Int?): String = when (season) {
    null -> "Unnumbered"
    0 -> "Specials"
    else -> "Season $season"
}
