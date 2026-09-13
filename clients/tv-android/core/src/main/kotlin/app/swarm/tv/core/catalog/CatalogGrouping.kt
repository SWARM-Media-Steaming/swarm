/**
 * Groups a merged catalog into Netflix-style Artist->Album->Track and
 * Show->Season->Episode hierarchies, entirely client-side over data already
 * fetched by [CatalogMerger] — no server round trip, same "STUN/the server
 * never needs to know about this, it happens locally" philosophy as
 * [CatalogMerger]'s own doc comment. Movies need no grouping (each is its
 * own leaf), so there's no `MovieGroup` here.
 *
 * Entries missing a grouping field (artist/album/show/season) are bucketed
 * under an "Unknown ..." group rather than dropped, so a scrape gap or a
 * loose file never silently disappears from the library.
 */
package app.swarm.tv.core.catalog

import app.swarm.tv.core.peer.MediaKind
import kotlin.random.Random

const val UNKNOWN_ARTIST = "Unknown Artist"
const val UNKNOWN_ALBUM = "Unknown Album"
const val UNKNOWN_SHOW = "Unknown Show"

data class AlbumGroup(val album: String, val tracks: List<MergedEntry>)
data class ArtistGroup(val artist: String, val albums: List<AlbumGroup>)

/**
 * How music "keep playing" chooses the track after the current one.
 *
 * - [OFF]: sequential — the next track on the album, then track 1 of the
 *   next album, then the end of the artist's discography.
 * - [ALBUM]: shuffle within the album the current track is on — "shuffle
 *   what I'm listening to."
 * - [ALL_SONGS]: shuffle across every track in the whole library,
 *   regardless of artist or album.
 *
 * [next] cycles OFF -> ALBUM -> ALL_SONGS -> OFF, matching the single
 * shuffle button on the music player screen.
 */
enum class ShuffleMode {
    OFF,
    ALBUM,
    ALL_SONGS;

    fun next(): ShuffleMode = entries[(ordinal + 1) % entries.size]
}

/**
 * The music player's repeat button, independent of [ShuffleMode].
 *
 * - [OFF]: "keep playing" advances normally and stops at the end of the
 *   artist's discography (or wherever [nextTrack] runs out).
 * - [ONE]: repeat the current song. Handled by the hoisted ExoPlayer's
 *   own `REPEAT_MODE_ONE` so the loop is gapless; [nextTrack] still
 *   reports the real following track so an explicit Skip press advances.
 * - [ALBUM]: repeat the current album — [nextTrack]/[previousTrack] wrap
 *   around the album the current track is on instead of crossing into
 *   the neighbouring album, and never return null at the album edge.
 *
 * [next] cycles OFF -> ONE -> ALBUM -> OFF, matching the two on-states
 * ("repeat song", "repeat album") the single repeat button steps through.
 */
enum class RepeatMode {
    OFF,
    ONE,
    ALBUM;

    fun next(): RepeatMode = entries[(ordinal + 1) % entries.size]
}

data class SeasonGroup(val season: Int?, val episodes: List<MergedEntry>)
data class ShowGroup(val show: String, val seasons: List<SeasonGroup>)

object CatalogGrouping {
    fun movies(entries: List<MergedEntry>): List<MergedEntry> =
        entries.filter { it.entry.kind == MediaKind.MOVIE && it.entry.extraType == null }

    fun movieExtras(movie: MergedEntry, entries: List<MergedEntry>): List<MergedEntry> =
        entries.filter { it.entry.extraType != null && it.entry.parentEntryKey == movie.entry.entryKey }
            .sortedWith(compareBy({ it.entry.extraType }, { it.entry.extraTitle?.lowercase() }))

    /** A season's true episodes/specials, excluding bonus content — see [seasonExtras]. */
    fun seasonEpisodes(season: SeasonGroup): List<MergedEntry> =
        season.episodes.filter { it.entry.extraType == null }

