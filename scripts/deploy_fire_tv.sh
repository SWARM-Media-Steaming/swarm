#!/usr/bin/env bash
# Rebuilds the Fire TV client, installs it on a real device over adb, and
# verifies it actually launches without crashing — automating the exact
# manual cycle that caught a real multidex ClassNotFoundException on real
# Fire OS hardware (SwarmTvApplication landed in classes11.dex; neither
# the emulator-less build nor any unit test could have caught it). Exits
# non-zero on any failure, so it doubles as a repeatable regression check
# after any change, not just a one-off convenience script.
#
# With no explicit target, this always scans the local /24 for Fire TVs (port
# 5555, manufacturer "Amazon"), lists every match, and prompts for which one(s)
# to deploy to. That scan only finds devices that already have
# ADB-over-network reachable — enable it once per device first: Settings ->
# My Fire TV -> Developer Options -> [ADB debugging / Network debugging] ON
# (some Fire OS versions require one initial `adb tcpip 5555` over USB first).
#
# Usage:
#   ./scripts/deploy_fire_tv.sh                # scans the LAN, lists Fire TVs, and prompts for the target(s)
#   ./scripts/deploy_fire_tv.sh 192.168.0.148   # connects to this IP first (find it: Settings -> My Fire TV -> About -> Network)
#   ./scripts/deploy_fire_tv.sh -f              # also tails logcat after a clean launch, until Ctrl+C (single target only)
#   ./scripts/deploy_fire_tv.sh -c              # uninstall first (wipes the device's saved STUN link/swarms/token) — see below
#
# Env vars:
#   SWARM_TV_IP     default target IP if none is passed as an argument (skips the LAN scan)
#   SWARM_TV_NAME   preferred device_name shown in the LAN choices; overrides the
#                   preferred_device_name in scripts/tests/tv_test_device.local.json
#                   (the same preferred-device config the closed-loop TV suites use)
#   ANDROID_HOME    default ~/Library/Android/sdk
#   JAVA_HOME       default /opt/homebrew/opt/openjdk@17
#   SWARM_LAN_IP    optional LAN-IP override shared with run_now.sh
#   SWARM_RENDEZVOUS_URL  SWARM service embedded in this debug build; when
#                         unset, this script uses this Mac's LAN IP on :8080

set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../clients/tv-android"

export ANDROID_HOME="${ANDROID_HOME:-$HOME/Library/Android/sdk}"
export JAVA_HOME="${JAVA_HOME:-/opt/homebrew/opt/openjdk@17}"
ADB="$ANDROID_HOME/platform-tools/adb"
GRADLEW="${SWARM_GRADLEW:-./gradlew}"
PACKAGE="app.swarm.tv"
# Fully qualified, not "$PACKAGE/.MainActivity": AGP namespace is
# app.swarm.tv but MainActivity's real Kotlin package is app.swarm.tv.app
# (one segment deeper) — a relative name here resolves against the
# namespace and points at a class that doesn't exist. See
# AndroidManifest.xml's comment on the activity element and the
# swarm-real-device-debugging skill for the full story.
ACTIVITY="$PACKAGE/app.swarm.tv.app.MainActivity"
ADB_PORT=5555

FOLLOW=0
CLEAN=0
TARGET=""
for arg in "$@"; do
    case "$arg" in
        -f|--follow) FOLLOW=1 ;;
        -c|--clean) CLEAN=1 ;;
        *) TARGET="$arg" ;;
    esac
done
TARGET="${TARGET:-${SWARM_TV_IP:-}}"

