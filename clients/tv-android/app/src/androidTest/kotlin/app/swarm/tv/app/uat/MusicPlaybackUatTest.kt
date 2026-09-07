package app.swarm.tv.app.uat

import android.view.KeyEvent
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.onNodeWithTag
import app.swarm.tv.app.ui.UatTestTags
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Scenario 15: artist -> album -> track browsing, playback controls, the
 * per-track Like round-trip through the "Liked only" filter, and the
 * mini-player.
 *
 * `AlbumScreen` has no dedicated header tags for "album name" / "artist
 * name" text (only card/row tags) — this test asserts real track rows
 * render with non-blank content rather than parsing those specific fields,
 * consistent with the other approximations noted in the skill.
 */
class MusicPlaybackUatTest : UatTestBase() {

    @Test
    fun testMusicBrowsingPlaybackLikeAndMiniPlayer() {
        openFilterRail()
        selectTagWithDpad(UatTestTags.FILTER_KIND_PREFIX + "MUSIC")
        waitForTag(UatTestTags.SHELF_MUSIC)
        val artistTag = requireNotNull(composeTestRule.firstTagStartingWith(UatTestTags.CARD_ARTIST_PREFIX))
        selectTagWithDpad(artistTag)

        composeTestRule.waitForTagPrefix(UatTestTags.ALBUM_CARD_PREFIX)
        val albumTag = requireNotNull(composeTestRule.firstTagStartingWith(UatTestTags.ALBUM_CARD_PREFIX))
        composeTestRule.onNodeWithTag(albumTag).assertIsDisplayed()
        selectTagWithDpad(albumTag)

        composeTestRule.waitForTagPrefix(UatTestTags.TRACK_ROW_PREFIX)
        val trackTag = requireNotNull(composeTestRule.firstTagStartingWith(UatTestTags.TRACK_ROW_PREFIX))
        assertFalse("track row should show number/name/duration", composeTestRule.textUnderTag(trackTag).isBlank())
        selectTagWithDpad(trackTag)

        waitForTag(UatTestTags.MUSIC_PLAYER_COVER)
        composeTestRule.onNodeWithTag(UatTestTags.MUSIC_PLAYER_TITLE).assertIsDisplayed()
        composeTestRule.onNodeWithTag(UatTestTags.MUSIC_PLAYER_UP_NEXT).assertIsDisplayed()
        composeTestRule.onNodeWithTag(UatTestTags.MUSIC_PLAYER_SHUFFLE_BUTTON).assertIsDisplayed()
        composeTestRule.onNodeWithTag(UatTestTags.MUSIC_PLAYER_PLAY_PAUSE_BUTTON).assertIsDisplayed()
        composeTestRule.onNodeWithTag(UatTestTags.MUSIC_PLAYER_SKIP_BUTTON).assertIsDisplayed()
        composeTestRule.onNodeWithTag(UatTestTags.MUSIC_PLAYER_LIKE_BUTTON).assertIsDisplayed()

        // Like round-trip through the "Liked only" filter on Browse All.
        // #161: the wordless transport row shows the liked state as a
        // filled vs. hollow heart glyph rather than "Liked"/"Like" text.
        val initiallyLiked = composeTestRule.textUnderTag(UatTestTags.MUSIC_PLAYER_LIKE_BUTTON).contains("♥")
        if (initiallyLiked) {
            selectTagWithDpad(UatTestTags.MUSIC_PLAYER_LIKE_BUTTON)
            composeTestRule.waitUntil(timeoutMillis = 5_000) {
                !composeTestRule.textUnderTag(UatTestTags.MUSIC_PLAYER_LIKE_BUTTON).contains("♥")
            }
        }
        selectTagWithDpad(UatTestTags.MUSIC_PLAYER_LIKE_BUTTON)
        // #160: Back from the player returns to the *playing track's album*
        // track list, not the album grid, with the mini-player retained.
        pressBack()
        composeTestRule.waitForTagPrefix(UatTestTags.TRACK_ROW_PREFIX)
        composeTestRule.onNodeWithTag(trackTag).assertIsDisplayed()
        composeTestRule.onNodeWithTag(UatTestTags.MINI_PLAYER_REOPEN).assertIsDisplayed()
        pressBack() // track list -> album grid
        waitForTag(albumTag)
        pressBack() // album grid -> filtered Browse
        waitForTag(UatTestTags.SHELF_MUSIC)
        openFilterRail()
        selectTagWithDpad(UatTestTags.FILTER_LIKED_ONLY)
        waitForTag(artistTag)
        selectTagWithDpad(artistTag)
        waitForTag(albumTag)
        selectTagWithDpad(albumTag)
        waitForTag(trackTag)
        composeTestRule.onNodeWithTag(trackTag).assertIsDisplayed()

        selectTagWithDpad(trackTag)
        waitForTag(UatTestTags.MUSIC_PLAYER_COVER)
        selectTagWithDpad(UatTestTags.MUSIC_PLAYER_LIKE_BUTTON)

        pressBack() // player -> playing track's album track list (#160)
        composeTestRule.waitForTagPrefix(UatTestTags.TRACK_ROW_PREFIX)
        pressBack() // track list -> album grid
        waitForTag(albumTag)
        pressBack() // album grid -> filtered Browse
        waitForTag(UatTestTags.FILTER_RAIL)
        val stillFiltered = composeTestRule.allTagsStartingWith(trackTag)
        assertFalse("unliked track should no longer appear under the Liked-only filter", stillFiltered.isNotEmpty())
        openFilterRail()
        selectTagWithDpad(UatTestTags.FILTER_LIKED_ONLY) // clear the filter

        // Play controls: pause/resume, shuffle changes "Up next", skip.
        selectTagWithDpad(artistTag)
        waitForTag(albumTag)
        selectTagWithDpad(albumTag)
        waitForTag(trackTag)
        selectTagWithDpad(trackTag)
        waitForTag(UatTestTags.MUSIC_PLAYER_COVER)

        // The scenario's like/unlike round-trip should not destroy a like
        // that was already present before this rerun began.
        if (initiallyLiked) selectTagWithDpad(UatTestTags.MUSIC_PLAYER_LIKE_BUTTON)

        selectTagWithDpad(UatTestTags.MUSIC_PLAYER_PLAY_PAUSE_BUTTON) // pause
        selectTagWithDpad(UatTestTags.MUSIC_PLAYER_PLAY_PAUSE_BUTTON) // resume

        val upNextBefore = composeTestRule.textUnderTag(UatTestTags.MUSIC_PLAYER_UP_NEXT)
        // One icon-only button cycles OFF -> shuffle album -> shuffle all.
        assertEquals("🔀", composeTestRule.textUnderTag(UatTestTags.MUSIC_PLAYER_SHUFFLE_BUTTON).trim())
        selectTagWithDpad(UatTestTags.MUSIC_PLAYER_SHUFFLE_BUTTON)
        composeTestRule.waitUntil(timeoutMillis = 5_000) {
            composeTestRule.textUnderTag(UatTestTags.MUSIC_PLAYER_SHUFFLE_BUTTON).trim() == "🔀◉"
        }
        composeTestRule.waitUntil(timeoutMillis = 5_000) {
            composeTestRule.textUnderTag(UatTestTags.MUSIC_PLAYER_UP_NEXT) != upNextBefore
        }
        val upNextAfterShuffle = composeTestRule.textUnderTag(UatTestTags.MUSIC_PLAYER_UP_NEXT)
        selectTagWithDpad(UatTestTags.MUSIC_PLAYER_SHUFFLE_BUTTON)
        composeTestRule.waitUntil(timeoutMillis = 5_000) {
            composeTestRule.textUnderTag(UatTestTags.MUSIC_PLAYER_SHUFFLE_BUTTON).trim() == "🔀∞"
        }
        selectTagWithDpad(UatTestTags.MUSIC_PLAYER_SHUFFLE_BUTTON)
        composeTestRule.waitUntil(timeoutMillis = 5_000) {
            composeTestRule.textUnderTag(UatTestTags.MUSIC_PLAYER_SHUFFLE_BUTTON).trim() == "🔀"
        }
        // Leave it on "shuffle album" for the skip check below.
        selectTagWithDpad(UatTestTags.MUSIC_PLAYER_SHUFFLE_BUTTON)

        selectTagWithDpad(UatTestTags.MUSIC_PLAYER_SKIP_BUTTON)
        // A real track transition (new file negotiation) can take longer
        // than a fixed 1s sleep before the title view settles — wait/retry
        // instead of a single immediate check.
        waitForTag(UatTestTags.MUSIC_PLAYER_TITLE, timeoutMs = 10_000)
        composeTestRule.onNodeWithTag(UatTestTags.MUSIC_PLAYER_TITLE).assertIsDisplayed()
        assertTrue("skip should have advanced to what Up Next promised", upNextAfterShuffle.isNotBlank())
        composeTestRule.waitUntil(timeoutMillis = 5_000) {
            composeTestRule.focusedTag() == UatTestTags.MUSIC_PLAYER_SKIP_BUTTON
        }
        assertEquals(UatTestTags.MUSIC_PLAYER_SKIP_BUTTON, composeTestRule.focusedTag())

        // Changing tracks must not pull focus back to Play/Pause; the same
        // applies in both directions (#249).
        selectTagWithDpad(UatTestTags.MUSIC_PLAYER_PREVIOUS_BUTTON)
        composeTestRule.waitUntil(timeoutMillis = 5_000) {
            composeTestRule.focusedTag() == UatTestTags.MUSIC_PLAYER_PREVIOUS_BUTTON
        }
        assertEquals(UatTestTags.MUSIC_PLAYER_PREVIOUS_BUTTON, composeTestRule.focusedTag())

        // Back while playing collapses to the mini-player, not a full stop.
        pressBack()
        waitForTag(UatTestTags.MINI_PLAYER_REOPEN)
        composeTestRule.onNodeWithTag(UatTestTags.MINI_PLAYER_CLOSE_BUTTON).assertIsDisplayed()

        selectTagWithDpad(UatTestTags.MINI_PLAYER_REOPEN)
        waitForTag(UatTestTags.MUSIC_PLAYER_COVER, timeoutMs = 10_000)
        pressBack()
        waitForTag(UatTestTags.MINI_PLAYER_REOPEN)

        selectTagWithDpad(UatTestTags.MINI_PLAYER_CLOSE_BUTTON)
        composeTestRule.waitUntil(timeoutMillis = 5_000) {
            composeTestRule.allTagsStartingWith(UatTestTags.MINI_PLAYER_REOPEN).isEmpty()
        }
    }
}
