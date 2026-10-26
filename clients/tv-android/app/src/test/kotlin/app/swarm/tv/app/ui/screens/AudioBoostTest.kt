package app.swarm.tv.app.ui.screens

import androidx.media3.common.C
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Test

class AudioBoostTest {
    @Test
    fun `gain maps dB levels to millibels`() {
        assertEquals(0, audioBoostGainMillibels(0))
        assertEquals(3000, audioBoostGainMillibels(3))
        assertEquals(6000, audioBoostGainMillibels(6))
        assertEquals(10000, audioBoostGainMillibels(10))
    }

    @Test
    fun `gain is bounded and non-negative`() {
        assertEquals(0, audioBoostGainMillibels(-5))
        assertEquals(10000, audioBoostGainMillibels(20))
    }

    @Test
    fun `status is Off when boost disabled`() {
        assertEquals("Off", audioBoostStatusFor(0, 123))
        assertEquals("Off", audioBoostStatusFor(0, C.AUDIO_SESSION_ID_UNSET))
        assertEquals("Off", audioBoostStatusFor(0, 0))
    }

    @Test
    fun `status is unavailable when session not ready`() {
        assertEquals("Unavailable: audio session not ready", audioBoostStatusFor(6, C.AUDIO_SESSION_ID_UNSET))
        assertEquals("Unavailable: audio session not ready", audioBoostStatusFor(3, 0))
        assertEquals("Unavailable: audio session not ready", audioBoostStatusFor(10, -1))
    }

    @Test
    fun `status is active on decoded audio otherwise`() {
        val status = audioBoostStatusFor(3, 42)
        assertEquals("Active on decoded audio", status)
        assertEquals("Active on decoded audio", audioBoostStatusFor(10, 99))
    }

    @Test
    fun `unavailable failure constant is distinct from off`() {
        assertEquals("Unavailable on this audio output", AUDIO_BOOST_UNAVAILABLE_STATUS)
    }
}
