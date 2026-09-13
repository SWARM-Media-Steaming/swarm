/**
 * Plays [url] — a negotiated budgeted-direct or HLS URL from
 * [app.swarm.tv.core.catalog.CatalogSession.preparePlayback] — through a
 * standard Media3 `ExoPlayer`/`PlayerView`. ExoPlayer's own HTTP data
 * source needs nothing more exotic than what
 * [app.swarm.tv.core.proxy.PeerLoopbackProxy] already speaks (proven with a
 * plain OkHttp client in `PeerQuicClientInteropTest`), so this screen is
 * mechanical: point it at the local proxy URL like any other HTTP stream.
 *
 * Resume/watched state: seeks to [resumePositionSecs] on prepare, reports
 * position back via [onPositionUpdate] periodically while playing, and
 * again on final disposal (back pressed, or navigated away from) — this
 * screen itself knows nothing about persistence, `SwarmViewModel` owns
 * deciding what to do with that report. The periodic save exists because
 * disposal never fires if the process dies outright — e.g. a TV switched
 * off mid-stream (#77) — so without it the resume point would silently
 * stay wherever it was at the last normal exit instead of near the true
 * last-watched position.
 *
 * Continue/autoplay: when [hasNext] is true, the same "Up next" overlay can
 * appear two ways (#80). If the episode's IntroDB markers include a CREDITS
 * segment, the overlay pops as soon as playback enters it — mirroring how
 * streaming apps offer the next episode while credits roll instead of over a
 * dead black frame — and Cancel/Back there just dismisses the overlay so
 * credits keep playing underneath. Otherwise (no credits marker, or the
 * viewer already dismissed that early offer) it falls back to appearing when
 * playback reaches `Player.STATE_ENDED` naturally, where Cancel/Back instead
 * exits through [onBack] since there is nothing left to keep watching. Either
 * way, "Play now" behaves the same. Manual-exit resume/position-save is
 * untouched by any of this — [onDispose] always fires the same report
 * regardless of which path led to disposal.
 */
package app.swarm.tv.app.ui.screens

import android.content.Context
import android.content.res.ColorStateList
import android.media.audiofx.LoudnessEnhancer
import android.net.Uri
import android.view.KeyEvent
import android.view.View
import android.view.ViewGroup
import android.widget.ImageButton
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.background
import androidx.compose.foundation.focusable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
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
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.toArgb
import androidx.compose.ui.input.key.onKeyEvent
import androidx.compose.ui.input.key.onPreviewKeyEvent
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.media3.common.C
import androidx.media3.common.Format
import androidx.media3.common.ForwardingPlayer
import androidx.media3.common.MediaItem
import androidx.media3.common.MimeTypes
import androidx.media3.common.PlaybackException
import androidx.media3.common.Player
import androidx.media3.common.Timeline
import androidx.media3.common.TrackGroup
import androidx.media3.common.Tracks
import androidx.media3.common.TrackSelectionOverride
import androidx.media3.common.util.UnstableApi
import androidx.media3.datasource.HttpDataSource
import androidx.media3.exoplayer.DecoderReuseEvaluation
import androidx.media3.exoplayer.DefaultLoadControl
import androidx.media3.exoplayer.ExoPlayer
import androidx.media3.exoplayer.analytics.AnalyticsListener
import androidx.media3.exoplayer.source.DefaultMediaSourceFactory
import androidx.media3.exoplayer.source.LoadEventInfo
import androidx.media3.exoplayer.source.MediaLoadData
import androidx.media3.exoplayer.trackselection.AdaptiveTrackSelection
import androidx.media3.exoplayer.trackselection.DefaultTrackSelector
import androidx.media3.exoplayer.upstream.DefaultBandwidthMeter
import androidx.media3.exoplayer.upstream.DefaultLoadErrorHandlingPolicy
import androidx.media3.exoplayer.upstream.LoadErrorHandlingPolicy.LoadErrorInfo
import androidx.media3.ui.PlayerView
import androidx.tv.material3.Button
import androidx.tv.material3.Card
import androidx.tv.material3.CardDefaults
import app.swarm.tv.app.PausePlayerWhenAppBackgrounded
import app.swarm.tv.app.data.episodeNumberLabel
import app.swarm.tv.app.data.pauseRecommendationTitle
import app.swarm.tv.app.data.PreparedEpisodePlayback
import app.swarm.tv.app.ui.UatTestTags
import app.swarm.tv.app.ui.components.SelectableChip
import app.swarm.tv.app.ui.components.swarmActionButtonColors
import app.swarm.tv.app.ui.theme.SwarmAccent
import app.swarm.tv.app.ui.theme.SwarmAccentHot
import app.swarm.tv.app.ui.theme.SwarmMuted
import app.swarm.tv.app.ui.theme.SwarmSurface
import app.swarm.tv.app.ui.theme.SwarmText
import app.swarm.tv.core.catalog.MergedEntry
import app.swarm.tv.core.catalog.displayTitle
import app.swarm.tv.core.peer.MediaKind
import app.swarm.tv.core.peer.PlaybackMode
import app.swarm.tv.core.peer.SkipSegment
import app.swarm.tv.core.peer.SkipSegmentKind
import app.swarm.tv.core.peer.SubtitleTrack
import java.io.EOFException
import java.io.IOException
import java.net.ProtocolException
import java.net.SocketException
import java.net.SocketTimeoutException
import java.util.Locale
import kotlinx.coroutines.delay

private const val CONTINUE_COUNTDOWN_SECS = 8
private const val SERVER_OFFLINE_RETRY_DELAY_MS = 2_000L
private const val SKIP_INTRO_OFFER_MS = 10_000L
private const val INTRO_POSITION_POLL_MS = 250L
private const val PERIODIC_POSITION_SAVE_MS = 15_000L
internal const val BUFFERING_QUALITY_RECOVERY_MS = 30_000L
internal const val STARTUP_BUFFERING_QUALITY_RECOVERY_MS = 4_000L
private const val BUFFERING_VIDEO_BITRATE_PERCENT = 80L

// A video wait shorter than this recovers before the viewer registers it, so
// the buffering toast stays hidden unless playback is still stalled after the
// delay. Cancelled the moment the player leaves its loading state.
internal const val BUFFERING_NOTIFICATION_DELAY_MS = 3_000L

/** D-pad left/right rewind/fast-forward step. Android's own key-repeat
 * mechanism redelivers ACTION_DOWN with an incrementing repeatCount while a
 * hardware button stays held, so consuming every one of those (not just the
 * first) is what makes holding the button keep seeking. */
internal const val PLAYBACK_SEEK_STEP_MS = 60_000L

/** Returns the [kind] IntroDB marker covering [positionMs], using IntroDB's
 * null-start convention for a segment that begins with the episode.
 * Open-ended segments (no [SkipSegment.endMs]) cannot provide a skip target
 * or a reliable "still in this section" bound, so they're ignored — same
 * reasoning [activeIntroSegment] always applied, generalized to any kind. */
internal fun activeSkipSegment(segments: List<SkipSegment>, positionMs: Long, kind: SkipSegmentKind): SkipSegment? =
    segments.firstOrNull { segment ->
        if (segment.kind != kind) return@firstOrNull false
        val start = segment.startMs ?: 0L
        val end = segment.endMs ?: return@firstOrNull false
        start >= 0L && end > start && positionMs >= start && positionMs < end
    }

internal fun activeIntroSegment(segments: List<SkipSegment>, positionMs: Long): SkipSegment? =
    activeSkipSegment(segments, positionMs, SkipSegmentKind.INTRO)

/**
 * Next value for the on-surface "Skip intro" offer, given the live playhead.
 *
 * Deliberately keyed only on the INTRO marker under [positionMs] — NOT on
 * transient buffering or overlay state (#102). When the next episode is
 * started from the pause screen there is no preloaded, pre-buffered player,
 * so its opening seconds are spent rebuffering over the peer proxy; folding
 * `isLoading` into this decision let each of those stalls null out a valid
 * offer, and for a short intro that begins at 0:00 (Aqua Teen Hunger Force
 * being the repro) the offer could churn away before it was ever pressable.
 * The pause/continue overlays still hide the button at the call site.
 *
 * Returns [current] unchanged while the active marker is the one the viewer
 * already dismissed, so a click or timeout stays dismissed until the
 * playhead reaches a different marker.
 */
internal fun nextIntroOffer(
    segments: List<SkipSegment>,
    positionMs: Long,
    isEpisode: Boolean,
    current: SkipSegment?,
    dismissed: SkipSegment?,
): SkipSegment? {
    if (!isEpisode) return null
    val active = activeSkipSegment(segments, positionMs, SkipSegmentKind.INTRO) ?: return null
    return if (active == dismissed) current else active
}

// Real complaint from live use: even at max system/TV volume, some content isn't
// loud enough. LoudnessEnhancer processes the decoded PCM before it reaches the
// audio sink, so it can boost past what raising system volume alone can reach.
// 1000 millibels (10dB) is a noticeable default lift chosen to stay clear of the
// audible clipping/distortion louder passages start to show past ~15-20dB of gain.
private const val AUDIO_BOOST_MILLIBELS = 1000

internal enum class RemotePlaybackAction {
    TOGGLE_PLAY_PAUSE,
    PLAY,
    PAUSE,
    SEEK_FORWARD,
    SEEK_BACK,
    SHOW_CONTROLS,
}

internal enum class PlaybackBackAction {
    EXIT,
    HIDE_CONTROLS,
    PAUSE,
    DISMISS_CONTINUE_PROMPT,
}

/** Fire TV remotes send these as activity key events, rather than invoking
 * Media3 commands out of band. Keep the mapping independent of focus so the
 * transport buttons still work while a Compose pause/continue overlay owns
 * focus instead of the embedded [PlayerView]. */
