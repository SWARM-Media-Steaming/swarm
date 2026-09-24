package app.swarm.tv.app.uat

import android.content.Intent
import androidx.compose.ui.semantics.SemanticsActions
import androidx.compose.ui.test.junit4.createEmptyComposeRule
import androidx.compose.ui.test.onAllNodesWithTag
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performSemanticsAction
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.rules.ActivityScenarioRule
import androidx.test.core.app.ActivityScenario
import androidx.test.platform.app.InstrumentationRegistry
import androidx.test.uiautomator.By
import androidx.test.uiautomator.UiDevice
import androidx.test.uiautomator.Until
import app.swarm.tv.app.MainActivity
import app.swarm.tv.app.data.AndroidLikedEntriesStore
import app.swarm.tv.app.data.AndroidKidModeStore
import app.swarm.tv.app.data.AndroidWatchStateStore
import app.swarm.tv.app.data.AndroidWatchlistStore
import app.swarm.tv.app.data.db.AppDatabase
import app.swarm.tv.app.ui.UatTestTags
import kotlinx.coroutines.runBlocking
import org.junit.Before
import org.junit.After
import org.junit.Rule
import org.junit.rules.RuleChain
import org.junit.rules.TestRule

/**
 * Shared setup for every scenario in the UAT suite (see the
 * swarm-tv-uat-suite skill). Launches [MainActivity] with the same
 * debug-only testing-mode extras `scripts/tv_uat_suite.sh` (and
 * `scripts/tv_e2e_suite.sh` before it) arms via adb — see
 * `MainActivity.EXTRA_ENABLE_TESTING_MODE` /
 * `MainActivity.EXTRA_TESTING_TOKEN`.
 *
 * The token comes from `-e TESTING_TOKEN <value>` instrumentation
 * arguments so the orchestration script can supply the same per-run,
 * host-only-file-backed token it already generates for the frozen suite.
 * Running a single test straight from Android Studio (no orchestration
 * script) falls back to a fixed local token; that only satisfies the
 * media server's debug `begin_testing` check when the server itself
 * was started with a matching control file — see `swarm-local-testing`
 * for the manual Testing-tab flow when no control file exists.
 */
abstract class UatTestBase {

    private val instrumentationArgs = InstrumentationRegistry.getArguments()

    protected val testingToken: String =
        instrumentationArgs.getString("TESTING_TOKEN") ?: "uat-local-run-token"

    private val launchIntent: Intent
        get() = Intent(
            ApplicationProvider.getApplicationContext(),
            MainActivity::class.java,
        ).apply {
            putExtra(EXTRA_ENABLE_TESTING_MODE, true)
            putExtra(EXTRA_TESTING_TOKEN, testingToken)
        }

    /**
     * Launches the real [MainActivity] with the testing-mode Intent above.
     * `createEmptyComposeRule()` (rather than `createAndroidComposeRule`,
     * which only supports launching with the default no-extras Intent) is
     * the documented pattern for exercising a Compose hierarchy behind a
     * custom-Intent activity launch — it attaches to whichever Activity is
     * currently resumed rather than launching one itself.
     */
    private val activityScenarioRule = ActivityScenarioRule<MainActivity>(launchIntent)
    private var restartedScenario: ActivityScenario<MainActivity>? = null

    val composeTestRule = createEmptyComposeRule()

    private val failureCaptureRule = UatFailureCaptureRule(composeTestRuleProvider = { composeTestRule })

    @get:Rule
    val ruleChain: TestRule = RuleChain
        // Compose must install its root registry before ActivityScenario
        // launches MainActivity; otherwise setContent() runs too early and
        // every semantics lookup polls an empty, unregistered hierarchy.
        .outerRule(composeTestRule)
        .around(activityScenarioRule)
        // Keep capture inside both lifecycle rules so a failure is recorded
        // while the Compose registry and MainActivity are still alive.
        .around(failureCaptureRule)

    protected val device: UiDevice by lazy {
        UiDevice.getInstance(InstrumentationRegistry.getInstrumentation())
    }

