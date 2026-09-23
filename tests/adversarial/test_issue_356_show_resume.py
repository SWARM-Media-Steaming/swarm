#!/usr/bin/env python3
"""Issue #356: shows must resume overnight at the saved season/episode/progress.

Expected behavior, derived from the issue and watch-state invariants before
trusting the implementation:

1. Leaving a show mid-episode must persist season, episode, and position.
2. The next session must resume that same logical episode at that position,
   including after a replacement encode changes the content fingerprint.
3. An exact fingerprint match still wins over identity fallback.
4. A watched item (>= 95%) resumes from 0.
5. Overlapping heartbeat / background / teardown writes are timestamp-ordered
   so an older callback cannot roll progress backward.
6. Foreground loss snapshots progress while the player/episode are still live.
"""

from __future__ import annotations

import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
APP = ROOT / "clients/tv-android/app/src/main/kotlin/app/swarm/tv/app"
CORE = ROOT / "clients/tv-android/core"
WATCH = CORE / "src/main/kotlin/app/swarm/tv/core/watch/WatchState.kt"
STORE = APP / "data/AndroidWatchStateStore.kt"
VIEW_MODEL = APP / "data/SwarmViewModel.kt"
PLAYER = APP / "ui/screens/PlayerScreen.kt"
LIFECYCLE = APP / "PlaybackLifecycle.kt"
CATALOG = APP / "ui/screens/CatalogScreen.kt"
SEASON = APP / "ui/screens/SeasonScreen.kt"
ACTIVITY = APP / "MainActivity.kt"
DRIVER = Path(__file__).with_name("ShowResumeDriver.kt")
GRADLEW = ROOT / "clients/tv-android/gradlew"


def fail(message: str) -> None:
    print(f"FAIL: {message}", file=sys.stderr)
    sys.exit(1)


def read(path: Path) -> str:
    if not path.is_file():
        fail(f"missing {path}")
    return path.read_text()


def match_braces(source: str, open_at: int, opener: str = "{", closer: str = "}") -> int:
    depth = 0
    for index, char in enumerate(source[open_at:], open_at):
        if char == opener:
            depth += 1
        elif char == closer:
            depth -= 1
            if depth == 0:
                return index
    fail(f"unbalanced {opener}{closer} at {open_at}")
    raise AssertionError


def function_body(source: str, signature: str) -> str:
    start = source.find(signature)
    if start == -1:
        fail(f"missing `{signature}`")
    brace = source.find("{", start)
    if brace == -1:
        fail(f"`{signature}` has no body")
    return source[start : match_braces(source, brace) + 1]


def require(condition: bool, message: str) -> None:
    if not condition:
        fail(message)


def jars_named(name: str) -> list[str]:
    cache = Path.home() / ".gradle/caches/modules-2/files-2.1"
    return [
        str(path)
        for path in sorted(cache.glob(f"**/{name}-*.jar"))
        if "sources" not in path.name and "javadoc" not in path.name
    ]


def kotlin_compiler() -> list[str]:
    embedded = jars_named("kotlin-compiler-embeddable")
    if not embedded:
        fail("kotlin compiler is absent from the Gradle cache")
    extras: list[str] = []
    for name in (
        "kotlin-stdlib",
        "kotlin-reflect",
        "kotlin-script-runtime",
        "kotlinx-coroutines-core-jvm",
        "annotations",
        "trove4j",
    ):
        extras.extend(jars_named(name)[:3])
    return [
        "java",
        "-cp",
        ":".join([embedded[-1], *extras]),
        "org.jetbrains.kotlin.cli.jvm.K2JVMCompiler",
    ]


def core_classpath() -> str:
    result = subprocess.run(
        [str(GRADLEW), "-p", str(ROOT / "clients/tv-android"), ":core:compileKotlin", "-q"],
        cwd=ROOT,
        capture_output=True,
        text=True,
    )
    if result.returncode:
        fail(f"core compilation failed:\n{result.stdout}\n{result.stderr}")
    classes = CORE / "build/classes/kotlin/main"
    if not classes.is_dir():
        fail(f"core classes missing at {classes}")
    jars = [str(classes)]
    for name in (
        "kotlin-stdlib",
        "kotlin-stdlib-jdk8",
        "kotlin-stdlib-jdk7",
        "kotlinx-serialization-core-jvm",
        "kotlinx-serialization-json-jvm",
        "kotlinx-coroutines-core-jvm",
        "annotations",
    ):
        jars.extend(jars_named(name))
    return ":".join(dict.fromkeys(jars))


