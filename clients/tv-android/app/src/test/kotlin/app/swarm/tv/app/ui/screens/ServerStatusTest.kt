package app.swarm.tv.app.ui.screens

import app.swarm.tv.app.data.LanServer
import app.swarm.tv.core.rest.DeviceType
import app.swarm.tv.core.rest.SwarmDevice
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Test

class ServerStatusTest {
    private val saved = LanServer(
        serviceName = "SWARM Media Server abc",
        name = "Living Room",
        host = "192.168.0.10",
        peerPort = 8543,
        pairingPort = 8544,
        certFingerprint = "ab".repeat(32),
    )

    @Test
    fun `paired server remains visible as offline when discovery is lost`() {
        assertEquals(
            listOf(LanServerRowState(saved, online = false)),
            knownLanServerRows(discovered = emptyList(), paired = listOf(saved)),
        )
    }

    @Test
    fun `discovery marks paired server connected and supplies its current route`() {
        val discovered = saved.copy(host = "192.168.0.22", peerPort = 9553)

        assertEquals(
            listOf(LanServerRowState(discovered, online = true)),
            knownLanServerRows(discovered = listOf(discovered), paired = listOf(saved)),
        )
    }

    @Test
    fun `server picker omits offline historical roster entries`() {
        val active = swarmDevice("active", DeviceType.SERVER, online = true)
        val stale = swarmDevice("stale", DeviceType.SERVER, online = false)
        val dualRole = swarmDevice("dual", DeviceType.BOTH, online = true)
        val client = swarmDevice("client", DeviceType.CLIENT, online = true)

        assertEquals(
            listOf(active, dualRole),
            visibleSwarmServers(listOf(active, stale, dualRole, client)),
        )
    }

    @Test
    fun `server status uses user facing connection terms`() {
        assertEquals("connected", connectionStatusLabel(online = true, disconnected = false))
        assertEquals("offline", connectionStatusLabel(online = false, disconnected = false))
        assertEquals("disconnected", connectionStatusLabel(online = true, disconnected = true))
    }

    @Test
    fun `an unreachable SWARM service is reported as that, not as an empty swarm`() {
        val known = swarmDevice("SWARM Media Server", DeviceType.SERVER, online = false)

        assertEquals(
            "Can't reach the SWARM service right now. Servers found on this network are listed below.",
            swarmServersEmptyMessage(listOf(known), serviceUnreachable = true),
        )
        assertEquals(
            swarmServersEmptyMessage(emptyList(), serviceUnreachable = true),
            swarmServersEmptyMessage(listOf(known), serviceUnreachable = true),
        )
    }

    @Test
    fun `a known server that is disconnected from SWARM is named as offline`() {
        val server = swarmDevice("SWARM Media Server", DeviceType.SERVER, online = false)
        val phone = swarmDevice("Michael's Phone", DeviceType.CLIENT, online = false)

        assertEquals(
            "SWARM Media Server is offline. It isn't connected to SWARM right now.",
            swarmServersEmptyMessage(listOf(server, phone), serviceUnreachable = false),
        )
        assertEquals(
            "Den, Garage are offline. They aren't connected to SWARM right now.",
            swarmServersEmptyMessage(
                listOf(
                    swarmDevice("Den", DeviceType.SERVER, online = false),
                    swarmDevice("Garage", DeviceType.BOTH, online = false),
                ),
                serviceUnreachable = false,
            ),
        )
    }

    @Test
    fun `a swarm with no servers says nobody has joined`() {
        val phone = swarmDevice("Michael's Phone", DeviceType.CLIENT, online = true)

        assertEquals(
            "No media servers have joined this swarm yet.",
            swarmServersEmptyMessage(emptyList(), serviceUnreachable = false),
        )
        assertEquals(
            "No media servers have joined this swarm yet.",
            swarmServersEmptyMessage(listOf(phone), serviceUnreachable = false),
        )
    }

