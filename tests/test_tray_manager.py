# SPDX-License-Identifier: MIT
"""Tests for the optional cross-platform tray controller."""
from __future__ import annotations

import sys
import unittest
from pathlib import Path
from unittest.mock import Mock, patch

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from tray_manager import TrayController


class _FakeImage:
    def __enter__(self):
        return self

    def __exit__(self, _kind, _value, _traceback) -> None:
        return None

    def convert(self, mode: str) -> tuple[str, str]:
        return ("converted", mode)


class _FakeImageModule:
    @staticmethod
    def open(_path: Path) -> _FakeImage:
        return _FakeImage()


class _FakeMenuItem:
    def __init__(self, text, action, *, enabled=True) -> None:
        self.text = text
        self.action = action
        self.enabled = enabled


class _FakeIcon:
    def __init__(self, name, image, title, menu, **options) -> None:
        self.name = name
        self.image = image
        self.title = title
        self.menu = menu
        self.options = options
        self.detached_calls = 0
        self.update_calls = 0
        self.stop_calls = 0

    def run_detached(self, setup=None) -> None:
        self.detached_calls += 1
        self.setup = setup

    def update_menu(self) -> None:
        self.update_calls += 1

    def stop(self) -> None:
        self.stop_calls += 1


class _FakeBackend:
    MenuItem = _FakeMenuItem

    def __init__(self) -> None:
        self.icons: list[_FakeIcon] = []

    @staticmethod
    def Menu(*items):
        return items

    def Icon(self, *args, **kwargs) -> _FakeIcon:
        icon = _FakeIcon(*args, **kwargs)
        self.icons.append(icon)
        return icon


class TrayControllerTest(unittest.TestCase):
    def test_menu_is_localized_and_web_ui_tracks_readiness(self) -> None:
        backend = _FakeBackend()
        on_show = Mock()
        on_open = Mock()
        on_quit = Mock()
        controller = TrayController(Path("icon.png"), on_show, on_open, on_quit)

        with (
            patch(
                "tray_manager._load_backend",
                return_value=(backend, _FakeImageModule, {"darwin_nsapplication": "app"}),
            ),
            patch("tray_manager._configure_macos_retina_icon") as configure_retina,
        ):
            self.assertTrue(controller.start("zh", False))

        icon = backend.icons[0]
        self.assertEqual(icon.detached_calls, 1)
        self.assertTrue(icon.visible)
        self.assertEqual(icon.options, {"darwin_nsapplication": "app"})
        configure_retina.assert_called_once_with(icon, _FakeImageModule)
        self.assertEqual(
            [item.text for item in icon.menu],
            ["显示启动主页面", "打开Web UI", "退出"],
        )
        self.assertFalse(icon.menu[1].enabled)

        icon.menu[0].action(None, None)
        icon.menu[1].action(None, None)
        icon.menu[2].action(None, None)
        on_show.assert_called_once_with()
        on_open.assert_called_once_with()
        on_quit.assert_called_once_with()

        controller.refresh("en", True)

        self.assertEqual(
            [item.text for item in icon.menu],
            ["Show Launcher", "Open Web UI", "Quit"],
        )
        self.assertTrue(icon.menu[1].enabled)
        self.assertEqual(icon.update_calls, 1)

    def test_stop_is_idempotent(self) -> None:
        backend = _FakeBackend()
        controller = TrayController(Path("icon.png"), Mock(), Mock(), Mock())
        with patch(
            "tray_manager._load_backend",
            return_value=(backend, _FakeImageModule, {}),
        ):
            self.assertTrue(controller.start("en", True))

        icon = backend.icons[0]
        controller.stop()
        controller.stop()

        self.assertEqual(icon.stop_calls, 1)
        self.assertFalse(controller.is_running)

    def test_start_failure_is_reported_without_leaving_a_running_icon(self) -> None:
        controller = TrayController(Path("missing.png"), Mock(), Mock(), Mock())
        with patch("tray_manager._load_backend", side_effect=ImportError("missing")):
            self.assertFalse(controller.start("zh", False))
        self.assertFalse(controller.is_running)

    def test_partial_backend_start_is_stopped_before_fallback(self) -> None:
        backend = _FakeBackend()
        icon = _FakeIcon("name", "image", "title", ())
        icon.run_detached = Mock(side_effect=RuntimeError("backend failed"))
        backend.Icon = Mock(return_value=icon)
        controller = TrayController(Path("icon.png"), Mock(), Mock(), Mock())

        with patch(
            "tray_manager._load_backend",
            return_value=(backend, _FakeImageModule, {}),
        ):
            self.assertFalse(controller.start("zh", False))

        self.assertEqual(icon.stop_calls, 1)
        self.assertFalse(controller.is_running)


if __name__ == "__main__":
    unittest.main()
