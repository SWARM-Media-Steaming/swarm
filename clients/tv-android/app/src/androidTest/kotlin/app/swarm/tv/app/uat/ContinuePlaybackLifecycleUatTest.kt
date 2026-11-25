package app.swarm.tv.app.uat

import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.onNodeWithTag
import app.swarm.tv.app.ui.UatTestTags
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/** Continue/resume/completion, server release acknowledgement, track pickers, and browse previews. */
class ContinuePlaybackLifecycleUatTest : UatTestBase() {

    @Test
    fun testContinueWatchingResumeAndCompletionRemoval() {
        val beforeStates = runBlocking { watchStateStore.all() }
        try {
            val movieTag = openFirstMovie()
            selectTagWithDpad(UatTestTags.MOVIE_DETAIL_PLAY_BUTTON)
            waitForTag(UatTestTags.PLAYER_SURFACE, timeoutMs = 15_000)
            Thread.sleep(4_000)
            exitPlaybackTo(UatTestTags.MOVIE_DETAIL_ARTWORK)

            composeTestRule.waitUntil(timeoutMillis = 5_000) {
                runBlocking { watchStateStore.all() } != beforeStates
            }
            val afterPartial = runBlocking { watchStateStore.all() }
            val changedFingerprint = requireNotNull(afterPartial.keys.firstOrNull { afterPartial[it] != beforeStates[it] })
            val partial = requireNotNull(afterPartial[changedFingerprint])
            assertTrue("partial playback should save a positive position", partial.positionSecs > 0.0)
            assertFalse("partial playback should not be marked watched", partial.watched)

            pressBack()
            waitForTag(UatTestTags.FILTER_RAIL)
            navigateUpUntilTag(UatTestTags.ROW_CONTINUE_WATCHING)
            val continueTags = composeTestRule.descendantTagsStartingWith(
                UatTestTags.ROW_CONTINUE_WATCHING,
                UatTestTags.CARD_QUICK_ACCESS_PREFIX,
            )
            assertTrue("partial playback should add a Continue Watching card", continueTags.isNotEmpty())
            val partialCount = continueTags.size
            selectTagWithDpad(continueTags.first())
            waitForTag(UatTestTags.PAUSE_LABEL)
            selectTagWithDpad(UatTestTags.PAUSE_RESUME_BUTTON)
            waitForTagGone(UatTestTags.PAUSE_LABEL)
            Thread.sleep(2_000)
            exitPlaybackTo(UatTestTags.FILTER_RAIL)

            navigateDownUntilTag(UatTestTags.SHELF_MOVIES)
            selectTagWithDpad(movieTag)
            waitForTag(UatTestTags.MOVIE_DETAIL_PLAY_BUTTON)
            selectTagWithDpad(UatTestTags.MOVIE_DETAIL_PLAY_BUTTON)
            waitForTag(UatTestTags.PLAYER_SURFACE, timeoutMs = 15_000)
            withActivity { it.seekPlaybackNearEndForUat() }
            composeTestRule.waitUntil(timeoutMillis = 20_000) {
                runBlocking { watchStateStore.get(changedFingerprint) }?.watched == true
            }
            exitPlaybackTo(UatTestTags.MOVIE_DETAIL_ARTWORK)
            pressBack()
            waitForTag(UatTestTags.FILTER_RAIL)
            navigateUpUntilTag(UatTestTags.OPEN_SWARM_BUTTON)
            val completedCount = composeTestRule.countDescendantNodesWithTagPrefix(
                UatTestTags.ROW_CONTINUE_WATCHING,
                UatTestTags.CARD_QUICK_ACCESS_PREFIX,
            )
            assertTrue("completed media should leave Continue Watching", completedCount < partialCount)
        } finally {
            // Restore the exact pre-test map even when an intermediate UI
            // assertion fails, so reruns never accumulate synthetic progress.
            restoreWatchStates(beforeStates)
        }
    }

    @Test
    fun testPlaybackReleaseAcknowledgedAndTrackChoicesWork() {
        val releasesBefore = openMovieWithTrackChoicesAndPause()

        // Each picker is a dropdown (issue #497): open it, then pick an option.
        selectTagWithDpad(UatTestTags.PAUSE_AUDIO_TRACK_PICKER)
        val audioOptions = composeTestRule.allTagsStartingWith(UatTestTags.PAUSE_AUDIO_OPTION_PREFIX)
        assertTrue("playback should expose at least one audio choice", audioOptions.isNotEmpty())
        selectTagWithDpad(audioOptions.last())
        waitForTag(UatTestTags.PAUSE_LABEL)
        selectTagWithDpad(UatTestTags.PAUSE_SUBTITLE_TRACK_PICKER)
        val subtitleOptions = composeTestRule.allTagsStartingWith(UatTestTags.PAUSE_SUBTITLE_OPTION_PREFIX)
        assertTrue("subtitle picker should expose Off plus a real subtitle track", subtitleOptions.size >= 2)
        selectTagWithDpad(subtitleOptions[1])
        waitForTag(UatTestTags.PAUSE_LABEL)
        selectTagWithDpad(UatTestTags.PAUSE_SUBTITLE_TRACK_PICKER)
        selectTagWithDpad(UatTestTags.PAUSE_SUBTITLE_OPTION_PREFIX + 0) // Off

        pressBack()
        waitForTag(UatTestTags.MOVIE_DETAIL_ARTWORK)
        composeTestRule.waitUntil(timeoutMillis = 8_000) {
            composeTestRule.allTagsStartingWith(UatTestTags.PLAYBACK_RELEASED_PREFIX) != releasesBefore
        }
    }

