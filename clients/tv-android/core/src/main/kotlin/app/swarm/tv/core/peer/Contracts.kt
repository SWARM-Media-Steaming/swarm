/**
 * Device <-> device contracts carried over hole-punched QUIC. Hand-mirrored
 * from `swarm-core::peer` (Rust) — see that module's docs for the wire
 * shape (one request per QUIC bidi stream: a JSON header line, then raw
 * body bytes). The actual QUIC transport is not implemented on this side
 * yet (see `docs/reference` notes on the kwik integration); these types
 * exist now so the catalog-merge logic in [app.swarm.tv.core.catalog] has
 * something real to operate on and is ready the moment the transport lands.
 */
package app.swarm.tv.core.peer

import app.swarm.tv.core.capability.CapabilityProfile
import kotlinx.serialization.KSerializer
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.descriptors.SerialDescriptor
import kotlinx.serialization.descriptors.buildClassSerialDescriptor
import kotlinx.serialization.encoding.Decoder
import kotlinx.serialization.encoding.Encoder
import kotlinx.serialization.json.JsonDecoder
import kotlinx.serialization.json.JsonEncoder
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.long
import kotlinx.serialization.json.longOrNull
import kotlinx.serialization.json.putJsonObject

@Serializable
enum class MediaKind {
    @SerialName("movie") MOVIE,
    @SerialName("episode") EPISODE,
    @SerialName("track") TRACK,
}

@Serializable
enum class SkipSegmentKind {
    @SerialName("intro") INTRO,
    @SerialName("recap") RECAP,
    @SerialName("credits") CREDITS,
    @SerialName("preview") PREVIEW,
}

/** Community playback marker cached by the server from TheIntroDB. */
@Serializable
data class SkipSegment(
    val kind: SkipSegmentKind,
    val startMs: Long? = null,
    val endMs: Long? = null,
)

@Serializable
data class VideoStreamInfo(
    val codec: String,
    val width: Int,
    val height: Int,
    val level: String? = null,
    val bitrate: Long? = null,
    /** ffprobe `profile` ("High", "Main 10"); absent on rows scanned before
     * the server captured it. Mirrors `swarm-core::peer::VideoStreamInfo`. */
    val profile: String? = null,
    /** Luma bit depth (8/10/12) derived from the pixel format. */
    val bitDepth: Int? = null,
    /** `true` when the transfer characteristics are PQ or HLG. */
    val hdr: Boolean? = null,
)

@Serializable
data class AudioStreamInfo(
    val codec: String,
    val channels: Int,
    val bitrate: Long? = null,
)

/** One TMDb credits-list entry — display overlay only, never a grouping key. */
@Serializable
data class CastMember(
    val name: String,
    val character: String? = null,
)

/** One asset as advertised by a server. Merge key across servers is [fingerprint]. */
@Serializable
data class CatalogEntry(
    val entryKey: String,
    val fingerprint: String,
    val kind: MediaKind,
    val title: String,
    val size: Long,
    val relativePath: String? = null,
    val durationSecs: Double? = null,
    val showTitle: String? = null,
    val season: Int? = null,
    val episode: Int? = null,
    val artist: String? = null,
    val album: String? = null,
    val trackNumber: Int? = null,
    val scrapedTitle: String? = null,
    /** TMDb's display title for a TV episode/special; separate from the canonical show title in scrapedTitle. */
    val episodeTitle: String? = null,
    val genres: List<String> = emptyList(),
    val video: VideoStreamInfo? = null,
    val audio: AudioStreamInfo? = null,
    val artworkEtag: String? = null,
    val year: Int? = null,
    val cast: List<CastMember> = emptyList(),
    /** Synopsis — TMDb's own, or a manual override. Null for tracks and anything not yet scraped. */
    val overview: String? = null,
    /** US content rating — TMDb's own, or a manual override. Null for tracks, anything not yet scraped, or without a US certification. */
    val rating: String? = null,
    /** Provider community score normalized to 0–10; separate from the parental content rating. */
    val communityRating: Double? = null,
    /** Provider vote count behind [communityRating]. */
    val communityRatingVotes: Long? = null,
    /** Number of distinct devices that currently have this liked — see [LikeToggle]. */
    val likeCount: Int = 0,
    /** IntroDB markers; retained for every supported kind even though today's player consumes intros. */
    val skipSegments: List<SkipSegment> = emptyList(),
    /** Local movie-extra association and presentation metadata. */
    val parentEntryKey: String? = null,
    val extraType: String? = null,
    val extraTitle: String? = null,
    val extraRelativePath: String? = null,
    val extraCategoryPath: String? = null,
)

@Serializable
data class CatalogThumbprint(
    val thumbprint: String,
    val entryCount: Long,
)

@Serializable
data class CatalogManifest(
    val thumbprint: String,
    val entries: List<CatalogEntry>,
    val removed: List<String> = emptyList(),
    /** True when this is a complete replacement returned by the change feed. */
    val reset: Boolean = false,
)