    /** Real, in-process access to the same Room DB the app itself uses — see AppDatabase.getInstance. */
    protected val appDatabase: AppDatabase by lazy {
        AppDatabase.getInstance(InstrumentationRegistry.getInstrumentation().targetContext)
    }

    /**
     * Real, in-process access to the app's SharedPreferences-backed stores
     * (watchlist/continue-watching/likes are NOT in Room — see store.rs /
     * AppDatabase doc comments and the plan's flagged finding #4). These
     * are plain `Context`-constructed wrappers, so a fresh instance here
     * reads/writes the exact same backing file the app under test uses.
     */
    protected val likedEntriesStore: AndroidLikedEntriesStore by lazy {
        AndroidLikedEntriesStore(InstrumentationRegistry.getInstrumentation().targetContext)
    }
    protected val watchlistStore: AndroidWatchlistStore by lazy {
        AndroidWatchlistStore(InstrumentationRegistry.getInstrumentation().targetContext)
    }
    protected val watchStateStore: AndroidWatchStateStore by lazy {
        AndroidWatchStateStore(InstrumentationRegistry.getInstrumentation().targetContext)
    }
    protected val kidModeStore: AndroidKidModeStore by lazy {
        AndroidKidModeStore(InstrumentationRegistry.getInstrumentation().targetContext)
    }

    @Before
    fun waitForIdleBeforeEachTest() {
        // Testing mode deliberately updates its countdown once per second,
        // so the device never satisfies UIAutomator's default global-idle
        // window. Give the activity a bounded settle; waitForCatalogReady
        // below performs the real readiness check.
        device.waitForIdle(250)
    }

    /**
     * Real pairing + catalog fetch after launch can take up to ~40s on real
     * hardware — the same window `tv_e2e_suite.sh`'s `lan_closed_loop_catalog`
     * test case already allows for this exact process. Gate every scenario
     * on the catalog screen genuinely being up (via [UatTestTags.FILTER_RAIL],
     * present regardless of what media the library actually has) before its
     * body runs, rather than each test's own first `waitForTag` racing that
     * window on a much shorter default timeout.
     */
    @Before
    fun waitForCatalogReady() {
        composeTestRule.waitUntil(timeoutMillis = 45_000) {
            try {
                composeTestRule.onAllNodesWithTag(UatTestTags.FILTER_RAIL).fetchSemanticsNodes().isNotEmpty() ||
                    composeTestRule.onAllNodesWithTag(UatTestTags.DASHBOARD_BROWSE_BUTTON).fetchSemanticsNodes().isNotEmpty()
            } catch (e: IllegalStateException) {
                false
            }
        }
        if (composeTestRule.onAllNodesWithTag(UatTestTags.FILTER_RAIL).fetchSemanticsNodes().isEmpty()) {
            selectTagWithDpad(UatTestTags.DASHBOARD_BROWSE_BUTTON)
            waitForTag(UatTestTags.FILTER_RAIL, timeoutMs = 45_000)
        }
    }

    protected fun pressSelect() = device.pressDPadCenter()

    protected fun pressBack() = device.pressBack()

    protected fun pressDpadUp() = device.pressDPadUp()
    protected fun pressDpadDown() = device.pressDPadDown()
    protected fun pressDpadLeft() = device.pressDPadLeft()
    protected fun pressDpadRight() = device.pressDPadRight()

    protected fun withActivity(action: (MainActivity) -> Unit) {
        (restartedScenario ?: activityScenarioRule.scenario).onActivity(action)
    }

    /** Closes the Activity and its ViewModelStore, then launches a genuinely fresh app Activity against persisted stores. */
    protected fun restartActivityAndWaitForCatalog() {
        restartedScenario?.close()
        activityScenarioRule.scenario.close()
        restartedScenario = ActivityScenario.launch(launchIntent)
        waitForTag(UatTestTags.FILTER_RAIL, timeoutMs = 45_000)
    }

    @After
    fun closeRestartedActivity() {
        restartedScenario?.close()
        restartedScenario = null
    }

