/**
 * Netflix-style rewind/fast-forward scrubbing for [PlayerScreen] (#498).
 *
 * Every rewind/fast-forward press — D-pad left/right on the bare playing
 * surface, or the remote's dedicated transport keys — moves the target by
 * [PLAYBACK_SEEK_STEP_MS] (15 seconds). Holding the button keeps stepping in
 * those same 15-second increments, throttled by [SCRUB_REPEAT_INTERVAL_MS] so
 * Android's fast key-repeat cannot fly through a whole movie in a few
 * seconds. While the target moves, [SeekScrubOverlay] shows a timeline bar of
 * where the target sits in the movie/episode and, for direct-play streams, a
 * small preview frame of the scene at that target ([ScrubPreviewFrames]).
 * The real Player.seekTo still commits only once presses go quiet (#443).
 */
package app.swarm.tv.app.ui.screens

import android.graphics.Bitmap
import android.media.MediaMetadataRetriever
import android.os.Build
import android.util.LruCache
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import app.swarm.tv.app.ui.UatTestTags
import app.swarm.tv.app.ui.theme.SwarmAccent
import app.swarm.tv.app.ui.theme.SwarmBorder
import app.swarm.tv.app.ui.theme.SwarmMuted
import app.swarm.tv.app.ui.theme.SwarmSurface
import app.swarm.tv.app.ui.theme.SwarmText
import app.swarm.tv.core.peer.PlaybackMode
import java.io.Closeable
import java.net.URI
import java.util.Locale
import java.util.concurrent.ExecutorService
import java.util.concurrent.Executors
import kotlinx.coroutines.asCoroutineDispatcher
import kotlinx.coroutines.withContext

/** Minimum gap between two applied steps while a rewind/fast-forward button
 * is held. Android redelivers ACTION_DOWN every ~50 ms during a hold; at one
 * 15-second step per redelivery a 2-hour movie would pass in ~24 seconds and
 * the preview could never keep up. 150 ms paces a hold at ~100 seconds of
 * media per real second. The first press of a gesture is never throttled. */
internal const val SCRUB_REPEAT_INTERVAL_MS = 150L

/** How long the timeline bar stays up after a scrub commits, so the viewer
 * can see where the jump landed before it fades away. */
internal const val SCRUB_BAR_LINGER_MS = 1_500L

/** Preview frames are cached per bucket of this size, so stepping back and
 * forth over the same spot does not decode the same frame twice. */
internal const val SCRUB_PREVIEW_BUCKET_MS = 5_000L

private const val SCRUB_PREVIEW_WIDTH_PX = 320
private const val SCRUB_PREVIEW_HEIGHT_PX = 180
private const val SCRUB_PREVIEW_CACHE_FRAMES = 16

/** True when a rewind/fast-forward key event should move the scrub target.
 * A fresh press ([repeatCount] 0) always applies; a key-repeat while held
 * applies only once [SCRUB_REPEAT_INTERVAL_MS] has passed since the last
 * applied step. */
internal fun shouldApplySeekStep(
    repeatCount: Int,
    eventTimeMs: Long,
    lastAppliedEventTimeMs: Long?,
): Boolean =
    repeatCount == 0 ||
        lastAppliedEventTimeMs == null ||
        eventTimeMs - lastAppliedEventTimeMs >= SCRUB_REPEAT_INTERVAL_MS

/** "1:02:03" for an hour or more, "2:03" below it. Negative/unknown is 0:00. */
internal fun formatPlaybackClock(positionMs: Long): String {
    val totalSecs = positionMs.coerceAtLeast(0L) / 1000L
    val hours = totalSecs / 3600L
    val minutes = (totalSecs % 3600L) / 60L
    val seconds = totalSecs % 60L
    return if (hours > 0L) {
        String.format(Locale.ROOT, "%d:%02d:%02d", hours, minutes, seconds)
    } else {
        String.format(Locale.ROOT, "%d:%02d", minutes, seconds)
    }
}

/** Where [positionMs] sits on a [durationMs]-long timeline, 0..1, or null when
 * the duration is unknown. */
