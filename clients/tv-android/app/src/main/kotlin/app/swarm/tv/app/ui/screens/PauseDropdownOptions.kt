package app.swarm.tv.app.ui.screens

/**
 * Pure model behind the pause-screen dropdown pickers (issues #497, #505).
 *
 * Option labels for the audio-language and subtitle pickers come straight
 * from the media container's track metadata (`Format.label`), which anyone
 * who authored the file controls. The dropdown draws its own selection and
 * open/closed markers next to those labels, so they are cleaned before
 * display: a crafted track name must not be able to fake the "selected"
 * check mark, reorder the row with bidi controls, break the line, or push
 * the real markers off-screen with an endless name.
 */
internal data class PauseDropdownOption<T>(
    val label: String,
    val value: T,
    val isSelected: Boolean,
)

internal const val PAUSE_DROPDOWN_SELECTED_MARK = "✓"
internal const val PAUSE_DROPDOWN_OPEN_MARK = "▴"
internal const val PAUSE_DROPDOWN_CLOSED_MARK = "▾"

/** Longest label shown in a dropdown row before it is cut with an ellipsis. */
internal const val PAUSE_DROPDOWN_MAX_LABEL_CHARS = 48

/** Glyphs the dropdown itself uses as markers, plus look-alikes of them. */
private val MARKER_LOOKALIKES = setOf(
    '✓', '✔', '✅', '☑', '☒', '✗', '✘',
    '▴', '▾', '▲', '▼', '▵', '▿', '▶', '◀',
)

private fun isInvisibleOrDirectional(ch: Char): Boolean {
    val type = Character.getType(ch)
    return Character.isISOControl(ch) ||
        type == Character.FORMAT.toInt() || // bidi overrides/isolates, zero-width chars, BOM
        type == Character.LINE_SEPARATOR.toInt() ||
        type == Character.PARAGRAPH_SEPARATOR.toInt()
}

/**
 * Display form of an untrusted option label: control, bidi-override,
 * zero-width and line/paragraph-separator characters are removed, the
 * dropdown's own marker glyphs (and look-alikes) are removed, whitespace is
 * collapsed, and the result is capped at [PAUSE_DROPDOWN_MAX_LABEL_CHARS]
 * without splitting a surrogate pair. A label that cleans down to nothing
 * falls back to [fallback].
 */
internal fun pauseDropdownDisplayLabel(raw: String, fallback: String): String {
    val cleaned = buildString(raw.length) {
        for (ch in raw) {
            when {
                isInvisibleOrDirectional(ch) -> append(' ')
                ch in MARKER_LOOKALIKES -> append(' ')
                Character.isWhitespace(ch) || Character.isSpaceChar(ch) -> append(' ')
                else -> append(ch)
            }
        }
    }.split(' ').filter { it.isNotEmpty() }.joinToString(" ")
    if (cleaned.isEmpty()) return fallback
    if (cleaned.length <= PAUSE_DROPDOWN_MAX_LABEL_CHARS) return cleaned
    var end = PAUSE_DROPDOWN_MAX_LABEL_CHARS - 1
    if (Character.isHighSurrogate(cleaned[end - 1])) end -= 1
    return cleaned.substring(0, end).trimEnd() + "…"
}

/** Index of the option to focus when the list opens: the selected one, else the first. */
internal fun <T> pauseDropdownSelectedIndex(options: List<PauseDropdownOption<T>>): Int =
    options.indexOfFirst { it.isSelected }.takeIf { it >= 0 } ?: 0

/** Text on the collapsed/expanded dropdown button. */
internal fun <T> pauseDropdownButtonLabel(
    options: List<PauseDropdownOption<T>>,
    expanded: Boolean,
    emptyLabel: String,
): String {
    val current = options.getOrNull(pauseDropdownSelectedIndex(options))
        ?.let { pauseDropdownDisplayLabel(it.label, emptyLabel) }
        ?: emptyLabel
    return "$current  ${if (expanded) PAUSE_DROPDOWN_OPEN_MARK else PAUSE_DROPDOWN_CLOSED_MARK}"
}

/**
 * Text on one row of the open list. Only the dropdown adds the check mark,
 * and only for the option that is really selected — the label itself can
 * never carry one (see [pauseDropdownDisplayLabel]).
 */
internal fun <T> pauseDropdownRowLabel(option: PauseDropdownOption<T>, fallback: String): String {
    val label = pauseDropdownDisplayLabel(option.label, fallback)
    return if (option.isSelected) "$PAUSE_DROPDOWN_SELECTED_MARK $label" else label
}

/** Audio Boost options; only the store's supported levels can ever be offered. */
internal fun audioBoostDropdownOptions(levels: List<Int>, current: Int): List<PauseDropdownOption<Int>> =
    levels.map { level ->
        PauseDropdownOption(
            label = if (level == 0) "Off" else "+$level dB",
            value = level,
            isSelected = level == current,
        )
    }
