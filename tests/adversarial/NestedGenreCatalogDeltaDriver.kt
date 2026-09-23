import app.swarm.tv.app.data.BROWSE_ALL_MUSIC_TITLE
import app.swarm.tv.app.data.BROWSE_ALL_SHOWS_TITLE
import app.swarm.tv.app.data.artistsForBrowseAll
import app.swarm.tv.app.data.showsForBrowseAll
import app.swarm.tv.core.catalog.ArtistGroup
import app.swarm.tv.core.catalog.CatalogGrouping
import app.swarm.tv.core.catalog.MergedEntry
import app.swarm.tv.core.catalog.ShowGroup
import app.swarm.tv.core.peer.CatalogEntry
import app.swarm.tv.core.peer.MediaKind
import kotlin.system.exitProcess

/**
 * Nested-screen catalog-delta membership for #369.
 *
 * A genre Browse All page already rebuilds through artistsForBrowseAll /
 * showsForBrowseAll. Opening an artist or show from that page must keep
 * the same membership when a later catalog delta arrives — grouping the
 * whole kind would add albums/episodes the heading never included.
 *
 * [genreScope] is the originating genre shelf title when that shelf was
 * genre-scoped, and null for the top-level Music/Shows grids (and for a
 * plain catalog open). Heading text is not the discriminator: a user
 * genre can be spelled "Music" or "Shows".
 */
private fun track(
    key: String,
    artist: String,
    album: String,
    vararg genres: String,
    trackNumber: Int = 1,
) = MergedEntry(
    fingerprint = key,
    sources = listOf("server-a"),
    entry = CatalogEntry(
        entryKey = key,
        fingerprint = key,
        kind = MediaKind.TRACK,
        title = key,
        size = 1,
        artist = artist,
        album = album,
        trackNumber = trackNumber,
        genres = genres.toList(),
    ),
)

private fun episode(
    key: String,
    show: String,
    vararg genres: String,
    season: Int? = 1,
    episode: Int? = 1,
    scrapedTitle: String? = null,
) = MergedEntry(
    fingerprint = key,
    sources = listOf("server-a"),
    entry = CatalogEntry(
        entryKey = key,
        fingerprint = key,
        kind = MediaKind.EPISODE,
        title = key,
        size = 1,
        showTitle = show,
        season = season,
        episode = episode,
        scrapedTitle = scrapedTitle,
        genres = genres.toList(),
    ),
)

/** Same rebuild a nested ArtistAlbums screen must use on a catalog delta. */
private fun nestedArtists(entries: List<MergedEntry>, genreScope: String?): List<ArtistGroup> =
    genreScope?.let { artistsForBrowseAll(entries, it) }
        ?: CatalogGrouping.groupTracksByArtistAlbum(entries)

/** Same rebuild a nested ShowSeasons screen must use on a catalog delta. */
private fun nestedShows(entries: List<MergedEntry>, genreScope: String?): List<ShowGroup> =
    genreScope?.let { showsForBrowseAll(entries, it) }
        ?: CatalogGrouping.groupEpisodesByShowSeason(entries)

private fun albumsOf(artists: List<ArtistGroup>, artist: String): List<String> =
    artists.find { it.artist == artist }?.albums?.map { it.album } ?: emptyList()

private fun tracksOf(artists: List<ArtistGroup>, artist: String, album: String): List<String> =
    artists.find { it.artist == artist }
        ?.albums
        ?.find { it.album == album }
        ?.tracks
        ?.map { it.fingerprint }
        ?: emptyList()

private fun showsNamed(shows: List<ShowGroup>): List<String> = shows.map { it.show }

private fun episodesOf(shows: List<ShowGroup>, show: String): List<String> =
    shows.find { it.show == show }
        ?.seasons
        ?.flatMap { season -> season.episodes.map { it.fingerprint } }
        ?: emptyList()

private val failures = mutableListOf<String>()

private fun check(name: String, got: List<String>, expected: List<String>) {
    if (got != expected) {
        failures += "$name: got=$got expected=$expected"
        println("FAIL ${failures.last()}")
    } else {
        println("ok $name")
    }
}

