#!/usr/bin/env python3
"""Unit tests for predict-sleep rewards and ε-explore. No Docker."""

from __future__ import annotations

import unittest

from predict_sleep import (
    ACTION_KEEP,
    ACTION_PAUSE,
    EXPLORE_PAUSE_EPS,
    EXPLORE_PAUSE_IDLE_MS,
    IDLE_TRIP_MS,
    KEEP_REWARD_ACTIVE,
    KEEP_REWARD_AT_BREAKER,
    KEEP_REWARD_MID_START,
    PAUSE_REWARD_SUCCESS,
    reward_from_idle,
    should_explore_pause,
)


class RewardFromIdleTest(unittest.TestCase):
    def test_keep_active_is_strong_positive(self) -> None:
        self.assertEqual(reward_from_idle(ACTION_KEEP, 10_000, 60_000), KEEP_REWARD_ACTIVE)

    def test_keep_ramps_toward_breaker_penalty(self) -> None:
        mid = reward_from_idle(ACTION_KEEP, 30_000 + 1, 60_000)
        late = reward_from_idle(ACTION_KEEP, IDLE_TRIP_MS - 1, 60_000)
        at = reward_from_idle(ACTION_KEEP, IDLE_TRIP_MS, 60_000)
        self.assertAlmostEqual(mid, KEEP_REWARD_MID_START, places=2)
        self.assertLess(late, mid)
        self.assertGreater(late, KEEP_REWARD_AT_BREAKER)
        self.assertEqual(at, KEEP_REWARD_AT_BREAKER)

    def test_keep_midpoint_is_between_endpoints(self) -> None:
        still_active = 30_000
        mid_idle = still_active + (IDLE_TRIP_MS - still_active) // 2
        mid = reward_from_idle(ACTION_KEEP, mid_idle, 60_000)
        expected = (KEEP_REWARD_MID_START + KEEP_REWARD_AT_BREAKER) / 2
        self.assertAlmostEqual(mid, expected, places=3)

    def test_pause_success_and_forced_wake(self) -> None:
        self.assertEqual(
            reward_from_idle(ACTION_PAUSE, IDLE_TRIP_MS, 60_000),
            PAUSE_REWARD_SUCCESS,
        )
        self.assertEqual(reward_from_idle(ACTION_PAUSE, 5_000, 30_000), -1.0)
        self.assertEqual(reward_from_idle(ACTION_PAUSE, 60_000, 60_000), 0.0)


class ExplorePauseTest(unittest.TestCase):
    def test_below_idle_floor_never_explores(self) -> None:
        self.assertFalse(should_explore_pause("Coursera", EXPLORE_PAUSE_IDLE_MS - 1, 1e9))

    def test_explore_rate_near_eps(self) -> None:
        # One minute bucket per sample over many profiles/minutes.
        hits = 0
        n = 5000
        base = 1_700_000_000.0
        for i in range(n):
            if should_explore_pause(f"p{i % 17}", EXPLORE_PAUSE_IDLE_MS, base + i * 60):
                hits += 1
        rate = hits / n
        self.assertGreater(rate, EXPLORE_PAUSE_EPS - 0.04)
        self.assertLess(rate, EXPLORE_PAUSE_EPS + 0.04)


if __name__ == "__main__":
    unittest.main()
