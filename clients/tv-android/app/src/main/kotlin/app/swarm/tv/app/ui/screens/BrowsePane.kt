package app.swarm.tv.app.ui.screens

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyListState
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.withFrameNanos
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusDirection
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.onFocusChanged
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.input.key.Key
import androidx.compose.ui.input.key.KeyEventType
import androidx.compose.ui.input.key.key
import androidx.compose.ui.input.key.onPreviewKeyEvent
import androidx.compose.ui.input.key.type
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.tv.material3.Border
import androidx.tv.material3.Card
import androidx.tv.material3.CardDefaults
import app.swarm.tv.app.ui.theme.SwarmAccent
import app.swarm.tv.app.ui.theme.SwarmMuted
import app.swarm.tv.app.ui.theme.SwarmSurfaceMuted
import app.swarm.tv.app.ui.theme.SwarmText

internal fun Modifier.onPaneDirection(direction: Key, action: () -> Unit): Modifier =
    onPreviewKeyEvent { event ->
        if (event.key != direction) return@onPreviewKeyEvent false
        if (event.type == KeyEventType.KeyDown) action()
        true
    }

internal suspend fun LazyListState.revealBrowseItem(index: Int) {
    val layout = layoutInfo
    val item = layout.visibleItemsInfo.firstOrNull { it.index == index }
    if (item == null || item.offset < layout.viewportStartOffset ||
        item.offset + item.size > layout.viewportEndOffset
    ) {
        scrollToItem(index)
    }
}

internal suspend fun LazyListState.focusBrowseItem(index: Int, requester: FocusRequester) {
    revealBrowseItem(index)
    // Lazy items only attach their requester after layout.
    repeat(10) {
        withFrameNanos {}
        if (runCatching { requester.requestFocus(FocusDirection.Enter) }.getOrDefault(false)) return
    }
}

@Composable
internal fun BrowseGroupRow(
    title: String,
    count: String,
    isDisplayed: Boolean,
    onFocused: () -> Unit,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Card(
        onClick = onClick,
        colors = CardDefaults.colors(
            containerColor = if (isDisplayed) SwarmSurfaceMuted else Color.Transparent,
            contentColor = SwarmText,
            focusedContainerColor = SwarmSurfaceMuted,
            focusedContentColor = SwarmText,
            pressedContainerColor = SwarmSurfaceMuted,
            pressedContentColor = SwarmText,
        ),
        border = CardDefaults.border(
            border = if (isDisplayed) Border(BorderStroke(1.dp, SwarmMuted)) else Border.None,
            focusedBorder = Border(BorderStroke(2.dp, SwarmAccent)),
            pressedBorder = Border(BorderStroke(2.dp, SwarmAccent)),
        ),
        scale = CardDefaults.scale(scale = 1f, focusedScale = 1f, pressedScale = 0.99f),
        modifier = modifier.fillMaxWidth()
            .semantics { selected = isDisplayed }
            .onFocusChanged { if (it.isFocused) onFocused() },
    ) {
        Row(
            modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 12.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text(
                title,
                color = SwarmText,
                fontSize = 16.sp,
                fontWeight = if (isDisplayed) FontWeight.Bold else FontWeight.Medium,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.weight(1f),
            )
            Text(count, color = SwarmMuted, fontSize = 12.sp, maxLines = 1, modifier = Modifier.padding(start = 8.dp))
        }
    }
}
