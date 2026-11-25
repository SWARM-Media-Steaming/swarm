package app.swarm.tv.app.ui.screens

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

/** Issue #505: hardening of the #497 pause-screen dropdown against crafted track metadata. */
class PauseDropdownOptionsTest {
    private fun opt(label: String, selected: Boolean = false) = PauseDropdownOption(label, label, selected)

    @Test
    fun `ordinary labels pass through unchanged`() {
        assertEquals("English", pauseDropdownDisplayLabel("English", "Audio"))
        assertEquals("Director's Commentary (5.1)", pauseDropdownDisplayLabel("Director's Commentary (5.1)", "Audio"))
        assertEquals("日本語", pauseDropdownDisplayLabel("日本語", "Audio"))
    }

    @Test
    fun `a track name cannot fake the selected check mark`() {
        val row = pauseDropdownRowLabel(opt("✓ English"), "Audio")
        assertFalse(row.contains('✓'), row)
        assertEquals("English", row)
        for (lookalike in listOf("✔", "✅", "☑")) {
            assertFalse(pauseDropdownDisplayLabel("$lookalike Spanish", "Audio").contains(lookalike))
        }
    }

    @Test
    fun `only the really selected row is checked`() {
        assertEquals("✓ English", pauseDropdownRowLabel(opt("English", selected = true), "Audio"))
        assertEquals("✓ English", pauseDropdownRowLabel(opt("✓✓ English", selected = true), "Audio"))
    }

    @Test
    fun `a track name cannot fake the open or closed arrow`() {
        val button = pauseDropdownButtonLabel(listOf(opt("English ▴", selected = true)), expanded = false, emptyLabel = "None")
        assertEquals("English  $PAUSE_DROPDOWN_CLOSED_MARK", button)
    }

    @Test
    fun `bidi overrides, zero-width and control characters are stripped`() {
        // A right-to-left override would visually reverse everything after it,
        // including the dropdown's own markers; it must not survive.
        assertEquals("English txet", pauseDropdownDisplayLabel("English‮txet‬", "Audio"))
        val cleaned = pauseDropdownDisplayLabel("Eng​lish⁦⁧⁩‎‏﻿\u0000\u0007", "Audio")
        assertEquals("Eng lish", cleaned)
        for (ch in cleaned) {
            assertFalse(Character.getType(ch) == Character.FORMAT.toInt() || Character.isISOControl(ch), "kept U+%04X".format(ch.code))
        }
    }

    @Test
    fun `line breaks and runs of whitespace collapse to single spaces`() {
        assertEquals("English Selected", pauseDropdownDisplayLabel("English\n\r\t    Selected", "Audio"))
    }

    @Test
    fun `labels that clean to nothing fall back`() {
        assertEquals("Audio", pauseDropdownDisplayLabel("", "Audio"))
        assertEquals("Audio", pauseDropdownDisplayLabel("‮​ ✓ \n", "Audio"))
    }

    @Test
    fun `endless labels are capped without splitting a surrogate pair`() {
        val long = pauseDropdownDisplayLabel("A".repeat(5_000), "Audio")
        assertEquals(PAUSE_DROPDOWN_MAX_LABEL_CHARS, long.length)
        assertTrue(long.endsWith("…"))

        val emoji = "🎬" // 🎬, a surrogate pair
        val prefix = "B".repeat(PAUSE_DROPDOWN_MAX_LABEL_CHARS - 2)
        val capped = pauseDropdownDisplayLabel(prefix + emoji + "tail", "Audio")
        assertTrue(capped.length <= PAUSE_DROPDOWN_MAX_LABEL_CHARS)
        assertFalse(Character.isHighSurrogate(capped[capped.length - 2]), "split a surrogate pair: $capped")
        assertTrue(capped.endsWith("…"))
    }

    @Test
    fun `focus starts on the selected option, else the first`() {
        assertEquals(2, pauseDropdownSelectedIndex(listOf(opt("a"), opt("b"), opt("c", true))))
        assertEquals(0, pauseDropdownSelectedIndex(listOf(opt("a"), opt("b"))))
        assertEquals(0, pauseDropdownSelectedIndex(emptyList<PauseDropdownOption<String>>()))
    }

    @Test
    fun `empty option list shows the empty label`() {
        assertEquals("None  $PAUSE_DROPDOWN_OPEN_MARK", pauseDropdownButtonLabel(emptyList<PauseDropdownOption<String>>(), true, "None"))
    }

    @Test
    fun `audio boost offers only the given levels and carries the level itself`() {
        val options = audioBoostDropdownOptions(listOf(0, 3, 6, 10), current = 6)
        assertEquals(listOf(0, 3, 6, 10), options.map { it.value })
        assertEquals(listOf("Off", "+3 dB", "+6 dB", "+10 dB"), options.map { it.label })
        assertEquals(listOf(false, false, true, false), options.map { it.isSelected })
        // An out-of-range stored level selects nothing rather than a wrong row.
        assertTrue(audioBoostDropdownOptions(listOf(0, 3, 6, 10), current = 99).none { it.isSelected })
    }
}
