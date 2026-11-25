package app.swarm.tv.app.ui.screens

import app.swarm.tv.core.peer.PlaybackMode
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

class PlayerScrubTest {
    @Test
    fun `rewind and fast-forward move in 15 second increments`() {
        assertEquals(15_000L, PLAYBACK_SEEK_STEP_MS)
        assertEquals(
            75_000L,
            coalescedSeekTargetMs(pendingTargetMs = null, currentPositionMs = 60_000L, deltaMs = PLAYBACK_SEEK_STEP_MS),
        )
        assertEquals(
            45_000L,
            coalescedSeekTargetMs(pendingTargetMs = null, currentPositionMs = 60_000L, deltaMs = -PLAYBACK_SEEK_STEP_MS),
        )
    }

    @Test
    fun `holding fast-forward stacks 15 second steps onto the pending target`() {
        var pending: Long? = null
        repeat(4) {
            pending = coalescedSeekTargetMs(pending, currentPositionMs = 0L, deltaMs = PLAYBACK_SEEK_STEP_MS)
        }
        assertEquals(60_000L, pending)
    }

    @Test
    fun `fast-forward stops at the known end of the media`() {
        assertEquals(
            6_600_000L,
            coalescedSeekTargetMs(
                pendingTargetMs = 6_590_000L,
                currentPositionMs = 0L,
                deltaMs = PLAYBACK_SEEK_STEP_MS,
                durationMs = 6_600_000L,
            ),
        )
        // Unknown duration: no upper clamp.
        assertEquals(
            6_605_000L,
            coalescedSeekTargetMs(6_590_000L, 0L, PLAYBACK_SEEK_STEP_MS, durationMs = null),
        )
    }

    @Test
    fun `first press always steps and held repeats are paced`() {
        assertTrue(shouldApplySeekStep(repeatCount = 0, eventTimeMs = 1_000L, lastAppliedEventTimeMs = 990L))
        assertTrue(shouldApplySeekStep(repeatCount = 3, eventTimeMs = 1_000L, lastAppliedEventTimeMs = null))
        assertFalse(
            shouldApplySeekStep(
                repeatCount = 2,
                eventTimeMs = 1_000L,
                lastAppliedEventTimeMs = 1_000L - SCRUB_REPEAT_INTERVAL_MS + 1L,
            ),
        )
        assertTrue(
            shouldApplySeekStep(
                repeatCount = 2,
                eventTimeMs = 1_000L,
                lastAppliedEventTimeMs = 1_000L - SCRUB_REPEAT_INTERVAL_MS,
            ),
        )
    }

    @Test
    fun `playback clock uses hours only when needed`() {
        assertEquals("0:00", formatPlaybackClock(0L))
        assertEquals("0:00", formatPlaybackClock(-5_000L))
        assertEquals("2:05", formatPlaybackClock(125_999L))
        assertEquals("1:02:03", formatPlaybackClock(3_723_000L))
    }

    @Test
    fun `timeline fraction is clamped and unknown without a duration`() {
        assertEquals(0.5f, scrubProgressFraction(50_000L, 100_000L))
        assertEquals(1f, scrubProgressFraction(150_000L, 100_000L))
        assertEquals(0f, scrubProgressFraction(-1L, 100_000L))
        assertNull(scrubProgressFraction(50_000L, null))
        assertNull(scrubProgressFraction(50_000L, 0L))
    }

    @Test
    fun `preview cache buckets nearby targets together`() {
        assertEquals(60_000L, scrubPreviewBucketMs(64_999L))
        assertEquals(65_000L, scrubPreviewBucketMs(65_000L))
        assertEquals(0L, scrubPreviewBucketMs(-10L))
    }

    @Test
    fun `scene previews are only decoded from direct-play loopback proxy streams`() {
        assertTrue(supportsScrubPreview(PlaybackMode.DIRECT, "http://127.0.0.1:43210/dev-1/media/abc"))
        assertTrue(supportsScrubPreview(PlaybackMode.DIRECT, "http://127.0.0.1:43210/dev-1/media/abc?session=s1"))
        assertFalse(supportsScrubPreview(PlaybackMode.HLS, "http://127.0.0.1:43210/dev-1/hls/index.m3u8"))
        assertFalse(supportsScrubPreview(PlaybackMode.DIRECT, "file:///sdcard/movie.mkv"))
        assertFalse(supportsScrubPreview(PlaybackMode.DIRECT, "content://media/external/video/1"))
        assertFalse(supportsScrubPreview(PlaybackMode.DIRECT, "not a url"))
        assertFalse(supportsScrubPreview(PlaybackMode.DIRECT, ""))
    }