internal fun remotePlaybackAction(keyCode: Int): RemotePlaybackAction? = when (keyCode) {
    KeyEvent.KEYCODE_MEDIA_PLAY_PAUSE,
    KeyEvent.KEYCODE_HEADSETHOOK,
    -> RemotePlaybackAction.TOGGLE_PLAY_PAUSE
    KeyEvent.KEYCODE_MEDIA_PLAY -> RemotePlaybackAction.PLAY
    KeyEvent.KEYCODE_MEDIA_PAUSE -> RemotePlaybackAction.PAUSE
    KeyEvent.KEYCODE_MEDIA_FAST_FORWARD,
    KeyEvent.KEYCODE_MEDIA_SKIP_FORWARD,
    -> RemotePlaybackAction.SEEK_FORWARD
    KeyEvent.KEYCODE_MEDIA_REWIND,
    KeyEvent.KEYCODE_MEDIA_SKIP_BACKWARD,
    -> RemotePlaybackAction.SEEK_BACK
    KeyEvent.KEYCODE_MENU,
    KeyEvent.KEYCODE_SETTINGS,
    KeyEvent.KEYCODE_MEDIA_TOP_MENU,
    -> RemotePlaybackAction.SHOW_CONTROLS
    else -> null
}

/** D-pad shortcuts belong to the bare playing video surface. Once playback
 * is paused, Compose owns the keys for pause-screen navigation; once Media3's
 * controller is visible, it owns them for moving between and activating its
 * controls. */
internal fun videoSurfacePlaybackAction(
    keyCode: Int,
    playWhenReady: Boolean,
    controlsVisible: Boolean,
    // True while an on-surface Compose button (currently the "Skip intro"
    // offer) holds D-pad focus. When a button is focused, Select must click
    // that button and the D-pad must interact with it, never pause or seek
    // the video underneath it (#94).
    surfaceButtonFocused: Boolean = false,
): RemotePlaybackAction? {
    if (!playWhenReady || controlsVisible || surfaceButtonFocused) return null
    return when (keyCode) {
        KeyEvent.KEYCODE_DPAD_LEFT -> RemotePlaybackAction.SEEK_BACK
        KeyEvent.KEYCODE_DPAD_RIGHT -> RemotePlaybackAction.SEEK_FORWARD
        KeyEvent.KEYCODE_DPAD_CENTER,
        KeyEvent.KEYCODE_ENTER,
        KeyEvent.KEYCODE_NUMPAD_ENTER,
        -> RemotePlaybackAction.PAUSE
        else -> null
    }
}

internal fun playbackBackAction(
    showPauseOverlay: Boolean,
    showContinuePrompt: Boolean,
    controlsVisible: Boolean,
    // True only while the current continue prompt was triggered by an
    // in-progress CREDITS marker rather than STATE_ENDED (#80) — playback is
    // still going underneath it, so Back should dismiss the offer and keep
    // watching rather than exit.
    continuePromptDismissable: Boolean = false,
): PlaybackBackAction = when {
    showContinuePrompt && continuePromptDismissable -> PlaybackBackAction.DISMISS_CONTINUE_PROMPT
    showPauseOverlay || showContinuePrompt -> PlaybackBackAction.EXIT
    controlsVisible -> PlaybackBackAction.HIDE_CONTROLS
    else -> PlaybackBackAction.PAUSE
}

/** Media3's `PlayerControlView` parks D-pad focus on one of its own transport
 * buttons and never hands it back when the controller hides — whether the
 * viewer dismissed it or it simply timed out on its own after being shown
 * mid-playback or during an episode autoplay handoff. Those buttons live
 * outside the Compose focus tree, so the surface's `onPreviewKeyEvent` stops
 * seeing Select and the remote looks dead until Back moves focus off the
 * now-invisible controller. Reclaim focus for the bare video surface whenever
 * the controller goes away and no Compose overlay legitimately owns it (#154). */
internal fun shouldReclaimSurfaceFocusOnControllerHidden(
    controllerVisible: Boolean,
    overlayOwnsFocus: Boolean,
): Boolean = !controllerVisible && !overlayOwnsFocus

/** A post-start HLS stall means the selected rendition has outrun the
 * connection. Direct play has no alternate rendition, and pausing must never
 * look like a bandwidth problem. */
internal fun shouldStartBufferingQualityRecovery(
    playbackState: Int,
    hasStartedPlayback: Boolean,
    playWhenReady: Boolean,
    playbackMode: PlaybackMode,
    hasVideo: Boolean,
): Boolean = playbackState == Player.STATE_BUFFERING &&
    hasStartedPlayback &&
    playWhenReady &&
    playbackMode == PlaybackMode.HLS &&
    hasVideo

/** Startup gets a short grace period before applying the same conservative
 * HLS ceiling. Unlike mid-playback recovery there may not be a selected video
 * format yet, so the negotiated ceiling is used as the reference bitrate. */
internal fun shouldStartStartupQualityRecovery(
    playbackState: Int,
    hasStartedPlayback: Boolean,
    playWhenReady: Boolean,
    playbackMode: PlaybackMode,
): Boolean = playbackState == Player.STATE_BUFFERING &&
    !hasStartedPlayback &&
    playWhenReady &&
    playbackMode == PlaybackMode.HLS

/** Caps video at 80% of the currently selected rendition, which forces
 * Media3 onto the next viable HLS rung. If Media3 has not exposed that
 * rendition's bitrate yet, the negotiated ceiling is the conservative
 * fallback. An existing recovery cap can only become tighter. */
internal fun bufferingRecoveryVideoBitrateCap(
    selectedVideoBitrate: Int?,
    negotiatedMaxBitrate: Long,
    currentMaxVideoBitrate: Int,
): Int? {
    val referenceBitrate = selectedVideoBitrate
        ?.takeIf { it > 0 }
        ?.toLong()
        ?: negotiatedMaxBitrate.takeIf { it > 0 }
        ?: return null
    val reducedBitrate = (referenceBitrate * BUFFERING_VIDEO_BITRATE_PERCENT / 100L)
        .coerceIn(1L, Int.MAX_VALUE.toLong())
        .toInt()
    return minOf(reducedBitrate, currentMaxVideoBitrate)
}

private data class PlaybackPlayerConfig(
    val sessionId: String,
    val url: String,
    val title: String,
    val maxBitrate: Long,
    val subtitles: List<SubtitleTrack>,
    val resumePositionSecs: Double,
)

private fun PreparedEpisodePlayback.toPlayerConfig() = PlaybackPlayerConfig(
    sessionId = sessionId,
    url = url,
    title = title,
    maxBitrate = maxBitrate,
    subtitles = subtitles,
    resumePositionSecs = resumePositionSecs,
)

/** Media3 normally gives up after a small number of failed loads. A server
 * outage is different from a bad asset or decoder failure: keep requesting
 * the current Range/HLS segment so already-buffered media can play out and
 * playback can resume without user intervention when the route returns. */
private class ServerOfflineRetryPolicy : DefaultLoadErrorHandlingPolicy(Int.MAX_VALUE) {
    override fun getRetryDelayMsFor(loadErrorInfo: LoadErrorInfo): Long {
        val responseCode = playbackHttpResponseCode(loadErrorInfo.exception)
        return when {
            isServerOfflineLoadError(loadErrorInfo.exception) -> SERVER_OFFLINE_RETRY_DELAY_MS
            // A permanent HTTP response (especially the expired-session
            // 404 handled by onPlayerError) must not inherit the unlimited
            // transport retry count.
            responseCode != null -> C.TIME_UNSET
            else -> super.getRetryDelayMsFor(loadErrorInfo)
        }
    }
}

internal fun serverOfflineMediaSourceFactory(context: Context): DefaultMediaSourceFactory =
    DefaultMediaSourceFactory(context.applicationContext)
        .setLoadErrorHandlingPolicy(ServerOfflineRetryPolicy())

internal fun isServerOfflineHttpStatus(responseCode: Int): Boolean =
    responseCode == 500 || responseCode == 502 || responseCode == 503 || responseCode == 504

internal fun playbackHttpResponseCode(error: Throwable): Int? =
    generateSequence<Throwable>(error) { it.cause }
        .filterIsInstance<HttpDataSource.InvalidResponseCodeException>()
        .firstOrNull()
        ?.responseCode

/** Intentionally narrow: codec/parser failures and permanent 4xx asset
 * errors must still become terminal player errors instead of retry loops. */
internal fun isServerOfflineLoadError(error: IOException): Boolean =
    generateSequence<Throwable>(error) { it.cause }.any { cause ->
        when (cause) {
            is HttpDataSource.InvalidResponseCodeException -> isServerOfflineHttpStatus(cause.responseCode)
            is SocketException,
            is SocketTimeoutException,
            is EOFException,
            is ProtocolException,
            -> true
            else -> false
        }
    }

/** Delays an offline report until a retryable load failure actually exhausts
 * the player's buffer. A single timed-out prefetch can recover without the
 * viewer noticing (and without the server ever going offline), so reporting
 * from AnalyticsListener.onLoadError alone produces false alarms. */
internal class PlaybackOutageTracker {
    private var pendingContext: String? = null
    private var reported = false

    val isPending: Boolean
        get() = pendingContext != null

    fun onLoadError(context: String, playbackState: Int): String? {
        if (pendingContext == null) pendingContext = context
        return reportIfBuffering(playbackState)
    }

    fun onPlaybackStateChanged(playbackState: Int): String? = reportIfBuffering(playbackState)

    fun onLoadCompleted() {
        pendingContext = null
        reported = false
    }

    private fun reportIfBuffering(playbackState: Int): String? {
        val context = pendingContext ?: return null
        if (reported || playbackState != Player.STATE_BUFFERING) return null
        reported = true
        return context
    }
}

internal fun playbackErrorContext(error: PlaybackException, player: Player): String {
    val causes = generateSequence<Throwable>(error) { it.cause }
        .take(6)
        .joinToString(" -> ") { cause ->
            val detail = cause.message?.takeIf { it.isNotBlank() }
            if (detail == null) cause.javaClass.simpleName else "${cause.javaClass.simpleName}: $detail"
        }
    return buildString {
        append("error_code=").append(error.errorCode)
        append("; error_code_name=").append(error.errorCodeName)
        append("; position_ms=").append(player.currentPosition)
        append("; buffered_position_ms=").append(player.bufferedPosition)
        append("; duration_ms=").append(player.duration)
        append("; playback_state=").append(player.playbackState)
        append("; play_when_ready=").append(player.playWhenReady)
        append("; causes=").append(causes)
    }
}

