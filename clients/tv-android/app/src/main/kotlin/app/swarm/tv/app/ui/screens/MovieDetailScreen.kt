/**
 * Movie detail: backdrop, poster, title/year/genres, cast, and a Play
 * button — reached from [CatalogScreen]'s Movies row.
 *
 * No title-plus-Back header and no vertical scroll: the backdrop is a
 * full-bleed background behind everything else instead of its own banner
 * stacked above the poster row, and the poster/cast sizing below is tuned
 * to fit a real 1080p Fire TV viewport in one screen — a real bug found
 * live, this used to need scrolling to see Play/cast, which is a strange,
 * D-pad-unfriendly experience for a "press select to watch" detail page.
 * The remote's own physical Back button (wired via [BackHandler]) replaces
 * the removed on-screen one; people already know to use it.
 */
package app.swarm.tv.app.ui.screens

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.items
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
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusProperties
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.tv.material3.Button
import app.swarm.tv.app.ui.UatTestTags
import app.swarm.tv.app.data.ProblemReportCategory
import app.swarm.tv.app.ui.components.ProblemReportPicker
import app.swarm.tv.app.ui.components.swarmActionButtonColors
import app.swarm.tv.app.ui.theme.SwarmBackground
import app.swarm.tv.app.ui.theme.SwarmMuted
import app.swarm.tv.app.ui.theme.SwarmText
import app.swarm.tv.core.catalog.CatalogGrouping
import app.swarm.tv.core.catalog.MergedEntry
import coil.compose.AsyncImage

