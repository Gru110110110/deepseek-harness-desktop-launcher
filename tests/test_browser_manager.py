# SPDX-License-Identifier: MIT
"""Browser discovery and explicit launch tests."""
from __future__ import annotations

import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

import browser_manager
from browser_manager import BrowserChoice, _discover_macos, _windows_command_path


class BrowserDiscoveryTest(unittest.TestCase):
    def test_macos_discovers_each_installed_browser_once(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            applications = Path(tmp)
            (applications / "Google Chrome.app").mkdir()
            (applications / "Firefox.app").mkdir()
            choices = _discover_macos((applications, applications))
        self.assertEqual(
            [(choice.key, choice.label) for choice in choices],
            [("chrome", "Google Chrome"), ("firefox", "Firefox")],
        )

    def test_macos_keeps_one_choice_when_only_one_browser_is_installed(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            applications = Path(tmp)
            (applications / "Safari.app").mkdir()
            choices = _discover_macos((applications,))
        self.assertEqual(len(choices), 1)
        self.assertEqual(choices[0].label, "Safari")

    def test_unknown_platform_has_one_default_choice(self) -> None:
        self.assertEqual(
            browser_manager.discover_browsers("unsupported"),
            (BrowserChoice("default", "默认浏览器", None),),
        )

    def test_windows_command_extracts_quoted_executable(self) -> None:
        self.assertEqual(
            _windows_command_path('"C:\\Program Files\\Browser\\browser.exe" --single-argument %1'),
            Path("C:\\Program Files\\Browser\\browser.exe"),
        )


class BrowserLaunchTest(unittest.TestCase):
    def test_explicit_browser_receives_url_as_its_own_argument(self) -> None:
        browser = BrowserChoice("firefox", "Firefox", ("/usr/bin/firefox",))
        with patch.object(browser_manager.subprocess, "Popen") as popen:
            self.assertTrue(browser_manager.open_in_browser(browser, "http://localhost:41873"))
        command = popen.call_args.args[0]
        self.assertEqual(command, ["/usr/bin/firefox", "http://localhost:41873"])

    def test_default_browser_uses_standard_fallback(self) -> None:
        browser = BrowserChoice("default", "默认浏览器", None)
        with patch.object(browser_manager.webbrowser, "open", return_value=True) as open_default:
            self.assertTrue(browser_manager.open_in_browser(browser, "http://localhost:41873"))
        open_default.assert_called_once_with("http://localhost:41873")

    def test_explicit_browser_launch_failure_is_reported(self) -> None:
        browser = BrowserChoice("firefox", "Firefox", ("/missing/firefox",))
        with patch.object(browser_manager.subprocess, "Popen", side_effect=OSError):
            self.assertFalse(browser_manager.open_in_browser(browser, "http://localhost:41873"))


if __name__ == "__main__":
    unittest.main()
