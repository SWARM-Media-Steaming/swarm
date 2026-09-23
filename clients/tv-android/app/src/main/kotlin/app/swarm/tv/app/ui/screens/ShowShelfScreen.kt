/**
 * Full grid of shows — reached from [CatalogScreen]'s Shows row or a
 * genre sub-shelf's "Browse All" tile. Selecting a show opens [SeasonScreen].
 *
 * The originating category name is shown at the top (#353) — see
 * [MovieShelfScreen]'s identical doc comment.
 */
package app.swarm.tv.app.ui.screens

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.requiredWidth
import androidx.compose.foundation.layout.wrapContentWidth
import androidx.compose.foundation.lazy.grid.GridCells
import androidx.compose.foundation.lazy.grid.LazyVerticalGrid
import androidx.compose.foundation.lazy.grid.itemsIndexed
import androidx.compose.foundation.lazy.grid.rememberLazyGridState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.runtime.withFrameNanos
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.zIndex
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.focus.onFocusChanged
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.tv.material3.Card
import androidx.tv.material3.CardDefaults
import app.swarm.tv.app.data.BROWSE_ALL_SHOWS_TITLE
import app.swarm.tv.app.data.BrowsePreview
import app.swarm.tv.app.ui.theme.SwarmMuted
import app.swarm.tv.app.ui.theme.SwarmSurface
import app.swarm.tv.app.ui.UatTestTags
import app.swarm.tv.core.catalog.CatalogGrouping
import app.swarm.tv.core.catalog.MergedEntry
import app.swarm.tv.core.catalog.ShowGroup

/**
 * @param preview / onStartPreview / onStopPreview / onPreviewFinished — the
 *   same hover-preview hooks [CatalogScreen] uses, so a focused grid card
 *   plays an inline video preview of a representative episode here too (#159).
 */
@Composable
fun ShowShelfScreen(
    shows: List<ShowGroup>,
    artworkUrl: (MergedEntry) -> String?,
    onOpenShow: (ShowGroup) -> Unit,
    onBack: () -> Unit,
    title: String = BROWSE_ALL_SHOWS_TITLE,
    initialFocusKey: String? = null,
    preview: BrowsePreview? = null,
    onStartPreview: (MergedEntry) -> Unit = {},
    onStopPreview: () -> Unit = {},
    onPreviewFinished: (String) -> Unit = {},
) {
    BackHandler(onBack = onBack)
    val previewCoordinator = rememberBrowsePreviewCoordinator(
        preview = preview,
        onStartPreview = onStartPreview,
        onStopPreview = onStopPreview,
        onPreviewFinished = onPreviewFinished,
    )

    // Alphabetical, not the rating order the shelf row that opened this
    // screen sorts by — see [browseAllSortKey].
    val sortedShows = remember(shows) { shows.sortedBy { browseAllSortKey(it.show) } }
    // One stable representative episode per show for the hover preview, kept
    // across recompositions/focus changes so it doesn't resample — same rule
    // as [CatalogScreen]'s show rows.
    val previewEntries = remember(sortedShows) {
        sortedShows.map { CatalogGrouping.randomPreviewEpisode(it) }
    }
    val firstCardFocusRequester = remember { FocusRequester() }
    val gridState = rememberLazyGridState()
    val focusIndex = remember(sortedShows, initialFocusKey) {
        initialFocusKey?.let { key -> sortedShows.indexOfFirst { it.show == key }.takeIf { it >= 0 } } ?: 0
    }
    // Place initial focus exactly once per visit — see MovieShelfScreen's
    // identical comment on why keying this on the sorted list (which the live
    // catalog feed rebuilds on every delta) broke the first row's hover
    // preview (#190).
    var initialFocusPlaced by remember { mutableStateOf(false) }
    LaunchedEffect(sortedShows.isEmpty()) {
        if (!shouldPlaceBrowseAllInitialFocus(initialFocusPlaced, sortedShows.isEmpty())) return@LaunchedEffect
        gridState.scrollToItem(focusIndex)
        repeat(BROWSE_ALL_FOCUS_ATTEMPTS) {
            withFrameNanos {}
            if (runCatching { firstCardFocusRequester.requestFocus() }.isSuccess) {
                initialFocusPlaced = true
                return@LaunchedEffect
            }
        }
    }

    Column(modifier = Modifier.fillMaxSize().padding(horizontal = 40.dp)) {
        BrowseAllScreenTitle(title)
        if (sortedShows.isEmpty()) {
            Text("No shows in the catalog yet.", color = SwarmMuted, fontSize = 14.sp)
        } else {
            // Title above the grid now supplies the top-edge headroom — see
            // MovieShelfScreen's identical comment.
            LazyVerticalGrid(
                state = gridState,
                columns = GridCells.Fixed(BROWSE_ALL_GRID_COLUMNS),
                verticalArrangement = Arrangement.spacedBy(20.dp),
                horizontalArrangement = Arrangement.spacedBy(20.dp),
                contentPadding = PaddingValues(start = 12.dp, end = 12.dp, top = 12.dp, bottom = 12.dp),
            ) {
                itemsIndexed(
                    items = sortedShows,
                    key = { _, show -> show.show },
                    contentType = { _, _ -> "show" },
                ) { index, show ->
                    val representative = show.seasons.firstOrNull()?.episodes?.firstOrNull()
                    val previewEntry = previewEntries[index]
                    val focusModifier = if (index == focusIndex) Modifier.focusRequester(firstCardFocusRequester) else Modifier
                    var isFocused by remember(show.show) { mutableStateOf(false) }
                    val isPreviewExpanded = isFocused && previewEntry != null &&
                        previewCoordinator.expandedPreviewEntryKey == previewEntry.entry.entryKey
                    BoxWithConstraints(
                        modifier = Modifier.fillMaxWidth().aspectRatio(2f / 3f)
                            .zIndex(if (isFocused) 1f else 0f),
                    ) {
                        val previewWidth = rememberBrowsePreviewWidth(maxWidth, isPreviewExpanded)
                        val previewAlignment = browsePreviewAlignment(index)
                        Card(
                            onClick = { onOpenShow(show) },
                            colors = CardDefaults.colors(containerColor = SwarmSurface),
                            modifier = focusModifier.fillMaxHeight()
                                .wrapContentWidth(align = previewAlignment, unbounded = true)
                                .requiredWidth(previewWidth)
                                .testTag(UatTestTags.GRID_SHOW_PREFIX + show.show)
                                .onFocusChanged { focusState ->
                                    if (isFocused != focusState.isFocused) {
                                        isFocused = focusState.isFocused
                                        previewEntry?.let {
                                            previewCoordinator.onPreviewFocusChanged(it, focusState.isFocused)
                                        }
                                    }
                                },
                        ) {
                            Box(modifier = Modifier.fillMaxSize().clip(RoundedCornerShape(4.dp))) {
                                ArtworkImage(
                                    label = show.show,
                                    placeholderType = "Show",
                                    primaryUrl = representative?.let(artworkUrl),
                                    modifier = Modifier.fillMaxSize(),
                                )
                                if (previewEntry != null) {
                                    BrowsePreviewGridOverlay(
                                        entryKey = previewEntry.entry.entryKey,
                                        isFocused = isFocused,
                                        isExpanded = isPreviewExpanded,
                                        preview = preview,
                                        onFinished = previewCoordinator.onPreviewFinished,
                                    )
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
