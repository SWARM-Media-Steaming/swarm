package app.swarm.tv.app

import app.swarm.tv.BuildConfig
import android.app.Activity
import android.content.Intent
import android.net.Uri
import android.os.Bundle
import android.view.WindowManager
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalConfiguration
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.text.font.FontWeight
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.ViewModel
import androidx.lifecycle.ViewModelProvider
import androidx.lifecycle.viewmodel.compose.viewModel
import androidx.media3.common.C
import androidx.media3.common.MediaItem
import androidx.media3.common.PlaybackException
import androidx.media3.common.Player
import androidx.media3.exoplayer.ExoPlayer
import androidx.media3.exoplayer.analytics.AnalyticsListener
import androidx.media3.exoplayer.source.LoadEventInfo
import androidx.media3.exoplayer.source.MediaLoadData
import app.swarm.tv.app.data.AndroidCapabilityProbe
import app.swarm.tv.app.data.AndroidCatalogCache
import app.swarm.tv.app.data.AndroidConnectionStore
import app.swarm.tv.app.data.AndroidClientNotificationStore
import app.swarm.tv.app.data.AndroidDeviceIdentity
import app.swarm.tv.app.data.AndroidDisconnectedServerStore
import app.swarm.tv.app.data.AndroidKidModeStore
import app.swarm.tv.app.data.AndroidLanConnectionStore
import app.swarm.tv.app.data.AndroidLikedEntriesStore
import app.swarm.tv.app.data.AndroidProblemReportDiagnostics
import app.swarm.tv.app.data.AndroidTokenStore
import app.swarm.tv.app.data.KidModeSettings
import app.swarm.tv.app.data.ResolvedProblemNotification
import app.swarm.tv.app.data.LanDiscoveryManager
import app.swarm.tv.app.data.LanPairingActivation
import app.swarm.tv.app.data.LanServer
import app.swarm.tv.app.data.AndroidWatchStateStore
import app.swarm.tv.app.data.AndroidWatchlistStore
import app.swarm.tv.app.data.WatchlistKeys
import app.swarm.tv.app.data.BrowsePreview
import app.swarm.tv.app.data.SwarmViewModel
import app.swarm.tv.app.data.TestingModeStatus
import app.swarm.tv.app.data.UiState
import app.swarm.tv.app.data.androidMachineId
import app.swarm.tv.app.data.resolveDeviceName
import app.swarm.tv.app.ui.components.SwarmStartupImage
import app.swarm.tv.app.ui.components.ClientToastHost
import app.swarm.tv.app.ui.components.rememberClientToastHostState
import app.swarm.tv.app.ui.UatTestTags
import app.swarm.tv.app.ui.screens.AlbumScreen
import app.swarm.tv.app.ui.screens.ArtistShelfScreen
import app.swarm.tv.app.ui.screens.BROWSE_ALL_TILE_FOCUS_KEY
import app.swarm.tv.app.ui.screens.CatalogScreen
import app.swarm.tv.app.ui.screens.CatalogBrowseState
import app.swarm.tv.app.ui.screens.ExitConfirmOverlay
import app.swarm.tv.app.ui.screens.MiniPlayerBar
import app.swarm.tv.app.ui.screens.MUSIC_SEEK_STEP_MS
import app.swarm.tv.app.ui.screens.MovieDetailScreen
import app.swarm.tv.app.ui.screens.MovieShelfScreen
import app.swarm.tv.app.ui.screens.MusicPlayerScreen
import app.swarm.tv.app.ui.screens.ActivationCodeScreen
import app.swarm.tv.app.ui.screens.ActivationRequestScreen
import app.swarm.tv.app.ui.screens.PlayerScreen
import app.swarm.tv.app.ui.screens.PlaybackOutageTracker
import app.swarm.tv.app.ui.screens.PreparingPlaybackScreen
import app.swarm.tv.app.ui.screens.isServerOfflineLoadError
import app.swarm.tv.app.ui.screens.playbackErrorContext
import app.swarm.tv.app.ui.screens.playbackHttpResponseCode
import app.swarm.tv.app.ui.screens.serverOfflineMediaSourceFactory
import app.swarm.tv.app.ui.screens.shouldRecoverExpiredPlaybackSession
import app.swarm.tv.app.ui.screens.SeasonScreen
import app.swarm.tv.app.ui.screens.resumeEpisode
import app.swarm.tv.app.ui.screens.ShowShelfScreen
import app.swarm.tv.app.ui.screens.SwarmDashboardScreen
import app.swarm.tv.app.ui.screens.SwarmSettingsScreen
import app.swarm.tv.app.ui.theme.SwarmBackground
import app.swarm.tv.app.ui.theme.SwarmError
import app.swarm.tv.app.ui.theme.SwarmText
import app.swarm.tv.app.ui.theme.SwarmTvTheme
import app.swarm.tv.core.capability.CapabilityProfile
import app.swarm.tv.core.catalog.ArtistGroup
import app.swarm.tv.core.catalog.MergedEntry
import app.swarm.tv.core.catalog.SeasonGroup
import app.swarm.tv.core.catalog.ShowGroup
import app.swarm.tv.core.catalog.RepeatMode
import app.swarm.tv.core.catalog.ShuffleMode
import app.swarm.tv.core.catalog.displayTitle
import app.swarm.tv.core.peer.MediaKind
import app.swarm.tv.core.rest.SwarmDevice
import app.swarm.tv.core.watch.WatchState
import java.io.IOException
import java.security.PrivateKey
import java.security.cert.X509Certificate
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.collect
import kotlinx.coroutines.withContext

/** Resolved off the main thread in onCreate's setContent — see the comment there. */
private data class DeviceIdentity(
    val fingerprint: String,
    val certificate: X509Certificate,
    val privateKey: PrivateKey,
)

/** Identity plus the one-time decoder/display capability probe, both resolved
 * together off the main thread before the ViewModel is built. */
private data class StartupState(
    val identity: DeviceIdentity,
    val capabilities: CapabilityProfile,
)

/** How far before a song's end the next track's stream is negotiated so it
 * can be appended to the player and buffered for a seamless transition (#160). */
private const val PRELOAD_NEXT_TRACK_LEAD_MS = 30_000L

private const val EXTRA_ENABLE_TESTING_MODE = "app.swarm.tv.extra.ENABLE_TESTING_MODE"
private const val EXTRA_DISABLE_TESTING_MODE = "app.swarm.tv.extra.DISABLE_TESTING_MODE"
private const val EXTRA_TESTING_TOKEN = "app.swarm.tv.extra.TESTING_TOKEN"

class MainActivity : ComponentActivity() {
    private var frameJankMonitor: FrameJankMonitor? = null
    private var activeViewModel: SwarmViewModel? = null

    override fun onStart() {
        super.onStart()
        if (applicationInfo.flags and android.content.pm.ApplicationInfo.FLAG_DEBUGGABLE != 0) {
            frameJankMonitor = FrameJankMonitor().also(FrameJankMonitor::start)
        }
    }

