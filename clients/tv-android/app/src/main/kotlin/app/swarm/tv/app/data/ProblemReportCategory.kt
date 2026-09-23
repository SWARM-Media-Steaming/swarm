package app.swarm.tv.app.data

/**
 * The concise, viewer-selectable context attached to a manual asset report.
 *
 * Keep these labels stable: they are shown on TV and included verbatim in the
 * server-side client-error queue, where they let a media-server operator sort
 * reports without having to infer the affected part of playback from a vague
 * generic message.
 */
enum class ProblemReportCategory(val label: String) {
    PLAYBACK_VIDEO("Playback Video"),
    PLAYBACK_AUDIO("Playback Audio"),
    ARTWORK("Artwork"),
    CONTENT("Content"),
    LANGUAGE("Language"),
    SUBTITLE("Subtitle"),
    ;

    fun reportMessage(surface: String): String =
        "User reported a $label problem from the $surface."
}
