/**
 * Cross-language wire-compatibility checks for the peer protocol, same
 * discipline as `rest/ContractsTest.kt` — fixtures captured from real
 * `serde_json` output. [ByteRange] gets the most scrutiny here since it's
 * hand-serialized (see [ByteRangeSerializer]'s doc comment for why).
 */
package app.swarm.tv.core.peer

import app.swarm.tv.core.capability.CapabilityProfile
import app.swarm.tv.core.rest.SwarmJson
import kotlinx.serialization.decodeFromString
import kotlinx.serialization.encodeToString
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Test

class ContractsTest {

    @Test
    fun `buzz response decodes serde screen model`() {
        val json = """{"session_id":"abc","screen":"recommendation","buzz_text":"I think I've got one.","voice_asset":"buzz/i_think_ive_got_one_01.opus","media_id":"123","title":"Galaxy Quest","reasons":["Funny","Not watched yet"],"actions":["play","try_again","not_interested"]}"""
        val response = SwarmJson.decodeFromString<BuzzResponse>(json)
        assertEquals("abc", response.sessionId)
        assertEquals("Galaxy Quest", response.title)
        assertEquals(listOf("play", "try_again", "not_interested"), response.actions)
        assertEquals(emptyList<BuzzChoice>(), response.choices)
    }

    @Test
    fun `resolved problem notification decodes serde field names`() {
        val json = """{"id":7,"asset_title":"Example Movie","original_message":"Playback failed.","comments":"Replaced the file.","resolved_at_ms":1700000000000}"""
        assertEquals(
            ClientResolutionNotification(
                id = 7,
                assetTitle = "Example Movie",
                originalMessage = "Playback failed.",
                comments = "Replaced the file.",
                resolvedAtMs = 1_700_000_000_000,
            ),
            SwarmJson.decodeFromString<ClientResolutionNotification>(json),
        )
    }

    @Test
    fun `kotlin decodes a real serde-produced catalog manifest`() {
        val json = """{"thumbprint":"${"ff".repeat(32)}","entries":[{"entry_key":"030fe19c72f2665e6efd018a",""" +
            """"fingerprint":"704ac5a4284267953aab77855e0e32aa","kind":"movie","title":"Inception",""" +
            """"size":4700000000,"duration_secs":8880.0,"scraped_title":"Inception (2010)","genres":["Sci-Fi"],""" +
            """"video":{"codec":"h264","width":1920,"height":1080,"level":"4.1","bitrate":8000000},""" +
            """"audio":{"codec":"aac","channels":6},"artwork_etag":"v1","year":2010,""" +
            """"cast":[{"name":"Leonardo DiCaprio","character":"Cobb"},{"name":"Ellen Page"}]}],"removed":[]}"""
        val manifest = SwarmJson.decodeFromString<CatalogManifest>(json)
        assertEquals(1, manifest.entries.size)
        val entry = manifest.entries[0]
        assertEquals(MediaKind.MOVIE, entry.kind)
        assertEquals("Inception", entry.title)
        assertEquals(4_700_000_000L, entry.size)
        assertEquals(8880.0, entry.durationSecs)
        assertEquals("Inception (2010)", entry.scrapedTitle)
        assertEquals(listOf("Sci-Fi"), entry.genres)
        assertEquals(VideoStreamInfo("h264", 1920, 1080, "4.1", 8_000_000L), entry.video)
        // audio.bitrate was omitted by serde (skip_serializing_if) — must
        // still decode cleanly to the Kotlin default of null.
        assertEquals(AudioStreamInfo("aac", 6, null), entry.audio)
        assertNull(entry.showTitle)
        assertNull(entry.artist)
        assertEquals(2010, entry.year)
        assertEquals(listOf(CastMember("Leonardo DiCaprio", "Cobb"), CastMember("Ellen Page", null)), entry.cast)
    }

    @Test
    fun `video stream info decodes serde's profile bit-depth and hdr fields`() {
        // Fixture captured from `serde_json::to_string` of a populated
        // swarm_core::peer::VideoStreamInfo (see swarm-contract-fixtures).
        val json =
            """{"codec":"hevc","width":3840,"height":2160,"level":"5.1","bitrate":2500000,""" +
                """"profile":"Main 10","bit_depth":10,"hdr":true}"""
        val decoded = SwarmJson.decodeFromString<VideoStreamInfo>(json)
        assertEquals(
            VideoStreamInfo("hevc", 3840, 2160, "5.1", 2_500_000L, "Main 10", 10, true),
            decoded,
        )
        // And the pre-existing shape (no new keys) still decodes with nulls.
        val legacy = SwarmJson.decodeFromString<VideoStreamInfo>(
            """{"codec":"h264","width":1920,"height":1080}""",
        )
        assertEquals(VideoStreamInfo("h264", 1920, 1080), legacy)
    }

    @Test
    fun `catalog entry with no year or cast decodes to empty defaults`() {
        val json = """{"entry_key":"k","fingerprint":"f","kind":"track","title":"Song","size":1}"""
        val entry = SwarmJson.decodeFromString<CatalogEntry>(json)
        assertNull(entry.year)
        assertEquals(emptyList<CastMember>(), entry.cast)
        assertEquals(emptyList<SkipSegment>(), entry.skipSegments)
    }

