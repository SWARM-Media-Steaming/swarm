#!/usr/bin/env python3
"""Issue #401: movie report UAT must complete the picker before claiming send.

The issue's contract is a two-step remote interaction: choosing "Report a
problem" only opens the modal; choosing one of its real category buttons is
what sends the report.  The UAT's checkpoint and success-toast assertion are
therefore meaningful only after that second D-pad Center event.  This suite
checks the production picker-to-screen wiring and the instrumented scenario
without relying on a Fire TV or on a pre-existing implementation claim.
"""

from __future__ import annotations

import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
APP = ROOT / "clients/tv-android/app/src"
MOVIE_UAT = APP / "androidTest/kotlin/app/swarm/tv/app/uat/MovieProblemReportUatTest.kt"
UAT_BASE = APP / "androidTest/kotlin/app/swarm/tv/app/uat/UatTestBase.kt"
TAGS = APP / "main/kotlin/app/swarm/tv/app/ui/UatTestTags.kt"
PICKER = APP / "main/kotlin/app/swarm/tv/app/ui/components/ProblemReportPicker.kt"
MOVIE_DETAIL = APP / "main/kotlin/app/swarm/tv/app/ui/screens/MovieDetailScreen.kt"


def fail(message: str) -> None:
    print(f"FAIL: {message}", file=sys.stderr)
    raise SystemExit(1)


def read(path: Path) -> str:
    if not path.is_file():
        fail(f"missing required source: {path}")
    return path.read_text()


def body(source: str, signature: str) -> str:
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
    fail(f"unbalanced function body for `{signature}`")
    raise AssertionError


def require_in_order(source: str, *needles: str) -> None:
    previous = -1
    for needle in needles:
        position = source.find(needle, previous + 1)
        if position < 0:
            fail(f"missing required step: {needle}")
        if position <= previous:
            fail(f"out-of-order required step: {needle}")
        previous = position


def assert_picker_is_a_second_action_not_a_one_click_send() -> None:
    picker = read(PICKER)
    detail = read(MOVIE_DETAIL)
    tags = read(TAGS)

    for constant in ("PROBLEM_REPORT_PICKER", "PROBLEM_REPORT_OPTION_PREFIX"):
        if f"const val {constant}" not in tags:
            fail(f"UAT tag contract dropped {constant}")
    if "onClick = { showProblemPicker = true }" not in detail:
        fail("movie Report a problem button does not merely open the picker")
    if "if (showProblemPicker)" not in detail or "ProblemReportPicker(" not in detail:
        fail("movie detail does not display the picker after the first select")
    if "onReport = { category ->" not in detail or "onReportProblem(entry, category)" not in detail:
        fail("selecting a picker category is no longer wired to report the movie problem")
    if "ProblemReportCategory.entries.forEachIndexed" not in picker:
        fail("picker no longer renders its production category set")
    if "onClick = { onReport(category) }" not in picker:
        fail("picker category buttons no longer invoke the selected-category callback")
    if ".testTag(UatTestTags.PROBLEM_REPORT_OPTION_PREFIX + category.name.lowercase())" not in picker:
        fail("picker categories do not expose deterministic prefix-tagged UAT targets")
    print("ok first select opens picker; a real category option supplies the sending action")


def assert_uat_selects_a_real_category_before_toast_and_checkpoint() -> None:
    scenario = body(read(MOVIE_UAT), "private fun submitReportAndAwaitResolve(")
    report_button = "selectTagWithDpad(UatTestTags.MOVIE_DETAIL_REPORT_PROBLEM_BUTTON)"
    picker = "waitForTag(UatTestTags.PROBLEM_REPORT_PICKER)"
    lookup = "composeTestRule.firstTagStartingWith(UatTestTags.PROBLEM_REPORT_OPTION_PREFIX)"
    select = "selectTagWithDpad(categoryTag)"
    toast = 'waitForText("Problem Report Sent")'
    checkpoint = "Log.i(\"UAT\", CHECKPOINT)"
    require_in_order(scenario, report_button, picker, lookup, select, toast, checkpoint)

    between_button_and_select = scenario[
        scenario.find(report_button) + len(report_button) : scenario.find(select)
    ]
    if "waitForText(" in between_button_and_select:
        fail("UAT claims a success toast before D-pad-selecting a problem category")
    lookup_statement = scenario[scenario.find(lookup) - 100 : scenario.find(lookup) + len(lookup) + 100]
    if "requireNotNull" not in lookup_statement:
        fail("missing category tag is silently tolerated instead of failing the UAT")
    print("ok UAT waits for picker, fails on no category target, selects one, then awaits send")


def assert_missing_toast_fails_instead_of_false_passing() -> None:
    waiter = body(read(UAT_BASE), "protected fun waitForText(")
    wait_call = "device.wait(Until.hasObject(By.textContains(text)), timeoutMs)"
    if wait_call not in waiter:
        fail("waitForText no longer waits for the UIAutomator text-contains target")
    if "check(" not in waiter or wait_call not in waiter[waiter.find("check(") :]:
        fail("waitForText ignores a missing toast instead of failing the instrumentation test")
    if "Timed out waiting" not in waiter:
        fail("waitForText failure omits timeout evidence needed to diagnose a missing send")
    print("ok missing success toast is an assertion failure, not an ignored UIAutomator timeout")


def main() -> None:
    assert_picker_is_a_second_action_not_a_one_click_send()
    assert_uat_selects_a_real_category_before_toast_and_checkpoint()
    assert_missing_toast_fails_instead_of_false_passing()
    print("PASS: Issue #401 movie problem report UAT completes the category picker")


if __name__ == "__main__":
    main()
