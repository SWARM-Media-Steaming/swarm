package app.swarm.tv.app.ui.screens

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.focusGroup
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
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.focus.FocusDirection
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusProperties
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.focus.onFocusChanged
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.input.key.Key
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

private class EpisodeSection(val season: SeasonGroup, val headerIndex: Int) {
    val episodes = CatalogGrouping.seasonEpisodes(season)
    val extras = CatalogGrouping.seasonExtras(season)
    val rowCount = 1 + episodes.size + if (extras.isEmpty()) 0 else (1 + extras.size)

    fun focusIndex(fingerprint: String?): Int? {
        val episodeIndex = episodes.indexOfFirst { it.fingerprint == fingerprint }
        if (episodeIndex >= 0) return headerIndex + 1 + episodeIndex
        val extraIndex = extras.indexOfFirst { it.fingerprint == fingerprint }
        if (extraIndex >= 0) return headerIndex + 2 + episodes.size + extraIndex
        return when {
            episodes.isNotEmpty() -> headerIndex + 1
            extras.isNotEmpty() -> headerIndex + 2
            else -> null
        }
    }
}

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
     * finished — see the season pane's Resume button (#152). Plays that
     * episode immediately from its saved position; it does not open the
     * paused Resume cover, since the viewer already pressed Resume (#496). */
    onResume: (() -> Unit)? = null,
    /** Saved per-episode progress: drives each episode thumbnail's progress
     * bar and which season the screen opens on. */
    watchStates: Map<String, WatchState> = emptyMap(),
) {
    val seasons = remember(show) { seasonPaneOrder(show.seasons) }
    val sections = remember(seasons) {
        var headerIndex = 0
        seasons.map { season ->
            EpisodeSection(season, headerIndex).also { headerIndex += it.rowCount }
        }
    }
    var highlightedKey by remember(show.show) {
        mutableStateOf(
            selectedSeason?.let(::seasonKey)
                ?: defaultSeason(show, watchStates)?.let(::seasonKey),
        )
    }
    var lastFocusedEpisodeFingerprint by remember(show.show) { mutableStateOf<String?>(null) }
    val displayedIndex = seasons.indexOfFirst { seasonKey(it) == highlightedKey }.coerceAtLeast(0)
    val displayedSection = sections.getOrNull(displayedIndex)
    val displayed = displayedSection?.season
    val inEpisodes = selectedSeason != null

    var seasonPaneFocused by remember { mutableStateOf(false) }
    val highlightedSeasonRequester = remember { FocusRequester() }
    val firstEpisodeRequester = remember { FocusRequester() }
    val seasonListState = rememberLazyListState(initialFirstVisibleItemIndex = displayedIndex)
    val episodeListState = rememberLazyListState(initialFirstVisibleItemIndex = displayedSection?.headerIndex ?: 0)
    val openSeason: (SeasonGroup) -> Unit = { season ->
        val section = sections.firstOrNull { seasonKey(it.season) == seasonKey(season) }
        highlightedKey = seasonKey(season)
        if (section != null && section.rowCount > 1) onSelectSeason(season)
    }

    BackHandler { if (inEpisodes) onSelectSeason(null) else onBack() }

    // Only transfer focus on pane changes. Focusing a row in the episode
    // list updates the season selection without moving focus again.
    LaunchedEffect(inEpisodes, show.show, seasons.isNotEmpty()) {
        val section = displayedSection ?: return@LaunchedEffect
        if (inEpisodes) {
            val index = section.focusIndex(lastFocusedEpisodeFingerprint)
            if (index != null) episodeListState.focusBrowseItem(index, firstEpisodeRequester)
            else onSelectSeason(null)
        } else if (!seasonPaneFocused) {
            seasonListState.focusBrowseItem(displayedIndex, highlightedSeasonRequester)
        }
    }
    LaunchedEffect(displayedSection?.headerIndex, displayed?.let(::seasonKey), inEpisodes) {
        val section = displayedSection ?: return@LaunchedEffect
        if (inEpisodes) seasonListState.revealBrowseItem(displayedIndex)
        else episodeListState.scrollToItem(section.headerIndex)
    }

    Row(
        modifier = Modifier.fillMaxSize().padding(horizontal = 40.dp, vertical = 28.dp),
        horizontalArrangement = Arrangement.spacedBy(28.dp),
    ) {
        SeasonPane(
            show = show,
            sections = sections,
            displayed = displayed,
            seasonArtworkUrl = seasonArtworkUrl,
            isWatchlisted = isWatchlisted,
            onToggleWatchlist = onToggleWatchlist,
            onResume = onResume,
            listState = seasonListState,
            highlightedSeasonRequester = highlightedSeasonRequester,
            onHighlightSeason = { season ->
                val key = seasonKey(season)
                if (highlightedKey != key) lastFocusedEpisodeFingerprint = null
                highlightedKey = key
            },
            onOpenSeason = openSeason,
            modifier = Modifier.width(300.dp).fillMaxHeight()
                .onFocusChanged { state ->
                    seasonPaneFocused = state.hasFocus
                    if (state.hasFocus && inEpisodes) onSelectSeason(null)
                }
                .focusGroup(),
        )
        if (displayedSection != null) {
            EpisodePane(
                sections = sections,
                listState = episodeListState,
                focusIndex = displayedSection.focusIndex(lastFocusedEpisodeFingerprint),
                watchStates = watchStates,
                episodeArtworkUrl = episodeArtworkUrl,
                onPlayEpisode = onPlayEpisode,
                firstEpisodeRequester = firstEpisodeRequester,
                onFocusEpisode = { season, episode ->
                    highlightedKey = seasonKey(season)
                    lastFocusedEpisodeFingerprint = episode.fingerprint
                    onSelectSeason(season)
                },
                modifier = Modifier.weight(1f).fillMaxHeight()
                    .onPaneDirection(Key.DirectionLeft) { onSelectSeason(null) },
            )
        }
    }
}

