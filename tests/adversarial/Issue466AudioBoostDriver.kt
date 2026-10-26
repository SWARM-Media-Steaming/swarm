// Driver for adversarial-issue-466-audio-boost-contract. Compiled together with
// the *actual* audioBoostGainMillibels / audioBoostStatusFor / cap / unavailable
// constant (extracted verbatim from PlayerScreen.kt by the wrapper script) and
// SUPPORTED_LEVELS_DB (generated from AndroidAudioBoostStore.LEVELS), so this
// exercises the real mapping and status code rather than a stand-in.
//
// Issue #466 expectations, derived from the issue before reading the fix:
// - every level the viewer can pick maps to exactly dB*1000 millibels and never
//   exceeds the cap, so the label never promises more gain than is applied;
// - any level at or above the top supported level saturates at the cap rather
//   than wrapping (an Int overflow would silently turn a boost into 0 dB);
// - "Active" is only reported when a non-zero boost is really applied, so a
//   non-positive level (gain 0 mB) must never be reported as Active;
// - Off is reported as Off regardless of session or output path;
// - an unready session or passthrough output is reported as Unavailable, and
//   the failure constant is distinct from both Off and Active.

// Mirrors androidx.media3.common.C; AUDIO_SESSION_ID_UNSET is 0 in media3 1.5.0.
object C {
    const val AUDIO_SESSION_ID_UNSET = 0
}

fun main() {
    val failures = mutableListOf<String>()

    fun check(name: String, ok: Boolean, detail: String) {
        if (!ok) failures += "$name: $detail"
    }

    val cap = AUDIO_BOOST_MAX_MILLIBELS
    val levels = SUPPORTED_LEVELS_DB

    check(
        "levels start at Off and the top level fits under the cap",
        levels.firstOrNull() == 0 && levels.max() * 1000 <= cap,
        "levels=$levels cap=$cap",
    )

    for (level in levels) {
        check(
            "gain for offered level +$level dB is exactly $level*1000 mB",
            audioBoostGainMillibels(level) == level * 1000,
            "got ${audioBoostGainMillibels(level)}",
        )
    }
    for (pair in levels.zipWithNext()) {
        check(
            "gain is strictly increasing from +${pair.first} to +${pair.second} dB",
            audioBoostGainMillibels(pair.first) < audioBoostGainMillibels(pair.second),
            "gains ${audioBoostGainMillibels(pair.first)} and ${audioBoostGainMillibels(pair.second)}",
        )
    }

    for (huge in listOf(levels.max(), 11, 20, 1_000, 2_147_484, Int.MAX_VALUE)) {
        check(
            "levels at or above the top saturate at the cap (input +$huge dB)",
            audioBoostGainMillibels(huge) == cap,
            "got ${audioBoostGainMillibels(huge)} mB, expected $cap mB",
        )
    }
    for (negative in listOf(-1, -10, Int.MIN_VALUE)) {
        check(
            "negative level $negative yields no gain",
            audioBoostGainMillibels(negative) == 0,
            "got ${audioBoostGainMillibels(negative)} mB",
        )
    }

    for (session in listOf(1, 42, Int.MAX_VALUE)) {
        check(
            "Off is reported as Off on session $session",
            audioBoostStatusFor(0, session) == "Off",
            "got '${audioBoostStatusFor(0, session)}'",
        )
        check(
            "Off is reported as Off on session $session even when passthrough",
            audioBoostStatusFor(0, session, passthrough = true) == "Off",
            "got '${audioBoostStatusFor(0, session, passthrough = true)}'",
        )
        for (level in levels.filter { it > 0 }) {
            val status = audioBoostStatusFor(level, session)
            check(
                "decoded +$level dB on session $session reports Active",
                status.startsWith("Active"),
                "got '$status'",
            )
        }
    }

    for (session in listOf(C.AUDIO_SESSION_ID_UNSET, -1, Int.MIN_VALUE)) {
        for (level in levels.filter { it > 0 }) {
            val status = audioBoostStatusFor(level, session)
            check(
                "+$level dB with unready session $session is not reported Active",
                !status.startsWith("Active") && status.startsWith("Unavailable"),
                "got '$status'",
            )
        }
    }

    for (level in levels.filter { it > 0 }) {
        val status = audioBoostStatusFor(level, 42, passthrough = true)
        check(
            "+$level dB on passthrough output is reported Unavailable, not Active",
            status.startsWith("Unavailable") && !status.startsWith("Active"),
            "got '$status'",
        )
    }

    for (negative in listOf(-1, -10, Int.MIN_VALUE)) {
        val status = audioBoostStatusFor(negative, 42)
        check(
            "non-positive level $negative applies 0 mB, so it must not report Active",
            !status.startsWith("Active"),
            "got '$status' while gain is ${audioBoostGainMillibels(negative)} mB",
        )
    }

    check(
        "unavailable failure status is non-blank and distinct from Off and Active",
        AUDIO_BOOST_UNAVAILABLE_STATUS.isNotBlank() &&
            AUDIO_BOOST_UNAVAILABLE_STATUS != "Off" &&
            !AUDIO_BOOST_UNAVAILABLE_STATUS.startsWith("Active"),
        "got '$AUDIO_BOOST_UNAVAILABLE_STATUS'",
    )

    if (failures.isNotEmpty()) {
        failures.forEach { System.err.println("FAIL: $it") }
        kotlin.system.exitProcess(1)
    }
    println("ALL_OK")
}
