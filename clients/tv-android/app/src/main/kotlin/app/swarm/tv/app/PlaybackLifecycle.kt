package app.swarm.tv.app

import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.rememberUpdatedState
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.media3.common.Player

/**
 * Silences a local Media3 player as soon as the Activity loses the foreground.
 * Home and Power are system-owned keys on Fire TV, so ON_PAUSE is the first
 * reliable app callback for both actions and arrives before ON_STOP.
 */
@Composable
internal fun PausePlayerWhenAppBackgrounded(
    player: Player?,
    onBackgrounded: (() -> Unit)? = null,
) {
    val lifecycleOwner = LocalLifecycleOwner.current
    val latestOnBackgrounded = rememberUpdatedState(onBackgrounded)
    DisposableEffect(lifecycleOwner, player) {
        val observer = LifecycleEventObserver { _, event ->
            if (event == Lifecycle.Event.ON_PAUSE) {
                // Snapshot while the player and its episode identity are
                // still live. stopAllStreaming removes the Player UiState on
                // this same lifecycle event, and a killed Activity is not
                // guaranteed to reach Compose disposal afterward.
                latestOnBackgrounded.value?.invoke()
                player?.pause()
            }
        }
        lifecycleOwner.lifecycle.addObserver(observer)
        onDispose { lifecycleOwner.lifecycle.removeObserver(observer) }
    }
}
