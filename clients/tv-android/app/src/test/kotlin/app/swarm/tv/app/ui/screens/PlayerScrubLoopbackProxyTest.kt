package app.swarm.tv.app.ui.screens

import app.swarm.tv.core.peer.PlaybackMode
import app.swarm.tv.core.proxy.PeerLoopbackProxy
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

/**
 * Integration check for #508: the scrub-preview allow-list must accept the
 * exact URLs the real [PeerLoopbackProxy] hands the player for direct play
 * (so the #498 scene preview keeps working), while HLS from the same proxy
 * and any non-proxy host stay preview-less.
 */
class PlayerScrubLoopbackProxyTest {
    @Test
    fun `real loopback proxy direct-play urls qualify for scene previews`() {
        PeerLoopbackProxy.start().use { proxy ->
            val sessionId = "3f9c2a7e-58b1-4d0e-9a51-0c2f6f0e9b11"
            val direct = proxy.urlFor("device-7c1e", "/stream/$sessionId/media")
            assertTrue(isLoopbackProxyStreamUrl(direct), direct)
            assertTrue(supportsScrubPreview(PlaybackMode.DIRECT, direct), direct)

            val hls = proxy.urlFor("device-7c1e", "/hls/$sessionId/master.m3u8")
            assertFalse(supportsScrubPreview(PlaybackMode.HLS, hls), hls)
        }
    }

    @Test
    fun `a server-supplied path cannot steer the preview off the loopback proxy`() {
        PeerLoopbackProxy.start().use { proxy ->
            // The playback path comes from the media server's plan; whatever
            // it contains is appended after the proxy authority, so the
            // result must either still be the proxy or be rejected outright.
            listOf(
                "@example.test/stream/x/media",
                "/../../stream/x/media",
                "/stream/x/media#@example.test",
            ).forEach { hostilePath ->
                val url = proxy.urlFor("device-7c1e", hostilePath)
                if (supportsScrubPreview(PlaybackMode.DIRECT, url)) {
                    val uri = java.net.URI(url)
                    assertTrue(uri.host == "127.0.0.1" && uri.port == proxy.port, url)
                }
            }
        }
    }
}
