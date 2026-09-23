/**
 * Page title for the "Browse All" full grids (#353). The originating
 * Movies/Shows/Music or genre-sub-shelf name is shown the same way
 * [CatalogScreen]'s shelf headers present it, so opening a row's Browse All
 * tile does not drop the category the viewer just selected.
 *
 * This is a label only — the remote's physical Back button still dismisses
 * the screen, matching the rest of the TV client (no on-screen Back).
 */
package app.swarm.tv.app.ui.screens

import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import app.swarm.tv.app.ui.UatTestTags
import app.swarm.tv.app.ui.theme.SwarmMuted

/** Matches CatalogScreen's top-level shelf title size. */
private val BROWSE_ALL_TITLE_SIZE = 20.sp

@Composable
internal fun BrowseAllScreenTitle(title: String) {
    Text(
        title,
        color = SwarmMuted,
        fontSize = BROWSE_ALL_TITLE_SIZE,
        fontWeight = FontWeight.Black,
        modifier = Modifier
            .padding(start = 12.dp, top = 32.dp, end = 12.dp, bottom = 22.dp)
            .testTag(UatTestTags.BROWSE_ALL_TITLE),
    )
}
