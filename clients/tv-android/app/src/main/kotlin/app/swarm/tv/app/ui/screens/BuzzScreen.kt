package app.swarm.tv.app.ui.screens

import android.media.MediaPlayer
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.tv.material3.Button
import app.swarm.tv.R
import app.swarm.tv.app.ui.components.SwarmLoadingIndicator
import app.swarm.tv.app.ui.components.swarmActionButtonColors
import app.swarm.tv.app.ui.theme.SwarmAccent
import app.swarm.tv.app.ui.theme.SwarmMuted
import app.swarm.tv.app.ui.theme.SwarmSurface
import app.swarm.tv.app.ui.theme.SwarmText
import app.swarm.tv.core.peer.BuzzResponse

/** Remote-first renderer for the structured screen model owned by the server. */
@Composable
fun BuzzScreen(
    response: BuzzResponse?,
    loading: Boolean,
    error: String?,
    onChoice: (String) -> Unit,
    onAction: (String) -> Unit,
    onBack: () -> Unit,
) {
    BackHandler(onBack = onBack)
    BuzzVoice(response?.voiceAsset)
    Row(
        modifier = Modifier.fillMaxSize().background(SwarmSurface).padding(56.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(48.dp),
    ) {
        Image(painterResource(R.drawable.mascot), "Buzz", Modifier.size(220.dp))
        Column(verticalArrangement = Arrangement.Center, modifier = Modifier.weight(1f)) {
            Text("BUZZ", color = SwarmAccent, fontSize = 18.sp, fontWeight = FontWeight.Black)
            Spacer(Modifier.height(12.dp))
            Text(response?.buzzText ?: if (loading) "Let's find you something." else "Buzz is unavailable.", color = SwarmText, fontSize = 32.sp, fontWeight = FontWeight.Bold)
            response?.title?.let { Spacer(Modifier.height(14.dp)); Text(it, color = Color.White, fontSize = 26.sp) }
            response?.reasons?.forEach { Text("• $it", color = SwarmMuted, fontSize = 16.sp) }
            error?.let { Spacer(Modifier.height(12.dp)); Text(it, color = Color(0xffff7777), fontSize = 15.sp) }
            Spacer(Modifier.height(24.dp))
            if (loading) SwarmLoadingIndicator()
            else Row(horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                response?.choices?.forEach { choice ->
                    Button(onClick = { onChoice(choice.id) }, colors = swarmActionButtonColors()) { Text(choice.label) }
                }
                response?.actions?.forEach { action ->
                    Button(onClick = { onAction(action) }, colors = swarmActionButtonColors()) {
                        Text(action.replace('_', ' ').uppercase())
                    }
                }
            }
        }
    }
}

/** Optional packaged voice playback. Missing/corrupt assets intentionally become silence. */
@Composable
private fun BuzzVoice(asset: String?) {
    val context = LocalContext.current
    DisposableEffect(asset) {
        val player = asset?.let { path ->
            runCatching {
                val descriptor = context.assets.openFd(path)
                MediaPlayer().apply {
                    setDataSource(descriptor.fileDescriptor, descriptor.startOffset, descriptor.length)
                    descriptor.close()
                    prepare()
                    start()
                }
            }.getOrNull()
        }
        onDispose { player?.release() }
    }
}