    @Test
    fun `catalog entry decodes intro database markers`() {
        val json = """{"entry_key":"k","fingerprint":"f","kind":"episode","title":"Episode","size":1,"skip_segments":[{"kind":"intro","start_ms":30000,"end_ms":90000},{"kind":"credits","start_ms":3000000}]}"""
        val entry = SwarmJson.decodeFromString<CatalogEntry>(json)

        assertEquals(
            listOf(
                SkipSegment(SkipSegmentKind.INTRO, 30_000L, 90_000L),
                SkipSegment(SkipSegmentKind.CREDITS, 3_000_000L, null),
            ),
            entry.skipSegments,
        )
    }

    @Test
    fun `byte range from_to matches serde's externally tagged shape`() {
        val expected = """{"path":"/media/030fe19c72f2665e6efd018a","range":{"from_to":{"start":1024,"end":null}}}"""
        val request = PeerRequest(path = "/media/030fe19c72f2665e6efd018a", range = ByteRange.FromTo(start = 1024, end = null))
        assertEquals(expected, SwarmJson.encodeToString(request))

        val decoded = SwarmJson.decodeFromString<PeerRequest>(expected)
        assertEquals(request, decoded)
    }

    @Test
    fun `byte range suffix matches serde's externally tagged shape`() {
        val expected = """{"path":"/media/x","range":{"suffix":{"last":500}}}"""
        val request = PeerRequest(path = "/media/x", range = ByteRange.Suffix(last = 500))
        assertEquals(expected, SwarmJson.encodeToString(request))
        assertEquals(request, SwarmJson.decodeFromString<PeerRequest>(expected))
    }

    @Test
    fun `byte range from_to with an explicit end decodes correctly`() {
        val decoded = SwarmJson.decodeFromString<ByteRange>("""{"from_to":{"start":100,"end":199}}""")
        assertEquals(ByteRange.FromTo(100, 199), decoded)
    }

    @Test
    fun `kotlin decodes a real serde-produced peer response header`() {
        val json = """{"status":206,"len":1024,"content_type":"video/x-matroska",""" +
            """"content_range":{"start":0,"end":1023,"total":4700000000}}"""
        val header = SwarmJson.decodeFromString<PeerResponseHeader>(json)
        assertEquals(206, header.status)
        assertEquals(1024L, header.len)
        assertEquals("video/x-matroska", header.contentType)
        assertEquals(ContentRange(0, 1023, 4_700_000_000L), header.contentRange)
        assertNull(header.etag)
    }

    @Test
    fun `playback negotiation matches serde shape`() {
        val request = PeerRequest(
            path = "/play/030fe19c72f2665e6efd018a",
            playback = PlaybackPreferences(
                capabilities = CapabilityProfile.fireTvBaseline(),
                startPositionSecs = 42,
            ),
        )
        val encoded = SwarmJson.encodeToString(request)
        assertEquals(
            """{"path":"/play/030fe19c72f2665e6efd018a","playback":{"capabilities":{"containers":["mp4","hls"],"video_codecs":["h264:high@4.2"],"audio_codecs":["aac","ac3","mp3"],"max_width":1920,"max_height":1080,"max_bitrate":12000000,"hdr":false},"start_position_secs":42,"prefer_direct":true,"preview":false}}""",
            encoded,
        )
        val previewEncoded = SwarmJson.encodeToString(
            request.copy(
                playback = request.playback?.copy(preferDirect = false, preview = true),
            ),
        )
        assertEquals(
            """{"path":"/play/030fe19c72f2665e6efd018a","playback":{"capabilities":{"containers":["mp4","hls"],"video_codecs":["h264:high@4.2"],"audio_codecs":["aac","ac3","mp3"],"max_width":1920,"max_height":1080,"max_bitrate":12000000,"hdr":false},"start_position_secs":42,"prefer_direct":false,"preview":true}}""",
            previewEncoded,
        )
        val plan = SwarmJson.decodeFromString<PlaybackPlan>(
            """{"mode":"hls","path":"/hls/session/master.m3u8","max_bitrate":4160000,"session_id":"session","lyrics":{"provider":"lrclib","provider_id":17,"language":"en","plain_lyrics":"First line","synced_lyrics":"[00:01.00]First line","instrumental":false},"subtitles":[{"id":"whisper-en","language":"en","label":"English — AI generated","source":"whisper","path":"/subtitles/030fe19c72f2665e6efd018a/whisper-en.vtt"}]}""",
        )
        assertEquals(
            PlaybackPlan(
                PlaybackMode.HLS,
                "/hls/session/master.m3u8",
                4_160_000,
                "session",
                TrackLyrics(
                    provider = "lrclib",
                    providerId = 17,
                    language = "en",
                    plainLyrics = "First line",
                    syncedLyrics = "[00:01.00]First line",
                ),
                listOf(
                    SubtitleTrack(
                        id = "whisper-en",
                        language = "en",
                        label = "English — AI generated",
                        source = "whisper",
                        path = "/subtitles/030fe19c72f2665e6efd018a/whisper-en.vtt",
                    ),
                ),
            ),
            plan,
        )
    }
}
