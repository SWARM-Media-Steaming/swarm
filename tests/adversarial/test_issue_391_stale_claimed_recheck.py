#!/usr/bin/env python3
"""Issue #391 adversarial contract: stale claimed-session removal rechecks use.

Expected behavior is derived from the issue and stream-lifetime invariant, not
from the implementation: finding a claimed session idle while planning a new
/play does not grant permission to delete it.  Between candidate collection
and removal, a direct-media request or an HLS playlist/segment request can
acquire the manager lock, increment ``in_use``, and become a live stream.
That live stream must retain its session, owner, and claimed bookkeeping.

There is intentionally no timing-based server test here.  The production API
does not expose a scheduling hook between collecting candidates and acquiring
the removal lock, so sleep/retry races would be nondeterministic.  This test
instead verifies the production critical section that makes the precise
interleaving safe, and drives the transition with deterministic fixtures.  It
fails against the pre-#391 implementation, which collected ``in_use == 0``
candidates and passed each directly to ``remove_session`` without a second
guard.
"""

from __future__ import annotations

import sys
from dataclasses import dataclass
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
TRANSCODE = ROOT / "crates/swarm-media/src/transcode.rs"


def fail(message: str) -> None:
    print(f"FAIL: {message}", file=sys.stderr)
    raise SystemExit(1)


def function_body(source: str, signature: str) -> str:
    start = source.find(signature)
    if start < 0:
        fail(f"missing `{signature}`")
    open_brace = source.find("{", start)
    if open_brace < 0:
        fail(f"`{signature}` has no body")
    depth = 0
    for index in range(open_brace, len(source)):
        if source[index] == "{":
            depth += 1
        elif source[index] == "}":
            depth -= 1
            if depth == 0:
                return source[start : index + 1]
    fail(f"unbalanced braces in `{signature}`")
    raise AssertionError("unreachable")


def require(condition: bool, message: str) -> None:
    if not condition:
        fail(message)


def assert_production_removal_critical_section() -> None:
    source = TRANSCODE.read_text()
    cancel = function_body(
        source,
        "fn cancel_stale_claimed_for_owner(&self, owner: &str, negotiating_entry_key: &str)",
    )
    predicate = function_body(
        source,
        "fn is_stale_claimed_session_for_owner(",
    )
    remove = function_body(
        source,
        "fn remove_stale_claimed_session_for_owner(",
    )

    require(
        "Self::is_stale_claimed_session_for_owner" in cancel,
        "candidate collection must use the stale predicate rather than an unguarded owner scan",
    )
    require(
        "self.remove_stale_claimed_session_for_owner(&id, owner, negotiating_entry_key)" in cancel,
        "each collected candidate must enter the guarded removal path, not remove_session directly",
    )
    require(
        "state.owners.get(id).map(String::as_str) == Some(owner)" in predicate,
        "the recheck must retain the same-owner boundary",
    )
    require(
        "state.claimed.contains(id)" in predicate,
        "the recheck must retain the claimed-session boundary",
    )
    require("!session.track" in predicate, "track preloads must remain excluded")
    require(
        "session.in_use == 0" in predicate,
        "the removal-time predicate must reject a session opened after collection",
    )

    lock_at = remove.find("let mut state = self.state.lock().unwrap()")
    recheck_at = remove.find("if !Self::is_stale_claimed_session_for_owner")
    removal_at = remove.find("state.sessions.remove(id)")
    require(lock_at >= 0, "guarded removal must acquire the state lock")
    require(recheck_at > lock_at, "stale state must be rechecked after acquiring the removal lock")
    require(removal_at > recheck_at, "session removal must occur only after the locked recheck")
    require(
        "state.owners.remove(id)" in remove and "state.claimed.remove(id)" in remove,
        "a genuinely stale removal must clear matching owner and claimed bookkeeping",
    )
    print("ok production removal rechecks owner, claimed state, track, and in_use under the lock")


@dataclass
class Session:
    owner: str
    claimed: bool
    track: bool
    in_use: int
    entry_key: str
    last_release_clean: bool


def stale_for(owner: str, negotiating_entry_key: str, session: Session) -> bool:
    """The issue-derived removal predicate, used to drive fixed interleavings."""
    return (
        session.owner == owner
        and session.claimed
        and not session.track
        and session.in_use == 0
        and (session.entry_key != negotiating_entry_key or not session.last_release_clean)
    )


def guarded_remove(owner: str, negotiating_entry_key: str, session: Session) -> bool:
    """Model the removal-lock recheck; True means the session is removed."""
    return stale_for(owner, negotiating_entry_key, session)


def assert_collect_then_open_interleavings() -> None:
    # Different-title stale candidate: it is eligible when the scan takes its
    # snapshot, then a direct request opens it before removal acquires the
    # lock.  Its owner/claim bookkeeping must survive exactly intact.
    direct = Session("living-room", True, False, 0, "old-movie", True)
    require(stale_for("living-room", "new-movie", direct), "fixture must be stale at collection")
    direct.in_use += 1  # open_direct happens after collection, before remove.
    require(
        not guarded_remove("living-room", "new-movie", direct),
        "a direct request opened after collection must prevent stale removal",
    )
    require(
        (direct.owner, direct.claimed, direct.in_use) == ("living-room", True, 1),
        "blocked removal must preserve direct session ownership and claim state",
    )

    # HLS has the identical lifetime rule: a playlist or segment open makes
    # the claimed session active before the deletion lock is acquired.
    hls = Session("living-room", True, False, 0, "old-episode", False)
    require(stale_for("living-room", "new-episode", hls), "HLS fixture must be stale at collection")
    hls.in_use += 1  # open_hls happens after collection, before remove.
    require(
        not guarded_remove("living-room", "new-episode", hls),
        "an HLS request opened after collection must prevent stale removal",
    )

    # Control: a session that remained inactive is still reclaimed, so this
    # race fix cannot silently retire the orphan-recovery behavior.
    abandoned = Session("living-room", True, False, 0, "crashed-episode", False)
    require(
        guarded_remove("living-room", "retry-episode", abandoned),
        "a still-inactive stale claimed session must remain reclaimable",
    )

    # Boundaries that must not be relaxed while fixing the race.
    other_owner = Session("bedroom", True, False, 0, "old-movie", False)
    require(not guarded_remove("living-room", "new-movie", other_owner), "never remove another owner's session")
    track = Session("living-room", True, True, 0, "old-track", False)
    require(not guarded_remove("living-room", "new-movie", track), "never reap a gapless track preload")
    unclaimed = Session("living-room", False, False, 0, "old-movie", False)
    require(not guarded_remove("living-room", "new-movie", unclaimed), "claimed-session cleanup must not absorb unclaimed state")
    print("ok deterministic direct/HLS handoffs preserve active sessions and retain orphan cleanup")


def main() -> None:
    if not TRANSCODE.is_file():
        fail(f"missing production source: {TRANSCODE}")
    assert_production_removal_critical_section()
    assert_collect_then_open_interleavings()
    print("PASS: Issue #391 stale claimed-session removal recheck contract")


if __name__ == "__main__":
    main()
