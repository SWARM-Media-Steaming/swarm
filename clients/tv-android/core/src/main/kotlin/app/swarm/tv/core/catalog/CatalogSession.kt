/**
 * Turns a swarm roster into a browsable merged catalog. For each server
 * device (`deviceType != CLIENT`), connects over QUIC — [connectToServer]
 * (direct, via the server's self-reported `peer_addr`) first, falling back
 * to [initiatePunchConnection] when [punchFallback] is configured and the
 * direct attempt fails, same best-first-with-fallback philosophy
 * `docs/PROTOCOL.md`'s source-selection section describes — registers the
 * live connection with [proxy] (so a later player can stream from it via
 * [PeerLoopbackProxy.urlFor]), fetches its `/catalog/manifest`, and merges
 * every server's manifest with [CatalogMerger]. A server that isn't
 * reachable by either path is reported in [Result.unreachable] rather than
 * failing the whole refresh: matches the fail-open-to-stale posture
 * `ServerCore` (Rust) already uses for its own roster sync, since one bad
 * peer should never block browsing the rest of the swarm.
 */
package app.swarm.tv.core.catalog

import app.swarm.tv.core.client.SignalingClient
import app.swarm.tv.core.capability.CapabilityProfile
import app.swarm.tv.core.peer.ByteRange
import app.swarm.tv.core.peer.CatalogManifest
import app.swarm.tv.core.peer.CatalogThumbprint
import app.swarm.tv.core.peer.ClientErrorReport
import app.swarm.tv.core.peer.BuzzRequest
import app.swarm.tv.core.peer.BuzzResponse
import app.swarm.tv.core.peer.ClientResolutionNotification
import app.swarm.tv.core.peer.LikeToggle
import app.swarm.tv.core.peer.PlaybackMode
import app.swarm.tv.core.peer.PlaybackPlan
import app.swarm.tv.core.peer.PlaybackPreferences
import app.swarm.tv.core.proxy.PeerLoopbackProxy
import app.swarm.tv.core.rest.DeviceType
import app.swarm.tv.core.rest.SwarmDevice
import app.swarm.tv.core.rest.SwarmJson
import app.swarm.tv.core.signal.SignalMessage
import app.swarm.tv.core.transport.PeerConnection
import app.swarm.tv.core.transport.PeerQuicClient
import app.swarm.tv.core.transport.PeerResponse
import app.swarm.tv.core.transport.connectToServer
import app.swarm.tv.core.transport.initiatePunchConnection
import java.net.InetSocketAddress
import java.io.IOException
import java.security.PrivateKey
import java.security.cert.X509Certificate
import java.util.concurrent.ConcurrentHashMap
import java.util.zip.GZIPInputStream
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.async
import kotlinx.coroutines.channels.ReceiveChannel
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.delay
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeoutOrNull
import kotlinx.serialization.ExperimentalSerializationApi
import kotlinx.serialization.decodeFromString
import kotlinx.serialization.encodeToString
import kotlinx.serialization.json.decodeFromStream

/**
 * What [CatalogSession] needs to attempt a hole-punched connection when a
 * server's `peer_addr` isn't directly reachable (off-LAN). Bundled rather
 * than three loose parameters because they're only ever meaningful
 * together, and because [signalRx] is borrowed exclusively for the
 * duration of one punch attempt — see [initiatePunchConnection]'s doc
 * comment — so whoever constructs this is asserting nothing else is
 * reading this signaling session's inbound messages concurrently.
 */
class PunchFallback(
    val signaling: SignalingClient,
    val signalRx: ReceiveChannel<SignalMessage>,
    val reflectorAddr: InetSocketAddress,
    val ownFingerprint: String,
)

/** Which method last got a connection to a given peer — see [CatalogSession]'s route-memory doc comment. */
enum class ConnectionRoute { DIRECT, PUNCH }

/**
 * `docs/PROTOCOL.md`'s route memory ("persist the last successful
 * candidate per peer and try it first next time — preference changes, the
 * trusted candidate set never widens"), scoped down to what actually costs
 * something to get wrong here: not *which* candidate within a punch
 * attempt (that negotiation is already fast — signaling round trips, not a
 * blocking timeout), but *whether to attempt the direct path at all*.
 * `connectToServer` blocks for a full `connectTimeout` (default 5s) before
 * failing when a peer's `peer_addr` isn't reachable, so remembering "punch
 * worked last time" and skipping straight to it is the one memory that's
 * actually worth the complexity right now.
 *
 * Deliberately in-memory only, not persisted to disk across app restarts
 * despite the protocol doc saying "persist" — this project has no existing
 * on-device storage pattern for this kind of cache yet (`TokenStore` is
 * for one secret, not a per-peer map), and the win that matters — not
 * re-trying a doomed direct connection *within one app session* while
 * browsing/switching back and forth between servers — is fully captured
 * without it. Revisit if cross-restart memory turns out to matter in
 * practice.
 */
private class RouteMemory {
    private val lastSuccessful = ConcurrentHashMap<String, ConnectionRoute>()
    fun record(deviceId: String, route: ConnectionRoute) {
        lastSuccessful[deviceId] = route
    }
    fun preferred(deviceId: String): ConnectionRoute? = lastSuccessful[deviceId]
    fun forget(deviceId: String) {
        lastSuccessful.remove(deviceId)
    }
}