    /**
     * A season's own bonus content (featurettes/deleted scenes/etc. living
     * in a season-0 extras folder) — same "nested under the parent, not a
     * plain listing" treatment as [movieExtras], but keyed by season
     * membership rather than `parentEntryKey` since a show has no synthetic
     * parent row to point at (it links via `showTitle` alone).
     */
    fun seasonExtras(season: SeasonGroup): List<MergedEntry> =
        season.episodes.filter { it.entry.extraType != null }
            .sortedWith(compareBy({ it.entry.extraType }, { it.entry.extraTitle?.lowercase() }))

    /** Human label for a Plex-style extras slug, shared by [MovieDetailScreen] and [SeasonScreen]. */
    fun extraTypeLabel(type: String?): String = when (type) {
        "behindTheScenes" -> "Behind the Scenes"
        "deletedScene" -> "Deleted Scene"
        "featurette" -> "Featurette"
        "interview" -> "Interview"
        "scene" -> "Scene"
        "short" -> "Short"
        "trailer" -> "Trailer"
        else -> "Other"
    }

    fun groupTracksByArtistAlbum(entries: List<MergedEntry>): List<ArtistGroup> {
        val byArtist = entries.filter { it.entry.kind == MediaKind.TRACK }.groupBy { artistNameOf(it) }
        return byArtist.entries
            .sortedWith(compareBy({ it.key == UNKNOWN_ARTIST }, { it.key.lowercase() }))
            .map { (artist, artistEntries) ->
                val byAlbum = artistEntries.groupBy { albumNameOf(it) }
                val albums = byAlbum.entries
                    .sortedWith(compareBy({ it.key == UNKNOWN_ALBUM }, { it.key.lowercase() }))
                    .map { (album, albumEntries) -> AlbumGroup(album, albumEntries.sortedWith(trackOrder)) }
                ArtistGroup(artist, albums)
            }
    }

    /**
     * Two on-disk show folders can hold the same real series under
     * different names ("Law & Order SVU" vs. "Law & Order Special Victims
     * Unit") — the path-derived show title alone can never tell them apart
     * (and by design never should: a bad scrape must never be able to split
     * or corrupt a grouping that's otherwise correct — see the Rust side's
     * `classify.rs` module doc comment). A *matching* scrape is different:
     * if episodes under two different show titles agree on the same
     * TMDb-confirmed `scrapedTitle`, that's real external corroboration,
     * not something a bad scrape could coincidentally produce for two
     * unrelated folders. So: fold any show title whose episodes have a
     * clear `scrapedTitle` consensus into that canonical key, purely for
     * display grouping — the underlying entries and their path-derived
     * fields are never touched. Mirrors `canonicalShowKeys` in the server
     * GUI's `media.js`.
     */
    private fun canonicalShowKeys(episodes: List<MergedEntry>): Map<String, String> {
        val scrapedCounts = mutableMapOf<String, MutableMap<String, Int>>()
        for (e in episodes) {
            val scraped = e.entry.scrapedTitle?.takeIf { it.isNotBlank() } ?: continue
            val counts = scrapedCounts.getOrPut(showNameOf(e)) { mutableMapOf() }
            counts[scraped] = (counts[scraped] ?: 0) + 1
        }
        return scrapedCounts.mapValues { (_, counts) -> counts.maxByOrNull { it.value }!!.key }
    }

    fun groupEpisodesByShowSeason(entries: List<MergedEntry>): List<ShowGroup> {
        val episodes = entries.filter { it.entry.kind == MediaKind.EPISODE }
        val canonicalFor = canonicalShowKeys(episodes)
        val byShow = episodes.groupBy { canonicalFor[showNameOf(it)] ?: showNameOf(it) }
        return byShow.entries
            .sortedWith(compareBy({ it.key == UNKNOWN_SHOW }, { it.key.lowercase() }))
            .map { (show, showEntries) ->
                val bySeason = showEntries.groupBy { it.entry.season }
                val seasons = bySeason.entries
                    .sortedWith(compareBy({ it.key == null }, { it.key ?: Int.MAX_VALUE }))
                    .map { (season, seasonEntries) -> SeasonGroup(season, seasonEntries.sortedWith(episodeOrder)) }
                ShowGroup(show, seasons)
            }
    }

