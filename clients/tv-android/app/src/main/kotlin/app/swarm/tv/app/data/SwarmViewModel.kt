package app.swarm.tv.app.data

import android.util.Log
import android.os.SystemClock
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import app.swarm.tv.core.catalog.ArtistGroup
import app.swarm.tv.core.catalog.CatalogGrouping
import app.swarm.tv.core.capability.CapabilityProfile
import app.swarm.tv.core.catalog.CatalogSession
import app.swarm.tv.core.catalog.MergedEntry
import app.swarm.tv.core.catalog.PunchFallback
import app.swarm.tv.core.catalog.SeasonGroup
import app.swarm.tv.core.catalog.ShowGroup
import app.swarm.tv.core.catalog.RepeatMode
import app.swarm.tv.core.catalog.ShuffleMode
import app.swarm.tv.core.catalog.displayTitle
import app.swarm.tv.core.client.SignalingClient
import app.swarm.tv.core.client.StunApiClient
import app.swarm.tv.core.client.StunClientError
import app.swarm.tv.core.peer.ClientErrorReport
import app.swarm.tv.core.peer.LikeToggle
import app.swarm.tv.core.peer.MediaKind
import app.swarm.tv.core.peer.PlaybackMode
import app.swarm.tv.core.peer.TrackLyrics
import app.swarm.tv.core.proxy.PeerLoopbackProxy
import app.swarm.tv.core.rest.DeviceRegistration
import app.swarm.tv.core.rest.DeviceType
import app.swarm.tv.core.rest.ActivationStatus
import app.swarm.tv.core.rest.SwarmDevice
import app.swarm.tv.core.rest.SwarmSummary
import app.swarm.tv.core.token.TokenStore
import app.swarm.tv.core.watch.WatchState
import app.swarm.tv.core.watch.WatchStateStore
import java.net.InetAddress
import java.net.InetSocketAddress
import java.util.UUID
import java.security.PrivateKey
import java.security.cert.X509Certificate
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asSharedFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.collect
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeoutOrNull
import kotlin.random.Random

enum class ClientNotificationKind { SUCCESS, WARNING, ERROR }

private const val BROWSE_PREVIEW_DURATION_SECS = 30L
private const val DASHBOARD_PRESENCE_REFRESH_MS = 10_000L

/**
 * Whole-operation ceiling for [SwarmViewModel.browseCatalog]'s network
 * refresh. `CatalogSession.refresh` already bounds each individual
 * connect and each manifest-fetch attempt and retries a few times, so this
 * is only the final backstop against the entire sequence hanging. Raised
 * from 30s (issue #100): 30s could expire mid-way through the retry loop on
 * a slow first connect, turning a blip that a manual retry cleared
 * instantly into a hard error. */
private const val CATALOG_LOAD_TIMEOUT_MS = 45_000L
const val CONNECTION_SETUP_SWARM_ID = "connection-setup"
const val TESTING_PAIRING_CODE = "00000000"
private const val TESTING_MODE_DURATION_MS = 10 * 60 * 1000L

data class ClientNotification(
    val message: String,
    val kind: ClientNotificationKind,
)

data class TestingModeStatus(
    val remainingSeconds: Long,
    val pairingCode: String = TESTING_PAIRING_CODE,
)

/** One inline browse preview negotiated through the normal,
 * authenticated playback path. [released] means its player has frozen the
 * final frame and the server-side stream reservation is already gone. */
data class BrowsePreview(
    val entryKey: String,
    val url: String,
    val maxBitrate: Long,
    val seekPositionSecs: Long,
    val serverId: String,
    val sessionId: String,
    val released: Boolean = false,
)

private data class PendingPlaybackReplacement(
    val requestGeneration: Long,
    val player: UiState.Player,
)

/** A next-episode (or next-track) session negotiated ahead of time.
 *
 * For episodes [PlayerScreen] prepares this URL in a paused ExoPlayer while
 * the finished episode's Continue prompt is visible. For tracks (#160) the
 * hoisted music player appends it as a second playlist item so ExoPlayer
 * transitions to it with no re-buffer — the server negotiation and the
 * initial buffering are both already done when the current song ends. */
data class PreparedEpisodePlayback(
    val url: String,
    val title: String,
    val playbackMode: PlaybackMode,
    val fingerprint: String,
    val resumePositionSecs: Double,
    val positionOffsetSecs: Double,
    val maxBitrate: Long,
    val mediaDurationSecs: Double?,
    val entry: MergedEntry,
    val nextEntry: MergedEntry?,
    val serverId: String,
    val sessionId: String,
    val subtitles: List<app.swarm.tv.core.peer.SubtitleTrack> = emptyList(),
    val lyrics: TrackLyrics? = null,
)

sealed class UiState {
    /**
     * The real initial state — shown only until [SwarmViewModel.restoreSession]
     * decides whether there's a saved session to resume. Real bug, found
     * live: this used to start straight at the old landing page and only switch
     * away from it once a *successful* restore completed. [Loading] remains
     * on screen only while persisted SWARM/LAN state is read; a new TV then
     * opens the same [Dashboard] used by connected TVs, in connection-setup
     * mode, instead of going through a separate landing page.
     */
    data object Loading : UiState()
    /**
     * A media session is being replaced (next episode, an out-of-buffer
     * seek, or recovery after an extended pause). This stays separate from
     * [Loading] so playback handoffs can keep a plain video backdrop while
     * their progress is reported through the shared toast surface.
     */
    data object PlaybackLoading : UiState()
    /**
     * Shown the instant a fresh play is requested from a browse/detail screen
     * — most visibly a "Continue Watching" tap — so the frozen catalog is
     * immediately replaced by a pause-style cover (title + artwork, and a
     * Resume button when [startPaused]) while the server session is negotiated
     * behind it. Distinct from [PlaybackLoading], which backs the mid-playback
     * handoffs (next episode, out-of-buffer seek, post-pause recovery) that
     * deliberately keep a plain video backdrop and report progress through the
     * shared toast surface.
     *
     * When negotiation finishes first for a plain play, this swaps to the real
     * [Player] with no visible seam. For a "Continue Watching" ([startPaused])
     * start the negotiated session is instead held in [prepared] and this
     * cover stays up until the viewer presses Resume — so they never see a
     * second, identical pause screen appear on its own (#211). Pressing Resume
     * before negotiation finishes flips [resumeRequested] and playback begins
     * the instant the session is ready.
     */
    data class PreparingPlayback(
        val title: String,
        val artworkUrl: String?,
        val previous: UiState,
        /** A "Continue Watching" style start: negotiation opens the player
         * paused, so this cover offers a Resume button. A plain play shows
         * only a preparing indicator. */
        val startPaused: Boolean,
        val resumeRequested: Boolean = false,
        /** A [startPaused] negotiation that completed before the viewer pressed
         * Resume: the session is ready and parked here while this cover stays
         * up. Committed by [resumeFromPreparingPlayback]; released by
         * [cancelPlaybackPreparation]/[stopAllStreaming] if they walk away. */
        val prepared: Player? = null,
    ) : UiState()
    data object RequestingActivation : UiState()
    data class Activating(
        val code: String,
        val expiresAt: String,
        val error: String? = null,
    ) : UiState()
    data class Dashboard(
        val swarm: SwarmSummary,
        val devices: List<SwarmDevice>,
        val resyncing: Boolean = false,
        val allSwarms: List<SwarmSummary> = emptyList(),
        val joiningServer: Boolean = false,
        val joinServerError: String? = null,
    ) : UiState()
    /** [activeSwarmId] is null only once every swarm has been left — the device is still registered, just not a member of anything yet. */
    data class Settings(
        val allSwarms: List<SwarmSummary>,
        val activeSwarmId: String?,
        val baseUrl: String,
        val deviceName: String,
        val busy: Boolean = false,
        val error: String? = null,
        /** Every distinct genre currently in the last-browsed catalog — backs Kid Mode's genre allow-list picker. Empty until [browseCatalog] has actually run once (Settings is only ever reached from [Dashboard], before that) — the picker degrades to "no genre restriction available yet" rather than erroring. */
        val availableGenres: List<String> = emptyList(),
    ) : UiState()
    data class Catalog(
        val swarm: SwarmSummary,
        /** Effective connection targets, including LAN-first routes and paired LAN-only servers. */
        val devices: List<SwarmDevice>,
        /** The unmodified membership roster restored when returning to the dashboard. */
        val dashboardDevices: List<SwarmDevice> = devices,
        val entries: List<MergedEntry> = emptyList(),
        val loading: Boolean = true,
        val unreachable: List<SwarmDevice> = emptyList(),
        val playbackError: String? = null,
    ) : UiState()
    /** Music: Music row -> here (grouped, replacing the old flat-track shelf) -> [ArtistAlbums]. */
    data class ArtistShelf(val catalog: Catalog, val artists: List<ArtistGroup>) : UiState()
    /** One artist's albums; [AlbumScreen] handles the album-grid<->track-list sub-navigation locally. */
    data class ArtistAlbums(
        val previous: UiState,
        val catalog: Catalog,
        val artists: List<ArtistGroup>,
        val artist: ArtistGroup,
        /** Album to open straight to the track list of — set when Back from
         * the music player returns here while a track from [artist] is
         * playing (#160). Null on a plain open, which shows the album grid. */
        val initialAlbum: String? = null,
    ) : UiState()
    /** Movies: Movies row -> here (all movies, "Browse all") -> [MovieDetail]. */
    data class MovieShelf(val catalog: Catalog, val movies: List<MergedEntry>) : UiState()
    /** Movies: Movies row or [MovieShelf] -> here (detail before play) -> [Player]. [previous] is whichever of those it was opened from, so Back returns to the right one — same reasoning as [Player.previous]. */
    data class MovieDetail(val previous: UiState, val entry: MergedEntry) : UiState()
    /** Shows: Shows row -> here (grouped, replacing the old flat-episode shelf) -> [ShowSeasons]. */
    data class ShowShelf(val catalog: Catalog, val shows: List<ShowGroup>) : UiState()
    /** One show's seasons; [SeasonScreen] handles the season-list<->episode-grid sub-navigation locally. */
    data class ShowSeasons(
        val previous: UiState,
        val catalog: Catalog,
        val shows: List<ShowGroup>,
        val show: ShowGroup,
        val selectedSeason: SeasonGroup? = null,
    ) : UiState()
    data class Player(
        val url: String,
        val title: String,
        val playbackMode: PlaybackMode,
        val fingerprint: String,
        val resumePositionSecs: Double,
        val positionOffsetSecs: Double,
        val maxBitrate: Long,
        val mediaDurationSecs: Double?,
        /** The entry actually playing — needed by [nextEpisode]-driven Continue/autoplay. */
        val entry: MergedEntry,
        /** Precomputed at negotiation time: the next episode if [entry] is an Episode and one follows it, else null. */
        val nextEntry: MergedEntry?,
        /**
         * The exact screen [play] was called from — [Catalog] if played
         * from the flat shelf, or the originating [MovieDetail]/
         * [ArtistAlbums]/[ShowSeasons] otherwise, so Back returns to where
         * the user actually was instead of always the top-level catalog.
         * Real bug, found live: this used to be typed as plain [Catalog]
         * (every `play` call site unwrapped its own nested state down to
         * just the embedded catalog before storing it here), so leaving
         * playback from three levels deep in Show/Artist browsing always
         * dropped the user back at the flat browse page. See
         * [embeddedCatalog] for how the pieces that still need a plain
         * [Catalog] (device lookup, next-episode lookup) get it back out.
         */
        val previous: UiState,
        /** Which server negotiated [sessionId] — both needed to release its bandwidth reservation on exit, see [SwarmViewModel.releasePlaybackSession]. */
        val serverId: String,
        val sessionId: String,
        val lyrics: TrackLyrics? = null,
        val subtitles: List<app.swarm.tv.core.peer.SubtitleTrack> = emptyList(),
        /** Locally ranked movies and one representative episode per show for the pause screen. */
        val recommendations: List<MergedEntry> = emptyList(),
        /** Populated after an episode ends while the Continue countdown is
         * visible. Its server session and client buffer must be released if
         * the viewer cancels instead of continuing. */
        val preloadedNext: PreparedEpisodePlayback? = null,
        /** The ended session is released before [preloadedNext] is negotiated
         * so a one-slot server can reserve the next stream immediately. */
        val sessionReleased: Boolean = false,
        /** Set by a "Continue Watching" tap so [PlayerScreen] opens straight
         * into its paused overlay instead of autoplaying — see that param's
         * own doc comment. */
        val startPaused: Boolean = false,
        /**
         * Tracks only: a stable id for one continuous listening session
         * that survives auto-advancing from track to track. The hoisted
         * music `ExoPlayer` in `SwarmApp` is keyed on this rather than
         * [sessionId], so promoting a [preloadedNext] track keeps the same
         * player instance and its already-buffered next item — that is what
         * makes song-to-song transitions seamless (#160). A fresh play from
         * a browse screen starts a new queue id; [playNext] carries it
         * forward. Null for movies/episodes.
         */
        val musicQueueId: String? = null,
    ) : UiState()
}

private fun connectionSetupDashboard(error: String? = null) = UiState.Dashboard(
    swarm = SwarmSummary(CONNECTION_SETUP_SWARM_ID, "Connect a media server"),
    devices = emptyList(),
    joinServerError = error,
)

/** The [UiState.Catalog] embedded in any screen built on top of it — every browse/detail state carries one. */
private fun UiState.embeddedCatalog(): UiState.Catalog? = when (this) {
    is UiState.Catalog -> this
    is UiState.ArtistAlbums -> catalog
    is UiState.MovieShelf -> catalog
    is UiState.MovieDetail -> previous.embeddedCatalog()
    is UiState.ShowSeasons -> catalog
    else -> null
}

/**
 * Screens that show hover-preview-eligible browse cards: the browse page
 * itself and the three "Browse All" full grids (#159). Hover previews are
 * negotiated/released against this catalog's devices regardless of which of
 * those the user is currently on.
 */
private fun UiState.browsePreviewCatalog(): UiState.Catalog? = when (this) {
    is UiState.Catalog -> this
    is UiState.MovieShelf -> catalog
    is UiState.ShowShelf -> catalog
    is UiState.ArtistShelf -> catalog
    else -> null
}

/** Catalog state relevant to diagnostics, including screens that wrap their
 * browse origin for playback transitions. */
private fun UiState.diagnosticCatalog(): UiState.Catalog? = when (this) {
    is UiState.Player -> previous.embeddedCatalog()
    is UiState.PreparingPlayback -> previous.embeddedCatalog()
    else -> embeddedCatalog()
}

/** Restores the dashboard that launched STUN activation and surfaces the
 * failure in its existing error area. */
private fun UiState.withActivationError(message: String): UiState = when (this) {
    is UiState.Dashboard -> copy(joiningServer = false, joinServerError = message)
    else -> this
}

/**
 * Onboarding, swarm-dashboard, merged-catalog, and playback state.
 * Registration is fully wired (real network calls against a real STUN
 * server, real encrypted token storage, a real device identity
 * fingerprint). Right after registration, also opens a [SignalingClient]
 * session and wires it into [catalogSession] as a [PunchFallback] —
 * best-effort, same as `ServerCore`'s `establish_signaling` (Rust): a
 * device with no working signaling session still reaches LAN servers via
 * `peer_addr` fine, it just can't fall back to a hole-punched connection
 * for one that isn't directly reachable. [browseCatalog] connects to every
 * server in the roster (direct first, punched fallback second — see
 * `CatalogSession`) and merges their catalogs; [play] streams the chosen
 * entry through the same session's loopback proxy. `clientCertificate`/
 * `clientKey` are this device's own identity — `AndroidDeviceIdentity`'s
 * `AndroidKeyStore`-backed cert/key on the shipped app, an in-memory one in
 * tests — the mTLS credential a peer's `RosterClientVerifier` checks
 * against the swarm roster it already trusts. [watchStateStore] backs
 * resume/watched state, keyed by each entry's cross-server fingerprint —
 * see [play] and [savePlaybackPosition].
 */