def assert_progress_identity_is_modeled() -> None:
    source = read(WATCH)
    for field in ("val showTitle: String? = null", "val season: Int? = null", "val episode: Int? = null"):
        require(field in source, f"WatchState must persist {field.split()[1]} for overnight show resume")
    from_playback = function_body(source, "fun fromPlayback(")
    require("showTitle" in from_playback and "season" in from_playback and "episode" in from_playback,
            "fromPlayback must snapshot show/season/episode with the position")
    require("WATCHED_FRACTION = 0.95" in source, "watched threshold must stay 95%")

    state_for = function_body(source, "fun Map<String, WatchState>.stateFor(")
    require("this[entry.fingerprint]" in state_for, "stateFor must prefer an exact fingerprint match")
    require("episodeIdentity" in state_for, "stateFor must fall back to show/season/episode identity")

    get_for_entry = function_body(source, "suspend fun WatchStateStore.getForEntry(")
    require("get(entry.fingerprint)" in get_for_entry, "getForEntry must keep fingerprint lookup first")
    require("all().values" in get_for_entry, "getForEntry must scan stored identity when the fingerprint is new")

    memory_set = function_body(source, "override suspend fun set(fingerprint: String, state: WatchState)")
    require("updatedAt" in memory_set and "return" in memory_set,
            "in-memory store must reject an older write")
    print("ok watch-state model captures identity, fingerprint-first lookup, and newest-write-wins")


def assert_save_path_snapshots_episode_identity() -> None:
    save = function_body(read(VIEW_MODEL), "fun savePlaybackPosition(")
    require("WatchState.fromPlayback(" in save, "savePlaybackPosition must go through fromPlayback")
    require("entry.entry.showTitle.takeIf { entry.entry.kind == MediaKind.EPISODE }" in save,
            "episode saves must snapshot show title; movies/tracks must not")
    require("entry.entry.season.takeIf { entry.entry.kind == MediaKind.EPISODE }" in save,
            "episode saves must snapshot season")
    require("entry.entry.episode.takeIf { entry.entry.kind == MediaKind.EPISODE }" in save,
            "episode saves must snapshot episode")
    require("lastWatchStateUpdatedAt + 1" in save,
            "saves sharing a wall-clock millisecond must still be strictly ordered")
    require("watchStateStore.set(fingerprint, saved)" in save,
            "progress must be written to the local watch-state store")

    model = read(VIEW_MODEL)
    play_at = model.find("val resumePositionSecs = startPositionSecsOverride")
    require(play_at != -1, "play() must compute a resume offset from saved watch state")
    play = model[play_at : play_at + 400]
    require("watchStateStore.getForEntry(entry.entry)" in play,
            "play() must resume via getForEntry so a replacement encode keeps the offset")
    require("takeUnless { it.watched }" in play, "play() must restart watched items at 0")

    next_ep = model[model.find("watchStateStore.getForEntry(next.entry)") :][:400]
    require("watchStateStore.getForEntry(next.entry)" in next_ep,
            "next-episode preload must resume via getForEntry")
    require("takeUnless { it.watched }" in next_ep, "next-episode preload must also skip watched offsets")

    load = model[model.find("val loaded = watchStateStore.all()") :][:900]
    require("maxBy { it.updatedAt }" in load,
            "startup must merge disk and live progress by newest updatedAt")
    require("lastWatchStateUpdatedAt" in load,
            "startup must advance the save clock past persisted timestamps")
    print("ok save/load/play paths capture identity and newest progress")


