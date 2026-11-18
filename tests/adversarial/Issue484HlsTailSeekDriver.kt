// Driver for adversarial-issue-484-hls-tail-seek. Compiled together with the
// *actual* shouldRestartHlsPlaybackForSeek / hlsWindowReachesMediaEnd /
// localHlsTailSeekMs source (extracted verbatim from PlayerScreen.kt by the
// wrapper script), so this exercises the real fix rather than a copy of it.
//
// Issue #484: "When watching movies ... fast forward towards the end to the
// post credit scenes the movie freezes, the media server always disconnects,
// then it reconnects, then it starts streaming again. This needs to be fixed."
//
// The fix serves a tail seek locally when the generated HLS window already
// reaches the movie end. These checks cover the boundaries of that rule.

package app.swarm.tv.app.ui.screens

import androidx.media3.common.C
import kotlin.system.exitProcess

private const val FULL_MS = 6_600_000L // 110-minute movie

private val failures = mutableListOf<String>()

private fun check(name: String, expected: Any?, actual: Any?) {
    if (expected != actual) {
        failures += "$name: expected $expected but got $actual"
    }
}

fun main() {
    // A completed transcode can end short of the probed duration by the
    // segment-length options the settings UI offers (2, 4, 6 seconds). Every
    // tail seek past the window edge must be served locally, clamped to the
    // edge, for every session start offset.
    for (gap in listOf(2_000L, 4_000L, 6_000L)) {
        for (offset in listOf(0L, 3_000_000L, 6_000_000L)) {
            val available = FULL_MS - offset - gap
            for (relativeTarget in listOf(available + 1L, available + 1_000L, FULL_MS - offset)) {
                check(
                    "tail seek gap=${gap}ms offset=${offset}ms target=${relativeTarget}ms served locally",
                    available,
                    localHlsTailSeekMs(relativeTarget, available, offset, FULL_MS),
                )
            }
        }
    }

    // Freeze guard: a window that does not reach the movie end must not be
    // clamped to its edge; the target lies in media that was never generated,
    // so the seek must still fall through to a real session restart.
    check(
        "incomplete window at offset 0 is not clamped",
        null,
        localHlsTailSeekMs(3_000_000L, 60_000L, 0L, FULL_MS),
    )
    check(
        "incomplete window at mid-movie offset is not clamped",
        null,
        localHlsTailSeekMs(6_500_000L, 60_000L, 3_000_000L, FULL_MS),
    )
    check(
        "window stopping at the movie midpoint is not clamped",
        null,
        localHlsTailSeekMs(5_000_000L, 1_000_000L, 0L, FULL_MS),
    )

    // Seeks that already land inside the generated window stay on the
    // current session: no clamp, no restart.
    check("seek inside window is not clamped", null, localHlsTailSeekMs(30_000L, 60_000L, 0L, FULL_MS))
    check(
        "seek exactly at window edge is not clamped",
        null,
        localHlsTailSeekMs(598_000L, 598_000L, 6_000_000L, FULL_MS),
    )

    // Non-positive relative targets (before the session start) always need an
    // earlier session, even when the window reaches the end.
    check("zero relative target is not clamped", null, localHlsTailSeekMs(0L, 598_000L, 6_000_000L, FULL_MS))
    check("negative relative target is not clamped", null, localHlsTailSeekMs(-1L, 598_000L, 6_000_000L, FULL_MS))

    // Unknown durations cannot prove the window reaches the end.
    check(
        "unknown window duration is not clamped",
        null,
        localHlsTailSeekMs(600_000L, C.TIME_UNSET, 6_000_000L, FULL_MS),
    )
    check(
        "unknown probed duration is not clamped",
        null,
        localHlsTailSeekMs(600_000L, 598_000L, 6_000_000L, null),
    )
    check("zero-length window is not clamped", null, localHlsTailSeekMs(600_000L, 0L, 6_000_000L, FULL_MS))

    // Reachability itself: a window that ends exactly where the movie ends is
    // complete, and a window far short of the end is not.
    check("window ending at the movie end reaches it", true, hlsWindowReachesMediaEnd(0L, FULL_MS, FULL_MS))
    check("window far short of the end does not reach it", false, hlsWindowReachesMediaEnd(0L, 60_000L, FULL_MS))

    if (failures.isNotEmpty()) {
        failures.forEach { System.err.println("FAIL: $it") }
        exitProcess(1)
    }
    println("ok issue #484 HLS tail-seek boundaries (${failures.size} failures)")
}