    override fun onStop() {
        frameJankMonitor?.stop()
        frameJankMonitor = null
        super.onStop()
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        // Real bug this fixes: without an explicit edge-to-edge opt-in, some
        // real TV hardware lays this Activity's window out inset from the
        // actual display bounds — content renders centered with unused
        // space around it instead of filling the screen. Compose already
        // owns every inset/margin decision this app needs (the manual
        // overscan-safe padding below), so there's nothing this should be
        // deferring to the system for.
        enableEdgeToEdge()

        val tokenStore = AndroidTokenStore(applicationContext)
        val watchStateStore = AndroidWatchStateStore(applicationContext)
        val watchlistStore = AndroidWatchlistStore(applicationContext)
        val connectionStore = AndroidConnectionStore(applicationContext)
        val likedEntriesStore = AndroidLikedEntriesStore(applicationContext)
        val kidModeStore = AndroidKidModeStore(applicationContext)
        val clientNotificationStore = AndroidClientNotificationStore(applicationContext)
        val lanDiscovery = LanDiscoveryManager(applicationContext)
        val lanConnectionStore = AndroidLanConnectionStore(applicationContext)
        val disconnectedServerStore = AndroidDisconnectedServerStore(applicationContext)
        val catalogCache = AndroidCatalogCache(applicationContext)
        val machineId = androidMachineId(applicationContext)
        val defaultDeviceName = resolveDeviceName(applicationContext)
        val initialTestingToken = intent
            .takeIf { BuildConfig.DEBUG && it.getBooleanExtra(EXTRA_ENABLE_TESTING_MODE, false) }
            ?.getStringExtra(EXTRA_TESTING_TOKEN)

        setContent {
            SwarmTvTheme {
                Box(modifier = Modifier.fillMaxSize().background(SwarmBackground)) {
                    // AndroidDeviceIdentity touches AndroidKeyStore and, on
                    // first launch (or whenever the alias is missing),
                    // synchronously generates an EC keypair in secure
                    // hardware — slow enough on some real devices to
                    // noticeably delay time-to-first-frame if resolved
                    // before setContent() as this used to. Resolve it off
                    // the main thread instead and hold the loading frame
                    // (same one UiState.Loading already shows a moment
                    // later) until it's ready.
                    var startup by remember { mutableStateOf<StartupState?>(null) }
                    LaunchedEffect(Unit) {
                        startup = withContext(Dispatchers.IO) {
                            catalogCache.clearTestingResidue()
                            AndroidDeviceIdentity.clearTestingIdentity()
                            val identity = DeviceIdentity(
                                fingerprint = AndroidDeviceIdentity.ensureFingerprint(),
                                certificate = AndroidDeviceIdentity.certificate(),
                                privateKey = AndroidDeviceIdentity.privateKey(),
                            )
                            val capabilities = runCatching {
                                AndroidCapabilityProbe(applicationContext).probe()
                            }.getOrDefault(CapabilityProfile.fireTvBaseline())
                            StartupState(identity, capabilities)
                        }
                    }
                    val resolvedStartup = startup
                    if (resolvedStartup == null) {
                        Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
                            SwarmStartupImage()
                        }
                        return@Box
                    }
                    val resolvedIdentity = resolvedStartup.identity
                    val factory = remember(resolvedStartup) {
                        object : ViewModelProvider.Factory {
                            @Suppress("UNCHECKED_CAST")
                            override fun <T : ViewModel> create(modelClass: Class<T>): T =
                                SwarmViewModel(
                                    tokenStore,
                                    machineId,
                                    resolvedIdentity.fingerprint,
                                    resolvedIdentity.certificate,
                                    resolvedIdentity.privateKey,
                                    watchStateStore,
                                    watchlistStore,
                                    connectionStore,
                                    likedEntriesStore,
                                    kidModeStore,
                                    clientNotificationStore,
                                    lanDiscovery,
                                    lanConnectionStore,
                                    disconnectedServerStore,
                                    catalogCache,
                                    BuildConfig.SWARM_RENDEZVOUS_URL,
                                    AndroidProblemReportDiagnostics(applicationContext),
                                    playbackCapabilities = resolvedStartup.capabilities,
                                    testingModeAvailable = BuildConfig.DEBUG,
                                    initialTestingToken = initialTestingToken,
                                    testingIdentityProvider = AndroidDeviceIdentity::testingIdentity,
                                    clearTestingIdentity = AndroidDeviceIdentity::clearTestingIdentity,
                                ) as T
                        }
                    }
                    val viewModel: SwarmViewModel = viewModel(factory = factory)
                    activeViewModel = viewModel
                    val toastHostState = rememberClientToastHostState()
                    LaunchedEffect(viewModel) {
                        viewModel.notifications.collect(toastHostState::show)
                    }
                    val state by viewModel.state.collectAsState()
                    val likedFingerprints by viewModel.likedFingerprints.collectAsState()
                    val watchStates by viewModel.watchStates.collectAsState()
                    val watchlistKeys by viewModel.watchlistKeys.collectAsState()
                    val kidModeSettings by viewModel.kidModeSettings.collectAsState()
                    val resolvedProblemNotifications by viewModel.resolvedProblemNotifications.collectAsState()
                    val shuffleMode by viewModel.shuffleMode.collectAsState()
                    val repeatMode by viewModel.repeatMode.collectAsState()
                    val minimizedPlayer by viewModel.minimizedPlayer.collectAsState()
                    val browsePreview by viewModel.browsePreview.collectAsState()
                    val lastReleasedPlaybackSession by viewModel.lastReleasedPlaybackSession.collectAsState()
                    val transportRecoveryGeneration by viewModel.transportRecoveryGeneration.collectAsState()
                    val lanServers by viewModel.lanServers.collectAsState()
                    val lanPairingBusy by viewModel.lanPairingBusy.collectAsState()
                    val lanPairingActivation by viewModel.lanPairingActivation.collectAsState()
                    val lanError by viewModel.lanError.collectAsState()
                    val pairedLanFingerprints by viewModel.pairedLanFingerprints.collectAsState()
                    val pairedLanServers by viewModel.pairedLanServers.collectAsState()
                    val disconnectedServerFingerprints by viewModel.disconnectedServerFingerprints.collectAsState()
                    val testingMode by viewModel.testingMode.collectAsState()
                    val isLikedCallback: (MergedEntry) -> Boolean = remember(likedFingerprints) {
                        { entry -> entry.entry.fingerprint in likedFingerprints }
                    }
                    SwarmApp(
                        state = state,
                        defaultDeviceName = defaultDeviceName,
                        lanServers = lanServers,
                        lanPairingBusy = lanPairingBusy,
                        lanPairingActivation = lanPairingActivation,
                        lanError = lanError,
                        pairedLanFingerprints = pairedLanFingerprints,
                        pairedLanServers = pairedLanServers,
                        disconnectedServerFingerprints = disconnectedServerFingerprints,
                        testingMode = testingMode,
                        testingModeAvailable = BuildConfig.DEBUG,
                        onConnectLan = viewModel::connectLanServer,
                        onStartLanPairing = viewModel::startLanPairing,
                        onCancelLanPairing = viewModel::cancelLanPairing,
                        onDisconnectServer = viewModel::disconnectSwarmServer,
                        onReconnectServer = viewModel::reconnectSwarmServer,
                        onDisconnectLanServer = viewModel::disconnectLanServer,
                        onReconnectLanServer = viewModel::reconnectLanServer,
                        onForgetLanServer = viewModel::forgetLanServer,
                        onEnableTestingMode = viewModel::enableTestingMode,
                        onDisableTestingMode = viewModel::disableTestingMode,
                        isLiked = isLikedCallback,
                        onToggleLike = viewModel::toggleLike,
                        watchStates = watchStates,
                        watchlistKeys = watchlistKeys,
                        onToggleMovieWatchlist = viewModel::toggleMovieWatchlist,
                        onToggleShowWatchlist = viewModel::toggleShowWatchlist,
                        kidModeSettings = kidModeSettings,
                        onEnableKidMode = viewModel::enableKidMode,
                        onUpdateKidModeRules = viewModel::updateKidModeRules,
                        onDisableKidMode = viewModel::disableKidMode,
                        resolvedProblemNotifications = resolvedProblemNotifications,
                        onDismissResolvedProblem = viewModel::dismissResolvedProblem,
                        onRefreshNotifications = viewModel::refreshResolutionNotifications,
                        shuffleMode = shuffleMode,
                        onToggleShuffle = viewModel::toggleShuffle,
                        repeatMode = repeatMode,
                        onPlayPrevious = viewModel::playPrevious,
                        onPreloadNextTrack = viewModel::preloadNextTrack,
                        onMusicPlaylistAdvanced = viewModel::onMusicPlaylistAdvanced,
                        minimizedPlayer = minimizedPlayer,
                        browsePreview = browsePreview,
                        onStartBrowsePreview = viewModel::startBrowsePreview,
                        onStopBrowsePreview = viewModel::stopBrowsePreview,
                        onFinishBrowsePreview = viewModel::finishBrowsePreview,
                        onMinimizePlayback = viewModel::minimizePlayback,
                        onRestoreMinimizedPlayback = viewModel::restoreMinimizedPlayback,
                        onStopMinimizedPlayback = viewModel::stopMinimizedPlayback,
                        onStopAllStreaming = viewModel::stopAllStreaming,
                        onTrackPlaybackEnded = viewModel::onTrackPlaybackEnded,
                        artistPhotoUrl = viewModel::artistPhotoUrl,
                        artistPhotoThumbnailUrl = viewModel::artistPhotoThumbnailUrl,
                        fullArtworkUrl = viewModel::fullArtworkUrl,
                        onStartActivation = viewModel::startActivation,
                        onCancelActivation = viewModel::cancelActivation,
                        onBrowseCatalog = viewModel::browseCatalog,
                        onPlay = viewModel::play,
                        onPlayPaused = { entry -> viewModel.play(entry, startPaused = true) },
                        onPlayPauseRecommendation = viewModel::playPauseRecommendation,
                        onPlayNext = viewModel::playNext,
                        onCancelPlaybackPreparation = viewModel::cancelPlaybackPreparation,
                        onResumePreparingPlayback = viewModel::resumeFromPreparingPlayback,
                        onPreloadNextEpisode = viewModel::preloadNextEpisode,
                        onSeekPlayback = viewModel::seekPlayback,
                        onStopPlayback = viewModel::stopPlayback,
                        onBackToDashboard = viewModel::backToDashboard,
                        artworkUrl = viewModel::artworkUrl,
                        seasonArtworkUrl = viewModel::seasonArtworkUrl,
                        episodeArtworkUrl = viewModel::episodeArtworkUrl,
                        backdropUrl = viewModel::backdropUrl,
                        onReportProblem = viewModel::reportAssetProblem,
                        onSavePlaybackPosition = viewModel::savePlaybackPosition,
                        onRecoverExpiredPlaybackSession = viewModel::recoverExpiredPlaybackSession,
                        onServerOffline = viewModel::reportServerOffline,
                        onPlaybackRuntimeError = viewModel::reportPlaybackRuntimeError,
                        onPlaybackBuffering = viewModel::reportPlaybackBuffering,
                        onPlaybackQualityReduced = viewModel::reportPlaybackQualityReduced,
                        onOpenSettings = viewModel::openSettings,
                        onUpdateBaseUrl = viewModel::updateBaseUrl,
                        onUpdateDeviceName = viewModel::updateDeviceName,
                        onBackFromSettings = viewModel::backFromSettings,
                        onOpenMovie = viewModel::openMovieDetail,
                        onBackFromMovie = viewModel::backFromMovieDetail,
                        onOpenMovieShelf = { movies -> viewModel.openMovieShelf(movies) },
                        onBackFromMovieShelf = viewModel::backFromMovieShelf,
                        onOpenArtistShelf = { artists -> viewModel.openArtistShelf(artists) },
                        onOpenArtist = viewModel::openArtistAlbums,
                        onBackFromArtistShelf = viewModel::backFromArtistShelf,
                        onBackFromArtistAlbums = viewModel::backFromArtistAlbums,
                        onOpenShowShelf = { shows -> viewModel.openShowShelf(shows) },
                        onOpenShow = viewModel::openShowSeasons,
                        onSelectShowSeason = viewModel::selectShowSeason,
                        onBackFromShowShelf = viewModel::backFromShowShelf,
                        onBackFromShowSeasons = viewModel::backFromShowSeasons,
                    )
                    ClientToastHost(
                        state = toastHostState,
                        modifier = Modifier
                            .fillMaxSize()
                            .align(Alignment.BottomEnd)
                            .padding(bottom = if (minimizedPlayer != null) 66.dp else 0.dp),
                    )
                    if (testingMode != null) {
                        lastReleasedPlaybackSession?.let { sessionId ->
                            Box(
                                Modifier.size(1.dp)
                                    .testTag(UatTestTags.PLAYBACK_RELEASED_PREFIX + sessionId),
                            )
                        }
                        Box(
                            Modifier.size(1.dp).testTag(
                                UatTestTags.TRANSPORT_RECOVERY_PREFIX + transportRecoveryGeneration,
                            ),
                        )
                    }
                }
            }
        }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        if (!BuildConfig.DEBUG) return
        when {
            intent.getBooleanExtra(EXTRA_DISABLE_TESTING_MODE, false) ->
                activeViewModel?.disableTestingMode()
            intent.getBooleanExtra(EXTRA_ENABLE_TESTING_MODE, false) ->
                intent.getStringExtra(EXTRA_TESTING_TOKEN)?.let {
                    activeViewModel?.enableTestingModeForAutomation(it)
                }
        }
    }

    /** Instrumented-UAT hooks; ViewModel re-checks active debug testing mode before doing anything. */
    fun seekPlaybackNearEndForUat() {
        if (BuildConfig.DEBUG) activeViewModel?.seekPlaybackNearEndForUat()
    }

    fun dropAndRecoverTransportForUat() {
        if (BuildConfig.DEBUG) activeViewModel?.dropAndRecoverTransportForUat()
    }

    fun disableKidModeForUat() {
        if (BuildConfig.DEBUG) activeViewModel?.disableKidMode()
    }
}