// Media3's own defaults (DEFAULT_BUFFER_FOR_PLAYBACK_MS = 2_500,
// DEFAULT_BUFFER_FOR_PLAYBACK_AFTER_REBUFFER_MS = 5_000) size these for a
// real internet path; see the loopback-source comment on createPlayer().
private const val LOOPBACK_BUFFER_FOR_PLAYBACK_MS = 500
private const val LOOPBACK_BUFFER_FOR_PLAYBACK_AFTER_REBUFFER_MS = 1_000

// Media3's own default (AdaptiveTrackSelection.DEFAULT_MIN_DURATION_FOR_QUALITY_INCREASE_MS
// = 10_000) let quality bounce back up after only ten seconds of recovered
// bandwidth, rapidly reselecting renditions whenever the estimate hovered
// near a rendition boundary (#67). Widening the hold to 30s only gates
// *increases* — a further downgrade is never delayed, since that path
// reacts to the current bandwidth estimate rather than this cooldown.
private const val QUALITY_INCREASE_HOLD_MS = 30_000

/** Owns the active video player plus at most one paused, buffering successor.
 * The pool lives across UiState.Player-to-UiState.Player recompositions, which
 * lets [activate] promote the exact preloaded ExoPlayer instead of throwing
 * away its buffer during the state handoff. */
@androidx.annotation.OptIn(UnstableApi::class)
private class VideoPlayerPool(context: Context) {
    private val appContext = context.applicationContext
    private var activeSessionId: String? = null
    private var activePlayer: ExoPlayer? = null
    private var preloadedSessionId: String? = null
    private var preloadedPlayer: ExoPlayer? = null

    fun activate(config: PlaybackPlayerConfig, startPaused: Boolean = false): ExoPlayer {
        if (activeSessionId == config.sessionId) return checkNotNull(activePlayer)

        var player = if (preloadedSessionId == config.sessionId) {
            preloadedSessionId = null
            preloadedPlayer.also { preloadedPlayer = null } ?: createPlayer(config, playWhenReady = !startPaused)
        } else {
            createPlayer(config, playWhenReady = !startPaused)
        }
        // A preload can fail while it has no UI listener. Recreate from the
        // still-valid negotiated URL on promotion so the active listener gets
        // a normal retry/error path instead of inheriting a silent terminal
        // player state.
        if (player.playerError != null) {
            player.release()
            player = createPlayer(config, playWhenReady = !startPaused)
        }
        activeSessionId = config.sessionId
        activePlayer = player
        player.playWhenReady = !startPaused
        return player
    }

    fun preload(config: PlaybackPlayerConfig) {
        if (activeSessionId == config.sessionId || preloadedSessionId == config.sessionId) return
        releasePreloaded()
        preloadedSessionId = config.sessionId
        preloadedPlayer = createPlayer(config, playWhenReady = false)
    }

    fun release(player: ExoPlayer) {
        if (activePlayer === player) {
            activePlayer = null
            activeSessionId = null
        }
        if (preloadedPlayer === player) {
            preloadedPlayer = null
            preloadedSessionId = null
        }
        player.release()
    }

    fun releasePreloaded() {
        preloadedPlayer?.release()
        preloadedPlayer = null
        preloadedSessionId = null
    }

    private fun createPlayer(config: PlaybackPlayerConfig, playWhenReady: Boolean): ExoPlayer {
        // The HTTP URL is loopback, so Media3's network-type-based initial
        // estimate describes the TV's Wi-Fi rather than the media server's
        // constrained uplink. Start conservatively; segment transfer samples
        // will replace this estimate as playback proceeds.
        val initialBitrate = config.maxBitrate.coerceIn(250_000L, 2_000_000L)
        val bandwidthMeter = DefaultBandwidthMeter.Builder(appContext)
            .setInitialBitrateEstimate(initialBitrate)
            .build()
        // Widen the quality-increase hold from Media3's 10s default to 30s
        // (see QUALITY_INCREASE_HOLD_MS) so a downgraded rendition sticks
        // for at least that long before ExoPlayer switches back up. Further
        // downgrades on continued/worsening bandwidth are unaffected.
        val trackSelector = DefaultTrackSelector(
            appContext,
            AdaptiveTrackSelection.Factory(
                QUALITY_INCREASE_HOLD_MS,
                AdaptiveTrackSelection.DEFAULT_MAX_DURATION_FOR_QUALITY_DECREASE_MS,
                AdaptiveTrackSelection.DEFAULT_MIN_DURATION_TO_RETAIN_AFTER_DISCARD_MS,
                AdaptiveTrackSelection.DEFAULT_BANDWIDTH_FRACTION,
            ),
        )
        // Same reasoning as the bandwidth estimate above: Media3's default
        // LoadControl targets a 2.5s buffer-for-playback because it assumes
        // an internet-latency source. Against a same-device loopback proxy
        // that's pure added time-to-first-frame, so start playback almost as
        // soon as the first segment lands; min/max buffer stay at Media3's
        // defaults since those bound steady-state memory use, not startup.
        val loadControl = DefaultLoadControl.Builder()
            .setBufferDurationsMs(
                DefaultLoadControl.DEFAULT_MIN_BUFFER_MS,
                DefaultLoadControl.DEFAULT_MAX_BUFFER_MS,
                LOOPBACK_BUFFER_FOR_PLAYBACK_MS,
                LOOPBACK_BUFFER_FOR_PLAYBACK_AFTER_REBUFFER_MS,
            )
            .build()
        return ExoPlayer.Builder(appContext)
            .setBandwidthMeter(bandwidthMeter)
            .setTrackSelector(trackSelector)
            .setLoadControl(loadControl)
            .setMediaSourceFactory(serverOfflineMediaSourceFactory(appContext))
            .build()
            .apply {
            // Direct-play containers may expose several embedded audio tracks.
            // Prefer English when it is tagged, while Media3 naturally falls
            // back to the container default when no English track exists.
            // setPreferredTextLanguages keeps that same English-first order
            // for whichever subtitle track the viewer later turns on, but by
            // itself it also makes DefaultTrackSelector auto-select a
            // matching text track — the exact "subtitles defaulted to ON"
            // complaint in #55 — so text tracks are additionally disabled by
            // type below and only re-enabled when the viewer explicitly
            // picks one via the pause-screen picker or native subtitle
            // control (see selectSubtitleTrack).
            trackSelectionParameters = trackSelectionParameters
                .buildUpon()
                .setPreferredAudioLanguages("en", "eng")
                .setPreferredTextLanguages("en", "eng")
                .setTrackTypeDisabled(C.TRACK_TYPE_TEXT, true)
                .build()
            // Real bug, found live (#55): Media3's subtitle button/settings
            // submenu (which is where the built-in "Off" option lives) stays
            // hidden unless explicitly requested, and every subtitle track
            // used to be flagged SELECTION_FLAG_DEFAULT, which left Media3's
            // tie-break picking an inconsistent language whenever more than
            // one subtitle track was attached. Subtitles now start disabled
            // (above); the viewer opts in per-track via the native
            // subtitle/settings control or the pause-screen picker instead of
            // one being forced on.
            val subtitleConfigurations = config.subtitles.map { track ->
                MediaItem.SubtitleConfiguration.Builder(Uri.parse(track.path))
                    .setMimeType(MimeTypes.TEXT_VTT)
                    .setLanguage(track.language)
                    .setLabel(track.label)
                    .build()
            }
            setMediaItem(
                MediaItem.Builder()
                    .setUri(Uri.parse(config.url))
                    .setMediaId(config.title)
                    .setSubtitleConfigurations(subtitleConfigurations)
                    .build(),
            )
            if (config.resumePositionSecs > 0) {
                seekTo((config.resumePositionSecs * 1000).toLong())
            }
            this.playWhenReady = playWhenReady
            prepare()
        }
    }
}

