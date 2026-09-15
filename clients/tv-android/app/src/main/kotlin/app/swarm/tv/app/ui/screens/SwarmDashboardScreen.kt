/** The TV client's SWARM home screen and its LAN-first server controls. */
package app.swarm.tv.app.ui.screens

import android.app.Activity
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusProperties
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.tv.material3.Button
import androidx.tv.material3.Border
import androidx.tv.material3.Card
import androidx.tv.material3.CardDefaults
import app.swarm.tv.app.data.CONNECTION_SETUP_SWARM_ID
import app.swarm.tv.app.data.LanPairingActivation
import app.swarm.tv.app.data.LanServer
import app.swarm.tv.app.ui.UatTestTags
import app.swarm.tv.app.ui.components.swarmActionButtonColors
import app.swarm.tv.app.ui.theme.SwarmAccent
import app.swarm.tv.app.ui.theme.SwarmAccentHot
import app.swarm.tv.app.ui.theme.SwarmError
import app.swarm.tv.app.ui.theme.SwarmGreen
import app.swarm.tv.app.ui.theme.SwarmMuted
import app.swarm.tv.app.ui.theme.SwarmBorder
import app.swarm.tv.app.ui.theme.SwarmSurface
import app.swarm.tv.app.ui.theme.SwarmSurfaceMuted
import app.swarm.tv.app.ui.theme.SwarmText
import app.swarm.tv.core.rest.DeviceType
import app.swarm.tv.core.rest.SwarmDevice
import app.swarm.tv.core.rest.SwarmSummary

