package app.swarm.tv.app.uat

import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.onNodeWithTag
import app.swarm.tv.app.ui.UatTestTags
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Issue #152: a show that was watched and stopped offers a Resume button on
 * its season list, next to Watchlist, independent of the capped Continue
 * Watching row. Starts an episode, leaves it partway, and verifies the
 * button appears on the season list and resumes that episode playing
 * straight away — no second, paused Resume screen (#496).
 */
class SeasonListResumeUatTest : UatTestBase() {

    @Test
    fun testResumeButtonAppearsAndResumesTheUnfinishedEpisode() {
        val beforeStates = runBlocking { watchStateStore.all() }
        try {
            openFilterRail()
            selectTagWithDpad(UatTestTags.FILTER_KIND_PREFIX + "SHOWS")
            waitForTag(UatTestTags.SHELF_SHOWS)
            val showTag = requireNotNull(composeTestRule.firstTagStartingWith(UatTestTags.CARD_SHOW_PREFIX))
            selectTagWithDpad(showTag)
            waitForTag(UatTestTags.SEASON_SCREEN_SHOW_TITLE)

            // No episode has been touched yet: no Resume button.
            assertTrue(
                "Resume should be absent before any episode is started",
                composeTestRule.allTagsStartingWith(UatTestTags.SEASON_SCREEN_RESUME_BUTTON).isEmpty(),
            )

            val seasonTag = requireNotNull(composeTestRule.firstTagStartingWith(UatTestTags.SEASON_CARD_PREFIX))
            selectTagWithDpad(seasonTag)
            composeTestRule.waitForTagPrefix(UatTestTags.EPISODE_ITEM_PREFIX)
            composeTestRule.waitUntil(timeoutMillis = 5_000) {
                composeTestRule.focusedTagStartingWith(UatTestTags.EPISODE_ITEM_PREFIX) != null
            }
            requireNotNull(composeTestRule.focusedTagStartingWith(UatTestTags.EPISODE_ITEM_PREFIX))
            pressSelect() // episodes play immediately on D-pad Center
            device.waitForIdle(250)
            waitForTag(UatTestTags.PLAYER_SURFACE, timeoutMs = 15_000)
            focusTag(UatTestTags.PLAYER_SURFACE)
            Thread.sleep(6_000) // accrue a real, non-zero resume position

            // Back out to the episode grid, then up to the season list.
            exitPlaybackTo(UatTestTags.EPISODE_ITEM_PREFIX)
            composeTestRule.waitUntil(timeoutMillis = 5_000) {
                runBlocking { watchStateStore.all() } != beforeStates
            }
            pressBack()
            waitForTag(UatTestTags.SEASON_SCREEN_SHOW_TITLE)

            // The show was watched and stopped: Resume is now offered.
            waitForTag(UatTestTags.SEASON_SCREEN_RESUME_BUTTON)
            composeTestRule.onNodeWithTag(UatTestTags.SEASON_SCREEN_RESUME_BUTTON).assertIsDisplayed()

            // Resume plays the episode immediately (#496): it must not stop
            // on the preparing cover's Resume button or the paused overlay.
            val savedBeforeResume = runBlocking { watchStateStore.all() }
            selectTagWithDpad(UatTestTags.SEASON_SCREEN_RESUME_BUTTON)
            waitForTag(UatTestTags.PLAYER_SURFACE, timeoutMs = 20_000)
            assertTrue(
                "season Resume must not ask for a second Resume press",
                composeTestRule.allTagsStartingWith(UatTestTags.PREPARING_PLAYBACK_RESUME_BUTTON).isEmpty() &&
                    composeTestRule.allTagsStartingWith(UatTestTags.PAUSE_LABEL).isEmpty(),
            )
            assertFalse(
                "resuming must not discard the saved watch position",
                runBlocking { watchStateStore.all() }.isEmpty() && savedBeforeResume.isNotEmpty(),
            )

            focusTag(UatTestTags.PLAYER_SURFACE)
            Thread.sleep(1_000)
            exitPlaybackTo(UatTestTags.SEASON_SCREEN_SHOW_TITLE)
        } finally {
            restoreWatchStates(beforeStates)
        }
    }
}
