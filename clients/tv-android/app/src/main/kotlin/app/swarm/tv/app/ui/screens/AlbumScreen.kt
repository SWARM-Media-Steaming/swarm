package app.swarm.tv.app.ui.screens

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
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
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.focus.onFocusChanged
import androidx.compose.ui.input.key.Key
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.tv.material3.Border
import androidx.tv.material3.Card
import androidx.tv.material3.CardDefaults
import app.swarm.tv.app.ui.UatTestTags
import app.swarm.tv.app.ui.theme.SwarmAccent
import app.swarm.tv.app.ui.theme.SwarmAccentHot
import app.swarm.tv.app.ui.theme.SwarmBorder
import app.swarm.tv.app.ui.theme.SwarmMuted
import app.swarm.tv.app.ui.theme.SwarmSurface
import app.swarm.tv.app.ui.theme.SwarmSurfaceMuted
import app.swarm.tv.app.ui.theme.SwarmText
import app.swarm.tv.core.catalog.AlbumGroup
import app.swarm.tv.core.catalog.ArtistGroup
import app.swarm.tv.core.catalog.MergedEntry

/**
 * The album named [albumKey] within [artist], or null. Used to reopen this
 * screen on a specific album's tracks — after Back from the music
 * player (#160) — with the current track in focus.
 */
fun albumForKey(artist: ArtistGroup, albumKey: String?): AlbumGroup? {
    if (albumKey == null) return null
    return artist.albums.firstOrNull { it.album == albumKey }
}

@Composable
fun AlbumScreen(
    artist: ArtistGroup,
    artworkUrl: (MergedEntry) -> String?,
    onPlay: (MergedEntry) -> Unit,
    onBack: () -> Unit,
    initialAlbumKey: String? = null,
    activeTrackKey: String? = null,
) {
    val initialAlbum = albumForKey(artist, initialAlbumKey)
    var highlightedAlbumKey by remember(artist.artist, initialAlbumKey) {
        mutableStateOf((initialAlbum ?: artist.albums.firstOrNull())?.album)
    }
    var inTracks by remember(artist.artist, initialAlbumKey) {
        mutableStateOf(initialAlbum?.tracks?.isNotEmpty() == true)
    }
    var lastFocusedTrackKey by remember(artist.artist, initialAlbumKey) {
        mutableStateOf(activeTrackKey)
    }
    val albumIndex = artist.albums.indexOfFirst { it.album == highlightedAlbumKey }.coerceAtLeast(0)
    val album = artist.albums.getOrNull(albumIndex)
    var albumPaneFocused by remember { mutableStateOf(false) }
    val albumFocusRequester = remember { FocusRequester() }
    val albumListState = rememberLazyListState(initialFirstVisibleItemIndex = albumIndex)

    BackHandler { if (inTracks) inTracks = false else onBack() }
    LaunchedEffect(inTracks, artist.artist, album != null) {
        if (!inTracks && !albumPaneFocused && album != null) {
            albumListState.focusBrowseItem(albumIndex, albumFocusRequester)
        }
    }
    LaunchedEffect(inTracks, album?.tracks?.isEmpty()) {
        if (inTracks && album?.tracks.isNullOrEmpty()) inTracks = false
    }

    Row(
        modifier = Modifier.fillMaxSize().padding(horizontal = 40.dp, vertical = 28.dp),
        horizontalArrangement = Arrangement.spacedBy(28.dp),
    ) {
        Column(
            modifier = Modifier.width(300.dp).fillMaxHeight()
                .onFocusChanged { state ->
                    albumPaneFocused = state.hasFocus
                    if (state.hasFocus) inTracks = false
                }
                .browsePaneFocusGroup(),
        ) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                ArtworkImage(
                    label = album?.album ?: artist.artist,
                    placeholderType = "Album",
                    primaryUrl = album?.coverArtworkUrl(artworkUrl),
                    modifier = Modifier.width(72.dp).aspectRatio(1f).clip(RoundedCornerShape(4.dp)),
                )
                Spacer(Modifier.width(14.dp))
                Column {
                    Text(
                        artist.artist,
                        color = SwarmText,
                        fontSize = 22.sp,
                        fontWeight = FontWeight.Black,
                        maxLines = 2,
                        overflow = TextOverflow.Ellipsis,
                    )
                    Spacer(Modifier.height(4.dp))
                    val count = artist.albums.size
                    Text("$count album" + if (count == 1) "" else "s", color = SwarmMuted, fontSize = 12.sp)
                }
            }
            Spacer(Modifier.height(18.dp))
            LazyColumn(
                state = albumListState,
                modifier = Modifier.fillMaxWidth().weight(1f),
                verticalArrangement = Arrangement.spacedBy(6.dp),
                contentPadding = PaddingValues(vertical = 4.dp),
            ) {
                itemsIndexed(
                    items = artist.albums,
                    key = { _, item -> item.album },
                    contentType = { _, _ -> "album" },
                ) { index, item ->
                    val onOpen = {
                        highlightedAlbumKey = item.album
                        if (item.tracks.isNotEmpty()) inTracks = true
                    }
                    val isDisplayed = index == albumIndex
                    val focusModifier = if (isDisplayed) Modifier.focusRequester(albumFocusRequester) else Modifier
                    BrowseGroupRow(
                        title = item.album,
                        count = trackCount(item.tracks.size),
                        isDisplayed = isDisplayed,
                        onFocused = {
                            if (highlightedAlbumKey != item.album) lastFocusedTrackKey = null
                            highlightedAlbumKey = item.album
                        },
                        onClick = onOpen,
                        modifier = focusModifier
                            .onPaneDirection(Key.DirectionRight, onOpen)
                            .testTag(UatTestTags.ALBUM_CARD_PREFIX + item.album),
                    )
                }
            }
        }
        if (album != null) key(album.album) {
            TrackList(
                album = album,
                onPlay = onPlay,
                inTracks = inTracks,
                focusTrackKey = lastFocusedTrackKey,
                onFocusTrack = { track ->
                    lastFocusedTrackKey = track.entry.entryKey
                    inTracks = true
                },
                modifier = Modifier.weight(1f).fillMaxHeight()
                    .onPaneDirection(Key.DirectionLeft) { inTracks = false },
            )
        }
    }
}

