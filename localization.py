# SPDX-License-Identifier: MIT
"""Desktop-launcher locale detection, persistence, and translated copy."""
from __future__ import annotations

import ctypes
import locale
import os
import platform
import plistlib
import re
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Iterable, Mapping

SUPPORTED_LANGUAGES = ("zh", "en")
DEFAULT_LANGUAGE = "zh"

_COPY: dict[str, dict[str, str]] = {
    "zh": {
        "startup_flow": "启动流程",
        "step_prepare_title": "运行环境",
        "step_prepare_description": "检查并准备本地依赖",
        "step_start_title": "本地服务",
        "step_start_description": "启动 Web 工作台",
        "workspace_eyebrow": "DEEPSEEK · 本地工作台",
        "status_preparing": "正在准备环境",
        "detail_preparing": "正在检查运行环境。首次安装会下载 Node.js 和数百个 Harness 依赖，并持续显示当前阶段与耗时。",
        "service_title": "本地服务",
        "badge_preparing": "●  准备中",
        "checking_components": "正在检查已安装组件",
        "preparing": "正在准备",
        "web_ui_address": "Web UI 地址",
        "waiting_address": "等待服务发布地址",
        "address_copied": "已复制 · {url}",
        "runtime_status": "运行状态",
        "waiting_service": "等待服务启动",
        "update_now": "立即更新",
        "close_to_tray": "关闭窗口后启动器和本地服务将在系统托盘继续运行",
        "close_stops_service": "关闭窗口会取消进行中的操作并停止本地服务",
        "tray_unavailable_close_exits": "系统托盘不可用；关闭窗口将停止本地服务并退出",
        "tray_show_launcher": "显示启动主页面",
        "tray_open_web_ui": "打开Web UI",
        "tray_quit": "退出",
        "starting": "正在启动 ...",
        "open_with_browser": "用 {browser} 打开  →",
        "open_web_ui": "打开 Web UI  →",
        "default_browser": "默认浏览器",
        "language_label": "语言 · 中文",
        "address_not_ready": "服务地址尚未就绪",
        "wait_for_service": "请等待本地服务完成启动。",
        "browser_open_failed": "浏览器打开失败",
        "manual_visit": "请手动访问 {url}",
        "starting_service": "正在启动服务",
        "updating_harness": "正在更新 Harness",
        "update_restart_detail": "更新完成后会自动重新启动本地服务。",
        "installing_version": "正在安装已验证版本",
        "updating": "更新中",
        "preparing_detail": "正在检查运行环境。首次安装会下载 Node.js 和数百个 Harness 依赖，并持续显示当前阶段与耗时。",
        "preparing_components": "正在检查并安装本地组件",
        "preparing_runtime": "准备运行环境",
        "activity_waiting_for_lock": "等待运行时部署锁 · 已用 {elapsed}",
        "activity_checking_runtime": "检查已安装运行时 · 已用 {elapsed}",
        "activity_resolving_version": "查询最新 Harness 版本 · 已用 {elapsed}",
        "activity_downloading_node": "下载 Node.js {version} · 已用 {elapsed}",
        "activity_verifying_node": "校验并解压 Node.js {version} · 已用 {elapsed}",
        "activity_checking_sources": "检查 Harness v{version} 安装源 · 已用 {elapsed}",
        "activity_installing_harness": "从 {source} 安装 Harness v{version} · 已用 {elapsed}",
        "activity_validating_harness": "验证 Harness v{version} · 已用 {elapsed}",
        "activity_activating_harness": "切换到 Harness v{version} · 已用 {elapsed}",
        "activity_starting_service": "启动本地 Web 服务 · 已用 {elapsed}",
        "starting_detail": "运行环境已就绪，正在启动本地 Web 服务。",
        "starting_web_service": "正在启动本地 Web 服务",
        "connecting_service": "连接本地服务",
        "workspace_ready": "工作台已就绪",
        "workspace_ready_detail": "本地服务运行正常，可以开始使用 DeepSeek Harness。",
        "badge_running": "●  运行中",
        "workspace_available": "Web 工作台可以随时打开",
        "runtime_running": "已运行 {elapsed}",
        "startup_problem": "操作遇到问题",
        "badge_attention": "●  需要处理",
        "service_failed": "启动或更新未完成",
        "address_unavailable": "未获取到服务地址",
        "not_running": "未运行",
        "retry": "重新尝试  →",
        "update_available": "Harness v{version} 可用",
        "missing_tk": "此环境缺少 tkinter，无法启动图形界面",
        "version_query_failed": "查询版本失败：{detail}",
        "version_missing": "查询版本失败：响应缺少 version",
        "unsupported_platform": "不支持的平台：{platform}",
        "node_version_query_failed": "获取 Node.js 版本信息失败：{detail}",
        "node_stable_missing": "找不到 Node.js {major} 的稳定版本",
        "download_failed": "下载失败：{detail}",
        "file_write_failed": "写入文件失败：{detail}",
        "node_archive_invalid": "Node.js 压缩包结构异常：{entries}",
        "node_archive_unsafe": "Node.js 压缩包包含不安全路径：{entry}",
        "node_checksum_missing": "缺少 Node.js {version}（{filename}）的可信 SHA-256",
        "runtime_validation_failed": "{component} 安装后验证失败，未启用无效版本；已有版本（如有）保持不变",
        "runtime_version_invalid": "无效的 Harness 版本：{version}",
        "environment_invalid": "环境变量 {variable} 的值无效：{value}",
        "deployment_busy": "另一个启动器正在准备或更新运行环境，请稍后重试",
        "deployment_cancelled": "运行环境准备或更新已取消",
        "install_failed": "安装 DeepSeek Harness 失败，详见日志：{log}",
        "service_starting_no_address": "服务正在启动，但尚未发布访问地址",
        "config_import_failed": "配置导入失败：{detail}",
        "service_start_failed": "服务启动失败：{detail}",
        "service_output_unreadable": "服务启动失败：无法读取启动输出",
        "free_port_failed": "默认端口被占用，自动选择空闲端口仍失败，请查看日志",
        "service_no_address": "服务未发布访问地址，请查看日志",
    },
    "en": {
        "startup_flow": "STARTUP",
        "step_prepare_title": "Runtime",
        "step_prepare_description": "Check and prepare local dependencies",
        "step_start_title": "Local service",
        "step_start_description": "Start the Web workspace",
        "workspace_eyebrow": "DEEPSEEK · LOCAL WORKSPACE",
        "status_preparing": "Preparing environment",
        "detail_preparing": "Checking the runtime. First installation downloads Node.js and hundreds of Harness dependencies while showing the current activity and elapsed time.",
        "service_title": "Local service",
        "badge_preparing": "●  Preparing",
        "checking_components": "Checking installed components",
        "preparing": "Preparing",
        "web_ui_address": "Web UI address",
        "waiting_address": "Waiting for the service address",
        "address_copied": "Copied · {url}",
        "runtime_status": "Runtime status",
        "waiting_service": "Waiting for the service to start",
        "update_now": "Update now",
        "close_to_tray": "Closing this window keeps the launcher and local service running in the system tray",
        "close_stops_service": "Closing this window cancels any operation in progress and stops the local service",
        "tray_unavailable_close_exits": "The system tray is unavailable; closing this window stops the local service and exits",
        "tray_show_launcher": "Show Launcher",
        "tray_open_web_ui": "Open Web UI",
        "tray_quit": "Quit",
        "starting": "Starting ...",
        "open_with_browser": "Open with {browser}  →",
        "open_web_ui": "Open Web UI  →",
        "default_browser": "Default browser",
        "language_label": "Language · English",
        "address_not_ready": "Service address not ready",
        "wait_for_service": "Wait for the local service to finish starting.",
        "browser_open_failed": "Could not open the browser",
        "manual_visit": "Open {url} manually.",
        "starting_service": "Starting service",
        "updating_harness": "Updating Harness",
        "update_restart_detail": "The local service restarts automatically after the update completes.",
        "installing_version": "Installing the verified version",
        "updating": "Updating",
        "preparing_detail": "Checking the runtime. First installation downloads Node.js and hundreds of Harness dependencies while showing the current activity and elapsed time.",
        "preparing_components": "Checking and installing local components",
        "preparing_runtime": "Preparing runtime",
        "activity_waiting_for_lock": "Waiting for the runtime deployment lock · {elapsed} elapsed",
        "activity_checking_runtime": "Checking the installed runtime · {elapsed} elapsed",
        "activity_resolving_version": "Checking the latest Harness version · {elapsed} elapsed",
        "activity_downloading_node": "Downloading Node.js {version} · {elapsed} elapsed",
        "activity_verifying_node": "Verifying and extracting Node.js {version} · {elapsed} elapsed",
        "activity_checking_sources": "Checking installation sources for Harness v{version} · {elapsed} elapsed",
        "activity_installing_harness": "Installing Harness v{version} from {source} · {elapsed} elapsed",
        "activity_validating_harness": "Validating Harness v{version} · {elapsed} elapsed",
        "activity_activating_harness": "Activating Harness v{version} · {elapsed} elapsed",
        "activity_starting_service": "Starting the local Web service · {elapsed} elapsed",
        "starting_detail": "The runtime is ready. Starting the local Web service.",
        "starting_web_service": "Starting the local Web service",
        "connecting_service": "Connecting to the local service",
        "workspace_ready": "Workspace ready",
        "workspace_ready_detail": "The local service is running. DeepSeek Harness is ready to use.",
        "badge_running": "●  Running",
        "workspace_available": "The Web workspace is ready to open",
        "runtime_running": "Running for {elapsed}",
        "startup_problem": "Something went wrong",
        "badge_attention": "●  Needs attention",
        "service_failed": "Startup or update did not complete",
        "address_unavailable": "No service address available",
        "not_running": "Not running",
        "retry": "Try again  →",
        "update_available": "Harness v{version} is available",
        "missing_tk": "tkinter is unavailable, so the graphical interface cannot start",
        "version_query_failed": "Could not check the version: {detail}",
        "version_missing": "Could not check the version: the response has no version",
        "unsupported_platform": "Unsupported platform: {platform}",
        "node_version_query_failed": "Could not retrieve Node.js version information: {detail}",
        "node_stable_missing": "No stable Node.js {major} release was found",
        "download_failed": "Download failed: {detail}",
        "file_write_failed": "Could not write the file: {detail}",
        "node_archive_invalid": "Unexpected Node.js archive contents: {entries}",
        "node_archive_unsafe": "The Node.js archive contains an unsafe path: {entry}",
        "node_checksum_missing": "No trusted SHA-256 is configured for Node.js {version} ({filename})",
        "runtime_validation_failed": "{component} failed post-install validation. The invalid version was not activated; any existing version was preserved.",
        "runtime_version_invalid": "Invalid Harness version: {version}",
        "environment_invalid": "Environment variable {variable} has an invalid value: {value}",
        "deployment_busy": "Another launcher is preparing or updating the runtime. Try again later.",
        "deployment_cancelled": "Runtime preparation or update was cancelled",
        "install_failed": "DeepSeek Harness installation failed. See {log}",
        "service_starting_no_address": "The service is starting but has not published an address",
        "config_import_failed": "Configuration import failed: {detail}",
        "service_start_failed": "Service startup failed: {detail}",
        "service_output_unreadable": "Service startup failed: startup output is unavailable",
        "free_port_failed": "The default port was occupied and the free-port retry failed. See the log.",
        "service_no_address": "The service did not publish an address. See the log.",
    },
}


