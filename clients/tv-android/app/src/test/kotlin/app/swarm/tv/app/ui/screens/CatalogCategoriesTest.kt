package app.swarm.tv.app.ui.screens

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

/** [rankCategories] orders the category tiles that open every Movies/Shows/Music page (#324). */
class CatalogCategoriesTest {
    @Test
    fun `no assets yields no categories`() {
        assertTrue(rankCategories(emptyList()).isEmpty())
        assertTrue(rankCategories(listOf(emptyList(), emptyList())).isEmpty())
    }

    @Test
    fun `categories are ordered from most assets to fewest`() {
        val ranked = rankCategories(
            listOf(
                listOf("Drama"),
                listOf("Action", "Drama"),
                listOf("Drama", "Comedy"),
                listOf("Action"),
                listOf("Drama"),
            ),
        )
        assertEquals(
            listOf(CategoryCount("Drama", 4), CategoryCount("Action", 2), CategoryCount("Comedy", 1)),
            ranked,
        )
    }

    @Test
    fun `ties are broken alphabetically ignoring case`() {
        val ranked = rankCategories(listOf(listOf("thriller", "Comedy"), listOf("Action", "western")))
        assertEquals(listOf("Action", "Comedy", "thriller", "western"), ranked.map { it.genre })
    }

    @Test
    fun `an asset counts once per category even if the tag repeats`() {
        val ranked = rankCategories(listOf(listOf("Drama", "Drama", "Drama"), listOf("Comedy", "Comedy")))
        assertEquals(listOf(CategoryCount("Comedy", 1), CategoryCount("Drama", 1)), ranked)
    }

    @Test
    fun `blank tags are not categories`() {
        val ranked = rankCategories(listOf(listOf("", "  ", "Drama")))
        assertEquals(listOf(CategoryCount("Drama", 1)), ranked)
    }
}