/**
 * The base term — and the entire budget for a small library — of how long one
 * attempt at downloading a server's catalog body (delta or full manifest) may
 * take before it is abandoned.
 *
 * `PeerQuicClient.request` bounds the QUIC *handshake* (`connectTimeout`) but
 * puts no bound on reading a response body: a request that stalls partway —
 * the server briefly overloaded (all of a household's TVs cold-starting Browse
 * at once), a just-restarted server still building its catalog, a Fire TV
 * Wi-Fi radio still waking — otherwise hangs until [SwarmViewModel]'s whole
 * outer catalog budget expires, burning every retry [refresh] would have made
 * and surfacing as a hard "loading timed out" error even though an immediate
 * manual retry then succeeds (issue #100). Bounding each attempt turns that
 * stall into a fast per-attempt failure so [refresh]'s existing reconnect loop
 * actually gets to run.
 *
 * A flat 12s was too tight for a *cold* 10k+ entry manifest streamed and
 * JSON-decoded on a starved 32-bit Fire TV Stick — it timed out, dropped the
 * QUIC connection, retried, and hung the app (issue #223). The bound now
 * scales with the server-reported entry count: this base plus
 * [MANIFEST_FETCH_MS_PER_1K_ENTRIES] per thousand entries, clamped to
 * [MANIFEST_FETCH_MAX_TIMEOUT_MS] so it stays under the outer budget and a
 * genuine stall still converts to a fast failure.
 */
private const val MANIFEST_FETCH_TIMEOUT_MS = 12_000L

/** Added to [MANIFEST_FETCH_TIMEOUT_MS] for every 1,000 server-reported
 * catalog entries when sizing a cold catalog-body fetch. */
private const val MANIFEST_FETCH_MS_PER_1K_ENTRIES = 3_000L

/** Ceiling for the entry-count-scaled catalog-body bound. Kept below
 * `SwarmViewModel.CATALOG_LOAD_TIMEOUT_MS` (the whole-sequence backstop) so
 * one full attempt on the largest library still fits inside it. */
private const val MANIFEST_FETCH_MAX_TIMEOUT_MS = 33_000L

/** The `/catalog/thumbprint` response is a few bytes; only a dead connection
 * makes it slow, so it keeps a tight fixed bound of its own rather than
 * borrowing the size-scaled manifest budget. */
private const val THUMBPRINT_FETCH_TIMEOUT_MS = 10_000L

/** Entry-count-scaled bound for a cold catalog-body download: a flat 12s
 * could not stream and decode a 10k-entry manifest on a starved 32-bit Fire
 * TV Stick (#223). [MANIFEST_FETCH_TIMEOUT_MS] plus
 * [MANIFEST_FETCH_MS_PER_1K_ENTRIES] per thousand entries, clamped to
 * [MANIFEST_FETCH_MAX_TIMEOUT_MS]. */
internal fun scaledCatalogFetchTimeoutMs(entryCount: Long): Long {
    val scaled = MANIFEST_FETCH_TIMEOUT_MS +
        entryCount.coerceAtLeast(0) / 1_000 * MANIFEST_FETCH_MS_PER_1K_ENTRIES
    return scaled.coerceIn(MANIFEST_FETCH_TIMEOUT_MS, MANIFEST_FETCH_MAX_TIMEOUT_MS)
}

/** Server-held change requests return after 20 seconds when quiet. */
private const val CATALOG_CHANGE_FETCH_TIMEOUT_MS = 27_000L

/** The server allows a progressing foreground HLS startup up to 120 seconds.
 * Give it a small delivery margin, then force a reconnect rather than leave
 * the Fire TV parked on "Starting…" forever if the QUIC response stream is
 * lost without kwik surfacing EOF. */
private const val PLAYBACK_PREPARATION_TIMEOUT_MS = 135_000L

/** Preview preparation is intentionally capped at ten seconds by the media
 * server. A lost QUIC response must not inherit the much longer foreground
 * playback allowance and leave a browse card waiting for over two minutes. */
private const val PREVIEW_PREPARATION_TIMEOUT_MS = 15_000L

/** Cleanup sits directly in front of the next serialized browse preview.
 * Bound it so a lost `/stop` response cannot prevent every later preview
 * from starting. */
private const val PLAYBACK_STOP_TIMEOUT_MS = 5_000L

internal typealias DirectConnector = (
    SwarmDevice,
    X509Certificate,
    PrivateKey,
) -> PeerConnection?