/**
 * HTTP-style byte range. Serde's default representation for a Rust enum
 * with struct variants and no explicit `#[serde(tag = ...)]` is
 * *externally* tagged — `{"from_to": {"start": N, "end": M|null}}` or
 * `{"suffix": {"last": N}}` — not kotlinx.serialization's default
 * internally-tagged `{"type": "from_to", ...}` shape, so this needs a
 * hand-written [ByteRangeSerializer] rather than the usual sealed-class
 * polymorphism.
 */
@Serializable(with = ByteRangeSerializer::class)
sealed class ByteRange {
    data class FromTo(val start: Long, val end: Long? = null) : ByteRange()
    data class Suffix(val last: Long) : ByteRange()
}

object ByteRangeSerializer : KSerializer<ByteRange> {
    override val descriptor: SerialDescriptor = buildClassSerialDescriptor("ByteRange")

    override fun serialize(encoder: Encoder, value: ByteRange) {
        check(encoder is JsonEncoder) { "ByteRange only supports JSON encoding" }
        val json = when (value) {
            is ByteRange.FromTo -> buildJsonObject {
                putJsonObject("from_to") {
                    put("start", JsonPrimitive(value.start))
                    put("end", value.end?.let { JsonPrimitive(it) } ?: JsonNull)
                }
            }
            is ByteRange.Suffix -> buildJsonObject {
                putJsonObject("suffix") { put("last", JsonPrimitive(value.last)) }
            }
        }
        encoder.encodeJsonElement(json)
    }

    override fun deserialize(decoder: Decoder): ByteRange {
        check(decoder is JsonDecoder) { "ByteRange only supports JSON decoding" }
        val obj = decoder.decodeJsonElement().jsonObject
        obj["from_to"]?.let { fromTo ->
            val inner = fromTo.jsonObject
            return ByteRange.FromTo(inner.getValue("start").jsonPrimitive.long, inner["end"]?.jsonPrimitive?.longOrNull)
        }
        obj["suffix"]?.let { suffix ->
            return ByteRange.Suffix(suffix.jsonObject.getValue("last").jsonPrimitive.long)
        }
        error("unknown ByteRange variant in $obj")
    }
}

@Serializable
data class PeerRequest(
    val path: String,
    val range: ByteRange? = null,
    val ifNoneMatch: String? = null,
    val playback: PlaybackPreferences? = null,
    val errorReport: ClientErrorReport? = null,
    val like: LikeToggle? = null,
)

/**
 * Mirrors `swarm_core::peer::LikeToggle` (Rust) — see that type's doc
 * comment. Sent on `/likes/toggle`; [liked] is the desired end state (not
 * "flip whatever it currently is"), so a retried request is idempotent.
 */
@Serializable
data class LikeToggle(
    val deviceId: String,
    val deviceName: String,
    val entryKey: String,
    val liked: Boolean,
)

/** Mirrors `swarm_core::peer::ClientErrorReport` (Rust) — see that type's doc comment. Sent on `/errors/report`. */
@Serializable
data class ClientErrorReport(
    val deviceId: String,
    val deviceName: String,
    val entryKey: String? = null,
    val assetTitle: String? = null,
    val kind: String? = null,
    val message: String,
    val context: String? = null,
    val occurredAtMs: Long,
)

/** A resolved problem returned by `/notifications/{deviceId}`. */
@Serializable
data class ClientResolutionNotification(
    val id: Long,
    val assetTitle: String? = null,
    val originalMessage: String,
    val comments: String? = null,
    val resolvedAtMs: Long,
)

@Serializable
data class PlaybackPreferences(
    val capabilities: CapabilityProfile,
    val startPositionSecs: Long = 0,
    val preferDirect: Boolean = true,
    val preview: Boolean = false,
)

@Serializable
enum class PlaybackMode {
    @SerialName("direct") DIRECT,
    @SerialName("hls") HLS,
}

/** Cached music lyrics returned with playback negotiation, not with the much larger catalog manifest. */
@Serializable
data class TrackLyrics(
    val provider: String,
    val providerId: Long? = null,
    val language: String? = null,
    val plainLyrics: String? = null,
    val syncedLyrics: String? = null,
    val instrumental: Boolean = false,
)

/** Completed side-loaded subtitle track generated by the media server. */
@Serializable
data class SubtitleTrack(
    val id: String,
    val language: String,
    val label: String,
    val source: String,
    val path: String,
)

@Serializable
data class PlaybackPlan(
    val mode: PlaybackMode,
    val path: String,
    val maxBitrate: Long,
    /** Same id embedded in [path] — pass to [app.swarm.tv.core.catalog.CatalogSession.stopPlayback] on exit to release this session's bandwidth reservation without waiting out the server's idle timeout. */
    val sessionId: String,
    val lyrics: TrackLyrics? = null,
    val subtitles: List<SubtitleTrack> = emptyList(),
)

@Serializable
data class ContentRange(
    val start: Long,
    val end: Long,
    val total: Long,
)

@Serializable
data class PeerResponseHeader(
    val status: Int,
    val len: Long,
    val contentType: String? = null,
    val contentRange: ContentRange? = null,
    val etag: String? = null,
)
