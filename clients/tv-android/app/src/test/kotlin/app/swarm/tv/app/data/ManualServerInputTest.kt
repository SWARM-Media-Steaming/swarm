package app.swarm.tv.app.data

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Test

class ManualServerInputTest {
    @Test
    fun `bare IP defaults to the server peer port`() {
        assertEquals("192.168.0.126" to 8543, parseManualServerInput("192.168.0.126"))
    }

    @Test
    fun `explicit port is honored`() {
        assertEquals("192.168.0.126" to 8544, parseManualServerInput("192.168.0.126:8544"))
    }

    @Test
    fun `hostname with port works`() {
        assertEquals("mediaserver" to 8543, parseManualServerInput("mediaserver:8543"))
    }

    @Test
    fun `scheme and path are stripped`() {
        assertEquals(
            "192.168.0.126" to 8543,
            parseManualServerInput("http://192.168.0.126:8543/some/path"),
        )
    }

    @Test
    fun `bracketed IPv6 with port works`() {
        assertEquals("::1" to 8543, parseManualServerInput("[::1]:8543"))
    }

    @Test
    fun `bare IPv6 keeps the default port`() {
        assertEquals("::1" to 8543, parseManualServerInput("::1"))
    }

    @Test
    fun `blank and garbage input is rejected`() {
        assertNull(parseManualServerInput(""))
        assertNull(parseManualServerInput("   "))
        assertNull(parseManualServerInput(":8543"))
        assertNull(parseManualServerInput("192.168.0.126:notaport"))
        assertNull(parseManualServerInput("192.168.0.126:0"))
        assertNull(parseManualServerInput("192.168.0.126:99999"))
        assertNull(parseManualServerInput("[::1"))
    }
}
