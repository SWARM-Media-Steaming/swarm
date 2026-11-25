package app.swarm.tv.app.uat

import android.view.KeyEvent
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.onNodeWithTag
import app.swarm.tv.app.ui.UatTestTags
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Scenario 12: movie playback and the pause overlay.
 *
 * Two known deviations from the literal scenario wording, confirmed in the
 * plan and the swarm-tv-uat-suite skill:
 *  - "reviews" doesn't exist as a field — [UatTestTags.PAUSE_METADATA] is
 *    one joined text line (year, duration, content rating, community
 *    rating+votes, resolution); this test asserts against that line rather
 *    than a nonexistent reviews field.
 *  - A movie's pause overlay correctly has NO Next Episode button (the app
 *    only shows it for episodes — see [ShowPlaybackPauseUatTest] for that
 *    assertion) — this test asserts its absence here.
 *
 * "Settings" has no dedicated pause-overlay control in the app today; the
 * closest real affordance is the audio/subtitle track picker, which this
 * test exercises in its place (see the skill for the full rationale).
 */
class MoviePlaybackPauseUatTest : UatTestBase() {

    @Test
    fun testPlaybackAndPauseOverlay() {
        navigateDownUntilTag(UatTestTags.SHELF_MOVIES)
        val movieTag = requireNotNull(composeTestRule.firstTagStartingWith(UatTestTags.CARD_MOVIE_PREFIX))
        selectTagWithDpad(movieTag)
        waitForTag(UatTestTags.MOVIE_DETAIL_ARTWORK)
        selectTagWithDpad(UatTestTags.MOVIE_DETAIL_PLAY_BUTTON)
        waitForTag(UatTestTags.PLAYER_SURFACE)
        focusTag(UatTestTags.PLAYER_SURFACE)

        // Plays for the first 30 seconds, per the scenario spec.
        Thread.sleep(30_000)

        pressDpadRight() // fast-forward
        Thread.sleep(1_000)
        pressDpadLeft() // rewind
        Thread.sleep(1_000)

        pressSelect() // opens the pause overlay
        waitForTag(UatTestTags.PAUSE_LABEL)
        composeTestRule.onNodeWithTag(UatTestTags.PAUSE_TITLE).assertIsDisplayed()
        composeTestRule.onNodeWithTag(UatTestTags.PAUSE_METADATA).assertIsDisplayed()
        composeTestRule.onNodeWithTag(UatTestTags.PAUSE_GENRES).assertIsDisplayed()
        composeTestRule.onNodeWithTag(UatTestTags.PAUSE_CAST).assertIsDisplayed()
        composeTestRule.onNodeWithTag(UatTestTags.PAUSE_DESCRIPTION).assertIsDisplayed()
        composeTestRule.onNodeWithTag(UatTestTags.PAUSE_AUDIO_TRACK_PICKER).assertIsDisplayed()
        composeTestRule.onNodeWithTag(UatTestTags.PAUSE_SUBTITLE_TRACK_PICKER).assertIsDisplayed()
        composeTestRule.onNodeWithTag(UatTestTags.PAUSE_RESUME_BUTTON).assertIsDisplayed()
        assertTrue(
            "a movie's pause overlay should have no Next Episode button",
            composeTestRule.allTagsStartingWith(UatTestTags.PAUSE_NEXT_EPISODE_BUTTON).isEmpty(),
        )

        selectTagWithDpad(UatTestTags.PAUSE_RESUME_BUTTON)
        composeTestRule.waitUntil(timeoutMillis = 5_000) {
            composeTestRule.allTagsStartingWith(UatTestTags.PAUSE_LABEL).isEmpty()
        }

        device.pressKeyCode(KeyEvent.KEYCODE_MEDIA_PLAY_PAUSE)
        waitForTag(UatTestTags.PAUSE_LABEL)
        selectTagWithDpad(UatTestTags.PAUSE_RESUME_BUTTON)
        composeTestRule.waitUntil(timeoutMillis = 5_000) {
            composeTestRule.allTagsStartingWith(UatTestTags.PAUSE_LABEL).isEmpty()
        }

        pressSelect()
        waitForTag(UatTestTags.PAUSE_LABEL)
        // PAUSE_AUDIO_TRACK_PICKER tags the dropdown button (issue #497);
        // its options (PAUSE_AUDIO_OPTION_PREFIX + index) only exist once it
        // is opened. A movie with no reported audio tracks shows "None
        // reported" text instead, so only interact with an option if one
        // appears after opening.
        selectTagWithDpad(UatTestTags.PAUSE_AUDIO_TRACK_PICKER)
        composeTestRule.firstTagStartingWith(UatTestTags.PAUSE_AUDIO_OPTION_PREFIX)?.let { optionTag ->
            selectTagWithDpad(optionTag)
        }
        waitForTag(UatTestTags.PAUSE_LABEL)

        // Issue #505: the Audio Boost dropdown. Exactly one open row carries
        // the dropdown's own check mark (labels can never fake one), and
        // pressing a row applies that row's level and collapses the list.
        // "Off" (row 0) is chosen so the persisted boost stays at its default.
        selectTagWithDpad(UatTestTags.PAUSE_AUDIO_BOOST_PICKER)
        waitForTag(UatTestTags.PAUSE_AUDIO_BOOST_OPTION_PREFIX + "0")
        val boostRows = composeTestRule.allTagsStartingWith(UatTestTags.PAUSE_AUDIO_BOOST_OPTION_PREFIX)
        assertEquals("every supported Audio Boost level should be offered", 4, boostRows.size)
        val checkedRows = boostRows.count { tag ->
            composeTestRule.onNodeWithTag(tag).fetchSemanticsNode().config
                .getOrElse(SemanticsProperties.Text) { emptyList() }
                .any { it.text.contains('\u2713') }
        }
        assertEquals("exactly one Audio Boost row should show the selected mark", 1, checkedRows)
        selectTagWithDpad(UatTestTags.PAUSE_AUDIO_BOOST_OPTION_PREFIX + "0")
        composeTestRule.waitUntil(timeoutMillis = 5_000) {
            composeTestRule.allTagsStartingWith(UatTestTags.PAUSE_AUDIO_BOOST_OPTION_PREFIX).isEmpty()
        }
        assertTrue(
            "the collapsed Audio Boost button should now read Off",
            composeTestRule.onNodeWithTag(UatTestTags.PAUSE_AUDIO_BOOST_PICKER).fetchSemanticsNode().config
                .getOrElse(SemanticsProperties.Text) { emptyList() }
                .any { it.text.startsWith("Off") },
        )

        pressBack() // pause overlay/player -> movie detail
        waitForTag(UatTestTags.MOVIE_DETAIL_ARTWORK)
        pressBack() // movie detail -> browse
        waitForTag(UatTestTags.SHELF_MOVIES)
    }
}