@Composable
fun SwarmDashboardScreen(
    swarm: SwarmSummary,
    devices: List<SwarmDevice>,
    lanServers: List<LanServer>,
    pairedLanServers: List<LanServer>,
    pairedLanFingerprints: Set<String>,
    disconnectedServerFingerprints: Set<String>,
    lanPairingBusy: Boolean,
    lanPairingActivation: LanPairingActivation?,
    lanError: String?,
    deviceName: String,
    joiningServer: Boolean,
    joinServerError: String?,
    onBrowseCatalog: () -> Unit,
    onOpenSettings: () -> Unit,
    onAddServer: () -> Unit,
    onConnectLan: (LanServer, String) -> Unit,
    onStartLanPairing: (LanServer, String) -> Unit,
    onCancelLanPairing: () -> Unit,
    onDisconnectServer: (SwarmDevice) -> Unit,
    onReconnectServer: (SwarmDevice) -> Unit,
    onDisconnectLanServer: (LanServer) -> Unit,
    onReconnectLanServer: (LanServer) -> Unit,
    onForgetLanServer: (LanServer) -> Unit,
    onBackToBrowse: () -> Unit,
) {
    fun normalized(value: String) = value.trim().lowercase()

    val initialActionFocusRequester = remember { FocusRequester() }
    val firstServerFocusRequester = remember { FocusRequester() }
    var showExitConfirm by remember { mutableStateOf(false) }
    var selectedSwarmServer by remember { mutableStateOf<SwarmDevice?>(null) }
    var selectedLanServer by remember { mutableStateOf<LanServer?>(null) }
    var selectedKnownLanServer by remember { mutableStateOf<LanServer?>(null) }
    var showAddServer by remember { mutableStateOf(false) }
    val activity = LocalContext.current as? Activity
    val modalOpen = showExitConfirm || selectedSwarmServer != null || selectedLanServer != null ||
        selectedKnownLanServer != null || showAddServer
    val isConnectionSetup = swarm.id == CONNECTION_SETUP_SWARM_ID

    val serversInSwarm = if (swarm.id == "lan") emptyList() else visibleSwarmServers(devices)
    val lanFingerprints = lanServers.mapTo(mutableSetOf()) { normalized(it.certFingerprint) }
    val swarmFingerprints = serversInSwarm.mapTo(mutableSetOf()) { normalized(it.certFingerprint) }
    val paired = pairedLanFingerprints.mapTo(mutableSetOf(), ::normalized)
    val disconnected = disconnectedServerFingerprints.mapTo(mutableSetOf(), ::normalized)
    val lanServerRows = knownLanServerRows(lanServers, pairedLanServers)
    val hasConnectedServer = devices.any {
        (it.deviceType == DeviceType.SERVER || it.deviceType == DeviceType.BOTH) &&
            it.online &&
            normalized(it.certFingerprint) !in disconnected
    }
    val hasServerRows = serversInSwarm.isNotEmpty() || lanServerRows.isNotEmpty()
    val downToFirstServer = if (hasServerRows) {
        Modifier.focusProperties { down = firstServerFocusRequester }
    } else {
        Modifier
    }

    LaunchedEffect(modalOpen) {
        if (!modalOpen) initialActionFocusRequester.requestFocus()
    }
    LaunchedEffect(pairedLanFingerprints, selectedLanServer?.certFingerprint) {
        val selected = selectedLanServer ?: return@LaunchedEffect
        if (normalized(selected.certFingerprint) in paired) {
            selectedLanServer = null
        }
    }
    LaunchedEffect(lanServerRows, pairedLanFingerprints, selectedKnownLanServer?.certFingerprint) {
        val selected = selectedKnownLanServer ?: return@LaunchedEffect
        val fp = normalized(selected.certFingerprint)
        val stillKnown = fp in paired || lanServerRows.any { normalized(it.server.certFingerprint) == fp }
        if (!stillKnown) selectedKnownLanServer = null
    }
    BackHandler(enabled = !modalOpen) {
        if (hasConnectedServer) onBackToBrowse() else showExitConfirm = true
    }

    Box(modifier = Modifier.fillMaxSize()) {
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(40.dp)
                .focusProperties { canFocus = !modalOpen },
        ) {
            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.SpaceBetween,
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Column {
                    Text("SWARM", color = SwarmAccent, fontSize = 22.sp, fontWeight = FontWeight.Black)
                    Text(swarm.name, color = SwarmText, fontSize = 16.sp)
                }
                Row(horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                    // Sits next to "Browse library" on the main SWARM page for
                    // every connection kind (including a LAN-only session) —
                    // this is the one entry point to the STUN activation flow
                    // (#158).
                    Button(
                        onClick = {
                            showAddServer = true
                        },
                        modifier = downToFirstServer
                            .then(
                                if (isConnectionSetup) Modifier.focusRequester(initialActionFocusRequester) else Modifier,
                            )
                            .testTag(UatTestTags.DASHBOARD_ADD_SERVER_BUTTON),
                        colors = swarmActionButtonColors(),
                    ) { Text("Add Server") }
                    Button(
                        onClick = onBrowseCatalog,
                        enabled = hasConnectedServer,
                        modifier = downToFirstServer.then(
                            if (!isConnectionSetup) Modifier.focusRequester(initialActionFocusRequester) else Modifier,
                        ).testTag(UatTestTags.DASHBOARD_BROWSE_BUTTON),
                        colors = swarmActionButtonColors(),
                    ) { Text("Browse library") }
                    Button(
                        onClick = onOpenSettings,
                        modifier = downToFirstServer.testTag(UatTestTags.DASHBOARD_SETTINGS_BUTTON),
                        colors = swarmActionButtonColors(),
                    ) { Text("Settings") }
                }
            }
            Spacer(Modifier.height(24.dp))

            if (isConnectionSetup) {
                Text(
                    "Connect through SWARM for access away from home, or select a media server found on this network.",
                    color = SwarmMuted,
                    fontSize = 14.sp,
                )
                Spacer(Modifier.height(18.dp))
            }
            joinServerError?.let {
                Text(it, color = SwarmError, fontSize = 12.sp)
                Spacer(Modifier.height(12.dp))
            }

            LazyColumn(
                modifier = Modifier.weight(1f),
                verticalArrangement = Arrangement.spacedBy(10.dp),
            ) {
                if (!isConnectionSetup) {
                    item {
                        Text("Servers in this swarm (${serversInSwarm.size})", color = SwarmMuted, fontSize = 14.sp)
                    }
                    if (serversInSwarm.isEmpty()) {
                        item {
                            Text(
                                "No media servers have joined this swarm yet.",
                                color = SwarmMuted,
                                fontSize = 14.sp,
                            )
                        }
                    } else {
                        itemsIndexed(serversInSwarm, key = { _, server -> "swarm-${server.deviceId}" }) { index, server ->
                            val fingerprint = normalized(server.certFingerprint)
                            ServerRow(
                                modifier = if (index == 0) Modifier.focusRequester(firstServerFocusRequester) else Modifier,
                                device = server,
                                onLan = fingerprint in lanFingerprints,
                                disconnected = fingerprint in disconnected,
                                onClick = { selectedSwarmServer = server },
                            )
                        }
                    }
                    item { Spacer(Modifier.height(14.dp)) }
                }
                item {
                    Text("Servers on LAN (${lanServerRows.size})", color = SwarmMuted, fontSize = 14.sp)
                }
                if (lanError != null && selectedLanServer == null) {
                    item { Text(lanError, color = SwarmError, fontSize = 12.sp) }
                }
                if (lanServerRows.isEmpty()) {
                    item {
                        Text(
                            "No media servers found on this network.",
                            color = SwarmMuted,
                            fontSize = 14.sp,
                        )
                    }
                } else {
                    itemsIndexed(lanServerRows, key = { _, row -> "lan-${row.server.certFingerprint}" }) { index, row ->
                        val server = row.server
                        val fingerprint = normalized(server.certFingerprint)
                        val inSwarm = fingerprint in swarmFingerprints
                        val isPaired = fingerprint in paired
                        val isDisconnected = fingerprint in disconnected
                        LanServerRow(
                            modifier = if (serversInSwarm.isEmpty() && index == 0) {
                                Modifier.focusRequester(firstServerFocusRequester)
                            } else {
                                Modifier
                            },
                            server = server,
                            inSwarm = inSwarm,
                            paired = isPaired,
                            disconnected = isDisconnected,
                            online = row.online,
                            busy = lanPairingBusy,
                            onClick = {
                                if (inSwarm || isPaired) {
                                    selectedKnownLanServer = server
                                } else {
                                    selectedLanServer = server
                                    onStartLanPairing(server, deviceName)
                                }
                            },
                        )
                    }
                }
            }
        }

        selectedSwarmServer?.let { server ->
            val isDisconnected = normalized(server.certFingerprint) in disconnected
            ServerConnectionOverlay(
                server = server,
                disconnected = isDisconnected,
                onConfirm = {
                    if (isDisconnected) onReconnectServer(server) else onDisconnectServer(server)
                    selectedSwarmServer = null
                },
                onDismiss = { selectedSwarmServer = null },
            )
        }

        selectedLanServer?.let { server ->
            LanPairingOverlay(
                server = server,
                activation = lanPairingActivation?.takeIf {
                    normalized(it.serverFingerprint) == normalized(server.certFingerprint)
                },
                busy = lanPairingBusy,
                error = lanError,
                onRetry = { onStartLanPairing(server, deviceName) },
                onDismiss = {
                    onCancelLanPairing()
                    selectedLanServer = null
                },
            )
        }

        selectedKnownLanServer?.let { server ->
            val isDisconnected = normalized(server.certFingerprint) in disconnected
            val isPaired = normalized(server.certFingerprint) in paired
            LanServerActionOverlay(
                server = server,
                disconnected = isDisconnected,
                paired = isPaired,
                onConnect = {
                    if (isDisconnected) onReconnectLanServer(server) else onConnectLan(server, deviceName)
                    selectedKnownLanServer = null
                },
                onDisconnect = {
                    onDisconnectLanServer(server)
                    selectedKnownLanServer = null
                },
                onForget = {
                    onForgetLanServer(server)
                    selectedKnownLanServer = null
                },
                onDismiss = { selectedKnownLanServer = null },
            )
        }

        if (showAddServer) {
            AddServerOverlay(
                busy = joiningServer,
                onAdd = {
                    showAddServer = false
                    onAddServer()
                },
                onDismiss = {
                    showAddServer = false
                },
            )
        }

        if (showExitConfirm) {
            ExitConfirmOverlay(
                onConfirmExit = { activity?.finish() },
                onDismiss = { showExitConfirm = false },
            )
        }
    }
}

