/**
 * The SWARM dark palette, ported from the STUN server and Tauri server
 * UIs' shared `:root` CSS variables so all three apps read as one product.
 */
package app.swarm.tv.app.ui.theme

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxScope
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.tv.material3.MaterialTheme
import androidx.tv.material3.darkColorScheme

val SwarmBackground = Color(0xFF101828)
val SwarmSurface = Color(0xFF151F32)
val SwarmSurfaceMuted = Color(0xFF1F2A44)
val SwarmBorder = Color(0xFF31405F)
val SwarmAccent = Color(0xFF00C2FF)
/** Warm interaction accent sampled to match the mascot's gold/yellow details. */
val SwarmAccentHot = Color(0xFFF5C451)
val SwarmError = Color(0xFFFF5D7A)
val SwarmLike = Color(0xFFFF4D67)
val SwarmGreen = Color(0xFF34D399)
val SwarmText = Color(0xFFECF6FF)
val SwarmMuted = Color(0xFF9FB0C9)

private val SwarmColorScheme = darkColorScheme(
    primary = SwarmAccent,
    onPrimary = SwarmBackground,
    secondary = SwarmAccentHot,
    background = SwarmBackground,
    surface = SwarmSurface,
    surfaceVariant = SwarmSurfaceMuted,
    onBackground = SwarmText,
    onSurface = SwarmText,
)

@Composable
fun SwarmTvTheme(content: @Composable () -> Unit) {
    MaterialTheme(colorScheme = SwarmColorScheme, content = content)
}

/**
 * Quiet app-wide backdrop that carries the cyan and gold interaction accents
 * into otherwise empty screen space. The low-opacity stops keep artwork and
 * controls visually dominant while avoiding the flat, single-color canvas.
 */
@Composable
fun SwarmBackdrop(
    modifier: Modifier = Modifier,
    content: @Composable BoxScope.() -> Unit,
) {
    Box(
        modifier = modifier
            .background(SwarmBackground)
            .background(
                Brush.linearGradient(
                    0.0f to SwarmAccent.copy(alpha = 0.11f),
                    0.30f to Color.Transparent,
                    0.72f to Color.Transparent,
                    1.0f to SwarmAccentHot.copy(alpha = 0.08f),
                ),
            ),
        content = content,
    )
}