    /**
     * Numbered seasons containing numbered episodes are the only safe source
     * for a show-card preview. Season 0 is the conventional specials/extras
     * bucket, while null seasons or episodes are commonly featurettes,
     * interviews, and other bonus files inferred from a show's directory.
     */
    fun previewSeasons(show: ShowGroup): List<SeasonGroup> = show.seasons.mapNotNull { season ->
        if ((season.season ?: 0) <= 0) return@mapNotNull null
        val episodes = season.episodes.filter { (it.entry.episode ?: 0) > 0 }
        season.copy(episodes = episodes).takeIf { episodes.isNotEmpty() }
    }

    /** Pick a season first and then an episode so both levels vary over time. */
    fun randomPreviewEpisode(show: ShowGroup, random: Random = Random.Default): MergedEntry? {
        val season = previewSeasons(show).randomOrNull(random) ?: return null
        return season.episodes.randomOrNull(random)
    }

    /**
     * The episode after [current] in the same show: next episode in the
     * same season if one exists, else episode 1 of the next season, else
     * null (end of the show, or [current] isn't a known episode of a known
     * show in [shows] — e.g. it was removed from the catalog mid-playback).
     * Finds [current]'s show by scanning for its fingerprint rather than by
     * name equality against `showNameOf(current)` — [shows]' own key may be
     * a merged canonical name (see [canonicalShowKeys]) that no longer
     * equals the raw path-derived show title on [current].
     */
    fun nextEpisode(current: MergedEntry, shows: List<ShowGroup>): MergedEntry? {
        val show = shows.find { g -> g.seasons.any { s -> s.episodes.any { it.fingerprint == current.fingerprint } } } ?: return null
        val seasonIndex = show.seasons.indexOfFirst { it.season == current.entry.season }
        if (seasonIndex == -1) return null
        val episodeIndex = show.seasons[seasonIndex].episodes.indexOfFirst { it.fingerprint == current.fingerprint }
        if (episodeIndex == -1) return null
        show.seasons[seasonIndex].episodes.getOrNull(episodeIndex + 1)?.let { return it }
        return show.seasons.getOrNull(seasonIndex + 1)?.episodes?.firstOrNull()
    }

