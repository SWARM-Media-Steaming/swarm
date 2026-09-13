package app.swarm.tv.app.data

import app.swarm.tv.core.catalog.CatalogGrouping
import app.swarm.tv.core.catalog.MergedEntry
import app.swarm.tv.core.catalog.displayTitle
import app.swarm.tv.core.peer.CatalogEntry
import app.swarm.tv.core.peer.MediaKind
import kotlin.math.abs

private const val MAX_PAUSE_RECOMMENDATIONS = 10

/**
 * Builds the pause screen's "More like this" shelf from the catalog already
 * held on the TV. Shows contribute one representative episode so a series
 * with many files cannot crowd every movie and other show out of the row.
 */
internal fun pauseRecommendations(
    current: MergedEntry,
    entries: List<MergedEntry>,
    limit: Int = MAX_PAUSE_RECOMMENDATIONS,
): List<MergedEntry> {
    if (limit <= 0) return emptyList()

    val shows = CatalogGrouping.groupEpisodesByShowSeason(entries)
    val currentShow = shows.firstOrNull { show ->
        show.seasons.any { season -> season.episodes.any { it.fingerprint == current.fingerprint } }
    }
    val showRepresentatives = shows.mapNotNull { show ->
        if (show == currentShow) return@mapNotNull null
        show.seasons
            .asSequence()
            .flatMap { it.episodes.asSequence() }
            .firstOrNull()
    }
    val movies = entries.filter {
        it.entry.kind == MediaKind.MOVIE && it.entry.extraType == null && it.fingerprint != current.fingerprint
    }

    return (movies + showRepresentatives)
        .distinctBy { it.fingerprint }
        .sortedWith(
            compareByDescending<MergedEntry> { recommendationScore(current.entry, it.entry) }
                .thenByDescending { it.entry.communityRating ?: -1.0 }
                .thenBy { pauseRecommendationTitle(it).lowercase() },
        )
        .take(limit)
}

private fun recommendationScore(current: CatalogEntry, candidate: CatalogEntry): Int {
    val currentGenres = current.genres.mapTo(mutableSetOf()) { it.lowercase() }
    val sharedGenres = candidate.genres.count { it.lowercase() in currentGenres }
    val currentType = if (current.kind == MediaKind.MOVIE) MediaKind.MOVIE else MediaKind.EPISODE
    val candidateType = if (candidate.kind == MediaKind.MOVIE) MediaKind.MOVIE else MediaKind.EPISODE
    val sameType = if (currentType == candidateType) 20 else 0
    val nearbyYear = current.year?.let { year ->
        candidate.year?.let { (10 - abs(year - it)).coerceAtLeast(0) }
    } ?: 0
    return sharedGenres * 100 + sameType + nearbyYear
}

internal fun pauseRecommendationTitle(entry: MergedEntry): String = when (entry.entry.kind) {
    MediaKind.EPISODE -> entry.entry.scrapedTitle
        ?.takeIf(String::isNotBlank)
        ?: entry.entry.showTitle?.takeIf(String::isNotBlank)
        ?: entry.entry.displayTitle()
    else -> entry.entry.displayTitle()
}

internal fun episodeNumberLabel(entry: MergedEntry): String? {
    if (entry.entry.kind != MediaKind.EPISODE) return null
    val parts = listOfNotNull(
        entry.entry.season?.let { "Season $it" },
        entry.entry.episode?.let { "Episode $it" },
        entry.entry.episodeTitle?.takeIf(String::isNotBlank),
    )
    return parts.takeIf { it.isNotEmpty() }?.joinToString("  •  ")
}