class SwarmViewModel(
    private val tokenStore: TokenStore,
    private val machineId: String,
    primaryCertFingerprint: String,
    primaryClientCertificate: X509Certificate,
    primaryClientKey: PrivateKey,
    private val watchStateStore: WatchStateStore,
    private val watchlistStore: AndroidWatchlistStore,
    private val connectionStore: AndroidConnectionStore,
    private val likedEntriesStore: AndroidLikedEntriesStore,
    private val kidModeStore: AndroidKidModeStore,
    private val clientNotificationStore: AndroidClientNotificationStore,
    private val lanDiscovery: LanDiscoveryManager,
    private val lanConnectionStore: AndroidLanConnectionStore,
    private val disconnectedServerStore: AndroidDisconnectedServerStore,
    private val catalogCache: AndroidCatalogCache,
    private val rendezvousUrl: String,
    private val problemReportDiagnostics: ProblemReportDiagnostics,
    /** This device's real decoder/display capabilities, probed once at
     * startup. Sent with every playback negotiation so the server only
     * transcodes what this hardware genuinely cannot play. Defaults to the
     * conservative baseline for tests and until the probe completes. */
    private val playbackCapabilities: CapabilityProfile = CapabilityProfile.fireTvBaseline(),
    private val testingModeAvailable: Boolean = false,
    private val initialTestingToken: String? = null,
    private val testingIdentityProvider: (() -> ClientIdentity)? = null,
    private val clearTestingIdentity: () -> Unit = {},
) : ViewModel() {
    private val primaryIdentity = ClientIdentity(
        primaryCertFingerprint,
        primaryClientCertificate,
        primaryClientKey,
    )
    private var activeIdentity = primaryIdentity
    private val certFingerprint: String get() = activeIdentity.fingerprint
    private val clientCertificate: X509Certificate get() = activeIdentity.certificate
    private val clientKey: PrivateKey get() = activeIdentity.privateKey
    private val _state = MutableStateFlow<UiState>(UiState.Loading)
    private val logTag = "SwarmViewModel"
    val state: StateFlow<UiState> = _state.asStateFlow()

    private val _notifications = MutableSharedFlow<ClientNotification>(extraBufferCapacity = 32)
    val notifications: SharedFlow<ClientNotification> = _notifications.asSharedFlow()

    private val _testingMode = MutableStateFlow<TestingModeStatus?>(null)
    val testingMode: StateFlow<TestingModeStatus?> = _testingMode.asStateFlow()

    private fun notify(message: String, kind: ClientNotificationKind) {
        _notifications.tryEmit(ClientNotification(message, kind))
    }

    val lanServers: StateFlow<List<LanServer>> = lanDiscovery.servers
    private val _lanPairingBusy = MutableStateFlow(false)
    val lanPairingBusy: StateFlow<Boolean> = _lanPairingBusy.asStateFlow()
    private val _lanPairingActivation = MutableStateFlow<LanPairingActivation?>(null)
    val lanPairingActivation: StateFlow<LanPairingActivation?> = _lanPairingActivation.asStateFlow()
    private val _lanError = MutableStateFlow<String?>(null)
    val lanError: StateFlow<String?> = _lanError.asStateFlow()
    private val _pairedLanFingerprints = MutableStateFlow<Set<String>>(emptySet())
    val pairedLanFingerprints: StateFlow<Set<String>> = _pairedLanFingerprints.asStateFlow()
    private val _pairedLanServers = MutableStateFlow<List<LanServer>>(emptyList())
    val pairedLanServers: StateFlow<List<LanServer>> = _pairedLanServers.asStateFlow()
    private val _disconnectedServerFingerprints = MutableStateFlow<Set<String>>(emptySet())
    val disconnectedServerFingerprints: StateFlow<Set<String>> = _disconnectedServerFingerprints.asStateFlow()

    /** In-memory mirror of [likedEntriesStore], loaded once in [init] — see that store's own doc comment for why the UI never reads through it directly. */
    private val _likedFingerprints = MutableStateFlow<Set<String>>(emptySet())
    val likedFingerprints: StateFlow<Set<String>> = _likedFingerprints.asStateFlow()

    /** Playback history is loaded as a map so the home shelves never perform per-card disk reads. */
    private val _watchStates = MutableStateFlow<Map<String, WatchState>>(emptyMap())
    val watchStates: StateFlow<Map<String, WatchState>> = _watchStates.asStateFlow()

    /** Movie/show watchlist membership, persisted locally and updated optimistically. */
    private val _watchlistKeys = MutableStateFlow<Set<String>>(emptySet())
    val watchlistKeys: StateFlow<Set<String>> = _watchlistKeys.asStateFlow()

    /** Loaded once in [init], refreshed on every [enableKidMode]/[updateKidModeRules]/[disableKidMode] — [browseCatalog] filters through this every time it applies a fresh manifest, see that function. */
    private val _kidModeSettings = MutableStateFlow<KidModeSettings?>(null)
    val kidModeSettings: StateFlow<KidModeSettings?> = _kidModeSettings.asStateFlow()

    private val _resolvedProblemNotifications = MutableStateFlow<List<ResolvedProblemNotification>>(emptyList())
    val resolvedProblemNotifications: StateFlow<List<ResolvedProblemNotification>> = _resolvedProblemNotifications.asStateFlow()

    /** The last successfully-browsed catalog's genres — see [UiState.Settings.availableGenres]'s doc comment for why this can't just be read off [UiState.Settings] itself. */
    private var lastKnownGenres: List<String> = emptyList()
    /** Unfiltered catalog snapshot used to decide when every real episode of a watchlisted show has been watched. */
    private var latestCatalogEntries: List<MergedEntry> = emptyList()

    /** Music-only "keep playing but pick something else" mode — see [CatalogGrouping.nextTrack] and [toggleShuffle]. */
    private val _shuffleMode = MutableStateFlow(ShuffleMode.OFF)
    val shuffleMode: StateFlow<ShuffleMode> = _shuffleMode.asStateFlow()

    /** Music-only repeat button state — see [RepeatMode] and [toggleRepeat]. */
    private val _repeatMode = MutableStateFlow(RepeatMode.OFF)
    val repeatMode: StateFlow<RepeatMode> = _repeatMode.asStateFlow()

    /** Non-null exactly when a track is playing in the background after [minimizePlayback] — see [activePlayerSession]. */
    private val _minimizedPlayer = MutableStateFlow<UiState.Player?>(null)
    val minimizedPlayer: StateFlow<UiState.Player?> = _minimizedPlayer.asStateFlow()

    private val _browsePreview = MutableStateFlow<BrowsePreview?>(null)
    val browsePreview: StateFlow<BrowsePreview?> = _browsePreview.asStateFlow()
    private val _lastReleasedPlaybackSession = MutableStateFlow<String?>(null)
    val lastReleasedPlaybackSession: StateFlow<String?> = _lastReleasedPlaybackSession.asStateFlow()
    private val _transportRecoveryGeneration = MutableStateFlow(0L)
    val transportRecoveryGeneration: StateFlow<Long> = _transportRecoveryGeneration.asStateFlow()
    private var requestedBrowsePreview: MergedEntry? = null
    private var browsePreviewWorker: Job? = null
    private var browsePreviewReleaseJob: Job? = null
    private var browsePreviewCatalog: UiState.Catalog? = null
    /** Parent of one server-held catalog change poll per connected server. */
    private var catalogUpdatesJob: Job? = null

    private var client: StunApiClient? = null
    private var accessToken: String? = null
    private var deviceId: String? = null
    private var swarmId: String? = null
    private var signaling: SignalingClient? = null
    private var activationJob: Job? = null
    private var lanPairingJob: Job? = null
    private var testingModeTimerJob: Job? = null
    private var testingPairingJob: Job? = null
    private var testingToken: String? = null
    private var testingActivation: Pair<LanServer, LanPairingActivation>? = null
    private val testingAttemptedFingerprints = mutableSetOf<String>()
    /** Serializes play/skip/autoplay negotiation so repeated callbacks cannot reserve duplicate server sessions. */
    private var playbackNegotiationJob: Job? = null
    /** Invalidates a negotiation without cancelling its server response, allowing a
     * late-created reservation to be explicitly released instead of timing out. */
    private var playbackRequestGeneration = 0L
    /** Set when the viewer presses Resume on the [UiState.PreparingPlayback]
     * cover before negotiation has finished: the still-buffering session then
     * starts playing the instant it is ready instead of opening paused. */
    private var preparingResumeRequested = false
    /** The player removed while a replacement is negotiated. Retained so a
     * foreground-loss cleanup can release it and restore its previous screen. */
    private var pendingPlaybackReplacement: PendingPlaybackReplacement? = null
    /** Enhancement-only next-episode negotiation performed during the
     * Continue countdown. Separate from [playbackNegotiationJob] so a viewer
     * accepting the prompt can wait for and promote this exact reservation. */
    private var nextEpisodePreloadJob: Job? = null
    private var nextEpisodePreloadSessionId: String? = null
    private var continueAfterPreloadSessionId: String? = null
    private var nextTrackPreloadJob: Job? = null
    private var nextTrackPreloadSessionId: String? = null
    /** Dashboard to restore when Add Server's activation is cancelled or fails. */
    private var activationReturnState: UiState.Dashboard? = null
    /** In-memory for the running session, same as [swarmId]/[accessToken] — see [AndroidConnectionStore]'s doc comment. */
    private var cachedSwarms: List<SwarmSummary> = emptyList()
    /** Set on a successful registration or a restored session; re-sent to [connectionStore] on every edit from the config page. */
    private var baseUrl: String? = null
    private var deviceName: String? = null
    private var localSession = false
    private var activeLocalServer: LanServer? = null
    /**
     * Settings is an overlay-like detour from the SWARM dashboard, but this
     * app models screens as state rather than with a navigation back stack.
     * Keep the exact dashboard we left so Back can restore it synchronously.
     * This is especially important for LAN-only sessions: they deliberately
     * have no rendezvous API client, so trying to rebuild the dashboard via
     * [loadRoster] leaves the user stranded in Settings.
     */
    private var settingsReturnDashboard: UiState.Dashboard? = null
    private var latestLanServers: List<LanServer> = emptyList()
    private data class LanRoute(val fingerprint: String, val address: String)
    private val activeLanRoutes = mutableMapOf<String, LanRoute>()
    private data class PendingClientError(val device: SwarmDevice, val report: ClientErrorReport)
    /** A catalog failure can make its own immediate report undeliverable.
     * Retain a small in-memory queue and retry after the next successful
     * catalog connection instead of silently losing the most useful error. */
    private val pendingClientErrors = mutableListOf<PendingClientError>()
    private val notificationServers = mutableMapOf<String, SwarmDevice>()
    private val playbackConnectionTracker = PlaybackConnectionTracker()

    private val proxy = PeerLoopbackProxy.start()
    private val catalogSession = CatalogSession(proxy, catalogCache) { serverId ->
        reportServerReconnected(serverId)
    }

    init {
        lanDiscovery.start()
        // Load parental rules before restoring a session. Previously these
        // launched concurrently, so a fast catalog-cache read could paint an
        // unrestricted library before Room emitted the enabled Family Mode
        // row; merely updating the settings flow afterward never re-filtered
        // those already-visible entries.
        viewModelScope.launch {
            _kidModeSettings.value = kidModeStore.get()
            if (testingModeAvailable && initialTestingToken != null) {
                _state.value = connectionSetupDashboard()
                enableTestingModeInternal(initialTestingToken)
            } else {
                restoreSession()
            }
        }
        viewModelScope.launch {
            lanDiscovery.servers.collect { discovered ->
                latestLanServers = discovered
                val discoveredFingerprints = discovered.mapTo(mutableSetOf()) { normalizeFingerprint(it.certFingerprint) }
                activeLanRoutes.entries.removeAll { (_, route) -> route.fingerprint !in discoveredFingerprints }
                refreshActiveLocalServer(discovered)
                refreshDashboardLanRoutes()
                maybeStartTestingPairing()
            }
        }
        viewModelScope.launch {
            lanConnectionStore.all().asReversed().forEach { rememberPairedLanServer(it.server) }
            refreshDashboardLanRoutes()
        }
        viewModelScope.launch { _likedFingerprints.value = likedEntriesStore.loadAll() }
        viewModelScope.launch { _watchStates.value = watchStateStore.all() }
        viewModelScope.launch { _watchlistKeys.value = watchlistStore.loadAll() }
        viewModelScope.launch {
            clientNotificationStore.observe().collect { _resolvedProblemNotifications.value = it }
        }
        // Keep the UI tied to Room's authoritative row instead of taking a
        // one-time startup snapshot. Enabling/updating Kid Mode is async;
        // observing the row guarantees the enabled state survives settings
        // navigation and application restarts as soon as the transaction
        // commits.
        viewModelScope.launch {
            kidModeStore.observe().collect {
                _kidModeSettings.value = it
                reapplyKidModeToCurrentCatalog()
            }
        }
        // Presence is live state, not a manual maintenance action. Refresh
        // it only while the dashboard is visible so server online/offline
        // badges stay current without disrupting browsing or playback.
        viewModelScope.launch {
            while (true) {
                delay(DASHBOARD_PRESENCE_REFRESH_MS)
                refreshDashboardPresence()
            }
        }
    }

    /**
     * Cold-start session resume, run once from [init] while [UiState.Loading]
     * is showing. Tries a complete SWARM session first, then the most recently
     * authenticated LAN server. If neither exists, the normal dashboard opens
     * in connection-setup mode so first-run and returning users share one UI.
     */
    private suspend fun restoreSession() {
        val token = tokenStore.load()
        val saved = token?.let { connectionStore.get() }
        val savedActiveSwarm = saved?.activeSwarmId?.let { activeId -> saved.swarms.find { it.id == activeId } }
        if (token != null && saved != null && savedActiveSwarm != null) {
            val effectiveBaseUrl = resolveSavedBaseUrl(saved, savedActiveSwarm, token)
            localSession = false
            activeLocalServer = null
            accessToken = token
            deviceId = saved.deviceId
            swarmId = saved.activeSwarmId
            baseUrl = effectiveBaseUrl
            deviceName = saved.deviceName
            cachedSwarms = saved.swarms
            client = StunApiClient(effectiveBaseUrl)
            _disconnectedServerFingerprints.value = disconnectedServerStore.load(savedActiveSwarm.id)
            val fallbackDashboard = UiState.Dashboard(
                savedActiveSwarm,
                emptyList(),
                allSwarms = saved.swarms,
            )
            establishSignaling(effectiveBaseUrl, token, saved.deviceId)
            loadRoster(
                openBrowseWhenConnected = true,
                fallbackDashboard = fallbackDashboard,
            )
            return
        }

        val local = lanConnectionStore.mostRecent()
        if (local != null) {
            // Discovery and Room restore run concurrently. If mDNS won that
            // race, use its current route immediately instead of restoring a
            // stale DHCP address and waiting for another discovery emission
            // that may never arrive. Trust remains anchored to the exact same
            // certificate fingerprint, so no new pairing is needed.
            val currentServer = preferDiscoveredLanServer(local.server, latestLanServers)
            if (currentServer != local.server) saveLanConnection(currentServer, local.deviceName)
            val localDashboard = buildLocalDashboard(currentServer, local.deviceName)
            val localDevice = currentServer.asSwarmDevice()
            val reachable = try {
                withTimeoutOrNull(5_000) {
                    withContext(Dispatchers.IO) {
                        catalogSession.probe(localDevice, clientCertificate, clientKey)
                    }
                } ?: false
            } catch (error: Exception) {
                Log.w(logTag, "could not probe the saved LAN server during startup", error)
                false
            }
            val restoredDashboard = localDashboard.copy(
                devices = listOf(localDevice.copy(online = reachable)),
            )
            if (reachable) {
                openCatalog(restoredDashboard)
            } else {
                _state.value = restoredDashboard
            }
            return
        }
        _state.value = connectionSetupDashboard()
    }

    /**
     * A packaged/configured endpoint may legitimately replace a saved one
     * after DNS or DHCP changes. Adopt it only when the existing bearer token
     * can read the active swarm there; otherwise retain the saved endpoint so
     * a build accidentally pointed at another service cannot strand a valid
     * session or send it into a re-registration loop.
     */
    private suspend fun resolveSavedBaseUrl(
        saved: SavedConnection,
        activeSwarm: SwarmSummary,
        token: String,
    ): String {
        val savedUrl = saved.baseUrl.trim().trimEnd('/')
        val configuredUrl = rendezvousUrl.trim().trimEnd('/')
        if (configuredUrl.isEmpty() || configuredUrl == savedUrl) return savedUrl
        return try {
            StunApiClient(configuredUrl).swarmDevices(token, activeSwarm.id)
            connectionStore.updateBaseUrl(configuredUrl)
            Log.i(logTag, "migrated saved SWARM endpoint to the configured service")
            configuredUrl
        } catch (error: StunClientError) {
            Log.w(logTag, "configured SWARM endpoint did not accept the saved session; keeping its saved address", error)
            savedUrl
        }
    }

    /** Starts the TV-first activation flow. A connected client reuses its
     * saved SWARM service address; first-run builds use the configured one.
     * Only the short-lived code is shown to the user.
     * The future device token stays in memory and is persisted only after
     * the media server explicitly approves this TV. */
    fun startActivation(deviceName: String) {
        val returnState = _state.value as? UiState.Dashboard ?: return
        activationReturnState = returnState
        val url = baseUrl
            ?.trim()
            ?.trimEnd('/')
            ?.takeIf { it.isNotEmpty() }
            ?: rendezvousUrl.trim().trimEnd('/')
        if (url.isEmpty()) {
            val message = "This app build does not have a SWARM service configured."
            _state.value = returnState.withActivationError(message)
            notify(message, ClientNotificationKind.ERROR)
            activationReturnState = null
            return
        }
        val name = deviceName.ifBlank { "Fire TV" }
        val priorToken = accessToken
        val priorDeviceId = deviceId
        activationJob?.cancel()
        activationJob = viewModelScope.launch {
            _state.value = UiState.RequestingActivation
            val api = StunApiClient(url)
            try {
                val activation = api.createActivation(
                    DeviceRegistration(
                        name = name,
                        deviceType = DeviceType.CLIENT,
                        machineId = machineId,
                        certFingerprint = certFingerprint,
                        platform = "android-tv",
                        appVersion = "0.1.0",
                    ),
                    priorToken,
                )
                _state.value = UiState.Activating(activation.code, activation.expiresAt)
                while (true) {
                    delay(2_000)
                    val status = api.activationStatus(activation.activationId, activation.pollToken)
                    when (status.status) {
                        ActivationStatus.PENDING -> Unit
                        ActivationStatus.EXPIRED -> {
                            val message = "This code expired. Go back and request a new one."
                            _state.value = UiState.Activating(
                                activation.code,
                                activation.expiresAt,
                                message,
                            )
                            notify(message, ClientNotificationKind.WARNING)
                            return@launch
                        }
                        ActivationStatus.APPROVED -> {
                            val approvedDeviceId = status.deviceId ?: throw StunClientError.Decode("approved activation had no device")
                            val approvedSwarm = status.swarm ?: throw StunClientError.Decode("approved activation had no swarm")
                            val effectiveToken = priorToken ?: activation.accessToken
                            val effectiveDeviceId = priorDeviceId ?: approvedDeviceId
                            if (priorToken == null) tokenStore.save(effectiveToken)
                            client = api
                            accessToken = effectiveToken
                            deviceId = effectiveDeviceId
                            swarmId = approvedSwarm.id
                            baseUrl = url
                            this@SwarmViewModel.deviceName = name
                            cachedSwarms = (cachedSwarms + approvedSwarm).distinctBy { it.id }
                            if (priorToken == null) {
                                connectionStore.saveNewConnection(url, name, effectiveDeviceId, approvedSwarm)
                            } else {
                                connectionStore.updateSwarms(cachedSwarms, approvedSwarm.id)
                            }
                            establishSignaling(url, effectiveToken, effectiveDeviceId)
                            activationReturnState = null
                            notify("${approvedSwarm.name} was added.", ClientNotificationKind.SUCCESS)
                            loadRoster(openBrowseWhenConnected = true)
                            return@launch
                        }
                    }
                }
            } catch (e: StunClientError) {
                val message = e.message ?: "Could not start TV activation."
                _state.value = activationReturnState
                    ?.withActivationError(message)
                    ?: connectionSetupDashboard(message)
                notify(message, ClientNotificationKind.ERROR)
                activationReturnState = null
            }
        }
    }

    fun cancelActivation() {
        activationJob?.cancel()
        activationJob = null
        _state.value = activationReturnState ?: connectionSetupDashboard()
        activationReturnState = null
    }

    /**
     * Connects directly to a discovered server. This succeeds without a
     * code when the server already trusts this client's certificate; a new
     * client is rejected by the existing mutual-TLS verifier and can then
     * use [startLanPairing] once to display a media-server approval code.
     *
     * [server] may already be a trusted, previously-paired device (the
     * common case: the user is just reselecting a server they stream from
     * all the time). Captured *before* the connection attempt — not
     * re-derived inside [connectLanServerNow] — because a successful pairing
     * updates [_pairedLanFingerprints] before that function's catalog fetch
     * even runs (see [startLanPairing]), which would otherwise make every
     * caller look "already trusted" by the time it could check.
     */
    fun enableTestingMode() {
        if (!testingModeAvailable) return
        viewModelScope.launch { enableTestingModeInternal(testingToken = null) }
    }

    fun enableTestingModeForAutomation(token: String) {
        if (!testingModeAvailable || token.length < 32) return
        viewModelScope.launch { enableTestingModeInternal(testingToken = token) }
    }

    private suspend fun enableTestingModeInternal(testingToken: String?) {
        val identityProvider = testingIdentityProvider ?: return
        if (_testingMode.value != null) return
        val identity = withContext(Dispatchers.IO) {
            clearTestingIdentity()
            identityProvider()
        }
        settingsReturnDashboard?.devices?.forEach { catalogSession.disconnect(it.deviceId) }
        activeLocalServer?.let { catalogSession.disconnect(it.asSwarmDevice().deviceId) }
        activeIdentity = identity
        this.testingToken = testingToken
        testingAttemptedFingerprints.clear()
        testingActivation = null
        localSession = false
        activeLocalServer = null
        settingsReturnDashboard = null
        _state.value = connectionSetupDashboard()

        val expiresAt = SystemClock.elapsedRealtime() + TESTING_MODE_DURATION_MS
        _testingMode.value = TestingModeStatus(TESTING_MODE_DURATION_MS / 1_000)
        Log.w(logTag, "TV_E2E_TEST_MODE enabled expires_in=600 pairing_code=$TESTING_PAIRING_CODE")
        notify("Testing mode enabled for 10 minutes.", ClientNotificationKind.WARNING)
        testingModeTimerJob?.cancel()
        testingModeTimerJob = viewModelScope.launch {
            while (true) {
                val remainingMs = expiresAt - SystemClock.elapsedRealtime()
                if (remainingMs <= 0) break
                _testingMode.value = TestingModeStatus((remainingMs + 999) / 1_000)
                delay(1_000)
            }
            testingModeTimerJob = null
            disableTestingModeInternal()
        }
        maybeStartTestingPairing()
    }

    fun disableTestingMode() {
        viewModelScope.launch { disableTestingModeInternal() }
    }

    private suspend fun disableTestingModeInternal() {
        if (_testingMode.value == null && activeIdentity == primaryIdentity) return
        testingModeTimerJob?.cancel()
        testingModeTimerJob = null
        testingPairingJob?.cancel()
        testingPairingJob = null
        lanPairingJob?.cancel()
        lanPairingJob = null
        val activeTesting = testingActivation
        testingActivation = null
        if (activeTesting != null) {
            val (server, activation) = activeTesting
            lanDiscovery.endTestingPairing(server, activation)
                .onFailure { Log.w(logTag, "could not explicitly revoke testing activation; server TTL remains the fallback", it) }
            val deviceId = server.asSwarmDevice().deviceId
            catalogSession.disconnect(deviceId)
            catalogCache.remove(deviceId)
            _pairedLanFingerprints.value = _pairedLanFingerprints.value - normalizeFingerprint(server.certFingerprint)
            _pairedLanServers.value = _pairedLanServers.value.filterNot {
                normalizeFingerprint(it.certFingerprint) == normalizeFingerprint(server.certFingerprint)
            }
        }
        _lanPairingActivation.value = null
        _lanPairingBusy.value = false
        _lanError.value = null
        activeLocalServer = null
        localSession = false
        activeIdentity = primaryIdentity
        testingToken = null
        testingAttemptedFingerprints.clear()
        withContext(Dispatchers.IO) { clearTestingIdentity() }
        _testingMode.value = null
        _state.value = connectionSetupDashboard()
        Log.w(logTag, "TV_E2E_TEST_MODE disabled ephemeral_connection_removed=true")
        notify("Testing mode disabled; its server connection was removed.", ClientNotificationKind.SUCCESS)
    }

    private fun maybeStartTestingPairing() {
        if (_testingMode.value == null || testingActivation != null || testingPairingJob?.isActive == true) return
        val candidates = latestLanServers.filter {
            normalizeFingerprint(it.certFingerprint) !in testingAttemptedFingerprints
        }
        if (candidates.isEmpty()) return
        testingPairingJob = viewModelScope.launch {
            try {
                for (server in candidates) {
                    if (_testingMode.value == null) return@launch
                    val fingerprint = normalizeFingerprint(server.certFingerprint)
                    testingAttemptedFingerprints += fingerprint
                    _lanPairingBusy.value = true
                    _lanError.value = null
                    val started = lanDiscovery.beginTestingPairing(
                        server = server,
                        deviceName = deviceName ?: "Fire TV testing",
                        fingerprint = certFingerprint,
                        testingToken = testingToken,
                    )
                    if (started.isFailure) {
                        Log.i(logTag, "testing pairing rejected by ${server.name}; trying the next discovered server")
                        continue
                    }
                    val activation = started.getOrThrow()
                    _lanPairingActivation.value = activation
                    var status = activation.status
                    while (status == "pending" && _testingMode.value != null) {
                        delay(1_000)
                        status = lanDiscovery.pollPairing(server, activation).getOrElse {
                            Log.w(logTag, "testing pairing poll failed for ${server.name}", it)
                            "failed"
                        }
                    }
                    if (status != "approved") continue
                    testingActivation = server to activation
                    rememberPairedLanServer(server)
                    catalogCache.markTestingServer(server.asSwarmDevice().deviceId)
                    Log.w(
                        logTag,
                        "TV_E2E_TEST_PAIRING approved server=${server.name} fingerprint=${server.certFingerprint}",
                    )
                    var connected = false
                    while (!connected && _testingMode.value != null) {
                        connected = connectLanServerNow(
                            server = server,
                            clientName = deviceName ?: "Fire TV testing",
                            knownPaired = true,
                            persistConnection = false,
                        )
                        if (!connected && _testingMode.value != null) {
                            Log.w(logTag, "testing-mode catalog connection failed; retrying ${server.name}")
                            delay(1_000)
                        }
                    }
                    return@launch
                }
                _lanPairingBusy.value = false
                val message = "No discovered media server accepted debug testing mode."
                _lanError.value = message
                notify(message, ClientNotificationKind.ERROR)
            } finally {
                testingPairingJob = null
            }
        }
    }

    fun connectLanServer(server: LanServer, deviceName: String) {
        if (_testingMode.value != null) {
            maybeStartTestingPairing()
            return
        }
        val knownPaired = normalizeFingerprint(server.certFingerprint) in _pairedLanFingerprints.value
        viewModelScope.launch { connectLanServerNow(server, deviceName, knownPaired) }
    }

    fun startLanPairing(server: LanServer, deviceName: String) {
        if (_testingMode.value != null) {
            maybeStartTestingPairing()
            return
        }
        val trimmedName = deviceName.ifBlank { "Fire TV" }
        lanPairingJob?.cancel()
        lanPairingJob = viewModelScope.launch {
            _lanPairingBusy.value = true
            _lanPairingActivation.value = null
            _lanError.value = null
            val started = lanDiscovery.beginPairing(server, trimmedName, certFingerprint)
            if (started.isFailure) {
                _lanPairingBusy.value = false
                val message = started.exceptionOrNull()?.message ?: "Could not request a LAN activation code."
                _lanError.value = message
                notify(message, ClientNotificationKind.ERROR)
                return@launch
            }
            val activation = started.getOrThrow()
            _lanPairingActivation.value = activation
            var consecutivePollFailures = 0
            while (true) {
                delay(2_000)
                val polled = lanDiscovery.pollPairing(server, activation)
                if (polled.isFailure) {
                    consecutivePollFailures += 1
                    if (consecutivePollFailures < 3) continue
                    _lanPairingBusy.value = false
                    val message = polled.exceptionOrNull()?.message ?: "Could not check the LAN approval."
                    _lanError.value = message
                    notify(message, ClientNotificationKind.ERROR)
                    return@launch
                }
                consecutivePollFailures = 0
                when (polled.getOrThrow()) {
                    "pending" -> Unit
                    "expired" -> {
                        _lanPairingBusy.value = false
                        val message = "This LAN activation code expired. Select Try again for a new code."
                        _lanError.value = message
                        notify(message, ClientNotificationKind.WARNING)
                        return@launch
                    }
                    "approved" -> break
                    else -> {
                        _lanPairingBusy.value = false
                        val message = "The media server returned an unknown LAN activation status."
                        _lanError.value = message
                        notify(message, ClientNotificationKind.ERROR)
                        return@launch
                    }
                }
            }

            // Approval and catalog loading are separate operations. Persist
            // trust immediately so a slow first QUIC connection never asks
            // the user to repeat an activation that already succeeded.
            saveLanConnection(server, trimmedName)
            _lanPairingActivation.value = null
            notify("${server.name} approved this TV.", ClientNotificationKind.SUCCESS)
            connectLanServerNow(server, trimmedName, knownPaired = false)
        }
    }

    fun cancelLanPairing() {
        lanPairingJob?.cancel()
        lanPairingJob = null
        _lanPairingBusy.value = false
        _lanPairingActivation.value = null
        _lanError.value = null
    }

    private suspend fun connectLanServerNow(
        server: LanServer,
        clientName: String,
        knownPaired: Boolean,
        persistConnection: Boolean = true,
    ): Boolean {
        _lanPairingBusy.value = true
        _lanError.value = null
        val name = clientName.ifBlank { "Fire TV" }
        val swarmDashboard = (_state.value as? UiState.Dashboard)?.takeIf {
            !localSession && it.swarm.id != "lan" && it.swarm.id != CONNECTION_SETUP_SWARM_ID
        }
        val lanDevice = server.asSwarmDevice()
        val matchingRosterDevice = swarmDashboard?.devices?.firstOrNull {
            normalizeFingerprint(it.certFingerprint) == normalizeFingerprint(server.certFingerprint) &&
                (it.deviceType == DeviceType.SERVER || it.deviceType == DeviceType.BOTH)
        }
        val device = matchingRosterDevice?.copy(
            online = true,
            metadata = matchingRosterDevice.metadata + lanDevice.metadata + ("connection_route" to "lan"),
        ) ?: lanDevice
        if (matchingRosterDevice != null) {
            catalogSession.preferDirect(device.deviceId)
            activeLanRoutes[device.deviceId] = LanRoute(
                normalizeFingerprint(server.certFingerprint),
                lanDevice.metadata.getValue("peer_addr"),
            )
        }
        val swarm = SwarmSummary("lan", "Local network")
        // CatalogSession.refresh() now allows up to 3 connection attempts
        // (#47) for a server this TV has never dialed before — worst case
        // that's 3 connect timeouts plus the backoff between them, so this
        // budget must stay comfortably above that or a slow-but-recovering
        // first pairing gets cut off by this timeout before its own retries
        // finish.
        val result = withTimeoutOrNull(25_000) {
            withContext(Dispatchers.IO) {
                catalogSession.refresh(listOf(device), clientCertificate, clientKey)
            }
        }
        _lanPairingBusy.value = false
        if (result == null || result.unreachable.isNotEmpty()) {
            // A server this TV has already paired with (the common case —
            // reselecting a server it streams from routinely) can still fail
            // one reconnect attempt to a route/ARP hiccup or a Wi-Fi blip
            // without anything actually being wrong: the device stays
            // trusted and any in-progress playback on its own connection is
            // unaffected. Surfacing a security-sounding error here was
            // misleading (#66) — it read as a real problem even while the
            // user was streaming successfully — so this case stays silent
            // and simply leaves the device as unreachable for this attempt;
            // the next resync/browse naturally retries. Only a device this
            // TV has never actually paired with gets an actionable message,
            // since that's the one case where LAN pairing genuinely helps.
            if (!knownPaired) {
                val message = "Could not reach ${server.name} on the local network. If it hasn't approved this TV, open LAN pairing on the media server and enter its code."
                _lanError.value = message
                notify(message, ClientNotificationKind.WARNING)
            } else {
                Log.w(logTag, "reconnect to already-paired LAN server ${server.name} failed; leaving it unreachable for this attempt")
            }
            return false
        }
        deviceId = deviceId ?: "lan-client-${certFingerprint.take(16)}"
        deviceName = name
        syncResolutionNotifications(listOf(device))
        lastKnownGenres = result.entries.flatMap { it.entry.genres }.distinct().sortedWith(String.CASE_INSENSITIVE_ORDER)
        if (persistConnection) saveLanConnection(server, name) else rememberPairedLanServer(server)
        val fingerprint = normalizeFingerprint(server.certFingerprint)
        if (swarmDashboard != null) {
            localSession = false
            activeLocalServer = null
            _disconnectedServerFingerprints.value = _disconnectedServerFingerprints.value - fingerprint
            disconnectedServerStore.setDisconnected(swarmDashboard.swarm.id, fingerprint, disconnected = false)
            _state.value = swarmDashboard
        } else {
            localSession = true
            activeLocalServer = server
            _state.value = UiState.Dashboard(swarm = swarm, devices = listOf(device))
        }
        notify("Connected to ${server.name} over the local network.", ClientNotificationKind.SUCCESS)
        browseCatalog()
        return true
    }

    /** Disconnects one server from this TV only; the shared roster remains unchanged for every other device. */
    fun disconnectSwarmServer(device: SwarmDevice) {
        val current = _state.value as? UiState.Dashboard ?: return
        if (current.swarm.id == "lan") return
        val fingerprint = normalizeFingerprint(device.certFingerprint)
        _disconnectedServerFingerprints.value = _disconnectedServerFingerprints.value + fingerprint
        catalogSession.disconnect(device.deviceId)
        viewModelScope.launch { disconnectedServerStore.setDisconnected(current.swarm.id, fingerprint, disconnected = true) }
        notify("Disconnected ${device.name} from this TV.", ClientNotificationKind.WARNING)
    }

    fun reconnectSwarmServer(device: SwarmDevice) {
        val current = _state.value as? UiState.Dashboard ?: return
        if (current.swarm.id == "lan") return
        val fingerprint = normalizeFingerprint(device.certFingerprint)
        _disconnectedServerFingerprints.value = _disconnectedServerFingerprints.value - fingerprint
        viewModelScope.launch { disconnectedServerStore.setDisconnected(current.swarm.id, fingerprint, disconnected = false) }
        notify("Reconnected ${device.name}.", ClientNotificationKind.SUCCESS)
    }

    /**
     * LAN counterparts of [disconnectSwarmServer] / [reconnectSwarmServer].
     * A paired LAN server otherwise connects silently on tap with no way to
     * drop it. Like the swarm versions these only affect this TV, and the
     * disconnected state is scoped to the current dashboard's swarm id exactly
     * as [connectLanServerNow] persists it.
     */
    fun disconnectLanServer(server: LanServer) {
        val current = _state.value as? UiState.Dashboard ?: return
        val fingerprint = normalizeFingerprint(server.certFingerprint)
        _disconnectedServerFingerprints.value = _disconnectedServerFingerprints.value + fingerprint
        catalogSession.disconnect(server.asSwarmDevice().deviceId)
        viewModelScope.launch { disconnectedServerStore.setDisconnected(current.swarm.id, fingerprint, disconnected = true) }
        notify("Disconnected ${server.name} from this TV.", ClientNotificationKind.WARNING)
    }

    fun reconnectLanServer(server: LanServer) {
        val current = _state.value as? UiState.Dashboard ?: return
        val fingerprint = normalizeFingerprint(server.certFingerprint)
        _disconnectedServerFingerprints.value = _disconnectedServerFingerprints.value - fingerprint
        viewModelScope.launch { disconnectedServerStore.setDisconnected(current.swarm.id, fingerprint, disconnected = false) }
        notify("Reconnected ${server.name}.", ClientNotificationKind.SUCCESS)
    }

    /** Drops the LAN pairing entirely; the server must be re-approved with a code to return. */
    fun forgetLanServer(server: LanServer) {
        val current = _state.value as? UiState.Dashboard
        val fingerprint = normalizeFingerprint(server.certFingerprint)
        val deviceId = server.asSwarmDevice().deviceId
        catalogSession.disconnect(deviceId)
        catalogCache.remove(deviceId)
        _pairedLanFingerprints.value = _pairedLanFingerprints.value - fingerprint
        _pairedLanServers.value = _pairedLanServers.value.filterNot {
            normalizeFingerprint(it.certFingerprint) == fingerprint
        }
        _disconnectedServerFingerprints.value = _disconnectedServerFingerprints.value - fingerprint
        val wasActiveLocal = normalizeFingerprint(activeLocalServer?.certFingerprint ?: "") == fingerprint
        viewModelScope.launch {
            // The Room row's primary key is the raw fingerprint as it was saved.
            lanConnectionStore.forget(server.certFingerprint)
            current?.let { disconnectedServerStore.setDisconnected(it.swarm.id, fingerprint, disconnected = false) }
        }
        if (wasActiveLocal) {
            activeLocalServer = null
            localSession = false
            _state.value = connectionSetupDashboard()
        }
        notify("Forgot ${server.name}. Add it again to reconnect.", ClientNotificationKind.WARNING)
    }

    private fun effectiveCatalogDevices(roster: List<SwarmDevice>): List<SwarmDevice> {
        val disconnected = _disconnectedServerFingerprints.value
        val activeRoster = roster.filterNot { normalizeFingerprint(it.certFingerprint) in disconnected }
        val rosterFingerprints = activeRoster.mapTo(mutableSetOf()) { normalizeFingerprint(it.certFingerprint) }
        val routedRoster = activeRoster.map(::withPreferredLanRoute)

        val pairedLanOnly = latestLanServers
            .filter { normalizeFingerprint(it.certFingerprint) in _pairedLanFingerprints.value }
            .filterNot { normalizeFingerprint(it.certFingerprint) in rosterFingerprints }
            .map { server ->
                val device = server.asSwarmDevice()
                device.copy(metadata = device.metadata + ("lan_only" to "true"))
            }
        return routedRoster + pairedLanOnly
    }

    /** Dashboard counterpart of [effectiveCatalogDevices]: retain explicitly
     * disconnected roster rows so the user can reconnect them, while still
     * adding paired LAN-only servers and immediately applying fresh mDNS
     * routes. Previously mDNS updates only refreshed LAN-only sessions; an
     * already-visible SWARM dashboard stayed stale until its next remote
     * roster poll. */
    private fun dashboardDevices(roster: List<SwarmDevice>): List<SwarmDevice> {
        val baseRoster = roster.filterNot { it.metadata["lan_only"] == "true" }
        val rosterFingerprints = baseRoster.mapTo(mutableSetOf()) { normalizeFingerprint(it.certFingerprint) }
        val pairedLanOnly = latestLanServers
            .filter { normalizeFingerprint(it.certFingerprint) in _pairedLanFingerprints.value }
            .filterNot { normalizeFingerprint(it.certFingerprint) in rosterFingerprints }
            .map { server ->
                val device = server.asSwarmDevice()
                device.copy(metadata = device.metadata + ("lan_only" to "true"))
            }
        return baseRoster.map(::withPreferredLanRoute) + pairedLanOnly
    }

    private fun refreshDashboardLanRoutes() {
        if (localSession) return
        val current = _state.value as? UiState.Dashboard ?: return
        if (current.swarm.id == CONNECTION_SETUP_SWARM_ID || current.swarm.id == "lan") return
        val refreshed = dashboardDevices(current.devices)
        if (refreshed != current.devices) {
            val updated = current.copy(devices = refreshed)
            if (!current.devices.any(::isConnectedMediaServer) && refreshed.any(::isConnectedMediaServer)) {
                openCatalog(updated)
            } else {
                _state.value = updated
            }
        }
    }

    private fun withPreferredLanRoute(device: SwarmDevice): SwarmDevice {
        if (device.deviceType != DeviceType.SERVER && device.deviceType != DeviceType.BOTH) return device
        val fingerprint = normalizeFingerprint(device.certFingerprint)
        val lan = latestLanServers.firstOrNull { normalizeFingerprint(it.certFingerprint) == fingerprint } ?: return device
        val direct = lan.asSwarmDevice()
        val route = LanRoute(fingerprint, direct.metadata.getValue("peer_addr"))
        if (activeLanRoutes.put(device.deviceId, route) != route) catalogSession.preferDirect(device.deviceId)
        return device.copy(
            online = true,
            metadata = device.metadata + direct.metadata + ("connection_route" to "lan"),
        )
    }

    private fun normalizeFingerprint(fingerprint: String): String = fingerprint.trim().lowercase()

    private fun rememberPairedLanServer(server: LanServer) {
        val fingerprint = normalizeFingerprint(server.certFingerprint)
        _pairedLanFingerprints.value = _pairedLanFingerprints.value + fingerprint
        _pairedLanServers.value = listOf(server) + _pairedLanServers.value.filterNot {
            normalizeFingerprint(it.certFingerprint) == fingerprint
        }
    }

    private suspend fun saveLanConnection(server: LanServer, clientName: String) {
        lanConnectionStore.save(server, clientName)
        rememberPairedLanServer(server)
    }

    private fun buildLocalDashboard(server: LanServer, clientName: String): UiState.Dashboard {
        localSession = true
        activeLocalServer = server
        deviceId = deviceId ?: "lan-client-${certFingerprint.take(16)}"
        deviceName = clientName
        return UiState.Dashboard(
            swarm = SwarmSummary("lan", "Local network"),
            devices = listOf(server.asSwarmDevice()),
        )
    }

    private fun showLocalDashboard(server: LanServer, clientName: String) {
        _state.value = buildLocalDashboard(server, clientName)
    }

    /** Refresh cached address data when the persisted server is rediscovered after DHCP or network changes. */
    private fun refreshActiveLocalServer(discovered: List<LanServer>) {
        if (!localSession) return
        val current = activeLocalServer ?: return
        val refreshed = preferDiscoveredLanServer(current, discovered)
        if (refreshed == current) return
        activeLocalServer = refreshed
        val stateNow = _state.value
        if (stateNow is UiState.Dashboard && stateNow.swarm.id == "lan") {
            val updated = stateNow.copy(devices = listOf(refreshed.asSwarmDevice()))
            if (!stateNow.devices.any(::isConnectedMediaServer)) {
                openCatalog(updated)
            } else {
                _state.value = updated
            }
        }
        viewModelScope.launch { saveLanConnection(refreshed, deviceName ?: "Fire TV") }
    }

    fun resync() {
        val current = _state.value
        Log.i(logTag, "resync() called, current state=${current::class.simpleName}")
        if (current !is UiState.Dashboard) return
        _state.value = current.copy(resyncing = true)
        if (localSession) {
            val server = activeLocalServer ?: run {
                _state.value = current.copy(resyncing = false)
                notify("The local media server is not available.", ClientNotificationKind.ERROR)
                return
            }
            viewModelScope.launch {
                val reachable = withTimeoutOrNull(10_000) {
                    withContext(Dispatchers.IO) {
                        catalogSession.probe(server.asSwarmDevice(), clientCertificate, clientKey)
                    }
                } ?: false
                val stateNow = _state.value
                if (stateNow is UiState.Dashboard && stateNow.swarm.id == "lan") {
                    _state.value = stateNow.copy(
                        devices = listOf(server.asSwarmDevice().copy(online = reachable)),
                        resyncing = false,
                    )
                    if (reachable) {
                        notify("Server connection refreshed.", ClientNotificationKind.SUCCESS)
                    } else {
                        notify("The media server is not reachable.", ClientNotificationKind.ERROR)
                    }
                }
            }
            return
        }
        viewModelScope.launch { loadRoster() }
    }

    private suspend fun refreshDashboardPresence() {
        val current = _state.value as? UiState.Dashboard ?: return
        if (!localSession) {
            loadRoster()
            return
        }

        val server = activeLocalServer ?: return
        val reachable = withTimeoutOrNull(5_000) {
            withContext(Dispatchers.IO) {
                catalogSession.probe(server.asSwarmDevice(), clientCertificate, clientKey)
            }
        } ?: false
        val stateNow = _state.value
        if (stateNow is UiState.Dashboard && stateNow.swarm.id == current.swarm.id) {
            val updated = stateNow.copy(
                devices = listOf(server.asSwarmDevice().copy(online = reachable)),
                resyncing = false,
            )
            if (reachable && !stateNow.devices.any(::isConnectedMediaServer)) {
                openCatalog(updated)
            } else {
                _state.value = updated
            }
        }
    }

    fun dismissError() {
        _state.value = connectionSetupDashboard()
    }

    fun openSettings() {
        val current = _state.value
        if (current !is UiState.Dashboard) return
        settingsReturnDashboard = current
        _state.value = UiState.Settings(
            allSwarms = current.allSwarms,
            activeSwarmId = current.swarm.id,
            baseUrl = baseUrl.orEmpty(),
            deviceName = deviceName.orEmpty(),
            availableGenres = lastKnownGenres,
        )
    }

    /** Config-page edit: where this device connects next — see [AndroidConnectionStore.updateBaseUrl]. */
    fun updateBaseUrl(newBaseUrl: String) {
        val current = _state.value
        if (current !is UiState.Settings) return
        val trimmed = newBaseUrl.trim()
        if (trimmed.isEmpty()) {
            val message = "Enter a SWARM server URL."
            _state.value = current.copy(error = message)
            notify(message, ClientNotificationKind.ERROR)
            return
        }
        baseUrl = trimmed
        _state.value = current.copy(baseUrl = trimmed, error = null)
        viewModelScope.launch { connectionStore.updateBaseUrl(trimmed) }
        notify("SWARM server address saved.", ClientNotificationKind.SUCCESS)
    }

    /** Config-page edit: the locally-remembered device label — see [AndroidConnectionStore.updateDeviceName] for why this never renames the device on the server. */
    fun updateDeviceName(newName: String) {
        val current = _state.value
        if (current !is UiState.Settings) return
        val trimmed = newName.ifBlank { "Fire TV" }
        deviceName = trimmed
        _state.value = current.copy(deviceName = trimmed, error = null)
        viewModelScope.launch { connectionStore.updateDeviceName(trimmed) }
        notify("Device name saved.", ClientNotificationKind.SUCCESS)
    }

    /** Redeems an additional join code from the main SWARM page and switches
     * to the newly joined swarm. The legacy Settings caller remains accepted
     * while that screen is being retired from persisted navigation state. */
    fun joinAdditionalSwarm(code: String) {
        val current = _state.value
        if (current !is UiState.Settings && current !is UiState.Dashboard) return
        val api = client ?: return
        val token = accessToken ?: return
        val trimmedCode = code.trim()
        if (trimmedCode.length != 8) {
            val message = "Enter the 8-digit join code."
            _state.value = when (current) {
                is UiState.Settings -> current.copy(error = message)
                is UiState.Dashboard -> current.copy(joinServerError = message)
                else -> current
            }
            notify(message, ClientNotificationKind.ERROR)
            return
        }
        _state.value = when (current) {
            is UiState.Settings -> current.copy(busy = true, error = null)
            is UiState.Dashboard -> current.copy(joiningServer = true, joinServerError = null)
            else -> current
        }
        viewModelScope.launch {
            try {
                val joined = withContext(Dispatchers.IO) { api.joinSwarm(token, trimmedCode) }
                cachedSwarms = (cachedSwarms + joined).distinctBy { it.id }
                swarmId = joined.id
                connectionStore.updateSwarms(cachedSwarms, joined.id)
                val stateNow = _state.value
                if (stateNow is UiState.Settings) {
                    _state.value = stateNow.copy(allSwarms = cachedSwarms, activeSwarmId = joined.id, busy = false)
                } else if (stateNow is UiState.Dashboard) {
                    loadRoster()
                }
                notify("${joined.name} was added.", ClientNotificationKind.SUCCESS)
            } catch (e: StunClientError) {
                val stateNow = _state.value
                val message = e.message ?: "Could not add that server."
                if (stateNow is UiState.Settings) {
                    _state.value = stateNow.copy(busy = false, error = message)
                } else if (stateNow is UiState.Dashboard) {
                    _state.value = stateNow.copy(
                        joiningServer = false,
                        joinServerError = message,
                    )
                }
                notify(message, ClientNotificationKind.ERROR)
            }
        }
    }

    /**
     * Leaves one swarm, keeping this device's registration and its other
     * memberships intact. If the swarm left was the active one, the first
     * remaining swarm becomes active; if none remain, [Settings.activeSwarmId]
     * goes null — the device stays registered, just in zero swarms, and
     * [backFromSettings] returns to the dashboard's connection-setup mode.
     */
    fun leaveSwarm(swarmIdToLeave: String) {
        val current = _state.value
        if (current !is UiState.Settings) return
        val api = client ?: return
        val token = accessToken ?: return
        val device = deviceId ?: return
        _state.value = current.copy(busy = true, error = null)
        viewModelScope.launch {
            try {
                withContext(Dispatchers.IO) { api.leaveSwarm(token, swarmIdToLeave, device) }
                cachedSwarms = cachedSwarms.filterNot { it.id == swarmIdToLeave }
                val newActiveId = if (current.activeSwarmId == swarmIdToLeave) cachedSwarms.firstOrNull()?.id else current.activeSwarmId
                swarmId = newActiveId
                connectionStore.updateSwarms(cachedSwarms, newActiveId)
                val stateNow = _state.value
                if (stateNow is UiState.Settings) {
                    _state.value = stateNow.copy(allSwarms = cachedSwarms, activeSwarmId = newActiveId, busy = false)
                }
            } catch (e: StunClientError) {
                val stateNow = _state.value
                if (stateNow is UiState.Settings) {
                    _state.value = stateNow.copy(busy = false, error = e.message ?: "Could not leave that swarm.")
                }
            }
        }
    }

    fun switchActiveSwarm(newSwarmId: String) {
        val current = _state.value
        if (current !is UiState.Settings) return
        swarmId = newSwarmId
        _state.value = current.copy(activeSwarmId = newSwarmId)
        viewModelScope.launch {
            _disconnectedServerFingerprints.value = disconnectedServerStore.load(newSwarmId)
            connectionStore.setActiveSwarm(newSwarmId)
        }
    }

    fun backFromSettings() {
        val current = _state.value
        if (current !is UiState.Settings) return
        settingsReturnDashboard?.let { dashboard ->
            settingsReturnDashboard = null
            _state.value = dashboard
            // A SWARM-backed session can refresh its roster after the screen
            // has changed. LAN-only sessions already have their authoritative
            // discovered server route and must not depend on this API call.
            if (!localSession) viewModelScope.launch { loadRoster() }
            return
        }
        if (current.activeSwarmId == null) {
            _state.value = connectionSetupDashboard()
            return
        }
        viewModelScope.launch { loadRoster() }
    }

    /** Connects to every reachable server in the roster and merges their catalogs — see [CatalogSession]. */
    fun browseCatalog() {
        val current = _state.value
        Log.i(logTag, "browseCatalog() called, current state=${current::class.simpleName}")
        if (current !is UiState.Dashboard) return
        openCatalog(current)
    }

    /** Transitions directly from a resolved startup dashboard to Browse.
     * [current] does not need to have been published to [_state], which
     * prevents a one-frame SWARM-page flash during cold start. */
    private fun openCatalog(current: UiState.Dashboard) {
        catalogUpdatesJob?.cancel()
        // Resolve the route again at the point of use. This covers the narrow
        // startup interleaving where discovery populated latestLanServers
        // after the dashboard was restored but before its collector refreshed
        // the saved row. Certificate identity—not the old IP—authorizes it.
        val connectionDevices = if (localSession) {
            val savedServer = activeLocalServer
            val routedServer = savedServer?.let { preferDiscoveredLanServer(it, latestLanServers) }
            if (routedServer != null && routedServer != savedServer) {
                activeLocalServer = routedServer
                viewModelScope.launch { saveLanConnection(routedServer, deviceName ?: "Fire TV") }
            }
            routedServer?.let { listOf(it.asSwarmDevice()) } ?: current.devices
        } else {
            effectiveCatalogDevices(current.devices)
        }
        _state.value = UiState.Catalog(
            swarm = current.swarm,
            devices = connectionDevices,
            dashboardDevices = current.devices,
            loading = true,
        )
        viewModelScope.launch {
            try {
            // Paint the last successful per-server snapshots first. On a
            // warm browse this avoids both a blank screen and dependence on
            // a large network response; refresh below only checks a compact
            // fingerprint unless the server's catalog actually changed.
            val cachedEntries = withContext(Dispatchers.IO) {
                catalogSession.cachedEntries(connectionDevices, clientCertificate, clientKey)
            }
            if (cachedEntries.isNotEmpty()) {
                latestCatalogEntries = cachedEntries
                val cachedState = _state.value
                if (cachedState is UiState.Catalog) {
                    lastKnownGenres = cachedEntries.flatMap { it.entry.genres }
                        .distinct()
                        .sortedWith(String.CASE_INSENSITIVE_ORDER)
                    _state.value = cachedState.copy(
                        entries = applyKidModeFilter(cachedEntries),
                        loading = false,
                    )
                }
            }

            // CatalogSession.refresh blocks on real network I/O (connect + one
            // request per server) — never run it on the Main dispatcher.
            //
            // Real bug, found live: refresh() itself fails open per-device
            // (a dead peer just lands in Result.unreachable), but nothing
            // ever bounded the *whole* call — PeerQuicClient.request has a
            // connect timeout but no read timeout on the response body, so
            // a manifest fetch that stalls partway through (or is just
            // very slow — a real library can now be thousands of entries,
            // far past anything tested on real hardware before) hung here
            // forever with `loading` never flipping back to false: an
            // infinite spinner with no visible error. refresh() now bounds
            // each manifest-fetch attempt itself (see
            // CatalogSession.MANIFEST_FETCH_TIMEOUT_MS, issue #100) so a
            // single stall converts into a fast per-attempt failure its
            // retry loop can recover from; CATALOG_LOAD_TIMEOUT_MS stays as
            // the whole-sequence backstop, mapping to the same "server(s)
            // not reachable" state the UI already renders for a normal
            // per-device failure rather than a wholly new error path.
            val result = withTimeoutOrNull(CATALOG_LOAD_TIMEOUT_MS) {
                withContext(Dispatchers.IO) {
                    catalogSession.refresh(connectionDevices, clientCertificate, clientKey)
                }
            }
            if (result == null) {
                Log.w(logTag, "browseCatalog() timed out waiting on ${connectionDevices.size} device(s)")
                val servers = connectionDevices.filter { it.deviceType != DeviceType.CLIENT }
                servers.forEach { server ->
                    reportClientError(
                        device = server,
                        message = "Catalog loading timed out after ${CATALOG_LOAD_TIMEOUT_MS / 1000} seconds.",
                        context = catalogFailureContext(server, servers.size, "timeout"),
                    )
                }
            } else {
                Log.i(logTag, "browseCatalog() refresh done: entries=${result.entries.size} unreachable=${result.unreachable.size}")
                syncResolutionNotifications(connectionDevices - result.unreachable.toSet())
                result.unreachable.forEach { activeLanRoutes.remove(it.deviceId) }
                if (result.entries.isNotEmpty() || result.unreachable.isEmpty()) flushPendingClientErrors()
                result.failures.forEach { failure ->
                    reportClientError(
                        device = failure.device,
                        message = "Catalog loading failed: ${failure.detail}",
                        context = catalogFailureContext(
                            failure.device,
                            connectionDevices.count { it.deviceType != DeviceType.CLIENT },
                            "manifest",
                        ),
                    )
                }
            }
                val stateNow = _state.value
                if (stateNow is UiState.Catalog) {
                    _state.value = if (result != null) {
                    // lastKnownGenres is derived from the *full*, unfiltered
                    // manifest (not what's about to be kept below) — the
                    // Kid Mode rules editor needs to offer every genre in
                    // the real library to restrict, including ones a
                    // currently-active restriction is already hiding, or a
                    // parent could never widen an existing restriction back
                    // out again.
                    latestCatalogEntries = result.entries
                    lastKnownGenres = result.entries.flatMap { it.entry.genres }.distinct().sortedWith(String.CASE_INSENSITIVE_ORDER)
                    stateNow.copy(entries = applyKidModeFilter(result.entries), loading = false, unreachable = result.unreachable)
                    } else {
                    // Same "which devices were actually dialed" filter refresh()
                    // itself applies, so the count shown matches what a
                    // completed (non-timed-out) refresh would have reported.
                    stateNow.copy(loading = false, unreachable = connectionDevices.filter { it.deviceType != DeviceType.CLIENT })
                    }
                }
            } catch (error: CancellationException) {
                throw error
            } catch (error: Exception) {
                // A saved server can disappear between restoration and the
                // first catalog request. Network failures normally become
                // Result.unreachable inside CatalogSession, but guard this
                // outer coroutine as well so an unexpected transport/cache
                // exception can never terminate the Android process.
                Log.e(logTag, "catalog loading failed without a result", error)
                val stateNow = _state.value
                if (stateNow is UiState.Catalog) {
                    _state.value = stateNow.copy(
                        loading = false,
                        unreachable = connectionDevices.filter { it.deviceType != DeviceType.CLIENT },
                    )
                }
                notify("The media server is not reachable right now.", ClientNotificationKind.WARNING)
            }
            if (_state.value is UiState.Catalog) startCatalogChangeFeed(connectionDevices)
        }
    }

    /**
     * Keeps one long-held request open to each media server. Deltas are
     * merged into the existing screen state; no loading flag or navigation
     * transition is involved, so Compose updates only the affected content.
     */
    private fun startCatalogChangeFeed(devices: List<SwarmDevice>) {
        catalogUpdatesJob?.cancel()
        val servers = devices.filter { it.deviceType != DeviceType.CLIENT }
        catalogUpdatesJob = viewModelScope.launch {
            servers.forEach { server ->
                launch {
                    while ((_state.value.diagnosticCatalog() ?: _state.value.browsePreviewCatalog()) != null) {
                        val poll = catalogSession.pollChanges(
                            server,
                            devices,
                            clientCertificate,
                            clientKey,
                        )
                        if (!poll.supported) break
                        poll.entries?.let(::publishCatalogUpdate)
                        // A normal quiet poll already waits twenty seconds;
                        // this small floor prevents a failed connection from
                        // becoming a tight reconnect loop.
                        delay(250)
                    }
                }
            }
        }
    }

    private fun publishCatalogUpdate(entries: List<MergedEntry>) {
        latestCatalogEntries = entries
        lastKnownGenres = entries.flatMap { it.entry.genres }
            .distinct()
            .sortedWith(String.CASE_INSENSITIVE_ORDER)
        val currentCatalog = _state.value.diagnosticCatalog() ?: _state.value.browsePreviewCatalog() ?: return
        val updatedCatalog = currentCatalog.copy(
            entries = applyKidModeFilter(entries),
            loading = false,
        )
        _state.value = replaceEmbeddedCatalog(_state.value, updatedCatalog)
    }

    /** Rebuilds derived shelves around a new catalog while preserving the
     * viewer's current screen, selected artist/show/season, and playback. */
    private fun replaceEmbeddedCatalog(state: UiState, catalog: UiState.Catalog): UiState = when (state) {
        is UiState.Catalog -> catalog
        is UiState.ArtistShelf -> state.copy(
            catalog = catalog,
            artists = CatalogGrouping.groupTracksByArtistAlbum(catalog.entries),
        )
        is UiState.MovieShelf -> state.copy(
            catalog = catalog,
            movies = catalog.entries.filter { it.entry.kind == MediaKind.MOVIE },
        )
        is UiState.ShowShelf -> state.copy(
            catalog = catalog,
            shows = CatalogGrouping.groupEpisodesByShowSeason(catalog.entries),
        )
        is UiState.ArtistAlbums -> {
            val artists = CatalogGrouping.groupTracksByArtistAlbum(catalog.entries)
            state.copy(
                previous = replaceEmbeddedCatalog(state.previous, catalog),
                catalog = catalog,
                artists = artists,
                artist = artists.find { it.artist == state.artist.artist } ?: state.artist,
            )
        }
        is UiState.ShowSeasons -> {
            val shows = CatalogGrouping.groupEpisodesByShowSeason(catalog.entries)
            val show = shows.find { it.show == state.show.show } ?: state.show
            state.copy(
                previous = replaceEmbeddedCatalog(state.previous, catalog),
                catalog = catalog,
                shows = shows,
                show = show,
                selectedSeason = state.selectedSeason?.let { selected ->
                    show.seasons.find { it.season == selected.season } ?: selected
                },
            )
        }
        is UiState.MovieDetail -> state.copy(
            previous = replaceEmbeddedCatalog(state.previous, catalog),
            entry = catalog.entries.find { it.fingerprint == state.entry.fingerprint } ?: state.entry,
        )
        is UiState.PreparingPlayback -> state.copy(
            previous = replaceEmbeddedCatalog(state.previous, catalog),
            prepared = state.prepared?.let { replaceEmbeddedCatalog(it, catalog) as? UiState.Player },
        )
        is UiState.Player -> state.copy(previous = replaceEmbeddedCatalog(state.previous, catalog))
        else -> state
    }

    /**
     * The proxy URL for [entry]'s artwork, or null if it never scraped any
     * (`artworkEtag == null` — no point spending a request finding that
     * out). Movies/episodes use the `poster` kind, tracks `cover` — the
     * scraper (`swarm-media`) always writes exactly one of those per kind
     * of entry, per `docs/PROTOCOL.md`'s artwork section.
     */
    fun artworkUrl(entry: MergedEntry): String? {
        val version = entry.entry.artworkEtag ?: return null
        val kind = if (entry.entry.kind == MediaKind.TRACK) "cover" else "poster"
        return catalogSession.urlFor(entry.sources.first(), "/art/${entry.entry.entryKey}/$kind?v=$version&w=320")
    }

    /** The poster TMDB provides for this episode's season. */
    fun seasonArtworkUrl(entry: MergedEntry): String? {
        val version = entry.entry.artworkEtag ?: return null
        return catalogSession.urlFor(entry.sources.first(), "/art/${entry.entry.entryKey}/season?v=$version&w=320")
    }

    /** The landscape still TMDB provides for this specific episode. */
    fun episodeArtworkUrl(entry: MergedEntry): String? {
        val version = entry.entry.artworkEtag ?: return null
        return catalogSession.urlFor(entry.sources.first(), "/art/${entry.entry.entryKey}/backdrop?v=$version&w=480")
    }

    /** Full-resolution poster/cover for detail and now-playing screens. */
    fun fullArtworkUrl(entry: MergedEntry): String? {
        val version = entry.entry.artworkEtag ?: return null
        val kind = if (entry.entry.kind == MediaKind.TRACK) "cover" else "poster"
        return catalogSession.urlFor(entry.sources.first(), "/art/${entry.entry.entryKey}/$kind?v=$version")
    }

    /** Movie/episode backdrop art for detail screens — best-effort, same gate as [artworkUrl]; a 404 (backdrop never scraped) just fails the image load silently, same as this app's existing artwork handling. */
    fun backdropUrl(entry: MergedEntry): String? {
        val version = entry.entry.artworkEtag ?: return null
        return catalogSession.urlFor(entry.sources.first(), "/art/${entry.entry.entryKey}/backdrop?v=$version")
    }

    /** Track-only fallback visual for the music player when no cover art was scraped — an artist photo reads better as "something to look at" than a blank/placeholder square. Same best-effort gate as [artworkUrl]; a 404 just fails the image load silently. */
    fun artistPhotoUrl(entry: MergedEntry): String? {
        val version = entry.entry.artworkEtag ?: return null
        return catalogSession.urlFor(entry.sources.first(), "/art/${entry.entry.entryKey}/artist?v=$version")
    }

    /** Shelf-sized artist photo; keeps browse cards from decoding the full-resolution source image. */
    fun artistPhotoThumbnailUrl(entry: MergedEntry): String? {
        val version = entry.entry.artworkEtag ?: return null
        return catalogSession.urlFor(entry.sources.first(), "/art/${entry.entry.entryKey}/artist?v=$version&w=320")
    }

    /**
     * Negotiates a budgeted direct/HLS session on the first connected
     * source, resuming where a previous watch left off unless it was
     * already finished. Callable from the top-level [Catalog] screen or
     * from any of the hierarchical browse/detail screens built on top of
     * it — the exact current screen (not just its embedded [Catalog]) is
     * what Back returns to, see [UiState.Player.previous].
     */
    fun play(entry: MergedEntry, startPaused: Boolean = false) {
        val current = _state.value
        val catalog = current.embeddedCatalog() ?: return
        stopBrowsePreview()
        playEntry(
            entry,
            catalog,
            previousScreen = current,
            replaceSession = _minimizedPlayer.value,
            startPaused = startPaused,
        )
    }

    /** Replaces the active movie/episode from its pause-screen recommendation shelf. */
    fun playPauseRecommendation(entry: MergedEntry) {
        val current = _state.value as? UiState.Player ?: return
        if (current.entry.entry.kind == MediaKind.TRACK || entry.entry.kind == MediaKind.TRACK) return
        val catalog = current.previous.embeddedCatalog() ?: return
        playEntry(
            entry = entry,
            catalog = catalog,
            previousScreen = current.previous,
            replaceSession = current,
        )
    }

    /**
     * Begins a preview for the currently focused browse card. Requests are
     * serialized: fast D-pad movement only changes the latest requested
     * entry, and any negotiation that finishes after focus moved is released
     * immediately instead of leaking a transcode/upload slot.
     */
    fun startBrowsePreview(entry: MergedEntry) {
        if (_state.value.browsePreviewCatalog() == null) return
        requestedBrowsePreview = entry
        if (_browsePreview.value?.entryKey == entry.entry.entryKey || browsePreviewWorker?.isActive == true) return

        browsePreviewWorker = viewModelScope.launch {
            while (true) {
                val requested = requestedBrowsePreview ?: return@launch
                if (_browsePreview.value?.entryKey == requested.entry.entryKey) return@launch
                browsePreviewReleaseJob?.join()

                val catalog = _state.value.browsePreviewCatalog() ?: return@launch
                val serverId = requested.sources.firstOrNull() ?: return@launch
                val device = catalog.devices.find { it.deviceId == serverId }?.let(::withPreferredLanRoute)
                    ?: return@launch
                val startPositionSecs = randomPreviewStart(requested)
                val selectionResult = runCatching {
                    withContext(Dispatchers.IO) {
                        catalogSession.preparePlayback(
                            device = device,
                            entryKey = requested.entry.entryKey,
                            startPositionSecs = startPositionSecs,
                            clientCertificate = clientCertificate,
                            clientKey = clientKey,
                            capabilities = playbackCapabilities,
                            preview = true,
                        )
                    }
                }
                val selection = selectionResult.getOrNull()
                if (selection == null) {
                    // Hover previews are enhancement-only. Keep the artwork
                    // visible and avoid a user-facing playback error/toast if
                    // a server is temporarily busy or this file cannot play.
                    Log.w(
                        logTag,
                        "browse preview negotiation failed for ${requested.entry.entryKey}",
                        selectionResult.exceptionOrNull(),
                    )
                    if (requestedBrowsePreview?.entry?.entryKey == requested.entry.entryKey) {
                        requestedBrowsePreview = null
                        return@launch
                    }
                    continue
                }

                if (requestedBrowsePreview?.entry?.entryKey != requested.entry.entryKey ||
                    _state.value.browsePreviewCatalog() == null
                ) {
                    runCatching {
                        releasePlaybackSessionNow(catalog, serverId, selection.sessionId)
                    }.onFailure { Log.w(logTag, "failed to release superseded browse preview", it) }
                    continue
                }

                browsePreviewCatalog = catalog
                _browsePreview.value = BrowsePreview(
                    entryKey = requested.entry.entryKey,
                    url = selection.url,
                    maxBitrate = selection.maxBitrate,
                    seekPositionSecs = if (selection.mode == PlaybackMode.HLS) 0L else startPositionSecs,
                    serverId = serverId,
                    sessionId = selection.sessionId,
                )
                return@launch
            }
        }
    }

    /** Stop and forget the active preview when focus leaves its card. */
    fun stopBrowsePreview() {
        requestedBrowsePreview = null
        val preview = _browsePreview.value ?: return
        _browsePreview.value = null
        val catalog = browsePreviewCatalog
        browsePreviewCatalog = null
        if (!preview.released && catalog != null) scheduleBrowsePreviewRelease(catalog, preview)
    }

    /** Called after 30 seconds of rendered preview. The UI collapses back to
     * box art while this releases the server-side stream reservation. */
    fun finishBrowsePreview(sessionId: String) {
        val preview = _browsePreview.value?.takeIf { it.sessionId == sessionId && !it.released } ?: return
        _browsePreview.value = preview.copy(released = true)
        browsePreviewCatalog?.let { scheduleBrowsePreviewRelease(it, preview) }
    }

    private fun scheduleBrowsePreviewRelease(catalog: UiState.Catalog, preview: BrowsePreview) {
        val prior = browsePreviewReleaseJob
        browsePreviewReleaseJob = viewModelScope.launch {
            prior?.join()
            runCatching {
                releasePlaybackSessionNow(catalog, preview.serverId, preview.sessionId)
            }.onFailure { Log.w(logTag, "failed to release browse preview ${preview.sessionId}", it) }
        }
    }

    private fun randomPreviewStart(entry: MergedEntry): Long {
        return previewStartSeconds(
            durationSecs = entry.entry.durationSecs,
            fraction = Random.nextDouble(from = 0.20, until = 0.35),
        )
    }

    /**
     * Starts the next episode's server stream as soon as the current episode
     * reaches ENDED. The finished reservation is released first so this also
     * works against a server with only one available transcode/upload slot.
     * Failures stay silent here: preloading is an optimization, and [playNext]
     * retries through the normal user-visible negotiation path if needed.
     */
    fun preloadNextEpisode(sessionId: String) {
        val current = _state.value as? UiState.Player ?: return
        val next = current.nextEntry ?: return
        if (current.sessionId != sessionId || current.entry.entry.kind != MediaKind.EPISODE) return
        if (current.preloadedNext != null || nextEpisodePreloadJob?.isActive == true) return
        val catalog = current.previous.embeddedCatalog() ?: return

        val job = viewModelScope.launch {
            val released = if (current.sessionReleased) {
                true
            } else {
                runCatching {
                    releasePlaybackSessionNow(catalog, current.serverId, current.sessionId)
                }.onFailure {
                    Log.w(logTag, "failed to release ended episode ${current.sessionId} before preloading", it)
                }.isSuccess
            }

            val stillCurrent = (_state.value as? UiState.Player)
                ?.takeIf { it.sessionId == current.sessionId && it.nextEntry?.entry?.fingerprint == next.entry.fingerprint }
                ?: return@launch
            if (released && !stillCurrent.sessionReleased) {
                _state.value = stillCurrent.copy(sessionReleased = true)
            }

            val serverId = next.sources.firstOrNull() ?: return@launch
            val device = catalog.devices.find { it.deviceId == serverId }?.let(::withPreferredLanRoute)
                ?: return@launch
            val resumePositionSecs = watchStateStore.get(next.entry.fingerprint)
                ?.takeUnless { it.watched }
                ?.positionSecs
                ?: 0.0
            val selection = runCatching {
                withContext(Dispatchers.IO) {
                    catalogSession.preparePlayback(
                        device,
                        next.entry.entryKey,
                        resumePositionSecs.toLong(),
                        clientCertificate,
                        clientKey,
                        capabilities = playbackCapabilities,
                    )
                }
            }.onFailure {
                Log.w(logTag, "next-episode preload failed for ${next.entry.entryKey}", it)
            }.getOrNull() ?: return@launch

            val currentAfterNegotiation = (_state.value as? UiState.Player)
                ?.takeIf { it.sessionId == current.sessionId && it.nextEntry?.entry?.fingerprint == next.entry.fingerprint }
            if (currentAfterNegotiation == null) {
                runCatching {
                    releasePlaybackSessionNow(catalog, serverId, selection.sessionId)
                }.onFailure { Log.w(logTag, "failed to release abandoned next-episode preload", it) }
                return@launch
            }

            val isHls = selection.mode == PlaybackMode.HLS
            val followingEntry = CatalogGrouping.nextEpisode(
                next,
                CatalogGrouping.groupEpisodesByShowSeason(catalog.entries),
            )
            _state.value = currentAfterNegotiation.copy(
                preloadedNext = PreparedEpisodePlayback(
                    url = selection.url,
                    title = next.entry.displayTitle(),
                    playbackMode = selection.mode,
                    fingerprint = next.entry.fingerprint,
                    resumePositionSecs = if (isHls) 0.0 else resumePositionSecs,
                    positionOffsetSecs = if (isHls) resumePositionSecs else 0.0,
                    maxBitrate = selection.maxBitrate,
                    mediaDurationSecs = next.entry.durationSecs,
                    entry = next,
                    nextEntry = followingEntry,
                    serverId = serverId,
                    sessionId = selection.sessionId,
                    subtitles = selection.subtitles,
                ),
                sessionReleased = currentAfterNegotiation.sessionReleased || released,
            )
        }
        nextEpisodePreloadSessionId = sessionId
        nextEpisodePreloadJob = job
        job.invokeOnCompletion {
            viewModelScope.launch {
                if (nextEpisodePreloadSessionId == sessionId) {
                    nextEpisodePreloadSessionId = null
                }
                if (continueAfterPreloadSessionId == sessionId) {
                    continueAfterPreloadSessionId = null
                    playNext()
                }
            }
        }
    }

    /**
     * Negotiates the next track's stream while the current one is still
     * playing so the hoisted music player can append it as a second
     * playlist item and cross into it with no gap or buffering screen
     * (#160). Unlike [preloadNextEpisode] the current session is *not*
     * released first — the song is still playing — so this needs a server
     * with a spare stream slot; if none is available the negotiation just
     * fails quietly and [playNext] falls back to the normal path. Safe to
     * call repeatedly (on every position tick near the end of a track); it
     * no-ops while a preload is already done or in flight for this session.
     */
    fun preloadNextTrack(sessionId: String) {
        val current = activePlayerSession() ?: return
        if (current.sessionId != sessionId || current.entry.entry.kind != MediaKind.TRACK) return
        // Repeat-song loops the current stream in the player itself, so
        // there is nothing to negotiate ahead — don't reserve a spare
        // server slot for a track that won't play until the user skips.
        if (_repeatMode.value == RepeatMode.ONE) return
        val next = current.nextEntry ?: return
        if (next.entry.fingerprint == current.entry.entry.fingerprint) return
        if (current.preloadedNext != null || nextTrackPreloadJob?.isActive == true) return
        val catalog = current.previous.embeddedCatalog() ?: return
        val serverId = next.sources.firstOrNull() ?: return
        val device = catalog.devices.find { it.deviceId == serverId }?.let(::withPreferredLanRoute) ?: return

        val job = viewModelScope.launch {
            // Queue transitions are new listens, not explicit Resume
            // actions. Reusing a saved position here can make a successor
            // whose last listen stopped near its end play only its final
            // seconds (#249).
            val resumePositionSecs = 0.0
            val selection = runCatching {
                withContext(Dispatchers.IO) {
                    catalogSession.preparePlayback(
                        device,
                        next.entry.entryKey,
                        resumePositionSecs.toLong(),
                        clientCertificate,
                        clientKey,
                        capabilities = playbackCapabilities,
                    )
                }
            }.onFailure {
                Log.w(logTag, "next-track preload failed for ${next.entry.entryKey}", it)
            }.getOrNull() ?: return@launch

            val stillCurrent = (activePlayerSession())
                ?.takeIf { it.sessionId == current.sessionId && it.nextEntry?.entry?.fingerprint == next.entry.fingerprint }
            if (stillCurrent == null || stillCurrent.preloadedNext != null) {
                runCatching { releasePlaybackSessionNow(catalog, serverId, selection.sessionId) }
                    .onFailure { Log.w(logTag, "failed to release abandoned next-track preload", it) }
                return@launch
            }
            val isHls = selection.mode == PlaybackMode.HLS
            val following = CatalogGrouping.nextTrack(
                next,
                CatalogGrouping.groupTracksByArtistAlbum(catalog.entries),
                _shuffleMode.value,
                _repeatMode.value,
            )
            val prepared = PreparedEpisodePlayback(
                url = selection.url,
                title = next.entry.displayTitle(),
                playbackMode = selection.mode,
                fingerprint = next.entry.fingerprint,
                resumePositionSecs = if (isHls) 0.0 else resumePositionSecs,
                positionOffsetSecs = if (isHls) resumePositionSecs else 0.0,
                maxBitrate = selection.maxBitrate,
                mediaDurationSecs = next.entry.durationSecs,
                entry = next,
                nextEntry = following,
                serverId = serverId,
                sessionId = selection.sessionId,
                subtitles = selection.subtitles,
                lyrics = selection.lyrics,
            )
            if (_minimizedPlayer.value?.sessionId == current.sessionId) {
                _minimizedPlayer.value = _minimizedPlayer.value?.copy(preloadedNext = prepared)
            } else if ((_state.value as? UiState.Player)?.sessionId == current.sessionId) {
                _state.value = (_state.value as UiState.Player).copy(preloadedNext = prepared)
            } else {
                runCatching { releasePlaybackSessionNow(catalog, serverId, selection.sessionId) }
            }
        }
        nextTrackPreloadSessionId = sessionId
        nextTrackPreloadJob = job
        job.invokeOnCompletion {
            if (nextTrackPreloadSessionId == sessionId) nextTrackPreloadSessionId = null
        }
    }

    /**
     * Finds and plays whatever comes after the *currently active* session's
     * entry — [UiState.Player.entry] if the full player screen is showing,
     * or [minimizedPlayer]'s if music is playing in the background while
     * the user browses elsewhere (see [activePlayerSession]). No-op if
     * neither is active or there's no next entry ([UiState.Player.nextEntry],
     * from [CatalogGrouping.nextEpisode]/[CatalogGrouping.nextTrack]).
     * A prepared episode is promoted directly, preserving the ExoPlayer
     * buffer built during the Continue countdown.
     */
    fun playNext() {
        val current = activePlayerSession() ?: return
        val next = current.nextEntry ?: return
        val catalog = current.previous.embeddedCatalog() ?: return
        val wasMinimized = _minimizedPlayer.value != null
        // A track prepared ahead of time (#160) is promoted directly whether
        // the full screen or the mini-player is showing — the hoisted music
        // player, keyed on [UiState.Player.musicQueueId], already has this
        // exact stream appended and buffered, so there is no re-negotiation
        // and no re-buffer. An episode's preload only applies on the full
        // screen (its buffer lives in PlayerScreen's own player).
        val preloaded = current.preloadedNext
        if (preloaded != null && preloaded.fingerprint == next.entry.fingerprint &&
            (current.entry.entry.kind == MediaKind.TRACK || !wasMinimized)
        ) {
            if (!current.sessionReleased) {
                releasePlaybackSession(catalog, current.serverId, current.sessionId)
            }
            val promoted = preloaded.toPlayerState(current.previous, musicQueueId = current.musicQueueId)
            if (wasMinimized) _minimizedPlayer.value = promoted else _state.value = promoted
            return
        }
        if (!wasMinimized && current.entry.entry.kind == MediaKind.EPISODE) {
            if (nextEpisodePreloadJob?.isActive == true && nextEpisodePreloadSessionId == current.sessionId) {
                continueAfterPreloadSessionId = current.sessionId
                return
            }
        }
        // Carries the same `previous` forward (not just `catalog`) so Back
        // after auto-playing into a second, third, ... episode/track still
        // returns to the screen the user actually started browsing from,
        // not somewhere reset to the flat catalog. keepMinimized preserves
        // "still in the background" across the track change instead of
        // popping the full screen back up just because playback advanced.
        playEntry(
            next,
            catalog,
            previousScreen = current.previous,
            keepMinimized = wasMinimized,
            replaceSession = current,
            startPositionSecsOverride = if (current.entry.entry.kind == MediaKind.TRACK) 0.0 else null,
            continueMusicQueueId = current.musicQueueId,
        )
    }

    /**
     * ExoPlayer auto-advanced the hoisted music playlist to the track that
     * [preloadNextTrack] appended — same promotion [playNext] does, but
     * driven by the gapless transition rather than a Skip press or an ENDED
     * callback, so a song ending never routes through the loading screen
     * (#160). A no-op if the queued item is not what's actually current now
     * (a race with Skip / shuffle toggle) — [onTrackPlaybackEnded] / the
     * next [playNext] recover.
     */
    fun onMusicPlaylistAdvanced(advancedToUrl: String?) {
        val current = activePlayerSession() ?: return
        if (current.entry.entry.kind != MediaKind.TRACK) return
        val preloaded = current.preloadedNext ?: return
        if (advancedToUrl != null && advancedToUrl != preloaded.url) return
        playNext()
    }

    private fun PreparedEpisodePlayback.toPlayerState(previous: UiState, musicQueueId: String? = null) = UiState.Player(
        url = url,
        title = title,
        playbackMode = playbackMode,
        fingerprint = fingerprint,
        resumePositionSecs = resumePositionSecs,
        positionOffsetSecs = positionOffsetSecs,
        maxBitrate = maxBitrate,
        mediaDurationSecs = mediaDurationSecs,
        entry = entry,
        nextEntry = nextEntry,
        previous = previous,
        serverId = serverId,
        sessionId = sessionId,
        subtitles = subtitles,
        lyrics = lyrics,
        musicQueueId = musicQueueId,
        recommendations = previous.embeddedCatalog()?.entries
            ?.let { pauseRecommendations(entry, it) }
            .orEmpty(),
    )

    /**
     * Seeks media to an absolute position in the original asset. Direct play
     * is handled inside ExoPlayer; this path is used when a progressively
     * generated HLS playlist (video or music) has not reached the requested
     * position yet. A fresh authenticated HLS session starts ffmpeg at the
     * requested point, so the seek is not limited by the current buffer.
     */
    fun seekPlayback(positionSecs: Double) {
        val current = _state.value as? UiState.Player ?: return
        val catalog = current.previous.embeddedCatalog() ?: return
        val duration = current.mediaDurationSecs?.takeIf { it.isFinite() && it > 0.0 }
        val target = if (duration == null) {
            positionSecs.coerceAtLeast(0.0)
        } else {
            positionSecs.coerceIn(0.0, (duration - 0.5).coerceAtLeast(0.0))
        }
        playEntry(
            current.entry,
            catalog,
            previousScreen = current.previous,
            replaceSession = current,
            startPositionSecsOverride = target,
        )
    }

    /** Debug-UAT hook: exercise the real seek/re-negotiation path close enough to the end for completion UI to occur promptly. */
    fun seekPlaybackNearEndForUat() {
        if (!testingModeAvailable || _testingMode.value == null) return
        val duration = (activePlayerSession()?.mediaDurationSecs ?: return).takeIf { it.isFinite() && it > 4.0 } ?: return
        seekPlayback(duration - 3.0)
    }

    /**
     * Debug-UAT resilience hook. It closes the actual catalog transports,
     * republishes the dashboard transition, then performs the same real
     * catalog open/refresh used after a normal reconnect. It never stops or
     * mutates the server and is ignored outside an active debug test mode.
     */
    fun dropAndRecoverTransportForUat() {
        if (!testingModeAvailable || _testingMode.value == null) return
        val current = _state.value
        val catalog = when (current) {
            is UiState.Player -> current.previous.embeddedCatalog()
            else -> current.embeddedCatalog()
        } ?: return
        viewModelScope.launch {
            if (current is UiState.Player && !current.sessionReleased) {
                runCatching { releasePlaybackSessionNow(catalog, current.serverId, current.sessionId) }
            }
            catalog.devices.forEach { catalogSession.disconnect(it.deviceId) }
            val dashboard = UiState.Dashboard(catalog.swarm, catalog.dashboardDevices)
            _state.value = dashboard
            openCatalog(dashboard)
            val deadline = SystemClock.elapsedRealtime() + CATALOG_LOAD_TIMEOUT_MS + 5_000
            while (SystemClock.elapsedRealtime() < deadline) {
                val recovered = _state.value as? UiState.Catalog
                if (recovered != null && !recovered.loading && recovered.entries.isNotEmpty()) {
                    _transportRecoveryGeneration.value += 1
                    return@launch
                }
                delay(100)
            }
        }
    }

    /** Whichever [UiState.Player] is actually live right now — the full screen's own state, or a session still playing in the background after [minimizePlayback]. At most one is ever non-null. */
    private fun activePlayerSession(): UiState.Player? = (_state.value as? UiState.Player) ?: _minimizedPlayer.value

    /** ExoPlayer's own `STATE_ENDED` callback for a track (see the hoisted player in [app.swarm.tv.app.MainActivity]'s `SwarmApp`) — advances to [UiState.Player.nextEntry] if there is one, same as pressing "Play now" would on the episode Continue prompt, but immediate and with no prompt (an unbroken "keep playing" queue is the expected music-player behavior, not a per-track confirmation). Does nothing when there's no next track, same as this app's existing end-of-content behavior for episodes: the player just sits at the ended state until the user backs out. */
    fun onTrackPlaybackEnded() {
        val current = activePlayerSession() ?: return
        if (current.nextEntry != null) {
            playNext()
        } else {
            current.previous.embeddedCatalog()?.let {
                releasePlaybackSession(it, current.serverId, current.sessionId)
            }
        }
    }

    /**
     * Music only — movies/episodes have no shuffle concept and never call
     * this. Cycles OFF -> shuffle album -> shuffle all songs -> OFF and, if
     * a track is currently active (full-screen or minimized), immediately
     * recomputes its `nextEntry` under the new mode so the "what's up next"
     * the UI reflects updates right away rather than only on the *next*
     * track change. Any track already buffered ahead under the old mode is
     * dropped so the change takes effect on the very next transition.
     */
    fun toggleShuffle() {
        _shuffleMode.value = _shuffleMode.value.next()
        recomputeActiveTrackSuccessor()
    }

    /**
     * Music only — the repeat button. Cycles OFF -> repeat song -> repeat
     * album -> OFF. Repeat-song loops the current stream in the hoisted
     * player itself (see [app.swarm.tv.app.MainActivity]); repeat-album is
     * expressed through [CatalogGrouping.nextTrack]/[CatalogGrouping.previousTrack]
     * wrapping inside the current album, so like [toggleShuffle] this
     * recomputes the active track's `nextEntry` and drops any track already
     * buffered ahead under the old mode.
     */
    fun toggleRepeat() {
        _repeatMode.value = _repeatMode.value.next()
        recomputeActiveTrackSuccessor()
    }

    /**
     * Re-derives the active music session's `nextEntry` under the current
     * [_shuffleMode]/[_repeatMode] and releases any preloaded successor
     * negotiated under the previous settings, so a shuffle/repeat change
     * takes effect on the very next transition instead of only after the
     * one already queued.
     */
    private fun recomputeActiveTrackSuccessor() {
        val current = activePlayerSession() ?: return
        if (current.entry.entry.kind != MediaKind.TRACK) return
        val catalog = current.previous.embeddedCatalog() ?: return
        val next = CatalogGrouping.nextTrack(
            current.entry,
            CatalogGrouping.groupTracksByArtistAlbum(catalog.entries),
            _shuffleMode.value,
            _repeatMode.value,
        )
        current.preloadedNext?.let { prepared ->
            releasePlaybackSession(catalog, prepared.serverId, prepared.sessionId)
        }
        val updated = current.copy(nextEntry = next, preloadedNext = null)
        if (_minimizedPlayer.value != null) _minimizedPlayer.value = updated else _state.value = updated
    }

    /**
     * Music "previous" — plays the track before the active one
     * ([CatalogGrouping.previousTrack]); a no-op if there is nothing
     * before it. The "restart the current song instead when more than a
     * few seconds in" rule lives on [app.swarm.tv.app.ui.screens.MusicPlayerScreen],
     * which seeks the hoisted player to zero without involving the
     * ViewModel. Unlike [playNext] there is no preloaded predecessor, so
     * this always renegotiates a fresh stream.
     */
    fun playPrevious() {
        val current = activePlayerSession() ?: return
        if (current.entry.entry.kind != MediaKind.TRACK) return
        val catalog = current.previous.embeddedCatalog() ?: return
        val previous = CatalogGrouping.previousTrack(
            current.entry,
            CatalogGrouping.groupTracksByArtistAlbum(catalog.entries),
            _repeatMode.value,
        ) ?: return
        playEntry(
            previous,
            catalog,
            previousScreen = current.previous,
            keepMinimized = _minimizedPlayer.value != null,
            replaceSession = current,
            startPositionSecsOverride = 0.0,
            continueMusicQueueId = current.musicQueueId,
        )
    }

    /**
     * "Minimize to tray": leaves the full music player screen for whatever
     * screen it was opened from (same navigation [stopPlayback] would land
     * on), but — unlike [stopPlayback] — keeps the session alive in
     * [minimizedPlayer] instead of releasing it, so the hoisted player in
     * `SwarmApp` keeps playing in the background while the user browses.
     * No-op for anything that isn't a track — movies/episodes have no
     * minimize button in the first place, but this guards the ViewModel
     * method too rather than trusting the UI alone.
     */
    fun minimizePlayback() {
        val current = _state.value
        if (current !is UiState.Player || current.entry.entry.kind != MediaKind.TRACK) return
        _minimizedPlayer.value = current
        _state.value = current.previous
    }

    /** Re-enters the full music player screen for whatever's playing in the background — same session, same ExoPlayer instance (keyed on sessionId in `SwarmApp`), no re-negotiation. */
    fun restoreMinimizedPlayback() {
        val minimized = _minimizedPlayer.value ?: return
        _minimizedPlayer.value = null
        _state.value = minimized
    }

    /** Stops a minimized session entirely (the mini-bar's own stop control) without returning to the full player screen first. */
    fun stopMinimizedPlayback() {
        val minimized = _minimizedPlayer.value ?: return
        _minimizedPlayer.value = null
        minimized.previous.embeddedCatalog()?.let { catalog ->
            releasePlaybackSession(catalog, minimized.serverId, minimized.sessionId)
            minimized.preloadedNext?.let { prepared ->
                releasePlaybackSession(catalog, prepared.serverId, prepared.sessionId)
            }
        }
    }

    /** Best-effort, fire-and-forget release of a just-finished player's server-side bandwidth reservation — see [CatalogSession.stopPlayback]. */
    private fun releasePlaybackSession(catalog: UiState.Catalog, serverId: String, sessionId: String) {
        viewModelScope.launch {
            runCatching {
                releasePlaybackSessionNow(catalog, serverId, sessionId)
            }.onFailure { Log.w(logTag, "failed to release playback session $sessionId", it) }
        }
    }

    /** Awaitable counterpart used when replacing one session with another; the new `/play` must not race the old `/stop`. */
    private suspend fun releasePlaybackSessionNow(catalog: UiState.Catalog, serverId: String, sessionId: String) {
        val device = catalog.devices.find { it.deviceId == serverId }?.let(::withPreferredLanRoute) ?: return
        withContext(Dispatchers.IO) {
            catalogSession.stopPlayback(device, sessionId, clientCertificate, clientKey)
        }
        _lastReleasedPlaybackSession.value = sessionId
    }

    /**
     * Best-effort, fire-and-forget report of a client-observed error back to
     * [device] — see [CatalogSession.reportError]'s doc comment for why
     * failures here are swallowed rather than surfaced. Reports the error to
     * the specific server the failure is about (rather than every reachable
     * server) since that's the one whose swarm page a human would actually
     * be looking at to triage it.
     */
    private fun reportClientError(
        device: SwarmDevice,
        message: String,
        entry: MergedEntry? = null,
        context: String? = null,
    ) {
        val id = deviceId ?: return
        val name = deviceName ?: "Fire TV"
        val occurredAtMs = System.currentTimeMillis()
        val stateSnapshot = _state.value
        val catalogSnapshot = stateSnapshot.diagnosticCatalog()
        val connectionMode = if (localSession) "lan" else "swarm"
        val clientCertFingerprint = certFingerprint
        val swarmIdSnapshot = swarmId
        val pendingReportCount = pendingClientErrors.size
        val kidModeEnabled = _kidModeSettings.value != null
        val shuffleMode = _shuffleMode.value
        val minimizedTitle = _minimizedPlayer.value?.title
        val previewEntryKey = _browsePreview.value?.entryKey
        viewModelScope.launch {
            val runtimeDiagnostics = withContext(Dispatchers.IO) {
                runCatching(problemReportDiagnostics::collect)
                    .getOrElse { "Client runtime\ndiagnostics=unavailable (${it.javaClass.simpleName})" }
            }
            val report = ClientErrorReport(
                deviceId = id,
                deviceName = name,
                entryKey = entry?.entry?.entryKey,
                assetTitle = entry?.entry?.displayTitle(),
                kind = entry?.entry?.kind?.name?.lowercase(),
                message = message,
                context = buildClientErrorContext(
                    entry = entry,
                    device = device,
                    screen = stateSnapshot.javaClass.simpleName,
                    connectionMode = connectionMode,
                    clientDeviceId = id,
                    clientMachineId = machineId,
                    clientCertFingerprint = clientCertFingerprint,
                    swarmId = swarmIdSnapshot,
                    catalogEntryCount = catalogSnapshot?.entries?.size ?: 0,
                    catalogServerCount = catalogSnapshot?.devices?.size ?: 0,
                    unreachableServerIds = catalogSnapshot?.unreachable?.map(SwarmDevice::deviceId).orEmpty(),
                    playbackError = catalogSnapshot?.playbackError,
                    pendingReportCount = pendingReportCount,
                    kidModeEnabled = kidModeEnabled,
                    shuffleMode = shuffleMode.name.lowercase(),
                    minimizedTitle = minimizedTitle,
                    previewEntryKey = previewEntryKey,
                    errorDetails = context,
                    runtimeDiagnostics = runtimeDiagnostics,
                ),
                occurredAtMs = occurredAtMs,
            )
            val sent = runCatching {
                withContext(Dispatchers.IO) {
                    catalogSession.reportError(device, report, clientCertificate, clientKey)
                }
            }.onFailure { Log.w(logTag, "failed to report client error to ${device.deviceId}", it) }
                .getOrDefault(false)
            if (!sent) {
                pendingClientErrors += PendingClientError(device, report)
                while (pendingClientErrors.size > 20) pendingClientErrors.removeAt(0)
                Log.w(logTag, "queued client error for retry to ${device.deviceId}")
            }
        }
    }

    private fun flushPendingClientErrors() {
        if (pendingClientErrors.isEmpty()) return
        val pending = pendingClientErrors.toList()
        pendingClientErrors.clear()
        viewModelScope.launch {
            for (item in pending) {
                val sent = withContext(Dispatchers.IO) {
                    catalogSession.reportError(item.device, item.report, clientCertificate, clientKey)
                }
                if (!sent) pendingClientErrors += item
            }
            while (pendingClientErrors.size > 20) pendingClientErrors.removeAt(0)
        }
    }

    /** Polls reachable media servers after a successful connection. Room's
     * insert-ignore result is the one-time-popup gate across app restarts. */
    private suspend fun syncResolutionNotifications(devices: List<SwarmDevice>) {
        val clientId = deviceId ?: return
        for (server in devices.filter { it.deviceType != DeviceType.CLIENT }) {
            notificationServers[server.deviceId] = server
            val remote = withContext(Dispatchers.IO) {
                catalogSession.resolutionNotifications(server, clientId, clientCertificate, clientKey)
            }
            for (notification in remote) {
                if (clientNotificationStore.add(normalizeFingerprint(server.certFingerprint), server.deviceId, server.name, notification)) {
                    val subject = notification.assetTitle?.let { "“$it”" } ?: "Your reported problem"
                    val detail = notification.comments?.takeIf { it.isNotBlank() }
                    notify(
                        if (detail == null) "$subject was resolved." else "$subject was resolved: $detail",
                        ClientNotificationKind.SUCCESS,
                    )
                }
            }
        }
    }

    /**
     * Resolution notifications are otherwise only synced on a fresh LAN
     * connection or a full catalog refresh — neither of which necessarily
     * happens again during a long session, so a resolution that lands after
     * the user is already connected can go unnoticed until one of those
     * happens to recur. Let opening the Notifications tab itself ask each
     * already-known server directly, using the same servers
     * syncResolutionNotifications has already discovered.
     */
    fun refreshResolutionNotifications() {
        viewModelScope.launch {
            syncResolutionNotifications(notificationServers.values.toList())
        }
    }

    fun dismissResolvedProblem(notification: ResolvedProblemNotification) {
        viewModelScope.launch {
            clientNotificationStore.dismiss(notification.key)
            val clientId = deviceId ?: return@launch
            val server = notificationServers[notification.serverId] ?: return@launch
            withContext(Dispatchers.IO) {
                catalogSession.dismissResolutionNotification(
                    server,
                    clientId,
                    notification.remoteId,
                    clientCertificate,
                    clientKey,
                )
            }
        }
    }

    private fun catalogFailureContext(device: SwarmDevice, serverCount: Int, phase: String): String {
        val route = device.metadata["connection_route"] ?: if (device.metadata.containsKey("peer_addr")) "direct" else "unknown"
        return "phase=$phase; route=$route; server_count=$serverCount; peer_addr=${device.metadata["peer_addr"] ?: "unknown"}"
    }

    /**
     * The actual negotiation, shared by [play] and [playNext]. A failed
     * negotiation always surfaces on the top-level [catalog] screen (same
     * recovery behavior this had before hierarchical browsing existed,
     * just now reachable from more places that navigate through it) —
     * pressing play three levels deep in Artist/Album/Episode browsing and
     * having it fail will visibly return to the flat catalog with an error
     * shown, rather than staying on the nested screen. A known rough edge,
     * not a silent one: acceptable for now since it can't lose any state
     * (browsing this deep is cheap to redo) and matches this codebase's
     * existing single-error-surface convention rather than adding
     * per-screen error UI to every new detail/list screen. [previousScreen]
     * is what a *successful* negotiation's Back button returns to instead —
     * see [UiState.Player.previous].
     */
    private fun playEntry(
        entry: MergedEntry,
        catalog: UiState.Catalog,
        previousScreen: UiState,
        keepMinimized: Boolean = false,
        replaceSession: UiState.Player? = null,
        startPositionSecsOverride: Double? = null,
        startPaused: Boolean = false,
        continueMusicQueueId: String? = null,
    ) {
        // ExoPlayer can report ENDED more than once around teardown, and a
        // remote button can repeat. Until negotiation commits a new Player
        // state, both callbacks otherwise see the same old `nextEntry` and
        // reserve duplicate transcode sessions for it.
        if (playbackNegotiationJob?.isActive == true) return
        val requestGeneration = ++playbackRequestGeneration
        preparingResumeRequested = false
        if (replaceSession != null) {
            pendingPlaybackReplacement = PendingPlaybackReplacement(requestGeneration, replaceSession)
            // Drop the client-side reader before doing any network cleanup.
            // For a full-screen player, the black video surface and buffering
            // toast occupy the brief handoff; for a minimized player, removing
            // the mini-session disposes the same hoisted ExoPlayer immediately.
            // No old song continues while `/stop` reconnects or the next
            // `/play` is negotiated.
            if ((_state.value as? UiState.Player)?.sessionId == replaceSession.sessionId) {
                _state.value = UiState.PlaybackLoading
            }
            if (_minimizedPlayer.value?.sessionId == replaceSession.sessionId) {
                _minimizedPlayer.value = null
            }
        } else if (!keepMinimized) {
            // A fresh play with nothing to hand off from: cover the frozen
            // browse screen right away so the tap registers instantly, rather
            // than leaving the user on an unresponsive catalog for the whole
            // negotiation-plus-buffer wait (#122).
            (_state.value as? UiState.PreparingPlayback)?.prepared?.let(::releaseHeldPlayback)
            _state.value = UiState.PreparingPlayback(
                title = entry.entry.displayTitle(),
                artworkUrl = backdropUrl(entry) ?: fullArtworkUrl(entry),
                previous = previousScreen,
                startPaused = startPaused,
            )
        }
        val serverId = entry.sources.first()
        val device = catalog.devices.find { it.deviceId == serverId }?.let(::withPreferredLanRoute)
        val fingerprint = entry.entry.fingerprint
        playbackNegotiationJob = viewModelScope.launch {
            // stopBrowsePreview() has already withdrawn the enhancement.
            // Never wait for its blocking network negotiation or best-effort
            // release before beginning user-requested playback; the server
            // preempts any remaining preview reservation for this request.
            if (requestGeneration != playbackRequestGeneration) return@launch
            if (replaceSession?.preloadedNext != null) {
                runCatching {
                    releasePlaybackSessionNow(
                        catalog,
                        replaceSession.preloadedNext.serverId,
                        replaceSession.preloadedNext.sessionId,
                    )
                }.onFailure {
                    Log.w(logTag, "failed to release unused next-episode preload ${replaceSession.preloadedNext.sessionId}", it)
                }
            }
            if (replaceSession != null && !replaceSession.sessionReleased) {
                runCatching {
                    releasePlaybackSessionNow(
                        catalog,
                        replaceSession.serverId,
                        replaceSession.sessionId,
                    )
                }.onFailure {
                    Log.w(logTag, "failed to release playback session ${replaceSession.sessionId}", it)
                }
            }
            if (requestGeneration != playbackRequestGeneration) return@launch
            val resumePositionSecs = startPositionSecsOverride
                ?: watchStateStore.get(fingerprint)?.takeUnless { it.watched }?.positionSecs
                ?: 0.0
            val selection = runCatching {
                requireNotNull(device) { "server no longer in the swarm roster" }
                withContext(Dispatchers.IO) {
                    catalogSession.preparePlayback(
                        device,
                        entry.entry.entryKey,
                        resumePositionSecs.toLong(),
                        clientCertificate,
                        clientKey,
                        capabilities = playbackCapabilities,
                    )
                }
            }.getOrElse { error ->
                if (requestGeneration != playbackRequestGeneration) return@launch
                if (pendingPlaybackReplacement?.requestGeneration == requestGeneration) {
                    pendingPlaybackReplacement = null
                }
                Log.e(logTag, "playback negotiation failed", error)
                val message = error.message ?: "Could not prepare playback."
                _state.value = catalog.copy(playbackError = message)
                notify(message, ClientNotificationKind.ERROR)
                if (device != null) {
                    reportClientError(
                        device = device,
                        message = message,
                        entry = entry,
                    )
                }
                return@launch
            }
            if (requestGeneration != playbackRequestGeneration) {
                runCatching {
                    releasePlaybackSessionNow(catalog, serverId, selection.sessionId)
                }.onFailure {
                    Log.w(logTag, "failed to release backgrounded playback session ${selection.sessionId}", it)
                }
                return@launch
            }
            val isHls = selection.mode == PlaybackMode.HLS
            // Both of these scan the whole catalog (grouping + a sort). Keep
            // them off the main thread so the browse→player hand-off doesn't
            // stutter on a large library right as the screen swaps in (#122).
            val shuffleMode = _shuffleMode.value
            val repeatMode = _repeatMode.value
            val (nextEntry, recommendations) = withContext(Dispatchers.Default) {
                val next = when (entry.entry.kind) {
                    MediaKind.EPISODE -> CatalogGrouping.nextEpisode(entry, CatalogGrouping.groupEpisodesByShowSeason(catalog.entries))
                    MediaKind.TRACK -> CatalogGrouping.nextTrack(entry, CatalogGrouping.groupTracksByArtistAlbum(catalog.entries), shuffleMode, repeatMode)
                    MediaKind.MOVIE -> null
                }
                next to pauseRecommendations(entry, catalog.entries)
            }
            // A stale error only ever lives on a flat Catalog screen (the
            // only state playbackError is ever set on — see this
            // function's own failure branch above) — clear it there so it
            // doesn't reappear on Back after a *successful* play; nothing
            // to clear on the other, richer previousScreen types.
            val cleanedPrevious = if (previousScreen is UiState.Catalog) previousScreen.copy(playbackError = null) else previousScreen
            // Back from the music player should return to the track's own
            // album, not the album grid (#160): remember which album this
            // track came from on the ArtistAlbums screen underneath.
            val resolvedPrevious = if (entry.entry.kind == MediaKind.TRACK && cleanedPrevious is UiState.ArtistAlbums) {
                cleanedPrevious.copy(initialAlbum = entry.entry.album ?: cleanedPrevious.initialAlbum)
            } else {
                cleanedPrevious
            }
            val playerState = UiState.Player(
                url = selection.url,
                title = entry.entry.displayTitle(),
                playbackMode = selection.mode,
                fingerprint = fingerprint,
                resumePositionSecs = if (isHls) 0.0 else resumePositionSecs,
                positionOffsetSecs = if (isHls) resumePositionSecs else 0.0,
                maxBitrate = selection.maxBitrate,
                mediaDurationSecs = entry.entry.durationSecs,
                entry = entry,
                nextEntry = nextEntry,
                previous = resolvedPrevious,
                serverId = serverId,
                sessionId = selection.sessionId,
                lyrics = selection.lyrics,
                subtitles = selection.subtitles,
                recommendations = recommendations,
                // A Resume press on the PreparingPlayback cover cancels the
                // "open paused" behavior so the stream just starts (#122).
                startPaused = startPaused && !preparingResumeRequested,
                musicQueueId = if (entry.entry.kind == MediaKind.TRACK) {
                    continueMusicQueueId ?: UUID.randomUUID().toString()
                } else {
                    null
                },
            )
            // keepMinimized: an autoplay-to-next-track that started while
            // the mini-bar (not the full screen) was showing stays in the
            // background instead of popping the full player back up just
            // because the track changed underneath it — see [playNext].
            val preparingCover = _state.value as? UiState.PreparingPlayback
            when {
                keepMinimized -> _minimizedPlayer.value = playerState
                // "Continue Watching": the session is ready, but the viewer
                // hasn't pressed Resume yet. Park it behind the cover they are
                // already looking at instead of swapping in a second,
                // identical pause screen on a timer (#211).
                playerState.startPaused && preparingCover != null ->
                    _state.value = preparingCover.copy(prepared = playerState)
                else -> _state.value = playerState
            }
            if (pendingPlaybackReplacement?.requestGeneration == requestGeneration) pendingPlaybackReplacement = null
        }
    }

    /**
     * Reports an ExoPlayer runtime failure (network drop mid-playback, a
     * codec/decoder error, and the like) that happened *after* negotiation
     * already succeeded — distinct from [playEntry]'s own failure branch,
     * which only ever covers the "couldn't even start" case. [PlayerScreen]
     * calls this from its `Player.Listener.onPlayerError`; a no-op if the
     * current screen isn't actually a [UiState.Player] by the time it fires
     * (can race a fast Back-press) or its originating server can no longer
     * be resolved.
     */
    fun reportPlaybackRuntimeError(message: String, context: String? = null) {
        val current = _state.value
        if (current !is UiState.Player) return
        notify(message, ClientNotificationKind.ERROR)
        val device = current.previous.embeddedCatalog()?.devices?.find { it.deviceId == current.serverId } ?: return
        reportClientError(
            device = device,
            message = message,
            entry = current.entry,
            context = context,
        )
    }

    /** A recoverable media-load failure: the player keeps its current buffer
     * and retries forever, so this path only informs the viewer and records
     * diagnostics. [sessionId] rejects late callbacks after Back/cancel or a
     * successful replacement session. */
    fun reportServerOffline(sessionId: String, context: String? = null) {
        val current = activePlayerSession()?.takeIf { it.sessionId == sessionId } ?: return
        playbackConnectionTracker.markOffline(current.serverId, current.sessionId)
        val message = "server has gone offline"
        notify(message, ClientNotificationKind.ERROR)
        val device = current.previous.embeddedCatalog()?.devices?.find { it.deviceId == current.serverId } ?: return
        reportClientError(
            device = device,
            message = message,
            entry = current.entry,
            context = context,
        )
    }

    /** The catalog transport invokes this only after replacing a connection
     * it previously evicted as failed. Restrict the toast to the server that
     * owns active playback so background artwork/catalog recovery stays
     * silent. */
    private fun reportServerReconnected(serverId: String) {
        val active = activePlayerSession()
        if (playbackConnectionTracker.markRestored(serverId, active?.serverId, active?.sessionId)) {
            notify("server has reconnected", ClientNotificationKind.SUCCESS)
        }
    }

    /** Keeps transient playback waits on the shared toast surface instead of
     * covering the video with a separate loading screen. */
    fun reportPlaybackBuffering() {
        if (_state.value !is UiState.Player && _state.value != UiState.PlaybackLoading) return
        notify("Buffering video…", ClientNotificationKind.WARNING)
    }

    /** Makes an adaptive downgrade visible instead of asking the viewer to
     * infer it from a subtle picture change. */
    fun reportPlaybackQualityReduced() {
        if (_state.value !is UiState.Player) return
        notify("Lowering video quality to reduce buffering…", ClientNotificationKind.WARNING)
    }

    /**
     * Replaces a server-side playback session that expired while Media3 was
     * paused. [sessionId] rejects a late callback from an already-disposed
     * player, while [positionSecs] preserves the live playhead rather than
     * falling back to the last position saved before playback began.
     *
     * Applies to both the full player and minimized music: a restarted server
     * loses the old in-memory stream id in either case. This path remains
     * silent for an ordinary idle-expiry 404; an actual outage/reconnect toast
     * is driven by the transport transition tracked above. The 404 is still
     * reported with Media3 context for diagnostics.
     */
    fun recoverExpiredPlaybackSession(sessionId: String, positionSecs: Double, context: String? = null) {
        val current = activePlayerSession()?.takeIf { it.sessionId == sessionId } ?: return
        val keepMinimized = _minimizedPlayer.value?.sessionId == sessionId
        val catalog = current.previous.embeddedCatalog() ?: return
        catalog.devices.find { it.deviceId == current.serverId }?.let { device ->
            reportClientError(
                device = device,
                message = "Playback session expired after an extended pause; recovering automatically.",
                entry = current.entry,
                context = context,
            )
        }
        playEntry(
            entry = current.entry,
            catalog = catalog,
            previousScreen = current.previous,
            keepMinimized = keepMinimized,
            replaceSession = current,
            startPositionSecsOverride = positionSecs,
        )
    }

    /**
     * User-initiated, from a "Report a problem" button on an asset's detail
     * page — distinct from [reportPlaybackRuntimeError] (an automatic report
     * ExoPlayer's own error callback fires) in that this fires whether or
     * not anything actually broke visibly: a user might notice wrong
     * artwork, a mislabeled title, or audio out of sync, none of which
     * throws a [androidx.media3.common.PlaybackException]. Lands in the
     * same place either way — the server's swarm page "Client errors"
     * panel — since triage doesn't care which path found the problem.
     */
    fun reportAssetProblem(entry: MergedEntry) {
        val current = _state.value
        val catalog = current.embeddedCatalog() ?: return
        val device = catalog.devices.find { it.deviceId == entry.sources.first() } ?: return
        Log.i(logTag, "user reported asset problem for ${entry.entry.entryKey}")
        reportClientError(
            device = device,
            message = "User reported a problem with this asset from its detail page.",
            entry = entry,
        )
        notify("Problem report sent.", ClientNotificationKind.SUCCESS)
    }

    /**
     * Toggles [entry]'s liked state — optimistic: [_likedFingerprints] and
     * [likedEntriesStore] both update immediately (instant heart-icon
     * feedback, no round trip on the critical path), then the toggle fires
     * at the first device in [MergedEntry.sources] (same "pick a
     * representative source" pattern [reportAssetProblem] already uses)
     * best-effort — see [CatalogSession.toggleLike]'s doc comment for why a
     * dropped request here never needs to revert the local UI.
     */
    fun toggleLike(entry: MergedEntry) {
        val fingerprint = entry.entry.fingerprint
        val liked = fingerprint !in _likedFingerprints.value
        _likedFingerprints.value = if (liked) _likedFingerprints.value + fingerprint else _likedFingerprints.value - fingerprint
        viewModelScope.launch { likedEntriesStore.setLiked(fingerprint, liked) }

        val catalog = _state.value.embeddedCatalog() ?: return
        val device = catalog.devices.find { it.deviceId == entry.sources.first() } ?: return
        val id = deviceId ?: return
        val name = deviceName ?: "Fire TV"
        viewModelScope.launch {
            runCatching {
                withContext(Dispatchers.IO) {
                    catalogSession.toggleLike(
                        device,
                        LikeToggle(deviceId = id, deviceName = name, entryKey = entry.entry.entryKey, liked = liked),
                        clientCertificate,
                        clientKey,
                    )
                }
            }.onFailure { Log.w(logTag, "failed to toggle like for ${entry.entry.entryKey}", it) }
        }
    }

    fun toggleMovieWatchlist(entry: MergedEntry) {
        val key = WatchlistKeys.movie(entry)
        val listed = key !in _watchlistKeys.value
        if (listed && _watchStates.value[entry.entry.fingerprint]?.watched == true) {
            notify("This movie is already marked watched.", ClientNotificationKind.WARNING)
            return
        }
        setWatchlisted(key, listed)
    }

    fun toggleShowWatchlist(show: ShowGroup) {
        val key = WatchlistKeys.show(show)
        val listed = key !in _watchlistKeys.value
        if (listed && showIsWatched(show, _watchStates.value)) {
            notify("This show is already marked watched.", ClientNotificationKind.WARNING)
            return
        }
        setWatchlisted(key, listed)
    }

    private fun setWatchlisted(key: String, listed: Boolean) {
        _watchlistKeys.value = if (listed) _watchlistKeys.value + key else _watchlistKeys.value - key
        viewModelScope.launch { watchlistStore.setListed(key, listed) }
        notify(if (listed) "Added to Watchlist." else "Removed from Watchlist.", ClientNotificationKind.SUCCESS)
    }

    /** Specials/featurettes do not keep an otherwise-completed show on the Watchlist. */
    private fun showIsWatched(show: ShowGroup, states: Map<String, WatchState>): Boolean {
        val realEpisodes = CatalogGrouping.previewSeasons(show).flatMap { it.episodes }
        return realEpisodes.isNotEmpty() && realEpisodes.all { states[it.entry.fingerprint]?.watched == true }
    }

    // --- Kid Mode ---

    /** True if [entry] passes the currently-active Kid Mode rules (or Kid Mode isn't on at all) — the one predicate every entry point into [applyKidModeFilter] shares. */
    private fun kidModeAllows(entry: MergedEntry, settings: KidModeSettings): Boolean {
        val e = entry.entry
        if (e.kind !in settings.allowedKinds) return false
        if (settings.allowedGenres != null && e.genres.none { it in settings.allowedGenres }) return false
        return when (e.kind) {
            MediaKind.MOVIE -> RatingScale.isAllowed(e.rating, settings.maxMovieRating, RatingScale.MOVIE_ORDER)
            MediaKind.EPISODE -> RatingScale.isAllowed(e.rating, settings.maxTvRating, RatingScale.TV_ORDER)
            MediaKind.TRACK -> true
        }
    }

    /** No-op (returns [entries] unchanged) when Kid Mode isn't enabled — the single chokepoint [browseCatalog] filters every fresh manifest through, so restricted content can't surface via search, genre shelves, or any "Browse all" grid just because one screen forgot to re-check. */
    private fun applyKidModeFilter(entries: List<MergedEntry>): List<MergedEntry> {
        val settings = _kidModeSettings.value?.takeIf { it.enabled } ?: return entries
        return entries.filter { kidModeAllows(it, settings) }
    }

    /** Turns Kid Mode on for the first time (or fully replaces the PIN + rules of an already-on one) — see [AndroidKidModeStore.enable]. Immediately re-filters the currently-browsed catalog, if any, so the new restriction takes effect without waiting for the next [browseCatalog]. */
    fun enableKidMode(pin: String, allowedKinds: Set<MediaKind>, allowedGenres: Set<String>?, maxMovieRating: String?, maxTvRating: String?) {
        viewModelScope.launch {
            kidModeStore.enable(pin, allowedKinds, allowedGenres, maxMovieRating, maxTvRating)
            _kidModeSettings.value = kidModeStore.get()
            reapplyKidModeToCurrentCatalog()
            notify("Family mode enabled.", ClientNotificationKind.SUCCESS)
        }
    }

    /** Edits the content rules on an already-enabled Kid Mode without touching its PIN — see [AndroidKidModeStore.updateRules]. */
    fun updateKidModeRules(allowedKinds: Set<MediaKind>, allowedGenres: Set<String>?, maxMovieRating: String?, maxTvRating: String?) {
        viewModelScope.launch {
            kidModeStore.updateRules(allowedKinds, allowedGenres, maxMovieRating, maxTvRating)
            _kidModeSettings.value = kidModeStore.get()
            reapplyKidModeToCurrentCatalog()
            notify("Family mode settings saved.", ClientNotificationKind.SUCCESS)
        }
    }

    fun disableKidMode() {
        viewModelScope.launch {
            kidModeStore.disable()
            _kidModeSettings.value = null
            reapplyKidModeToCurrentCatalog()
            notify("Family mode disabled.", ClientNotificationKind.WARNING)
        }
    }

    /**
     * Re-applies the Kid Mode filter to whatever's *currently* shown in
     * [UiState.Catalog.entries] — the full unfiltered manifest isn't kept
     * around separately, so this can only re-filter what's already there,
     * not restore something already filtered out. Correct for tightening a
     * restriction (strictly removes entries) but a newly-*widened* one
     * (raising a max rating, adding a genre back) only fully reflects
     * everything again after the next manual resync/re-browse. Acceptable
     * trade-off: Kid Mode is managed from the Settings screen, reached from
     * Dashboard, not mid-browse, so the common case is "browse fresh right
     * after," not "watch this list retroactively grow while already
     * looking at it."
     */
    private fun reapplyKidModeToCurrentCatalog() {
        val current = _state.value
        if (current is UiState.Catalog) _state.value = current.copy(entries = applyKidModeFilter(current.entries))
    }

    // --- Hierarchical browsing: Music (Artist -> Album -> Track) ---

    /** [artists] lets a genre sub-shelf's own "Browse All" tile open this
     * same full grid pre-filtered to just that genre, reusing the one
     * [ArtistShelfScreen] rather than needing a genre-aware variant — the
     * screen itself has no title/header to reflect either way, see
     * [MovieShelfScreen]'s doc comment. Null (the top-level Music row's own
     * tile) falls back to the full catalog, same as before. */
    fun openArtistShelf(artists: List<ArtistGroup>? = null) {
        val current = _state.value
        if (current !is UiState.Catalog) return
        _state.value = UiState.ArtistShelf(current, artists ?: CatalogGrouping.groupTracksByArtistAlbum(current.entries))
    }

    fun openArtistAlbums(artist: ArtistGroup) {
        val previous = _state.value
        val (catalog, artists) = when (previous) {
            is UiState.Catalog -> previous to CatalogGrouping.groupTracksByArtistAlbum(previous.entries)
            is UiState.ArtistShelf -> previous.catalog to previous.artists
            else -> return
        }
        _state.value = UiState.ArtistAlbums(previous, catalog, artists, artist)
    }

    fun backFromArtistShelf() {
        val current = _state.value
        if (current is UiState.ArtistShelf) _state.value = current.catalog
    }

    fun backFromArtistAlbums() {
        val current = _state.value
        if (current is UiState.ArtistAlbums) _state.value = current.previous
    }

    // --- Hierarchical browsing: Movies (flat, just a detail step before play) ---

    fun openMovieDetail(entry: MergedEntry) {
        val current = _state.value
        if (current !is UiState.Catalog && current !is UiState.MovieShelf) return
        _state.value = UiState.MovieDetail(current, entry)
    }

    fun backFromMovieDetail() {
        val current = _state.value
        if (current is UiState.MovieDetail) _state.value = current.previous
    }

    /** [movies] lets a genre sub-shelf's own "Browse All" tile reuse this
     * same full grid pre-filtered to just that genre — see [openArtistShelf]'s
     * doc comment for why a single un-titled screen can serve both cases. */
    fun openMovieShelf(movies: List<MergedEntry>? = null) {
        val current = _state.value
        if (current !is UiState.Catalog) return
        _state.value = UiState.MovieShelf(current, movies ?: current.entries.filter { it.entry.kind == MediaKind.MOVIE })
    }

    fun backFromMovieShelf() {
        val current = _state.value
        if (current is UiState.MovieShelf) _state.value = current.catalog
    }

    // --- Hierarchical browsing: Shows (Show -> Season -> Episode) ---

    /** [shows] lets a genre sub-shelf's own "Browse All" tile reuse this
     * same full grid pre-filtered to just that genre — see [openArtistShelf]'s
     * doc comment for why a single un-titled screen can serve both cases. */
    fun openShowShelf(shows: List<ShowGroup>? = null) {
        val current = _state.value
        if (current !is UiState.Catalog) return
        _state.value = UiState.ShowShelf(current, shows ?: CatalogGrouping.groupEpisodesByShowSeason(current.entries))
    }

    fun openShowSeasons(show: ShowGroup) {
        val previous = _state.value
        val (catalog, shows) = when (previous) {
            is UiState.Catalog -> previous to CatalogGrouping.groupEpisodesByShowSeason(previous.entries)
            is UiState.ShowShelf -> previous.catalog to previous.shows
            else -> return
        }
        _state.value = UiState.ShowSeasons(previous, catalog, shows, show)
    }

    fun backFromShowShelf() {
        val current = _state.value
        if (current is UiState.ShowShelf) _state.value = current.catalog
    }

    fun backFromShowSeasons() {
        val current = _state.value
        if (current is UiState.ShowSeasons) _state.value = current.previous
    }

    fun selectShowSeason(season: SeasonGroup?) {
        val current = _state.value as? UiState.ShowSeasons ?: return
        _state.value = current.copy(selectedSeason = season)
    }

    /** Called when [PlayerScreen] is disposed; 95% counts as complete so credits do not leave an item in Continue Watching. */
    fun savePlaybackPosition(entry: MergedEntry, positionSecs: Double, durationSecs: Double) {
        viewModelScope.launch {
            val fingerprint = entry.entry.fingerprint
            val saved = WatchState.fromPlayback(positionSecs, durationSecs, System.currentTimeMillis())
            watchStateStore.set(fingerprint, saved)
            val states = _watchStates.value + (fingerprint to saved)
            _watchStates.value = states

            if (saved.watched) {
                val completedKey = when (entry.entry.kind) {
                    MediaKind.MOVIE -> WatchlistKeys.movie(entry)
                    MediaKind.EPISODE -> CatalogGrouping.groupEpisodesByShowSeason(latestCatalogEntries)
                        .firstOrNull { show -> show.seasons.any { season -> season.episodes.any { it.entry.fingerprint == fingerprint } } }
                        ?.takeIf { showIsWatched(it, states) }
                        ?.let(WatchlistKeys::show)
                    MediaKind.TRACK -> null
                }
                if (completedKey != null && completedKey in _watchlistKeys.value) {
                    _watchlistKeys.value = _watchlistKeys.value - completedKey
                    watchlistStore.setListed(completedKey, false)
                }
            }
        }
    }

    /**
     * Back-press while the [UiState.PreparingPlayback] cover is up: abandon
     * the in-flight negotiation and return to the screen the play started
     * from. Cancelling closes the blocking QUIC request in [CatalogSession],
     * allowing the server to terminate any pending transcode reservation
     * immediately rather than waiting for negotiation or an idle timeout.
     */
    fun cancelPlaybackPreparation() {
        val current = _state.value as? UiState.PreparingPlayback ?: return
        playbackRequestGeneration++
        playbackNegotiationJob?.cancel()
        playbackNegotiationJob = null
        preparingResumeRequested = false
        current.prepared?.let(::releaseHeldPlayback)
        _state.value = current.previous
    }

    /**
     * Releases a [UiState.PreparingPlayback.prepared] session that was
     * negotiated while the cover was up but that the viewer abandoned without
     * ever pressing Resume (#211).
     */
    private fun releaseHeldPlayback(player: UiState.Player) {
        if (player.sessionReleased) return
        player.previous.embeddedCatalog()?.let { catalog ->
            releasePlaybackSession(catalog, player.serverId, player.sessionId)
        }
    }

    /**
     * Resume pressed on the [UiState.PreparingPlayback] cover. If negotiation
     * already finished, its parked session ([UiState.PreparingPlayback.prepared])
     * is committed and starts playing straight away — no intervening pause
     * overlay (#211). If it is still in flight, the intent is recorded so
     * [playEntry] starts playback the moment the session is ready instead of
     * opening paused, and the button becomes a "Starting…" indicator.
     */
    fun resumeFromPreparingPlayback() {
        val current = _state.value as? UiState.PreparingPlayback ?: return
        if (!current.startPaused || current.resumeRequested) return
        preparingResumeRequested = true
        val prepared = current.prepared
        _state.value = prepared?.copy(startPaused = false) ?: current.copy(resumeRequested = true)
    }

    fun stopPlayback() {
        val current = _state.value
        if (current is UiState.Player) {
            continueAfterPreloadSessionId = null
            current.previous.embeddedCatalog()?.let { catalog ->
                if (!current.sessionReleased) {
                    releasePlaybackSession(catalog, current.serverId, current.sessionId)
                }
                current.preloadedNext?.let { prepared ->
                    releasePlaybackSession(catalog, prepared.serverId, prepared.sessionId)
                }
            }
            _state.value = current.previous
        }
    }

    /**
     * Stops every stream owned by this client when the Activity loses the
     * foreground: full-screen video/music, minimized music, browse previews,
     * prepared next episodes, and a replacement negotiation still in flight.
     * The Compose players pause synchronously on ON_PAUSE; this method removes
     * their state and releases all known server-side reservations.
     */
    fun stopAllStreaming() {
        val current = _state.value as? UiState.Player
        val minimized = _minimizedPlayer.value
        val pendingReplacement = pendingPlaybackReplacement?.player

        // Close an in-flight /play request while the Activity backgrounds.
        // CatalogSession eviction unblocks its blocking QUIC read and the
        // server owns cancellation-safe cleanup before returning a plan.
        playbackRequestGeneration++
        playbackNegotiationJob?.cancel()
        playbackNegotiationJob = null
        preparingResumeRequested = false
        pendingPlaybackReplacement = null
        continueAfterPreloadSessionId = null

        stopBrowsePreview()
        _minimizedPlayer.value = null
        when {
            current != null -> _state.value = current.previous
            _state.value == UiState.PlaybackLoading && pendingReplacement != null -> {
                _state.value = pendingReplacement.previous
            }
            _state.value is UiState.PreparingPlayback -> {
                val preparing = _state.value as UiState.PreparingPlayback
                preparing.prepared?.let(::releaseHeldPlayback)
                _state.value = preparing.previous
            }
        }

        listOfNotNull(current, minimized, pendingReplacement)
            .distinctBy { it.sessionId }
            .forEach { player ->
                player.previous.embeddedCatalog()?.let { catalog ->
                    if (!player.sessionReleased) {
                        releasePlaybackSession(catalog, player.serverId, player.sessionId)
                    }
                    player.preloadedNext?.let { prepared ->
                        releasePlaybackSession(catalog, prepared.serverId, prepared.sessionId)
                    }
                }
            }
    }

    fun backToDashboard() {
        val current = _state.value
        if (current is UiState.Catalog) {
            catalogUpdatesJob?.cancel()
            catalogUpdatesJob = null
            if (localSession) {
                _state.value = UiState.Dashboard(current.swarm, current.dashboardDevices)
            } else {
                _state.value = UiState.Dashboard(current.swarm, current.dashboardDevices, allSwarms = cachedSwarms)
            }
        }
    }

    /**
     * Opens a signaling session and, if that succeeds, resolves the
     * reflector's address and wires [catalogSession]'s [PunchFallback].
     * Best-effort and never fatal — see the class doc.
     */
    private suspend fun establishSignaling(baseUrl: String, accessToken: String, deviceId: String) {
        val (signalingClient, signalRx) = try {
            SignalingClient.connect(baseUrl, accessToken, deviceId, capabilities = playbackCapabilities)
        } catch (e: Exception) {
            return
        }
        val reflectorAddr = resolveReflectorAddr(baseUrl, signalingClient.reflectorPorts)
        if (reflectorAddr == null) {
            signalingClient.close()
            return
        }
        signaling = signalingClient
        catalogSession.punchFallback = PunchFallback(signalingClient, signalRx, reflectorAddr, certFingerprint)
    }

    /** The reflector runs inside the STUN server process, so its address is the STUN host plus whichever port `hello_ack` advertised as live. */
    private suspend fun resolveReflectorAddr(baseUrl: String, reflectorPorts: List<Int>): InetSocketAddress? {
        val port = reflectorPorts.firstOrNull() ?: return null
        val host = baseUrl.removePrefix("https://").removePrefix("http://").substringBefore('/').substringBefore(':')
        return withContext(Dispatchers.IO) {
            runCatching { InetSocketAddress(InetAddress.getByName(host), port) }.getOrNull()
        }
    }

    private suspend fun loadRoster(
        openBrowseWhenConnected: Boolean = false,
        fallbackDashboard: UiState.Dashboard? = null,
    ) {
        val api = client ?: return
        val token = accessToken ?: return
        val id = swarmId ?: return
        try {
            val previousDashboard = _state.value as? UiState.Dashboard
            _disconnectedServerFingerprints.value = disconnectedServerStore.load(id)
            val roster = api.swarmDevices(token, id)
            val dashboard = UiState.Dashboard(
                roster.swarm,
                dashboardDevices(roster.devices),
                allSwarms = cachedSwarms,
            )
            val gainedFirstConnection = previousDashboard != null &&
                !previousDashboard.devices.any(::isConnectedMediaServer) &&
                dashboard.devices.any(::isConnectedMediaServer)
            if ((openBrowseWhenConnected || gainedFirstConnection) && dashboard.devices.any(::isConnectedMediaServer)) {
                openCatalog(dashboard)
            } else {
                _state.value = dashboard
            }
        } catch (e: StunClientError) {
            val current = (_state.value as? UiState.Dashboard) ?: fallbackDashboard
            if (current != null) {
                Log.w(logTag, "could not refresh the saved STUN roster", e)
                _state.value = current.copy(
                    devices = current.devices.map { it.copy(online = false) },
                    resyncing = false,
                    joiningServer = false,
                    joinServerError = if (current.joiningServer) {
                        e.message ?: "The server was added, but its swarm could not be loaded."
                    } else {
                        current.joinServerError
                    },
                )
            } else {
                _state.value = connectionSetupDashboard(e.message ?: "Could not load the swarm roster.")
            }
        }
    }

    private fun isConnectedMediaServer(device: SwarmDevice): Boolean =
        device.online &&
            (device.deviceType == DeviceType.SERVER || device.deviceType == DeviceType.BOTH) &&
            normalizeFingerprint(device.certFingerprint) !in _disconnectedServerFingerprints.value

    override fun onCleared() {
        catalogUpdatesJob?.cancel()
        clearTestingIdentity()
        lanDiscovery.close()
        signaling?.close()
        catalogSession.close()
        proxy.close()
    }
}

/**
 * Movie and episode previews begin within the 20–35% band instead of using
 * fixed minute offsets or sampling near the ending. Clamp only when a very
 * short video's remaining runtime cannot fit the full preview window.
 */
internal fun previewStartSeconds(durationSecs: Double?, fraction: Double): Long {
    val duration = durationSecs?.takeIf { it.isFinite() && it > 0.0 } ?: return 0L
    val latestFullPreviewStart = (duration - BROWSE_PREVIEW_DURATION_SECS).coerceAtLeast(0.0)
    return (duration * fraction.coerceIn(0.20, 0.35))
        .coerceAtMost(latestFullPreviewStart)
        .toLong()
}
