package app.swarm.tv.app.data

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

class ProblemReportCategoryTest {

    @Test
    fun `all viewer-facing problem categories are available to report`() {
        assertEquals(
            listOf("Playback Video", "Playback Audio", "Artwork", "Content", "Language", "Subtitle"),
            ProblemReportCategory.entries.map(ProblemReportCategory::label),
        )
    }

    @Test
    fun `report message preserves category and source context`() {
        val message = ProblemReportCategory.SUBTITLE.reportMessage("pause screen")

        assertTrue(message.contains("Subtitle"))
        assertTrue(message.contains("pause screen"))
    }
}
