package app.swarm.tv.app.ui.screens

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
    @Test
    fun `untagged audio tracks get distinct numbered labels instead of colliding`() {
        val first = audioTrackLabel(language = null, label = null, index = 0)
        val second = audioTrackLabel(language = null, label = null, index = 1)

        assertEquals("Audio 1", first)
        assertEquals("Audio 2", second)
    }

    @Test
    fun `numbered fallback labels survive distinctByLabel so both tracks stay selectable`() {
        val choices = listOf(
            TrackChoice(label = audioTrackLabel(null, null, 0), group = null, trackIndex = 0, isSelected = true),
            TrackChoice(label = audioTrackLabel(null, null, 1), group = null, trackIndex = 1, isSelected = false),
        )

        assertEquals(2, choices.distinctByLabel().size)
    }

    @Test
    fun `duplicate labels must not imply duplicate audio streams`() {
        val choices = listOf(
            TrackChoice(label = "English", group = null, trackIndex = 0, isSelected = true),
            TrackChoice(label = "English", group = null, trackIndex = 1, isSelected = false),
        )

        val visible = choices.withDistinctAudioLabels()
        assertEquals(2, visible.size)
        assertEquals(listOf("English 1", "English 2"), visible.map { it.label })
    }

    @Test
    fun `an explicit track name wins over the numbered fallback`() {
        assertNotEquals("Audio 2", audioTrackLabel(null, "Director's Commentary", index = 1))
    }

    @Test
    fun `und language sentinel does not hide meaningful track names`() {
        assertEquals("Spanish", audioTrackLabel("und", "Spanish", index = 0))
        assertEquals("English", audioTrackLabel("und", "English", index = 1))
    }

    @Test
    fun `und language and labels receive distinct numbered fallbacks`() {
        assertEquals("Audio 1", audioTrackLabel("und", "UND", index = 0))
        assertEquals("Audio 2", audioTrackLabel("und", "unknown", index = 1))
        assertEquals("Audio 2", audioTrackLabel(null, "und1", index = 1))
        assertEquals("Audio 2", audioTrackLabel(null, "audio_2", index = 1))
    }

    @Test
    fun `english labels are recognized for initial selection`() {
        assertEquals(true, isEnglishAudioLabel("English"))
        assertEquals(true, isEnglishAudioLabel("English 5.1"))
        assertEquals(false, isEnglishAudioLabel("Spanish"))
    }

    @Test
    fun `equivalent language tags share a cross episode preference key`() {
        assertEquals("english", audioLanguageKey("en", null, index = 0))
        assertEquals("english", audioLanguageKey("eng", null, index = 1))
        assertEquals("english", audioLanguageKey("en-US", null, index = 2))
    }

    @Test
    fun `untagged named tracks retain their language as the preference key`() {
        assertEquals("spanish", audioLanguageKey("und", "Spanish", index = 0))
        assertEquals("english", audioLanguageKey(null, "English", index = 1))
    }

    @Test
    fun `fully untagged tracks can still be matched by their stable position`() {
        assertEquals("audio 2", audioLanguageKey(null, null, index = 1))
    }

    @Test
    fun `saved show language wins over the default english track`() {
        val english = TrackChoice("English", null, 0, isSelected = true, audioLanguageKey = "english")
        val spanish = TrackChoice("Spanish", null, 1, isSelected = false, audioLanguageKey = "spanish")

        assertEquals(spanish, preferredAudioTrack(listOf(english, spanish), "spanish"))
    }

    @Test
    fun `english remains the fallback when a show has no usable preference`() {
        val spanish = TrackChoice("Spanish", null, 0, isSelected = true, audioLanguageKey = "spanish")
        val english = TrackChoice("English", null, 1, isSelected = false, audioLanguageKey = "english")

        assertEquals(english, preferredAudioTrack(listOf(spanish, english), null))
        assertEquals(english, preferredAudioTrack(listOf(spanish, english), "french"))
    }
}