internal fun scrubProgressFraction(positionMs: Long, durationMs: Long?): Float? {
    if (durationMs == null || durationMs <= 0L) return null
    return (positionMs.coerceIn(0L, durationMs).toDouble() / durationMs.toDouble()).toFloat()
}

internal fun scrubPreviewBucketMs(positionMs: Long): Long {
    val position = positionMs.coerceAtLeast(0L)
    return position - position % SCRUB_PREVIEW_BUCKET_MS
}

/** Scene previews are decoded straight from the stream, which only works for
 * a single progressive file (direct play). An HLS URL is a growing playlist
 * that MediaMetadataRetriever cannot open, so HLS scrubbing shows the
 * timeline bar without a picture. Only plain http(s) stream URLs qualify —
 * never a file/content URI. */
internal fun supportsScrubPreview(playbackMode: PlaybackMode, url: String): Boolean {
    if (playbackMode != PlaybackMode.DIRECT) return false
    val scheme = runCatching { URI(url).scheme }.getOrNull()?.lowercase(Locale.ROOT)
    return scheme == "http" || scheme == "https"
}

/**
 * Decodes small preview frames from a direct-play stream for the scrub
 * overlay. All retriever work runs on one private background thread, so a
 * frame request never blocks the UI and two decodes never overlap; a request
 * whose caller has already moved on is skipped before it starts. Any failure
 * (unsupported container, network error) simply yields no preview — the
 * timeline bar works the same either way — and a failed open is not retried
 * for the rest of this session.
 */
internal class ScrubPreviewFrames(private val url: String) : Closeable {
    private val executor: ExecutorService = Executors.newSingleThreadExecutor { runnable ->
        Thread(runnable, "swarm-scrub-preview").apply { isDaemon = true }
    }
    private val dispatcher = executor.asCoroutineDispatcher()
    private val cache = LruCache<Long, Bitmap>(SCRUB_PREVIEW_CACHE_FRAMES)

    // Touched only on [executor]'s thread.
    private var retriever: MediaMetadataRetriever? = null
    private var unavailable = false

    @Volatile
    private var closed = false

    suspend fun frameAt(positionMs: Long): Bitmap? {
        if (closed) return null
        val bucket = scrubPreviewBucketMs(positionMs)
        cache.get(bucket)?.let { return it }
        return runCatching {
            withContext(dispatcher) {
                if (closed) return@withContext null
                cache.get(bucket) ?: decode(bucket)?.also { cache.put(bucket, it) }
            }
        }.getOrElse { error ->
            if (error is kotlinx.coroutines.CancellationException) throw error
            null
        }
    }

    private fun decode(positionMs: Long): Bitmap? {
        val source = openRetriever() ?: return null
        val timeUs = positionMs * 1000L
        return runCatching {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O_MR1) {
                source.getScaledFrameAtTime(
                    timeUs,
                    MediaMetadataRetriever.OPTION_CLOSEST_SYNC,
                    SCRUB_PREVIEW_WIDTH_PX,
                    SCRUB_PREVIEW_HEIGHT_PX,
                )
            } else {
                source.getFrameAtTime(timeUs, MediaMetadataRetriever.OPTION_CLOSEST_SYNC)?.let { full ->
                    val scaled = Bitmap.createScaledBitmap(
                        full,
                        SCRUB_PREVIEW_WIDTH_PX,
                        SCRUB_PREVIEW_HEIGHT_PX,
                        true,
                    )
                    if (scaled !== full) full.recycle()
                    scaled
                }
            }
        }.getOrNull()
    }

    private fun openRetriever(): MediaMetadataRetriever? {
        retriever?.let { return it }
        if (unavailable) return null
        val opened = MediaMetadataRetriever()
        return runCatching {
            opened.setDataSource(url, emptyMap())
            opened
        }.onSuccess { retriever = it }
            .onFailure {
                unavailable = true
                runCatching { opened.release() }
            }
            .getOrNull()
    }

    /** Releases the retriever after any in-flight decode finishes, then stops
     * the worker thread. Safe to call more than once. */
    override fun close() {
        if (closed) return
        closed = true
        runCatching {
            executor.execute {
                runCatching { retriever?.release() }
                retriever = null
                cache.evictAll()
            }
        }
        executor.shutdown()
    }
}