@Composable
private fun TrackList(
    album: AlbumGroup,
    onPlay: (MergedEntry) -> Unit,
    inTracks: Boolean,
    focusTrackKey: String?,
    onFocusTrack: (MergedEntry) -> Unit,
    modifier: Modifier = Modifier,
) {
    val focusIndex = album.tracks.indexOfFirst { it.entry.entryKey == focusTrackKey }.coerceAtLeast(0)
    val rowFocusRequester = remember { FocusRequester() }
    val listState = rememberLazyListState(initialFirstVisibleItemIndex = if (inTracks) focusIndex else 0)
    LaunchedEffect(inTracks) {
        if (inTracks && album.tracks.isNotEmpty()) listState.focusBrowseItem(focusIndex, rowFocusRequester)
    }

    Column(modifier = modifier.browsePaneFocusGroup()) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(
                album.album,
                color = SwarmText,
                fontSize = 20.sp,
                fontWeight = FontWeight.Bold,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.weight(1f, fill = false),
            )
            Spacer(Modifier.width(10.dp))
            Text(trackCount(album.tracks.size), color = SwarmMuted, fontSize = 13.sp, maxLines = 1)
        }
        Spacer(Modifier.height(12.dp))
        LazyColumn(
            state = listState,
            modifier = Modifier.fillMaxWidth().weight(1f),
            verticalArrangement = Arrangement.spacedBy(8.dp),
            contentPadding = PaddingValues(vertical = 4.dp),
        ) {
            itemsIndexed(
                items = album.tracks,
                key = { _, track -> track.fingerprint },
                contentType = { _, _ -> "track" },
            ) { index, track ->
                val focusModifier = if (index == focusIndex) Modifier.focusRequester(rowFocusRequester) else Modifier
                TrackRow(
                    track,
                    focusModifier.onFocusChanged { if (it.isFocused) onFocusTrack(track) },
                    onClick = { onPlay(track) },
                )
            }
        }
    }
}

private fun trackCount(count: Int): String = "$count track" + if (count == 1) "" else "s"

@Composable
private fun TrackRow(track: MergedEntry, focusModifier: Modifier, onClick: () -> Unit) {
    val rowShape = RoundedCornerShape(8.dp)
    Card(
        onClick = onClick,
        colors = CardDefaults.colors(containerColor = SwarmSurface, focusedContainerColor = SwarmSurfaceMuted),
        scale = CardDefaults.scale(scale = 1f, focusedScale = 1f, pressedScale = 0.99f),
        border = CardDefaults.border(
            border = Border(BorderStroke(1.dp, SwarmBorder), shape = rowShape),
            focusedBorder = Border(BorderStroke(2.dp, SwarmAccent), shape = rowShape),
            pressedBorder = Border(BorderStroke(2.dp, SwarmAccentHot), shape = rowShape),
        ),
        shape = CardDefaults.shape(rowShape, rowShape, rowShape),
        modifier = focusModifier.fillMaxWidth().testTag(UatTestTags.TRACK_ROW_PREFIX + track.entry.entryKey),
    ) {
        Row(modifier = Modifier.fillMaxWidth().padding(14.dp), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(14.dp)) {
            Text(track.entry.trackNumber?.toString() ?: "–", color = SwarmMuted, fontSize = 14.sp, modifier = Modifier.width(24.dp))
            Text(track.entry.scrapedTitle ?: track.entry.title, color = SwarmText, fontSize = 14.sp, fontWeight = FontWeight.SemiBold, maxLines = 1, overflow = TextOverflow.Ellipsis, modifier = Modifier.weight(1f))
            track.entry.durationSecs?.let { secs ->
                val minutes = (secs / 60).toInt()
                val seconds = (secs % 60).toInt()
                Text("%d:%02d".format(minutes, seconds), color = SwarmMuted, fontSize = 12.sp)
            }
        }
    }
}