@Composable
private fun ServerRow(
    device: SwarmDevice,
    onLan: Boolean,
    disconnected: Boolean,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val cardShape = RoundedCornerShape(10.dp)
    Card(
        onClick = onClick,
        modifier = modifier.fillMaxWidth(),
        shape = CardDefaults.shape(cardShape, cardShape, cardShape),
        colors = CardDefaults.colors(containerColor = SwarmSurface, focusedContainerColor = SwarmSurfaceMuted),
        scale = CardDefaults.scale(scale = 1f, focusedScale = 1.02f, pressedScale = 1.01f),
        border = CardDefaults.border(
            border = Border(BorderStroke(1.dp, SwarmBorder), shape = cardShape),
            focusedBorder = Border(BorderStroke(3.dp, SwarmAccent), shape = cardShape),
            pressedBorder = Border(BorderStroke(3.dp, SwarmAccentHot), shape = cardShape),
        ),
    ) {
        Row(
            modifier = Modifier.fillMaxWidth().padding(16.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.SpaceBetween,
        ) {
            Column {
                Text(device.name, color = SwarmText, fontSize = 16.sp, fontWeight = FontWeight.SemiBold)
                Text(device.metadata["hostname"] ?: device.deviceId, color = SwarmMuted, fontSize = 12.sp)
            }
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                if (onLan) Badge("LAN", SwarmAccent)
                Badge(
                    text = connectionStatusLabel(device.online, disconnected),
                    color = if (disconnected || !device.online) SwarmMuted else SwarmGreen,
                )
            }
        }
    }
}

