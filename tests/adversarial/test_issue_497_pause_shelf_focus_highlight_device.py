#!/usr/bin/env python3
"""Adversarial real-device UAT for Issue #497 ("More like this" focus highlight).

Issue #497 (verbatim):
  "When "more like this" asset is selected you cannot see that anything is
  highlighted when it is selected. I want the asset that is in focus to be more
  deliberately focused and highlighted so it is clear which one is selected."

Trusted amendment (verbatim):
  "When a user navigates to "more like this" it should highlight the movie/show
  that is in focus."

What this drives, on a real Fire TV over adb (no instrumentation, no product
hooks):
  1. Builds and installs the current debug APK (skip with
     SWARM_ISSUE497_SKIP_INSTALL=1), cold-starts the app and walks the D-pad from
     the landing shelf into playback, then pauses to open the pause screen.
  2. Calibrates the accent-colour detector on the focused Resume button
     (SwarmAccent #00C2FF fill), so a detector problem can never masquerade as a
     product defect.
  3. Presses D-pad Down into "More like this" and requires exactly one
     recommendation card to carry a visible accent highlight ring.
  4. Presses D-pad Right and requires the highlight to move to the next card.

Exit codes: 0 pass, 1 assertion failure (product defect), 2 missing
prerequisite (no device, app not paired, no recommendations shown, ...).
Screenshots and UI dumps for every step are kept in a temp dir printed on exit.
"""

from __future__ import annotations

import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
ANDROID_DIR = ROOT / "clients/tv-android"
PACKAGE = "app.swarm.tv"
ACTIVITY = f"{PACKAGE}/app.swarm.tv.app.MainActivity"
EVIDENCE = Path(tempfile.mkdtemp(prefix="issue497-shelf-focus-"))

# SwarmAccent = Color(0xFF00C2FF) (ui/theme/Theme.kt). Tolerant band so the
# glow's anti-aliased edge and display scaling still count.
def is_accent(r: int, g: int, b: int) -> bool:
    return r <= 80 and 140 <= g <= 235 and b >= 190 and b - r >= 120


def log(msg: str) -> None:
    print(msg, flush=True)


def finish(code: int, msg: str) -> None:
    stream = sys.stdout if code == 0 else sys.stderr
    print(msg, file=stream, flush=True)
    print(f"evidence: {EVIDENCE}", flush=True)
    sys.exit(code)


def adb_path() -> str:
    for cand in (
        os.environ.get("ADB"),
        str(Path(os.environ.get("ANDROID_HOME", str(Path.home() / "Library/Android/sdk"))) / "platform-tools/adb"),
        shutil.which("adb"),
    ):
        if cand and Path(cand).exists():
            return cand
    finish(2, "PREREQ: adb not found (set ADB or ANDROID_HOME)")
    raise AssertionError


ADB = adb_path()


def run(args: list[str], timeout: float = 60, binary: bool = False, check: bool = True):
    res = subprocess.run(args, capture_output=True, timeout=timeout)
    if check and res.returncode != 0:
        finish(2, f"PREREQ: command failed ({res.returncode}): {' '.join(args)}\n{res.stderr.decode(errors='replace')}")
    return res.stdout if binary else res.stdout.decode(errors="replace")


def resolve_serial() -> str:
    for key in ("SWARM_FIRE_TV_SERIAL", "FIRE_TV_SERIAL", "fireTvSerial", "ANDROID_SERIAL"):
        val = os.environ.get(key, "").strip()
        if val:
            if re.fullmatch(r"\d+\.\d+\.\d+\.\d+", val):
                val += ":5555"
            if ":" in val:
                run([ADB, "connect", val], check=False)
            return val
    devices = [
        line.split()[0]
        for line in run([ADB, "devices"]).splitlines()[1:]
        if line.strip().endswith("device")
    ]
    amazon = [
        s for s in devices
        if "amazon" in run([ADB, "-s", s, "shell", "getprop", "ro.product.manufacturer"], check=False).lower()
    ]
    if len(amazon) != 1:
        finish(2, f"PREREQ: need exactly one attached Fire TV (or SWARM_FIRE_TV_SERIAL); found {amazon or 'none'}")
    return amazon[0]


SERIAL = resolve_serial()


def adb(*args: str, timeout: float = 60, binary: bool = False, check: bool = True):
    return run([ADB, "-s", SERIAL, *args], timeout=timeout, binary=binary, check=check)


def key(name: str, settle: float = 1.8) -> None:
    adb("shell", "input", "keyevent", name)
    time.sleep(settle)


# --- UI-tree helpers ------------------------------------------------------