@Composable
private fun SeasonPane(
    show: ShowGroup,
    sections: List<EpisodeSection>,
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
                modifier = Modifier.onPaneDirection(Key.DirectionRight) { displayed?.let(onOpenSeason) }
                    .testTag(UatTestTags.SEASON_SCREEN_WATCHLIST_BUTTON),
            ) {
                Text(if (isWatchlisted) "✓ Watchlisted" else "+ Watchlist", fontSize = 13.sp)
            }
        }
        Spacer(Modifier.height(18.dp))
        LazyColumn(
            state = listState,
            verticalArrangement = Arrangement.spacedBy(6.dp),
            modifier = Modifier.fillMaxWidth().weight(1f),
            contentPadding = PaddingValues(vertical = 4.dp),
        ) {
            itemsIndexed(
                items = sections,
                key = { _, section -> seasonKey(section.season) },
                contentType = { _, _ -> "season" },
            ) { _, section ->
                val season = section.season
                val isDisplayed = displayed != null && seasonKey(season) == seasonKey(displayed)
                val onOpen = { onOpenSeason(season) }
                val focusModifier = if (isDisplayed) Modifier.focusRequester(highlightedSeasonRequester) else Modifier
                BrowseGroupRow(
                    title = seasonLabel(season.season),
                    count = seasonCountLabel(section.episodes.size, section.extras.size),
                    isDisplayed = isDisplayed,
                    onFocused = { onHighlightSeason(season) },
                    onClick = onOpen,
                    modifier = focusModifier
                        .onPaneDirection(Key.DirectionRight, onOpen)
                        .testTag(UatTestTags.SEASON_CARD_PREFIX + (season.season ?: -1)),
                )
            }
        }
    }
}

@Composable
private fun EpisodePane(
    sections: List<EpisodeSection>,
    listState: LazyListState,
    focusIndex: Int?,
    watchStates: Map<String, WatchState>,
    episodeArtworkUrl: (MergedEntry) -> String?,
    onPlayEpisode: (MergedEntry) -> Unit,
    firstEpisodeRequester: FocusRequester,
    onFocusEpisode: (SeasonGroup, MergedEntry) -> Unit,
    modifier: Modifier = Modifier,
) {
    LazyColumn(
        state = listState,
        verticalArrangement = Arrangement.spacedBy(10.dp),
        modifier = modifier.focusProperties {
            onExit = {
                if (requestedFocusDirection == FocusDirection.Up || requestedFocusDirection == FocusDirection.Down) {
                    cancelFocusChange()
                }
            }
        }.focusGroup(),
        contentPadding = PaddingValues(vertical = 4.dp),
    ) {
        for (section in sections) {
            val seasonNumber = section.season.season
            item(key = "season-header:$seasonNumber", contentType = "header") {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text(seasonLabel(seasonNumber), color = SwarmText, fontSize = 20.sp, fontWeight = FontWeight.Bold)
                    Spacer(Modifier.width(10.dp))
                    Text(seasonCountLabel(section.episodes.size, section.extras.size), color = SwarmMuted, fontSize = 13.sp)
                }
            }
            itemsIndexed(
                items = section.episodes,
                key = { _, episode -> "episode:${episode.fingerprint}" },
                contentType = { _, _ -> "episode" },
            ) { index, episode ->
                val focusModifier = if (section.headerIndex + 1 + index == focusIndex) {
                    Modifier.focusRequester(firstEpisodeRequester)
                } else Modifier
                EpisodeRow(
                    entry = episode,
                    label = episodeLabel(episode),
                    title = episode.entry.displayTitle(),
                    progress = watchStates.stateFor(episode.entry)?.progressFraction(),
                    episodeArtworkUrl = episodeArtworkUrl,
                    onPlayEpisode = onPlayEpisode,
                    modifier = focusModifier.onFocusChanged { if (it.isFocused) onFocusEpisode(section.season, episode) },
                )
            }
            if (section.extras.isNotEmpty()) {
                item(key = "extras-header:$seasonNumber", contentType = "header") {
                    Text(
                        "Extras",
                        color = SwarmText,
                        fontSize = 17.sp,
                        fontWeight = FontWeight.Bold,
                        modifier = Modifier.padding(top = 8.dp),
                    )
                }
                itemsIndexed(
                    items = section.extras,
                    key = { _, extra -> "extra:${extra.fingerprint}" },
                    contentType = { _, _ -> "extra" },
                ) { index, extra ->
                    val focusModifier = if (section.headerIndex + 2 + section.episodes.size + index == focusIndex) {
                        Modifier.focusRequester(firstEpisodeRequester)
                    } else Modifier
                    EpisodeRow(
                        entry = extra,
                        label = CatalogGrouping.extraTypeLabel(extra.entry.extraType),
                        title = extra.entry.extraTitle ?: extra.entry.title,
                        progress = watchStates.stateFor(extra.entry)?.progressFraction(),
                        episodeArtworkUrl = episodeArtworkUrl,
                        onPlayEpisode = onPlayEpisode,
                        modifier = focusModifier.onFocusChanged { if (it.isFocused) onFocusEpisode(section.season, extra) },
                    )
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

private fun seasonKey(season: SeasonGroup): String = season.season.toString()

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
