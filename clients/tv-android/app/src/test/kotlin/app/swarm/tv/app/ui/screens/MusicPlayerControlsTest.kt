package app.swarm.tv.app.ui.screens

import app.swarm.tv.core.catalog.ShuffleMode
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Test

class MusicPlayerControlsTest {
    @Test
    fun `Previous restarts the current song once past the threshold`() {
        assertEquals(
            PreviousButtonAction.RESTART_CURRENT,
            previousButtonAction(MUSIC_PREVIOUS_RESTART_THRESHOLD_MS + 1),
        )
        assertEquals(
            PreviousButtonAction.RESTART_CURRENT,
            previousButtonAction(45_000L),
        )
    }

    @Test
    fun `Previous steps to the previous track early in the song`() {
        assertEquals(PreviousButtonAction.PREVIOUS_TRACK, previousButtonAction(0L))
        assertEquals(
            PreviousButtonAction.PREVIOUS_TRACK,
            previousButtonAction(MUSIC_PREVIOUS_RESTART_THRESHOLD_MS),
        )
    }

    @Test
    fun `shuffle cycles are represented by icons without visible mode text`() {
        assertEquals("🔀", shuffleGlyph(ShuffleMode.OFF))
        assertEquals("🔀◉", shuffleGlyph(ShuffleMode.ALBUM))
        assertEquals("🔀∞", shuffleGlyph(ShuffleMode.ALL_SONGS))
    }

    @Test
    fun `shuffle icons retain an accessible mode description`() {
        assertEquals("Shuffle off", shuffleDescription(ShuffleMode.OFF))
        assertEquals("Shuffle album", shuffleDescription(ShuffleMode.ALBUM))
        assertEquals("Shuffle all songs", shuffleDescription(ShuffleMode.ALL_SONGS))
    }
}