NODE_RE = re.compile(r"<node [^>]*>")
BOUNDS_RE = re.compile(r'bounds="\[(\d+),(\d+)\]\[(\d+),(\d+)\]"')


class Node:
    def __init__(self, raw: str):
        m = BOUNDS_RE.search(raw)
        self.l, self.t, self.r, self.b = (int(x) for x in m.groups()) if m else (0, 0, 0, 0)
        self.focusable = 'focusable="true"' in raw
        self.focused = 'focused="true"' in raw

    @property
    def w(self) -> int:
        return self.r - self.l

    @property
    def h(self) -> int:
        return self.b - self.t

    def __repr__(self) -> str:
        return f"[{self.l},{self.t}][{self.r},{self.b}]{'*' if self.focused else ''}"


_dump_seq = 0


def ui_nodes(label: str) -> list[Node]:
    global _dump_seq
    _dump_seq += 1
    remote = f"/sdcard/issue497_{os.getpid()}.xml"
    for _ in range(3):
        adb("shell", "uiautomator", "dump", remote, timeout=40, check=False)
        xml = adb("exec-out", "cat", remote, check=False)
        if "<hierarchy" in xml:
            break
        time.sleep(1)
    adb("shell", "rm", "-f", remote, check=False)
    (EVIDENCE / f"{_dump_seq:02d}-{label}.xml").write_text(xml)
    return [n for n in (Node(raw) for raw in NODE_RE.findall(xml)) if n.focusable]


def screen(label: str) -> tuple[int, int, bytes]:
    data = adb("exec-out", "screencap", timeout=40, binary=True)
    w = int.from_bytes(data[0:4], "little")
    h = int.from_bytes(data[4:8], "little")
    header = len(data) - w * h * 4
    if header not in (12, 16):
        finish(2, f"PREREQ: unexpected raw screencap layout ({len(data)} bytes for {w}x{h})")
    pixels = data[header:]
    try:
        png = adb("exec-out", "screencap", "-p", timeout=40, binary=True, check=False)
        (EVIDENCE / f"{_dump_seq:02d}-{label}.png").write_bytes(png)
    except Exception:  # evidence only
        pass
    return w, h, pixels


def accent_count(img: tuple[int, int, bytes], l: int, t: int, r: int, b: int,
                 hole: tuple[int, int, int, int] | None = None) -> int:
    w, h, px = img
    l, t, r, b = max(0, l), max(0, t), min(w, r), min(h, b)
    count = 0
    for y in range(t, b):
        row = y * w * 4
        for x in range(l, r):
            if hole and hole[0] <= x < hole[2] and hole[1] <= y < hole[3]:
                continue
            i = row + x * 4
            if is_accent(px[i], px[i + 1], px[i + 2]):
                count += 1
    return count


def ring_count(img, card: Node) -> int:
    # Outer box leaves room for the 1.12x lift (~13px x, ~19px y at 1080p) plus
    # the 3dp border; the hole drops the poster art so artwork colours
    # never count as a highlight.
    pad = 34
    inset = 10
    return accent_count(
        img, card.l - pad, card.t - pad, card.r + pad, card.b + pad,
        hole=(card.l + inset, card.t + inset, card.r - inset, card.b - inset),
    )


# --- Pause-screen detection ----------------------------------------------

def pause_screen(nodes: list[Node]) -> tuple[list[Node], list[Node], list[Node]] | None:
    """(top buttons, dropdowns, recommendation cards) when the pause screen is up."""
    cards = sorted((n for n in nodes if n.t >= 560 and n.h > n.w * 1.2), key=lambda n: n.l)
    dropdowns = [n for n in nodes if n.l >= 1100 and n.w > n.h * 3 and n.t < 640]
    buttons = sorted((n for n in nodes if n.l < 1100 and n.w > n.h and n.t < 640), key=lambda n: n.l)
    if len(dropdowns) >= 3 and buttons:
        return buttons, dropdowns, cards
    return None


def install_build() -> None:
    if os.environ.get("SWARM_ISSUE497_SKIP_INSTALL") == "1":
        log("skip install (SWARM_ISSUE497_SKIP_INSTALL=1)")
        return
    env = dict(os.environ)
    env.setdefault("JAVA_HOME", "/opt/homebrew/opt/openjdk@17")
    log("building debug APK ...")
    res = subprocess.run(
        [str(ANDROID_DIR / "gradlew"), "-p", str(ANDROID_DIR), ":app:assembleDebug", "-q"],
        cwd=ROOT, env=env, capture_output=True, timeout=1200,
    )
    if res.returncode != 0:
        finish(2, "PREREQ: assembleDebug failed\n" + res.stdout.decode(errors="replace")[-4000:]
               + res.stderr.decode(errors="replace")[-4000:])
    abis = adb("shell", "getprop", "ro.product.cpu.abilist").strip().split(",")
    apk_dir = ANDROID_DIR / "app/build/outputs/apk/debug"
    apk = next((apk_dir / f"app-{abi}-debug.apk" for abi in abis if (apk_dir / f"app-{abi}-debug.apk").exists()), None)
    apk = apk or next(iter(sorted(apk_dir.glob("*-debug.apk"))), None)
    if apk is None:
        finish(2, f"PREREQ: no debug APK under {apk_dir}")
    log(f"installing {apk.name} on {SERIAL} ...")
    out = adb("install", "-r", str(apk), timeout=300, check=False)
    if "Success" not in out:
        finish(2, f"PREREQ: install failed: {out}")


