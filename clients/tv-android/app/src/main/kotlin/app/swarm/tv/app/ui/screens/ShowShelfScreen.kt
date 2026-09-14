/**
 * Full grid of every show in the catalog — reached from [CatalogScreen]'s
 * Shows row header ("Browse all"). Selecting a show opens [SeasonScreen].
 *
 * No title/Back header — see [MovieShelfScreen]'s identical doc comment.
 */
package app.swarm.tv.app.ui.screens

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.padding
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
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.focus.onFocusChanged
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.tv.material3.Card
import androidx.tv.material3.CardDefaults
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
        if (sortedShows.isEmpty()) {
            Text("No shows in the catalog yet.", color = SwarmMuted, fontSize = 14.sp, modifier = Modifier.padding(top = 40.dp))
        } else {
            // top = 32.dp — see MovieShelfScreen's identical comment on why.
            LazyVerticalGrid(
                state = gridState,
                columns = GridCells.Fixed(5),
                verticalArrangement = Arrangement.spacedBy(20.dp),
                horizontalArrangement = Arrangement.spacedBy(20.dp),
                contentPadding = PaddingValues(start = 12.dp, end = 12.dp, top = 32.dp, bottom = 12.dp),
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
                    Card(
                        onClick = { onOpenShow(show) },
                        colors = CardDefaults.colors(containerColor = SwarmSurface),
                        modifier = focusModifier.fillMaxWidth()
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
                        val previewAspectRatio = rememberBrowsePreviewAspectRatio(isPreviewExpanded)
                        Box(
                            modifier = Modifier.fillMaxWidth().aspectRatio(previewAspectRatio).clip(RoundedCornerShape(4.dp)),
                        ) {
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