    @Test
    fun `a server that is only found on the network is available, not connected`() {
        // The reported state: paired, visible over mDNS, no session — it used to
        // read "connected" next to a greyed-out Browse button.
        assertEquals(LanServerStatus.AVAILABLE, lanServerStatus(found = true, inSession = false, disconnected = false))
        assertEquals("available", lanServerStatusLabel(LanServerStatus.AVAILABLE))

        assertEquals(LanServerStatus.CONNECTED, lanServerStatus(found = true, inSession = true, disconnected = false))
        assertEquals(LanServerStatus.OFFLINE, lanServerStatus(found = false, inSession = false, disconnected = false))
        assertEquals(LanServerStatus.DISCONNECTED, lanServerStatus(found = true, inSession = true, disconnected = true))
        assertEquals("connected", lanServerStatusLabel(LanServerStatus.CONNECTED))
        assertEquals("offline", lanServerStatusLabel(LanServerStatus.OFFLINE))
    }

    @Test
    fun `row text says what pressing the server will do`() {
        assertEquals(
            "Found on your network. Select to connect.",
            lanServerSubtitle(LanServerStatus.AVAILABLE, inSwarm = false, paired = true),
        )
        assertEquals(
            "Connected directly on your network",
            lanServerSubtitle(LanServerStatus.CONNECTED, inSwarm = true, paired = true),
        )
        assertEquals(
            "Not found on your network right now",
            lanServerSubtitle(LanServerStatus.OFFLINE, inSwarm = false, paired = true),
        )
        assertEquals(
            "Disconnected from this TV",
            lanServerSubtitle(LanServerStatus.DISCONNECTED, inSwarm = false, paired = true),
        )
        // A server this TV has never paired with still invites pairing.
        assertEquals(
            "Select to show an approval code on this TV",
            lanServerSubtitle(LanServerStatus.AVAILABLE, inSwarm = false, paired = false),
        )
    }

    @Test
    fun `browse opens the catalog when a server is connected`() {
        val rows = listOf(LanServerRowState(saved, online = true))
        assertEquals(BrowseAction.OpenCatalog, browseAction(true, rows, setOf(fp(saved)), emptySet()))
    }

    @Test
    fun `browse connects first when a paired server is found but there is no session`() {
        val rows = listOf(LanServerRowState(saved, online = true))
        assertEquals(
            BrowseAction.ConnectFirst(saved),
            browseAction(false, rows, setOf(fp(saved)), emptySet()),
        )
    }

    @Test
    fun `browse stays unavailable when there is nothing to connect to`() {
        val paired = setOf(fp(saved))
        // Paired, but not currently found.
        assertEquals(
            BrowseAction.Unavailable,
            browseAction(false, listOf(LanServerRowState(saved, online = false)), paired, emptySet()),
        )
        // Found, but this TV has never paired with it.
        assertEquals(
            BrowseAction.Unavailable,
            browseAction(false, listOf(LanServerRowState(saved, online = true)), emptySet(), emptySet()),
        )
        // Found and paired, but the person disconnected it on purpose.
        assertEquals(
            BrowseAction.Unavailable,
            browseAction(false, listOf(LanServerRowState(saved, online = true)), paired, paired),
        )
        assertEquals(BrowseAction.Unavailable, browseAction(false, emptyList(), paired, emptySet()))
    }

    @Test
    fun `browse skips a server it cannot use and picks one it can`() {
        val other = saved.copy(name = "Garage", certFingerprint = "cd".repeat(32), host = "192.168.0.11")
        val rows = listOf(
            LanServerRowState(saved, online = false),
            LanServerRowState(other, online = true),
        )
        assertEquals(
            BrowseAction.ConnectFirst(other),
            browseAction(false, rows, setOf(fp(saved), fp(other)), emptySet()),
        )
    }

    private fun fp(server: LanServer) = server.certFingerprint.trim().lowercase()

    private fun swarmDevice(id: String, type: DeviceType, online: Boolean) = SwarmDevice(
        deviceId = id,
        name = id,
        deviceType = type,
        certFingerprint = id.padEnd(64, '0'),
        online = online,
    )
}
