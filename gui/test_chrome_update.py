#!/usr/bin/env python3
"""Unit tests for Chrome update checking and comparison."""

from __future__ import annotations

import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import chrome_update
from chrome_update import (
    _normalize_version_tuple,
    check_for_chrome_update,
    is_newer_version,
    parse_packages_data,
)


class ChromeUpdateTest(unittest.TestCase):
    def test_parse_packages_data(self) -> None:
        raw = """Package: dummy
Version: 1.0.0

Package: google-chrome-stable
Version: 155.0.8059.84-1
Architecture: amd64

Package: another-package
Version: 2.0.0
"""
        ver = parse_packages_data(raw)
        self.assertEqual(ver, "155.0.8059.84-1")

    def test_parse_packages_data_missing(self) -> None:
        raw = "Package: something-else\nVersion: 1.0\n"
        self.assertIsNone(parse_packages_data(raw))

    def test_normalize_version_tuple(self) -> None:
        self.assertEqual(
            _normalize_version_tuple("155.0.8059.84-1"),
            (155, 0, 8059, 84),
        )
        self.assertEqual(
            _normalize_version_tuple("153.0.8010.52"),
            (153, 0, 8010, 52),
        )
        self.assertEqual(
            _normalize_version_tuple("1:155.0.8059.84-1"),
            (155, 0, 8059, 84),
        )

    def test_is_newer_version(self) -> None:
        self.assertTrue(is_newer_version("155.0.8059.84-1", "153.0.8010.52"))
        self.assertFalse(is_newer_version("153.0.8010.52", "153.0.8010.52"))
        self.assertFalse(is_newer_version("152.0.0.0", "153.0.8010.52"))
        self.assertTrue(is_newer_version("153.0.8010.53", "153.0.8010.52"))
        self.assertFalse(is_newer_version("155.0.8059.84-1", "155.0.8059.84"))
        self.assertTrue(is_newer_version("155.0.8059.85-1", "155.0.8059.84"))
        self.assertFalse(is_newer_version("", "153.0.8010.52"))
        self.assertFalse(is_newer_version("155.0.0.0", ""))

    def _isolated_state(self, prior: dict):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        state_dir = Path(tmp.name)
        state_file = state_dir / "chrome-update-check.json"
        state_file.write_text(json.dumps(prior), encoding="utf-8")
        return (
            patch.object(chrome_update, "STATE_DIR", state_dir),
            patch.object(chrome_update, "UPDATE_STATE_FILE", state_file),
            state_file,
        )

    def test_failed_probe_keeps_known_update(self) -> None:
        prior = {
            "last_checked_at": 0,
            "installed_version": "153.0.8010.52",
            "latest_version": "155.0.8059.84-1",
            "update_available": True,
            "last_notified_at": 111,
        }
        dir_patch, file_patch, state_file = self._isolated_state(prior)
        with dir_patch, file_patch, patch.object(
            chrome_update, "fetch_upstream_version", return_value=None
        ), patch.object(
            chrome_update, "get_installed_chrome_version", return_value=None
        ):
            result = check_for_chrome_update(force=True)
        self.assertTrue(result["update_available"])
        self.assertEqual(result["latest_version"], "155.0.8059.84-1")
        self.assertEqual(result["last_notified_at"], 111)
        self.assertTrue(result["check_failed"])
        saved = json.loads(state_file.read_text(encoding="utf-8"))
        self.assertTrue(saved["update_available"])
        self.assertEqual(saved["last_notified_at"], 111)

    def test_same_chrome_with_debian_revision_is_current(self) -> None:
        prior = {"last_checked_at": 0, "last_notified_at": 222}
        dir_patch, file_patch, _state_file = self._isolated_state(prior)
        with dir_patch, file_patch, patch.object(
            chrome_update, "fetch_upstream_version", return_value="155.0.8059.84-1"
        ), patch.object(
            chrome_update, "get_installed_chrome_version", return_value="155.0.8059.84"
        ):
            result = check_for_chrome_update(force=True)
        self.assertFalse(result["update_available"])
        self.assertEqual(result["last_notified_at"], 222)
        self.assertNotIn("check_failed", result)


if __name__ == "__main__":
    unittest.main()
