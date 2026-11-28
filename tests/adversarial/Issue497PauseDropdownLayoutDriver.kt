package app.swarm.tv.app.ui.screens

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.ui.Alignment
import androidx.compose.ui.focus.FocusDirection
import androidx.compose.ui.geometry.Rect
import androidx.compose.ui.layout.AlignmentLine
import androidx.compose.ui.layout.Measurable
import androidx.compose.ui.layout.MeasurePolicy
import androidx.compose.ui.layout.MeasureResult
import androidx.compose.ui.layout.MeasureScope
import androidx.compose.ui.layout.Placeable
import androidx.compose.ui.unit.Constraints
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.IntSize
import androidx.compose.ui.unit.LayoutDirection
import androidx.tv.material3.ButtonDefaults
import kotlin.math.roundToInt
import kotlin.system.exitProcess

/**
 * Adversarial JVM layout + D-pad focus driver for Issue #497:
 *   "On this screen audio boost has multiple buttons. Same with audio language
 *    and subtitles. I want each of these to be replaced with a drop-down that is
 *    used to view multiple values instead of having multiple buttons."
 *
 * Uses:
 * - the real `PauseDropdownOptions.kt` helpers (`PauseDropdownOption`,
 *   `audioBoostDropdownOptions`, `pauseDropdownButtonLabel`,
 *   `pauseDropdownRowLabel`),
 * - Compose Foundation Layout's real `ColumnMeasurePolicy` and
 *   `RowColumnParentData`,
 * - TV Material3's real `ButtonDefaults.ContentPadding`, and
 * - Compose UI's real `TwoDimensionalFocusSearchKt` (`isCandidate` +
 *   `isBetterCandidate`)
 * on a standard 1080p Fire TV viewport (`1920x1080 px` at `xhdpi` `density = 2.0`,
 * i.e. `960.dp x 540.dp`) while `"More like this"` recommendations are present.
 */
private const val DENSITY = 2f
private const val SCREEN_WIDTH_DP = 960f
private const val SCREEN_HEIGHT_DP = 540f

// Conservative single-line text heights at fontScale=1.0 on Android (1sp = 2px at xhdpi;
// Android Roboto ascent+descent adds ~2-4dp, so 1.25x is a conservative lower bound).
private const val HEADING_12SP_DP = 15f
private const val STATUS_11SP_DP = 14f
private const val CHIP_TEXT_13SP_DP = 16f
private const val MORE_LIKE_THIS_HEADING_17SP_DP = 21f

private fun dpToPx(dp: Float): Int = (dp * DENSITY).roundToInt()

private val failures = mutableListOf<String>()

private fun check(name: String, ok: Boolean, detail: String = "") {
    if (ok) {
        println("ok $name")
    } else {
        val msg = if (detail.isEmpty()) name else "$name: $detail"
        failures += msg
        System.err.println("FAIL: $msg")
    }
}

private fun newColumnPolicy(spacingPx: Int = 0): MeasurePolicy {
    val arrangement: Arrangement.Vertical =
        if (spacingPx > 0) Arrangement.spacedBy((spacingPx / DENSITY).let { androidx.compose.ui.unit.Dp(it) })
        else Arrangement.Top
    return Class.forName("androidx.compose.foundation.layout.ColumnMeasurePolicy")
        .getDeclaredConstructor(Arrangement.Vertical::class.java, Alignment.Horizontal::class.java)
        .newInstance(arrangement, Alignment.Start) as MeasurePolicy
}

private fun newWeightParentData(weight: Float, fill: Boolean = true): Any {
    val cls = Class.forName("androidx.compose.foundation.layout.RowColumnParentData")
    val inst = cls.getDeclaredConstructor().newInstance()
    cls.getDeclaredMethod("setWeight", Float::class.javaPrimitiveType).invoke(inst, weight)
    cls.getDeclaredMethod("setFill", Boolean::class.javaPrimitiveType).invoke(inst, fill)
    return inst
}