@dataclass(frozen=True)
class LocalizedText:
    """A translated-copy key whose parameters remain renderable after a language switch."""

    key: str
    values: Mapping[str, object]


class LocalizedError(RuntimeError):
    """A runtime failure with copy that the desktop UI renders in its active language."""

    def __init__(self, key: str, **values: object):
        self.text = LocalizedText(key, values)
        super().__init__(translate(DEFAULT_LANGUAGE, key, **values))


def message(key: str, **values: object) -> LocalizedText:
    """Create deferred translated copy for a desktop UI callback."""
    return LocalizedText(key, values)


def translate(language: str, key: str, **values: object) -> str:
    """Render one shipped desktop string in ``language``."""
    dictionary = _COPY.get(language, _COPY[DEFAULT_LANGUAGE])
    template = dictionary.get(key, _COPY[DEFAULT_LANGUAGE].get(key, key))
    return template.format_map(values)


def select_language(preferred_languages: Iterable[str]) -> str:
    """Return the first shipped primary language from an ordered preference list."""
    for tag in preferred_languages:
        primary = re.split(r"[-_.@]", tag.strip().lower(), maxsplit=1)[0]
        if primary in SUPPORTED_LANGUAGES:
            return primary
    return DEFAULT_LANGUAGE


def system_preferred_languages(system_name: str | None = None) -> tuple[str, ...]:
    """Read the current user's ordered UI-language preferences."""
    system = system_name or platform.system()
    if system == "Darwin":
        languages = _macos_preferred_languages()
        if languages:
            return languages
    elif system == "Windows":
        languages = _windows_preferred_languages()
        if languages:
            return languages
    return _environment_languages()


