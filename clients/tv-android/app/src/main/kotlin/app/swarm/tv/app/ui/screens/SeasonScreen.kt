/**
 * One show: a list of its seasons, drilling into an episode grid on
 * selection — same local-state pattern as [AlbumScreen] (see its doc
 * comment for the rationale). Selecting an episode calls [onPlayEpisode]
 * directly — no separate detail screen in between, so getting to an
 * episode never takes more than season -> episode -> playing.
 */
package app.swarm.tv.app.ui.screens

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.grid.GridCells
import androidx.compose.foundation.lazy.grid.GridItemSpan
import androidx.compose.foundation.lazy.grid.LazyVerticalGrid
import androidx.compose.foundation.lazy.grid.itemsIndexed
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.Alignment
import androidx.compose.ui.draw.clip
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.tv.material3.Card
import androidx.tv.material3.CardDefaults
import androidx.tv.material3.Button
import app.swarm.tv.app.ui.UatTestTags
import app.swarm.tv.app.ui.components.swarmActionButtonColors
import app.swarm.tv.core.catalog.CatalogGrouping
import app.swarm.tv.core.catalog.displayTitle
import app.swarm.tv.app.ui.theme.SwarmMuted
import app.swarm.tv.app.ui.theme.SwarmSurface
import app.swarm.tv.app.ui.theme.SwarmText
import app.swarm.tv.core.catalog.MergedEntry
import app.swarm.tv.core.catalog.SeasonGroup
import app.swarm.tv.core.catalog.ShowGroup
import app.swarm.tv.core.watch.WatchState

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
     * finished — see [SeasonList]'s Resume button (#152). Resumes that
     * episode from its saved position, Continue-Watching style. */
    onResume: (() -> Unit)? = null,
) {
    BackHandler { if (selectedSeason != null) onSelectSeason(null) else onBack() }

    val season = selectedSeason
    if (season == null) {
        SeasonList(
            show,
            seasonArtworkUrl,
            isWatchlisted,
            onToggleWatchlist,
            onResume = onResume,
            onOpenSeason = onSelectSeason,
        )
    } else {
        EpisodeGrid(season, episodeArtworkUrl, onPlayEpisode)
    }
}