@androidx.annotation.OptIn(UnstableApi::class)
@Composable
fun PlayerScreen(
    sessionId: String,
    url: String,
    title: String,
    playbackMode: PlaybackMode,
    resumePositionSecs: Double,
    positionOffsetSecs: Double,
    mediaDurationSecs: Double?,
    maxBitrate: Long,
    subtitles: List<SubtitleTrack>,
    entry: MergedEntry,
    recommendations: List<MergedEntry>,
    artworkUrl: (MergedEntry) -> String?,
    hasNext: Boolean,
    nextTitle: String?,
    nextArtworkUrl: String?,
    preloadedNext: PreparedEpisodePlayback?,
    /** Opens straight into [PauseOverlay] instead of autoplaying — set for
     * "Continue Watching" taps, so the viewer sees where they left off (cast,
     * synopsis, "More like this") and presses Resume deliberately rather than
     * being dropped straight back into mid-scene audio/video. */
    startPaused: Boolean = false,
    onBack: () -> Unit,
    onPositionUpdate: (positionSecs: Double, durationSecs: Double) -> Unit,
    onContinue: () -> Unit,
    onPlayRecommendation: (MergedEntry) -> Unit,
    onPreloadNext: () -> Unit,
    onSeekOutsideBuffer: (positionSecs: Double) -> Unit,
    onPlaybackSessionExpired: (positionSecs: Double, context: String?) -> Unit,
    onServerOffline: (context: String?) -> Unit,
    onPlaybackRuntimeError: (message: String, context: String?) -> Unit,
    onPlaybackBuffering: () -> Unit,
    onPlaybackQualityReduced: () -> Unit,
) {
    val context = LocalContext.current
    var showContinuePrompt by remember(sessionId) { mutableStateOf(false) }
    // Covers the gap between "screen opened" and "a frame is actually up" —
    // negotiation already succeeded by the time this screen exists, but the
    // player still has to buffer its first segment(s) over the peer proxy,
    // which visibly took long enough on real hardware to look broken with
    // nothing on screen but a black box. Keyed on sessionId so autoplaying
    // into a preloaded next episode shows it again only when that player has
    // not already reached READY during the countdown.
    val playerPool = remember(context) { VideoPlayerPool(context) }
    val player = remember(playerPool, sessionId) {
        playerPool.activate(
            PlaybackPlayerConfig(
                sessionId = sessionId,
                url = url,
                title = title,
                maxBitrate = maxBitrate,
                subtitles = subtitles,
                resumePositionSecs = resumePositionSecs,
            ),
            startPaused = startPaused,
        )
    }
    PausePlayerWhenAppBackgrounded(player)
    var isLoading by remember(sessionId) { mutableStateOf(player.playbackState != Player.STATE_READY) }
    var hasStartedPlayback by remember(sessionId) {
        // A preloaded next episode can already be READY when it is promoted,
        // so it may never emit another initial READY callback to this screen.
        mutableStateOf(player.playbackState == Player.STATE_READY)
    }
    val playbackOutage = remember(sessionId) { PlaybackOutageTracker() }
    var showPauseOverlay by remember(sessionId) { mutableStateOf(!player.playWhenReady) }
    var consumeSurfaceSelectKeyUp by remember(sessionId) { mutableStateOf(false) }
    var offeredIntro by remember(sessionId) { mutableStateOf<SkipSegment?>(null) }
    var dismissedIntro by remember(sessionId) { mutableStateOf<SkipSegment?>(null) }
    // Credits-triggered "Play Next Episode" (#80): non-null while the active
    // continue prompt was popped because playback entered this CREDITS
    // segment, rather than from STATE_ENDED — see playbackBackAction and the
    // ContinueOverlay call site below for how that changes Cancel/Back.
    // dismissedCreditsSegment stops a cancelled offer from immediately
    // reappearing on the very next poll while still inside the same segment.
    var continuePromptCreditsSegment by remember(sessionId) { mutableStateOf<SkipSegment?>(null) }
    var dismissedCreditsSegment by remember(sessionId) { mutableStateOf<SkipSegment?>(null) }
    // Netflix-style "which section is this" label — Recap/Credits get a
    // plain badge (no skip action; Recap is short and Credits is already
    // near the Continue/next-episode flow), while Intro keeps its dedicated
    // Skip button below instead of duplicating into this badge too.
    var sectionLabel by remember(sessionId) { mutableStateOf<String?>(null) }
    var trackAvailability by remember(sessionId) {
        mutableStateOf(trackAvailability(player.currentTracks))
    }
    val normalMaxVideoBitrate = remember(player) { player.trackSelectionParameters.maxVideoBitrate }
    var bufferingQualityRecoveryGeneration by remember(sessionId) { mutableStateOf(0L) }

    fun reduceBufferingQuality() {
        val currentCap = player.trackSelectionParameters.maxVideoBitrate
        val recoveryCap = bufferingRecoveryVideoBitrateCap(
            selectedVideoBitrate = player.videoFormat?.bitrate,
            negotiatedMaxBitrate = maxBitrate,
            currentMaxVideoBitrate = currentCap,
        ) ?: return
        if (recoveryCap >= currentCap) return
        player.trackSelectionParameters = player.trackSelectionParameters
            .buildUpon()
            .setMaxVideoBitrate(recoveryCap)
            .build()
        bufferingQualityRecoveryGeneration += 1L
        onPlaybackQualityReduced()
    }

    // Each excessive-buffering transition immediately constrains adaptive
    // selection to 80% of the active rendition. Re-entering BUFFERING after a
    // recovery tightens the cap again when Media3 has already stepped down,
    // and restarts this 30-second hold. Restore only the video ceiling so any
    // audio/subtitle choice made while recovering remains intact.
    // The 30-second hold begins only once playback is READY again. Remaining
    // in or returning to BUFFERING cancels the countdown, preventing the old
    // ceiling from being restored while the connection is still struggling.
    LaunchedEffect(player, bufferingQualityRecoveryGeneration, isLoading) {
        if (bufferingQualityRecoveryGeneration == 0L || isLoading) return@LaunchedEffect
        delay(BUFFERING_QUALITY_RECOVERY_MS)
        if (player.playbackState != Player.STATE_READY || isLoading) return@LaunchedEffect
        player.trackSelectionParameters = player.trackSelectionParameters
            .buildUpon()
            .setMaxVideoBitrate(normalMaxVideoBitrate)
            .build()
    }

    // A new HLS show used to remain at its negotiated ceiling for the entire
    // startup wait because quality recovery required a previously rendered
    // frame. Give normal startup a brief chance, then step down if it is still
    // buffering. Direct play and deliberately paused playback remain exempt.
    LaunchedEffect(player, sessionId, playbackMode, hasStartedPlayback, showPauseOverlay) {
        if (
            !shouldStartStartupQualityRecovery(
                player.playbackState,
                hasStartedPlayback,
                player.playWhenReady,
                playbackMode,
            )
        ) return@LaunchedEffect
        delay(STARTUP_BUFFERING_QUALITY_RECOVERY_MS)
        if (
            shouldStartStartupQualityRecovery(
                player.playbackState,
                hasStartedPlayback,
                player.playWhenReady,
                playbackMode,
            )
        ) {
            reduceBufferingQuality()
        }
    }

    // Keep the video surface visible while Media3 waits for data. The global
    // toast host is layered above this screen, so each transition into a
    // loading state reports buffering without replacing the picture with a
    // second, full-screen loading experience. The report is held back until the
    // wait has lasted BUFFERING_NOTIFICATION_DELAY_MS so brief stalls that
    // recover on their own never surface a toast — leaving the loading state
    // cancels this effect before the delay elapses.
    LaunchedEffect(sessionId, isLoading) {
        if (!isLoading) return@LaunchedEffect
        delay(BUFFERING_NOTIFICATION_DELAY_MS)
        onPlaybackBuffering()
    }

    // Negotiation finishes asynchronously during the Continue countdown.
    // Preparing with playWhenReady=false makes Media3 fetch and buffer the
    // next stream without advancing away from the ended episode. When the
    // ViewModel promotes this session, activate() returns this same player
    // instance instead of creating one with an empty buffer.
    LaunchedEffect(preloadedNext?.sessionId) {
        if (preloadedNext == null) {
            playerPool.releasePreloaded()
        } else {
            playerPool.preload(preloadedNext.toPlayerConfig())
        }
    }
    DisposableEffect(playerPool) {
        onDispose { playerPool.releasePreloaded() }
    }
    // An EVENT HLS playlist grows as ffmpeg produces segments, so its native
    // Media3 timeline ends at the generated/buffered edge rather than at the
    // end of the movie. Present the catalog's probed full duration to the
    // controller. Seeks inside the generated window stay local; seeks beyond
    // it ask the ViewModel to replace this session with one transcoding from
    // the requested absolute position. Direct play uses the same full-length
    // display but continues to seek normally through HTTP Range requests.
    val controllerPlayer = remember(player, playbackMode, positionOffsetSecs, mediaDurationSecs) {
        FullDurationPlayer(
            delegate = player,
            fullDurationMs = mediaDurationSecs
                ?.takeIf { it.isFinite() && it > 0.0 }
                ?.let { (it * 1000.0).toLong() },
            positionOffsetMs = (positionOffsetSecs * 1000.0).toLong(),
            restartOutsideAvailableWindow = playbackMode == PlaybackMode.HLS,
            onRestartAt = { positionMs -> onSeekOutsideBuffer(positionMs / 1000.0) },
        )
    }
    // Media3 has no continuous playhead callback, so sample the lightweight
    // in-process position while video is running. The offer appears at most
    // once per marker, for ten seconds, and remains dismissed after a click
    // or timeout until playback reaches another intro marker.
    LaunchedEffect(player, entry.fingerprint) {
        while (true) {
            val segmentsVisible = !isLoading && !showPauseOverlay && !showContinuePrompt
            // Intentionally NOT gated on segmentsVisible: startup rebuffering
            // must not wipe a still-valid "Skip intro" offer (#102). The
            // button itself stays hidden behind the pause/continue overlays
            // at its call site.
            offeredIntro = nextIntroOffer(
                segments = entry.entry.skipSegments,
                positionMs = controllerPlayer.currentPosition,
                isEpisode = entry.entry.kind == MediaKind.EPISODE,
                current = offeredIntro,
                dismissed = dismissedIntro,
            )
            // Popped the moment playback enters a CREDITS marker, instead of
            // waiting for STATE_ENDED (#80). Guarded on segmentsVisible so it
            // never fires while some other overlay (pause, an already-shown
            // continue prompt) is up, and on identity against
            // dismissedCreditsSegment so cancelling the offer doesn't
            // re-trigger it on the very next poll of the same segment.
            if (entry.entry.kind == MediaKind.EPISODE && hasNext && segmentsVisible) {
                val credits = activeSkipSegment(entry.entry.skipSegments, controllerPlayer.currentPosition, SkipSegmentKind.CREDITS)
                if (credits != null && credits != dismissedCreditsSegment) {
                    continuePromptCreditsSegment = credits
                    showContinuePrompt = true
                    onPreloadNext()
                }
            }
            sectionLabel = if (!segmentsVisible) {
                null
            } else {
                val position = controllerPlayer.currentPosition
                when {
                    activeSkipSegment(entry.entry.skipSegments, position, SkipSegmentKind.RECAP) != null -> "Recap"
                    activeSkipSegment(entry.entry.skipSegments, position, SkipSegmentKind.CREDITS) != null -> "Credits"
                    else -> null
                }
            }
            delay(INTRO_POSITION_POLL_MS)
        }
    }
    LaunchedEffect(offeredIntro, isLoading) {
        val segment = offeredIntro ?: return@LaunchedEffect
        // Don't spend the ten-second window while the picture is still
        // buffering — the countdown restarts once playback is actually up.
        if (isLoading) return@LaunchedEffect
        delay(SKIP_INTRO_OFFER_MS)
        if (offeredIntro == segment) {
            dismissedIntro = segment
            offeredIntro = null
        }
    }
    val playerView = remember(context) {
        PlayerView(context).apply {
            useController = true
            // Real bug, found live (#55): Media3's subtitle button/settings
            // submenu (which is where the built-in "Off" option lives) stays
            // hidden unless explicitly requested — off by default. Without
            // this, viewers had no reachable control to disable subtitles or
            // switch tracks during active playback.
            setShowSubtitleButton(true)
            applySwarmPlaybackControlColors(this)
        }
    }
    // Focus target for the bare, playing video surface. Requested through
    // Compose (not the native View) so the surface's onPreviewKeyEvent regains
    // ownership of Select after the native controller — whose buttons sit
    // outside the Compose focus tree — releases it (#154).
    val videoSurfaceFocusRequester = remember { FocusRequester() }
    val reclaimVideoSurfaceFocus: () -> Unit = {
        playerView.hideController()
        playerView.requestFocus()
        // The requester is not attached until the surface enters composition;
        // an early controller-hide callback must not crash playback over it.
        runCatching { videoSurfaceFocusRequester.requestFocus() }
        Unit
    }
    // The native controller hangs onto D-pad focus after it disappears — on
    // its own timeout just as much as via Back — leaving Select landing on an
    // invisible transport button until Back dislodges it. Pull focus back to
    // the video surface every time the controller hides while playback (not a
    // Compose overlay) should own the remote (#154).
    val overlayOwnsFocus by rememberUpdatedState(
        showPauseOverlay || showContinuePrompt || offeredIntro != null,
    )
    DisposableEffect(playerView) {
        val listener = PlayerView.ControllerVisibilityListener { visibility ->
            if (shouldReclaimSurfaceFocusOnControllerHidden(
                    controllerVisible = visibility == View.VISIBLE,
                    overlayOwnsFocus = overlayOwnsFocus,
                )
            ) {
                reclaimVideoSurfaceFocus()
            }
        }
        playerView.setControllerVisibilityListener(listener)
        onDispose {
            playerView.setControllerVisibilityListener(
                null as PlayerView.ControllerVisibilityListener?,
            )
        }
    }
    // Back dismisses the native playback controls before it affects the
    // video. From the bare playing surface it pauses; from the Compose pause
    // or Continue overlay it exits to the screen/card playback came from.
    BackHandler(
        onBack = {
            when (
                playbackBackAction(
                    showPauseOverlay = showPauseOverlay,
                    showContinuePrompt = showContinuePrompt,
                    controlsVisible = playerView.isControllerFullyVisible,
                    continuePromptDismissable = continuePromptCreditsSegment != null,
                )
            ) {
                PlaybackBackAction.EXIT -> onBack()
                PlaybackBackAction.DISMISS_CONTINUE_PROMPT -> {
                    showContinuePrompt = false
                    continuePromptCreditsSegment?.let { dismissedCreditsSegment = it }
                    continuePromptCreditsSegment = null
                }
                PlaybackBackAction.HIDE_CONTROLS -> reclaimVideoSurfaceFocus()
                PlaybackBackAction.PAUSE -> player.pause()
            }
        },
    )
    val skipIntroFocusRequester = remember(sessionId) { FocusRequester() }
    LaunchedEffect(sessionId, playerView, showPauseOverlay, showContinuePrompt, offeredIntro) {
        if (offeredIntro != null && !showPauseOverlay && !showContinuePrompt) {
            skipIntroFocusRequester.requestFocus()
        } else if (!showPauseOverlay && !showContinuePrompt) {
            // PlayerView is deliberately reused across episode handoffs. Its
            // native controller can still own focus after the ended episode's
            // Continue overlay disappears, even when that controller is no
            // longer visibly rendered. Reset both controller visibility and
            // focus for every new session so the next Select reaches the bare
            // video surface immediately instead of needing Back to dislodge
            // the stale controller (#154).
            reclaimVideoSurfaceFocus()
        }
    }
    val loudnessEnhancer = remember(player) {
        // player.audioSessionId is generated by ExoPlayer's audio sink at
        // construction time, so it's already valid here, before prepare()/playback
        // start. Guarded with runCatching: a small number of OEM audio HALs reject
        // LoudnessEnhancer for a given session, and that's not worth failing
        // playback over — it just means this specific device plays at unboosted
        // volume.
        runCatching {
            LoudnessEnhancer(player.audioSessionId).apply {
                setTargetGain(AUDIO_BOOST_MILLIBELS)
                enabled = true
            }
        }.getOrNull()
    }

    // Flush the resume position on a heartbeat, not just on dispose: if the
    // TV loses power outright (#77) or the process is otherwise killed, the
    // Compose disposal in the DisposableEffect below never runs, so without
    // this the persisted position would stay stuck wherever it was at the
    // last normal exit rather than near the true last-watched point.
    LaunchedEffect(player, entry.fingerprint) {
        while (true) {
            delay(PERIODIC_POSITION_SAVE_MS)
            if (!player.isPlaying) continue
            val positionSecs = positionOffsetSecs + player.currentPosition / 1000.0
            val durationSecs = player.duration.takeIf { it != C.TIME_UNSET }?.let { positionOffsetSecs + it / 1000.0 } ?: 0.0
            onPositionUpdate(positionSecs, durationSecs)
        }
    }

    DisposableEffect(player) {
        val analyticsListener = object : AnalyticsListener {
            override fun onLoadError(
                eventTime: AnalyticsListener.EventTime,
                loadEventInfo: LoadEventInfo,
                mediaLoadData: MediaLoadData,
                error: IOException,
                wasCanceled: Boolean,
            ) {
                if (wasCanceled || !isServerOfflineLoadError(error)) return
                val failureContext =
                    "position_ms=${player.currentPosition}; buffered_position_ms=${player.bufferedPosition}; " +
                        "load_error=${error.javaClass.simpleName}: ${error.message.orEmpty()}"
                playbackOutage.onLoadError(failureContext, player.playbackState)?.let(onServerOffline)
                // Let the buffered picture continue uninterrupted. Report
                // the outage and buffering only once playback actually runs
                // out of media. A successful retry before then stays silent.
                if (player.playbackState == Player.STATE_BUFFERING) isLoading = true
            }

            override fun onLoadCompleted(
                eventTime: AnalyticsListener.EventTime,
                loadEventInfo: LoadEventInfo,
                mediaLoadData: MediaLoadData,
            ) {
                playbackOutage.onLoadCompleted()
                if (player.playbackState == Player.STATE_READY) isLoading = false
            }

        }
        val listener = object : Player.Listener {
            override fun onPlaybackStateChanged(playbackState: Int) {
                // Covers audio (tracks never render a video frame at all,
                // so onRenderedFirstFrame below would never fire for them).
                if (playbackState == Player.STATE_READY) {
                    isLoading = false
                    hasStartedPlayback = true
                }
                playbackOutage.onPlaybackStateChanged(playbackState)?.let(onServerOffline)
                if (playbackState == Player.STATE_BUFFERING && playbackOutage.isPending) {
                    isLoading = true
                }
                // A stall after the viewer already saw a frame is a real
                // mid-playback rebuffer (almost always the bandwidth running
                // out from under the current rendition). Keep the last frame
                // visible and let the loading-state effect surface the toast.
                if (playbackState == Player.STATE_BUFFERING && hasStartedPlayback && !showPauseOverlay) {
                    isLoading = true
                }
                if (
                    shouldStartBufferingQualityRecovery(
                        playbackState = playbackState,
                        hasStartedPlayback = hasStartedPlayback,
                        playWhenReady = player.playWhenReady,
                        playbackMode = playbackMode,
                        hasVideo = player.videoFormat != null,
                    )
                ) {
                    reduceBufferingQuality()
                }
                if (playbackState == Player.STATE_ENDED && hasNext) {
                    // Reached the true end, not just a credits marker (or no
                    // marker exists at all) — Cancel/Back on this prompt must
                    // exit rather than dismiss-and-keep-watching.
                    continuePromptCreditsSegment = null
                    showPauseOverlay = false
                    showContinuePrompt = true
                    onPreloadNext()
                }
            }

            override fun onPlayWhenReadyChanged(playWhenReady: Boolean, reason: Int) {
                showPauseOverlay = !playWhenReady && player.playbackState != Player.STATE_ENDED
            }

            override fun onTracksChanged(tracks: Tracks) {
                trackAvailability = trackAvailability(tracks)
            }

            // Fires strictly before STATE_READY can be observed for video,
            // so relying on it too (whichever comes first) avoids a brief
            // dead frame between "ready" and pixels actually landing.
            override fun onRenderedFirstFrame() {
                isLoading = false
                hasStartedPlayback = true
            }

            // Runtime failures after negotiation already succeeded (network
            // drop mid-stream, a decoder/codec error) — distinct from, and
            // not caught by, the negotiation-failure path in SwarmViewModel.
            // Playback-triage-worthy either way, so it gets the same report.
            override fun onPlayerError(error: PlaybackException) {
                val context = playbackErrorContext(error, player)

                // Playback URLs name a server-side session that is removed
                // after five idle minutes. A long pause therefore makes the
                // next HLS segment/direct-play Range request return 404 even
                // though the asset still exists. Preparing this same URL
                // again can never heal it: negotiate a fresh session at the
                // absolute playhead instead. Other 404s and all other player
                // failures retain the normal reporting path below.
                val responseCode = playbackHttpResponseCode(error)
                if (shouldRecoverExpiredPlaybackSession(error.errorCode, responseCode)) {
                    isLoading = true
                    val positionSecs = positionOffsetSecs + player.currentPosition.coerceAtLeast(0L) / 1000.0
                    onPlaybackSessionExpired(positionSecs, context)
                    return
                }

                isLoading = false
                onPlaybackRuntimeError(
                    "${error.message ?: "Playback failed"} (${error.errorCodeName})",
                    context,
                )
            }
        }
        player.addAnalyticsListener(analyticsListener)
        player.addListener(listener)
        onDispose {
            player.removeAnalyticsListener(analyticsListener)
            player.removeListener(listener)
            val positionSecs = positionOffsetSecs + player.currentPosition / 1000.0
            val durationSecs = player.duration.takeIf { it != C.TIME_UNSET }?.let { positionOffsetSecs + it / 1000.0 } ?: 0.0
            onPositionUpdate(positionSecs, durationSecs)
            loudnessEnhancer?.release()
            playerPool.release(player)
        }
    }

    // Letterbox/pillarbox areas around the video frame must be plain black,
    // not the app's own themed background (a dark blue) showing through —
    // this Box had no background of its own, so it inherited whatever was
    // behind it.
    Box(
        modifier = Modifier
            .fillMaxSize()
            .background(Color.Black)
            .onPreviewKeyEvent { composeEvent ->
                val event = composeEvent.nativeKeyEvent
                val isSelectKey = event.keyCode == KeyEvent.KEYCODE_DPAD_CENTER ||
                    event.keyCode == KeyEvent.KEYCODE_ENTER ||
                    event.keyCode == KeyEvent.KEYCODE_NUMPAD_ENTER
                // pause() updates playWhenReady immediately, before this
                // gesture's key-up arrives. Remember the matching key-up so
                // it cannot land on the newly focused Resume button and
                // instantly undo the pause.
                if (isSelectKey && consumeSurfaceSelectKeyUp) {
                    if (event.action == KeyEvent.ACTION_UP) consumeSurfaceSelectKeyUp = false
                    return@onPreviewKeyEvent true
                }
                val surfaceAction = videoSurfacePlaybackAction(
                    keyCode = event.keyCode,
                    playWhenReady = player.playWhenReady && !showPauseOverlay && !showContinuePrompt,
                    controlsVisible = playerView.isControllerFullyVisible,
                    // The "Skip intro" button owns focus while it is offered,
                    // so let its key events reach it instead of pausing (#94).
                    surfaceButtonFocused = offeredIntro != null,
                )
                if (surfaceAction != null) {
                    if (event.action == KeyEvent.ACTION_DOWN) {
                        when (surfaceAction) {
                            RemotePlaybackAction.SEEK_FORWARD -> controllerPlayer.seekTo(
                                controllerPlayer.currentPosition + PLAYBACK_SEEK_STEP_MS,
                            )
                            RemotePlaybackAction.SEEK_BACK -> controllerPlayer.seekTo(
                                (controllerPlayer.currentPosition - PLAYBACK_SEEK_STEP_MS).coerceAtLeast(0L),
                            )
                            RemotePlaybackAction.PAUSE -> {
                                consumeSurfaceSelectKeyUp = true
                                player.pause()
                            }
                            else -> Unit
                        }
                    }
                    return@onPreviewKeyEvent true
                }
                val action = remotePlaybackAction(event.keyCode) ?: return@onPreviewKeyEvent false
                if (event.action == KeyEvent.ACTION_DOWN) {
                    when (action) {
                        RemotePlaybackAction.TOGGLE_PLAY_PAUSE -> {
                            if (player.playWhenReady) player.pause() else player.play()
                        }
                        RemotePlaybackAction.PLAY -> player.play()
                        RemotePlaybackAction.PAUSE -> player.pause()
                        RemotePlaybackAction.SEEK_FORWARD -> controllerPlayer.seekForward()
                        RemotePlaybackAction.SEEK_BACK -> controllerPlayer.seekBack()
                        RemotePlaybackAction.SHOW_CONTROLS -> {
                            if (!showPauseOverlay && !showContinuePrompt) {
                                playerView.showController()
                                playerView.requestFocus()
                            }
                        }
                    }
                }
                // Consume the matching key-up too. Otherwise it can land on a
                // newly focused controller/overlay button and click it after
                // the key-down changed playback state or focus.
                true
            },
    ) {
        AndroidView(
            // Compose does not automatically forward TV remote events into
            // an embedded Android View. Make the host focusable and delegate
            // its events so D-pad Select reveals the controller and its
            // play/pause, seek, and settings buttons can receive navigation.
            modifier = Modifier
                .fillMaxSize()
                .testTag(UatTestTags.PLAYER_SURFACE)
                .focusRequester(videoSurfaceFocusRequester)
                .focusable()
                .onKeyEvent {
                    val event = it.nativeKeyEvent
                    // Let Back bubble up to [BackHandler] instead of handing it to
                    // Media3's PlayerView — its embedded PlayerControlView can
                    // swallow the key itself (e.g. while its transport bar is
                    // visible/settling), which silently ate the first Back press
                    // and made pause-on-Back need two clicks every time.
                    if (event.keyCode == KeyEvent.KEYCODE_BACK) false else playerView.dispatchKeyEvent(event)
                },
            factory = { playerView },
            // Real bug, found live: autoplaying the next episode creates a
            // new active ExoPlayer (either freshly created or promoted from
            // the preload pool), but `factory` only ever runs once for a
            // given AndroidView call site — without this `update`, the
            // on-screen PlayerView stayed bound to the *previous*, already-
            // released player forever. Audio kept working regardless
            // (ExoPlayer's audio output doesn't need a bound view, only video
            // rendering does), which is exactly why sound played over a
            // frozen frame showing the old episode's final progress bar.
            update = { view ->
                view.player = controllerPlayer
                view.useController = !showPauseOverlay
                if (showPauseOverlay) view.hideController()
            },
        )

        if (showContinuePrompt) {
            ContinueOverlay(
                nextTitle = nextTitle,
                nextArtworkUrl = nextArtworkUrl,
                onPlayNow = { showContinuePrompt = false; onContinue() },
                onCancel = {
                    showContinuePrompt = false
                    val creditsSegment = continuePromptCreditsSegment
                    if (creditsSegment != null) {
                        // Popped early, mid-credits — playback never stopped,
                        // so dismiss the offer and keep watching instead of
                        // exiting like the STATE_ENDED-triggered prompt does.
                        dismissedCreditsSegment = creditsSegment
                        continuePromptCreditsSegment = null
                    } else {
                        onBack()
                    }
                },
            )
        }

        if (showPauseOverlay && !showContinuePrompt) {
            PauseOverlay(
                entry = entry,
                recommendations = recommendations,
                artworkUrl = artworkUrl,
                audioTracks = trackAvailability.audioTracks,
                subtitleTracks = trackAvailability.subtitleTracks,
                hasNextEpisode = hasNext && entry.entry.kind == MediaKind.EPISODE,
                onResume = player::play,
                onNextEpisode = onContinue,
                onPlayRecommendation = onPlayRecommendation,
                onSelectAudioTrack = { choice -> selectAudioTrack(player, choice) },
                onSelectSubtitleTrack = { choice -> selectSubtitleTrack(player, choice) },
            )
        }

        sectionLabel?.takeIf { !showPauseOverlay && !showContinuePrompt }?.let { label ->
            Text(
                label,
                color = SwarmText,
                fontSize = 13.sp,
                fontWeight = FontWeight.Bold,
                modifier = Modifier
                    .align(Alignment.TopStart)
                    .padding(start = 42.dp, top = 36.dp)
                    .background(Color.Black.copy(alpha = 0.55f), RoundedCornerShape(4.dp))
                    .padding(horizontal = 10.dp, vertical = 5.dp),
            )
        }

        offeredIntro?.takeIf { !showPauseOverlay && !showContinuePrompt }?.let { segment ->
            Button(
                onClick = {
                    dismissedIntro = segment
                    offeredIntro = null
                    controllerPlayer.seekTo(checkNotNull(segment.endMs))
                },
                modifier = Modifier
                    .align(Alignment.BottomEnd)
                    .padding(end = 42.dp, bottom = 54.dp)
                    .focusRequester(skipIntroFocusRequester),
                colors = swarmActionButtonColors(),
            ) {
                Text("Skip intro  ⏭", color = Color(0xFF04263A), fontWeight = FontWeight.Bold)
            }
        }
    }
}