@Composable
private fun LanServerRow(
    modifier: Modifier = Modifier,
    server: LanServer,
    inSwarm: Boolean,
    paired: Boolean,
    disconnected: Boolean,
    online: Boolean,
    busy: Boolean,
    onClick: () -> Unit,
) {
    val cardShape = RoundedCornerShape(10.dp)
    Card(
        onClick = { if (!busy) onClick() },
        modifier = modifier.fillMaxWidth(),
        shape = CardDefaults.shape(cardShape, cardShape, cardShape),
        colors = CardDefaults.colors(containerColor = SwarmSurface, focusedContainerColor = SwarmSurfaceMuted),
        scale = CardDefaults.scale(scale = 1f, focusedScale = 1.02f, pressedScale = 1.01f),
        border = CardDefaults.border(
            border = Border(BorderStroke(1.dp, SwarmBorder), shape = cardShape),
            focusedBorder = Border(BorderStroke(3.dp, SwarmAccent), shape = cardShape),
            pressedBorder = Border(BorderStroke(3.dp, SwarmAccentHot), shape = cardShape),
        ),
    ) {
        Row(
            modifier = Modifier.fillMaxWidth().padding(16.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.SpaceBetween,
        ) {
            Column {
                Text(server.name, color = SwarmText, fontSize = 16.sp, fontWeight = FontWeight.SemiBold)
                Text(
                    when {
                        disconnected -> "Disconnected from this TV"
                        inSwarm -> "Connected directly on your network"
                        paired -> "Paired with this TV"
                        else -> "Select to show an approval code on this TV"
                    },
                    color = SwarmMuted,
                    fontSize = 12.sp,
                )
            }
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Badge("LAN", SwarmAccent)
                if (inSwarm) Badge("in SWARM", SwarmGreen) else if (paired) Badge("paired", SwarmGreen)
                if (inSwarm || paired || disconnected) {
                    Badge(
                        text = connectionStatusLabel(online, disconnected),
                        color = if (online && !disconnected) SwarmGreen else SwarmMuted,
                    )
                }
            }
        }
    }
}

internal data class LanServerRowState(val server: LanServer, val online: Boolean)

/** The rendezvous roster retains historical device registrations. Offline
 * servers are not usable connection targets, so keep them out of the TV's
 * server picker; a current mDNS route promotes a reachable roster entry to
 * online before the dashboard is rendered. */
internal fun visibleSwarmServers(devices: List<SwarmDevice>): List<SwarmDevice> =
    devices.filter {
        it.online && (it.deviceType == DeviceType.SERVER || it.deviceType == DeviceType.BOTH)
    }

/** Keeps paired servers visible after their mDNS advertisement disappears,
 * while preferring the current address and ports for servers still online. */
internal fun knownLanServerRows(
    discovered: List<LanServer>,
    paired: List<LanServer>,
): List<LanServerRowState> {
    val seen = mutableSetOf<String>()
    return buildList {
        discovered.forEach { server ->
            if (seen.add(server.certFingerprint.trim().lowercase())) {
                add(LanServerRowState(server, online = true))
            }
        }
        paired.forEach { server ->
            if (seen.add(server.certFingerprint.trim().lowercase())) {
                add(LanServerRowState(server, online = false))
            }
        }
    }
}