def reach_pause_screen():
    adb("shell", "am", "force-stop", PACKAGE)
    adb("shell", "am", "start", "-n", ACTIVITY)
    deadline = time.time() + 120
    while time.time() < deadline:
        time.sleep(3)
        if len(ui_nodes("launch")) >= 10:
            break
    else:
        finish(2, "PREREQ: catalog never loaded (is the TV paired with a running media server?)")

    for step in range(14):
        nodes = ui_nodes(f"route{step}")
        ps = pause_screen(nodes)
        if ps:
            return ps
        focused = next((n for n in nodes if n.focused), None)
        if focused is None:
            key("DPAD_CENTER", 3)  # playing video: Select pauses
        elif focused.h > focused.w * 1.2:
            key("DPAD_CENTER", 6)  # poster: open detail / resume
        elif focused.t > 300:
            key("DPAD_CENTER", 15)  # detail-page Play: start playback
        else:
            key("DPAD_DOWN", 2)  # tabs / genre chips: move to a poster
    finish(2, "PREREQ: could not reach the pause screen by D-pad")


def main() -> None:
    log(f"device {SERIAL}")
    install_build()
    buttons, dropdowns, cards = reach_pause_screen()
    log(f"pause screen: buttons={buttons} dropdowns={len(dropdowns)} cards={cards}")
    if len(cards) < 2:
        finish(2, "PREREQ: pause screen shows fewer than two 'More like this' cards")
    focused_btn = next((n for n in buttons if n.focused), None)
    if focused_btn is None:
        finish(2, f"PREREQ: no focused top-row button on the pause screen ({buttons})")

    before = screen("paused-initial")
    calib = accent_count(before, focused_btn.l, focused_btn.t, focused_btn.r, focused_btn.b)
    area = focused_btn.w * focused_btn.h
    log(f"calibration: focused button accent pixels {calib}/{area}")
    if calib < area * 0.3:
        finish(2, "PREREQ: accent detector could not see the focused Resume button; display colour calibration differs")
    baseline = [ring_count(before, c) for c in cards]
    log(f"shelf ring accent counts before Down: {baseline}")

    failures: list[str] = []

    key("DPAD_DOWN", 2.5)
    after_down = screen("after-down")
    rings = [ring_count(after_down, c) for c in cards]
    log(f"shelf ring accent counts after Down: {rings}")
    threshold = 1500
    lit = [i for i, c in enumerate(rings) if c >= threshold]
    nodes = ui_nodes("after-down")
    if len(lit) != 1:
        failures.append(
            "after D-pad Down into 'More like this', expected exactly one recommendation card to show the "
            f"accent focus highlight (>= {threshold} accent px in its ring); got counts {rings} "
            f"(focused nodes: {[n for n in nodes if n.focused] or 'none'})"
        )
        down_idx = None
    else:
        down_idx = lit[0]
        log(f"ok highlight-after-down card={down_idx}")

    if down_idx is not None and down_idx + 1 < len(cards):
        key("DPAD_RIGHT", 2.5)
        after_right = screen("after-right")
        rings_r = [ring_count(after_right, c) for c in cards]
        log(f"shelf ring accent counts after Right: {rings_r}")
        lit_r = [i for i, c in enumerate(rings_r) if c >= threshold]
        if lit_r != [down_idx + 1]:
            failures.append(
                f"after D-pad Right the highlight should move from card {down_idx} to card {down_idx + 1}; "
                f"got lit cards {lit_r} with counts {rings_r}"
            )
        else:
            log("ok highlight-follows-right")

    key("BACK", 1)  # leave the player so the TV isn't left paused on this title
    if failures:
        finish(1, "FAIL: Issue #497 'More like this' focus highlight\n  - " + "\n  - ".join(failures))
    finish(0, "PASS: Issue #497 'More like this' card in focus is visibly highlighted on a real Fire TV")


if __name__ == "__main__":
    main()
