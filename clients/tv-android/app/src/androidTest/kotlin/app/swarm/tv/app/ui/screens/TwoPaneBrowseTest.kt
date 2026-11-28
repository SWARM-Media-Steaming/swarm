package app.swarm.tv.app.ui.screens

import android.view.KeyEvent
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsFocused
import androidx.compose.ui.test.assertIsSelected
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.test.platform.app.InstrumentationRegistry
import androidx.test.uiautomator.UiDevice
import app.swarm.tv.app.ui.UatTestTags
import app.swarm.tv.app.ui.theme.SwarmTvTheme
import app.swarm.tv.core.catalog.AlbumGroup
import app.swarm.tv.core.catalog.ArtistGroup
import app.swarm.tv.core.catalog.MergedEntry
import app.swarm.tv.core.catalog.SeasonGroup
import app.swarm.tv.core.catalog.ShowGroup
import app.swarm.tv.core.peer.CatalogEntry
import app.swarm.tv.core.peer.MediaKind
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test

class TwoPaneBrowseTest {
    @get:Rule val compose = createComposeRule()

    @Test
    fun episodeFocusCrossesSeasonsAndReturnsToTheActiveSeason() {
        val first = episode("first", 1)
        val extra = episode("extra", 1, extraType = "featurette")
        val second = episode("second", 2)
        val special = episode("special", 0)
        val show = ShowGroup("Show", listOf(
            SeasonGroup(0, listOf(special)),
            SeasonGroup(1, listOf(first, extra)),
            SeasonGroup(2, listOf(second)),
        ))
        var played: MergedEntry? = null
        compose.setContent {
            var selectedSeason by remember { mutableStateOf<SeasonGroup?>(null) }
            SwarmTvTheme {
                SeasonScreen(
                    show = show,
                    seasonArtworkUrl = { null },
                    episodeArtworkUrl = { null },
                    onPlayEpisode = { played = it },
                    onBack = { error("Back should return from episodes to the season list") },
                    selectedSeason = selectedSeason,
                    onSelectSeason = { selectedSeason = it },
                    isWatchlisted = false,
                    onToggleWatchlist = {},
                )
            }
        }

        season(1).assertIsFocused()
        press(KeyEvent.KEYCODE_DPAD_RIGHT)
        episodeRow(first).assertIsFocused()
        press(KeyEvent.KEYCODE_DPAD_DOWN)
        episodeRow(extra).assertIsFocused()
        press(KeyEvent.KEYCODE_DPAD_DOWN)
        episodeRow(second).assertIsFocused()
        season(2).assertIsSelected()
        press(KeyEvent.KEYCODE_DPAD_DOWN)
        episodeRow(special).assertIsFocused()
        season(0).assertIsSelected()
        press(KeyEvent.KEYCODE_DPAD_UP)
        episodeRow(second).assertIsFocused()

        press(KeyEvent.KEYCODE_DPAD_LEFT)
        season(2).assertIsFocused()
        press(KeyEvent.KEYCODE_DPAD_CENTER)
        episodeRow(second).assertIsFocused()
        press(KeyEvent.KEYCODE_DPAD_CENTER)
        compose.runOnIdle { assertEquals(second, played) }
        press(KeyEvent.KEYCODE_BACK)
        season(2).assertIsFocused()
    }

    @Test
    fun albumFocusPreviewsTracksAndPlayerReturnFocusesTheActiveTrack() {
        val a = AlbumGroup("First album", listOf(entry("a", MediaKind.TRACK)))
        val b = AlbumGroup("Second album", (1..25).map { entry("b$it", MediaKind.TRACK) })
        val artist = ArtistGroup("Artist", listOf(a, b))
        val active = b.tracks.last()
        val returnFromPlayer = mutableStateOf(false)
        compose.setContent {
            SwarmTvTheme {
                AlbumScreen(
                    artist = artist,
                    artworkUrl = { null },
                    onPlay = {},
                    onBack = { error("Back should return from tracks to the album list") },
                    initialAlbumKey = b.album.takeIf { returnFromPlayer.value },
                    activeTrackKey = active.entry.entryKey.takeIf { returnFromPlayer.value },
                )
            }
        }

        album(a).assertIsFocused()
        trackRow(a.tracks.first()).assertIsDisplayed()
        press(KeyEvent.KEYCODE_DPAD_DOWN)
        album(b).assertIsFocused()
        trackRow(a.tracks.first()).assertDoesNotExist()
        trackRow(b.tracks.first()).assertIsDisplayed()
        press(KeyEvent.KEYCODE_DPAD_RIGHT)
        trackRow(b.tracks.first()).assertIsFocused()
        press(KeyEvent.KEYCODE_DPAD_LEFT)
        album(b).assertIsFocused()

        compose.runOnIdle { returnFromPlayer.value = true }
        trackRow(active).assertIsFocused()
        album(b).assertIsSelected()
        press(KeyEvent.KEYCODE_BACK)
        album(b).assertIsFocused()
    }

    private fun press(keyCode: Int) {
        UiDevice.getInstance(InstrumentationRegistry.getInstrumentation()).pressKeyCode(keyCode)
        compose.waitForIdle()
    }

    private fun season(number: Int) = compose.onNodeWithTag(UatTestTags.SEASON_CARD_PREFIX + number)
    private fun episodeRow(entry: MergedEntry) =
        compose.onNodeWithTag(UatTestTags.EPISODE_ITEM_PREFIX + entry.entry.entryKey)
    private fun album(group: AlbumGroup) = compose.onNodeWithTag(UatTestTags.ALBUM_CARD_PREFIX + group.album)
    private fun trackRow(entry: MergedEntry) =
        compose.onNodeWithTag(UatTestTags.TRACK_ROW_PREFIX + entry.entry.entryKey)

    private fun episode(key: String, season: Int, extraType: String? = null): MergedEntry {
        val entry = entry(key, MediaKind.EPISODE)
        return entry.copy(entry = entry.entry.copy(season = season, episode = 1, extraType = extraType))
    }

    private fun entry(key: String, kind: MediaKind) = MergedEntry(
        fingerprint = key,
        sources = listOf("fixture"),
        entry = CatalogEntry(entryKey = key, fingerprint = key, kind = kind, title = key, size = 1),
    )
}