    /**
     * The track after [current], same "keep playing" concept as
     * [nextEpisode].
     *
     * - [ShuffleMode.OFF]: sequential (next track in the same album, else
     *   track 1 of the next album, else null at the end of the artist's
     *   discography or if [current] is no longer in [artists]).
     * - [ShuffleMode.ALBUM]: picks uniformly at random from every *other*
     *   track on the same album — scoped to the album [current] is actually
     *   on, so "shuffle" means "shuffle what I'm listening to."
     * - [ShuffleMode.ALL_SONGS]: picks uniformly at random from every other
     *   track anywhere in [artists], so a long listening session wanders
     *   across the whole library rather than staying on one album.
     *
     * Both shuffle modes fall back to [current] itself when there is
     * nothing else to shuffle to (a single-track album / library) rather
     * than returning null and silently stopping.
     *
     * [repeat] layers on top: [RepeatMode.ALBUM] keeps selection inside
     * the current album (a sequential run wraps back to track 1 at the
     * end, and an [ShuffleMode.ALL_SONGS] request is narrowed to the
     * album) so the album loops forever; [RepeatMode.ONE] is handled by
     * the player itself, so it behaves like [RepeatMode.OFF] here on
     * purpose — an explicit Skip should still move to the next song.
     */
    fun nextTrack(
        current: MergedEntry,
        artists: List<ArtistGroup>,
        mode: ShuffleMode,
        repeat: RepeatMode = RepeatMode.OFF,
        random: Random = Random.Default,
    ): MergedEntry? {
        val artistIndex = artists.indexOfFirst { a -> a.albums.any { al -> al.tracks.any { it.fingerprint == current.fingerprint } } }
        if (artistIndex == -1) return null
        val artist = artists[artistIndex]
        val albumIndex = artist.albums.indexOfFirst { al -> al.tracks.any { it.fingerprint == current.fingerprint } }
        if (albumIndex == -1) return null
        val album = artist.albums[albumIndex]
        val effectiveMode = if (repeat == RepeatMode.ALBUM && mode == ShuffleMode.ALL_SONGS) ShuffleMode.ALBUM else mode
        when (effectiveMode) {
            ShuffleMode.ALBUM -> {
                val others = album.tracks.filter { it.fingerprint != current.fingerprint }
                return others.randomOrNull(random) ?: current
            }
            ShuffleMode.ALL_SONGS -> {
                val others = artists.asSequence()
                    .flatMap { it.albums.asSequence() }
                    .flatMap { it.tracks.asSequence() }
                    .filter { it.fingerprint != current.fingerprint }
                    .toList()
                return others.randomOrNull(random) ?: current
            }
            ShuffleMode.OFF -> {
                val trackIndex = album.tracks.indexOfFirst { it.fingerprint == current.fingerprint }
                if (trackIndex == -1) return null
                album.tracks.getOrNull(trackIndex + 1)?.let { return it }
                if (repeat == RepeatMode.ALBUM) return album.tracks.firstOrNull()
                return artist.albums.getOrNull(albumIndex + 1)?.tracks?.firstOrNull()
            }
        }
    }

    /**
     * The track before [current] — the sequential mirror of [nextTrack]
     * with [ShuffleMode.OFF]: previous track on the same album, else the
     * last track of the previous album, else null at the very start of
     * the artist's discography (or if [current] is no longer in
     * [artists]). "Previous" deliberately ignores [ShuffleMode] — there
     * is no playback history to walk back through — and, with [repeat] ==
     * [RepeatMode.ALBUM], wraps to the last track of the same album
     * instead of stopping.
     */
    fun previousTrack(
        current: MergedEntry,
        artists: List<ArtistGroup>,
        repeat: RepeatMode = RepeatMode.OFF,
    ): MergedEntry? {
        val artistIndex = artists.indexOfFirst { a -> a.albums.any { al -> al.tracks.any { it.fingerprint == current.fingerprint } } }
        if (artistIndex == -1) return null
        val artist = artists[artistIndex]
        val albumIndex = artist.albums.indexOfFirst { al -> al.tracks.any { it.fingerprint == current.fingerprint } }
        if (albumIndex == -1) return null
        val album = artist.albums[albumIndex]
        val trackIndex = album.tracks.indexOfFirst { it.fingerprint == current.fingerprint }
        if (trackIndex == -1) return null
        album.tracks.getOrNull(trackIndex - 1)?.let { return it }
        if (repeat == RepeatMode.ALBUM) return album.tracks.lastOrNull()
        return artist.albums.getOrNull(albumIndex - 1)?.tracks?.lastOrNull()
    }

    private val trackOrder = compareBy<MergedEntry>({ it.entry.trackNumber == null }, { it.entry.trackNumber ?: Int.MAX_VALUE }, { it.entry.title.lowercase() })
    private val episodeOrder = compareBy<MergedEntry>({ it.entry.episode == null }, { it.entry.episode ?: Int.MAX_VALUE }, { it.entry.title.lowercase() })

    private fun artistNameOf(e: MergedEntry) = e.entry.artist?.takeIf { it.isNotBlank() } ?: UNKNOWN_ARTIST
    private fun albumNameOf(e: MergedEntry) = e.entry.album?.takeIf { it.isNotBlank() } ?: UNKNOWN_ALBUM
    private fun showNameOf(e: MergedEntry) = e.entry.showTitle?.takeIf { it.isNotBlank() } ?: UNKNOWN_SHOW
}