    @Test
    fun testBrowsePreviewMovesAndDoesNotBlockPlayback() {
        navigateDownUntilTag(UatTestTags.SHELF_MOVIES)
        val movieTags = composeTestRule.allTagsStartingWith(UatTestTags.CARD_MOVIE_PREFIX)
        assertTrue("preview test needs two visible movies", movieTags.size >= 2)

        focusTag(movieTags[0])
        composeTestRule.waitForTagPrefix(UatTestTags.BROWSE_PREVIEW_PREFIX, timeoutMs = 12_000)
        val firstPreview = requireNotNull(composeTestRule.firstTagStartingWith(UatTestTags.BROWSE_PREVIEW_PREFIX))
        focusTag(movieTags[1])
        composeTestRule.waitUntil(timeoutMillis = 12_000) {
            composeTestRule.firstTagStartingWith(UatTestTags.BROWSE_PREVIEW_PREFIX)?.let { it != firstPreview } == true
        }
        val secondPreview = requireNotNull(composeTestRule.firstTagStartingWith(UatTestTags.BROWSE_PREVIEW_PREFIX))
        assertNotEquals(firstPreview, secondPreview)

        selectTagWithDpad(movieTags[1])
        waitForTag(UatTestTags.MOVIE_DETAIL_PLAY_BUTTON)
        waitForTagGone(firstPreview)
        selectTagWithDpad(UatTestTags.MOVIE_DETAIL_PLAY_BUTTON)
        waitForTag(UatTestTags.PLAYER_SURFACE, timeoutMs = 15_000)
        composeTestRule.onNodeWithTag(UatTestTags.PLAYER_SURFACE).assertIsDisplayed()
        exitPlaybackTo(UatTestTags.MOVIE_DETAIL_ARTWORK)
    }

    private fun openFirstMovie(): String {
        navigateDownUntilTag(UatTestTags.SHELF_MOVIES)
        val movieTag = requireNotNull(composeTestRule.firstTagStartingWith(UatTestTags.CARD_MOVIE_PREFIX))
        selectTagWithDpad(movieTag)
        waitForTag(UatTestTags.MOVIE_DETAIL_PLAY_BUTTON)
        return movieTag
    }

    /**
     * Real track choices depend on the specific asset's negotiated ExoPlayer
     * tracks, not something visible before playing — the shelf's first movie
     * has no guarantee of reporting any audio track at all (see the sibling
     * capability gap this exact assumption caused in
     * MoviePlaybackPauseUatTest). Play a bounded number of real candidates
     * from the Movies shelf, back out of ones with too few choices, and stop
     * once a qualifying one is genuinely paused. Returns the
     * PLAYBACK_RELEASED_PREFIX snapshot taken right before opening the pause
     * overlay on the qualifying candidate.
     */
    private fun openMovieWithTrackChoicesAndPause(maxCandidates: Int = 5): List<String> {
        navigateDownUntilTag(UatTestTags.SHELF_MOVIES)
        val candidates = composeTestRule.allTagsStartingWith(UatTestTags.CARD_MOVIE_PREFIX).take(maxCandidates)
        for (candidate in candidates) {
            // One candidate's own transient failure (a slow negotiation, a
            // truly track-less file) must not abort the whole search — catch
            // it, best-effort-recover the navigation state, and try the next
            // candidate instead of failing the test on the first unlucky one.
            val releasesBefore = runCatching {
                navigateDownUntilTag(candidate)
                selectTagWithDpad(candidate)
                waitForTag(UatTestTags.MOVIE_DETAIL_PLAY_BUTTON)
                selectTagWithDpad(UatTestTags.MOVIE_DETAIL_PLAY_BUTTON)
                waitForTag(UatTestTags.PLAYER_SURFACE, timeoutMs = 15_000)
                val releases = composeTestRule.allTagsStartingWith(UatTestTags.PLAYBACK_RELEASED_PREFIX)
                Thread.sleep(2_000)
                pressSelect()
                waitForTag(UatTestTags.PAUSE_LABEL)

                // Track dropdowns (issue #497) only list options while open;
                // opening both is harmless and they collapse on selection.
                selectTagWithDpad(UatTestTags.PAUSE_AUDIO_TRACK_PICKER)
                val hasAudio = composeTestRule.allTagsStartingWith(UatTestTags.PAUSE_AUDIO_OPTION_PREFIX).isNotEmpty()
                pressBack() // collapse the audio dropdown
                selectTagWithDpad(UatTestTags.PAUSE_SUBTITLE_TRACK_PICKER)
                val hasSubtitles = composeTestRule.allTagsStartingWith(UatTestTags.PAUSE_SUBTITLE_OPTION_PREFIX).size >= 2
                pressBack() // collapse the subtitle dropdown
                if (hasAudio && hasSubtitles) releases else null
            }.getOrNull()
            if (releasesBefore != null) return releasesBefore

            recoverToMoviesShelf()
        }
        throw AssertionError(
            "none of the first $maxCandidates visible movies reported both an audio track and a real subtitle track",
        )
    }

    /** Best-effort recovery to the catalog after an unqualified or failed candidate, regardless of what state it left the UI in. */
    private fun recoverToMoviesShelf() {
        runCatching { exitPlaybackTo(UatTestTags.MOVIE_DETAIL_ARTWORK) }
        runCatching { pressBack() }
        runCatching { navigateDownUntilTag(UatTestTags.SHELF_MOVIES, timeoutMs = 15_000) }
    }
}