internal fun connectionStatusLabel(online: Boolean, disconnected: Boolean): String = when {
    disconnected -> "disconnected"
    online -> "connected"
    else -> "offline"
}

@Composable
private fun AddServerOverlay(
    busy: Boolean,
    onAdd: () -> Unit,
    onDismiss: () -> Unit,
) {
    BackHandler(onBack = onDismiss)
    val actionFocusRequester = remember { FocusRequester() }
    LaunchedEffect(Unit) { actionFocusRequester.requestFocus() }

    ModalSurface {
        Text("Add Server", color = SwarmText, fontSize = 20.sp, fontWeight = FontWeight.Black)
        Spacer(Modifier.height(6.dp))
        Text("This TV will show a temporary code. Enter it in the media server app to approve the connection.", color = SwarmMuted, fontSize = 13.sp)
        Spacer(Modifier.height(14.dp))
        Button(
            onClick = onAdd,
            enabled = !busy,
            modifier = Modifier
                .focusRequester(actionFocusRequester)
                .testTag(UatTestTags.ADD_SERVER_START_BUTTON),
            colors = swarmActionButtonColors(),
        ) {
            Text(if (busy) "Requesting…" else "Show activation code", fontWeight = FontWeight.Bold)
        }
    }
}

@Composable
private fun Badge(text: String, color: Color) {
    Row(
        modifier = Modifier
            .clip(RoundedCornerShape(999.dp))
            .background(color.copy(alpha = 0.18f))
            .padding(horizontal = 10.dp, vertical = 4.dp),
    ) {
        Text(text, color = color, fontSize = 12.sp)
    }
}

@Composable
private fun ServerConnectionOverlay(
    server: SwarmDevice,
    disconnected: Boolean,
    onConfirm: () -> Unit,
    onDismiss: () -> Unit,
) {
    BackHandler(onBack = onDismiss)
    val actionFocusRequester = remember { FocusRequester() }
    LaunchedEffect(Unit) { actionFocusRequester.requestFocus() }

    ModalSurface {
        Text(
            if (disconnected) "Reconnect server?" else "Remove from swarm?",
            color = SwarmText,
            fontSize = 20.sp,
            fontWeight = FontWeight.Black,
        )
        Spacer(Modifier.height(8.dp))
        Text(server.name, color = SwarmText, fontSize = 16.sp, fontWeight = FontWeight.SemiBold)
        Spacer(Modifier.height(6.dp))
        Text(
            if (disconnected) {
                "This TV will include this server in browsing and playback again."
            } else {
                "This disconnects the server from this TV only. Other devices in the swarm are not affected."
            },
            color = SwarmMuted,
            fontSize = 13.sp,
        )
        Spacer(Modifier.height(20.dp))
        Button(
            onClick = onConfirm,
            modifier = Modifier.focusRequester(actionFocusRequester),
            colors = swarmActionButtonColors(),
        ) {
            Text(if (disconnected) "Connect" else "Disconnect", fontWeight = FontWeight.Bold)
        }
    }
}

@Composable
private fun LanServerActionOverlay(
    server: LanServer,
    disconnected: Boolean,
    paired: Boolean,
    onConnect: () -> Unit,
    onDisconnect: () -> Unit,
    onForget: () -> Unit,
    onDismiss: () -> Unit,
) {
    BackHandler(onBack = onDismiss)
    val actionFocusRequester = remember { FocusRequester() }
    LaunchedEffect(Unit) { actionFocusRequester.requestFocus() }

    ModalSurface {
        Column(
            modifier = Modifier.testTag(UatTestTags.LAN_SERVER_MODAL),
            horizontalAlignment = Alignment.CenterHorizontally,
        ) {
            Text(server.name, color = SwarmText, fontSize = 20.sp, fontWeight = FontWeight.Black)
            Spacer(Modifier.height(6.dp))
            Text(server.host, color = SwarmMuted, fontSize = 13.sp)
            Spacer(Modifier.height(6.dp))
            Text(
                when {
                    disconnected -> "Disconnected from this TV"
                    paired -> "Paired with this TV"
                    else -> "Connected on your network"
                },
                color = SwarmMuted,
                fontSize = 13.sp,
            )
            Spacer(Modifier.height(20.dp))
            Button(
                onClick = onConnect,
                modifier = Modifier
                    .focusRequester(actionFocusRequester)
                    .testTag(UatTestTags.LAN_SERVER_CONNECT_BUTTON),
                colors = swarmActionButtonColors(),
            ) {
                Text(if (disconnected) "Reconnect" else "Connect", fontWeight = FontWeight.Bold)
            }
            if (!disconnected) {
                Spacer(Modifier.height(10.dp))
                Button(
                    onClick = onDisconnect,
                    modifier = Modifier.testTag(UatTestTags.LAN_SERVER_DISCONNECT_BUTTON),
                    colors = swarmActionButtonColors(),
                ) {
                    Text("Disconnect", fontWeight = FontWeight.Bold)
                }
            }
            Spacer(Modifier.height(10.dp))
            Button(
                onClick = onForget,
                modifier = Modifier.testTag(UatTestTags.LAN_SERVER_FORGET_BUTTON),
                colors = swarmActionButtonColors(),
            ) {
                Text("Forget this server", color = SwarmError, fontWeight = FontWeight.Bold)
            }
        }
    }
}

