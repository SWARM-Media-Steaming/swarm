package app.swarm.tv.app.ui.screens

import android.net.Uri
import android.util.Log
import android.view.LayoutInflater
import android.view.ViewGroup
import androidx.annotation.OptIn
import androidx.compose.animation.core.animateDpAsState
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxScope
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.size
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.Stable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.focus.focusProperties
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.media3.common.MediaItem
import androidx.media3.common.Player
import androidx.media3.common.util.UnstableApi
import androidx.media3.exoplayer.ExoPlayer
import androidx.media3.exoplayer.upstream.DefaultBandwidthMeter
import androidx.media3.ui.PlayerView
import androidx.compose.material3.CircularProgressIndicator
import app.swarm.tv.R
import app.swarm.tv.app.PausePlayerWhenAppBackgrounded
import app.swarm.tv.app.data.BrowsePreview
import app.swarm.tv.app.ui.UatTestTags
import app.swarm.tv.app.ui.theme.SwarmAccentHot
import app.swarm.tv.core.catalog.MergedEntry
import kotlinx.coroutines.delay

private const val PREVIEW_PLAY_TIME_MS = 30_000L
private const val PREVIEW_VISIBLE_STARTUP_TIMEOUT_MS = 20_000L

/** Focus dwell before a hover preview begins warming its stream, then a
 * second dwell before the card actually swaps box art for the playing
 * preview. Shared by [CatalogScreen]'s browse rows and the "Browse All"
 * grids (#159) so both feel identical. */
private const val BROWSE_PREVIEW_WARMUP_MS = 2_000L
private const val BROWSE_PREVIEW_EXPAND_MS = 2_000L
internal const val BROWSE_ALL_GRID_COLUMNS = 5

/** Resting poster shape vs. the widescreen shape a hover preview animates
 * into, shared by the "Browse All" grids (#269). */
private const val BROWSE_PREVIEW_POSTER_ASPECT_RATIO = 2f / 3f
private const val BROWSE_PREVIEW_VIDEO_ASPECT_RATIO = 16f / 9f

/**
 * The per-screen state machine behind hover previews, extracted from
 * [CatalogScreen] so [MovieShelfScreen]/[ShowShelfScreen] can drive the same
 * two-stage warm-up/expand flow against the same [BrowsePreview] the
 * ViewModel negotiates (#159). Recreated cheaply on every recomposition; the
 * actual state lives in the [rememberBrowsePreviewCoordinator] `remember`s.
 */
@Stable
internal class BrowsePreviewCoordinator(
    /** Entry key of the card currently showing (or about to show) its
     * expanded inline preview, or null while nothing is expanded. */
    val expandedPreviewEntryKey: String?,
    /** A browse card calls this from its own `onFocusChanged`. */
    val onPreviewFocusChanged: (MergedEntry, Boolean) -> Unit,
    /** Forwarded to [BrowsePreviewPlayer.onFinished]; also collapses the card. */
    val onPreviewFinished: (String) -> Unit,
)

@Composable
internal fun rememberBrowsePreviewCoordinator(
    preview: BrowsePreview?,
    onStartPreview: (MergedEntry) -> Unit,
    onStopPreview: () -> Unit,
    onPreviewFinished: (String) -> Unit,
): BrowsePreviewCoordinator {
    var focusedPreviewEntry by remember { mutableStateOf<MergedEntry?>(null) }
    var previewExpansionEligibleEntryKey by remember { mutableStateOf<String?>(null) }
    var expandedPreviewEntryKey by remember { mutableStateOf<String?>(null) }

    // Warm the stream halfway through the dwell, but keep the card at poster
    // width/art and the player hidden until the full dwell has elapsed.
    // Moving focus cancels both stages and releases any session already made.
    LaunchedEffect(focusedPreviewEntry?.entry?.entryKey) {
        onStopPreview()
        previewExpansionEligibleEntryKey = null
        expandedPreviewEntryKey = null
        val entry = focusedPreviewEntry ?: return@LaunchedEffect
        delay(BROWSE_PREVIEW_WARMUP_MS)
        onStartPreview(entry)
        delay(BROWSE_PREVIEW_EXPAND_MS)
        previewExpansionEligibleEntryKey = entry.entry.entryKey
    }

    // Expand only after both dwell and negotiation have completed. Failed or
    // stalled negotiation therefore leaves the poster visible rather than
    // turning the card into an unbounded loading indicator.
    LaunchedEffect(previewExpansionEligibleEntryKey, preview?.entryKey) {
        expandedPreviewEntryKey = previewExpansionEntryKey(
            previewExpansionEligibleEntryKey,
            preview?.entryKey,
        )
    }
    DisposableEffect(Unit) {
        onDispose { onStopPreview() }
    }

    return BrowsePreviewCoordinator(
        expandedPreviewEntryKey = expandedPreviewEntryKey,
        onPreviewFocusChanged = { entry, focused ->
            if (focused) {
                focusedPreviewEntry = entry
            } else if (focusedPreviewEntry?.entry?.entryKey == entry.entry.entryKey) {
                focusedPreviewEntry = null
            }
        },
        onPreviewFinished = { sessionId ->
            if (preview?.sessionId == sessionId && preview.entryKey == expandedPreviewEntryKey) {
                expandedPreviewEntryKey = null
            }
            onPreviewFinished(sessionId)
        },
    )
}

