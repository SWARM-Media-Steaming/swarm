// Driver for adversarial-issue-466-audio-boost-persistence. Compiled together with the
// real clients/tv-android .../data/AndroidAudioBoostStore.kt (unmodified) and the
// android.content stand-ins in Issue466AndroidContextStub.kt.
//
// Issue #466 expectations, derived from the issue before reading the store:
// - "The selected value persists across sessions and restarts": a value written
//   by one store instance is read back by a new store over the same preferences
//   after a simulated relaunch, for every level the viewer can pick;
// - a fresh install and any value that is not one of the offered levels read as
//   Off, so a corrupt preference can never create an unbounded or unoffered gain;
// - writes are normalised the same way, so the disk never holds an unoffered level;
// - the preference lives in its own file and is written through the application
//   context, so it is isolated from other prefs and cannot leak an Activity.

import android.content.Context
import app.swarm.tv.app.data.AndroidAudioBoostStore

const val PREFS_FILE = "swarm_audio_boost"
const val PREFS_KEY = "level_db"

fun main() {
    val failures = mutableListOf<String>()

    fun check(name: String, ok: Boolean, detail: String) {
        if (!ok) failures += "$name: $detail"
    }

    val levels = AndroidAudioBoostStore.LEVELS
    val unoffered = (-2..12).filter { it !in levels } + listOf(Int.MIN_VALUE, Int.MAX_VALUE)

    val fresh = mutableMapOf<String, MutableMap<String, Any>>()
    check(
        "fresh install reads as Off",
        AndroidAudioBoostStore(Context(fresh)).get() == 0,
        "got ${AndroidAudioBoostStore(Context(fresh)).get()}",
    )

    for (level in levels) {
        val files = mutableMapOf<String, MutableMap<String, Any>>()
        AndroidAudioBoostStore(Context(files)).set(level)
        val afterRelaunch = AndroidAudioBoostStore(Context(files)).get()
        check(
            "+$level dB survives a simulated relaunch",
            afterRelaunch == level,
            "got $afterRelaunch after restart",
        )
    }

    val sequence = mutableMapOf<String, MutableMap<String, Any>>()
    val store = AndroidAudioBoostStore(Context(sequence))
    for (level in listOf(3, 10, 0, 6)) store.set(level)
    check(
        "last written level wins across a relaunch",
        AndroidAudioBoostStore(Context(sequence)).get() == 6,
        "got ${AndroidAudioBoostStore(Context(sequence)).get()}",
    )

    for (bad in unoffered) {
        val files = mutableMapOf<String, MutableMap<String, Any>>()
        val store = AndroidAudioBoostStore(Context(files))
        store.set(6)
        store.set(bad)
        check(
            "set($bad) is normalised to Off on read",
            store.get() == 0,
            "got ${store.get()}",
        )
        check(
            "set($bad) never writes an unoffered value to disk",
            files[PREFS_FILE]?.get(PREFS_KEY) == 0,
            "stored ${files[PREFS_FILE]?.get(PREFS_KEY)}",
        )
    }

    for (raw in listOf(-1, 1, 5, 99, Int.MIN_VALUE, Int.MAX_VALUE)) {
        val files = mutableMapOf<String, MutableMap<String, Any>>(
            PREFS_FILE to mutableMapOf<String, Any>(PREFS_KEY to raw),
        )
        check(
            "corrupt stored value $raw reads as Off",
            AndroidAudioBoostStore(Context(files)).get() == 0,
            "got ${AndroidAudioBoostStore(Context(files)).get()}",
        )
    }

    val otherPrefs = mutableMapOf<String, MutableMap<String, Any>>(
        "other_prefs" to mutableMapOf<String, Any>(PREFS_KEY to 6),
    )
    check(
        "a same-named key in another prefs file is not read",
        AndroidAudioBoostStore(Context(otherPrefs)).get() == 0,
        "got ${AndroidAudioBoostStore(Context(otherPrefs)).get()}",
    )

    val written = mutableMapOf<String, MutableMap<String, Any>>()
    AndroidAudioBoostStore(Context(written)).set(3)
    check(
        "boost is written to the dedicated $PREFS_FILE file",
        written[PREFS_FILE]?.get(PREFS_KEY) == 3,
        "files=${written.keys}",
    )

    val appFiles = mutableMapOf<String, MutableMap<String, Any>>()
    val activityContext = Context(mutableMapOf(), app = Context(appFiles))
    AndroidAudioBoostStore(activityContext).set(10)
    check(
        "store writes through the application context, not the Activity",
        appFiles[PREFS_FILE]?.get(PREFS_KEY) == 10 && activityContext.applicationContext !== activityContext,
        "app files=${appFiles.keys}",
    )

    if (failures.isNotEmpty()) {
        failures.forEach { System.err.println("FAIL: $it") }
        kotlin.system.exitProcess(1)
    }
    println("ALL_OK")
}
