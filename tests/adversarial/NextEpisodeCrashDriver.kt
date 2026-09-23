import app.swarm.tv.core.catalog.CatalogGrouping
import app.swarm.tv.core.catalog.MergedEntry
import app.swarm.tv.core.peer.CatalogEntry
import app.swarm.tv.core.peer.MediaKind

/**
 * Issue #358: tapping Next from Forensic Files S4E3 to S4E4 hard-crashes.
 *
 * The player pool's `activate` runs inside a Compose `remember` block with
 * no surrounding try/catch. A throw there is a process-level crash, not a
 * failed episode. The pool may legally lose its ExoPlayer while still
 * tagging that session as active (release vs. recompose interleaving);
 * activate must recreate instead of throwing.
 */
private class FakePlayer(val token: Int, val sessionId: String)

private class VideoPlayerPool {
    private var nextToken = 1
    private var activeSessionId: String? = null
    private var activePlayer: FakePlayer? = null
    private var preloadedSessionId: String? = null
    private var preloadedPlayer: FakePlayer? = null

    fun activate(sessionId: String): FakePlayer {
        // Production must match this control flow: same session id with a
        // live player is a reuse; same session id with a missing player is
        // a recreate, never a checkNotNull throw.
        if (activeSessionId == sessionId) {
            activePlayer?.let { return it }
        }
        val player = if (preloadedSessionId == sessionId) {
            preloadedSessionId = null
            preloadedPlayer.also { preloadedPlayer = null } ?: create(sessionId)
        } else {
            create(sessionId)
        }
        activeSessionId = sessionId
        activePlayer = player
        return player
    }

    fun preload(sessionId: String) {
        if (activeSessionId == sessionId || preloadedSessionId == sessionId) return
        releasePreloaded()
        preloadedSessionId = sessionId
        preloadedPlayer = create(sessionId)
    }

    fun release(player: FakePlayer) {
        if (activePlayer === player) {
            activePlayer = null
            activeSessionId = null
        }
        if (preloadedPlayer === player) {
            preloadedPlayer = null
            preloadedSessionId = null
        }
    }

    fun releasePreloaded() {
        preloadedPlayer = null
        preloadedSessionId = null
    }

    /** The #358 desync: session still tagged active, player already gone. */
    fun loseActivePlayerWithoutClearingSession() {
        activePlayer = null
    }

    fun losePreloadedPlayerWithoutClearingSession() {
        preloadedPlayer = null
    }

    private fun create(sessionId: String): FakePlayer = FakePlayer(nextToken++, sessionId)
}

private fun episode(
    fp: String,
    show: String,
    season: Int,
    episode: Int,
): MergedEntry = MergedEntry(
    fingerprint = fp,
    sources = listOf("server-a"),
    entry = CatalogEntry(
        entryKey = fp,
        fingerprint = fp,
        kind = MediaKind.EPISODE,
        title = "S${season}E$episode",
        size = 1000,
        showTitle = show,
        season = season,
        episode = episode,
        scrapedTitle = show,
    ),
)

private fun check(name: String, condition: Boolean, detail: String = "") {
    if (!condition) {
        System.err.println("FAIL $name ${detail}".trim())
        kotlin.system.exitProcess(1)
    }
    println("ok $name")
}

fun main() {
    val show = "Forensic Files"
    val s4e3 = episode("ff-s4e3", show, 4, 3)
    val s4e4 = episode("ff-s4e4", show, 4, 4)
    val s4e5 = episode("ff-s4e5", show, 4, 5)
    val s5e1 = episode("ff-s5e1", show, 5, 1)
    val entries = listOf(s4e3, s4e4, s4e5, s5e1)
    val shows = CatalogGrouping.groupEpisodesByShowSeason(entries)

    check(
        "next-s4e3-is-s4e4",
        CatalogGrouping.nextEpisode(s4e3, shows)?.fingerprint == "ff-s4e4",
        "Next from Forensic Files S4E3 must be S4E4",
    )
    check(
        "next-s4e4-is-s4e5",
        CatalogGrouping.nextEpisode(s4e4, shows)?.fingerprint == "ff-s4e5",
        "S4E5 is the following sibling and must remain reachable",
    )
    check(
        "next-s4e5-crosses-season",
        CatalogGrouping.nextEpisode(s4e5, shows)?.fingerprint == "ff-s5e1",
    )
    check(
        "next-at-end-is-null",
        CatalogGrouping.nextEpisode(s5e1, shows) == null,
    )
    check(
        "next-unknown-episode-is-null",
        CatalogGrouping.nextEpisode(episode("gone", "Other Show", 1, 1), shows) == null,
    )

    val pool = VideoPlayerPool()
    val first = pool.activate("s4e3")
    check("activate-reuses-live-player", pool.activate("s4e3") === first)

    pool.preload("s4e4")
    val promoted = pool.activate("s4e4")
    check("activate-promotes-preload", promoted.sessionId == "s4e4")
    check("promoted-is-not-the-previous-episode-player", promoted !== first)

    // Release of the previous player after a session-id change must not
    // clear the newly activated successor (Compose dispose-vs-remember order).
    pool.release(first)
    val stillPromoted = pool.activate("s4e4")
    check("release-old-does-not-drop-new", stillPromoted === promoted)

    // #358: same session id, player already null. Must recreate, not throw.
    pool.loseActivePlayerWithoutClearingSession()
    val recreated = try {
        pool.activate("s4e4")
    } catch (thrown: Throwable) {
        System.err.println("FAIL activate-lost-player-throws ${thrown::class.simpleName}: ${thrown.message}")
        kotlin.system.exitProcess(1)
    }
    check("activate-lost-player-does-not-throw", true)
    check("activate-lost-player-recreates", recreated !== promoted && recreated.sessionId == "s4e4")

    pool.preload("s4e5")
    pool.losePreloadedPlayerWithoutClearingSession()
    val fromBrokenPreload = try {
        pool.activate("s4e5")
    } catch (thrown: Throwable) {
        System.err.println("FAIL activate-lost-preload-throws ${thrown::class.simpleName}: ${thrown.message}")
        kotlin.system.exitProcess(1)
    }
    check("activate-lost-preload-recreates", fromBrokenPreload.sessionId == "s4e5")

    val afterFullReleaseSession = fromBrokenPreload.sessionId
    pool.release(fromBrokenPreload)
    val fresh = pool.activate(afterFullReleaseSession)
    check("activate-after-full-release-creates", fresh.token != fromBrokenPreload.token)

    println("ALL_OK")
}