/** One selectable option in the pause-screen audio/subtitle pickers.
 * [group]/[trackIndex] identify the real Media3 track to select; [group] is
 * null only for the subtitle list's synthetic "Off" entry. */
private data class TrackChoice(
    val label: String,
    val group: TrackGroup?,
    val trackIndex: Int,
    val isSelected: Boolean,
)

private data class TrackAvailability(
    val audioTracks: List<TrackChoice>,
    val subtitleTracks: List<TrackChoice>,
)

private const val OFF_SUBTITLE_CHOICE_LABEL = "Off"

private fun trackAvailability(tracks: Tracks): TrackAvailability {
    val audioTracks = mutableListOf<TrackChoice>()
    val subtitleTracks = mutableListOf<TrackChoice>()
    var subtitleSelected = false
    for (group in tracks.groups) {
        for (index in 0 until group.length) {
            if (!group.isTrackSupported(index)) continue
            val format = group.getTrackFormat(index)
            val isSelected = group.isTrackSelected(index)
            when (group.type) {
                C.TRACK_TYPE_AUDIO -> audioTracks += TrackChoice(
                    label = audioTrackLabel(format),
                    group = group.mediaTrackGroup,
                    trackIndex = index,
                    isSelected = isSelected,
                )
                C.TRACK_TYPE_TEXT -> {
                    subtitleSelected = subtitleSelected || isSelected
                    subtitleTracks += TrackChoice(
                        label = subtitleTrackLabel(format, subtitleTracks.size),
                        group = group.mediaTrackGroup,
                        trackIndex = index,
                        isSelected = isSelected,
                    )
                }
            }
        }
    }
    val offChoice = TrackChoice(
        label = OFF_SUBTITLE_CHOICE_LABEL,
        group = null,
        trackIndex = C.INDEX_UNSET,
        isSelected = !subtitleSelected,
    )
    return TrackAvailability(
        audioTracks = audioTracks.distinctByLabel(),
        subtitleTracks = listOf(offChoice) + subtitleTracks.distinctByLabel(),
    )
}

