/**
 * The category (genre) tiles at the top of a Movies/Shows/Music page.
 *
 * Kept free of Compose so the ranking rule is unit-testable: categories are
 * ordered by how many *assets* carry them — a movie, a show, or an artist,
 * the same units the shelves below show — not by how many episodes or tracks
 * happen to be tagged, so a 200-episode show can't outrank a category
 * spanning many different shows.
 */
package app.swarm.tv.app.ui.screens

internal data class CategoryCount(val genre: String, val assets: Int)

/**
 * [assetGenres] holds one entry per asset (its genre tags, repeats and
 * blanks allowed). Returns each distinct genre with the number of assets
 * carrying it, most assets first, ties broken alphabetically so the row
 * never reshuffles between recompositions.
 */
internal fun rankCategories(assetGenres: List<Collection<String>>): List<CategoryCount> {
    val counts = HashMap<String, Int>()
    for (genres in assetGenres) {
        for (genre in genres.filter { it.isNotBlank() }.toSet()) {
            counts[genre] = (counts[genre] ?: 0) + 1
        }
    }
    return counts.map { (genre, assets) -> CategoryCount(genre, assets) }
        .sortedWith(compareByDescending<CategoryCount> { it.assets }.thenBy(String.CASE_INSENSITIVE_ORDER) { it.genre })
}
