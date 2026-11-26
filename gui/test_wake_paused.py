#!/usr/bin/env python3
"""Unit tests for wake-on-focus vs bare workspace switches. No Docker, no D-Bus."""

from __future__ import annotations

import unittest
from unittest import mock

import wake_paused
from wake_paused import WORKSPACE_SWITCH_S, FocusState, tick


class WakeOnFocusTest(unittest.TestCase):
    def setUp(self) -> None:
        # (focused profile, seconds since last bare workspace switch or None)
        self.focus: tuple[str, float | None] = ("", None)
        self.paused = {"Relax", "Stoc"}
        self.woken: list[str] = []
        patches = [
            mock.patch.object(wake_paused, "focused_docked_profile", lambda: self.focus),
            mock.patch.object(wake_paused, "container_paused", lambda p: p in self.paused),
            mock.patch.object(wake_paused, "request_wake", self.woken.append),
            mock.patch.object(wake_paused, "_log", lambda _line: None),
        ]
        for p in patches:
            p.start()
            self.addCleanup(p.stop)
        self.state = FocusState()

    def poll(self, profile: str, switch_age: float | None) -> None:
        self.focus = (profile, switch_age)
        tick(self.state)

    def test_focus_without_any_switch_wakes(self) -> None:
        self.poll("", None)
        self.poll("Relax", None)
        self.assertEqual(self.woken, ["Relax"])

    def test_dock_click_onto_other_workspace_wakes(self) -> None:
        # Dock / Alt+Tab use activate_with_focus: no fresh bare switch.
        self.poll("", 120.0)
        self.poll("Relax", 120.4)
        self.assertEqual(self.woken, ["Relax"])

    def test_bare_switch_focus_does_not_wake(self) -> None:
        self.poll("", 60.0)
        self.poll("Relax", 0.2)
        self.poll("Stoc", 0.3)
        self.assertEqual(self.woken, [])

    def test_focus_one_poll_after_bare_switch_does_not_wake(self) -> None:
        self.poll("", 0.1)
        self.poll("Relax", WORKSPACE_SWITCH_S / 2)
        self.assertEqual(self.woken, [])

    def test_click_after_switch_settles_wakes(self) -> None:
        self.poll("", 30.0)
        self.poll("Relax", 0.2)  # landed on it by the switch
        self.poll("", 3.0)  # focus moved to something else
        self.poll("Relax", 4.0)  # clicked it on purpose
        self.assertEqual(self.woken, ["Relax"])

    def test_paused_while_focused_waits_for_focus_to_leave(self) -> None:
        self.paused = set()
        self.poll("Relax", None)
        self.paused = {"Relax"}
        self.poll("Relax", None)
        self.poll("Relax", None)
        self.assertEqual(self.woken, [])




class HyprSwitchTest(unittest.TestCase):
    """Same cases as the HyprSwitch tests in wake-paused/src/main.rs."""

    def test_bare_switch_is_reported_after_it_settles(self) -> None:
        sw = wake_paused.HyprSwitch()
        self.assertIsNone(sw.age(0.0))
        sw.on_event("activewindow>>docked-browser-Relax,GitHub", 0.0)
        sw.on_event("workspace>>4", 0.0)
        sw.on_event("workspacev2>>4,4", 0.0)
        self.assertTrue(sw.settling(0.01))
        self.assertAlmostEqual(sw.age(0.3), 0.3)
        self.assertFalse(sw.settling(0.3))

    def test_window_pick_on_other_workspace_is_not_a_bare_switch(self) -> None:
        sw = wake_paused.HyprSwitch()
        sw.on_event("activewindow>>docked-browser-Relax,GitHub", 0.0)
        sw.on_event("workspace>>4", 0.0)
        sw.on_event("activewindow>>docked-browser-Relax,GitHub", 0.001)
        self.assertIsNone(sw.age(0.3))
        sw.on_event("workspace>>1", 1.0)
        self.assertIsNotNone(sw.age(1.2))
        sw.on_event("workspace>>4", 5.0)
        sw.on_event("activewindow>>docked-browser-Stoc,x", 5.001)
        self.assertAlmostEqual(sw.age(5.2), 4.2)

    def test_focus_change_long_after_switch_is_ignored(self) -> None:
        sw = wake_paused.HyprSwitch()
        sw.on_event("workspace>>2", 0.0)
        sw.on_event("activewindow>>docked-browser-Relax,x", 0.5)
        self.assertIsNotNone(sw.age(0.6))

if __name__ == "__main__":
    unittest.main()
