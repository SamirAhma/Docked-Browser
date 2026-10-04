#!/usr/bin/env python3
"""Count/threshold tests for weekday-hour suggestions. No D-Bus or Docker."""

from __future__ import annotations

import inspect
import subprocess
import tempfile
import unittest
from datetime import datetime, timedelta, timezone
from pathlib import Path

from habit_suggest import (
    MIN_WEEKS,
    WINDOW_WEEKS,
    clear_suggested,
    for_now_profile,
    log_use,
    mark_suggested,
    maybe_suggest,
    myt_datetime,
    notification_requests_open,
    panel_for_now,
    pick_suggestion,
    slot_winner,
    profile_from_wm_class,
    prune_habit,
    record_use,
    slot_key,
    spawn_activate,
    week_key,
)
from predict_sleep import MYT_OFFSET_SECS, PredictSleepDaemon, mark_used


def _monday(hour: int = 9) -> datetime:
    now = myt_datetime()
    monday = now - timedelta(days=now.isocalendar().weekday - 1)
    return monday.replace(hour=hour, minute=0, second=0, microsecond=0)


def _seed(data: dict, profile: str, base: datetime, weeks_ago: tuple[int, ...]) -> None:
    for n in weeks_ago:
        record_use(data, profile, base - timedelta(weeks=n))