@Composable
private fun SeasonList(
    show: ShowGroup,
    seasonArtworkUrl: (MergedEntry) -> String?,
    isWatchlisted: Boolean,
    onToggleWatchlist: () -> Unit,
    onResume: (() -> Unit)?,
    onOpenSeason: (SeasonGroup) -> Unit,
) {
    val firstCardFocusRequester = remember { FocusRequester() }
    LaunchedEffect(show) { if (show.seasons.isNotEmpty()) firstCardFocusRequester.requestFocus() }

    LazyVerticalGrid(
        columns = GridCells.Fixed(5),
        modifier = Modifier.fillMaxSize().padding(40.dp),
        verticalArrangement = Arrangement.spacedBy(20.dp),
        horizontalArrangement = Arrangement.spacedBy(20.dp),
        // Room for tv-material3's focus-scale animation on edge cards — see CatalogScreen.kt.
        contentPadding = PaddingValues(12.dp),
    ) {
        item(
            key = "show-actions",
            span = { GridItemSpan(maxLineSpan) },
            contentType = "header",
        ) {
            Row(
                modifier = Modifier.fillMaxWidth().padding(bottom = 8.dp),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.SpaceBetween,
            ) {
                Text(
                    show.show,
                    color = SwarmText,
                    fontSize = 22.sp,
                    fontWeight = FontWeight.Black,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                    modifier = Modifier.weight(1f).padding(end = 16.dp)
                        .testTag(UatTestTags.SEASON_SCREEN_SHOW_TITLE),
                )
                Row(horizontalArrangement = Arrangement.spacedBy(12.dp)) {
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
            }
        }
        itemsIndexed(
            items = show.seasons,
            key = { _, season -> season.season ?: -1 },
            contentType = { _, _ -> "season" },
        ) { index, season ->
            val focusModifier = if (index == 0) Modifier.focusRequester(firstCardFocusRequester) else Modifier
            val realEpisodes = CatalogGrouping.seasonEpisodes(season)
            val extras = CatalogGrouping.seasonExtras(season)
            val seasonArt = (realEpisodes.firstOrNull() ?: extras.firstOrNull())?.let(seasonArtworkUrl)
            Card(
                onClick = { onOpenSeason(season) },
                colors = CardDefaults.colors(containerColor = SwarmSurface),
                scale = CardDefaults.scale(scale = 1f, focusedScale = 1f, pressedScale = 0.99f),
                modifier = focusModifier.fillMaxWidth()
                    .testTag(UatTestTags.SEASON_CARD_PREFIX + (season.season ?: -1)),
            ) {
                Column {
                    ArtworkImage(
                        label = seasonLabel(season.season),
                        placeholderType = "Show",
                        primaryUrl = seasonArt,
                        modifier = Modifier.fillMaxWidth().aspectRatio(2f / 3f).clip(RoundedCornerShape(4.dp)),
                    )
                    Column(Modifier.padding(14.dp)) {
                        Text(seasonLabel(season.season), color = SwarmText, fontSize = 14.sp, fontWeight = FontWeight.SemiBold, maxLines = 1)
                        Spacer(Modifier.height(4.dp))
                        val episodeCount = "${realEpisodes.size} episode" + if (realEpisodes.size == 1) "" else "s"
                        val extrasCount = if (extras.isNotEmpty()) ", ${extras.size} extra" + if (extras.size == 1) "" else "s" else ""
                        Text(episodeCount + extrasCount, color = SwarmMuted, fontSize = 11.sp)
                    }
                }
            }
        }
    }
}

@Composable
private fun EpisodeGrid(
    season: SeasonGroup,
    episodeArtworkUrl: (MergedEntry) -> String?,
    onPlayEpisode: (MergedEntry) -> Unit,
) {
    val episodes = CatalogGrouping.seasonEpisodes(season)
    val extras = CatalogGrouping.seasonExtras(season)
    val firstCardFocusRequester = remember { FocusRequester() }
    LaunchedEffect(season) { if (episodes.isNotEmpty() || extras.isNotEmpty()) firstCardFocusRequester.requestFocus() }

    LazyVerticalGrid(
        columns = GridCells.Fixed(4),
        modifier = Modifier.fillMaxSize().padding(40.dp),
        verticalArrangement = Arrangement.spacedBy(20.dp),
        horizontalArrangement = Arrangement.spacedBy(20.dp),
        // Room for tv-material3's focus-scale animation on edge cards — see CatalogScreen.kt.
        contentPadding = PaddingValues(12.dp),
    ) {
        // Bonus content (featurettes/deleted scenes/etc.) nested under this
        // show's Specials, labeled and grouped exactly like a movie's
        // extras row on MovieDetailScreen — never a plain, undifferentiated
        // episode card.
        if (extras.isNotEmpty()) {
            item(
                key = "extras-header",
                span = { GridItemSpan(maxLineSpan) },
                contentType = "header",
            ) {
                Text("Extras", color = SwarmText, fontSize = 17.sp, fontWeight = FontWeight.Bold)
            }
            itemsIndexed(
                items = extras,
                key = { _, extra -> extra.entry.entryKey },
                contentType = { _, _ -> "extra" },
            ) { index, extra ->
                val focusModifier =
                    if (index == 0 && episodes.isEmpty()) Modifier.focusRequester(firstCardFocusRequester) else Modifier
                EpisodeCard(
                    entry = extra,
                    label = CatalogGrouping.extraTypeLabel(extra.entry.extraType),
                    title = extra.entry.extraTitle ?: extra.entry.title,
                    episodeArtworkUrl = episodeArtworkUrl,
                    onPlayEpisode = onPlayEpisode,
                    modifier = focusModifier,
                )
            }
        }
        itemsIndexed(
            items = episodes,
            key = { _, episode -> episode.entry.entryKey },
            contentType = { _, _ -> "episode" },
        ) { index, episode ->
            val focusModifier = if (index == 0) Modifier.focusRequester(firstCardFocusRequester) else Modifier
            EpisodeCard(
                entry = episode,
                label = episode.entry.episode?.let { "Episode $it" } ?: "Episode",
                title = episode.entry.displayTitle(),
                episodeArtworkUrl = episodeArtworkUrl,
                onPlayEpisode = onPlayEpisode,
                modifier = focusModifier,
            )
        }
    }
}

@Composable
private fun EpisodeCard(
    entry: MergedEntry,
    label: String,
    title: String,
    episodeArtworkUrl: (MergedEntry) -> String?,
    onPlayEpisode: (MergedEntry) -> Unit,
    modifier: Modifier = Modifier,
) {
    Card(
        onClick = { onPlayEpisode(entry) },
        colors = CardDefaults.colors(containerColor = SwarmSurface),
        scale = CardDefaults.scale(scale = 1f, focusedScale = 1f, pressedScale = 0.99f),
        modifier = modifier.fillMaxWidth()
            .testTag(UatTestTags.EPISODE_ITEM_PREFIX + entry.entry.entryKey),
    ) {
        Column {
            ArtworkImage(
                label = title,
                placeholderType = "Show",
                primaryUrl = episodeArtworkUrl(entry),
                modifier = Modifier.fillMaxWidth().aspectRatio(16f / 9f).clip(RoundedCornerShape(4.dp)),
            )
            Column(Modifier.padding(14.dp)) {
                Text(label, color = SwarmMuted, fontSize = 11.sp)
                Text(title, color = SwarmText, fontSize = 14.sp, fontWeight = FontWeight.SemiBold, minLines = 2, maxLines = 2)
            }
        }
    }
}

/**
 * The most recently started-but-unfinished episode of [show], or null when
 * every episode is either untouched or already watched. Drives the
 * season-list Resume button (#152): a show that was watched and stopped
 * offers Resume here even after it has aged out of the capped Continue
 * Watching row. "Started" means a saved position past 0s; "unfinished"
 * means the saved state is not yet flagged watched (95%+, see [WatchState]).
 */
internal fun resumeEpisode(show: ShowGroup, watchStates: Map<String, WatchState>): MergedEntry? =
    show.seasons.asSequence()
        .flatMap { it.episodes.asSequence() }
        .mapNotNull { episode ->
            val saved = watchStates[episode.entry.fingerprint]
            if (saved == null || saved.watched || saved.positionSecs <= 0.0) null else episode to saved
        }
        .maxByOrNull { it.second.updatedAt }
        ?.first

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
