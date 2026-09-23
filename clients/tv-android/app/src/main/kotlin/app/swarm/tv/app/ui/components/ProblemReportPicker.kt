package app.swarm.tv.app.ui.components

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.tv.material3.Button
import androidx.compose.material3.Text
import app.swarm.tv.app.data.ProblemReportCategory
import app.swarm.tv.app.ui.UatTestTags
import app.swarm.tv.app.ui.theme.SwarmMuted
import app.swarm.tv.app.ui.theme.SwarmSurface
import app.swarm.tv.app.ui.theme.SwarmText

/** A focused, remote-friendly chooser shown before a manual problem report is sent. */
@Composable
fun ProblemReportPicker(
    onReport: (ProblemReportCategory) -> Unit,
    onDismiss: () -> Unit,
) {
    BackHandler(onBack = onDismiss)
    val firstOptionFocusRequester = remember { FocusRequester() }
    LaunchedEffect(Unit) { firstOptionFocusRequester.requestFocus() }

    androidx.compose.foundation.layout.Box(
        modifier = Modifier.fillMaxSize().background(Color.Black.copy(alpha = 0.85f)),
        contentAlignment = Alignment.Center,
    ) {
        Column(
            modifier = Modifier
                .width(400.dp)
                .clip(RoundedCornerShape(16.dp))
                .background(SwarmSurface)
                .padding(28.dp)
                .testTag(UatTestTags.PROBLEM_REPORT_PICKER),
            horizontalAlignment = Alignment.CenterHorizontally,
        ) {
            Text("Report a problem", color = SwarmText, fontSize = 20.sp, fontWeight = FontWeight.Black)
            Spacer(Modifier.height(6.dp))
            Text("What needs attention?", color = SwarmMuted, fontSize = 13.sp)
            Spacer(Modifier.height(18.dp))
            ProblemReportCategory.entries.forEachIndexed { index, category ->
                Button(
                    onClick = { onReport(category) },
                    modifier = Modifier
                        .fillMaxWidth()
                        .then(if (index == 0) Modifier.focusRequester(firstOptionFocusRequester) else Modifier)
                        .testTag(UatTestTags.PROBLEM_REPORT_OPTION_PREFIX + category.name.lowercase()),
                    colors = swarmActionButtonColors(),
                ) {
                    Text(category.label, fontWeight = FontWeight.Bold)
                }
                if (index < ProblemReportCategory.entries.lastIndex) Spacer(Modifier.height(8.dp))
            }
        }
    }
}
