// Driver for adversarial-issue-443-seek-coalescing. Compiled together with
// the *actual* coalescedSeekTargetMs source (extracted verbatim from
// PlayerScreen.kt by the wrapper script) so this exercises the real fix, not
// a reimplementation of it.
//
// Issue #443: mashing/holding skip on a direct-play MKV fired one real
// Player.seekTo per key-repeat tick, each tearing down and restarting
// Media3's progressive extractor mid-EBML-element — corrupting the parse and
// falsely tripping "server has gone offline" from the resulting burst of
// transport IOExceptions on the abandoned loads. The fix accumulates a burst
// into a single pending target and commits one real seek only once the
// burst goes quiet. These are the boundary behaviors of that accumulation
// that a naive implementation gets wrong.

fun main() {
    val failures = mutableListOf<String>()

    fun check(name: String, expected: Long, actual: Long) {
        if (expected != actual) {
            failures += "$name: expected $expected but got $actual"
        }
    }

    // A three-press forward burst must stack onto the pending target, not
    // silently re-read a stale currentPositionMs supplied by a caller that
    // forgot to also freeze the playhead argument.
    run {
        var pending: Long? = null
        pending = coalescedSeekTargetMs(pending, currentPositionMs = 100_000L, deltaMs = 60_000L)
        // A buggy caller might keep passing the *live* (unmoving, since no
        // real seekTo has landed yet) currentPositionMs instead of null once
        // pending is non-null. The pure function must still prefer pending.
        pending = coalescedSeekTargetMs(pending, currentPositionMs = 100_000L, deltaMs = 60_000L)
        pending = coalescedSeekTargetMs(pending, currentPositionMs = 100_000L, deltaMs = 60_000L)
        check("three-press forward burst from a frozen playhead", 280_000L, pending)
    }

    // Skip-back presses that would go negative must clamp per-step (the
    // floor "sticks") rather than accumulating a negative debt that a later
    // forward press would have to pay off before actually moving. Real
    // seekBack behavior at position 0 just stays at 0; a subsequent forward
    // press from there should look exactly like a fresh forward seek.
    run {
        var pending: Long? = null
        pending = coalescedSeekTargetMs(pending, currentPositionMs = 10_000L, deltaMs = -60_000L)
        check("first over-clamped skip-back lands at the floor", 0L, pending)
        pending = coalescedSeekTargetMs(pending, currentPositionMs = 10_000L, deltaMs = -60_000L)
        check("a second skip-back at the floor stays at the floor", 0L, pending)
        pending = coalescedSeekTargetMs(pending, currentPositionMs = 10_000L, deltaMs = 60_000L)
        check(
            "a forward press after the floor must not pay off a phantom negative debt",
            60_000L,
            pending,
        )
    }

    // A duplicate/zero-delta key event (e.g. a stray ACTION_DOWN redelivery)
    // must be a true no-op on the pending target, not perturb it.
    run {
        val pending = coalescedSeekTargetMs(pendingTargetMs = 45_000L, currentPositionMs = 999_000L, deltaMs = 0L)
        check("a zero-delta press leaves the pending target unchanged", 45_000L, pending)
    }

    // Mixed-direction burst (forward, forward, back) must net correctly
    // rather than a sign error dropping or doubling one leg.
    run {
        var pending: Long? = null
        pending = coalescedSeekTargetMs(pending, currentPositionMs = 200_000L, deltaMs = 60_000L)
        pending = coalescedSeekTargetMs(pending, currentPositionMs = 200_000L, deltaMs = 60_000L)
        pending = coalescedSeekTargetMs(pending, currentPositionMs = 200_000L, deltaMs = -60_000L)
        check("a mixed forward/forward/back burst nets to one step forward", 260_000L, pending)
    }

    if (failures.isEmpty()) {
        println("ALL_OK")
    } else {
        for (failure in failures) {
            println("FAIL: $failure")
        }
    }
}