private fun measureColumn(
    list: List<Measurable>,
    constraints: Constraints,
    spacingPx: Int = 0,
): MeasureResult {
    val policy = newColumnPolicy(spacingPx)
    val unbox = Constraints::class.java.getDeclaredMethod("unbox-impl").apply { isAccessible = true }
    val m = MeasurePolicy::class.java.declaredMethods.first { it.name.startsWith("measure-") }
    return m.invoke(policy, TvMeasureScope, list, unbox.invoke(constraints)) as MeasureResult
}

private object TvMeasureScope : MeasureScope {
    override val density: Float = DENSITY
    override val fontScale: Float = 1f
    override val layoutDirection: LayoutDirection = LayoutDirection.Ltr
    override fun layout(
        width: Int,
        height: Int,
        alignmentLines: Map<AlignmentLine, Int>,
        placementBlock: Placeable.PlacementScope.() -> Unit,
    ): MeasureResult = object : MeasureResult {
        override val width: Int = width
        override val height: Int = height
        override val alignmentLines: Map<AlignmentLine, Int> = alignmentLines
        override fun placeChildren() {
            val scope = object : Placeable.PlacementScope() {
                override val parentWidth: Int = width
                override val parentLayoutDirection: LayoutDirection = LayoutDirection.Ltr
            }
            scope.placementBlock()
        }
    }
}

private class RecordedPlaceable(
    w: Int,
    h: Int,
    private val onPlaced: (Int, Int) -> Unit,
) : Placeable() {
    init {
        measuredSize = IntSize(w, h)
    }

    override fun get(alignmentLine: AlignmentLine): Int = AlignmentLine.Unspecified

    override fun placeAt(
        position: IntOffset,
        zIndex: Float,
        layerBlock: (androidx.compose.ui.graphics.GraphicsLayerScope.() -> Unit)?,
    ) {
        onPlaced(position.x, position.y)
    }
}

private class BoxMeasurable(
    val name: String,
    val desiredW: Int,
    val desiredH: Int,
    val weight: Float = 0f,
    val outRects: MutableMap<String, Rect>,
) : Measurable {
    override val parentData: Any? = if (weight > 0f) newWeightParentData(weight, true) else null

    override fun measure(constraints: Constraints): Placeable {
        val w = desiredW.coerceIn(constraints.minWidth, constraints.maxWidth)
        val h = desiredH.coerceIn(constraints.minHeight, constraints.maxHeight)
        return RecordedPlaceable(w, h) { x, y ->
            outRects[name] = Rect(x.toFloat(), y.toFloat(), (x + w).toFloat(), (y + h).toFloat())
        }
    }

    override fun minIntrinsicWidth(height: Int): Int = desiredW
    override fun maxIntrinsicWidth(height: Int): Int = desiredW
    override fun minIntrinsicHeight(width: Int): Int = desiredH
    override fun maxIntrinsicHeight(width: Int): Int = desiredH
}

/**
 * Models `SelectableChip` (`androidx.tv.material3.Button` with
 * `ButtonDefaults.ContentPadding` around `Text(label, fontSize = 13.sp, maxLines = 1)`):
 * `PaddingNode` subtracts top+bottom padding from incoming `maxHeight` before
 * measuring the inner `Text`, then adds top+bottom padding back and clamps to
 * `constraints`.
 */