# The preferred device is highlighted in the interactive list, but no longer
# bypasses discovery or silently selects a target. An env override wins over
# the same scripts/tests/tv_test_device.local.json the closed-loop suites read.
PREFERRED_DEVICE_FILE="$(dirname "${BASH_SOURCE[0]}")/tests/tv_test_device.local.json"
PREFERRED_TV_NAME="${SWARM_TV_NAME:-}"
if [ -z "$PREFERRED_TV_NAME" ] && [ -f "$PREFERRED_DEVICE_FILE" ]; then
    PREFERRED_TV_NAME="$(grep -o '"preferred_device_name"[[:space:]]*:[[:space:]]*"[^"]*"' "$PREFERRED_DEVICE_FILE" \
        | sed 's/.*"preferred_device_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/')"
fi

# A debug deploy normally targets the local service started by run_now.sh.
# Embed its current LAN address so a Mac DHCP change does not leave the TV
# retrying an obsolete saved IP forever. An explicit environment value still
# wins for testing against a remote/public service.
if [ -z "${SWARM_RENDEZVOUS_URL:-}" ]; then
    host_ip="${SWARM_LAN_IP:-}"
    if [ -z "$host_ip" ]; then
        for iface in en0 en1; do
            host_ip="$(ipconfig getifaddr "$iface" 2>/dev/null || true)"
            [ -n "$host_ip" ] && break
        done
    fi
    [ -z "$host_ip" ] || export SWARM_RENDEZVOUS_URL="http://$host_ip:8080"
fi
if [ -n "${SWARM_RENDEZVOUS_URL:-}" ]; then
    echo "==> Debug SWARM service: $SWARM_RENDEZVOUS_URL"
else
    echo "==> No SWARM service URL detected; LAN-only discovery will still work."
fi

# Scans this Mac's /24 for hosts with adb's TCP port open, connects to each
# long enough to read ro.product.manufacturer, and keeps only the ones that
# say "Amazon" (real Fire TV hardware) — disconnecting everything else so
# the scan doesn't leave stray adb connections behind. Prints one
# "name<TAB>ip" pair per line to stdout.
inspect_fire_tv_candidates() {
    local candidates="$1" ip manufacturer name found connect_failed connect_out
    found=0
    connect_failed=0
    while IFS= read -r ip; do
        [ -n "$ip" ] || continue
        # `adb connect` exits 0 even when it fails ("failed to connect to ...:
        # No route to host"), so judge success by its output. Checking only the
        # exit status meant the stale-daemon retry below never fired, and the
        # scan reported "no Fire TVs" while three were reachable.
        connect_out="$("$ADB" connect "$ip:$ADB_PORT" 2>&1 || true)"
        if [[ "$connect_out" != *"connected to"* ]]; then
            connect_failed=1
            continue
        fi
        if [ "$("$ADB" -s "$ip:$ADB_PORT" get-state 2>/dev/null || true)" != "device" ]; then
            "$ADB" disconnect "$ip:$ADB_PORT" >/dev/null 2>&1 || true
            continue
        fi
        # </dev/null on every `adb shell` call here: without it, `adb shell`
        # reads from this loop's own input stream, so the first device can
        # silently swallow every remaining candidate.
        manufacturer="$("$ADB" -s "$ip:$ADB_PORT" shell getprop ro.product.manufacturer </dev/null 2>/dev/null | tr -d '\r')"
        if [[ "$manufacturer" != *[Aa]mazon* ]]; then
            "$ADB" disconnect "$ip:$ADB_PORT" >/dev/null 2>&1 || true
            continue
        fi
        name="$("$ADB" -s "$ip:$ADB_PORT" shell settings get global device_name </dev/null 2>/dev/null | tr -d '\r')"
        if [ -z "$name" ] || [ "$name" = "null" ]; then
            name="$("$ADB" -s "$ip:$ADB_PORT" shell getprop ro.product.model </dev/null 2>/dev/null | tr -d '\r')"
        fi
        printf '%s\t%s\n' "${name:-Fire TV}" "$ip"
        found=1
        "$ADB" disconnect "$ip:$ADB_PORT" >/dev/null 2>&1 || true
    done <<< "$candidates"

    [ "$found" -eq 1 ] && return 0
    # A reachable port with a failed `adb connect` usually means the shared
    # macOS adb daemon was launched from an app context that cannot use the
    # LAN. Let the caller restart it once from this terminal and retry.
    [ "$connect_failed" -eq 1 ] && return 2
    return 1
}

scan_lan_for_fire_tvs() {
    local iface subnet_ip prefix live_ips open_ips matches inspect_status
    subnet_ip=""
    for iface in en0 en1 eth0; do
        subnet_ip="$(ipconfig getifaddr "$iface" 2>/dev/null || true)"
        [ -n "$subnet_ip" ] && break
    done
    if [ -z "$subnet_ip" ]; then
        echo "Could not determine this Mac's LAN IPv4 address to scan." >&2
        return 1
    fi
    prefix="${subnet_ip%.*}"

    echo "==> Pinging $prefix.0/24 to find live hosts ..." >&2
    # A direct `nc -z -w1` port scan of all 254 addresses is unreliable as a
    # first pass: macOS's connect() to an *unassigned* local address can stall
    # well past nc's own -w timeout (ARP resolution, not the port, is what's
    # slow), so most of a /24 — where the large majority of addresses have no
    # host at all — takes far longer than the timeout implies (minutes, not
    # seconds, observed firsthand). `ping -t1` bounds each probe reliably, so
    # ping every address first and only nc-scan the handful that answer.
    # xargs exits non-zero whenever at least one probe fails, which is
    # expected here and would otherwise kill the script via this assignment
    # under `set -e`.
    live_ips="$(seq 1 254 | xargs -P 64 -I{} bash -c \
        'ping -c1 -t1 -q "$1.$2" >/dev/null 2>&1 && echo "$1.$2"' _ "$prefix" {} || true)"
    [ -n "$live_ips" ] || return 0

    echo "==> Checking live hosts for adb (port $ADB_PORT) ..." >&2
    # -G 1 (BSD/macOS connect timeout) is required, not just -w1: a host that
    # answers ping but silently drops SYNs to this port (seen firsthand on a
    # Wi-Fi device) made `nc -z -w1` block indefinitely, since -w doesn't bound
    # connect() on macOS — and one stuck probe stalls the whole xargs pool, so
    # the scan hung with no output and never reached the device list.
    open_ips="$(printf '%s\n' "$live_ips" | xargs -P 32 -I{} bash -c \
        'nc -z -G 1 -w1 "$1" '"$ADB_PORT"' 2>/dev/null && echo "$1"' _ {} | sort || true)"

    [ -n "$open_ips" ] || return 0
    inspect_status=0
    matches="$(inspect_fire_tv_candidates "$open_ips")" || inspect_status=$?
    if [ "$inspect_status" -eq 2 ]; then
        echo "==> ADB could not reach open TV ports; restarting its local daemon and retrying ..." >&2
        "$ADB" kill-server >/dev/null 2>&1 || true
        "$ADB" start-server >/dev/null 2>&1
        inspect_status=0
        matches="$(inspect_fire_tv_candidates "$open_ips")" || inspect_status=$?
    fi
    [ -n "$matches" ] && printf '%s\n' "$matches"
}

SERIALS=()
if [ -n "$TARGET" ]; then
    [[ "$TARGET" == *:* ]] || TARGET="$TARGET:$ADB_PORT"
    echo "==> Connecting to $TARGET ..."
    "$ADB" connect "$TARGET"
    SERIALS=("$TARGET")
fi

if [ -z "$TARGET" ] && [ "${#SERIALS[@]}" -eq 0 ]; then
    names=()
    ips=()
    while IFS=$'\t' read -r name ip; do
        [ -n "$ip" ] || continue
        names+=("$name")
        ips+=("$ip")
    done < <(scan_lan_for_fire_tvs)

    if [ "${#ips[@]}" -eq 0 ]; then
        echo "No device IP given, no \$SWARM_TV_IP set, and the LAN scan found no Amazon Fire TVs." >&2
        echo "Usage: $0 <fire-tv-ip>   (find it: Settings -> My Fire TV -> About -> Network)" >&2
        "$ADB" devices >&2
        exit 1
    fi

    # No implicit choice: even one match is listed and requires confirmation.
    echo "Found ${#ips[@]} Fire TV(s) on the LAN:"
    for i in "${!ips[@]}"; do
        preferred=""
        [ -n "$PREFERRED_TV_NAME" ] && [ "${names[$i]}" = "$PREFERRED_TV_NAME" ] && preferred=" (preferred)"
        printf '  %d) %s%s | %s\n' "$((i + 1))" "${names[$i]}" "$preferred" "${ips[$i]}"
    done
    read -rp "Deploy to which one(s)? [1-${#ips[@]}, space/comma-separated for multiple, or 'a' for all]: " choice

    # bash 3.2 has no associative arrays, so dedupe selections (e.g. "1,1")
    # by linear-scanning SERIALS before appending.
    add_serial() {
        local candidate="$1" existing
        # `${SERIALS[@]}` alone, under `set -u`, is a bash 3.2 bug
        # (macOS's default /bin/bash) that raises "unbound variable" for
        # a zero-length array even though SERIALS is legitimately
        # declared — `${SERIALS[@]:-}` sidesteps it. See run_now.sh's
        # cleanup() for the same issue.
        for existing in "${SERIALS[@]:-}"; do
            [ "$existing" = "$candidate" ] && return 0
        done
        SERIALS+=("$candidate")
    }

    if [[ "$choice" =~ ^[Aa](ll)?$ ]]; then
        for ip in "${ips[@]}"; do
            add_serial "$ip:$ADB_PORT"
        done
    else
        for tok in ${choice//,/ }; do
            if [[ "$tok" =~ ^[0-9]+$ ]] && [ "$tok" -ge 1 ] && [ "$tok" -le "${#ips[@]}" ]; then
                add_serial "${ips[$((tok - 1))]}:$ADB_PORT"
            else
                echo "Invalid choice: '$tok'" >&2
                exit 1
            fi
        done
    fi

    if [ "${#SERIALS[@]}" -eq 0 ]; then
        echo "No device selected." >&2
        exit 1
    fi

    for serial in "${SERIALS[@]}"; do
        "$ADB" connect "$serial" >/dev/null
    done
fi

# bash 3.2 (macOS's default /bin/bash) has no negative array indices, so the
# last serial is spelled out via its count instead of SERIALS[-1].
LAST_SERIAL="${SERIALS[$((${#SERIALS[@]} - 1))]}"
if [ "${#SERIALS[@]}" -gt 1 ] && [ "$FOLLOW" -eq 1 ]; then
    echo "==> -f/--follow only tails one device; it will follow $LAST_SERIAL once every device is deployed."
fi

echo "==> Building debug APK ..."
BUILD_LOG="$(mktemp -t swarm-fire-tv-build.XXXXXX)"
build_status=0
"$GRADLEW" :app:assembleDebug 2>&1 | tee "$BUILD_LOG" || build_status=$?
if [ "$build_status" -ne 0 ]; then
    if grep -Fq "is located outside the root directory" "$BUILD_LOG"; then
        echo "==> Gradle reused outputs from a different checkout path; cleaning generated Android build files and retrying once ..."
        "$GRADLEW" clean
        build_status=0
        "$GRADLEW" :app:assembleDebug 2>&1 | tee "$BUILD_LOG" || build_status=$?
    fi
fi
rm -f "$BUILD_LOG"
if [ "$build_status" -ne 0 ]; then
    echo "Debug APK build failed." >&2
    exit "$build_status"
fi

FAILED=()
for SERIAL in "${SERIALS[@]}"; do
    echo
    echo "==> Deploying to $SERIAL ..."

    STATE="$("$ADB" -s "$SERIAL" get-state 2>/dev/null || echo "unreachable")"
    if [ "$STATE" != "device" ]; then
        echo "Device $SERIAL is '$STATE', not ready." >&2
        if [ "$STATE" = "unauthorized" ]; then
            echo "Accept the \"Allow USB debugging?\" prompt on the TV screen, then re-run." >&2
        fi
        FAILED+=("$SERIAL")
        continue
    fi

    # Real bug, found live: an unconditional uninstall-before-install here wiped
    # the device's saved STUN link/swarm membership/access token (Room DB +
    # EncryptedSharedPreferences, both in the app's private data dir, both gone
    # on uninstall) on *every single redeploy* — reported as "the TV doesn't
    # save the STUN server/device name/SWARM info across resets/redeploy", which
    # was this script doing exactly that on every run, not a real persistence
    # bug in the app itself (AndroidConnectionStore/AndroidTokenStore round-trip
    # correctly otherwise). `gradlew installDebug` already does a plain
    # replace-in-place install (`adb install -r` semantics) on its own — no
    # uninstall needed for the normal case. -c/--clean opts back into the old
    # uninstall-first behavior for when that's actually wanted (recovering from
    # a genuinely corrupted install, or deliberately testing first-run
    # onboarding from a clean slate).
    if [ "$CLEAN" -eq 1 ]; then
        echo "==> --clean: uninstalling any previous install of $PACKAGE on $SERIAL (this wipes its saved STUN link/swarms/token) ..."
        "$ADB" -s "$SERIAL" uninstall "$PACKAGE" || true
    fi

    echo "==> Installing on $SERIAL (in place; add -c/--clean for a fresh uninstall first) ..."
    ANDROID_SERIAL="$SERIAL" "$GRADLEW" :app:installDebug

    echo "==> Force-stopping any previous run and clearing logcat ..."
    "$ADB" -s "$SERIAL" shell am force-stop "$PACKAGE"
    "$ADB" -s "$SERIAL" logcat -c

    echo "==> Launching $ACTIVITY ..."
    "$ADB" -s "$SERIAL" shell am start -n "$ACTIVITY"

    echo "==> Waiting to confirm it stays up (polling for 16s — a real crash on real hardware took >4s to surface once, so a single short sleep isn't trustworthy) ..."
    CRASH=""
    PID=""
    for _ in $(seq 1 8); do
        sleep 2
        CRASH="$("$ADB" -s "$SERIAL" logcat -d | grep "FATAL EXCEPTION" || true)"
        PID="$("$ADB" -s "$SERIAL" shell pidof "$PACKAGE" || true)"
        [ -n "$CRASH" ] && break
    done

    if [ -n "$CRASH" ] || [ -z "$PID" ]; then
        echo
        echo "FAILED: $PACKAGE did not stay running on $SERIAL." >&2
        echo "--- relevant logcat ---" >&2
        "$ADB" -s "$SERIAL" logcat -d | grep -A 25 "FATAL EXCEPTION" >&2 || "$ADB" -s "$SERIAL" logcat -d | tail -40 >&2
        FAILED+=("$SERIAL")
        continue
    fi

    echo
    echo "OK: $PACKAGE is running on $SERIAL (pid $PID)."

    if [ "$FOLLOW" -eq 1 ] && [ "$SERIAL" = "$LAST_SERIAL" ]; then
        echo "==> Tailing logcat on $SERIAL (Ctrl+C to stop) ..."
        "$ADB" -s "$SERIAL" logcat --pid="$PID"
    fi
done

if [ "${#FAILED[@]}" -gt 0 ]; then
    echo
    echo "FAILED on: ${FAILED[*]}" >&2
    exit 1
fi