/** Animates a grid card's width while keeping its resting poster height
 * fixed, matching [CatalogScreen]'s 2:3-to-16:9 preview expansion. The grid
 * item retains its original slot size; callers let the focused card draw
 * over adjacent slots until the preview finishes or loses focus. */
@Composable
internal fun rememberBrowsePreviewWidth(posterWidth: Dp, isExpanded: Boolean): Dp {
    val expandedWidth = browsePreviewExpandedWidth(posterWidth)
    val width by animateDpAsState(
        if (isExpanded) expandedWidth else posterWidth,
        label = "browse-grid-preview-width",
    )
    return width
}

internal fun browsePreviewExpandedWidth(posterWidth: Dp): Dp =
    posterWidth / BROWSE_PREVIEW_POSTER_ASPECT_RATIO * BROWSE_PREVIEW_VIDEO_ASPECT_RATIO

/** Keeps the wider card inside the grid's horizontal bounds at either edge,
 * while letting middle-column cards expand around their original center. */
internal fun browsePreviewAlignment(
    index: Int,
    columns: Int = BROWSE_ALL_GRID_COLUMNS,
): Alignment.Horizontal = when (index % columns) {
    0, 1 -> Alignment.Start
    columns - 2, columns - 1 -> Alignment.End
    else -> Alignment.CenterHorizontally
}

/**
 * Poster-box preview layer for the "Browse All" full grids (#159): the
 * loading indicator while a focused card's stream negotiates, then the
 * inline video once it is ready. Pair with [rememberBrowsePreviewWidth]
 * on the hosting card so the video plays widescreen rather than
 * zoom-cropped inside the resting poster bounds. Call from inside the
 * card's artwork [Box], over the [ArtworkImage].
 */
@Composable
internal fun BoxScope.BrowsePreviewGridOverlay(
    entryKey: String,
    isFocused: Boolean,
    isExpanded: Boolean,
    preview: BrowsePreview?,
    onFinished: (String) -> Unit,
    hasVideo: Boolean = true,
) {
    val activePreview = preview?.takeIf { isFocused && it.entryKey == entryKey }
    if (isExpanded && activePreview == null) {
        PreviewLoadingIndicator(Modifier.matchParentSize())
    }
    activePreview?.let {
        BrowsePreviewPlayer(
            preview = it,
            shouldPlay = isExpanded,
            onFinished = onFinished,
            modifier = Modifier.matchParentSize(),
            hasVideo = hasVideo,
        )
    }
}

@Composable
internal fun PreviewLoadingIndicator(modifier: Modifier = Modifier) {
    Box(
        modifier = modifier.background(Color.Black).focusProperties { canFocus = false },
        contentAlignment = Alignment.Center,
    ) {
        CircularProgressIndicator(
            color = SwarmAccentHot,
            strokeWidth = 5.dp,
            modifier = Modifier.size(48.dp),
        )
    }
}

/** Inline video preview with its original audio. `Player.stop()` cancels every media request after the
 * preview window; the catalog then fades this layer away and collapses the
 * still-focused card back to its box art.
 *
 * [hasVideo] `false` is the music-track case: there is no video track, so
 * Media3's `onRenderedFirstFrame` never fires and the loading spinner used
 * to spin forever even though audio was already playing fine — real bug,
 * found live. Rather than wait on a signal that never arrives, an
 * audio-only preview skips the opaque black backing and spinner entirely
 * and lets the card's own album-cover artwork (already composed underneath
 * this layer at every call site) show through instead. */
