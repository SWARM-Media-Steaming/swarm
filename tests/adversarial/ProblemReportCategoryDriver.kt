import app.swarm.tv.app.data.ProblemReportCategory

/**
 * Executable contract for issue #354's viewer-facing report categories.
 *
 * The issue lists the popup options that must be sent back to the media
 * server as triage context. Labels are the operator-visible strings, not
 * the enum constant names.
 */
private val ISSUE_LABELS = listOf(
    "Playback Video",
    "Playback Audio",
    "Artwork",
    "Content",
    "Language",
    "Subtitle",
)

private val failures = mutableListOf<String>()

private fun check(name: String, ok: Boolean, detail: String = "") {
    if (ok) {
        println("ok $name")
    } else {
        val message = if (detail.isEmpty()) name else "$name: $detail"
        failures += message
        println("FAIL $message")
    }
}

fun main() {
    val labels = ProblemReportCategory.entries.map(ProblemReportCategory::label)
    check(
        "issue-354-category-labels-in-order",
        labels == ISSUE_LABELS,
        "got=$labels expected=$ISSUE_LABELS",
    )
    check("no-extra-categories", labels.size == ISSUE_LABELS.size, "count=${labels.size}")
    check("labels-are-unique", labels.toSet().size == labels.size, "labels=$labels")
    check(
        "labels-are-non-blank",
        labels.all { it.isNotBlank() && it == it.trim() },
        "labels=$labels",
    )

    val fireTvMessages = ProblemReportCategory.entries.map { it.reportMessage("Fire TV") }
    for (category in ProblemReportCategory.entries) {
        val message = category.reportMessage("Fire TV")
        check(
            "server-message-contains-${category.name}-label",
            category.label in message,
            "message=$message",
        )
        check(
            "server-message-does-not-use-enum-name-${category.name}",
            category.name !in message,
            "message=$message",
        )
        check(
            "server-message-is-non-blank-${category.name}",
            message.isNotBlank(),
            "message=$message",
        )
    }
    check(
        "each-category-produces-a-distinct-server-message",
        fireTvMessages.toSet().size == fireTvMessages.size,
        "messages=$fireTvMessages",
    )

    // Boundary / malformed surfaces: the selected issue still has to reach
    // the media server even if the caller passes an empty or odd surface.
    for (surface in listOf("", " ", "\n\t", "pause screen", "detail page")) {
        for (category in ProblemReportCategory.entries) {
            val message = category.reportMessage(surface)
            check(
                "label-survives-surface[${surface.hashCode()}]-${category.name}",
                category.label in message,
                "surface=${surface.replace("\n", "\\n").replace("\t", "\\t")} message=$message",
            )
        }
    }

    val emptySurface = ProblemReportCategory.CONTENT.reportMessage("")
    check(
        "empty-surface-still-names-the-category",
        "Content" in emptySurface,
        "message=$emptySurface",
    )

    if (failures.isNotEmpty()) {
        throw IllegalStateException("FAILED ${failures.size}: ${failures.joinToString("; ")}")
    }
    println("ALL_OK")
}
