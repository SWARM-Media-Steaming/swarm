/**
 * Executable UAT for issue #357's early-Resume race: pressing Resume on a
 * Continue Watching cover *before* negotiation has produced a parked
 * session must still anchor the 10s suppression window to the moment that
 * asset actually starts playing, not to whatever a previous, unrelated
 * session left behind.
 *
 * This models `SwarmViewModel.resumeFromPreparingPlayback` +
 * `SwarmViewModel.playEntry`'s negotiation completion as two independently
 * timed events (Resume can be pressed before OR after negotiation lands),
 * mirroring the source's `preparingResumeRequested` flag rather than
 * re-deriving the shipped driver's simpler happy-path model.
 */
private const val SUPPRESSION_MS = 10_000L

private class EarlyResumeRacePolicy {
    private var activePlaybackSessionStartedAtMs: Long = 0L
    private var preparingResumeRequested = false
    private var parked = false
    private var showingPlayer = false
    private val toasts = mutableListOf<String>()

    fun toasts(): List<String> = toasts.toList()

    /** A previous, unrelated session already played well past its own window. */
    fun seedStalePreviousSession(startedAtMs: Long) {
        activePlaybackSessionStartedAtMs = startedAtMs
    }

    /** Continue Watching cover appears; negotiation for the parked asset begins. */
    fun beginNegotiation() {
        preparingResumeRequested = false
        parked = false
        showingPlayer = false
    }

    /** Viewer presses Resume before negotiation has produced a session. */
    fun pressResumeEarly() {
        preparingResumeRequested = true
        // Mirrors resumeFromPreparingPlayback's `prepared == null` branch:
        // records intent only, does not touch the clock or show the player.
    }

    /** Viewer presses Resume after negotiation already parked a session. */
    fun pressResumeAfterParked(nowMs: Long) {
        check(parked) { "no parked session to resume" }
        activePlaybackSessionStartedAtMs = nowMs
        showingPlayer = true
    }

    /** Negotiation finishes while the cover is still up and Resume was not yet pressed. */
    fun negotiationParksSession() {
        if (preparingResumeRequested) return
        parked = true
    }

    /**
     * Negotiation finishes. Mirrors playEntry's completion: an early Resume
     * forces `startPaused` false so this commits straight to the player
     * (and stamps the clock there) instead of parking behind the cover a
     * second time.
     */
    fun negotiationCompletes(nowMs: Long) {
        if (preparingResumeRequested) {
            activePlaybackSessionStartedAtMs = nowMs
            showingPlayer = true
        } else {
            parked = true
        }
    }

    fun reportBuffering(nowMs: Long) {
        if (!showingPlayer) return
        if (nowMs - activePlaybackSessionStartedAtMs < SUPPRESSION_MS) return
        toasts += "Buffering"
    }
}

private val failures = mutableListOf<String>()

private fun check(name: String, ok: Boolean, detail: String = "") {
    if (ok) {
        println("ok $name")
    } else {
        val message = if (detail.isEmpty()) name else "$name: $detail"
        failures += message
        println("FAIL $message")
    }
}

private fun checkEq(name: String, got: Any?, expected: Any?) {
    check(name, got == expected, "got=$got expected=$expected")
}

/**
 * The scenario the issue describes, reached via the early-press race: a
 * title already played past its own 10s window, then the viewer picks a
 * Continue Watching item and taps Resume before its negotiation lands. If
 * the completion path failed to stamp the clock, this new asset's first
 * fill would immediately toast using the stale timestamp from the old
 * session.
 */
private fun earlyResumeBeforeNegotiationGetsItsOwnFreshWindow() {
    val policy = EarlyResumeRacePolicy()
    val staleStart = 0L
    policy.seedStalePreviousSession(staleStart)
    // Prove the seed alone would have gone stale: at t=20_000 it is 20s old.
    val negotiationStart = 20_000L

    policy.beginNegotiation()
    policy.pressResumeEarly()
    val negotiationDoneAt = 20_400L
    policy.negotiationCompletes(negotiationDoneAt)

    // Immediately after commit: within the new window even though the
    // stale timestamp (0L) would have made this 20.4s old and toast at once.
    policy.reportBuffering(negotiationDoneAt)
    checkEq(
        "asset resumed via early Resume is silent right after commit",
        policy.toasts(),
        emptyList<String>(),
    )
    policy.reportBuffering(negotiationDoneAt + 9_999L)
    checkEq(
        "still silent just under 10s from when negotiation actually committed it",
        policy.toasts(),
        emptyList<String>(),
    )
    policy.reportBuffering(negotiationDoneAt + 10_000L)
    checkEq(
        "toasts once 10s have elapsed from the early-resumed asset's own commit",
        policy.toasts(),
        listOf("Buffering"),
    )
}

/** Sanity companion: negotiation finishes first, Resume pressed after — already
 * covered by the shipped suite, kept here so both interleavings live beside
 * each other and a change to one does not silently stop covering the other. */
private fun lateResumeAfterNegotiationParksAlsoGetsFreshWindow() {
    val policy = EarlyResumeRacePolicy()
    policy.seedStalePreviousSession(0L)
    policy.beginNegotiation()
    policy.negotiationCompletes(5_000L) // preparingResumeRequested still false: parks
    policy.reportBuffering(50_000L)
    checkEq("parked (not yet resumed) session never toasts", policy.toasts(), emptyList<String>())

    val resumeAt = 60_000L
    policy.pressResumeAfterParked(resumeAt)
    policy.reportBuffering(resumeAt + 9_999L)
    checkEq("silent just under 10s from the late Resume press", policy.toasts(), emptyList<String>())
    policy.reportBuffering(resumeAt + 10_000L)
    checkEq("toasts 10s after the late Resume press", policy.toasts(), listOf("Buffering"))
}

/** preparingResumeRequested must not leak into a later, unrelated negotiation. */
private fun resumeFlagDoesNotLeakAcrossSubsequentPlays() {
    val policy = EarlyResumeRacePolicy()
    policy.seedStalePreviousSession(0L)
    policy.beginNegotiation()
    policy.pressResumeEarly()
    policy.negotiationCompletes(1_000L)
    policy.reportBuffering(1_000L)
    checkEq("first early-resumed asset silent right after commit", policy.toasts(), emptyList<String>())

    // A brand new, unrelated negotiation starts (playEntry resets the flag
    // at its top). It must behave like an ordinary parked Continue Watching
    // session, not as though Resume were already pressed for it too.
    policy.beginNegotiation()
    policy.negotiationCompletes(2_000L)
    policy.reportBuffering(200_000L)
    checkEq(
        "a fresh negotiation must not inherit the previous request's early-resume flag",
        policy.toasts(),
        emptyList<String>(),
    )
}

fun main() {
    earlyResumeBeforeNegotiationGetsItsOwnFreshWindow()
    lateResumeAfterNegotiationParksAlsoGetsFreshWindow()
    resumeFlagDoesNotLeakAcrossSubsequentPlays()
    if (failures.isNotEmpty()) {
        println("FAILED ${failures.size} checks")
        kotlin.system.exitProcess(1)
    }
    println("ALL_OK")
}
