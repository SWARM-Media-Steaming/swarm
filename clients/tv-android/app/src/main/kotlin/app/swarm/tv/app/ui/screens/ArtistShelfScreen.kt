/**
 * Full grid of artists — reached from [CatalogScreen]'s Music row or a
 * genre sub-shelf's "Browse All" tile. Selecting an artist opens [AlbumScreen].
 *
 * The originating category name is shown at the top (#353) — see
 * [MovieShelfScreen]'s identical doc comment.
 */
package app.swarm.tv.app.ui.screens

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.layout.Arrangement
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
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.tv.material3.Card
import androidx.tv.material3.CardDefaults
import app.swarm.tv.app.data.BROWSE_ALL_MUSIC_TITLE
import app.swarm.tv.app.ui.theme.SwarmMuted
import app.swarm.tv.app.ui.theme.SwarmSurface
import app.swarm.tv.app.ui.UatTestTags
import app.swarm.tv.core.catalog.ArtistGroup
import app.swarm.tv.core.catalog.MergedEntry

@Composable
fun ArtistShelfScreen(
    artists: List<ArtistGroup>,
    artworkUrl: (MergedEntry) -> String?,
    artistPhotoUrl: (MergedEntry) -> String?,
    onOpenArtist: (ArtistGroup) -> Unit,
    onBack: () -> Unit,
    title: String = BROWSE_ALL_MUSIC_TITLE,
    initialFocusKey: String? = null,
) {
    BackHandler(onBack = onBack)

    // Alphabetical, not the rating order the shelf row that opened this
    // screen sorts by — see [browseAllSortKey].
    val sortedArtists = remember(artists) { artists.sortedBy { browseAllSortKey(it.artist) } }
    val firstCardFocusRequester = remember { FocusRequester() }
    val gridState = rememberLazyGridState()
    val focusIndex = remember(sortedArtists, initialFocusKey) {
        initialFocusKey?.let { key -> sortedArtists.indexOfFirst { it.artist == key }.takeIf { it >= 0 } } ?: 0
    }
    // Place initial focus exactly once per visit so a live catalog delta
    // (#147) can't yank focus and scroll back to the first card — same fix
    // and reasoning as MovieShelfScreen (#190).
    var initialFocusPlaced by remember { mutableStateOf(false) }
    LaunchedEffect(sortedArtists.isEmpty()) {
        if (!shouldPlaceBrowseAllInitialFocus(initialFocusPlaced, sortedArtists.isEmpty())) return@LaunchedEffect
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
        if (sortedArtists.isEmpty()) {
            Text("No music in the catalog yet.", color = SwarmMuted, fontSize = 14.sp)
        } else {
            // Title above the grid now supplies the top-edge headroom — see
            // MovieShelfScreen's identical comment.
            LazyVerticalGrid(
                state = gridState,
                columns = GridCells.Fixed(5),
                verticalArrangement = Arrangement.spacedBy(20.dp),
                horizontalArrangement = Arrangement.spacedBy(20.dp),
                contentPadding = PaddingValues(start = 12.dp, end = 12.dp, top = 12.dp, bottom = 12.dp),
            ) {
                itemsIndexed(sortedArtists) { index, artist ->
                    val focusModifier = if (index == focusIndex) Modifier.focusRequester(firstCardFocusRequester) else Modifier
                    val artwork = remember(artist, artworkUrl, artistPhotoUrl) {
                        artist.artworkUrls(artworkUrl, artistPhotoUrl)
                    }
                    Card(
                        onClick = { onOpenArtist(artist) },
                        colors = CardDefaults.colors(containerColor = SwarmSurface),
                        modifier = focusModifier.fillMaxWidth()
                            .testTag(UatTestTags.GRID_ARTIST_PREFIX + artist.artist),
                    ) {
                        ArtworkImage(
                            label = artist.artist,
                            placeholderType = "Artist",
                            primaryUrl = artwork.artistPhoto,
                            fallbackUrl = artwork.albumCoverFallback,
                            modifier = Modifier.fillMaxWidth().aspectRatio(1f).clip(RoundedCornerShape(8.dp)),
                        )
                    }
                }
            }
        }
    }
}