class HabitSuggestTest(unittest.TestCase):
    def test_three_of_four_suggests_the_winner(self) -> None:
        data: dict = {}
        base = _monday()
        _seed(data, "work", base, (1, 2, 3))
        _seed(data, "blog", base, (1,))
        self.assertEqual(pick_suggestion(data, base, focused=""), "work")
        self.assertGreaterEqual(MIN_WEEKS, 3)
        self.assertEqual(WINDOW_WEEKS, 4)

    def test_two_of_four_stays_quiet(self) -> None:
        data: dict = {}
        base = _monday()
        _seed(data, "work", base, (1, 2))
        self.assertIsNone(pick_suggestion(data, base))

    def test_same_week_repeats_count_once(self) -> None:
        data: dict = {}
        base = _monday()
        for _ in range(5):
            record_use(data, "work", base - timedelta(weeks=1))
        self.assertIsNone(pick_suggestion(data, base))

    def test_week_outside_the_window_does_not_count(self) -> None:
        data: dict = {}
        base = _monday()
        _seed(data, "work", base, (2, 3, 4))
        self.assertIsNone(pick_suggestion(data, base))

    def test_current_week_counts(self) -> None:
        data: dict = {}
        base = _monday()
        _seed(data, "work", base, (0, 1, 2))
        self.assertEqual(pick_suggestion(data, base), "work")

    def test_tie_stays_quiet(self) -> None:
        data: dict = {}
        base = _monday()
        _seed(data, "work", base, (1, 2, 3))
        _seed(data, "blog", base, (1, 2, 3))
        self.assertIsNone(pick_suggestion(data, base))

    def test_strict_winner_beats_another_consistent_profile(self) -> None:
        data: dict = {}
        base = _monday()
        _seed(data, "work", base, (0, 1, 2, 3))
        _seed(data, "blog", base, (1, 2, 3))
        self.assertEqual(pick_suggestion(data, base), "work")

    def test_slot_winner_ignores_notification_skips(self) -> None:
        data: dict = {}
        base = _monday()
        _seed(data, "work", base, (1, 2, 3))
        mark_suggested(data, base)
        self.assertEqual(slot_winner(data, base), "work")
        self.assertEqual(for_now_profile(data, base), "work")
        self.assertIsNone(pick_suggestion(data, base, focused="work"))
        self.assertIsNone(pick_suggestion(data, base, focused=""))

    def test_for_now_hides_when_already_resumed(self) -> None:
        data: dict = {}
        base = _monday()
        _seed(data, "work", base, (0, 1, 2))
        self.assertEqual(for_now_profile(data, base, resumed=False), "work")
        self.assertIsNone(for_now_profile(data, base, resumed=True))

    def test_for_now_stays_quiet_without_a_unique_winner(self) -> None:
        data: dict = {}
        base = _monday()
        _seed(data, "work", base, (1, 2))
        self.assertIsNone(for_now_profile(data, base))
        _seed(data, "blog", base, (1, 2, 3))
        _seed(data, "mail", base, (1, 2, 3))
        self.assertIsNone(slot_winner(data, base))
        self.assertIsNone(for_now_profile(data, base, resumed=False))

    def test_for_now_does_not_substitute_a_runner_up(self) -> None:
        data: dict = {}
        base = _monday()
        _seed(data, "work", base, (0, 1, 2, 3))
        _seed(data, "blog", base, (1, 2, 3))
        self.assertEqual(for_now_profile(data, base, resumed=False), "work")
        self.assertIsNone(for_now_profile(data, base, resumed=True))

    def test_panel_for_now_reads_habit_and_hides_running(self) -> None:
        base = _monday()
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "habit.json"
            for n in (0, 1, 2):
                self.assertTrue(log_use("work", when=base - timedelta(weeks=n), path=path))
            before = path.read_text(encoding="utf-8")
            self.assertEqual(panel_for_now(set(), path=path, when=base), "work")
            self.assertEqual(panel_for_now({"blog"}, path=path, when=base), "work")
            self.assertIsNone(panel_for_now({"work"}, path=path, when=base))
            self.assertEqual(path.read_text(encoding="utf-8"), before)

    def test_already_focused_skips(self) -> None:
        data: dict = {}
        base = _monday()
        _seed(data, "Work", base, (1, 2, 3))
        self.assertIsNone(pick_suggestion(data, base, focused="work"))
        self.assertEqual(pick_suggestion(data, base, focused="blog"), "Work")

    def test_one_suggestion_per_slot_per_day(self) -> None:
        data: dict = {}
        base = _monday(9)
        later_hour = base.replace(hour=10)
        _seed(data, "work", base, (1, 2, 3))
        _seed(data, "work", later_hour, (1, 2, 3))
        self.assertEqual(pick_suggestion(data, base), "work")
        mark_suggested(data, base)
        self.assertIsNone(pick_suggestion(data, base))
        self.assertEqual(pick_suggestion(data, later_hour), "work")
        next_week = base + timedelta(weeks=1)
        _seed(data, "work", base, (0, 1, 2, 3))
        self.assertEqual(pick_suggestion(data, next_week), "work")

    def test_failed_notify_can_retry_same_slot(self) -> None:
        base = _monday()
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "habit.json"
            for n in (1, 2, 3):
                self.assertTrue(log_use("work", when=base - timedelta(weeks=n), path=path))
            calls: list[str] = []

            def fail(profile: str) -> bool:
                calls.append(profile)
                return False

            self.assertIsNone(maybe_suggest(focused="", path=path, notify=fail, when=base))
            self.assertEqual(calls, ["work"])
            self.assertEqual(maybe_suggest(focused="blog", path=path, notify=fail, when=base), None)

            def ok(profile: str) -> bool:
                calls.append(profile)
                return True

            self.assertEqual(maybe_suggest(focused="", path=path, notify=ok, when=base), "work")
            self.assertEqual(calls, ["work", "work", "work"])
            self.assertIsNone(maybe_suggest(focused="", path=path, notify=ok, when=base))
            self.assertEqual(calls, ["work", "work", "work"])
            saved = path.read_text(encoding="utf-8")
            self.assertIn("work", saved)
            self.assertIn(slot_key(base), saved)

    def test_clear_suggestion_keeps_the_use_log(self) -> None:
        data: dict = {}
        base = _monday()
        record_use(data, "work", base)
        mark_suggested(data, base)
        clear_suggested(data, base)
        self.assertIn("work", data["weeks"][week_key(base)][slot_key(base)])
        self.assertNotIn(base.date().isoformat(), data.get("suggested", {}))

    def test_log_use_rejects_bad_profile(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "habit.json"
            self.assertFalse(log_use("has space", path=path))
            self.assertFalse(log_use("../etc", path=path))
            self.assertFalse(path.exists())

    def test_prune_keeps_about_eight_weeks(self) -> None:
        base = _monday()
        data: dict = {"weeks": {}, "suggested": {}}
        record_use(data, "old", base - timedelta(weeks=10))
        record_use(data, "kept", base - timedelta(weeks=7))
        record_use(data, "new", base)
        old_day = (base.date() - timedelta(weeks=10)).isoformat()
        data["suggested"] = {old_day: ["1-09"], base.date().isoformat(): ["1-09"]}
        prune_habit(data, base)
        self.assertNotIn(week_key(base - timedelta(weeks=10)), data["weeks"])
        self.assertIn(week_key(base - timedelta(weeks=7)), data["weeks"])
        self.assertIn(week_key(base), data["weeks"])
        self.assertNotIn(old_day, data["suggested"])
        self.assertIn(base.date().isoformat(), data["suggested"])

    def test_myt_weekday_and_hour(self) -> None:
        utc = datetime(2026, 10, 4, 16, 30, tzinfo=timezone.utc)
        self.assertEqual(utc.isocalendar().weekday, 7)
        local = myt_datetime(utc.timestamp())
        self.assertEqual(local.hour, 0)
        self.assertEqual(local.isocalendar().weekday, 1)
        self.assertEqual(local.date().isoformat(), "2026-10-05")
        self.assertEqual(
            local,
            datetime.fromtimestamp(utc.timestamp() + MYT_OFFSET_SECS, tz=timezone.utc),
        )
        self.assertEqual(slot_key(local), "1-00")

    def test_wm_class(self) -> None:
        self.assertEqual(
            profile_from_wm_class('"docked-browser-work", "docked-browser-work"'),
            "work",
        )
        self.assertEqual(profile_from_wm_class("docked-browser-my_work"), "my_work")
        self.assertEqual(profile_from_wm_class('"google-chrome", "Google-chrome"'), "")
        self.assertEqual(profile_from_wm_class(""), "")

    def test_notification_click_is_not_dismiss_or_expiry(self) -> None:
        self.assertTrue(notification_requests_open("ActionInvoked", "default"))
        self.assertTrue(notification_requests_open("ActionInvoked", "open"))
        self.assertFalse(notification_requests_open("ActionInvoked", "keep"))
        self.assertTrue(notification_requests_open("ActivationToken", "token-1"))
        self.assertFalse(notification_requests_open("ActivationToken", ""))
        self.assertFalse(notification_requests_open("NotificationClosed", 1))
        self.assertFalse(notification_requests_open("NotificationClosed", 2))

    def test_spawn_activate_argv_returns_without_waiting(self) -> None:
        seen: dict = {}

        def fake_popen(argv, **kwargs):
            seen["argv"] = list(argv)
            seen["kwargs"] = kwargs

            class Proc:
                def wait(self, *args, **kw):
                    raise AssertionError("activate must not be waited on")

            return Proc()

        self.assertTrue(spawn_activate("work", token="tok", popen=fake_popen))
        self.assertEqual(seen["argv"][1:], ["activate", "work"])
        self.assertTrue(str(seen["argv"][0]).endswith("docked-browser"))
        self.assertEqual(seen["kwargs"]["env"]["DOCKED_ACTIVATION_TOKEN"], "tok")
        self.assertTrue(seen["kwargs"]["start_new_session"])
        self.assertIs(seen["kwargs"]["stdout"], subprocess.DEVNULL)
        calls = {"n": 0}

        def count_popen(*args, **kwargs):
            calls["n"] += 1

        self.assertFalse(spawn_activate("not a name", popen=count_popen))
        self.assertEqual(calls["n"], 0)

    def test_daemon_loop_ticks_habit_and_mark_used_logs(self) -> None:
        loop = inspect.getsource(PredictSleepDaemon._loop)
        self.assertIn("daemon_tick", loop)
        self.assertIn("tick(bandit)", loop)
        self.assertIn("log_use", inspect.getsource(mark_used))


if __name__ == "__main__":
    unittest.main()
