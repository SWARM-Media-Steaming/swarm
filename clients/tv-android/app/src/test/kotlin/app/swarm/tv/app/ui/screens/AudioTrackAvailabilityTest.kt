package app.swarm.tv.app.ui.screens

import androidx.media3.common.Format
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertNotEquals
import org.junit.jupiter.api.Test

/**
 * #278: a Daria episode with an untagged Spanish + English MKV audio pair
 * always played Spanish with no way to switch — both tracks lacked a
 * language tag and a track name, so they fell back to the same literal
 * label and [distinctByLabel] discarded the second one before it ever
 * reached the pause-screen picker.
 */
class AudioTrackAvailabilityTest {
    private fun formatWith(language: String? = null, label: String? = null): Format =
        Format.Builder().setLanguage(language).setLabel(label).build()

    @Test
    fun `untagged audio tracks get distinct numbered labels instead of colliding`() {
        val first = audioTrackLabel(formatWith(), index = 0)
        val second = audioTrackLabel(formatWith(), index = 1)

        assertEquals("Audio 1", first)
        assertEquals("Audio 2", second)
    }

    @Test
    fun `numbered fallback labels survive distinctByLabel so both tracks stay selectable`() {
        val choices = listOf(
            TrackChoice(label = audioTrackLabel(formatWith(), 0), group = null, trackIndex = 0, isSelected = true),
            TrackChoice(label = audioTrackLabel(formatWith(), 1), group = null, trackIndex = 1, isSelected = false),
        )

        assertEquals(2, choices.distinctByLabel().size)
    }

    @Test
    fun `genuinely duplicate labels still collapse to one choice`() {
        val choices = listOf(
            TrackChoice(label = "English", group = null, trackIndex = 0, isSelected = true),
            TrackChoice(label = "English", group = null, trackIndex = 1, isSelected = false),
        )

        assertEquals(1, choices.distinctByLabel().size)
    }

    @Test
    fun `an explicit track name wins over the numbered fallback`() {
        // Format's language path routes through android.text.TextUtils, which
        // this plain-JVM unit test can't instantiate (no Robolectric) — the
        // label path exercises the same precedence rule without it.
        assertNotEquals("Audio 2", audioTrackLabel(formatWith(label = "Director's Commentary"), index = 1))
    }
}
