package app.swarm.tv.app.data

import android.content.Context
import android.net.nsd.NsdManager
import android.net.nsd.NsdServiceInfo
import android.net.wifi.WifiManager
import android.os.Handler
import android.os.Looper
import app.swarm.tv.core.rest.DeviceType
import app.swarm.tv.core.rest.SwarmDevice
import app.swarm.tv.core.rest.SwarmJson
import java.io.Closeable
import java.net.InetSocketAddress
import java.net.Socket
import java.util.ArrayDeque
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.withContext
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.decodeFromString
import kotlinx.serialization.encodeToString

/** A media server resolved from SWARM's DNS-SD advertisement. */
data class LanServer(
    val serviceName: String,
    val name: String,
    val host: String,
    val peerPort: Int,
    val pairingPort: Int,
    val certFingerprint: String,
) {
    fun asSwarmDevice(): SwarmDevice {
        val address = if (host.contains(':')) "[$host]:$peerPort" else "$host:$peerPort"
        return SwarmDevice(
            deviceId = "lan-${certFingerprint.take(16)}",
            name = name,
            deviceType = DeviceType.SERVER,
            certFingerprint = certFingerprint,
            online = true,
            metadata = mapOf("peer_addr" to address, "hostname" to host),
        )
    }
}

/** Short-lived LAN approval shown on the TV and privately polled afterward. */
data class LanPairingActivation(
    val serverFingerprint: String,
    val code: String,
    val activationId: String,
    val pollToken: String,
    val expiresInSeconds: Long,
    val status: String = "pending",
)

/**
 * Re-resolving a known certificate is an address refresh, not a new pairing.
 * The certificate fingerprint is the server's stable security identity; DHCP
 * addresses and ports are only routing data and may legitimately change.
 */
internal fun preferDiscoveredLanServer(saved: LanServer, discovered: List<LanServer>): LanServer =
    discovered.firstOrNull {
        it.certFingerprint.trim().lowercase() == saved.certFingerprint.trim().lowercase()
    } ?: saved

/**
 * Discovers `_swarm-peer._udp` services using Android's native DNS-SD API.
 * A multicast lock is held only while discovery is active; some Fire TV and
 * Android TV Wi-Fi drivers otherwise filter mDNS packets before NSD sees them.
 */
class LanDiscoveryManager(context: Context) : Closeable {
    companion object {
        private const val SERVICE_TYPE = "_swarm-peer._udp."
    }

    private val nsd = context.getSystemService(Context.NSD_SERVICE) as NsdManager
    private val wifi = context.applicationContext.getSystemService(Context.WIFI_SERVICE) as? WifiManager
    private val mainHandler = Handler(Looper.getMainLooper())
    private val resolved = linkedMapOf<String, LanServer>()
    private val resolveQueue = ArrayDeque<NsdServiceInfo>()
    private var resolving = false
    private var started = false
    private var multicastLock: WifiManager.MulticastLock? = null

    private val _servers = MutableStateFlow<List<LanServer>>(emptyList())
    val servers: StateFlow<List<LanServer>> = _servers.asStateFlow()

    private val discoveryListener = object : NsdManager.DiscoveryListener {
        override fun onDiscoveryStarted(serviceType: String) = Unit

        override fun onServiceFound(serviceInfo: NsdServiceInfo) {
            if (serviceInfo.serviceType.startsWith(SERVICE_TYPE)) {
                resolveQueue.addLast(serviceInfo)
                resolveNext()
            }
        }

        override fun onServiceLost(serviceInfo: NsdServiceInfo) {
            resolved.remove(serviceInfo.serviceName)
            publish()
        }

        override fun onDiscoveryStopped(serviceType: String) = Unit
        override fun onStartDiscoveryFailed(serviceType: String, errorCode: Int) = stopAfterFailure()
        override fun onStopDiscoveryFailed(serviceType: String, errorCode: Int) = stopAfterFailure()
    }

    fun start() {
        mainHandler.post {
            if (started) return@post
            started = true
            multicastLock = wifi?.createMulticastLock("swarm-lan-discovery")?.apply {
                setReferenceCounted(false)
                acquire()
            }
            runCatching { nsd.discoverServices(SERVICE_TYPE, NsdManager.PROTOCOL_DNS_SD, discoveryListener) }
                .onFailure { stopAfterFailure() }
        }
    }

    @Suppress("DEPRECATION")
    private fun resolveNext() {
        if (resolving || resolveQueue.isEmpty()) return
        resolving = true
        val service = resolveQueue.removeFirst()
        nsd.resolveService(service, object : NsdManager.ResolveListener {
            override fun onResolveFailed(serviceInfo: NsdServiceInfo, errorCode: Int) = finishResolve()

            override fun onServiceResolved(serviceInfo: NsdServiceInfo) {
                parse(serviceInfo)?.let { resolved[serviceInfo.serviceName] = it }
                publish()
                finishResolve()
            }
        })
    }

