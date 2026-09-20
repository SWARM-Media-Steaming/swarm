/**
 * An [OutlinedTextField] that only shows the on-screen keyboard once the
 * user explicitly selects it (D-pad center/Enter while it's focused), not
 * just when D-pad focus lands on it while arrowing past. Compose's default
 * `TextField` shows the IME as soon as it gains focus, which is correct on
 * a touchscreen (focus and intent-to-type are the same tap) but wrong on a
 * D-pad remote, where landing focus on a field while navigating around the
 * screen is a completely routine, frequent action with no intent to type —
 * real bug, found live: every field on this screen popped the keyboard up
 * just from being scrolled/arrowed past.
 *
 * Implemented as a `readOnly` toggle: the field starts (and returns to,
 * whenever it loses focus) `readOnly = true`, which Compose never shows
 * the IME for regardless of focus; D-pad center/Enter while focused flips
 * it briefly editable and requests the keyboard. Pressing Done on the IME
 * (or losing focus another way) flips back to read-only and hides it.
 */
package app.swarm.tv.app.ui.components

import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.OutlinedTextFieldDefaults
import androidx.compose.material3.TextFieldColors
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.onFocusChanged
import androidx.compose.ui.input.key.Key
import androidx.compose.ui.input.key.KeyEventType
import androidx.compose.ui.input.key.key
import androidx.compose.ui.input.key.onPreviewKeyEvent
import androidx.compose.ui.input.key.type
import androidx.compose.ui.platform.LocalSoftwareKeyboardController
import androidx.compose.ui.text.input.ImeAction

@Composable
fun TvOutlinedTextField(
    value: String,
    onValueChange: (String) -> Unit,
    modifier: Modifier = Modifier,
    label: @Composable (() -> Unit)? = null,
    placeholder: @Composable (() -> Unit)? = null,
    colors: TextFieldColors = OutlinedTextFieldDefaults.colors(),
    /**
     * Starts editing as soon as D-pad focus arrives. Keep this false for
     * ordinary forms, where merely navigating across a field must not summon
     * the IME; use it for actions whose explicit purpose is text entry.
     */
    startEditingOnFocus: Boolean = false,
    /**
     * Fires when the user presses Done on the IME — the field's own value
     * has already been updated live via [onValueChange] as they typed;
     * this is purely a "they're finished" signal for callers that want to
     * defer some other action (e.g. applying a search filter) until then.
     */
    onSubmit: (() -> Unit)? = null,
) {
    val keyboardController = LocalSoftwareKeyboardController.current
    var isEditing by remember { mutableStateOf(false) }

    // Showing the IME after `readOnly` has recomposed to false is important:
    // requesting it in the same focus callback can race the creation of the
    // editable input connection on Fire TV and silently do nothing.
    LaunchedEffect(isEditing) {
        if (isEditing) keyboardController?.show()
    }

    OutlinedTextField(
        value = value,
        onValueChange = onValueChange,
        label = label,
        placeholder = placeholder,
        singleLine = true,
        readOnly = !isEditing,
        colors = colors,
        keyboardOptions = KeyboardOptions(imeAction = ImeAction.Done),
        keyboardActions = KeyboardActions(onDone = {
            isEditing = false
            keyboardController?.hide()
            onSubmit?.invoke()
        }),
        modifier = modifier
            .onFocusChanged { state ->
                if (state.isFocused && startEditingOnFocus) {
                    isEditing = true
                }
                if (!state.isFocused && isEditing) {
                    isEditing = false
                    keyboardController?.hide()
                }
            }
            .onPreviewKeyEvent { event ->
                if (!isEditing && event.type == KeyEventType.KeyUp && (event.key == Key.DirectionCenter || event.key == Key.Enter)) {
                    isEditing = true
                    true
                } else {
                    false
                }
            },
    )
}
