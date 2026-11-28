package app.swarm.tv.app.uat

import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.onNodeWithTag
import app.swarm.tv.app.ui.UatTestTags
import kotlinx.coroutines.runBlocking
import java.util.Locale
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/** Scenario 14: show seasons/episodes, and the watchlist round-trip on a show. */
class ShowSeasonsEpisodesWatchlistUatTest : UatTestBase() {

    @Test
    fun testSeasonsEpisodesAndWatchlistRoundTrip() {
        // Reaching Shows can mean traversing Continue Watching, Watchlist,
        // Movies, and up to 3 movie-genre sub-shelves first on a real,
        // populated library — the default 5s D-pad-press budget isn't
        // always enough real time to get there.
        navigateDownUntilTag(UatTestTags.SHELF_SHOWS, timeoutMs = 15_000)
        val before = runBlocking { watchlistStore.loadAll() }
        val showTag = requireNotNull(
            composeTestRule.allTagsStartingWith(UatTestTags.CARD_SHOW_PREFIX).firstOrNull { tag ->
                val title = tag.removePrefix(UatTestTags.CARD_SHOW_PREFIX)
                "show:${title.trim().lowercase(Locale.ROOT)}" !in before
            },
        ) { "expected at least one visible show that is not already watchlisted" }
        navigateDownUntilTag(showTag)
        selectTagWithDpad(showTag)
        waitForTag(UatTestTags.SEASON_SCREEN_SHOW_TITLE)

        val seasonTags = composeTestRule.allTagsStartingWith(UatTestTags.SEASON_CARD_PREFIX)
        assertTrue("expected at least one season", seasonTags.isNotEmpty())
        composeTestRule.onNodeWithTag(seasonTags.first()).assertIsDisplayed()
        // Season card text (number + episode count) is a single merged
        // block rather than separately tagged fields — assert it's present
        // and non-blank rather than parsing an exact format.
        assertFalse(
            "season card should show its number/episode count",
            composeTestRule.textUnderTag(seasonTags.first()).isBlank(),
        )

        val watchlistButton = UatTestTags.SEASON_SCREEN_WATCHLIST_BUTTON
        focusTag(watchlistButton)
        pressDpadUp()
        composeTestRule.waitForIdle()
        assertEquals(watchlistButton, composeTestRule.focusedTag())
        selectTagWithDpad(watchlistButton)
        composeTestRule.waitUntil(timeoutMillis = 5_000) {
            runBlocking { watchlistStore.loadAll() }.size == before.size + 1
        }

        pressBack()
        waitForTag(UatTestTags.SHELF_SHOWS)
        // Focus restoration returns to the selected show near the bottom of
        // the lazy catalog. Move toward the newly-added top row so Compose
        // materializes it before asserting its presence.
        navigateUpUntilTag(UatTestTags.ROW_WATCHLIST)
        assertTrue(
            "Watchlist row should be visible after adding the show",
            composeTestRule.allTagsStartingWith(UatTestTags.ROW_WATCHLIST).isNotEmpty(),
        )

        navigateDownUntilTag(showTag)
        selectTagWithDpad(showTag)
        waitForTag(UatTestTags.SEASON_SCREEN_SHOW_TITLE)
        selectTagWithDpad(watchlistButton)
        composeTestRule.waitUntil(timeoutMillis = 5_000) {
            runBlocking { watchlistStore.loadAll() }.size == before.size
        }

        pressBack()
        waitForTag(UatTestTags.SHELF_SHOWS)
        assertTrue(
            "watchlist store should be back to its original size",
            runBlocking { watchlistStore.loadAll() }.size == before.size,
        )

        // Re-open the same show and check episode structure: at least one
        // episode, no duplicate entry keys, non-blank displayed text
        // (contains the episode name, per the scenario spec).
        navigateDownUntilTag(showTag)
        selectTagWithDpad(showTag)
        waitForTag(UatTestTags.SEASON_SCREEN_SHOW_TITLE)
        selectTagWithDpad(seasonTags.first())
        composeTestRule.waitForTagPrefix(UatTestTags.EPISODE_ITEM_PREFIX)

        val episodeTags = composeTestRule.allTagsStartingWith(UatTestTags.EPISODE_ITEM_PREFIX)
        assertTrue("expected at least one episode", episodeTags.isNotEmpty())
        assertEquals("episode entry keys should not repeat", episodeTags.size, episodeTags.distinct().size)
        for (tag in episodeTags) {
            assertFalse("episode item should display its number/name", composeTestRule.textUnderTag(tag).isBlank())
        }
    }
}