    @Test
    fun `scene previews never fetch from a host other than the loopback proxy (#508)`() {
        val rejected = listOf(
            // Remote hosts, in either scheme.
            "http://example.test/dev-1/media/abc",
            "https://example.test/dev-1/media/abc",
            "HTTPS://example.test/media",
            "http://192.168.0.10:8080/dev-1/media/abc",
            // Loopback, but not the proxy's exact shape.
            "https://127.0.0.1:43210/dev-1/media/abc",
            "http://localhost:43210/dev-1/media/abc",
            "http://[::1]:43210/dev-1/media/abc",
            "http://127.0.0.2:43210/dev-1/media/abc",
            "http://127.0.0.1/dev-1/media/abc",
            "http://127.0.0.1:0/dev-1/media/abc",
            // Authority tricks that put a different real host behind the
            // literal "127.0.0.1" text.
            "http://127.0.0.1:43210@example.test/dev-1/media/abc",
            "http://127.0.0.1@example.test:43210/dev-1/media/abc",
            "http://user:pw@127.0.0.1:43210/dev-1/media/abc",
            "http://127.0.0.1.example.test:43210/dev-1/media/abc",
            "http://0x7f000001:43210/dev-1/media/abc",
            "http://2130706433:43210/dev-1/media/abc",
            // Malformed or traversal paths, fragments, opaque/relative forms.
            "http://127.0.0.1:43210/",
            "http://127.0.0.1:43210/dev-1",
            "http://127.0.0.1:43210//media/abc",
            "http://127.0.0.1:43210/dev-1/../other/media",
            "http://127.0.0.1:43210/dev-1/./media",
            "http://127.0.0.1:43210/dev-1/media/abc#frag",
            "http:127.0.0.1:43210/dev-1/media/abc",
            "//127.0.0.1:43210/dev-1/media/abc",
            "/dev-1/media/abc",
        )
        rejected.forEach { url ->
            assertFalse(isLoopbackProxyStreamUrl(url), "must not open a preview retriever for $url")
            assertFalse(supportsScrubPreview(PlaybackMode.DIRECT, url), "must not preview $url")
        }
    }

    @Test
    fun `pre-8_1 full-size preview decode refuses oversized or crafted dimensions (#508)`() {
        assertTrue(scrubPreviewFullFrameAllowed("1920", "1080"))
        assertTrue(scrubPreviewFullFrameAllowed("1280", "720"))
        assertTrue(scrubPreviewFullFrameAllowed(" 1920 ", "1088"))
        assertFalse(scrubPreviewFullFrameAllowed("3840", "2160"))
        assertFalse(scrubPreviewFullFrameAllowed("2560", "1440"))
        assertFalse(scrubPreviewFullFrameAllowed("16385", "1"))
        assertFalse(scrubPreviewFullFrameAllowed("100000", "10"))
        assertFalse(scrubPreviewFullFrameAllowed("9223372036854775807", "9223372036854775807"))
        assertFalse(scrubPreviewFullFrameAllowed("-1920", "-1080"))
        assertFalse(scrubPreviewFullFrameAllowed("0", "1080"))
        assertFalse(scrubPreviewFullFrameAllowed(null, "1080"))
        assertFalse(scrubPreviewFullFrameAllowed("1920", null))
        assertFalse(scrubPreviewFullFrameAllowed("wide", "1080"))
        assertFalse(scrubPreviewFullFrameAllowed("", ""))
    }

    @Test
    fun `preview time never overflows for runaway targets (#508)`() {
        assertEquals(0L, scrubPreviewTimeUs(-5_000L))
        assertEquals(65_000_000L, scrubPreviewTimeUs(65_000L))
        val huge = scrubPreviewTimeUs(Long.MAX_VALUE)
        assertTrue(huge > 0L, "clamped time must stay positive, was $huge")
    }
}