private class ChipButtonMeasurable(
    val name: String,
    val desiredW: Int,
    val textHeightPx: Int,
    val verticalPaddingPx: Int,
    val outButtonRects: MutableMap<String, Rect>,
    val outTextHeights: MutableMap<String, Float>,
) : Measurable {
    override val parentData: Any? = null

    override fun measure(constraints: Constraints): Placeable {
        val w = desiredW.coerceIn(constraints.minWidth, constraints.maxWidth)
        val innerMaxH = (constraints.maxHeight - verticalPaddingPx * 2).coerceAtLeast(0)
        val measuredTextH = textHeightPx.coerceAtMost(innerMaxH)
        val h = (measuredTextH + verticalPaddingPx * 2).coerceIn(constraints.minHeight, constraints.maxHeight)
        return RecordedPlaceable(w, h) { x, y ->
            outButtonRects[name] = Rect(x.toFloat(), y.toFloat(), (x + w).toFloat(), (y + h).toFloat())
            outTextHeights[name] = measuredTextH.toFloat()
        }
    }

    override fun minIntrinsicWidth(height: Int): Int = desiredW
    override fun maxIntrinsicWidth(height: Int): Int = desiredW
    override fun minIntrinsicHeight(width: Int): Int = textHeightPx + verticalPaddingPx * 2
    override fun maxIntrinsicHeight(width: Int): Int = textHeightPx + verticalPaddingPx * 2
}

private fun twoDimensionalWinner(
    candidates: List<Pair<String, Rect>>,
    source: Rect,
    direction: FocusDirection,
): String? {
    val searchClass = Class.forName("androidx.compose.ui.focus.TwoDimensionalFocusSearchKt")
    val isBetter = searchClass.declaredMethods.first {
        it.name.startsWith("isBetterCandidate-") && it.parameterCount == 4
    }.apply { isAccessible = true }
    val isCand = searchClass.declaredMethods.first {
        it.name.contains("isCandidate")
    }.apply { isAccessible = true }
    val dirInt = FocusDirection::class.java
        .getDeclaredMethod("unbox-impl")
        .apply { isAccessible = true }
        .invoke(direction) as Int

    var bestRect: Rect? = null
    var bestName: String? = null
    for ((name, rect) in candidates) {
        if (!(isCand.invoke(null, rect, dirInt, source) as Boolean)) continue
        if (bestRect == null || isBetter.invoke(null, rect, bestRect, source, dirInt) as Boolean) {
            bestRect = rect
            bestName = name
        }
    }
    return bestName
}

private data class LayoutConfig(
    val outerHorizontalPaddingDp: Float,
    val outerVerticalPaddingDp: Float,
    val rightColumnWidthDp: Float,
    val pickerSpacingDp: Float,
    val optionSpacingDp: Float,
    val moreLikeThisTopSpacerDp: Float,
    val moreLikeThisHeaderSpacerDp: Float,
    val moreLikeThisRowHeightDp: Float,
    val rightColumnScrollable: Boolean,
    val dropdownUsesPopupOrOverlay: Boolean,
    val dropdownOptionsScrollable: Boolean,
    val dropdownOptionsMaxHeightDp: Float?,
    val hidesSiblingPickersWhenExpanded: Boolean,
)

private enum class PickerId { AUDIO_BOOST, AUDIO_LANGUAGE, SUBTITLES }