def assert_foreground_loss_flushes_progress() -> None:
    lifecycle = read(LIFECYCLE)
    pause = function_body(lifecycle, "if (event == Lifecycle.Event.ON_PAUSE)")
    require("latestOnBackgrounded.value?.invoke()" in pause,
            "ON_PAUSE must snapshot progress while the player/episode are still live")
    invoke_at = pause.find("latestOnBackgrounded.value?.invoke()")
    pause_at = pause.find("player?.pause()")
    require(invoke_at != -1 and pause_at != -1 and invoke_at < pause_at,
            "progress snapshot must run before the player is paused on background")

    player = read(PLAYER)
    require("PERIODIC_POSITION_SAVE_MS = 15_000L" in player,
            "active playback must still heartbeat progress every 15s")
    require("PausePlayerWhenAppBackgrounded(player)" in player, "the player screen must hook the background snapshot")
    require("onPositionUpdate(positionSecs, durationSecs)" in function_body(
        player, "PausePlayerWhenAppBackgrounded(player)"
    ), "background snapshot must persist the live position, not a stale heartbeat")

    activity = read(ACTIVITY)
    require("onSavePlaybackPosition(" in function_body(activity, "onPositionUpdate = { positionSecs, durationSecs ->"),
            "player position updates must reach savePlaybackPosition")
    print("ok foreground-loss and 15s heartbeat flush live show progress")


def assert_resume_surfaces_use_identity() -> None:
    season = read(SEASON)
    resume_at = season.find("internal fun resumeEpisode(")
    require(resume_at != -1, "season list must expose resumeEpisode")
    resume = season[resume_at : season.find("\nprivate fun ", resume_at)]
    require("watchStates.stateFor(episode.entry)" in resume,
            "season Resume must resolve progress by fingerprint then show/season/episode")
    require("saved.watched" in resume and "saved.positionSecs <= 0.0" in resume,
            "season Resume must ignore watched and zero-position records")
    require("maxByOrNull { it.second.updatedAt }" in resume,
            "season Resume must pick the most recently touched unfinished episode")

    catalog = read(CATALOG)
    require("private const val MAX_CONTINUE_WATCHING = 6" in catalog,
            "Continue Watching must stay capped at 6")
    require("watchStates.stateFor(entry.entry)" in catalog,
            "Continue Watching must use identity fallback after a replacement encode")
    require(".take(MAX_CONTINUE_WATCHING)" in catalog, "Continue Watching must apply the 6-item cap")
    print("ok Continue Watching and season Resume use identity-aware lookup")


def assert_android_store_rejects_stale_writes() -> None:
    store = read(STORE)
    setter = function_body(store, "override suspend fun set(fingerprint: String, state: WatchState)")
    require("writeMutex.withLock" in setter, "Android watch-state writes must be serialized")
    require("current.updatedAt > state.updatedAt" in setter,
            "Android store must drop an older async write")
    require("withCurrentCompletionRule" in function_body(store, "override suspend fun get(fingerprint: String)"),
            "reads must still re-evaluate the 95% watched rule")
    require(
        "WatchState.fromPlayback(positionSecs, durationSecs, updatedAt, showTitle, season, episode)"
        in store,
        "95% migration must preserve snapshotted show/season/episode",
    )
    print("ok Android store is newest-write-wins and keeps episode identity")


def assert_driver_executes_production_watch_state() -> None:
    classpath = core_classpath()
    with tempfile.TemporaryDirectory() as temp:
        out = Path(temp) / "classes"
        out.mkdir()
        compile_result = subprocess.run(
            kotlin_compiler()
            + [
                "-no-stdlib",
                "-no-reflect",
                "-cp",
                classpath,
                "-d",
                str(out),
                str(DRIVER),
            ],
            capture_output=True,
            text=True,
        )
        if compile_result.returncode:
            fail(
                "Show resume driver compilation failed:\n"
                f"{compile_result.stdout}\n{compile_result.stderr}"
            )
        run_result = subprocess.run(
            ["java", "-cp", f"{out}:{classpath}", "ShowResumeDriverKt"],
            capture_output=True,
            text=True,
        )
        if run_result.returncode or "ALL_OK" not in run_result.stdout:
            fail(
                "show resume UAT failed:\n"
                f"{run_result.stdout}\n{run_result.stderr}"
            )
        print(run_result.stdout.strip())


def main() -> None:
    assert_progress_identity_is_modeled()
    assert_save_path_snapshots_episode_identity()
    assert_foreground_loss_flushes_progress()
    assert_resume_surfaces_use_identity()
    assert_android_store_rejects_stale_writes()
    assert_driver_executes_production_watch_state()
    print("PASS: Issue #356 show resume season/episode/progress UAT")


if __name__ == "__main__":
    main()
