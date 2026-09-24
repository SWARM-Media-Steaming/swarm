#!/usr/bin/env python3
"""Issue #372: frozen TV UAT still waits for "Problem Report Sent".

Amended by a later adversarial pass (issue #407 delivery cycle) to resolve a
conflict with `test_issue_401_problem_report_picker_submit.py`: issue #401's
delivery commit (57aab69, "Frozen MovieProblemReportUatTest still one-clicks
Report a problem while the picker requires a second select") deliberately
changed the product so a `ProblemReportPicker` category must be D-pad-selected
before a report sends, and updated the real
`MovieProblemReportUatTest.submitReportAndAwaitResolve` to select one. That
postdates and supersedes this suite's original "no picker option" premise
(point 1 below, as originally written) — verified against the checked-in
`MovieProblemReportUatTest.kt`, which now selects a category tag between the
report button and `waitForText`. The one-click assertion was stale, not the
production code, so it is corrected here rather than left failing.

Expected behavior, derived from the issue and the locked MovieProblemReportUatTest
before trusting the product diff:

1. MovieProblemReportUatTest.submitReportAndAwaitResolve D-pad-selects
   MOVIE_DETAIL_REPORT_PROBLEM_BUTTON, waits for the ProblemReportPicker, D-pad
   selects exactly one PROBLEM_REPORT_OPTION_PREFIX-tagged category, and only
   then waitForText("Problem Report Sent") (issue #401).
2. UatTestBase.waitForText uses UIAutomator By.textContains, so the viewer-facing
   success notification must contain that exact contiguous substring.
3. Issue #354 still requires the selected category to reach the media server and
   to appear in the success toast. Prefixing the frozen substring must not drop
   the category label from the notify payload.
4. Every ProblemReportCategory label interpolated into the notify template must
   still match textContains("Problem Report Sent").
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
APP = ROOT / "clients/tv-android/app/src/main/kotlin/app/swarm/tv/app"
VIEW_MODEL = APP / "data/SwarmViewModel.kt"
CATEGORY = APP / "data/ProblemReportCategory.kt"
UAT = ROOT / "clients/tv-android/app/src/androidTest/kotlin/app/swarm/tv/app/uat"
MOVIE_UAT = UAT / "MovieProblemReportUatTest.kt"
UAT_BASE = UAT / "UatTestBase.kt"

FROZEN_SUBSTRING = "Problem Report Sent"
ISSUE_LABELS = [
    "Playback Video",
    "Playback Audio",
    "Artwork",
    "Content",
    "Language",
    "Subtitle",
]


def fail(message: str) -> None:
    print(f"FAIL: {message}", file=sys.stderr)
    sys.exit(1)


def read(path: Path) -> str:
    if not path.is_file():
        fail(f"missing {path}")
    return path.read_text()


def match_braces(source: str, open_at: int, opener: str, closer: str) -> int:
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
    end = match_braces(source, brace, "{", "}")
    return source[start : end + 1]


def assert_frozen_uat_selects_a_category_before_frozen_text() -> None:
    """Issue #401 made the picker's category selection mandatory before a
    report sends; the frozen UAT this suite protects must require that
    order, not the pre-#401 one-click contract."""
    source = read(MOVIE_UAT)
    body = function_body(source, "private fun submitReportAndAwaitResolve(")
    button = "selectTagWithDpad(UatTestTags.MOVIE_DETAIL_REPORT_PROBLEM_BUTTON)"
    picker = "waitForTag(UatTestTags.PROBLEM_REPORT_PICKER)"
    option_lookup = "firstTagStartingWith(UatTestTags.PROBLEM_REPORT_OPTION_PREFIX)"
    select_category = "selectTagWithDpad(categoryTag)"
    wait = f'waitForText("{FROZEN_SUBSTRING}")'

    button_at = body.find(button)
    picker_at = body.find(picker)
    option_at = body.find(option_lookup)
    select_at = body.find(select_category)
    wait_at = body.find(wait)

    if button_at == -1:
        fail("frozen UAT no longer D-pad-selects MOVIE_DETAIL_REPORT_PROBLEM_BUTTON")
    if picker_at == -1 or picker_at < button_at:
        fail("frozen UAT no longer waits for the problem-report picker after the report button (issue #401)")
    if option_at == -1 or option_at < picker_at:
        fail("frozen UAT no longer looks up a category option from the picker (issue #401)")
    if select_at == -1 or select_at < option_at:
        fail("frozen UAT no longer D-pad-selects the looked-up category before sending (issue #401)")
    if wait_at == -1:
        fail(
            "frozen UAT no longer waitForText("
            f"{FROZEN_SUBSTRING!r}); By.textContains would miss a renamed toast"
        )
    if wait_at < select_at:
        fail("frozen UAT waits for Problem Report Sent before selecting a category")
    between = body[select_at + len(select_category) : wait_at]
    if "selectTagWithDpad" in between:
        fail("frozen UAT inserted another D-pad select between the category choice and waitForText")
    if 'waitForText("Problem report sent")' in body or 'waitForText("problem report sent")' in body:
        fail("frozen UAT waitForText is case-sensitive; a lowercased needle is a different contract")
    print("ok frozen UAT selects a category (issue #401) then waitForText('Problem Report Sent')")