@Composable
private fun LanPairingOverlay(
    server: LanServer,
    activation: LanPairingActivation?,
    busy: Boolean,
    error: String?,
    onRetry: () -> Unit,
    onDismiss: () -> Unit,
) {
    BackHandler(onBack = onDismiss)
    val actionFocusRequester = remember { FocusRequester() }
    LaunchedEffect(Unit) { actionFocusRequester.requestFocus() }

    ModalSurface {
        Text("Pair with ${server.name}", color = SwarmText, fontSize = 20.sp, fontWeight = FontWeight.Black)
        Spacer(Modifier.height(6.dp))
        if (activation == null && busy) {
            Text("Requesting a secure activation code…", color = SwarmMuted, fontSize = 13.sp)
        } else if (activation != null) {
            Text("Enter this code on the media server's SWARM page", color = SwarmMuted, fontSize = 13.sp)
            Spacer(Modifier.height(18.dp))
            Text(
                activation.code.chunked(4).joinToString("  "),
                color = SwarmAccent,
                fontSize = 42.sp,
                fontWeight = FontWeight.Black,
            )
            Spacer(Modifier.height(12.dp))
            Text("Waiting for approval · expires in about five minutes", color = SwarmMuted, fontSize = 12.sp)
        }
        if (error != null) {
            Spacer(Modifier.height(10.dp))
            Text(error, color = SwarmError, fontSize = 12.sp)
        }
        Spacer(Modifier.height(14.dp))
        Button(
            onClick = if (error != null) onRetry else onDismiss,
            modifier = Modifier.focusRequester(actionFocusRequester),
            colors = swarmActionButtonColors(),
        ) {
            Text(if (error != null) "Try again" else "Cancel", fontWeight = FontWeight.Bold)
        }
    }
}

@Composable
private fun ModalSurface(content: @Composable ColumnScope.() -> Unit) {
    Box(
        modifier = Modifier.fillMaxSize().background(Color.Black.copy(alpha = 0.85f)),
        contentAlignment = Alignment.Center,
    ) {
        Column(
            modifier = Modifier
                .width(540.dp)
                .clip(RoundedCornerShape(16.dp))
                .background(SwarmSurface)
                .verticalScroll(rememberScrollState())
                .padding(28.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
            content = content,
        )
    }
}

@Composable
internal fun ExitConfirmOverlay(onConfirmExit: () -> Unit, onDismiss: () -> Unit) {
    BackHandler(onBack = onDismiss)
    val exitFocusRequester = remember { FocusRequester() }
    LaunchedEffect(Unit) { exitFocusRequester.requestFocus() }

    ModalSurface {
        Text("Exit app?", color = SwarmText, fontSize = 20.sp, fontWeight = FontWeight.Black)
        Spacer(Modifier.height(8.dp))
        Text("Choose Exit app to close SWARM.", color = SwarmMuted, fontSize = 13.sp)
        Spacer(Modifier.height(20.dp))
        Button(
            onClick = onConfirmExit,
            modifier = Modifier.focusRequester(exitFocusRequester),
            colors = swarmActionButtonColors(),
        ) {
            Text("Exit app", fontWeight = FontWeight.Bold)
        }
    }
}