private fun verifyExpandedDropdown(
    pickerId: PickerId,
    caseName: String,
    options: List<PauseDropdownOption<String>>,
    cfg: LayoutConfig,
    topRowMaxHeightPx: Int,
    overlayMaxHeightPx: Int,
    moreLikeThisRectScreen: Rect,
) {
    val pad = ButtonDefaults.ContentPadding
    val vPadPx = dpToPx(pad.calculateTopPadding().value)
    val textHeightPx = dpToPx(CHIP_TEXT_13SP_DP)
    val fullChipHeightPx = textHeightPx + vPadPx * 2
    val optionSpacingPx = dpToPx(cfg.optionSpacingDp)
    val rightColWidthPx = dpToPx(cfg.rightColumnWidthDp)
    val rightColLeftScreen = dpToPx(SCREEN_WIDTH_DP - cfg.outerHorizontalPaddingDp) - rightColWidthPx
    val topRowTopScreen = dpToPx(cfg.outerVerticalPaddingDp).toFloat()

    // Sanity-check real PauseDropdownOptions labels for this picker.
    val buttonLabel = pauseDropdownButtonLabel(options, expanded = true, emptyLabel = "None")
    check("$caseName-button-shows-open-marker", buttonLabel.endsWith(PAUSE_DROPDOWN_OPEN_MARK), "buttonLabel=$buttonLabel")
    val rowLabels = options.map { pauseDropdownRowLabel(it, "None") }
    check(
        "$caseName-exactly-one-selected-row-mark",
        rowLabels.count { it.startsWith("$PAUSE_DROPDOWN_SELECTED_MARK ") } == 1,
        "rowLabels=$rowLabels",
    )

    val optionRectsScreen = LinkedHashMap<String, Rect>()
    val optionTextHeights = LinkedHashMap<String, Float>()
    val viewportHeightPx: Int

    if (cfg.dropdownUsesPopupOrOverlay) {
        val cappedPopupMaxPx = cfg.dropdownOptionsMaxHeightDp?.let(::dpToPx)?.coerceAtMost(overlayMaxHeightPx)
            ?: overlayMaxHeightPx
        val listMaxH = if (cfg.dropdownOptionsScrollable) Constraints.Infinity else cappedPopupMaxPx
        val localRects = LinkedHashMap<String, Rect>()
        val optionMeasurables = options.indices.map { idx ->
            ChipButtonMeasurable("opt$idx", rightColWidthPx - dpToPx(8f), textHeightPx, vPadPx, localRects, optionTextHeights)
        }
        val res = measureColumn(
            optionMeasurables,
            Constraints(minWidth = rightColWidthPx - dpToPx(8f), maxWidth = rightColWidthPx - dpToPx(8f), minHeight = 0, maxHeight = listMaxH),
            spacingPx = optionSpacingPx,
        )
        res.placeChildren()
        viewportHeightPx = cappedPopupMaxPx
        for ((k, r) in localRects) {
            optionRectsScreen[k] = r.translate(rightColLeftScreen.toFloat(), topRowTopScreen)
        }
    } else {
        val colMaxH = if (cfg.rightColumnScrollable) Constraints.Infinity else topRowMaxHeightPx
        val colRects = LinkedHashMap<String, Rect>()
        val colTextHeights = LinkedHashMap<String, Float>()
        val children = mutableListOf<Measurable>()

        fun addPicker(id: PickerId, isAudioBoost: Boolean) {
            if (cfg.hidesSiblingPickersWhenExpanded && id != pickerId) return
            if (children.isNotEmpty()) {
                children += BoxMeasurable("pickerSpacerBefore_$id", 0, dpToPx(cfg.pickerSpacingDp), outRects = colRects)
            }
            children += BoxMeasurable("heading_$id", 200, dpToPx(HEADING_12SP_DP), outRects = colRects)
            children += BoxMeasurable("headingSp_$id", 0, dpToPx(5f), outRects = colRects)
            children += ChipButtonMeasurable("btn_$id", rightColWidthPx, textHeightPx, vPadPx, colRects, colTextHeights)

            if (id == pickerId) {
                children += BoxMeasurable("expSp_$id", 0, dpToPx(4f), outRects = colRects)
                if (cfg.dropdownOptionsScrollable) {
                    // Inner scrollable options container: children measure with Constraints.Infinity,
                    // while the scroll container itself only gets whatever maxHeight remains in the parent Column.
                    val innerRects = LinkedHashMap<String, Rect>()
                    val innerRes = measureColumn(
                        options.indices.map { idx ->
                            ChipButtonMeasurable(
                                "opt$idx",
                                rightColWidthPx - dpToPx(8f),
                                textHeightPx,
                                vPadPx,
                                innerRects,
                                optionTextHeights,
                            )
                        },
                        Constraints(
                            minWidth = rightColWidthPx - dpToPx(8f),
                            maxWidth = rightColWidthPx - dpToPx(8f),
                            minHeight = 0,
                            maxHeight = Constraints.Infinity,
                        ),
                        spacingPx = optionSpacingPx,
                    )
                    innerRes.placeChildren()
                    val desiredScrollH = cfg.dropdownOptionsMaxHeightDp?.let(::dpToPx)?.coerceAtMost(innerRes.height)
                        ?: innerRes.height
                    children += BoxMeasurable("optionsViewport_$id", rightColWidthPx - dpToPx(8f), desiredScrollH, outRects = colRects)
                    for ((k, r) in innerRects) {
                        optionRectsScreen[k] = r.translate((rightColLeftScreen + dpToPx(8f)).toFloat(), topRowTopScreen)
                    }
                } else {
                    options.indices.forEach { idx ->
                        if (idx > 0) {
                            children += BoxMeasurable("optSp_$idx", 0, optionSpacingPx, outRects = colRects)
                        }
                        children += ChipButtonMeasurable(
                            "opt$idx",
                            rightColWidthPx - dpToPx(8f),
                            textHeightPx,
                            vPadPx,
                            colRects,
                            optionTextHeights,
                        )
                    }
                }
            }

            if (isAudioBoost) {
                children += BoxMeasurable("boostStatusSp", 0, dpToPx(4f), outRects = colRects)
                children += BoxMeasurable("boostStatus", rightColWidthPx, dpToPx(STATUS_11SP_DP), outRects = colRects)
            }
        }

        addPicker(PickerId.AUDIO_BOOST, isAudioBoost = true)
        addPicker(PickerId.AUDIO_LANGUAGE, isAudioBoost = false)
        addPicker(PickerId.SUBTITLES, isAudioBoost = false)

        val res = measureColumn(
            children,
            Constraints(minWidth = rightColWidthPx, maxWidth = rightColWidthPx, minHeight = 0, maxHeight = colMaxH),
        )
        res.placeChildren()

        viewportHeightPx = when {
            cfg.rightColumnScrollable -> topRowMaxHeightPx
            cfg.dropdownOptionsScrollable -> colRects["optionsViewport_$pickerId"]!!.height.roundToInt()
            else -> topRowMaxHeightPx
        }

        if (!cfg.dropdownOptionsScrollable) {
            options.indices.forEach { idx ->
                val r = colRects.getValue("opt$idx")
                optionRectsScreen["opt$idx"] = r.translate((rightColLeftScreen + dpToPx(8f)).toFloat(), topRowTopScreen)
            }
        }
    }

    // 1. Viewport hosting expanded options must fit at least 2 full option rows so the dropdown
    //    can genuinely be "used to view multiple values" rather than clipping to a sub-row slit.
    val minMultiValueViewportPx = if (options.size >= 2) fullChipHeightPx * 2 + optionSpacingPx else fullChipHeightPx
    check(
        "$caseName-viewport-fits-multiple-values",
        viewportHeightPx >= minMultiValueViewportPx,
        "viewportHeightPx=$viewportHeightPx (${viewportHeightPx / DENSITY}dp) < required=$minMultiValueViewportPx (${minMultiValueViewportPx / DENSITY}dp)",
    )

    // 2. Every option must measure with full unclipped chip height and non-clipped label text.
    for (idx in options.indices) {
        val rect = optionRectsScreen.getValue("opt$idx")
        val txtH = optionTextHeights.getValue("opt$idx")
        check(
            "$caseName-option-$idx-unclipped-height",
            rect.height.roundToInt() >= fullChipHeightPx && txtH.roundToInt() >= textHeightPx,
            "option[$idx] ('${options[idx].label}') measured height=${rect.height}px (${rect.height / DENSITY}dp), " +
                "innerTextHeight=${txtH}px; expected >= ${fullChipHeightPx}px (${fullChipHeightPx / DENSITY}dp) with text >= ${textHeightPx}px",
        )
    }

    // 3. Real Compose 2D D-pad Down search from option[i] must land on option[i+1],
    //    never skipping clipped 0px options to jump prematurely onto "More like this".
    val candidates = buildList {
        options.indices.forEach { idx -> add("opt$idx" to optionRectsScreen.getValue("opt$idx")) }
        if (!cfg.dropdownUsesPopupOrOverlay) {
            add("moreLikeThisLazyRow" to moreLikeThisRectScreen)
        }
    }
    for (idx in 0 until options.lastIndex) {
        val from = optionRectsScreen.getValue("opt$idx")
        val nextKey = "opt${idx + 1}"
        val landed = twoDimensionalWinner(candidates.filter { it.first != "opt$idx" }, from, FocusDirection.Down)
        check(
            "$caseName-dpad-down-from-opt$idx-reaches-$nextKey",
            landed == nextKey,
            "D-pad Down from opt$idx ('${options[idx].label}', rect=$from) landed on '$landed' instead of '$nextKey' ('${options[idx + 1].label}', rect=${optionRectsScreen[nextKey]})",
        )
    }
}