@Composable
fun MovieDetailScreen(
    entry: MergedEntry,
    extras: List<MergedEntry>,
    artworkUrl: (MergedEntry) -> String?,
    backdropUrl: (MergedEntry) -> String?,
    onPlay: (MergedEntry) -> Unit,
    onBack: () -> Unit,
    onReportProblem: (MergedEntry, ProblemReportCategory) -> Unit,
    isLiked: Boolean,
    onToggleLike: () -> Unit,
    isWatchlisted: Boolean,
    onToggleWatchlist: () -> Unit,
) {
    BackHandler(onBack = onBack)
    val playFocusRequester = remember { FocusRequester() }
    LaunchedEffect(entry) { playFocusRequester.requestFocus() }
    var showProblemPicker by remember(entry) { mutableStateOf(false) }

    Box(modifier = Modifier.fillMaxSize().background(SwarmBackground)) {
        backdropUrl(entry)?.let { url ->
            AsyncImage(
                model = url,
                contentDescription = null,
                contentScale = ContentScale.Crop,
                modifier = Modifier.fillMaxSize(),
            )
            // Scrim so the poster/title/cast text stays legible over whatever the
            // backdrop art looks like, without needing a solid banner that would
            // eat into the single-screen height budget.
            Box(
                modifier = Modifier.fillMaxSize().background(
                    Brush.horizontalGradient(
                        listOf(Color.Black.copy(alpha = 0.92f), Color.Black.copy(alpha = 0.55f), Color.Black.copy(alpha = 0.15f)),
                    ),
                ),
            )
        }

        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(horizontal = 48.dp, vertical = 28.dp)
                // The report picker is modal, so its dimmed detail controls must
                // not be reachable after the last picker option.
                .focusProperties { canFocus = !showProblemPicker },
        ) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                ArtworkImage(
                    label = entry.entry.scrapedTitle ?: entry.entry.title,
                    placeholderType = "Movie",
                    primaryUrl = artworkUrl(entry),
                    modifier = Modifier.width(170.dp).aspectRatio(2f / 3f).clip(RoundedCornerShape(8.dp))
                        .testTag(UatTestTags.MOVIE_DETAIL_ARTWORK),
                )
                Spacer(Modifier.width(32.dp))
                Column(Modifier.weight(1f)) {
                    Text(
                        entry.entry.scrapedTitle ?: entry.entry.title,
                        color = SwarmText,
                        fontSize = 26.sp,
                        fontWeight = FontWeight.Black,
                        maxLines = 2,
                    )
                    val meta = listOfNotNull(entry.entry.year?.toString())
                    if (meta.isNotEmpty()) {
                        Spacer(Modifier.height(6.dp))
                        Text(
                            meta.joinToString("  •  "),
                            color = SwarmMuted,
                            fontSize = 14.sp,
                            maxLines = 1,
                            modifier = Modifier.testTag(UatTestTags.MOVIE_DETAIL_YEAR),
                        )
                    }
                    // Categories = genres — same field TMDb auto-populates at scrape
                    // time and the media server's category picker lets a user hand-
                    // assign; shown here as tags rather than folded into the meta
                    // line above so they read as the browsable/clickable-elsewhere
                    // concept they are, not just descriptive text.
                    if (entry.entry.genres.isNotEmpty()) {
                        Spacer(Modifier.height(8.dp))
                        Row(
                            horizontalArrangement = Arrangement.spacedBy(8.dp),
                            modifier = Modifier.testTag(UatTestTags.MOVIE_DETAIL_GENRES),
                        ) {
                            for (genre in entry.entry.genres.take(4)) {
                                CategoryChip(genre)
                            }
                        }
                    }
                    Spacer(Modifier.height(16.dp))
                    Row(horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                        Button(
                            onClick = { onPlay(entry) },
                            modifier = Modifier.focusRequester(playFocusRequester)
                                .testTag(UatTestTags.MOVIE_DETAIL_PLAY_BUTTON),
                            colors = swarmActionButtonColors(),
                        ) {
                            Text("Play", fontWeight = FontWeight.Bold)
                        }
                        Button(
                            onClick = onToggleLike,
                            colors = swarmActionButtonColors(),
                            modifier = Modifier.testTag(UatTestTags.MOVIE_DETAIL_LIKE_BUTTON),
                        ) {
                            Text(if (isLiked) "♥ Liked" else "♡ Like", fontSize = 13.sp)
                        }
                        Button(
                            onClick = onToggleWatchlist,
                            colors = swarmActionButtonColors(),
                            modifier = Modifier.testTag(UatTestTags.MOVIE_DETAIL_WATCHLIST_BUTTON),
                        ) {
                            Text(if (isWatchlisted) "✓ Watchlisted" else "+ Watchlist", fontSize = 13.sp)
                        }
                        Button(
                            onClick = { showProblemPicker = true },
                            colors = swarmActionButtonColors(),
                            modifier = Modifier.testTag(UatTestTags.MOVIE_DETAIL_REPORT_PROBLEM_BUTTON),
                        ) {
                            Text("Report a problem", fontSize = 13.sp)
                        }
                    }
                    if (entry.entry.cast.isNotEmpty()) {
                        Spacer(Modifier.height(14.dp))
                        Text("Cast", color = SwarmMuted, fontSize = 12.sp, fontWeight = FontWeight.SemiBold)
                        Spacer(Modifier.height(4.dp))
                        Text(
                            entry.entry.cast.take(6).joinToString("   •   ") { it.name },
                            color = SwarmText,
                            fontSize = 13.sp,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                            modifier = Modifier.testTag(UatTestTags.MOVIE_DETAIL_CAST),
                        )
                    }
                }
            }
            // Full screen width, not squeezed into the info column next to
            // the poster — real feedback from live use: sharing that
            // narrower column left this cramped to ~3 half-legible lines.
            // A fixed height (not just maxLines) is what actually keeps
            // every detail page's layout consistent regardless of synopsis
            // length, long or short.
            entry.entry.overview?.takeIf { it.isNotBlank() }?.let { overview ->
                Spacer(Modifier.height(18.dp))
                Text(
                    overview,
                    color = SwarmMuted,
                    fontSize = 14.sp,
                    lineHeight = 20.sp,
                    maxLines = 5,
                    overflow = TextOverflow.Ellipsis,
                    modifier = Modifier.fillMaxWidth().height(100.dp)
                        .testTag(UatTestTags.MOVIE_DETAIL_DESCRIPTION),
                )
            }
            if (extras.isNotEmpty()) {
                Spacer(Modifier.height(14.dp))
                Text("Extras", color = SwarmText, fontSize = 17.sp, fontWeight = FontWeight.Bold)
                Spacer(Modifier.height(6.dp))
                LazyRow(horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                    items(extras, key = { it.entry.entryKey }) { extra ->
                        Button(
                            onClick = { onPlay(extra) },
                            colors = swarmActionButtonColors(),
                            modifier = Modifier.testTag("movie-extra-${extra.entry.entryKey}"),
                        ) {
                            val kind = CatalogGrouping.extraTypeLabel(extra.entry.extraType)
                            val title = extra.entry.extraTitle ?: extra.entry.title
                            Text("$kind • $title", fontSize = 13.sp, maxLines = 1)
                        }
                    }
                }
            }
        }
        if (showProblemPicker) {
            ProblemReportPicker(
                onReport = { category ->
                    onReportProblem(entry, category)
                    showProblemPicker = false
                },
                onDismiss = { showProblemPicker = false },
            )
        }
    }
}

@Composable
private fun CategoryChip(label: String) {
    Box(
        modifier = Modifier
            .clip(RoundedCornerShape(999.dp))
            .background(Color.White.copy(alpha = 0.12f))
            .padding(horizontal = 12.dp, vertical = 5.dp),
    ) {
        Text(label, color = SwarmText, fontSize = 12.sp, fontWeight = FontWeight.SemiBold)
    }
}
