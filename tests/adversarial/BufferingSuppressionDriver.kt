/**
 * Executable UAT for issue #357: suppress the buffering toaster for the
 * first 10 seconds after an asset is initially played.
 *
 * This driver is the issue's policy, not a copy of SwarmViewModel. Movie,
 * show, and music sessions share one toast surface. Buffering while that
 * session's stream first fills is normal; mid-playback rebuffers after the
 * window are not.
 */
private const val SUPPRESSION_MS = 10_000L

private enum class Screen {
    CATALOG,
    PREPARING,
    PLAYBACK_LOADING,
    PLAYER,
}

private enum class Kind { MOVIE, EPISODE, TRACK }

private data class Session(
    val id: String,
    val kind: Kind,
    val startedAtMs: Long,
)

private class BufferingToastPolicy {
    var screen: Screen = Screen.CATALOG
        private set
    var session: Session? = null
        private set
    private val toasts = mutableListOf<String>()

    fun toasts(): List<String> = toasts.toList()

    /** Fresh play from browse/detail: cover while negotiating, no toaster. */
    fun beginFreshPlay() {
        screen = Screen.PREPARING
    }

    /**
     * Continue Watching parks a ready session behind the cover. The viewer
     * has not started playing yet, so the suppression clock must not start.
     */
    fun parkPreparedSession(id: String, kind: Kind) {
        screen = Screen.PREPARING
        session = Session(id, kind, startedAtMs = Long.MIN_VALUE)
    }

    /** Resume on the cover is when that asset is initially played. */
    fun resumeParkedSession(nowMs: Long) {
        val parked = session ?: error("no parked session")
        session = parked.copy(startedAtMs = nowMs)
        screen = Screen.PLAYER
    }

    /** Negotiation finished and the player (or mini-player) is actually shown. */
    fun showPlayer(id: String, kind: Kind, nowMs: Long) {
        session = Session(id, kind, startedAtMs = nowMs)
        screen = Screen.PLAYER
    }

    /**
     * Next episode/track, recovery, or a renegotiated seek: a new stream is
     * filling. That wait is an initial asset load, not a mid-playback stall
     * of the previous session.
     */
    fun beginReplacementLoad(id: String, kind: Kind, nowMs: Long) {
        session = Session(id, kind, startedAtMs = nowMs)
        screen = Screen.PLAYBACK_LOADING
    }

    fun promoteReplacementToPlayer(nowMs: Long) {
        val current = session ?: error("no replacement session")
        if (current.startedAtMs == Long.MIN_VALUE) {
            session = current.copy(startedAtMs = nowMs)
        }
        screen = Screen.PLAYER
    }

    fun backToCatalog() {
        screen = Screen.CATALOG
        session = null
    }

    /**
     * Pause/resume of the same player must not reopen the window: the asset
     * was already initially played.
     */
    fun pauseAndResumeSameSession() {
        check(screen == Screen.PLAYER)
    }

    fun reportBuffering(nowMs: Long) {
        if (screen != Screen.PLAYER && screen != Screen.PLAYBACK_LOADING) return
        val started = session?.startedAtMs ?: return
        if (started == Long.MIN_VALUE) return
        if (nowMs - started < SUPPRESSION_MS) return
        toasts += "Buffering"
    }