    private fun finishResolve() {
        resolving = false
        resolveNext()
    }

    private fun parse(info: NsdServiceInfo): LanServer? {
        fun attribute(name: String): String? = info.attributes[name]?.toString(Charsets.UTF_8)
        val fingerprint = attribute("fingerprint")?.lowercase() ?: return null
        if (fingerprint.length != 64 || fingerprint.any { !it.isDigit() && it !in 'a'..'f' }) return null
        val host = info.host?.hostAddress ?: return null
        val peerPort = attribute("peer_port")?.toIntOrNull() ?: info.port
        val pairingPort = attribute("pair_port")?.toIntOrNull() ?: return null
        if (peerPort !in 1..65535 || pairingPort !in 1..65535) return null
        return LanServer(
            serviceName = info.serviceName,
            name = attribute("name")?.takeIf(String::isNotBlank) ?: info.serviceName,
            host = host,
            peerPort = peerPort,
            pairingPort = pairingPort,
            certFingerprint = fingerprint,
        )
    }

    private fun publish() {
        _servers.value = resolved.values.sortedBy { it.name.lowercase() }
    }

    /**
     * Unicast equivalent of resolving an mDNS record: asks the pairing port
     * at [host]:[port] for the `info` action and builds the same [LanServer]
     * discovery would have produced. Used by manual IP entry when multicast
     * announcements can't be seen on the LAN.
     */
    suspend fun fetchServerInfo(host: String, port: Int): Result<LanServer> =
        withContext(Dispatchers.IO) {
            runCatching {
                val response = exchangeInfo(host, port)
                if (!response.ok) {
                    error(response.error ?: "The media server refused the request.")
                }
                val fingerprint = response.serverFingerprint?.lowercase()
                if (fingerprint == null || fingerprint.length != 64 ||
                    fingerprint.any { !it.isDigit() && it !in 'a'..'f' }
                ) {
                    error("That address did not answer like a SWARM media server.")
                }
                val peerPort = response.peerPort ?: port
                val pairingPort = response.pairingPort ?: port
                if (peerPort !in 1..65535 || pairingPort !in 1..65535) {
                    error("That address did not answer like a SWARM media server.")
                }
                LanServer(
                    serviceName = "manual:$host:$port",
                    name = response.serverName?.takeIf { it.isNotBlank() } ?: host,
                    host = host,
                    peerPort = peerPort,
                    pairingPort = pairingPort,
                    certFingerprint = fingerprint,
                )
            }
        }

    private fun exchangeInfo(host: String, port: Int): LanPairResponse =
        Socket().use { socket ->
            socket.connect(InetSocketAddress(host, port), 5_000)
            socket.soTimeout = 5_000
            val writer = socket.getOutputStream().bufferedWriter()
            writer.write(SwarmJson.encodeToString(LanPairRequest(action = "info")))
            writer.newLine()
            writer.flush()
            val line = socket.getInputStream().bufferedReader().readLine()
                ?: error("The media server closed the connection.")
            SwarmJson.decodeFromString(line)
        }

    private fun stopAfterFailure() {
        started = false
        resolving = false
        resolveQueue.clear()
        multicastLock?.takeIf { it.isHeld }?.release()
        multicastLock = null
    }

    override fun close() {
        mainHandler.post {
            if (started) runCatching { nsd.stopServiceDiscovery(discoveryListener) }
            stopAfterFailure()
            resolved.clear()
            publish()
        }
    }

    suspend fun beginPairing(
        server: LanServer,
        deviceName: String,
        fingerprint: String,
    ): Result<LanPairingActivation> = beginPairing(
        server = server,
        deviceName = deviceName,
        fingerprint = fingerprint,
        action = "begin",
        testingToken = null,
    )

    suspend fun beginTestingPairing(
        server: LanServer,
        deviceName: String,
        fingerprint: String,
        testingToken: String?,
    ): Result<LanPairingActivation> = beginPairing(
        server = server,
        deviceName = deviceName,
        fingerprint = fingerprint,
        action = "begin_testing",
        testingToken = testingToken,
    )

    private suspend fun beginPairing(
        server: LanServer,
        deviceName: String,
        fingerprint: String,
        action: String,
        testingToken: String?,
    ): Result<LanPairingActivation> =
        withContext(Dispatchers.IO) {
            runCatching {
                val response = exchange(
                    server,
                    LanPairRequest(
                        action = action,
                        name = deviceName,
                        fingerprint = fingerprint,
                        testingToken = testingToken,
                    ),
                )
                requireSuccessful(response)
                LanPairingActivation(
                    serverFingerprint = server.certFingerprint,
                    code = response.code ?: error("The media server did not return an activation code."),
                    activationId = response.activationId ?: error("The media server returned an incomplete activation."),
                    pollToken = response.pollToken ?: error("The media server returned an incomplete activation."),
                    expiresInSeconds = response.expiresInSeconds ?: 300,
                    status = response.status ?: "pending",
                )
            }
        }

