// Minimal android.content stand-ins for adversarial-issue-466-audio-boost-persistence.
// They let the real AndroidAudioBoostStore.kt compile and run on the JVM without
// the Android SDK. Only the members the store touches are modelled, and each
// Context instance shares the backing file map it is given, so constructing a
// new Context over the same map simulates a process restart.
package android.content

class Context(
    private val files: MutableMap<String, MutableMap<String, Any>> = mutableMapOf(),
    private val app: Context? = null,
) {
    val applicationContext: Context get() = app ?: this

    fun getSharedPreferences(name: String, mode: Int): SharedPreferences =
        FakePreferences(files.getOrPut(name) { mutableMapOf() })

    companion object {
        const val MODE_PRIVATE = 0
    }
}

interface SharedPreferences {
    fun getInt(key: String, defValue: Int): Int
    fun edit(): SharedPreferences.Editor

    interface Editor {
        fun putInt(key: String, value: Int): SharedPreferences.Editor
        fun apply()
    }
}

class FakePreferences(private val values: MutableMap<String, Any>) : SharedPreferences {
    override fun getInt(key: String, defValue: Int): Int = values[key] as? Int ?: defValue

    override fun edit(): SharedPreferences.Editor = FakeEditor(values)
}

class FakeEditor(private val values: MutableMap<String, Any>) : SharedPreferences.Editor {
    private val staged = mutableMapOf<String, Any>()

    override fun putInt(key: String, value: Int): SharedPreferences.Editor {
        staged[key] = value
        return this
    }

    override fun apply() {
        values.putAll(staged)
    }
}