private fun audioTrackLabel(format: Format): String =
    format.language?.takeIf(String::isNotBlank)?.let(::languageDisplayName)
        ?: format.label?.takeIf(String::isNotBlank)
        ?: "Default audio"

private fun subtitleTrackLabel(format: Format, index: Int): String =
    format.label?.takeIf(String::isNotBlank)
        ?: format.language?.takeIf(String::isNotBlank)?.let(::languageDisplayName)
        ?: "Subtitle ${index + 1}"

private fun languageDisplayName(code: String): String {
    val normalized = code.trim().replace('_', '-')
    val locale = Locale.forLanguageTag(normalized)
    return locale.getDisplayLanguage(Locale.getDefault())
        .takeIf { it.isNotBlank() && !it.equals(normalized, ignoreCase = true) }
        ?: code.uppercase()
}

private fun List<TrackChoice>.distinctByLabel(): List<TrackChoice> =
    distinctBy { it.label.trim().lowercase() }

/** Applies [choice] as the sole override for its track type, so picking a
 * new audio track deselects whichever one was previously playing. */
private fun selectAudioTrack(player: Player, choice: TrackChoice) {
    val group = choice.group ?: return
    player.trackSelectionParameters = player.trackSelectionParameters
        .buildUpon()
        .setOverrideForType(TrackSelectionOverride(group, choice.trackIndex))
        .build()
}

