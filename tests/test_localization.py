# SPDX-License-Identifier: MIT
"""Desktop-language selection, persistence, and copy tests."""
from __future__ import annotations

import plistlib
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

import localization
from localization import (
    LocalizedError,
    load_language,
    save_language,
    select_language,
    translate,
)


class LanguageSelectionTest(unittest.TestCase):
    def test_selects_the_first_shipped_system_language(self) -> None:
        self.assertEqual(select_language(("en-GB", "zh-Hans")), "en")
        self.assertEqual(select_language(("fr-FR", "zh-Hans-CN")), "zh")
        self.assertEqual(select_language(("de-DE", "en_US.UTF-8")), "en")
        self.assertEqual(select_language(("fr-FR",)), "zh")

    def test_saved_selection_overrides_the_system_and_invalid_content_does_not(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "language"
            save_language(path, "zh")
            self.assertEqual(load_language(path, ("en-US",)), "zh")
            path.write_text("unsupported\n", encoding="utf-8")
            self.assertEqual(load_language(path, ("en-US",)), "en")

    def test_save_language_rejects_unknown_values(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            with self.assertRaises(ValueError):
                save_language(Path(tmp) / "language", "fr")

    def test_macos_preferences_preserve_the_system_order(self) -> None:
        payload = plistlib.dumps({"AppleLanguages": ["en-US", "zh-Hans-CN"]})
        completed = subprocess.CompletedProcess([], 0, stdout=payload, stderr=b"")
        with patch.object(localization.subprocess, "run", return_value=completed):
            self.assertEqual(
                localization.system_preferred_languages("Darwin"),
                ("en-US", "zh-Hans-CN"),
            )


class DesktopCopyTest(unittest.TestCase):
    def test_shipped_dictionaries_have_identical_keys(self) -> None:
        self.assertEqual(set(localization._COPY["zh"]), set(localization._COPY["en"]))

    def test_status_and_error_copy_render_in_both_languages(self) -> None:
        self.assertEqual(translate("zh", "workspace_ready"), "工作台已就绪")
        self.assertEqual(translate("en", "workspace_ready"), "Workspace ready")
        error = LocalizedError("download_failed", detail="offline")
        self.assertEqual(translate("zh", error.text.key, **error.text.values), "下载失败：offline")
        self.assertEqual(translate("en", error.text.key, **error.text.values), "Download failed: offline")

    def test_install_activity_names_version_source_and_elapsed_time(self) -> None:
        values = {
            "version": "0.1.0-rc.6",
            "source": "https://registry.npmmirror.com",
            "elapsed": "00:01:03",
        }
        self.assertEqual(
            translate("zh", "activity_installing_harness", **values),
            "从 https://registry.npmmirror.com 安装 Harness v0.1.0-rc.6 · 已用 00:01:03",
        )
        self.assertEqual(
            translate("en", "activity_installing_harness", **values),
            "Installing Harness v0.1.0-rc.6 from https://registry.npmmirror.com · 00:01:03 elapsed",
        )

    def test_operational_copy_matches_the_actions_it_describes(self) -> None:
        cases = {
            "close_to_tray": (
                "关闭窗口后启动器和本地服务将在系统托盘继续运行",
                "Closing this window keeps the launcher and local service running in the system tray",
            ),
            "close_stops_service": (
                "关闭窗口会取消进行中的操作并停止本地服务",
                "Closing this window cancels any operation in progress and stops the local service",
            ),
            "update_restart_detail": (
                "更新完成后会自动重新启动本地服务。",
                "The local service restarts automatically after the update completes.",
            ),
            "starting_detail": (
                "运行环境已就绪，正在启动本地 Web 服务。",
                "The runtime is ready. Starting the local Web service.",
            ),
            "startup_problem": ("操作遇到问题", "Something went wrong"),
            "service_failed": ("启动或更新未完成", "Startup or update did not complete"),
            "deployment_busy": (
                "另一个启动器正在准备或更新运行环境，请稍后重试",
                "Another launcher is preparing or updating the runtime. Try again later.",
            ),
            "deployment_cancelled": (
                "运行环境准备或更新已取消",
                "Runtime preparation or update was cancelled",
            ),
        }
        for key, (chinese, english) in cases.items():
            with self.subTest(key=key):
                self.assertEqual(translate("zh", key), chinese)
                self.assertEqual(translate("en", key), english)

    def test_diagnostics_name_the_actual_runtime_operation(self) -> None:
        values = {"elapsed": "00:00:12", "version": "0.1.0-rc.6"}
        self.assertEqual(
            translate("zh", "activity_waiting_for_lock", **values),
            "等待运行时部署锁 · 已用 00:00:12",
        )
        self.assertEqual(
            translate("en", "activity_waiting_for_lock", **values),
            "Waiting for the runtime deployment lock · 00:00:12 elapsed",
        )
        self.assertEqual(
            translate("zh", "activity_checking_sources", **values),
            "检查 Harness v0.1.0-rc.6 安装源 · 已用 00:00:12",
        )
        self.assertEqual(
            translate("en", "activity_checking_sources", **values),
            "Checking installation sources for Harness v0.1.0-rc.6 · 00:00:12 elapsed",
        )

    def test_failure_copy_preserves_conditional_guarantees(self) -> None:
        values = {"component": "Node.js", "detail": "permission denied"}
        self.assertEqual(
            translate("zh", "runtime_validation_failed", **values),
            "Node.js 安装后验证失败，未启用无效版本；已有版本（如有）保持不变",
        )
        self.assertEqual(
            translate("en", "runtime_validation_failed", **values),
            "Node.js failed post-install validation. The invalid version was not activated; any existing version was preserved.",
        )
        self.assertEqual(
            translate("zh", "config_import_failed", **values),
            "配置导入失败：permission denied",
        )
        self.assertEqual(
            translate("en", "config_import_failed", **values),
            "Configuration import failed: permission denied",
        )


if __name__ == "__main__":
    unittest.main()