    protected fun waitForTagGone(tag: String, timeoutMs: Long = 5_000) {
        composeTestRule.waitUntil(timeoutMillis = timeoutMs) {
            composeTestRule.allTagsStartingWith(tag).isEmpty()
        }
    }

    /**
     * Leaves real playback without assuming whether Back first pauses or the
     * player has already transitioned to its origin screen. On a bare video
     * surface Back intentionally opens the pause overlay; Back from that
     * overlay performs the server-acknowledged release and exits.
     */
    protected fun exitPlaybackTo(targetTag: String, timeoutMs: Long = 15_000) {
        if (composeTestRule.allTagsStartingWith(UatTestTags.PLAYER_SURFACE).isNotEmpty()) {
            // A newly negotiated stream can briefly expose the bare surface
            // before a final buffering transition, during which Back is
            // unreliable — settle, reconfirm, and give the surface real
            // focus first (same fix already proven in
            // NavigationSearchPersistenceUatTest's passing navigation test).
            Thread.sleep(2_000)
            waitForTag(UatTestTags.PLAYER_SURFACE, timeoutMs)
            focusTag(UatTestTags.PLAYER_SURFACE)
        }
        pressBack()
        composeTestRule.waitUntil(timeoutMillis = timeoutMs) {
            composeTestRule.allTagsStartingWith(targetTag).isNotEmpty() ||
                composeTestRule.allTagsStartingWith(UatTestTags.PAUSE_LABEL).isNotEmpty()
        }
        if (composeTestRule.allTagsStartingWith(targetTag).isEmpty()) {
            pressBack()
        }
        waitForTag(targetTag, timeoutMs)
    }

    /** Replaces every watch-state record with an exact pre-test snapshot. */
    protected fun restoreWatchStates(snapshot: Map<String, app.swarm.tv.core.watch.WatchState>) {
        runBlocking {
            watchStateStore.all().keys.forEach { watchStateStore.clear(it) }
            snapshot.forEach { (fingerprint, state) -> watchStateStore.set(fingerprint, state) }
        }
    }

    /** Repeatedly sends one real remote direction until focus reaches [prefix]. */
    protected fun navigateFocusUntilPrefix(
        prefix: String,
        timeoutMs: Long = 8_000,
        press: () -> Unit,
    ): String {
        val deadline = System.currentTimeMillis() + timeoutMs
        while (System.currentTimeMillis() < deadline) {
            composeTestRule.focusedTagStartingWith(prefix)?.let { return it }
            press()
            device.waitForIdle(250)
        }
        return requireNotNull(composeTestRule.focusedTagStartingWith(prefix)) {
            "real D-pad focus never reached tag prefix $prefix"
        }
    }

    /**
     * Activates a tagged TV control through the same focus + D-pad Center
     * path as the physical remote. Compose's `performClick()` injects a
     * pointer click at the node's coordinates; on Fire TV that can silently
     * land on the clipped edge of an off-screen lazy item without invoking
     * the control. Requesting focus first also makes lazy containers bring
     * the target on-screen before the real key event is sent.
     */
    protected fun selectTagWithDpad(tag: String) {
        focusTag(tag)
        pressSelect()
        // The testing-mode countdown changes the Compose tree every second,
        // so UIAutomator's default "wait until globally idle" condition is
        // never reached and burns its full timeout after every real keypress.
        // A short bounded settle is enough for focus/click dispatch while
        // leaving the scenario's explicit state waits to prove navigation.
        device.waitForIdle(250)
    }

    /** Gives a tagged TV surface focus without activating it. */
    protected fun focusTag(tag: String) {
        composeTestRule.onNodeWithTag(tag).performSemanticsAction(SemanticsActions.RequestFocus)
        composeTestRule.waitForIdle()
        device.waitForIdle(250)
    }