class CatalogSession internal constructor(
    private val proxy: PeerLoopbackProxy,
    private val catalogCache: CatalogCache? = null,
    private val directConnector: DirectConnector,
    private val onConnectionRestored: (serverId: String) -> Unit = {},
    /** Non-null forces a fixed bound on every catalog fetch (tests); null
     * uses the tight thumbprint bound plus the entry-count-scaled body bound. */
    private val manifestFetchTimeoutMs: Long? = null,
    private val playbackPreparationTimeoutMs: Long = PLAYBACK_PREPARATION_TIMEOUT_MS,
    private val previewPreparationTimeoutMs: Long = PREVIEW_PREPARATION_TIMEOUT_MS,
    private val playbackStopTimeoutMs: Long = PLAYBACK_STOP_TIMEOUT_MS,
) : AutoCloseable {
    constructor(
        proxy: PeerLoopbackProxy,
        catalogCache: CatalogCache? = null,
        onConnectionRestored: (serverId: String) -> Unit = {},
    ) : this(
        proxy,
        catalogCache,
        { device, clientCertificate, clientKey ->
            connectToServer(device, clientCertificate, clientKey)
        },
        onConnectionRestored,
    )

    private val connections = ConcurrentHashMap<String, PeerConnection>()
    /** A transport failure removed these devices' raw connections. The next
     * successful handshake is a genuine reconnect, not an initial connect. */
    private val disconnectedDevices = ConcurrentHashMap.newKeySet<String>()
    /** Prevent an artwork burst and a simultaneous catalog refresh from
     * opening several replacement QUIC connections to the same server. */
    private val connectionLocks = ConcurrentHashMap<String, Mutex>()
    /** Decoded once per process; persistent storage supplies the cold-start copy. */
    private val manifests = ConcurrentHashMap<String, CatalogManifest>()
    private val routeMemory = RouteMemory()
    /** Which devices already have a [ReconnectingConnection] registered with [proxy] — see [connectionFor]'s doc comment on why that registration must happen at most once per device, never repeated on a reconnect. */
    private val proxyRegisteredDevices = ConcurrentHashMap.newKeySet<String>()

    /**
     * Set once a signaling session is available (typically right after
     * registration) to enable the hole-punch fallback; left null to stay
     * LAN-only (`peer_addr` direct connections work regardless).
     */
    var punchFallback: PunchFallback? = null

    /** One server whose catalog could not be loaded, with the transport or
     * decode failure retained for automatic reporting back to that server. */
    data class CatalogFailure(val device: SwarmDevice, val detail: String)

    data class Result(
        val entries: List<MergedEntry>,
        val unreachable: List<SwarmDevice>,
        val failures: List<CatalogFailure> = emptyList(),
    )
    data class ChangePoll(
        val entries: List<MergedEntry>? = null,
        val supported: Boolean = true,
    )
    data class PlaybackSelection(
        val url: String,
        val mode: PlaybackMode,
        val maxBitrate: Long,
        val sessionId: String,
        val lyrics: app.swarm.tv.core.peer.TrackLyrics? = null,
        val subtitles: List<app.swarm.tv.core.peer.SubtitleTrack> = emptyList(),
    )

    /**
     * Returns the last successfully downloaded catalogs without touching the
     * network. The app uses this before [refresh] so Browse can paint useful
     * content immediately while a fingerprint check happens in the
     * background.
     */
    suspend fun cachedEntries(
        devices: List<SwarmDevice>,
        clientCertificate: X509Certificate,
        clientKey: PrivateKey,
    ): List<MergedEntry> {
        val byServer = mutableMapOf<String, CatalogManifest>()
        for (device in devices.filter { it.deviceType != DeviceType.CLIENT }) {
            cachedManifest(device.deviceId)?.let {
                byServer[device.deviceId] = it
                // Register a lazy, reconnecting proxy route before cached
                // cards are painted. Artwork can then initiate the server
                // connection itself instead of racing refresh and receiving
                // an immediate 404 from an unregistered proxy route.
                ensureProxyRegistration(device, clientCertificate, clientKey)
            }
        }
        return CatalogMerger.merge(byServer)
    }

    /** The URL to hand a media player for `peerPath` on `serverId` — only live once that server appeared connected in a [refresh]. */
    fun urlFor(serverId: String, peerPath: String): String = proxy.urlFor(serverId, peerPath)

    /** Drop a cached indirect route so the next connection uses a newly discovered LAN address first. */
    fun preferDirect(deviceId: String) {
        connections.remove(deviceId)?.let(::closeConnection)
        routeMemory.record(deviceId, ConnectionRoute.DIRECT)
    }

    /** Close this TV's connection to one server without changing any other device's swarm membership. */
    fun disconnect(deviceId: String) {
        connections.remove(deviceId)?.let(::closeConnection)
        disconnectedDevices.remove(deviceId)
        connectionLocks.remove(deviceId)
        routeMemory.forget(deviceId)
        if (proxyRegisteredDevices.remove(deviceId)) proxy.unregister(deviceId)
    }

    /**
     * Reserve this server's shared upload budget and obtain either a paced
     * direct-play path or a capability-pruned HLS master path. This request
     * goes over the authenticated peer connection; the rendezvous server is
     * not involved in playback policy.
     */
    @Throws(IOException::class)
    suspend fun preparePlayback(
        device: SwarmDevice,
        entryKey: String,
        startPositionSecs: Long,
        clientCertificate: X509Certificate,
        clientKey: PrivateKey,
        capabilities: CapabilityProfile = CapabilityProfile.fireTvBaseline(),
        preview: Boolean = false,
    ): PlaybackSelection {
        val preferences = PlaybackPreferences(
            capabilities = capabilities,
            startPositionSecs = startPositionSecs,
            preferDirect = !preview,
            preview = preview,
        )
        var connection = connectionFor(device, clientCertificate, clientKey)
            ?: throw IOException("server is no longer connected")
        val response = try {
            requestPlayback(
                device.deviceId,
                connection,
                entryKey,
                preferences,
                if (preview) previewPreparationTimeoutMs else playbackPreparationTimeoutMs,
            )
        } catch (e: IOException) {
            // A cached connection can die between browsing and pressing play
            // — confirmed live: QUIC dropped it well under a minute after its
            // last request. Evict and retry once with a fresh connection,
            // same self-heal refresh()/fetchManifest already does for browse.
            // Only the raw connection is evicted here, never the proxy
            // registration itself — see ReconnectingConnection's doc comment.
            evictConnection(device.deviceId, connection)
            connection = connectionFor(device, clientCertificate, clientKey)
                ?: throw IOException("server is no longer connected")
            requestPlayback(
                device.deviceId,
                connection,
                entryKey,
                preferences,
                if (preview) previewPreparationTimeoutMs else playbackPreparationTimeoutMs,
            )
        }
        if (response.status != 200) {
            throw IOException("server could not prepare playback (${response.status}): ${response.body}")
        }
        val plan = SwarmJson.decodeFromString<PlaybackPlan>(response.body)
        return PlaybackSelection(
            proxy.urlFor(device.deviceId, plan.path),
            plan.mode,
            plan.maxBitrate,
            plan.sessionId,
            plan.lyrics,
            plan.subtitles.map { track -> track.copy(path = proxy.urlFor(device.deviceId, track.path)) },
        )
    }

    private data class PlaybackResponse(val status: Int, val body: String)

    /** Bounds both the response header and its small JSON body. Closing the
     * raw connection is what unblocks kwik's blocking InputStream when the
     * timeout/caller cancellation wins; the server observes that abandoned
     * stream and drops its cancellation-safe transcode reservation. */
    private suspend fun requestPlayback(
        serverId: String,
        connection: PeerConnection,
        entryKey: String,
        preferences: PlaybackPreferences,
        timeoutMs: Long,
    ): PlaybackResponse = coroutineScope {
        val request = async(Dispatchers.IO) {
            runCatching {
                val response = connection.request(path = "/play/$entryKey", playback = preferences)
                PlaybackResponse(
                    status = response.header.status,
                    body = response.body.use { it.readBytes().decodeToString() },
                )
            }
        }
        try {
            val outcome = withTimeoutOrNull(timeoutMs) { request.await() }
                ?: kotlin.Result.failure(
                    IOException(
                        "playback preparation stalled; aborted after ${timeoutMs}ms",
                    ),
                )
            if (outcome.isFailure) {
                request.cancel()
                evictConnection(serverId, connection)
            }
            outcome.getOrThrow()
        } finally {
            // Structured concurrency waits for blocking children even after
            // cancellation. Close first so kwik's parked read actually exits.
            if (!request.isCompleted) {
                request.cancel()
                evictConnection(serverId, connection)
            }
        }
    }

    /**
     * Releases [sessionId]'s bandwidth reservation on [device] — call once
     * the player screen for it is torn down (back-press, or moving on to
     * the next entry), so a same-device retry doesn't get rejected with
     * "not enough upload bandwidth" for the rest of the server's idle
     * timeout. Best-effort and silent on failure: this is cleanup on the
     * way out, and the worst case if it doesn't land (dead connection, no
     * reachable server) is exactly today's behavior — the reservation
     * still expires on its own, just later.
     */
    suspend fun stopPlayback(device: SwarmDevice, sessionId: String, clientCertificate: X509Certificate, clientKey: PrivateKey) {
        var connection = connectionFor(device, clientCertificate, clientKey) ?: return
        if (requestStop(device.deviceId, connection, sessionId)) return

        // A track can end after several minutes with no control requests in
        // between, leaving the cached QUIC connection stale. Cleanup matters
        // most precisely then, so reconnect and retry once instead of silently
        // leaving the old transcode reservation until its idle timeout.
        evictConnection(device.deviceId, connection)
        connection = connectionFor(device, clientCertificate, clientKey) ?: return
        requestStop(device.deviceId, connection, sessionId)
    }

    /** A stop is best-effort, but it must also be cancellation-safe: kwik's
     * body read is blocking, so closing the connection is what makes a timed
     * out cleanup return to the preview serializer. */
    private suspend fun requestStop(
        serverId: String,
        connection: PeerConnection,
        sessionId: String,
    ): Boolean = coroutineScope {
        val request = async(Dispatchers.IO) {
            runCatching {
                val response = connection.request(path = "/stop/$sessionId")
                response.body.use { it.readBytes() }
                response.header.status == 200
            }
        }
        try {
            val outcome = withTimeoutOrNull(playbackStopTimeoutMs) { request.await() }
                ?: kotlin.Result.failure(IOException("playback stop stalled"))
            if (outcome.isFailure) {
                request.cancel()
                evictConnection(serverId, connection)
            }
            outcome.getOrDefault(false)
        } finally {
            if (!request.isCompleted) {
                request.cancel()
                evictConnection(serverId, connection)
            }
        }
    }

    /**
     * Sends [report] to [device] for triage on that server's own swarm page
     * — see `swarm_core::peer::ClientErrorReport`'s doc comment for why this
     * rides the authenticated peer connection rather than a separate HTTP
     * call. Best-effort and silent on failure, same posture as
     * [stopPlayback]: a device too unreachable to accept an error report is
     * itself unremarkable (it's very possibly *why* the original error
     * happened), and this must never itself become a second point of
     * failure in an error-handling path. Returns whether the server accepted
     * the report so the app can retain a failed delivery and retry after the
     * next successful catalog connection.
     */
    suspend fun reportError(device: SwarmDevice, report: ClientErrorReport, clientCertificate: X509Certificate, clientKey: PrivateKey): Boolean {
        var connection = connectionFor(device, clientCertificate, clientKey) ?: return false
        repeat(2) { attempt ->
            val accepted = runCatching {
                val response = connection.request(path = "/errors/report", errorReport = report)
                response.body.readBytes()
                response.header.status == 204
            }.getOrDefault(false)
            if (accepted) return true
            evictConnection(device.deviceId, connection)
            if (attempt == 0) {
                connection = connectionFor(device, clientCertificate, clientKey) ?: return false
            }
        }
        return false
    }

    /** Fetches every resolved report the client has not dismissed on this
     * server. The app persists these locally before showing them. */
    suspend fun resolutionNotifications(
        device: SwarmDevice,
        clientDeviceId: String,
        clientCertificate: X509Certificate,
        clientKey: PrivateKey,
    ): List<ClientResolutionNotification> {
        var connection = connectionFor(device, clientCertificate, clientKey) ?: return emptyList()
        repeat(2) { attempt ->
            val notifications = runCatching {
                val response = connection.request(path = "/notifications/$clientDeviceId")
                val body = response.body.readBytes().decodeToString()
                if (response.header.status != 200) throw IOException("notification request failed (${response.header.status})")
                SwarmJson.decodeFromString<List<ClientResolutionNotification>>(body)
            }.getOrNull()
            if (notifications != null) return notifications
            evictConnection(device.deviceId, connection)
            if (attempt == 0) {
                connection = connectionFor(device, clientCertificate, clientKey) ?: return emptyList()
            }
        }
        return emptyList()
    }

    /** Best-effort remote acknowledgement; the local tombstone remains the
     * authoritative UI state if the server is unreachable during dismissal. */
    suspend fun dismissResolutionNotification(
        device: SwarmDevice,
        clientDeviceId: String,
        notificationId: Long,
        clientCertificate: X509Certificate,
        clientKey: PrivateKey,
    ) {
        val connection = connectionFor(device, clientCertificate, clientKey) ?: return
        runCatching {
            connection.request(path = "/notifications/$clientDeviceId/$notificationId/dismiss").body.readBytes()
        }
    }

    /**
     * Sends a like/unlike toggle to [device] — see
     * `swarm_core::peer::LikeToggle`'s doc comment. Best-effort and silent
     * on failure, same posture as [reportError]/[stopPlayback]: the local
     * liked-state (see [app.swarm.tv.app.data.AndroidLikedEntriesStore])
     * already updated optimistically before this is called, so a dropped
     * request just means the server-side aggregate count/dashboard listing
     * lags until the next successful toggle — never blocks or reverts the
     * client's own UI.
     */
    suspend fun toggleLike(device: SwarmDevice, like: LikeToggle, clientCertificate: X509Certificate, clientKey: PrivateKey) {
        val connection = connectionFor(device, clientCertificate, clientKey) ?: return
        runCatching { connection.request(path = "/likes/toggle", like = like) }
    }

    /** Advance a server-owned Buzz session on the authenticated device link. */
    @Throws(IOException::class)
    suspend fun buzz(
        device: SwarmDevice,
        request: BuzzRequest,
        clientCertificate: X509Certificate,
        clientKey: PrivateKey,
    ): BuzzResponse? = withContext(Dispatchers.IO) {
        var connection = connectionFor(device, clientCertificate, clientKey)
            ?: throw IOException("server is no longer connected")
        repeat(2) { attempt ->
            try {
                val json = SwarmJson.encodeToString(request)
                val payload = json.encodeToByteArray().joinToString("") { byte -> "%02x".format(byte) }
                val response = connection.request(path = "/buzz?payload=$payload")
                val body = response.body.use { it.readBytes().decodeToString() }
                if (response.header.status == 204) return@withContext null
                if (response.header.status != 200) throw IOException("Buzz request failed (${response.header.status})")
                return@withContext SwarmJson.decodeFromString<BuzzResponse>(body)
            } catch (error: IOException) {
                evictConnection(device.deviceId, connection)
                if (attempt == 0) connection = connectionFor(device, clientCertificate, clientKey) ?: throw error
                else throw error
            }
        }
        null
    }

    suspend fun refresh(devices: List<SwarmDevice>, clientCertificate: X509Certificate, clientKey: PrivateKey): Result {
        val manifestsByServer = mutableMapOf<String, CatalogManifest>()
        val unreachable = mutableListOf<SwarmDevice>()
        val failures = mutableListOf<CatalogFailure>()

        for (device in devices.filter { it.deviceType != DeviceType.CLIENT }) {
            val cached = cachedManifest(device.deviceId)
            var manifest: CatalogManifest? = null
            var failure: Throwable? = null
            // Retry both halves of catalog connection setup. Previously a
            // request failure retried after evicting its connection, while
            // an initial handshake failure did not. A TV whose first QUIC
            // handshake landed during the LAN-pairing handoff therefore
            // showed an error until the user selected the server again.
            //
            // Two back-to-back attempts (confirmed live, #47) still weren't
            // enough on a real device: a server that has *just* approved a
            // LAN pairing can flake on this TV's next couple of connection
            // attempts (route/ARP still settling for a peer this device has
            // never dialed before) faster than it recovers, so two attempts
            // fired with no gap between them can both land in that window.
            // A third attempt with a short backoff gives that a beat to
            // clear without materially slowing down the common case where
            // the very first attempt already succeeds.
            var attemptsRemaining = 3
            while (manifest == null && attemptsRemaining > 0) {
                attemptsRemaining -= 1
                if (attemptsRemaining < 2) delay(500)
                val connection = connectionFor(device, clientCertificate, clientKey)
                if (connection != null) {
                    val fetch = fetchCurrentManifest(device.deviceId, connection, cached)
                    manifest = fetch.getOrNull()
                    failure = fetch.exceptionOrNull() ?: failure
                }
            }
            if (manifest == null) {
                unreachable += device
                val detail = failure?.let { "${it.javaClass.simpleName}: ${it.message ?: "no detail"}" }
                    ?: "Could not establish a catalog connection."
                failures += CatalogFailure(device, detail)
                // A temporary network failure must not erase a library that
                // this TV has already browsed successfully. Keep the server
                // visibly marked unreachable while its stale catalog remains
                // usable for browsing; playback will reconnect independently.
                if (cached != null) manifestsByServer[device.deviceId] = cached
            } else {
                manifestsByServer[device.deviceId] = manifest
            }
        }

        return Result(CatalogMerger.merge(manifestsByServer), unreachable, failures)
    }

    /**
     * Waits on one server's authenticated change feed and atomically applies
     * its delta to the cached manifest. A quiet timeout returns no entries;
     * callers immediately open another poll without repainting the UI.
     */
    suspend fun pollChanges(
        device: SwarmDevice,
        activeDevices: List<SwarmDevice>,
        clientCertificate: X509Certificate,
        clientKey: PrivateKey,
    ): ChangePoll {
        val baseline = cachedManifest(device.deviceId) ?: return ChangePoll()
        val connection = connectionFor(device, clientCertificate, clientKey) ?: return ChangePoll()
        val outcome = coroutineScope {
            val fetch = async(Dispatchers.IO) {
                runCatching { fetchChangesBlocking(connection, baseline.thumbprint) }
            }
            val result = withTimeoutOrNull(CATALOG_CHANGE_FETCH_TIMEOUT_MS) { fetch.await() }
                ?: kotlin.Result.failure(IOException("catalog change feed stalled"))
            if (result.isFailure) {
                fetch.cancel()
                evictConnection(device.deviceId, connection)
            }
            result
        }.getOrElse { return ChangePoll() }

        if (outcome.unsupported) return ChangePoll(supported = false)
        val delta = outcome.manifest ?: return ChangePoll()

        // Applying the delta, re-serialising the whole manifest to disk, and
        // re-merging every server's catalog are all CPU/IO-bound and grow
        // with library size. This function is driven from a Main-dispatched
        // coroutine, so doing that work inline stalled the UI thread on a
        // large library — long enough to drop remote key presses and, on a
        // memory-pressured Fire TV, trip an input-dispatch ANR (#208). Run it
        // off the caller's dispatcher and hand back only the merged result.
        return withContext(Dispatchers.Default) {
            val current = manifests[device.deviceId] ?: baseline
            // Another refresh won the race. Re-poll from its newer version
            // instead of applying a delta based on an obsolete snapshot.
            if (current.thumbprint != baseline.thumbprint) return@withContext ChangePoll()
            val next = if (delta.reset) {
                delta.copy(reset = false)
            } else {
                val byKey = current.entries.associateByTo(linkedMapOf()) { it.entryKey }
                delta.removed.forEach(byKey::remove)
                delta.entries.forEach { byKey[it.entryKey] = it }
                CatalogManifest(delta.thumbprint, byKey.values.toList())
            }
            manifests[device.deviceId] = next
            runCatching { catalogCache?.store(device.deviceId, next) }.onFailure { it.printStackTrace() }
            val activeIds = activeDevices
                .filter { it.deviceType != DeviceType.CLIENT }
                .mapTo(hashSetOf()) { it.deviceId }
            ChangePoll(
                entries = CatalogMerger.merge(manifests.filterKeys { it in activeIds }),
            )
        }
    }

    private data class ChangeResponse(
        val manifest: CatalogManifest? = null,
        val unsupported: Boolean = false,
    )

    private fun fetchChangesBlocking(connection: PeerConnection, thumbprint: String): ChangeResponse {
        var response = connection.request("/catalog/changes.gz?since=$thumbprint")
        var compressed = response.header.status == 200
        if (response.header.status == 404) {
            response.body.close()
            response = connection.request("/catalog/changes?since=$thumbprint")
            compressed = false
        }
        return when (response.header.status) {
            200 -> ChangeResponse(decodeBody<CatalogManifest>(response, compressed))
            204 -> {
                response.body.close()
                ChangeResponse()
            }
            404 -> {
                response.body.close()
                ChangeResponse(unsupported = true)
            }
            else -> {
                response.body.close()
                throw IOException("catalog change feed returned ${response.header.status}")
            }
        }
    }

    /** Lightweight authenticated reachability check used by the dashboard's Resync action. */
    suspend fun probe(device: SwarmDevice, clientCertificate: X509Certificate, clientKey: PrivateKey): Boolean {
        repeat(2) {
            val connection = connectionFor(device, clientCertificate, clientKey) ?: return@repeat
            val reachable = runCatching {
                val response = connection.request("/catalog/thumbprint")
                response.body.readBytes()
                response.header.status == 200
            }.getOrDefault(false)
            if (reachable) return true
            evictConnection(device.deviceId, connection)
        }
        return false
    }

    /**
     * Resolves (connecting fresh if necessary) the raw [PeerQuicClient] for
     * [device] — used directly by [preparePlayback]/[stopPlayback]/
     * [reportError]/[refresh], each of which already has its own
     * deliberate, narrowly-scoped retry-once-on-failure logic for the one
     * request it's making. [ReconnectingConnection] is the *other* consumer
     * of this method: registered once with [proxy] the first time a device
     * connects, it calls back in here on every failure so anything routed
     * through the loopback proxy (chiefly artwork — many concurrent,
     * unattended fetches with no caller left to notice and retry) gets the
     * same self-healing without each of *those* call sites needing to know
     * anything about reconnection.
     */
    private suspend fun connectionFor(device: SwarmDevice, clientCertificate: X509Certificate, clientKey: PrivateKey): PeerConnection? =
        connectionLocks.computeIfAbsent(device.deviceId) { Mutex() }.withLock {
            connectionForLocked(device, clientCertificate, clientKey)
        }

    private suspend fun connectionForLocked(device: SwarmDevice, clientCertificate: X509Certificate, clientKey: PrivateKey): PeerConnection? {
        connections[device.deviceId]?.let { return it }

        // Failures here fail open (device just ends up in Result.unreachable)
        // by design, but that previously made a real bug — a NoSuchMethodError
        // from an API-33-only call — indistinguishable from an ordinary
        // network timeout. Logging beats a second silent-failure debugging session.
        suspend fun tryDirect(): PeerConnection? =
            runCatching { directConnector(device, clientCertificate, clientKey) }
                .onFailure { it.printStackTrace() }
                .getOrNull()
        suspend fun tryPunch(): PeerConnection? = punchFallback?.let { fallback ->
            runCatching {
                initiatePunchConnection(
                    signaling = fallback.signaling,
                    signalRx = fallback.signalRx,
                    reflectorAddr = fallback.reflectorAddr,
                    peerDeviceId = device.deviceId,
                    ownFingerprint = fallback.ownFingerprint,
                    clientCertificate = clientCertificate,
                    clientKey = clientKey,
                    expectedFingerprint = device.certFingerprint,
                )
            }.getOrNull()
        }

        // Route memory only ever changes which method is tried *first* —
        // the trusted fingerprint being dialed never changes, so this
        // can't widen trust, only save the wasted wait on a connectToServer
        // attempt already known to time out for this peer.
        val preferPunch = routeMemory.preferred(device.deviceId) == ConnectionRoute.PUNCH
        val (connection, route) = if (preferPunch) {
            tryPunch()?.let { it to ConnectionRoute.PUNCH } ?: (tryDirect()?.let { it to ConnectionRoute.DIRECT })
        } else {
            tryDirect()?.let { it to ConnectionRoute.DIRECT } ?: (tryPunch()?.let { it to ConnectionRoute.PUNCH })
        } ?: return null

        routeMemory.record(device.deviceId, route)
        connections[device.deviceId] = connection
        // Registered once, ever, per device — a reconnect after this
        // (this same branch, later, once `connections[device.deviceId]` has
        // been cleared by a failure elsewhere) must not register a second
        // wrapper on top of the still-live first one; the wrapper always
        // looks up the *current* raw connection dynamically at request
        // time, so it never goes stale on its own.
        ensureProxyRegistration(device, clientCertificate, clientKey)
        if (disconnectedDevices.remove(device.deviceId)) {
            // Observability must never turn a healthy replacement transport
            // into a failed request if an app-level listener misbehaves.
            runCatching { onConnectionRestored(device.deviceId) }
        }
        return connection
    }

    private fun ensureProxyRegistration(
        device: SwarmDevice,
        clientCertificate: X509Certificate,
        clientKey: PrivateKey,
    ) {
        if (proxyRegisteredDevices.add(device.deviceId)) {
            proxy.register(device.deviceId, ReconnectingConnection(device, clientCertificate, clientKey))
        }
    }

    /**
     * A [PeerConnection] that transparently reconnects through
     * [connectionFor] on failure instead of just failing — see
     * [connectionFor]'s doc comment for why this exists (artwork fetches
     * over the loopback proxy have no caller left to notice a dead
     * connection and retry the way [preparePlayback]/[refresh] do for
     * themselves). `runBlocking` is safe here: [PeerConnection.request] is
     * called from [PeerLoopbackProxy]'s own cached-thread-pool worker
     * threads, never the caller's main/UI thread.
     */
    private inner class ReconnectingConnection(
        private val device: SwarmDevice,
        private val clientCertificate: X509Certificate,
        private val clientKey: PrivateKey,
    ) : PeerConnection {
        override fun request(path: String, range: ByteRange?, ifNoneMatch: String?, playback: PlaybackPreferences?, errorReport: ClientErrorReport?, like: LikeToggle?): PeerResponse {
            val current = connections[device.deviceId]
                ?: connectionForBlocking()
                ?: throw IOException("server is no longer connected")
            return try {
                current.request(path, range, ifNoneMatch, playback, errorReport, like)
            } catch (e: IOException) {
                evictConnection(device.deviceId, current)
                val fresh = connectionForBlocking() ?: throw e
                fresh.request(path, range, ifNoneMatch, playback, errorReport, like)
            }
        }

        /**
         * `runBlocking` is interruptible. A closed QUIC connection can leave
         * the proxy worker interrupted just as it enters this reconnect; if
         * the resulting checked [InterruptedException] escapes an
         * [java.util.concurrent.ExecutorService] task, Android's executor
         * wraps it in [Error] and terminates the process. Preserve the
         * interrupt while normalizing it to the transport contract the
         * loopback proxy already contains as a recoverable 503.
         */
        private fun connectionForBlocking(): PeerConnection? = try {
            runBlocking { connectionFor(device, clientCertificate, clientKey) }
        } catch (error: InterruptedException) {
            Thread.currentThread().interrupt()
            throw IOException("server reconnect interrupted", error)
        }
    }

    private suspend fun cachedManifest(serverId: String): CatalogManifest? {
        manifests[serverId]?.let { return it }
        return runCatching { catalogCache?.load(serverId) }
            .onFailure { it.printStackTrace() }
            .getOrNull()
            ?.also { manifests[serverId] = it }
    }

    /**
     * Checks the tiny version response before requesting the large manifest.
     * An unchanged server therefore transfers only a few bytes on every
     * visit, while a changed/first-seen server still gets a fresh copy.
     *
     * Two independently bounded phases, each raced against a timer that on
     * expiry closes the raw connection to unblock the parked stream read and
     * report a failure [refresh]'s retry loop can recover from (issue #100):
     *
     *  1. `/catalog/thumbprint` — a few bytes, so a tight fixed
     *     [THUMBPRINT_FETCH_TIMEOUT_MS]. A match returns [cached] untouched.
     *  2. The catalog body — a cold browse whose only change is the server
     *     thumbprint moving (a flapping SMB share rescanning, #222/#223)
     *     folds the compact `/catalog/changes` delta onto [cached] rather
     *     than re-downloading the whole manifest; the full `/catalog/manifest`
     *     is the fallback for a first-ever browse, a server-history miss, or
     *     an older server. Bounded by an entry-count-scaled budget — see
     *     [MANIFEST_FETCH_TIMEOUT_MS]'s doc comment and issue #223.
     */
    private suspend fun fetchCurrentManifest(
        serverId: String,
        connection: PeerConnection,
        cached: CatalogManifest?,
    ): kotlin.Result<CatalogManifest> {
        val thumbprintTimeoutMs = manifestFetchTimeoutMs ?: THUMBPRINT_FETCH_TIMEOUT_MS
        val remote = racedFetch(
            serverId,
            connection,
            thumbprintTimeoutMs,
            "catalog thumbprint fetch stalled; aborted after ${thumbprintTimeoutMs}ms",
        ) { fetchThumbprintBlocking(connection) }
        remote.exceptionOrNull()?.let {
            it.printStackTrace()
            return kotlin.Result.failure(it)
        }
        val thumbprint = remote.getOrThrow()
        if (cached != null &&
            thumbprint.thumbprint == cached.thumbprint &&
            thumbprint.entryCount == cached.entries.size.toLong()
        ) {
            return kotlin.Result.success(cached)
        }

        val bodyTimeoutMs = manifestFetchTimeoutMs ?: scaledCatalogFetchTimeoutMs(thumbprint.entryCount)
        val result = racedFetch(
            serverId,
            connection,
            bodyTimeoutMs,
            "catalog manifest fetch stalled; aborted after ${bodyTimeoutMs}ms",
        ) { fetchCatalogBodyBlocking(serverId, connection, cached, thumbprint.thumbprint) }
        result.exceptionOrNull()?.printStackTrace()
        // Persisting the freshly downloaded copy is a suspend call, so it
        // can't live inside the blocking fetch above.
        result.getOrNull()?.let { fetched ->
            if (fetched.freshlyDownloaded) {
                runCatching { catalogCache?.store(serverId, fetched.manifest) }
                    .onFailure { it.printStackTrace() }
            }
        }
        return result.map { it.manifest }
    }

    /**
     * Runs [block] on [Dispatchers.IO] under a [timeoutMs] race. The block
     * swallows its own failure into a [kotlin.Result] so a stalled attempt
     * that only completes *after* we stop awaiting it can never propagate an
     * unconsumed exception out of this [coroutineScope]. On timeout or
     * failure the raw connection is evicted — which also closes it, making
     * any blocking stream read still parked inside [block] throw so the scope
     * can finish instead of leaking an IO thread until process exit. The
     * proxy registration itself stays — see [ReconnectingConnection].
     */
    private suspend fun <T> racedFetch(
        serverId: String,
        connection: PeerConnection,
        timeoutMs: Long,
        stallMessage: String,
        block: () -> T,
    ): kotlin.Result<T> = coroutineScope {
        val fetch = async(Dispatchers.IO) { runCatching { block() } }
        val outcome = withTimeoutOrNull(timeoutMs) { fetch.await() }
            ?: kotlin.Result.failure(IOException(stallMessage))
        if (outcome.isFailure) {
            fetch.cancel()
            evictConnection(serverId, connection)
        }
        outcome
    }

    /** A catalog manifest plus whether this refresh actually re-downloaded
     * it (vs. reusing the still-current cached copy after a thumbprint match). */
    private class FetchedManifest(val manifest: CatalogManifest, val freshlyDownloaded: Boolean)

    private fun fetchThumbprintBlocking(connection: PeerConnection): CatalogThumbprint {
        val response = connection.request("/catalog/thumbprint")
        if (response.header.status != 200) {
            response.body.close()
            throw IOException("catalog thumbprint returned ${response.header.status}")
        }
        return decodeBody<CatalogThumbprint>(response)
    }

    /** The blocking half of phase 2; throws on any transport or decode
     * failure. Runs on [Dispatchers.IO] under a timeout race. */
    private fun fetchCatalogBodyBlocking(
        serverId: String,
        connection: PeerConnection,
        cached: CatalogManifest?,
        remoteThumbprint: String,
    ): FetchedManifest {
        if (cached != null && cached.thumbprint != remoteThumbprint) {
            catalogDeltaBlocking(connection, cached)?.let { merged ->
                manifests[serverId] = merged
                return FetchedManifest(merged, freshlyDownloaded = true)
            }
        }

        var response = connection.request("/catalog/manifest.gz")
        var compressed = response.header.status == 200
        if (response.header.status == 404) {
            // Compatibility with media servers from before compressed
            // manifests were introduced.
            response.body.close()
            response = connection.request("/catalog/manifest")
            compressed = false
        }
        if (response.header.status != 200) {
            response.body.close()
            throw IOException("catalog manifest returned ${response.header.status}")
        }
        val manifest = decodeBody<CatalogManifest>(response, compressed)
        manifests[serverId] = manifest
        return FetchedManifest(manifest, freshlyDownloaded = true)
    }

    /**
     * The compact delta path for a cold browse: fold `/catalog/changes` onto
     * [cached] instead of re-downloading the whole manifest when the server
     * thumbprint has merely moved. Returns null — the caller then pulls the
     * full manifest — when the server has no change feed or the feed errors.
     * A reset response already carries a full snapshot, so it is used
     * directly rather than triggering a second download. The server answers
     * immediately here (its snapshot already differs from `since`), so the
     * long-poll wait never applies.
     */
    private fun catalogDeltaBlocking(
        connection: PeerConnection,
        cached: CatalogManifest,
    ): CatalogManifest? {
        val change = runCatching { fetchChangesBlocking(connection, cached.thumbprint) }
            .onFailure { it.printStackTrace() }
            .getOrNull() ?: return null
        if (change.unsupported) return null
        val delta = change.manifest ?: return null
        if (delta.reset) return delta.copy(reset = false)
        val byKey = cached.entries.associateByTo(linkedMapOf()) { it.entryKey }
        delta.removed.forEach(byKey::remove)
        delta.entries.forEach { byKey[it.entryKey] = it }
        return CatalogManifest(delta.thumbprint, byKey.values.toList())
    }

    /** Evict only the connection that actually failed. Another worker may
     * already have installed a healthy replacement for this server. */
    private fun evictConnection(serverId: String, failed: PeerConnection) {
        if (connections.remove(serverId, failed)) disconnectedDevices.add(serverId)
        closeConnection(failed)
    }

    private fun closeConnection(connection: PeerConnection) {
        (connection as? AutoCloseable)?.let { runCatching { it.close() } }
    }

    /** Decode directly from the bounded QUIC stream, avoiding byte[] and
     * String copies of a multi-megabyte catalog on memory-constrained TVs. */
    @OptIn(ExperimentalSerializationApi::class)
    private inline fun <reified T> decodeBody(response: PeerResponse, gzip: Boolean = false): T {
        val input = if (gzip) GZIPInputStream(response.body.buffered()) else response.body.buffered()
        return input.use { SwarmJson.decodeFromStream<T>(it) }
    }

    override fun close() {
        // Every device that ever got a ReconnectingConnection registered,
        // not just connections.keys — a device whose raw connection died
        // and was evicted (awaiting self-heal on next use) would otherwise
        // keep its wrapper registered with the proxy past this session's end.
        proxyRegisteredDevices.forEach(proxy::unregister)
        proxyRegisteredDevices.clear()
        connections.values.forEach(::closeConnection)
        connections.clear()
        disconnectedDevices.clear()
        connectionLocks.clear()
        manifests.clear()
    }
}
