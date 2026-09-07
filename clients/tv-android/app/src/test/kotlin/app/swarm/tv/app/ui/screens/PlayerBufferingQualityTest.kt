package app.swarm.tv.app.ui.screens

import androidx.media3.common.Player
import app.swarm.tv.core.peer.PlaybackMode
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

class PlayerBufferingQualityTest {
    @Test
    fun `mid-playback HLS buffering starts quality recovery`() {
        assertTrue(
            shouldStartBufferingQualityRecovery(
                playbackState = Player.STATE_BUFFERING,
                hasStartedPlayback = true,
                playWhenReady = true,
                playbackMode = PlaybackMode.HLS,
                hasVideo = true,
            ),
        )
        assertEquals(30_000L, BUFFERING_QUALITY_RECOVERY_MS)
    }

    @Test
    fun `prolonged HLS startup buffering becomes eligible for recovery`() {
        assertTrue(
            shouldStartStartupQualityRecovery(
                Player.STATE_BUFFERING,
                hasStartedPlayback = false,
                playWhenReady = true,
                PlaybackMode.HLS,
            ),
        )
        assertEquals(4_000L, STARTUP_BUFFERING_QUALITY_RECOVERY_MS)
    }

    @Test
    fun `buffering toast is held back for three seconds`() {
        assertEquals(3_000L, BUFFERING_NOTIFICATION_DELAY_MS)
    }

    @Test
    fun `paused direct-play ready and post-start states do not trigger startup recovery`() {
        assertFalse(
            shouldStartStartupQualityRecovery(
                Player.STATE_BUFFERING,
                hasStartedPlayback = false,
                playWhenReady = false,
                PlaybackMode.HLS,
            ),
        )
        assertFalse(
            shouldStartStartupQualityRecovery(
                Player.STATE_BUFFERING,
                hasStartedPlayback = false,
                playWhenReady = true,
                PlaybackMode.DIRECT,
            ),
        )
        assertFalse(
            shouldStartStartupQualityRecovery(
                Player.STATE_READY,
                hasStartedPlayback = false,
                playWhenReady = true,
                PlaybackMode.HLS,
            ),
        )
        assertFalse(
            shouldStartStartupQualityRecovery(
                Player.STATE_BUFFERING,
                hasStartedPlayback = true,
                playWhenReady = true,
                PlaybackMode.HLS,
            ),
        )
    }

    @Test
    fun `paused and direct-play mid-playback buffering do not downgrade`() {
        assertFalse(
            shouldStartBufferingQualityRecovery(
                Player.STATE_BUFFERING,
                hasStartedPlayback = true,
                playWhenReady = false,
                PlaybackMode.HLS,
                hasVideo = true,
            ),
        )
        assertFalse(
            shouldStartBufferingQualityRecovery(
                Player.STATE_BUFFERING,
                hasStartedPlayback = true,
                playWhenReady = true,
                PlaybackMode.DIRECT,
                hasVideo = true,
            ),
        )
        assertFalse(
            shouldStartBufferingQualityRecovery(
                Player.STATE_READY,
                hasStartedPlayback = true,
                playWhenReady = true,
                PlaybackMode.HLS,
                hasVideo = true,
            ),
        )
        assertFalse(
            shouldStartBufferingQualityRecovery(
                Player.STATE_BUFFERING,
                hasStartedPlayback = true,
                playWhenReady = true,
                PlaybackMode.HLS,
                hasVideo = false,
            ),
        )
    }

    @Test
    fun `recovery cap removes twenty percent from selected rendition`() {
        assertEquals(
            4_800_000,
            bufferingRecoveryVideoBitrateCap(
                selectedVideoBitrate = 6_000_000,
                negotiatedMaxBitrate = 8_000_000,
                currentMaxVideoBitrate = Int.MAX_VALUE,
            ),
        )
    }

    @Test
    fun `recovery cap falls back to negotiated ceiling`() {
        assertEquals(
            6_400_000,
            bufferingRecoveryVideoBitrateCap(
                selectedVideoBitrate = null,
                negotiatedMaxBitrate = 8_000_000,
                currentMaxVideoBitrate = Int.MAX_VALUE,
            ),
        )
    }

    @Test
    fun `repeated recovery never loosens an active cap`() {
        assertEquals(
            2_400_000,
            bufferingRecoveryVideoBitrateCap(
                selectedVideoBitrate = 3_000_000,
                negotiatedMaxBitrate = 8_000_000,
                currentMaxVideoBitrate = 4_800_000,
            ),
        )
        assertEquals(
            2_400_000,
            bufferingRecoveryVideoBitrateCap(
                selectedVideoBitrate = 6_000_000,
                negotiatedMaxBitrate = 8_000_000,
                currentMaxVideoBitrate = 2_400_000,
            ),
        )
    }

    @Test
    fun `unknown bitrate cannot create a recovery cap`() {
        assertNull(
            bufferingRecoveryVideoBitrateCap(
                selectedVideoBitrate = null,
                negotiatedMaxBitrate = 0,
                currentMaxVideoBitrate = Int.MAX_VALUE,
            ),
        )
    }
}
