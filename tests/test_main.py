# SPDX-License-Identifier: MIT
"""Tests for desktop status presentation helpers."""
from __future__ import annotations

import sys
import unittest
from pathlib import Path
from unittest.mock import Mock, patch

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

import localization
from main import (
    ACTIVITY_COPY_KEYS,
    INDETERMINATE_PROGRESS_INTERVAL_MS,
    INDETERMINATE_PROGRESS_MAXIMUM,
    OFFICIAL_WEBSITE,
    DesktopApp,
    button_layout_without_focus,
    format_uptime,
    wrap_text_to_width,
)


class StatusPresentationTest(unittest.TestCase):
    def test_custom_button_layouts_omit_dotted_focus_element(self) -> None:
        def element_names(layout: list[tuple[str, dict[str, object]]]) -> list[str]:
            names: list[str] = []
            for name, options in layout:
                names.append(name)
                children = options.get("children", [])
                if isinstance(children, list):
                    names.extend(element_names(children))
            return names

        for border in (False, True):
            names = element_names(button_layout_without_focus(border=border))
            self.assertNotIn("Button.focus", names)
            self.assertIn("Button.padding", names)
            self.assertIn("Button.label", names)

    def test_format_uptime_uses_clock_fields(self) -> None:
        self.assertEqual(format_uptime(3661.9), "01:01:01")

    def test_each_runtime_activity_has_bilingual_copy(self) -> None:
        for copy_key in ACTIVITY_COPY_KEYS.values():
            self.assertIn(copy_key, localization._COPY["zh"])
            self.assertIn(copy_key, localization._COPY["en"])

    def test_activity_renderer_includes_live_elapsed_time(self) -> None:
        app = DesktopApp.__new__(DesktopApp)
        app._activity_started_at = 100.0
        app._activity_copy_key = "activity_installing_harness"
        app._activity_values = {"version": "0.1.0-rc.6", "source": "https://registry.example"}
        app._set_copy = Mock()
        with patch("main.time.monotonic", return_value=163.9):
            app._render_activity()
        app._set_copy.assert_called_once_with(
            "progress_label",
            "activity_installing_harness",
            version="0.1.0-rc.6",
            source="https://registry.example",
            elapsed="00:01:03",
        )

    def test_indeterminate_progress_resets_download_byte_range_before_animation(self) -> None:
        app = DesktopApp.__new__(DesktopApp)
        app.progress = Mock()
        app.percent_var = Mock()
        app._progress_indeterminate = False

        app._set_progress_indeterminate()

        app.progress.stop.assert_called_once_with()
        app.progress.configure.assert_called_once_with(
            mode="indeterminate",
            maximum=INDETERMINATE_PROGRESS_MAXIMUM,
            value=0,
        )
        app.progress.start.assert_called_once_with(INDETERMINATE_PROGRESS_INTERVAL_MS)
        app.percent_var.set.assert_called_once_with("")
        self.assertTrue(app._progress_indeterminate)

    def test_close_hint_wrap_inserts_visible_line_breaks(self) -> None:
        app = DesktopApp.__new__(DesktopApp)
        app.close_hint_label = Mock()
        app.close_hint_label.winfo_width.return_value = 156
        app.close_hint_display_var = Mock()
        app.close_hint_display_var.get.return_value = ""
        app._hint_font = Mock()
        app._hint_font.measure.side_effect = len
        source_var = Mock()
        source_var.get.return_value = "Closing this window stops the service"
        app._copy_bindings = {"close_hint": (source_var, "close_stops_service", {})}

        app._resize_close_hint()

        app.close_hint_display_var.set.assert_called_once_with(
            "Closing this window stops the service",
        )

    def test_pixel_wrapper_breaks_words_without_truncating_text(self) -> None:
        wrapped = wrap_text_to_width(
            "Closing this window stops the local service",
            20,
            len,
        )

        self.assertEqual(
            wrapped,
            "Closing this window\nstops the local\nservice",
        )
        self.assertEqual(wrapped.replace("\n", " "), "Closing this window stops the local service")

    @patch("main.open_in_browser", return_value=True)
    def test_logo_opens_official_website_in_selected_browser(self, open_browser: Mock) -> None:
        app = DesktopApp.__new__(DesktopApp)
        app.selected_browser = Mock()

        app.open_official_website()

        open_browser.assert_called_once_with(app.selected_browser, OFFICIAL_WEBSITE)

    @patch("main.open_in_browser", return_value=True)
    def test_web_ui_open_reports_success(self, open_browser: Mock) -> None:
        app = DesktopApp.__new__(DesktopApp)
        app.selected_browser = Mock()
        app.web_url = "http://127.0.0.1:41873"
        app._set_copy = Mock()

        self.assertTrue(app.open_web_ui())
        open_browser.assert_called_once_with(app.selected_browser, app.web_url)

    def test_window_close_hides_when_tray_is_available(self) -> None:
        app = DesktopApp.__new__(DesktopApp)
        app._tray_ready = True
        app.root = Mock()
        app.quit_app = Mock()

        app._handle_window_close()

        app.root.withdraw.assert_called_once_with()
        app.quit_app.assert_not_called()

    def test_window_close_exits_when_tray_is_unavailable(self) -> None:
        app = DesktopApp.__new__(DesktopApp)
        app._tray_ready = False
        app.root = Mock()
        app.quit_app = Mock()

        app._handle_window_close()

        app.quit_app.assert_called_once_with()
        app.root.withdraw.assert_not_called()

    def test_show_main_window_restores_and_focuses_it(self) -> None:
        app = DesktopApp.__new__(DesktopApp)
        app._closing = False
        app.root = Mock()

        app.show_main_window()

        app.root.deiconify.assert_called_once_with()
        app.root.state.assert_called_once_with("normal")
        app.root.lift.assert_called_once_with()
        app.root.focus_force.assert_called_once_with()

    def test_true_exit_stops_tray_service_and_window_once(self) -> None:
        app = DesktopApp.__new__(DesktopApp)
        app._closing = False
        app._tray_ready = True
        app.tray = Mock()
        app._deployment_controller = Mock()
        app._worker_thread = None
        app.server = Mock()
        app.root = Mock()

        app.quit_app()
        app.quit_app()

        app.tray.stop.assert_called_once_with()
        app._deployment_controller.cancel.assert_called_once_with()
        app.server.stop.assert_called_once_with()
        app.root.destroy.assert_called_once_with()
        self.assertTrue(app._closing)
        self.assertFalse(app._tray_ready)

    def test_clicking_web_address_copies_it_and_shows_feedback(self) -> None:
        app = DesktopApp.__new__(DesktopApp)
        app.root = Mock()
        app.root.after.return_value = "feedback-job"
        app.web_url = "http://127.0.0.1:41873"
        app._copy_feedback_after_id = None
        app._set_copy = Mock()

        app.copy_web_url()

        app.root.clipboard_clear.assert_called_once_with()
        app.root.clipboard_append.assert_called_once_with(app.web_url)
        app._set_copy.assert_called_once_with("web_url", "address_copied", url=app.web_url)
        app.root.after.assert_called_once_with(1400, app._restore_web_url_copy)
        self.assertEqual(app._copy_feedback_after_id, "feedback-job")


if __name__ == "__main__":
    unittest.main()