def assert_wait_for_text_is_text_contains() -> None:
    body = function_body(read(UAT_BASE), "protected fun waitForText(")
    if "By.textContains(text)" not in body:
        fail("waitForText no longer uses By.textContains; substring matching is the UAT contract")
    if "By.text(" in body.replace("By.textContains", ""):
        fail("waitForText switched to exact By.text, which would reject a prefixed toast")
    print("ok waitForText matches By.textContains, so a prefix/suffix around the frozen needle is visible")


def enum_labels() -> list[str]:
    source = read(CATEGORY)
    labels = re.findall(r'enum class ProblemReportCategory[\s\S]*?\n\}\n', source)
    if not labels:
        fail("could not parse ProblemReportCategory")
    found = re.findall(r'\("([^"]+)"\)', labels[0])
    if found != ISSUE_LABELS:
        fail(f"category labels drifted from the issue list: {found}")
    return found


def interpolate_notify(template: str, label: str) -> str:
    message = template
    for token in (
        "${category.label}",
        "$category.label",
        "${category.label}",
    ):
        message = message.replace(token, label)
    # Kotlin string templates like ${category.label} already handled; also
    # "$category.label" without braces.
    message = re.sub(r"\$\{category\.label\}", label, message)
    return message


def assert_success_notify_contains_frozen_substring_and_category() -> None:
    body = function_body(read(VIEW_MODEL), "fun reportAssetProblem(")
    if "category: ProblemReportCategory" not in body.split("{", 1)[0]:
        fail("reportAssetProblem dropped the selected category")
    if "category.reportMessage(" not in body:
        fail("reportAssetProblem no longer attaches the category to the server payload")
    if not re.search(r"reportClientError\s*\(", body):
        fail("reportAssetProblem never calls reportClientError")

    notify_match = re.search(
        r'notify\(\s*"([^"]+)"\s*,\s*ClientNotificationKind\.SUCCESS\s*\)',
        body,
    )
    if not notify_match:
        fail("reportAssetProblem has no SUCCESS notify with a string literal")
    template = notify_match.group(1)
    if "${category.label}" not in template and "$category.label" not in template:
        fail(f"success notify does not interpolate the category label: {template!r}")

    labels = enum_labels()
    for label in labels:
        rendered = interpolate_notify(template, label)
        if FROZEN_SUBSTRING not in rendered:
            fail(
                "SUCCESS notify for "
                f"{label!r} is {rendered!r}, which does not contain {FROZEN_SUBSTRING!r} "
                "(UIAutomator By.textContains would miss it)"
            )
        if label not in rendered:
            fail(f"SUCCESS notify dropped the category label {label!r}: {rendered!r}")
        # Boundary: a toast that only equals the frozen needle hides #354 triage.
        if rendered.strip() == FROZEN_SUBSTRING:
            fail("SUCCESS notify is only the frozen UAT needle; category triage is gone")
        # Boundary: case fold. textContains is case-sensitive.
        if FROZEN_SUBSTRING.lower() in rendered.lower() and FROZEN_SUBSTRING not in rendered:
            fail(f"SUCCESS notify has the frozen needle only with different case: {rendered!r}")

    # Malformed / empty interpolation must not be how production renders labels.
    empty = interpolate_notify(template, "")
    if empty == template.replace("${category.label}", "").replace("$category.label", ""):
        # Empty label is not a real enum value; just ensure the frozen needle remains.
        if FROZEN_SUBSTRING not in empty:
            fail("empty interpolated label would drop the frozen UAT substring")
    print("ok reportAssetProblem SUCCESS notify contains 'Problem Report Sent' and every category label")


def assert_early_return_does_not_claim_success() -> None:
    body = function_body(read(VIEW_MODEL), "fun reportAssetProblem(")
    # Missing catalog or device must return before notify, otherwise UAT would
    # see a false "sent" toast with no server report.
    before_notify = body.split("notify(", 1)[0]
    if "embeddedCatalog() ?: return" not in before_notify:
        fail("reportAssetProblem notifies success even when there is no catalog")
    if "?: return" not in before_notify.split("devices.find", 1)[-1][:200]:
        fail("reportAssetProblem notifies success even when the asset's device is missing")
    print("ok missing catalog/device returns before claiming Problem Report Sent")


def main() -> None:
    assert_frozen_uat_selects_a_category_before_frozen_text()
    assert_wait_for_text_is_text_contains()
    assert_success_notify_contains_frozen_substring_and_category()
    assert_early_return_does_not_claim_success()
    print("PASS: Issue #372 frozen UAT Problem Report Sent contract")


if __name__ == "__main__":
    main()