fun main() {
    val jazzA = track("jazz-a", "Miles", "Kind of Blue", "Jazz", trackNumber = 1)
    val jazzB = track("jazz-b", "Miles", "Kind of Blue", "Jazz", trackNumber = 2)
    val rockAlbum = track("rock-a", "Miles", "On the Corner", "Rock")
    val otherArtistJazz = track("coltrane", "Coltrane", "Giant Steps", "Jazz")
    val otherArtistRock = track("hendrix", "Hendrix", "Are You Experienced", "Rock")
    val musicGenreTrack = track("named-music", "Miles", "Music Label", "Music")
    val blankGenre = track("blank", "Miles", "Untitled", "")
    val noGenre = track("none", "Miles", "No Genre")
    val unicode = track("anime-op", "LiSA", "Gurenge", "アニメ")
    val rnb = track("rnb", "Frank", "Channel Orange", "R&B")
    val bothGenres = track("crossover", "Miles", "Crossover", "Jazz", "Rock")

    val afterArtistDelta = listOf(
        jazzA, jazzB, rockAlbum, otherArtistJazz, otherArtistRock,
        musicGenreTrack, blankGenre, noGenre, unicode, rnb, bothGenres,
    )

    // Reproduction: Jazz Browse All -> Miles -> catalog grows a Rock album.
    check(
        "genre-nested-artist-does-not-gain-other-genre-albums",
        albumsOf(nestedArtists(afterArtistDelta, "Jazz"), "Miles"),
        listOf("Crossover", "Kind of Blue"),
    )
    check(
        "genre-nested-artist-keeps-in-genre-tracks-on-shared-album",
        tracksOf(nestedArtists(afterArtistDelta, "Jazz"), "Miles", "Kind of Blue"),
        listOf("jazz-a", "jazz-b"),
    )
    check(
        "multi-genre-track-stays-on-the-selected-genre-page",
        tracksOf(nestedArtists(afterArtistDelta, "Jazz"), "Miles", "Crossover"),
        listOf("crossover"),
    )
    check(
        "other-artists-in-the-genre-do-not-merge-into-this-artist",
        nestedArtists(afterArtistDelta, "Jazz").map { it.artist },
        listOf("Coltrane", "Miles"),
    )
    check(
        "blank-and-missing-genre-are-not-Jazz",
        albumsOf(nestedArtists(afterArtistDelta, "Jazz"), "Miles"),
        listOf("Crossover", "Kind of Blue"),
    )

    // Unscoped Music Browse All (or catalog) must still regroup the whole kind.
    check(
        "unscoped-nested-artist-gains-every-album",
        albumsOf(nestedArtists(afterArtistDelta, null), "Miles"),
        listOf("Crossover", "Kind of Blue", "Music Label", "No Genre", "On the Corner", "Untitled"),
    )

    // A user genre spelled "Music" is still that genre, not the Music row.
    check(
        "genre-named-Music-does-not-widen-to-the-whole-kind",
        albumsOf(nestedArtists(afterArtistDelta, BROWSE_ALL_MUSIC_TITLE), "Miles"),
        listOf("Music Label"),
    )

    check(
        "unicode-genre-nested-artist",
        albumsOf(nestedArtists(listOf(unicode, jazzA), "アニメ"), "LiSA"),
        listOf("Gurenge"),
    )
    check(
        "punctuation-genre-nested-artist",
        albumsOf(nestedArtists(listOf(rnb, jazzA), "R&B"), "Frank"),
        listOf("Channel Orange"),
    )
    check(
        "unknown-genre-nested-artist-is-empty",
        albumsOf(nestedArtists(afterArtistDelta, "Western"), "Miles"),
        emptyList(),
    )
    check(
        "empty-catalog-nested-artist",
        albumsOf(nestedArtists(emptyList(), "Jazz"), "Miles"),
        emptyList(),
    )

    // In-genre arrival after open: a second Jazz album must still appear.
    val beforeSecondJazz = listOf(jazzA)
    check(
        "before-second-in-genre-album",
        albumsOf(nestedArtists(beforeSecondJazz, "Jazz"), "Miles"),
        listOf("Kind of Blue"),
    )
    val afterSecondJazz = listOf(jazzA, track("jazz-live", "Miles", "Live at the Plugged Nickel", "Jazz"))
    check(
        "delta-adds-in-genre-album",
        albumsOf(nestedArtists(afterSecondJazz, "Jazz"), "Miles"),
        listOf("Kind of Blue", "Live at the Plugged Nickel"),
    )

    val crimeS1 = episode("wire-s1e1", "The Wire", "Crime")
    val crimeS2 = episode("wire-s2e1", "The Wire", "Crime", season = 2, episode = 1)
    val comedyS1 = episode("wire-comedy", "The Wire", "Comedy", season = 3, episode = 1)
    val friends = episode("friends-s1e1", "Friends", "Comedy")
    val crimeExtra = episode("wire-s0", "The Wire", "Crime", season = 0, episode = 1)
    val extrasOnly = episode("bts", "Behind the Scenes", "Crime", season = 0, episode = 1)
    val showsNamedGenre = episode("in-shows", "Selected", "Shows")
    val dramaOfSameShow = episode("wire-drama", "The Wire", "Drama", season = 4, episode = 1)

    val afterShowDelta = listOf(
        crimeS1, crimeS2, comedyS1, friends, crimeExtra, extrasOnly, showsNamedGenre, dramaOfSameShow,
    )

    check(
        "genre-nested-show-does-not-gain-other-genre-episodes",
        episodesOf(nestedShows(afterShowDelta, "Crime"), "The Wire"),
        listOf("wire-s0", "wire-s1e1", "wire-s2e1"),
    )
    check(
        "genre-nested-show-keeps-same-genre-extras-under-the-show",
        episodesOf(nestedShows(afterShowDelta, "Crime"), "The Wire"),
        listOf("wire-s0", "wire-s1e1", "wire-s2e1"),
    )
    check(
        "extras-only-other-title-is-not-a-sibling-show-on-genre-delta",
        showsNamed(nestedShows(afterShowDelta, "Crime")),
        listOf("The Wire"),
    )
    check(
        "unscoped-nested-show-gains-every-episode",
        episodesOf(nestedShows(afterShowDelta, null), "The Wire"),
        listOf("wire-s0", "wire-s1e1", "wire-s2e1", "wire-comedy", "wire-drama"),
    )
    check(
        "genre-named-Shows-does-not-widen-to-the-whole-kind",
        showsNamed(nestedShows(afterShowDelta, BROWSE_ALL_SHOWS_TITLE)),
        listOf("Selected"),
    )

    // Split-genre rescue: Drama numbered season must not pull Crime extras
    // onto a Crime nested show, and must not keep an extras-only Crime title.
    val split = listOf(
        episode("split-extra", "Split", "Crime", season = 0, episode = 1),
        episode("split-real", "Split", "Drama", season = 1, episode = 1),
    )
    check(
        "other-genre-numbered-season-does-not-keep-this-genre-extras",
        showsNamed(nestedShows(split, "Crime")),
        emptyList(),
    )
    check(
        "drama-nested-show-keeps-the-numbered-season",
        episodesOf(nestedShows(split, "Drama"), "Split"),
        listOf("split-real"),
    )

    val beforeCrimeDelta = listOf(crimeS1, friends)
    check(
        "before-crime-season-delta",
        episodesOf(nestedShows(beforeCrimeDelta, "Crime"), "The Wire"),
        listOf("wire-s1e1"),
    )
    check(
        "delta-adds-in-genre-season",
        episodesOf(nestedShows(listOf(crimeS1, crimeS2, friends), "Crime"), "The Wire"),
        listOf("wire-s1e1", "wire-s2e1"),
    )

    val merged = listOf(
        episode(
            "svu-s1",
            "Law & Order SVU",
            "Crime",
            scrapedTitle = "Law & Order: Special Victims Unit",
        ),
        episode(
            "svu-comedy",
            "Law & Order Special Victims Unit",
            "Comedy",
            scrapedTitle = "Law & Order: Special Victims Unit",
        ),
    )
    check(
        "canonical-merge-stays-on-selected-genre",
        episodesOf(nestedShows(merged, "Crime"), "Law & Order: Special Victims Unit"),
        listOf("svu-s1"),
    )

    if (failures.isNotEmpty()) {
        System.err.println(failures.joinToString("; "))
        exitProcess(1)
    }
    println("ALL_OK")
}