    /** Expands the browse filter rail through real directional navigation. */
    protected fun openFilterRail() {
        val expandedTag = UatTestTags.FILTER_KIND_PREFIX + "ALL"
        val deadline = System.currentTimeMillis() + 5_000
        while (
            System.currentTimeMillis() < deadline &&
            composeTestRule.onAllNodesWithTag(expandedTag).fetchSemanticsNodes().isEmpty()
        ) {
            pressDpadLeft()
            device.waitForIdle(250)
        }
        waitForTag(expandedTag)
    }

    /**
     * Moves down the real TV focus graph until a lazy catalog item is
     * composed. Merely polling semantics cannot materialize rows that are
     * still below a LazyColumn's viewport.
     */
    protected fun navigateDownUntilTag(tag: String, timeoutMs: Long = 5_000) {
        navigateDownUntil(timeoutMs) {
            composeTestRule.onAllNodesWithTag(tag).fetchSemanticsNodes().isNotEmpty()
        }
    }

    /** Moves up the real TV focus graph until an earlier lazy row is composed. */
    protected fun navigateUpUntilTag(tag: String, timeoutMs: Long = 5_000) {
        val deadline = System.currentTimeMillis() + timeoutMs
        while (System.currentTimeMillis() < deadline) {
            if (composeTestRule.onAllNodesWithTag(tag).fetchSemanticsNodes().isNotEmpty()) return
            pressDpadUp()
            // The testing-mode banner updates continuously, so the default
            // UIAutomator idle wait can consume this helper's whole timeout
            // after a single key. Use a bounded settle so several real D-pad
            // steps can traverse the lazy catalog within the same deadline.
            device.waitForIdle(250)
        }
        composeTestRule.waitUntil(timeoutMillis = 1) {
            composeTestRule.onAllNodesWithTag(tag).fetchSemanticsNodes().isNotEmpty()
        }
    }

    protected fun navigateDownUntilTagPrefix(prefix: String, timeoutMs: Long = 5_000) {
        navigateDownUntil(timeoutMs) { composeTestRule.allTagsStartingWith(prefix).isNotEmpty() }
    }

    private fun navigateDownUntil(timeoutMs: Long, condition: () -> Boolean) {
        val deadline = System.currentTimeMillis() + timeoutMs
        while (System.currentTimeMillis() < deadline) {
            if (condition()) return
            pressDpadDown()
            device.waitForIdle(250)
        }
        composeTestRule.waitUntil(timeoutMillis = 1) { condition() }
    }

    /**
     * Polls Compose semantics for a node carrying [tag], failing the test if
     * it never appears. Tolerates `IllegalStateException: No compose
     * hierarchies found in the app` — the real, transient window between
     * `MainActivity` launching and its first `setContent()` registering with
     * the test framework — by retrying instead of letting it abort the poll
     * on its first check (see `UatMatchers.kt`'s doc comment for the full
     * story; this method has its own copy of the same tolerance since it
     * queries by exact tag rather than going through those helpers).
     */
    protected fun waitForTag(tag: String, timeoutMs: Long = 5_000) {
        composeTestRule.waitUntil(timeoutMillis = timeoutMs) {
            try {
                composeTestRule.onAllNodesWithTag(tag).fetchSemanticsNodes().isNotEmpty()
            } catch (e: IllegalStateException) {
                false
            }
        }
    }

    /** UIAutomator fallback for content that has left Compose's tree (e.g. after a navigation transition). */
    protected fun waitForText(text: String, timeoutMs: Long = 5_000) {
        check(device.wait(Until.hasObject(By.textContains(text)), timeoutMs)) {
            "Timed out waiting ${timeoutMs}ms for UI text containing: $text"
        }
    }

    private companion object {
        // Mirrors the private constants in MainActivity.kt — kept in sync
        // manually since those are `private const val` there by design
        // (see MainActivity.kt's own doc comment on why testing-mode
        // extras aren't part of any public API surface).
        const val EXTRA_ENABLE_TESTING_MODE = "app.swarm.tv.extra.ENABLE_TESTING_MODE"
        const val EXTRA_TESTING_TOKEN = "app.swarm.tv.extra.TESTING_TOKEN"
    }
}