/** [TrackChoice.group] is null for the synthetic "Off" entry — disabling the
 * text track type is how Media3 turns subtitles off outright, distinct from
 * merely clearing which track is overridden. */
private fun selectSubtitleTrack(player: Player, choice: TrackChoice) {
    val builder = player.trackSelectionParameters
        .buildUpon()
        .clearOverridesOfType(C.TRACK_TYPE_TEXT)
    val group = choice.group
    if (group == null) {
        builder.setTrackTypeDisabled(C.TRACK_TYPE_TEXT, true)
    } else {
        builder
            .setTrackTypeDisabled(C.TRACK_TYPE_TEXT, false)
            .setOverrideForType(TrackSelectionOverride(group, choice.trackIndex))
    }
    player.trackSelectionParameters = builder.build()
}

@Composable
private fun PauseOverlay(
    entry: MergedEntry,
    recommendations: List<MergedEntry>,
    artworkUrl: (MergedEntry) -> String?,
    audioTracks: List<TrackChoice>,
    subtitleTracks: List<TrackChoice>,
    hasNextEpisode: Boolean,
    onResume: () -> Unit,
    onNextEpisode: () -> Unit,
    onPlayRecommendation: (MergedEntry) -> Unit,
    onSelectAudioTrack: (TrackChoice) -> Unit,
    onSelectSubtitleTrack: (TrackChoice) -> Unit,
) {
    val resumeFocusRequester = remember { FocusRequester() }
    LaunchedEffect(entry.fingerprint) { resumeFocusRequester.requestFocus() }
    val media = entry.entry
    val title = if (media.kind == MediaKind.EPISODE) pauseRecommendationTitle(entry) else media.displayTitle()
    val metadata = listOfNotNull(
        media.year?.toString(),
        media.durationSecs?.takeIf { it.isFinite() && it > 0.0 }?.let(::durationLabel),
        media.rating?.takeIf(String::isNotBlank)?.let { "Rated $it" },
        media.communityRating?.let { score ->
            val votes = media.communityRatingVotes?.takeIf { it > 0 }
                ?.let { String.format(Locale.US, " (%,d votes)", it) }
                .orEmpty()
            String.format(Locale.US, "★ %.1f/10", score) + votes
        },
        media.video?.height?.takeIf { it > 0 }?.let { "${it}p" },
    )

    Box(
        modifier = Modifier.fillMaxSize().background(
            Brush.horizontalGradient(
                listOf(Color.Black.copy(alpha = 0.96f), Color.Black.copy(alpha = 0.90f), Color.Black.copy(alpha = 0.82f)),
            ),
        ),
    ) {
        Column(modifier = Modifier.fillMaxSize().padding(horizontal = 44.dp, vertical = 28.dp)) {
            Row(modifier = Modifier.fillMaxWidth().weight(1f), horizontalArrangement = Arrangement.spacedBy(36.dp)) {
                Column(modifier = Modifier.weight(1f)) {
                    Text(
                        "Paused",
                        color = SwarmAccent,
                        fontSize = 13.sp,
                        fontWeight = FontWeight.Bold,
                        modifier = Modifier.testTag(UatTestTags.PAUSE_LABEL),
                    )
                    Spacer(Modifier.height(5.dp))
                    Text(
                        title,
                        color = SwarmText,
                        fontSize = 28.sp,
                        fontWeight = FontWeight.Black,
                        maxLines = 2,
                        modifier = Modifier.testTag(UatTestTags.PAUSE_TITLE),
                    )
                    episodeNumberLabel(entry)?.let { episodeLabel ->
                        Spacer(Modifier.height(4.dp))
                        Text(episodeLabel, color = SwarmText, fontSize = 15.sp, maxLines = 1)
                    }
                    if (metadata.isNotEmpty()) {
                        Spacer(Modifier.height(7.dp))
                        Text(
                            metadata.joinToString("  •  "),
                            color = SwarmMuted,
                            fontSize = 13.sp,
                            maxLines = 1,
                            modifier = Modifier.testTag(UatTestTags.PAUSE_METADATA),
                        )
                    }
                    if (media.genres.isNotEmpty()) {
                        Spacer(Modifier.height(6.dp))
                        Text(
                            media.genres.take(4).joinToString("  •  "),
                            color = SwarmAccent,
                            fontSize = 12.sp,
                            maxLines = 1,
                            modifier = Modifier.testTag(UatTestTags.PAUSE_GENRES),
                        )
                    }
                    if (media.cast.isNotEmpty()) {
                        Spacer(Modifier.height(6.dp))
                        Text(
                            "Cast: ${media.cast.take(5).joinToString { it.name }}",
                            color = SwarmMuted,
                            fontSize = 12.sp,
                            maxLines = 1,
                            overflow = androidx.compose.ui.text.style.TextOverflow.Ellipsis,
                            modifier = Modifier.testTag(UatTestTags.PAUSE_CAST),
                        )
                    }
                    media.overview?.takeIf(String::isNotBlank)?.let { overview ->
                        Spacer(Modifier.height(10.dp))
                        Text(
                            overview,
                            color = SwarmMuted,
                            fontSize = 13.sp,
                            lineHeight = 18.sp,
                            maxLines = 3,
                            overflow = androidx.compose.ui.text.style.TextOverflow.Ellipsis,
                            modifier = Modifier.testTag(UatTestTags.PAUSE_DESCRIPTION),
                        )
                    }
                    Spacer(Modifier.height(12.dp))
                    Row(horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                        Button(
                            onClick = onResume,
                            modifier = Modifier.focusRequester(resumeFocusRequester)
                                .testTag(UatTestTags.PAUSE_RESUME_BUTTON),
                            colors = swarmActionButtonColors(),
                        ) {
                            Text("▶  Resume", color = Color(0xFF04263A), fontWeight = FontWeight.Bold)
                        }
                        if (hasNextEpisode) {
                            Button(
                                onClick = onNextEpisode,
                                colors = swarmActionButtonColors(),
                                modifier = Modifier.testTag(UatTestTags.PAUSE_NEXT_EPISODE_BUTTON),
                            ) {
                                Text("Next Episode  ⏭", color = Color(0xFF04263A), fontWeight = FontWeight.Bold)
                            }
                        }
                    }
                }
                Column(modifier = Modifier.width(285.dp)) {
                    TrackPickerBlock(
                        heading = "Audio language",
                        choices = audioTracks,
                        emptyLabel = "None reported",
                        onSelect = onSelectAudioTrack,
                        testTag = UatTestTags.PAUSE_AUDIO_TRACK_PICKER,
                        optionTagPrefix = UatTestTags.PAUSE_AUDIO_OPTION_PREFIX,
                    )
                    Spacer(Modifier.height(18.dp))
                    TrackPickerBlock(
                        heading = "Subtitles",
                        choices = subtitleTracks,
                        emptyLabel = "None available",
                        onSelect = onSelectSubtitleTrack,
                        testTag = UatTestTags.PAUSE_SUBTITLE_TRACK_PICKER,
                        optionTagPrefix = UatTestTags.PAUSE_SUBTITLE_OPTION_PREFIX,
                    )
                }
            }

            if (recommendations.isNotEmpty()) {
                Spacer(Modifier.height(12.dp))
                Text("More like this", color = SwarmText, fontSize = 17.sp, fontWeight = FontWeight.Bold)
                Spacer(Modifier.height(8.dp))
                LazyRow(
                    modifier = Modifier.fillMaxWidth().height(162.dp),
                    horizontalArrangement = Arrangement.spacedBy(12.dp),
                ) {
                    items(recommendations, key = { it.fingerprint }) { recommendation ->
                        PauseRecommendationCard(
                            entry = recommendation,
                            artworkUrl = artworkUrl(recommendation),
                            onClick = { onPlayRecommendation(recommendation) },
                        )
                    }
                }
            }
        }
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun TrackPickerBlock(
    heading: String,
    choices: List<TrackChoice>,
    emptyLabel: String,
    onSelect: (TrackChoice) -> Unit,
    testTag: String? = null,
    optionTagPrefix: String? = null,
) {
    Text(heading, color = SwarmMuted, fontSize = 12.sp, fontWeight = FontWeight.SemiBold)
    Spacer(Modifier.height(5.dp))
    if (choices.isEmpty()) {
        Text(emptyLabel, color = SwarmText, fontSize = 13.sp, modifier = if (testTag != null) Modifier.testTag(testTag) else Modifier)
        return
    }
    FlowRow(
        modifier = if (testTag != null) Modifier.testTag(testTag) else Modifier,
        horizontalArrangement = Arrangement.spacedBy(6.dp),
        verticalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        for ((index, choice) in choices.withIndex()) {
            SelectableChip(
                choice.label,
                isSelected = choice.isSelected,
                onClick = { onSelect(choice) },
                modifier = if (optionTagPrefix != null) Modifier.testTag(optionTagPrefix + index) else Modifier,
            )
        }
    }
}

/** Same 2:3 poster box every other shelf in the app uses (see
 * `CatalogScreen`'s `CARD_MEDIA_HEIGHT`/`MovieShelfScreen`'s
 * `aspectRatio(2f / 3f)`) — a square box here used to force [ArtworkImage]'s
 * `ContentScale.Crop` to chop the top/bottom off every portrait poster. No
 * title underneath either: this shelf is "more like this" browsing, not a
 * text-first list, and the poster art alone reads fine at this size like it
 * does everywhere else artwork is shown without a caption. */
@Composable
private fun PauseRecommendationCard(entry: MergedEntry, artworkUrl: String?, onClick: () -> Unit) {
    Card(
        onClick = onClick,
        colors = CardDefaults.colors(containerColor = SwarmSurface),
        scale = CardDefaults.scale(scale = 1f, focusedScale = 1.06f, pressedScale = 0.98f),
        modifier = Modifier.width(108.dp),
    ) {
        ArtworkImage(
            label = pauseRecommendationTitle(entry),
            placeholderType = if (entry.entry.kind == MediaKind.MOVIE) "Movie" else "Show",
            primaryUrl = artworkUrl,
            modifier = Modifier.fillMaxWidth().aspectRatio(2f / 3f).clip(RoundedCornerShape(4.dp)),
        )
    }
}

private fun durationLabel(durationSecs: Double): String {
    val totalMinutes = (durationSecs / 60.0).toInt().coerceAtLeast(1)
    val hours = totalMinutes / 60
    val minutes = totalMinutes % 60
    return if (hours == 0) "${minutes}m" else "${hours}h ${minutes}m"
}

/** Only a missing negotiated stream is self-healable. A 404 reported under
 * another Media3 category, or a different bad HTTP status, needs the normal
 * error path instead of an automatic replay loop. */
internal fun shouldRecoverExpiredPlaybackSession(errorCode: Int, responseCode: Int?): Boolean =
    errorCode == PlaybackException.ERROR_CODE_IO_BAD_HTTP_STATUS && responseCode == 404

/**
 * Gives Media3's stock TV controller an absolute, full-length timeline even
 * when the underlying HLS EVENT playlist currently contains only the first
 * few generated segments.
 */
@androidx.annotation.OptIn(UnstableApi::class)
private class FullDurationPlayer(
    delegate: Player,
    private val fullDurationMs: Long?,
    private val positionOffsetMs: Long,
    private val restartOutsideAvailableWindow: Boolean,
    private val onRestartAt: (Long) -> Unit,
) : ForwardingPlayer(delegate) {
    private var restartRequested = false

    override fun getDuration(): Long = fullDurationMs ?: super.getDuration()

    override fun getContentDuration(): Long = duration

    override fun getCurrentPosition(): Long = absolutePosition(super.getCurrentPosition())

    override fun getContentPosition(): Long = absolutePosition(super.getContentPosition())

    override fun getBufferedPosition(): Long = absolutePosition(super.getBufferedPosition())

    override fun getContentBufferedPosition(): Long = absolutePosition(super.getContentBufferedPosition())

    override fun getBufferedPercentage(): Int {
        val duration = fullDurationMs ?: return super.getBufferedPercentage()
        if (duration <= 0) return 0
        return ((bufferedPosition * 100L) / duration).coerceIn(0L, 100L).toInt()
    }

    override fun getCurrentTimeline(): Timeline {
        val timeline = super.getCurrentTimeline()
        val duration = fullDurationMs ?: return timeline
        return if (timeline.isEmpty) timeline else FullDurationTimeline(timeline, duration)
    }

    override fun isCurrentMediaItemSeekable(): Boolean = fullDurationMs != null || super.isCurrentMediaItemSeekable

    override fun isCurrentMediaItemDynamic(): Boolean = false

    override fun isCurrentMediaItemLive(): Boolean = false

    override fun seekTo(positionMs: Long) {
        seekTo(currentMediaItemIndex, positionMs)
    }

    override fun seekTo(mediaItemIndex: Int, positionMs: Long) {
        val target = clampToFullDuration(positionMs)
        val availableDuration = super.getDuration()
        val relativeTarget = target - positionOffsetMs
        val outsideAvailableWindow = shouldRestartHlsPlaybackForSeek(relativeTarget, availableDuration)

        if (restartOutsideAvailableWindow && outsideAvailableWindow) {
            if (!restartRequested) {
                restartRequested = true
                pause()
                onRestartAt(target)
            }
            return
        }
        super.seekTo(mediaItemIndex, relativeTarget.coerceAtLeast(0L))
    }

    override fun seekForward() {
        seekTo(currentPosition + seekForwardIncrement)
    }

    override fun seekBack() {
        seekTo(currentPosition - seekBackIncrement)
    }

    override fun seekToDefaultPosition() {
        seekTo(0L)
    }

    override fun seekToDefaultPosition(mediaItemIndex: Int) {
        seekTo(mediaItemIndex, 0L)
    }

    private fun absolutePosition(relativeMs: Long): Long =
        if (relativeMs == C.TIME_UNSET) relativeMs else positionOffsetMs + relativeMs

    private fun clampToFullDuration(positionMs: Long): Long =
        fullDurationMs?.let { positionMs.coerceIn(0L, it) } ?: positionMs.coerceAtLeast(0L)
}

internal fun shouldRestartHlsPlaybackForSeek(relativeTargetMs: Long, availableDurationMs: Long): Boolean =
    relativeTargetMs < 0L || availableDurationMs == C.TIME_UNSET || relativeTargetMs > availableDurationMs

/** Single-item timeline facade used by [FullDurationPlayer]. */
private class FullDurationTimeline(
    private val delegate: Timeline,
    fullDurationMs: Long,
) : Timeline() {
    private val fullDurationUs = fullDurationMs * 1000L

    override fun getWindowCount(): Int = delegate.windowCount

    override fun getWindow(windowIndex: Int, window: Window, defaultPositionProjectionUs: Long): Window =
        delegate.getWindow(windowIndex, window, defaultPositionProjectionUs).apply {
            durationUs = fullDurationUs
            defaultPositionUs = 0L
            positionInFirstPeriodUs = 0L
            isSeekable = true
            isDynamic = false
            liveConfiguration = null
        }

    override fun getPeriodCount(): Int = delegate.periodCount

    override fun getPeriod(periodIndex: Int, period: Period, setIds: Boolean): Period =
        delegate.getPeriod(periodIndex, period, setIds).apply {
            durationUs = fullDurationUs
            positionInWindowUs = 0L
        }

    override fun getIndexOfPeriod(uid: Any): Int = delegate.getIndexOfPeriod(uid)

    override fun getUidOfPeriod(periodIndex: Int): Any = delegate.getUidOfPeriod(periodIndex)
}

/** Media3 owns the movie/show transport UI, so tint its native buttons with
 * the same white -> cyan -> hot interaction sequence as Compose actions. */
private fun applySwarmPlaybackControlColors(root: ViewGroup) {
    val states = arrayOf(
        intArrayOf(android.R.attr.state_pressed),
        intArrayOf(android.R.attr.state_focused),
        intArrayOf(),
    )
    val colors = intArrayOf(SwarmAccentHot.toArgb(), SwarmAccent.toArgb(), Color.White.toArgb())
    for (index in 0 until root.childCount) {
        when (val child = root.getChildAt(index)) {
            is ImageButton -> child.imageTintList = ColorStateList(states, colors)
            is ViewGroup -> applySwarmPlaybackControlColors(child)
        }
    }
}

@Composable
private fun ContinueOverlay(nextTitle: String?, nextArtworkUrl: String?, onPlayNow: () -> Unit, onCancel: () -> Unit) {
    var secondsLeft by remember { mutableStateOf(CONTINUE_COUNTDOWN_SECS) }
    LaunchedEffect(Unit) {
        while (secondsLeft > 0) {
            delay(1000)
            secondsLeft -= 1
        }
        onPlayNow()
    }
    val playNowFocusRequester = remember { FocusRequester() }
    LaunchedEffect(Unit) { playNowFocusRequester.requestFocus() }

    Box(
        modifier = Modifier.fillMaxSize()
            .background(Color.Black.copy(alpha = 0.78f))
            .testTag(UatTestTags.CONTINUE_OVERLAY),
        contentAlignment = Alignment.BottomEnd,
    ) {
        Column(modifier = Modifier.padding(40.dp).width(340.dp)) {
            Text("Up next", color = SwarmMuted, fontSize = 13.sp)
            Spacer(Modifier.height(4.dp))
            Text(nextTitle ?: "Next episode", color = SwarmText, fontSize = 18.sp, fontWeight = FontWeight.Bold)
            Spacer(Modifier.height(12.dp))
            ArtworkImage(
                label = nextTitle ?: "Next episode",
                placeholderType = "Show",
                primaryUrl = nextArtworkUrl,
                modifier = Modifier.width(140.dp).aspectRatio(16f / 9f).clip(RoundedCornerShape(4.dp)),
            )
            Spacer(Modifier.height(16.dp))
            Row(horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                Button(
                    onClick = onPlayNow,
                    modifier = Modifier.focusRequester(playNowFocusRequester)
                        .testTag(UatTestTags.CONTINUE_PLAY_NOW_BUTTON),
                    colors = swarmActionButtonColors(),
                ) {
                    Text("Play now ($secondsLeft)", color = Color(0xFF04263A), fontSize = 14.sp, fontWeight = FontWeight.Bold)
                }
                Button(
                    onClick = onCancel,
                    colors = swarmActionButtonColors(),
                    modifier = Modifier.testTag(UatTestTags.CONTINUE_CANCEL_BUTTON),
                ) {
                    Text("Cancel", fontSize = 14.sp)
                }
            }
        }
    }
}