    fun reportQualityReduced() {
        if (screen != Screen.PLAYER) return
        toasts += "Lowering video quality to reduce buffering…"
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

private fun gateTable() {
    val policy = BufferingToastPolicy()
    policy.showPlayer("m1", Kind.MOVIE, nowMs = 1_000_000L)
    val t0 = 1_000_000L
    val samples = listOf(
        -1L to 0,
        0L to 0,
        1L to 0,
        3_000L to 0,
        9_999L to 0,
        10_000L to 1,
        10_001L to 2,
    )
    var seen = 0
    for ((elapsed, expectedTotal) in samples) {
        policy.reportBuffering(t0 + elapsed)
        seen = expectedTotal
        checkEq("gate elapsed=${elapsed}ms", policy.toasts().size, expectedTotal)
    }
    check("gate table consumed", seen == 2)
}

private fun preparingCoverNeverToasts() {
    val policy = BufferingToastPolicy()
    policy.beginFreshPlay()
    policy.reportBuffering(nowMs = 50_000L)
    checkEq("preparing cover is silent even after 10s", policy.toasts(), emptyList<String>())
}

private fun catalogAndMissingSessionNeverToast() {
    val policy = BufferingToastPolicy()
    policy.reportBuffering(nowMs = 50_000L)
    checkEq("catalog does not toast buffering", policy.toasts(), emptyList<String>())
}

private fun movieShowMusicShareTheWindow() {
    for (kind in Kind.entries) {
        val policy = BufferingToastPolicy()
        policy.showPlayer("s-$kind", kind, nowMs = 0L)
        policy.reportBuffering(nowMs = 9_999L)
        checkEq("$kind silent at 9999ms", policy.toasts(), emptyList<String>())
        policy.reportBuffering(nowMs = 10_000L)
        checkEq("$kind toasts at 10000ms", policy.toasts(), listOf("Buffering"))
    }
}

private fun continueWatchingStartsClockOnResumeNotPrepare() {
    val policy = BufferingToastPolicy()
    policy.parkPreparedSession("ep1", Kind.EPISODE)
    policy.reportBuffering(nowMs = 20_000L)
    checkEq("parked continue-watching session does not toast", policy.toasts(), emptyList<String>())

    val resumeAt = 30_000L
    policy.resumeParkedSession(resumeAt)
    policy.reportBuffering(resumeAt + 3_000L)
    checkEq("initial buffer after resume is still silent", policy.toasts(), emptyList<String>())
    policy.reportBuffering(resumeAt + 10_000L)
    checkEq("logic begins 10s after the viewer actually started playback", policy.toasts(), listOf("Buffering"))
}

private fun nextAssetLoadDoesNotInheritPreviousClock() {
    val policy = BufferingToastPolicy()
    policy.showPlayer("ep1", Kind.EPISODE, nowMs = 0L)
    policy.reportBuffering(nowMs = 11_000L)
    checkEq("mid-playback rebuffer of first episode toasts", policy.toasts(), listOf("Buffering"))

    policy.beginReplacementLoad("ep2", Kind.EPISODE, nowMs = 12_000L)
    policy.reportBuffering(nowMs = 12_000L)
    policy.reportBuffering(nowMs = 21_999L)
    checkEq(
        "next episode's initial fill does not reuse the previous episode's clock",
        policy.toasts(),
        listOf("Buffering"),
    )
    policy.reportBuffering(nowMs = 22_000L)
    checkEq(
        "next episode can toast only after its own 10s window",
        policy.toasts(),
        listOf("Buffering", "Buffering"),
    )
}

private fun musicTrackAdvanceResetsTheWindow() {
    val policy = BufferingToastPolicy()
    policy.showPlayer("t1", Kind.TRACK, nowMs = 0L)
    policy.beginReplacementLoad("t2", Kind.TRACK, nowMs = 40_000L)
    policy.reportBuffering(nowMs = 40_000L)
    checkEq("next track initial load is silent", policy.toasts(), emptyList<String>())
    policy.promoteReplacementToPlayer(nowMs = 41_000L)
    policy.reportBuffering(nowMs = 49_999L)
    checkEq("promoted track still inside its own window", policy.toasts(), emptyList<String>())
    policy.reportBuffering(nowMs = 50_000L)
    checkEq("promoted track toasts after 10s from replacement start", policy.toasts(), listOf("Buffering"))
}

private fun pauseDoesNotRestartTheWindow() {
    val policy = BufferingToastPolicy()
    policy.showPlayer("m1", Kind.MOVIE, nowMs = 0L)
    policy.pauseAndResumeSameSession()
    policy.reportBuffering(nowMs = 10_000L)
    checkEq("pause/resume of the same asset does not extend suppression", policy.toasts(), listOf("Buffering"))
}

private fun qualityToastIsIndependent() {
    val policy = BufferingToastPolicy()
    policy.showPlayer("m1", Kind.MOVIE, nowMs = 0L)
    policy.reportQualityReduced()
    checkEq(
        "quality-reduced toaster is not the buffering toaster and is not gated here",
        policy.toasts(),
        listOf("Lowering video quality to reduce buffering…"),
    )
}

private fun briefStallAfterWindowStillToasts() {
    val policy = BufferingToastPolicy()
    policy.showPlayer("m1", Kind.MOVIE, nowMs = 0L)
    // PlayerScreen already waits 3s before reporting; this is that report
    // arriving after the issue's 10s notification logic has begun.
    policy.reportBuffering(nowMs = 13_000L)
    checkEq("mid-playback report after the window still surfaces Buffering", policy.toasts(), listOf("Buffering"))
}

fun main() {
    gateTable()
    preparingCoverNeverToasts()
    catalogAndMissingSessionNeverToast()
    movieShowMusicShareTheWindow()
    continueWatchingStartsClockOnResumeNotPrepare()
    nextAssetLoadDoesNotInheritPreviousClock()
    musicTrackAdvanceResetsTheWindow()
    pauseDoesNotRestartTheWindow()
    qualityToastIsIndependent()
    briefStallAfterWindowStillToasts()
    if (failures.isNotEmpty()) {
        println("FAILED ${failures.size} checks")
        kotlin.system.exitProcess(1)
    }
    println("ALL_OK")
}