@OptIn(UnstableApi::class)
@Composable
internal fun BrowsePreviewPlayer(
    preview: BrowsePreview,
    shouldPlay: Boolean,
    onFinished: (sessionId: String) -> Unit,
    modifier: Modifier = Modifier,
    hasVideo: Boolean = true,
) {
    val context = LocalContext.current
    var renderedFirstFrame by remember(preview.sessionId) { mutableStateOf(false) }
    var finished by remember(preview.sessionId) { mutableStateOf(preview.released) }
    var failedBeforeFirstFrame by remember(preview.sessionId) { mutableStateOf(false) }
    val player = remember(preview.sessionId) {
        val bandwidthMeter = DefaultBandwidthMeter.Builder(context)
            .setInitialBitrateEstimate(preview.maxBitrate.coerceIn(250_000L, 2_000_000L))
            .build()
        ExoPlayer.Builder(context).setBandwidthMeter(bandwidthMeter).build().apply {
            // Keep direct/HLS preview selection aligned with full playback.
            // The server already maps English into transcoded previews; this
            // also covers any multi-track source exposed directly to Media3.
            trackSelectionParameters = trackSelectionParameters
                .buildUpon()
                .setPreferredAudioLanguages("en", "eng")
                .build()
            setMediaItem(MediaItem.fromUri(Uri.parse(preview.url)))
            if (preview.seekPositionSecs > 0L) seekTo(preview.seekPositionSecs * 1000L)
            // Prepare immediately so the first frame can buffer during the
            // second half of the focus dwell, but do not produce audio or
            // advance video until the card expands.
            playWhenReady = shouldPlay && !preview.released
            prepare()
        }
    }
    PausePlayerWhenAppBackgrounded(player)

    LaunchedEffect(player, shouldPlay, finished) {
        player.playWhenReady = shouldPlay && !finished
    }

    fun freezeAndRelease(reason: String) {
        if (finished) return
        finished = true
        player.pause()
        // Cancels buffering/the active HTTP request. PlayerView below keeps
        // the already-rendered frame on its surface after this reset.
        player.stop()
        Log.i("BrowsePreview", "Stopped ${preview.sessionId}: $reason")
        onFinished(preview.sessionId)
    }

    DisposableEffect(player) {
        val listener = object : Player.Listener {
            override fun onRenderedFirstFrame() {
                renderedFirstFrame = true
                Log.i("BrowsePreview", "First frame rendered for ${preview.sessionId}")
            }

            override fun onPlaybackStateChanged(playbackState: Int) {
                if (playbackState == Player.STATE_ENDED) freezeAndRelease("media ended")
            }

            override fun onPlayerError(error: androidx.media3.common.PlaybackException) {
                failedBeforeFirstFrame = !renderedFirstFrame
                Log.w("BrowsePreview", "Playback failed for ${preview.sessionId}", error)
                freezeAndRelease("playback error")
            }
        }
        player.addListener(listener)
        onDispose {
            player.removeListener(listener)
            player.release()
        }
    }

    // Buffering time does not consume the requested 30-second preview; the
    // clock begins only after the first frame is actually visible.
    LaunchedEffect(player, renderedFirstFrame, shouldPlay) {
        if (!renderedFirstFrame || !shouldPlay || finished) return@LaunchedEffect
        delay(PREVIEW_PLAY_TIME_MS)
        freezeAndRelease("30-second window complete")
    }

    // Media3 can remain BUFFERING without raising onPlayerError when a proxy
    // response stalls. Give up and fall back to the card's own artwork the
    // same way a real playback error does, rather than leaving the spinner —
    // or, once `finished` suppresses it, a black rectangle — up for good.
    LaunchedEffect(player, renderedFirstFrame, shouldPlay, finished) {
        if (renderedFirstFrame || !shouldPlay || finished || !hasVideo) return@LaunchedEffect
        delay(PREVIEW_VISIBLE_STARTUP_TIMEOUT_MS)
        if (!renderedFirstFrame && !finished) {
            failedBeforeFirstFrame = true
            freezeAndRelease("first-frame timeout")
        }
    }

    // If playback failed before producing a frame, leave the transparent
    // overlay empty so the card's artwork remains visible instead of replacing
    // it with a black rectangle. A successfully rendered final frame remains.
    if (failedBeforeFirstFrame) return

    // Audio-only: the player above is already driving sound, but there is no
    // video surface or first frame to wait for, so render nothing and let
    // the card's own album-cover artwork underneath keep showing.
    if (!hasVideo) return

    Box(
        modifier = modifier
            .background(Color.Black)
            .focusProperties { canFocus = false }
            .testTag(UatTestTags.BROWSE_PREVIEW_PREFIX + preview.sessionId),
    ) {
        AndroidView(
            modifier = Modifier.fillMaxSize().focusProperties { canFocus = false },
            factory = { ctx ->
                // PlayerView's default SurfaceView is a separate Android
                // window layer and can sit behind the Compose card on Fire
                // TV, producing audio over a blank card. This layout opts
                // into TextureView so the video is composed and clipped with
                // the rest of the card.
                (LayoutInflater.from(ctx).inflate(R.layout.browse_preview_player, null) as PlayerView).apply {
                    isFocusable = false
                    isFocusableInTouchMode = false
                    descendantFocusability = ViewGroup.FOCUS_BLOCK_DESCENDANTS
                    this.player = player
                }
            },
            update = { it.player = player },
        )
        if (!renderedFirstFrame && !finished) {
            PreviewLoadingIndicator(Modifier.fillMaxSize())
        }
    }
}

internal fun previewExpansionEntryKey(eligibleEntryKey: String?, previewEntryKey: String?): String? =
    eligibleEntryKey?.takeIf { it == previewEntryKey }