/**
 * Bottom-of-screen scrub feedback: a timeline bar showing where
 * [positionMs] sits within [durationMs], the elapsed and remaining time, and
 * a small preview card riding above the playhead. [preview] is the decoded
 * scene frame when one is available; without it the card collapses to just
 * the time bubble so HLS scrubbing still reads clearly.
 */
@Composable
internal fun SeekScrubOverlay(
    positionMs: Long,
    durationMs: Long?,
    preview: ImageBitmap?,
    modifier: Modifier = Modifier,
) {
    val fraction = scrubProgressFraction(positionMs, durationMs)
    val clock = formatPlaybackClock(positionMs)
    Box(
        modifier = modifier
            .fillMaxWidth()
            .testTag(UatTestTags.PLAYER_SCRUB_OVERLAY)
            .background(
                Brush.verticalGradient(listOf(Color.Transparent, Color.Black.copy(alpha = 0.78f))),
            )
            .padding(start = 42.dp, end = 42.dp, top = 48.dp, bottom = 30.dp),
    ) {
        BoxWithConstraints(modifier = Modifier.fillMaxWidth()) {
            val cardWidth = if (preview != null) 240.dp else 96.dp
            val trackWidth = maxWidth
            val cardStart = ((trackWidth * (fraction ?: 0f)) - cardWidth / 2)
                .coerceIn(0.dp, (trackWidth - cardWidth).coerceAtLeast(0.dp))
            Column(modifier = Modifier.fillMaxWidth()) {
                Column(
                    modifier = Modifier
                        .offset(x = cardStart)
                        .width(cardWidth),
                    horizontalAlignment = Alignment.CenterHorizontally,
                ) {
                    if (preview != null) {
                        Image(
                            bitmap = preview,
                            contentDescription = "Preview at $clock",
                            contentScale = ContentScale.Crop,
                            modifier = Modifier
                                .width(cardWidth)
                                .height(cardWidth * 9f / 16f)
                                .clip(RoundedCornerShape(6.dp))
                                .border(BorderStroke(2.dp, SwarmAccent), RoundedCornerShape(6.dp))
                                .background(SwarmSurface)
                                .testTag(UatTestTags.PLAYER_SCRUB_PREVIEW),
                        )
                        Spacer(modifier = Modifier.height(6.dp))
                    }
                    Text(
                        clock,
                        color = SwarmText,
                        fontSize = 15.sp,
                        fontWeight = FontWeight.Bold,
                        modifier = Modifier
                            .background(Color.Black.copy(alpha = 0.65f), RoundedCornerShape(4.dp))
                            .padding(horizontal = 10.dp, vertical = 3.dp)
                            .testTag(UatTestTags.PLAYER_SCRUB_TIME),
                    )
                }
                Spacer(modifier = Modifier.height(10.dp))
                Box(
                    modifier = Modifier
                        .fillMaxWidth()
                        .height(14.dp),
                    contentAlignment = Alignment.CenterStart,
                ) {
                    Box(
                        modifier = Modifier
                            .fillMaxWidth()
                            .height(5.dp)
                            .clip(RoundedCornerShape(3.dp))
                            .background(SwarmBorder),
                    ) {
                        Box(
                            modifier = Modifier
                                .fillMaxHeight()
                                .fillMaxWidth(fraction ?: 0f)
                                .background(SwarmAccent),
                        )
                    }
                    if (fraction != null) {
                        Box(
                            modifier = Modifier
                                .offset(x = (trackWidth * fraction - 7.dp).coerceIn(0.dp, trackWidth - 14.dp))
                                .size(14.dp)
                                .clip(CircleShape)
                                .background(SwarmAccent),
                        )
                    }
                }
                Spacer(modifier = Modifier.height(6.dp))
                Row(
                    modifier = Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.SpaceBetween,
                ) {
                    Text(clock, color = SwarmMuted, fontSize = 12.sp)
                    if (durationMs != null && durationMs > 0L) {
                        Text(
                            "-" + formatPlaybackClock(durationMs - positionMs.coerceIn(0L, durationMs)),
                            color = SwarmMuted,
                            fontSize = 12.sp,
                        )
                    }
                }
            }
        }
    }
}