    suspend fun pollPairing(server: LanServer, activation: LanPairingActivation): Result<String> =
        withContext(Dispatchers.IO) {
            runCatching {
                val response = exchange(
                    server,
                    LanPairRequest(
                        action = "poll",
                        activationId = activation.activationId,
                        pollToken = activation.pollToken,
                    ),
                )
                requireSuccessful(response)
                response.status ?: error("The media server returned no activation status.")
            }
        }

    suspend fun endTestingPairing(server: LanServer, activation: LanPairingActivation): Result<Unit> =
        withContext(Dispatchers.IO) {
            runCatching {
                val response = exchange(
                    server,
                    LanPairRequest(
                        action = "end_testing",
                        activationId = activation.activationId,
                        pollToken = activation.pollToken,
                    ),
                )
                requireSuccessful(response)
                check(response.status == "ended") { "The media server did not end testing authorization." }
            }
        }

    private fun exchange(server: LanServer, request: LanPairRequest): LanPairResponse =
        Socket().use { socket ->
            socket.connect(InetSocketAddress(server.host, server.pairingPort), 5_000)
            socket.soTimeout = 5_000
            val writer = socket.getOutputStream().bufferedWriter()
            writer.write(SwarmJson.encodeToString(request))
            writer.newLine()
            writer.flush()
            val line = socket.getInputStream().bufferedReader().readLine()
                ?: error("The media server closed the activation connection.")
            SwarmJson.decodeFromString(line)
        }

    private fun requireSuccessful(response: LanPairResponse) {
        if (response.ok) return
        val message = when (response.error) {
            "not_lan" -> "The media server did not recognize this as a local-network connection."
            "too_many_pending_activations" -> "The media server has too many pending TV approvals. Try again shortly."
            "testing_unavailable" -> "That media server does not support debug testing mode."
            "invalid_testing_activation" -> "The media server no longer recognizes this testing session."
            else -> "The media server rejected the LAN activation request."
        }
        error(message)
    }
}

@Serializable
private data class LanPairRequest(
    val action: String,
    val name: String? = null,
    val fingerprint: String? = null,
    @SerialName("activation_id") val activationId: String? = null,
    @SerialName("poll_token") val pollToken: String? = null,
    @SerialName("testing_token") val testingToken: String? = null,
)

@Serializable
private data class LanPairResponse(
    val ok: Boolean,
    val error: String? = null,
    val code: String? = null,
    @SerialName("activation_id") val activationId: String? = null,
    @SerialName("poll_token") val pollToken: String? = null,
    @SerialName("expires_in_seconds") val expiresInSeconds: Long? = null,
    val status: String? = null,
    // Present only on the `info` action: the same fields the mDNS TXT record
    // carries. All optional so old servers (which lack the action) still
    // decode; `ignoreUnknownKeys` covers the reverse direction.
    @SerialName("server_name") val serverName: String? = null,
    @SerialName("server_fingerprint") val serverFingerprint: String? = null,
    @SerialName("peer_port") val peerPort: Int? = null,
    @SerialName("pair_port") val pairingPort: Int? = null,
    @SerialName("http_media_port") val httpMediaPort: Int? = null,
)

/**
 * Parses manual server entry: "192.168.0.126" or "192.168.0.126:8543" (a
 * scheme prefix or trailing path is tolerated and stripped). Returns null
 * when no usable host remains. The port defaults to the server's default
 * peer/pairing port (gui.rs binds 0.0.0.0:8543 unless SWARM_PEER_BIND says
 * otherwise); the `info` reply then reports the authoritative ports, so this
 * default is only the first knock.
 */
internal fun parseManualServerInput(input: String): Pair<String, Int>? {
    val defaultPort = 8543
    var text = input.trim()
    if (text.isEmpty()) return null
    text = text.substringAfter("://", text).substringBefore("/").trim()
    if (text.isEmpty()) return null
    // "[v6]" or "[v6]:port".
    if (text.startsWith("[")) {
        val close = text.indexOf(']')
        if (close < 0) return null
        val host = text.substring(1, close).trim()
        val rest = text.substring(close + 1).trim()
        val port = if (rest.startsWith(":")) {
            rest.drop(1).toIntOrNull() ?: return null
        } else if (rest.isEmpty()) {
            defaultPort
        } else {
            return null
        }
        if (host.isEmpty() || port !in 1..65535) return null
        return host to port
    }
    // "host", "host:port", or a bare IPv6 literal (no port).
    val firstColon = text.indexOf(':')
    val lastColon = text.lastIndexOf(':')
    if (firstColon >= 0 && firstColon != lastColon) {
        // More than one colon: a bare IPv6 literal, port stays default.
        return text to defaultPort
    }
    if (lastColon < 0) return text to defaultPort
    val host = text.substring(0, lastColon).trim()
    val port = text.substring(lastColon + 1).trim().toIntOrNull() ?: return null
    if (host.isEmpty() || port !in 1..65535) return null
    return host to port
}