def load_language(path: Path, preferred_languages: Iterable[str] | None = None) -> str:
    """Load a valid saved selection or derive one from user UI-language preferences."""
    try:
        saved = path.read_text(encoding="utf-8").strip()
    except OSError:
        saved = ""
    if saved in SUPPORTED_LANGUAGES:
        return saved
    preferences = preferred_languages if preferred_languages is not None else system_preferred_languages()
    return select_language(preferences)


def save_language(path: Path, language: str) -> None:
    """Atomically persist one validated desktop-language selection."""
    if language not in SUPPORTED_LANGUAGES:
        raise ValueError(f'unsupported desktop language "{language}"')
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(path.name + ".tmp")
    try:
        temporary.write_text(language + "\n", encoding="utf-8")
        temporary.replace(path)
    except OSError:
        temporary.unlink(missing_ok=True)
        raise


def _macos_preferred_languages() -> tuple[str, ...]:
    try:
        result = subprocess.run(
            ["/usr/bin/defaults", "export", "NSGlobalDomain", "-"],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            check=False,
            timeout=2,
        )
        document = plistlib.loads(result.stdout) if result.returncode == 0 else {}
    except (OSError, subprocess.TimeoutExpired, plistlib.InvalidFileException):
        return ()
    languages = document.get("AppleLanguages") if isinstance(document, dict) else None
    if not isinstance(languages, list):
        return ()
    return tuple(language for language in languages if isinstance(language, str))


def _windows_preferred_languages() -> tuple[str, ...]:
    if sys.platform != "win32":
        return ()
    kernel32 = ctypes.windll.kernel32
    count = ctypes.c_ulong()
    size = ctypes.c_ulong()
    flags = 0x8  # MUI_LANGUAGE_NAME
    if not kernel32.GetUserPreferredUILanguages(flags, ctypes.byref(count), None, ctypes.byref(size)):
        return ()
    buffer = ctypes.create_unicode_buffer(size.value)
    if not kernel32.GetUserPreferredUILanguages(
        flags, ctypes.byref(count), buffer, ctypes.byref(size),
    ):
        return ()
    return tuple(tag for tag in buffer[:size.value].split("\0") if tag)


def _environment_languages() -> tuple[str, ...]:
    result: list[str] = []
    language = os.environ.get("LANGUAGE")
    if language:
        result.extend(language.split(":"))
    for name in ("LC_ALL", "LC_MESSAGES", "LANG"):
        value = os.environ.get(name)
        if value:
            result.append(value)
    current, _encoding = locale.getlocale()
    if current:
        result.append(current)
    return tuple(result)
