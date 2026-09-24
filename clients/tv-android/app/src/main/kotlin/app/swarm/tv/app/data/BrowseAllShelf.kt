/**
 * Titles and catalog-refresh filtering for the "Browse All" full grids (#353).
 * The originating Movies/Shows/Music or genre-sub-shelf name is the page
 * title; a live catalog delta must rebuild the same subset, not the whole
 * kind, or a genre grid would silently widen while still labeled Action.
 */
package app.swarm.tv.app.data

import app.swarm.tv.core.catalog.ArtistGroup
import app.swarm.tv.core.catalog.CatalogGrouping
import app.swarm.tv.core.catalog.MergedEntry
import app.swarm.tv.core.catalog.ShowGroup

internal const val BROWSE_ALL_MOVIES_TITLE = "Movies"
internal const val BROWSE_ALL_SHOWS_TITLE = "Shows"
internal const val BROWSE_ALL_MUSIC_TITLE = "Music"

/**
 * Rebuild a genre-scoped Browse All grid. [title] is the clicked shelf's
 * genre name, which may spell the same as the top-level Movies/Shows/Music
 * headings — those strings are not reserved, so membership is always the
 * genre match, never "the whole kind."
 */
internal fun moviesForBrowseAll(entries: List<MergedEntry>, title: String): List<MergedEntry> =
    CatalogGrouping.movies(entries).filter { it.entry.genres.contains(title) }

internal fun showsForBrowseAll(entries: List<MergedEntry>, title: String): List<ShowGroup> =
    CatalogGrouping.browsableShows(entries.filter { it.entry.genres.contains(title) })

internal fun artistsForBrowseAll(entries: List<MergedEntry>, title: String): List<ArtistGroup> =
    CatalogGrouping.groupTracksByArtistAlbum(entries.filter { it.entry.genres.contains(title) })