fun main(args: Array<String>) {
    val flags = args.associate { arg ->
        val clean = arg.removePrefix("--")
        val eq = clean.indexOf('=')
        if (eq == -1) clean to "true" else clean.substring(0, eq) to clean.substring(eq + 1)
    }

    val cfg = LayoutConfig(
        outerHorizontalPaddingDp = flags["outer-horizontal-padding-dp"]?.toFloatOrNull() ?: 44f,
        outerVerticalPaddingDp = flags["outer-vertical-padding-dp"]?.toFloatOrNull() ?: 28f,
        rightColumnWidthDp = flags["right-column-width-dp"]?.toFloatOrNull() ?: 285f,
        pickerSpacingDp = flags["picker-spacing-dp"]?.toFloatOrNull() ?: 18f,
        optionSpacingDp = flags["option-spacing-dp"]?.toFloatOrNull() ?: 4f,
        moreLikeThisTopSpacerDp = flags["more-like-this-top-spacer-dp"]?.toFloatOrNull() ?: 12f,
        moreLikeThisHeaderSpacerDp = flags["more-like-this-header-spacer-dp"]?.toFloatOrNull() ?: 8f,
        moreLikeThisRowHeightDp = flags["more-like-this-row-height-dp"]?.toFloatOrNull() ?: 186f,
        rightColumnScrollable = flags["right-column-scrollable"] == "true",
        dropdownUsesPopupOrOverlay = flags["dropdown-uses-popup-or-overlay"] == "true",
        dropdownOptionsScrollable = flags["dropdown-options-scrollable"] == "true",
        dropdownOptionsMaxHeightDp = flags["dropdown-options-max-height-dp"]?.takeIf { it.isNotEmpty() && it != "none" }?.toFloatOrNull(),
        hidesSiblingPickersWhenExpanded = flags["hides-sibling-pickers-when-expanded"] == "true",
    )

    val innerWidthPx = dpToPx(SCREEN_WIDTH_DP - cfg.outerHorizontalPaddingDp * 2f)
    val innerHeightPx = dpToPx(SCREEN_HEIGHT_DP - cfg.outerVerticalPaddingDp * 2f)

    // Measure PauseOverlay's outer Column when "More like this" recommendations are non-empty.
    val outerRects = LinkedHashMap<String, Rect>()
    val outerRes = measureColumn(
        listOf(
            BoxMeasurable("topRow", innerWidthPx, 0, weight = 1f, outRects = outerRects),
            BoxMeasurable("moreTopSp", 0, dpToPx(cfg.moreLikeThisTopSpacerDp), outRects = outerRects),
            BoxMeasurable("moreHeading", 240, dpToPx(MORE_LIKE_THIS_HEADING_17SP_DP), outRects = outerRects),
            BoxMeasurable("moreHeaderSp", 0, dpToPx(cfg.moreLikeThisHeaderSpacerDp), outRects = outerRects),
            BoxMeasurable("moreLazyRow", innerWidthPx, dpToPx(cfg.moreLikeThisRowHeightDp), outRects = outerRects),
        ),
        Constraints.fixed(innerWidthPx, innerHeightPx),
    )
    outerRes.placeChildren()

    val topRowMaxHeightPx = outerRects.getValue("topRow").height.roundToInt()
    val moreLikeThisRectScreen = outerRects.getValue("moreLazyRow").translate(
        dpToPx(cfg.outerHorizontalPaddingDp).toFloat(),
        dpToPx(cfg.outerVerticalPaddingDp).toFloat(),
    )

    // 1. Audio Boost dropdown (4 supported levels: Off, +3 dB, +6 dB, +10 dB).
    val boostOptions = audioBoostDropdownOptions(listOf(0, 3, 6, 10), current = 0)
        .map { PauseDropdownOption(it.label, it.value.toString(), it.isSelected) }
    verifyExpandedDropdown(
        pickerId = PickerId.AUDIO_BOOST,
        caseName = "audio-boost-4-values",
        options = boostOptions,
        cfg = cfg,
        topRowMaxHeightPx = topRowMaxHeightPx,
        overlayMaxHeightPx = innerHeightPx,
        moreLikeThisRectScreen = moreLikeThisRectScreen,
    )

    // 2. Subtitles dropdown with minimal multiple values (Off + 1 real subtitle track = 2 values).
    val subtitleTwoOptions = listOf(
        PauseDropdownOption("Off", "off", isSelected = true),
        PauseDropdownOption("English", "en", isSelected = false),
    )
    verifyExpandedDropdown(
        pickerId = PickerId.SUBTITLES,
        caseName = "subtitles-2-values",
        options = subtitleTwoOptions,
        cfg = cfg,
        topRowMaxHeightPx = topRowMaxHeightPx,
        overlayMaxHeightPx = innerHeightPx,
        moreLikeThisRectScreen = moreLikeThisRectScreen,
    )

    // 3. Subtitles dropdown with Off + 3 subtitle tracks (4 values).
    val subtitleFourOptions = listOf(
        PauseDropdownOption("Off", "off", isSelected = true),
        PauseDropdownOption("English", "en", isSelected = false),
        PauseDropdownOption("Spanish", "es", isSelected = false),
        PauseDropdownOption("French", "fr", isSelected = false),
    )
    verifyExpandedDropdown(
        pickerId = PickerId.SUBTITLES,
        caseName = "subtitles-4-values",
        options = subtitleFourOptions,
        cfg = cfg,
        topRowMaxHeightPx = topRowMaxHeightPx,
        overlayMaxHeightPx = innerHeightPx,
        moreLikeThisRectScreen = moreLikeThisRectScreen,
    )

    // 4. Audio language dropdown with 4 audio tracks.
    val audioFourOptions = listOf(
        PauseDropdownOption("English", "en", isSelected = true),
        PauseDropdownOption("Spanish", "es", isSelected = false),
        PauseDropdownOption("Japanese", "ja", isSelected = false),
        PauseDropdownOption("Director's Commentary", "commentary", isSelected = false),
    )
    verifyExpandedDropdown(
        pickerId = PickerId.AUDIO_LANGUAGE,
        caseName = "audio-language-4-values",
        options = audioFourOptions,
        cfg = cfg,
        topRowMaxHeightPx = topRowMaxHeightPx,
        overlayMaxHeightPx = innerHeightPx,
        moreLikeThisRectScreen = moreLikeThisRectScreen,
    )

    if (failures.isNotEmpty()) {
        System.err.println("FAILED (${failures.size} assertions)")
        exitProcess(1)
    }
    println("ALL_OK")
}