@Composable
private fun SwarmApp(
    state: UiState,
    defaultDeviceName: String,
    lanServers: List<LanServer>,
    lanPairingBusy: Boolean,
    lanPairingActivation: LanPairingActivation?,
    lanError: String?,
    pairedLanFingerprints: Set<String>,
    pairedLanServers: List<LanServer>,
    disconnectedServerFingerprints: Set<String>,
    testingMode: TestingModeStatus?,
    testingModeAvailable: Boolean,
    onConnectLan: (server: LanServer, deviceName: String) -> Unit,
    onStartLanPairing: (server: LanServer, deviceName: String) -> Unit,
    onCancelLanPairing: () -> Unit,
    onDisconnectServer: (SwarmDevice) -> Unit,
    onReconnectServer: (SwarmDevice) -> Unit,
    onDisconnectLanServer: (LanServer) -> Unit,
    onReconnectLanServer: (LanServer) -> Unit,
    onForgetLanServer: (LanServer) -> Unit,
    onEnableTestingMode: () -> Unit,
    onDisableTestingMode: () -> Unit,
    isLiked: (MergedEntry) -> Boolean,
    onToggleLike: (MergedEntry) -> Unit,
    watchStates: Map<String, WatchState>,
    watchlistKeys: Set<String>,
    onToggleMovieWatchlist: (MergedEntry) -> Unit,
    onToggleShowWatchlist: (ShowGroup) -> Unit,
    kidModeSettings: KidModeSettings?,
    onEnableKidMode: (pin: String, allowedKinds: Set<MediaKind>, allowedGenres: Set<String>?, maxMovieRating: String?, maxTvRating: String?) -> Unit,
    onUpdateKidModeRules: (allowedKinds: Set<MediaKind>, allowedGenres: Set<String>?, maxMovieRating: String?, maxTvRating: String?) -> Unit,
    onDisableKidMode: () -> Unit,
    resolvedProblemNotifications: List<ResolvedProblemNotification>,
    onDismissResolvedProblem: (ResolvedProblemNotification) -> Unit,
    onRefreshNotifications: () -> Unit,
    shuffleMode: ShuffleMode,
    onToggleShuffle: () -> Unit,
    repeatMode: RepeatMode,
    onPlayPrevious: () -> Unit,
    onPreloadNextTrack: (String) -> Unit,
    onMusicPlaylistAdvanced: (String?) -> Unit,
    minimizedPlayer: UiState.Player?,
    browsePreview: BrowsePreview?,
    onStartBrowsePreview: (MergedEntry) -> Unit,
    onStopBrowsePreview: () -> Unit,
    onFinishBrowsePreview: (String) -> Unit,
    onMinimizePlayback: () -> Unit,
    onRestoreMinimizedPlayback: () -> Unit,
    onStopMinimizedPlayback: () -> Unit,
    onStopAllStreaming: () -> Unit,
    onTrackPlaybackEnded: () -> Unit,
    artistPhotoUrl: (MergedEntry) -> String?,
    artistPhotoThumbnailUrl: (MergedEntry) -> String?,
    fullArtworkUrl: (MergedEntry) -> String?,
    onStartActivation: (deviceName: String) -> Unit,
    onCancelActivation: () -> Unit,
    onBrowseCatalog: () -> Unit,
    onPlay: (MergedEntry) -> Unit,
    onPlayPaused: (MergedEntry) -> Unit,
    onPlayPauseRecommendation: (MergedEntry) -> Unit,
    onPlayNext: () -> Unit,
    onCancelPlaybackPreparation: () -> Unit,
    onResumePreparingPlayback: () -> Unit,
    onPreloadNextEpisode: (String) -> Unit,
    onSeekPlayback: (Double) -> Unit,
    onStopPlayback: () -> Unit,
    onBackToDashboard: () -> Unit,
    artworkUrl: (MergedEntry) -> String?,
    seasonArtworkUrl: (MergedEntry) -> String?,
    episodeArtworkUrl: (MergedEntry) -> String?,
    backdropUrl: (MergedEntry) -> String?,
    onReportProblem: (MergedEntry) -> Unit,
    onSavePlaybackPosition: (entry: MergedEntry, positionSecs: Double, durationSecs: Double) -> Unit,
    onRecoverExpiredPlaybackSession: (sessionId: String, positionSecs: Double, context: String?) -> Unit,
    onServerOffline: (sessionId: String, context: String?) -> Unit,
    onPlaybackRuntimeError: (message: String, context: String?) -> Unit,
    onPlaybackBuffering: () -> Unit,
    onPlaybackQualityReduced: () -> Unit,
    onOpenSettings: () -> Unit,
    onUpdateBaseUrl: (baseUrl: String) -> Unit,
    onUpdateDeviceName: (name: String) -> Unit,
    onBackFromSettings: () -> Unit,
    onOpenMovie: (MergedEntry) -> Unit,
    onBackFromMovie: () -> Unit,
    onOpenMovieShelf: (List<MergedEntry>) -> Unit,
    onBackFromMovieShelf: () -> Unit,
    onOpenArtistShelf: (List<ArtistGroup>) -> Unit,
    onOpenArtist: (ArtistGroup) -> Unit,
    onBackFromArtistShelf: () -> Unit,
    onBackFromArtistAlbums: () -> Unit,
    onOpenShowShelf: (List<ShowGroup>) -> Unit,
    onOpenShow: (ShowGroup) -> Unit,
    onSelectShowSeason: (SeasonGroup?) -> Unit,
    onBackFromShowShelf: () -> Unit,
    onBackFromShowSeasons: () -> Unit,
) {
    // Real Fire TV hardware (and TVs generally) can crop a border of the
    // rendered frame via overscan — content with no safe margin renders
    // correctly on this machine's screenshot but gets clipped by the
    // physical bezel on the actual TV. Google's TV design guidance is a
    // 5% action-safe margin (2.5% each side); computed from the real
    // reported screen size via LocalConfiguration rather than a fixed dp
    // value so it scales correctly across different real Fire TV models'
    // resolutions/densities. Not applied to PlayerScreen: video content is
    // meant to fill the screen edge-to-edge — padding it would just look
    // like unwanted letterboxing, and it's the one screen where overscan
    // cropping a sliver of picture is the normal, expected trade-off every
    // TV app makes.
    // Which card was last opened from CatalogScreen's own top-level Movies/
    // Shows/Music rows — hoisted here, not inside CatalogScreen itself,
    // because CatalogScreen is a brand-new composable instance every time
    // the UiState swaps away from and back to Catalog (its own `remember`ed
    // state doesn't survive that), while SwarmApp never gets torn down
    // across state changes. Read back by CatalogScreen's
    // initialFocus{Movie,Show,Artist}Key params below — see that screen's
    // own doc comment on those for how they're used.
    var lastFocusedMovieKey by remember { mutableStateOf<String?>(null) }
    var lastFocusedShowKey by remember { mutableStateOf<String?>(null) }
    var lastFocusedArtistKey by remember { mutableStateOf<String?>(null) }
    var catalogBrowseState by remember { mutableStateOf(CatalogBrowseState()) }
    var showCatalogExitConfirm by remember { mutableStateOf(false) }

    // The one track session actually live right now, whichever of the two
    // places it can be is holding it — see minimizePlayback's doc comment.
    // At most one of these is ever non-null.
    val activeMusicSession = (state as? UiState.Player)?.takeIf { it.entry.entry.kind == MediaKind.TRACK } ?: minimizedPlayer

    // Hoisted above both MusicPlayerScreen and MiniPlayerBar, keyed on the
    // music *queue* id (#160) — stable across auto-advancing from track to
    // track, unlike sessionId — so a song ending promotes the next track
    // into the same player instance, keeping the item that
    // preloadNextTrack already appended and buffered instead of tearing the
    // player down and re-buffering from scratch. Falls back to sessionId
    // for safety when there is no queue id. This is also *why* track
    // playback survives minimizing away from MusicPlayerScreen's own
    // composition, unlike PlayerScreen's video player, which is
    // deliberately still tied to its own screen (movies/episodes never
    // minimize, so there's nothing to hoist for). `remember` with a key
    // already gives "build a new one when the key changes, otherwise keep
    // the existing instance" for free — no separate hand-rolled holder
    // class needed.
    val context = LocalContext.current
    val musicPlayer = remember(activeMusicSession?.musicQueueId ?: activeMusicSession?.sessionId) {
        activeMusicSession?.let { session ->
            ExoPlayer.Builder(context)
                .setMediaSourceFactory(serverOfflineMediaSourceFactory(context))
                .build()
                .apply {
                setMediaItem(MediaItem.Builder().setUri(Uri.parse(session.url)).setMediaId(session.title).build())
                if (session.resumePositionSecs > 0) seekTo((session.resumePositionSecs * 1000).toLong())
                playWhenReady = true
                prepare()
            }
        }
    }
    // The listeners below outlive any single track now that the player is
    // queue-keyed, so they must read the *current* session rather than the
    // one captured when the player was built.
    val currentMusicSession = rememberUpdatedState(activeMusicSession)
    PausePlayerWhenAppBackgrounded(musicPlayer)
    var musicIsPlaying by remember(musicPlayer) { mutableStateOf(true) }
    var musicIsLoading by remember(musicPlayer) { mutableStateOf(true) }
    var musicPositionMs by remember(musicPlayer) { mutableLongStateOf(0L) }
    var musicPausedForPreview by remember(musicPlayer) { mutableStateOf(false) }
    val musicPlaybackOutage = remember(musicPlayer) { PlaybackOutageTracker() }

    // Keep the hoisted player's playlist tracking the active session as it
    // advances from song to song (#160). The player instance itself
    // survives — it is keyed on musicQueueId — so this only ever nudges the
    // playlist: load a song it doesn't have yet (the one unavoidable buffer,
    // at the start of a listening session or after a jump the preload did
    // not cover), seek to one already queued and buffered as the next item,
    // or just trim the finished leading item(s).
    LaunchedEffect(musicPlayer, activeMusicSession?.sessionId, activeMusicSession?.url) {
        val player = musicPlayer ?: return@LaunchedEffect
        val session = activeMusicSession ?: return@LaunchedEffect
        val index = (0 until player.mediaItemCount).firstOrNull {
            player.getMediaItemAt(it).localConfiguration?.uri?.toString() == session.url
        }
        when {
            index == null -> {
                player.setMediaItem(
                    MediaItem.Builder().setUri(Uri.parse(session.url)).setMediaId(session.title).build(),
                )
                if (session.resumePositionSecs > 0) player.seekTo((session.resumePositionSecs * 1000).toLong())
                player.playWhenReady = true
                player.prepare()
            }
            index > player.currentMediaItemIndex -> {
                player.seekToDefaultPosition(index)
                player.playWhenReady = true
            }
        }
        while (player.mediaItemCount > 1 && player.currentMediaItemIndex > 0) {
            player.removeMediaItem(0)
        }
    }

    // Append the track preloadNextTrack negotiated so ExoPlayer buffers it
    // ahead of time and crosses into it with no gap — and drop a stale
    // queued item first (a shuffle-mode change replaces the preloaded next
    // with a different track, whose old stream toggleShuffle already
    // released, so it must not stay queued for playback).
    LaunchedEffect(musicPlayer, activeMusicSession?.sessionId, activeMusicSession?.preloadedNext?.url) {
        val player = musicPlayer ?: return@LaunchedEffect
        val session = activeMusicSession ?: return@LaunchedEffect
        val preloadedUrl = session.preloadedNext?.url
        while (player.mediaItemCount > player.currentMediaItemIndex + 1) {
            val tailIndex = player.mediaItemCount - 1
            val tailUrl = player.getMediaItemAt(tailIndex).localConfiguration?.uri?.toString()
            if (tailUrl == preloadedUrl || tailUrl == session.url) break
            player.removeMediaItem(tailIndex)
        }
        if (preloadedUrl != null) {
            val alreadyQueued = (0 until player.mediaItemCount).any {
                player.getMediaItemAt(it).localConfiguration?.uri?.toString() == preloadedUrl
            }
            if (!alreadyQueued) {
                player.addMediaItem(
                    MediaItem.Builder().setUri(Uri.parse(preloadedUrl)).setMediaId(session.preloadedNext!!.title).build(),
                )
            }
        }
    }

    // Ask the ViewModel to negotiate the next track once the current one is
    // within PRELOAD_NEXT_TRACK_LEAD_MS of its end, so the append above can
    // happen before playback reaches the seam.
    LaunchedEffect(musicPlayer, activeMusicSession?.sessionId) {
        val session = activeMusicSession ?: return@LaunchedEffect
        val player = musicPlayer ?: return@LaunchedEffect
        while (true) {
            val duration = player.duration
            val remaining = if (duration == C.TIME_UNSET) Long.MAX_VALUE else duration - player.currentPosition
            if (remaining in 0..PRELOAD_NEXT_TRACK_LEAD_MS) onPreloadNextTrack(session.sessionId)
            delay(1_000)
        }
    }

    // Inline previews intentionally include audio. If music was already
    // playing in the minimized bar, pause it for the preview and restore it
    // afterward; never mix two unrelated soundtracks together.
    LaunchedEffect(musicPlayer, browsePreview?.sessionId, browsePreview?.released) {
        val previewPlaying = browsePreview != null && !browsePreview.released
        if (previewPlaying && musicPlayer?.isPlaying == true) {
            musicPausedForPreview = true
            musicPlayer.pause()
        } else if (!previewPlaying && musicPausedForPreview) {
            musicPausedForPreview = false
            musicPlayer?.play()
        }
    }

    // Lyrics need a lightweight playhead clock, but only while the full
    // music screen is visible. The minimized player does not trigger a
    // quarter-second recomposition loop across the browsing UI.
    LaunchedEffect(musicPlayer, (state as? UiState.Player)?.sessionId) {
        val visibleSession = (state as? UiState.Player)?.takeIf { it.entry.entry.kind == MediaKind.TRACK }
            ?: return@LaunchedEffect
        while (true) {
            musicPositionMs = (visibleSession.positionOffsetSecs * 1000.0).toLong() + (musicPlayer?.currentPosition ?: 0L)
            delay(250)
        }
    }

    // Repeat-song loops the current stream seamlessly in ExoPlayer itself
    // rather than renegotiating the same track on every ENDED (#161). The
    // other two repeat states advance normally — repeat-album's wrap lives
    // in CatalogGrouping.nextTrack, driving the same preload/advance path
    // as any other track change.
    LaunchedEffect(musicPlayer, repeatMode) {
        musicPlayer?.repeatMode =
            if (repeatMode == RepeatMode.ONE) Player.REPEAT_MODE_ONE else Player.REPEAT_MODE_OFF
    }

    DisposableEffect(musicPlayer) {
        val player = musicPlayer
        val analyticsListener = object : AnalyticsListener {
            override fun onLoadError(
                eventTime: AnalyticsListener.EventTime,
                loadEventInfo: LoadEventInfo,
                mediaLoadData: MediaLoadData,
                error: IOException,
                wasCanceled: Boolean,
            ) {
                if (player == null || wasCanceled || !isServerOfflineLoadError(error)) return
                val failureContext =
                    "position_ms=${player.currentPosition}; buffered_position_ms=${player.bufferedPosition}; " +
                        "load_error=${error.javaClass.simpleName}: ${error.message.orEmpty()}"
                musicPlaybackOutage.onLoadError(failureContext, player.playbackState)?.let { context ->
                    currentMusicSession.value?.let { session -> onServerOffline(session.sessionId, context) }
                }
                if (player.playbackState == Player.STATE_BUFFERING) musicIsLoading = true
            }

            override fun onLoadCompleted(
                eventTime: AnalyticsListener.EventTime,
                loadEventInfo: LoadEventInfo,
                mediaLoadData: MediaLoadData,
            ) {
                musicPlaybackOutage.onLoadCompleted()
                if (player?.playbackState == Player.STATE_READY) musicIsLoading = false
            }
        }
        val listener = object : Player.Listener {
            override fun onIsPlayingChanged(isPlaying: Boolean) {
                musicIsPlaying = isPlaying
            }
            override fun onPlaybackStateChanged(playbackState: Int) {
                if (playbackState == Player.STATE_READY) musicIsLoading = false
                musicPlaybackOutage.onPlaybackStateChanged(playbackState)?.let { context ->
                    currentMusicSession.value?.let { session -> onServerOffline(session.sessionId, context) }
                }
                if (playbackState == Player.STATE_BUFFERING && musicPlaybackOutage.isPending) musicIsLoading = true
                // onTrackPlaybackEnded reads the *current* session fresh off
                // the ViewModel's own state rather than anything captured
                // here, so this stays correct even if nextEntry changed
                // (shuffle toggled) since this listener was attached. Only
                // reached when the next track was *not* already queued as a
                // playlist item; a queued next crosses over via
                // onMediaItemTransition below without ever hitting ENDED.
                if (playbackState == Player.STATE_ENDED) onTrackPlaybackEnded()
            }

            override fun onMediaItemTransition(mediaItem: MediaItem?, reason: Int) {
                // The player reached the end of a song and moved on to the
                // track preloadNextTrack appended — promote it in the
                // ViewModel so state follows the seamless transition (#160).
                if (reason == Player.MEDIA_ITEM_TRANSITION_REASON_AUTO) {
                    onMusicPlaylistAdvanced(mediaItem?.localConfiguration?.uri?.toString())
                }
            }

            override fun onPlayerError(error: PlaybackException) {
                val session = currentMusicSession.value ?: return
                val activePlayer = player ?: return
                val responseCode = playbackHttpResponseCode(error)
                if (!shouldRecoverExpiredPlaybackSession(error.errorCode, responseCode)) return

                // A restarted server accepts the proxy connection again but
                // cannot restore its old in-memory playback session, so the
                // first successful request is a 404. Music used to stop here
                // because only PlayerScreen's video listener renegotiated.
                musicIsLoading = true
                val positionSecs = session.positionOffsetSecs + activePlayer.currentPosition.coerceAtLeast(0L) / 1000.0
                onRecoverExpiredPlaybackSession(
                    session.sessionId,
                    positionSecs,
                    playbackErrorContext(error, activePlayer),
                )
            }
        }
        player?.addAnalyticsListener(analyticsListener)
        player?.addListener(listener)
        onDispose {
            player?.removeAnalyticsListener(analyticsListener)
            player?.removeListener(listener)
            // Position save-on-exit for whichever session *this* player
            // instance was actually playing — mirrors PlayerScreen's own
            // onDispose save, needed here too since this player now
            // outlives any single screen's composition.
            currentMusicSession.value?.let { session ->
                val positionSecs = session.positionOffsetSecs + (player?.currentPosition ?: 0L) / 1000.0
                val durationSecs = player?.duration?.takeIf { it != C.TIME_UNSET }?.let { session.positionOffsetSecs + it / 1000.0 } ?: 0.0
                onSavePlaybackPosition(session.entry, positionSecs, session.mediaDurationSecs ?: durationSecs)
            }
            player?.release()
        }
    }

    // Prevent Fire TV's screensaver/sleep timeout from replacing SWARM with
    // a black screen or launcher while media is active. Music playback is
    // hoisted and can continue behind any browse screen, so this must live
    // here rather than only in MusicPlayerScreen. FLAG_KEEP_SCREEN_ON is
    // foreground-only and is cleared immediately on pause/end/disposal; a
    // broad wake lock would outlive the UI and is neither needed nor wanted.
    val videoPlaybackActive = (state as? UiState.Player)
        ?.entry?.entry?.kind
        ?.let { it != MediaKind.TRACK } == true
    KeepScreenAwakeWhile(
        videoPlaybackActive ||
            (activeMusicSession != null && musicIsPlaying) ||
            (browsePreview != null && !browsePreview.released),
    )

    // #78: Home and Power are intercepted by Fire TV before key dispatch.
    // ON_PAUSE is the earliest reliable foreground-loss signal, so stop every
    // client/server stream here instead of waiting for the later ON_STOP.
    val lifecycleOwner = LocalLifecycleOwner.current
    val latestOnStopAllStreaming = rememberUpdatedState(onStopAllStreaming)
    DisposableEffect(lifecycleOwner) {
        val observer = LifecycleEventObserver { _, event ->
            if (event == Lifecycle.Event.ON_PAUSE) latestOnStopAllStreaming.value()
        }
        lifecycleOwner.lifecycle.addObserver(observer)
        onDispose { lifecycleOwner.lifecycle.removeObserver(observer) }
    }

    val config = LocalConfiguration.current
    val contentModifier = if (state is UiState.Player || state is UiState.PlaybackLoading || state is UiState.PreparingPlayback) {
        Modifier.fillMaxSize()
    } else {
        Modifier.fillMaxSize().padding(
            horizontal = (config.screenWidthDp * 0.025f).dp,
            vertical = (config.screenHeightDp * 0.025f).dp,
        )
    }
    Box(modifier = contentModifier) {
        when (state) {
            is UiState.Loading ->
                Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
                    SwarmStartupImage()
                }
            is UiState.PlaybackLoading ->
                Box(Modifier.fillMaxSize().background(Color.Black)) {
                    LaunchedEffect(Unit) {
                        onPlaybackBuffering()
                    }
                }
            is UiState.PreparingPlayback ->
                PreparingPlaybackScreen(
                    title = state.title,
                    artworkUrl = state.artworkUrl,
                    startPaused = state.startPaused,
                    resumeRequested = state.resumeRequested,
                    ready = state.prepared != null,
                    onResume = onResumePreparingPlayback,
                    onCancel = onCancelPlaybackPreparation,
                )
            is UiState.RequestingActivation ->
                ActivationRequestScreen(onCancel = onCancelActivation)
            is UiState.Activating ->
                ActivationCodeScreen(
                    code = state.code,
                    expiresAt = state.expiresAt,
                    errorMessage = state.error,
                    onCancel = onCancelActivation,
                )
            is UiState.Dashboard ->
                SwarmDashboardScreen(
                    swarm = state.swarm,
                    devices = state.devices,
                    lanServers = lanServers,
                    pairedLanServers = pairedLanServers,
                    pairedLanFingerprints = pairedLanFingerprints,
                    disconnectedServerFingerprints = disconnectedServerFingerprints,
                    lanPairingBusy = lanPairingBusy,
                    lanPairingActivation = lanPairingActivation,
                    lanError = lanError,
                    deviceName = defaultDeviceName,
                    joiningServer = state.joiningServer,
                    joinServerError = state.joinServerError,
                    onBrowseCatalog = onBrowseCatalog,
                    onOpenSettings = onOpenSettings,
                    onAddServer = { onStartActivation(defaultDeviceName) },
                    onConnectLan = onConnectLan,
                    onStartLanPairing = onStartLanPairing,
                    onCancelLanPairing = onCancelLanPairing,
                    onDisconnectServer = onDisconnectServer,
                    onReconnectServer = onReconnectServer,
                    onDisconnectLanServer = onDisconnectLanServer,
                    onReconnectLanServer = onReconnectLanServer,
                    onForgetLanServer = onForgetLanServer,
                    onBackToBrowse = onBrowseCatalog,
                )
            is UiState.Settings ->
                SwarmSettingsScreen(
                    baseUrl = state.baseUrl,
                    deviceName = state.deviceName,
                    busy = state.busy,
                    errorMessage = state.error,
                    onUpdateBaseUrl = onUpdateBaseUrl,
                    onUpdateDeviceName = onUpdateDeviceName,
                    onBack = onBackFromSettings,
                    kidModeSettings = kidModeSettings,
                    availableGenres = state.availableGenres,
                    onEnableKidMode = onEnableKidMode,
                    onUpdateKidModeRules = onUpdateKidModeRules,
                    onDisableKidMode = onDisableKidMode,
                    notifications = resolvedProblemNotifications,
                    onDismissNotification = onDismissResolvedProblem,
                    onRefreshNotifications = onRefreshNotifications,
                    testingModeAvailable = testingModeAvailable,
                    testingMode = testingMode,
                    onEnableTestingMode = onEnableTestingMode,
                    onDisableTestingMode = onDisableTestingMode,
                )
            is UiState.Catalog ->
                CatalogScreen(
                    entries = state.entries,
                    loading = state.loading,
                    unreachable = state.unreachable,
                    playbackError = state.playbackError,
                    artworkUrl = artworkUrl,
                    artistPhotoUrl = artistPhotoThumbnailUrl,
                    onOpenMovie = { entry ->
                        lastFocusedMovieKey = entry.entry.entryKey
                        lastFocusedShowKey = null
                        lastFocusedArtistKey = null
                        onOpenMovie(entry)
                    },
                    // A Browse All tile press remembers a per-kind sentinel so
                    // pressing Back out of the full grid lands focus back on
                    // that tile rather than the filter rail (#159).
                    onOpenMovieShelf = { movies ->
                        lastFocusedMovieKey = BROWSE_ALL_TILE_FOCUS_KEY
                        lastFocusedShowKey = null
                        lastFocusedArtistKey = null
                        onOpenMovieShelf(movies)
                    },
                    onOpenArtistShelf = { artists ->
                        lastFocusedMovieKey = null
                        lastFocusedShowKey = null
                        lastFocusedArtistKey = BROWSE_ALL_TILE_FOCUS_KEY
                        onOpenArtistShelf(artists)
                    },
                    onOpenArtist = { artist ->
                        lastFocusedMovieKey = null
                        lastFocusedShowKey = null
                        lastFocusedArtistKey = artist.artist
                        onOpenArtist(artist)
                    },
                    onOpenShowShelf = { shows ->
                        lastFocusedMovieKey = null
                        lastFocusedShowKey = BROWSE_ALL_TILE_FOCUS_KEY
                        lastFocusedArtistKey = null
                        onOpenShowShelf(shows)
                    },
                    onOpenShow = { show ->
                        lastFocusedMovieKey = null
                        lastFocusedShowKey = show.show
                        lastFocusedArtistKey = null
                        onOpenShow(show)
                    },
                    onOpenSwarm = {
                        showCatalogExitConfirm = false
                        onBackToDashboard()
                    },
                    onBack = {
                        val unreachableIds = state.unreachable.mapTo(mutableSetOf()) { it.deviceId }
                        val hasConnectedServer = state.devices.any {
                            (it.deviceType == app.swarm.tv.core.rest.DeviceType.SERVER ||
                                it.deviceType == app.swarm.tv.core.rest.DeviceType.BOTH) &&
                                it.online && it.deviceId !in unreachableIds
                        }
                        if (hasConnectedServer) {
                            showCatalogExitConfirm = true
                        } else {
                            onBackToDashboard()
                        }
                    },
                    initialFocusMovieKey = lastFocusedMovieKey,
                    initialFocusShowKey = lastFocusedShowKey,
                    initialFocusArtistKey = lastFocusedArtistKey,
                    isLiked = isLiked,
                    watchStates = watchStates,
                    watchlistKeys = watchlistKeys,
                    onPlay = onPlay,
                    onPlayPaused = onPlayPaused,
                    preview = browsePreview,
                    onStartPreview = onStartBrowsePreview,
                    onStopPreview = onStopBrowsePreview,
                    onPreviewFinished = onFinishBrowsePreview,
                    initialBrowseState = catalogBrowseState,
                    onBrowseStateChange = { catalogBrowseState = it },
                )
            is UiState.ArtistShelf ->
                ArtistShelfScreen(
                    state.artists,
                    artworkUrl = artworkUrl,
                    artistPhotoUrl = artistPhotoThumbnailUrl,
                    onOpenArtist = { artist ->
                        lastFocusedMovieKey = null
                        lastFocusedShowKey = null
                        lastFocusedArtistKey = artist.artist
                        onOpenArtist(artist)
                    },
                    onBack = onBackFromArtistShelf,
                    initialFocusKey = lastFocusedArtistKey,
                )
            is UiState.ArtistAlbums ->
                AlbumScreen(
                    state.artist,
                    artworkUrl,
                    onPlay = onPlay,
                    onBack = onBackFromArtistAlbums,
                    initialAlbumKey = state.initialAlbum,
                    activeTrackKey = activeMusicSession?.entry?.entry?.entryKey,
                )
            is UiState.MovieShelf ->
                MovieShelfScreen(
                    state.movies,
                    artworkUrl,
                    onOpenMovie = { entry ->
                        lastFocusedMovieKey = entry.entry.entryKey
                        lastFocusedShowKey = null
                        lastFocusedArtistKey = null
                        onOpenMovie(entry)
                    },
                    onBack = onBackFromMovieShelf,
                    initialFocusKey = lastFocusedMovieKey,
                    preview = browsePreview,
                    onStartPreview = onStartBrowsePreview,
                    onStopPreview = onStopBrowsePreview,
                    onPreviewFinished = onFinishBrowsePreview,
                )
            is UiState.MovieDetail ->
                MovieDetailScreen(
                    state.entry,
                    fullArtworkUrl,
                    backdropUrl,
                    onPlay = onPlay,
                    onBack = onBackFromMovie,
                    onReportProblem = onReportProblem,
                    isLiked = isLiked(state.entry),
                    onToggleLike = { onToggleLike(state.entry) },
                    isWatchlisted = WatchlistKeys.movie(state.entry) in watchlistKeys,
                    onToggleWatchlist = { onToggleMovieWatchlist(state.entry) },
                )
            is UiState.ShowShelf ->
                ShowShelfScreen(
                    state.shows,
                    artworkUrl,
                    onOpenShow = { show ->
                        lastFocusedMovieKey = null
                        lastFocusedShowKey = show.show
                        lastFocusedArtistKey = null
                        onOpenShow(show)
                    },
                    onBack = onBackFromShowShelf,
                    initialFocusKey = lastFocusedShowKey,
                    preview = browsePreview,
                    onStartPreview = onStartBrowsePreview,
                    onStopPreview = onStopBrowsePreview,
                    onPreviewFinished = onFinishBrowsePreview,
                )
            is UiState.ShowSeasons -> {
                // The most recently started-but-unfinished episode of this
                // show, if any — surfaces a Resume button on the season list
                // even when the show has aged out of the 6-item Continue
                // Watching row (#152).
                val resumeTarget = remember(state.show, watchStates) {
                    resumeEpisode(state.show, watchStates)
                }
                SeasonScreen(
                    state.show,
                    seasonArtworkUrl = seasonArtworkUrl,
                    episodeArtworkUrl = episodeArtworkUrl,
                    onPlayEpisode = onPlay,
                    onBack = onBackFromShowSeasons,
                    selectedSeason = state.selectedSeason,
                    onSelectSeason = onSelectShowSeason,
                    isWatchlisted = WatchlistKeys.show(state.show) in watchlistKeys,
                    onToggleWatchlist = { onToggleShowWatchlist(state.show) },
                    onResume = resumeTarget?.let { episode -> { onPlayPaused(episode) } },
                )
            }
            is UiState.Player ->
                if (state.entry.entry.kind == MediaKind.TRACK) {
                    MusicPlayerScreen(
                        entry = state.entry,
                        nextTitle = state.nextEntry?.let { it.entry.displayTitle() },
                        isPlaying = musicIsPlaying,
                        isLoading = musicIsLoading,
                        shuffleMode = shuffleMode,
                        isLiked = isLiked(state.entry),
                        artworkUrl = fullArtworkUrl(state.entry),
                        artistPhotoUrl = artistPhotoUrl(state.entry),
                        lyrics = state.lyrics,
                        positionMs = musicPositionMs,
                        onTogglePlayPause = { musicPlayer?.let { it.playWhenReady = !it.playWhenReady } },
                        onPlay = { musicPlayer?.play() },
                        onPause = { musicPlayer?.pause() },
                        onToggleShuffle = onToggleShuffle,
                        onToggleLike = { onToggleLike(state.entry) },
                        onSkipNext = onPlayNext,
                        onSkipPrevious = onPlayPrevious,
                        onRestartTrack = { musicPlayer?.seekTo(0L) },
                        onSeekForward = {
                            musicPlayer?.let { p ->
                                val target = p.currentPosition + MUSIC_SEEK_STEP_MS
                                val end = p.duration.takeIf { it != C.TIME_UNSET }
                                p.seekTo(if (end != null) target.coerceAtMost(end) else target)
                            }
                        },
                        onSeekBack = {
                            musicPlayer?.let { p ->
                                p.seekTo((p.currentPosition - MUSIC_SEEK_STEP_MS).coerceAtLeast(0L))
                            }
                        },
                        onMinimize = onMinimizePlayback,
                        onClose = onStopPlayback,
                    )
                } else {
                    PlayerScreen(
                        sessionId = state.sessionId,
                        url = state.url,
                        title = state.title,
                        playbackMode = state.playbackMode,
                        resumePositionSecs = state.resumePositionSecs,
                        positionOffsetSecs = state.positionOffsetSecs,
                        mediaDurationSecs = state.mediaDurationSecs,
                        maxBitrate = state.maxBitrate,
                        subtitles = state.subtitles,
                        entry = state.entry,
                        recommendations = state.recommendations,
                        artworkUrl = artworkUrl,
                        hasNext = state.nextEntry != null,
                        nextTitle = state.nextEntry?.let { it.entry.displayTitle() },
                        nextArtworkUrl = state.nextEntry?.let(artworkUrl),
                        preloadedNext = state.preloadedNext,
                        startPaused = state.startPaused,
                        onBack = onStopPlayback,
                        onPositionUpdate = { positionSecs, durationSecs ->
                            onSavePlaybackPosition(
                                state.entry,
                                positionSecs,
                                state.mediaDurationSecs ?: durationSecs,
                            )
                        },
                        onContinue = onPlayNext,
                        onPlayRecommendation = onPlayPauseRecommendation,
                        onPreloadNext = { onPreloadNextEpisode(state.sessionId) },
                        onSeekOutsideBuffer = onSeekPlayback,
                        onPlaybackSessionExpired = { positionSecs, context ->
                            onRecoverExpiredPlaybackSession(state.sessionId, positionSecs, context)
                        },
                        onServerOffline = { context -> onServerOffline(state.sessionId, context) },
                        onPlaybackRuntimeError = onPlaybackRuntimeError,
                        onPlaybackBuffering = onPlaybackBuffering,
                        onPlaybackQualityReduced = onPlaybackQualityReduced,
                    )
                }
        }

        // Rendered as an overlay sibling on top of whatever the when(state)
        // block above just showed, not instead of it — a track playing in
        // the background should stay visible/controllable no matter which
        // browse/detail screen the user is actually looking at. Never shown
        // while UiState.Player itself is the current screen (the full
        // MusicPlayerScreen already covers this exact session).
        if (minimizedPlayer != null) {
            MiniPlayerBar(
                entry = minimizedPlayer.entry,
                isPlaying = musicIsPlaying,
                artworkUrl = artworkUrl(minimizedPlayer.entry),
                onReopen = onRestoreMinimizedPlayback,
                onStop = onStopMinimizedPlayback,
                modifier = Modifier.align(Alignment.BottomEnd).padding(12.dp),
            )
        }
        if (state is UiState.Catalog && showCatalogExitConfirm) {
            ExitConfirmOverlay(
                onConfirmExit = { (context as? Activity)?.finish() },
                onDismiss = { showCatalogExitConfirm = false },
            )
        }
        testingMode?.let {
            TestingModeBanner(
                remainingSeconds = it.remainingSeconds,
                modifier = Modifier.align(Alignment.TopCenter),
            )
        }
    }
}

@Composable
private fun TestingModeBanner(remainingSeconds: Long, modifier: Modifier = Modifier) {
    val minutes = remainingSeconds / 60
    val seconds = remainingSeconds % 60
    Box(
        modifier = modifier
            .fillMaxWidth()
            .background(SwarmError)
            .padding(vertical = 8.dp, horizontal = 24.dp),
        contentAlignment = Alignment.Center,
    ) {
        Text(
            text = "TESTING MODE • pairing 00000000 • %d:%02d remaining".format(minutes, seconds),
            color = SwarmText,
            fontSize = 18.sp,
            fontWeight = FontWeight.ExtraBold,
        )
    }
}

@Composable
private fun KeepScreenAwakeWhile(enabled: Boolean) {
    val activity = LocalContext.current as? Activity
    DisposableEffect(activity, enabled) {
        if (enabled) {
            activity?.window?.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        }
        onDispose {
            if (enabled) {
                activity?.window?.clearFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
            }
        }
    }
}
