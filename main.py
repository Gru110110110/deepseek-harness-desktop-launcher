# SPDX-License-Identifier: MIT
"""DSH Launcher desktop application and local service status window.

The launcher presents two startup stages plus the current blocking activity and
its elapsed time, keeps the local Web UI address visible, and owns the service
lifetime. It also checks the npm registry for a newer ``@deepseek-ai/dsh``
release and offers an in-place update after startup.
"""
from __future__ import annotations

import argparse
import queue
import sys
import threading
import time
from collections.abc import Callable

from app_paths import APP_NAME, APP_VERSION, APPLICATION_PATHS, resource_root
from browser_manager import BrowserChoice, discover_browsers, open_in_browser
from localization import (
    LocalizedError,
    LocalizedText,
    load_language,
    save_language,
    translate,
)
from runtime import (
    DeploymentCancelled,
    DeploymentController,
    deployment_shutdown_timeout_seconds,
    deploy_runtime,
    installed_version,
    is_newer_version,
    is_runtime_ready,
    latest_harness_version,
)
from server_manager import ServerManager
from tray_manager import TrayController, check_tray_dependencies

try:
    from tkinter import BOTH, LEFT, RIGHT, X, Y, Menu, PhotoImage, StringVar, TclError, Tk, ttk
    from tkinter import font as tkfont
except ImportError:  # allow ``--check`` on hosts without tkinter
    HAS_TK = False
else:
    HAS_TK = True

BACKGROUND = "#f3f5f8"
SURFACE = "#ffffff"
SIDEBAR = "#f8fafc"
INK = "#182230"
MUTED = "#667085"
SOFT_MUTED = "#98a2b3"
BORDER = "#dfe5ec"
BRAND = "#4d6bfe"
BRAND_DARK = "#3b55d9"
BRAND_SOFT = "#eef2ff"
SUCCESS = "#12a76a"
SUCCESS_SOFT = "#eafaf3"
ERROR = "#d92d20"
ERROR_SOFT = "#fef3f2"
WARNING = "#b54708"
WARNING_SOFT = "#fff7ed"
INDETERMINATE_PROGRESS_MAXIMUM = 100
INDETERMINATE_PROGRESS_INTERVAL_MS = 12
OFFICIAL_WEBSITE = "https://dsdesktop.com"

STEP_DATA = (
    ("01", "step_prepare_title", "step_prepare_description"),
    ("02", "step_start_title", "step_start_description"),
)
STEP_INDEX = {"prepare": 0, "start": 1}
ACTIVITY_COPY_KEYS = {
    "waiting_for_lock": "activity_waiting_for_lock",
    "checking_runtime": "activity_checking_runtime",
    "resolving_version": "activity_resolving_version",
    "downloading_node": "activity_downloading_node",
    "verifying_node": "activity_verifying_node",
    "checking_sources": "activity_checking_sources",
    "installing_harness": "activity_installing_harness",
    "validating_harness": "activity_validating_harness",
    "activating_harness": "activity_activating_harness",
    "starting_service": "activity_starting_service",
}


def format_uptime(seconds: float) -> str:
    """Render an elapsed second count as ``HH:MM:SS``."""
    elapsed = int(seconds)
    hours, remainder = divmod(elapsed, 3600)
    minutes, seconds = divmod(remainder, 60)
    return f"{hours:02}:{minutes:02}:{seconds:02}"


def wrap_text_to_width(text: str, width: int, measure: Callable[[str], int]) -> str:
    """Insert line breaks so every rendered line fits the available pixel width."""
    if width <= 0 or measure(text) <= width:
        return text
    words = text.split()
    units = words if len(words) > 1 else list(text)
    separator = " " if len(words) > 1 else ""
    lines: list[str] = []
    current = ""
    for unit in units:
        candidate = f"{current}{separator}{unit}" if current else unit
        if current and measure(candidate) > width:
            lines.append(current)
            current = unit
        else:
            current = candidate
    if current:
        lines.append(current)
    return "\n".join(lines)


def button_layout_without_focus(*, border: bool) -> list[tuple[str, dict[str, object]]]:
    """Build a ttk button layout without the dotted ``Button.focus`` element."""
    content: list[tuple[str, dict[str, object]]] = [
        ("Button.padding", {
            "sticky": "nswe",
            "children": [("Button.label", {"sticky": "nswe"})],
        }),
    ]
    if not border:
        return content
    return [("Button.border", {"sticky": "nswe", "children": content})]


class DesktopApp:
    """Owns the Tk window and the deploy/start worker thread."""

    def __init__(self, root: Tk):
        self.root = root
        self.paths = APPLICATION_PATHS
        self.language = load_language(self.paths.language_file)
        self._copy_bindings: dict[str, tuple[StringVar, str | None, dict[str, object]]] = {}
        self.server = ServerManager(self.paths)
        self.queue: queue.Queue[tuple[str, object]] = queue.Queue()
        self.browser_choices = discover_browsers()
        self.selected_browser: BrowserChoice = self.browser_choices[0]
        self.browser_menu_button: ttk.Button | None = None
        self.browser_menu: Menu | None = None
        self._browser_arrow: PhotoImage | None = None
        self.language_control: ttk.Frame | None = None
        self.language_menu_button: ttk.Button | None = None
        self._language_arrow: PhotoImage | None = None
        self.close_hint_label: ttk.Label | None = None
        self.close_hint_display_var: StringVar | None = None
        self._hint_font = None
        self.service_started_at: float | None = None
        self._activity_started_at: float | None = None
        self._activity_copy_key: str | None = None
        self._activity_values: dict[str, object] = {}
        self.web_url: str | None = None
        self._progress_indeterminate = True
        self._closing = False
        self._tray_ready = False
        self.tray: TrayController | None = None
        self._deployment_controller: DeploymentController | None = None
        self._worker_thread: threading.Thread | None = None
        self._available_version: str | None = None
        self._retry_force = False
        self._retry_target: str | None = None
        self._logo: PhotoImage | None = None
        self.web_url_button: ttk.Button | None = None
        self._copy_feedback_after_id: str | None = None
        self.step_rows: list[ttk.Frame] = []
        self.step_label_frames: list[ttk.Frame] = []
        self.step_numbers: list[ttk.Label] = []
        self.step_titles: list[ttk.Label] = []
        self.step_descriptions: list[ttk.Label] = []
        self._configure_styles()
        self._build_shell()
        self._start_tray()
        self._start_worker()

    def _copy_var(self, name: str, key: str, **values: object) -> StringVar:
        variable = StringVar(value=translate(self.language, key, **values))
        self._copy_bindings[name] = (variable, key, values)
        return variable

    def _set_copy(self, name: str, key: str, **values: object) -> None:
        variable, _old_key, _old_values = self._copy_bindings[name]
        self._copy_bindings[name] = (variable, key, values)
        variable.set(translate(self.language, key, **values))

    def _set_raw_copy(self, name: str, value: str) -> None:
        variable, _old_key, _old_values = self._copy_bindings[name]
        self._copy_bindings[name] = (variable, None, {})
        variable.set(value)

    def _refresh_copy(self) -> None:
        for variable, key, values in self._copy_bindings.values():
            if key is not None:
                variable.set(translate(self.language, key, **values))

    def _set_language(self, language: str) -> None:
        if language == self.language:
            return
        self.language = language
        self.language_choice_var.set(language)
        try:
            save_language(self.paths.language_file, language)
        except OSError as error:
            print(f"could not save desktop language preference: {error}", file=sys.stderr)
        self._refresh_copy()
        self._refresh_browser_menu()
        self._refresh_tray_menu()
        self.root.after_idle(self._resize_close_hint)

    def _browser_label(self, browser: BrowserChoice) -> str:
        if browser.key == "default":
            return translate(self.language, "default_browser")
        return browser.label

    def _configure_styles(self) -> None:
        style = ttk.Style(self.root)
        try:
            style.theme_use("clam")
        except Exception:
            pass
        ui = str(tkfont.nametofont("TkDefaultFont", root=self.root).actual("family"))
        style.configure(".", font=(ui, 10))
        style.configure("Root.TFrame", background=BACKGROUND)
        style.configure("Surface.TFrame", background=SURFACE)
        style.configure("Border.TFrame", background=BORDER)
        style.configure("Sidebar.TFrame", background=SIDEBAR)
        style.configure(
            "BrandLogo.TButton", background=SIDEBAR, padding=0, borderwidth=0,
            relief="flat", focusthickness=0,
        )
        style.configure("SidebarBrand.TLabel", background=SIDEBAR, foreground=INK, font=(ui, 15, "bold"))
        style.configure("SidebarSub.TLabel", background=SIDEBAR, foreground=MUTED, font=(ui, 9))
        style.configure("SidebarCaption.TLabel", background=SIDEBAR, foreground=SOFT_MUTED, font=(ui, 9, "bold"))
        style.configure("LanguageBorder.TFrame", background=BORDER)
        style.configure(
            "Language.TButton", background=SIDEBAR, foreground=MUTED,
            padding=(8, 5), borderwidth=0, relief="flat", anchor="w",
            focusthickness=0, font=(ui, 9),
        )
        style.configure(
            "LanguageArrow.TButton", background=SIDEBAR, foreground=SOFT_MUTED,
            padding=(6, 4), borderwidth=0, relief="flat", focusthickness=0,
        )
        style.map(
            "Language.TButton", background=[("active", BRAND_SOFT), ("focus", BRAND_SOFT)],
            foreground=[("active", INK)],
        )
        style.map(
            "LanguageArrow.TButton", background=[("active", BRAND_SOFT), ("focus", BRAND_SOFT)],
            foreground=[("active", BRAND_DARK)],
        )
        style.configure("StepRow.TFrame", background=SIDEBAR)
        style.configure("StepRowActive.TFrame", background=BRAND_SOFT)
        style.configure("StepNumber.TLabel", background=SIDEBAR, foreground=SOFT_MUTED, font=(ui, 9, "bold"))
        style.configure("StepNumberActive.TLabel", background=BRAND_SOFT, foreground=BRAND, font=(ui, 9, "bold"))
        style.configure("StepNumberDone.TLabel", background=SIDEBAR, foreground=SUCCESS, font=(ui, 10, "bold"))
        style.configure("StepTitle.TLabel", background=SIDEBAR, foreground=MUTED, font=(ui, 10, "bold"))
        style.configure("StepTitleActive.TLabel", background=BRAND_SOFT, foreground=INK, font=(ui, 10, "bold"))
        style.configure("StepTitleDone.TLabel", background=SIDEBAR, foreground=INK, font=(ui, 10, "bold"))
        style.configure("StepDescription.TLabel", background=SIDEBAR, foreground=SOFT_MUTED, font=(ui, 8))
        style.configure("StepDescriptionActive.TLabel", background=BRAND_SOFT, foreground=MUTED, font=(ui, 8))
        style.configure("StepDescriptionDone.TLabel", background=SIDEBAR, foreground=MUTED, font=(ui, 8))
        style.configure("Eyebrow.TLabel", background=BACKGROUND, foreground=BRAND, font=(ui, 9, "bold"))
        style.configure("Title.TLabel", background=BACKGROUND, foreground=INK, font=(ui, 22, "bold"))
        style.configure("Subtitle.TLabel", background=BACKGROUND, foreground=MUTED, font=(ui, 10))
        style.configure("CardTitle.TLabel", background=SURFACE, foreground=INK, font=(ui, 12, "bold"))
        style.configure("CardBody.TLabel", background=SURFACE, foreground=MUTED, font=(ui, 9))
        style.configure("FieldLabel.TLabel", background=SURFACE, foreground=MUTED, font=(ui, 9))
        style.configure("FieldValue.TLabel", background=SURFACE, foreground=INK, font=(ui, 10, "bold"))
        style.configure(
            "Address.TButton", background=SURFACE, foreground=BRAND_DARK,
            padding=0, borderwidth=0, relief="flat", anchor="e",
            focusthickness=0, font=(ui, 10, "bold", "underline"),
        )
        style.configure(
            "StatusBadge.TLabel", background=BRAND_SOFT, foreground=BRAND_DARK,
            font=(ui, 9, "bold"), padding=(9, 5),
        )
        style.configure(
            "StatusBadgeSuccess.TLabel", background=SUCCESS_SOFT, foreground=SUCCESS,
            font=(ui, 9, "bold"), padding=(9, 5),
        )
        style.configure(
            "StatusBadgeError.TLabel", background=ERROR_SOFT, foreground=ERROR,
            font=(ui, 9, "bold"), padding=(9, 5),
        )
        style.configure("ProgressText.TLabel", background=SURFACE, foreground=MUTED, font=(ui, 9))
        style.configure("Percent.TLabel", background=SURFACE, foreground=BRAND_DARK, font=(ui, 9, "bold"))
        self._hint_font = tkfont.Font(root=self.root, family=ui, size=9)
        style.configure("Hint.TLabel", background=BACKGROUND, foreground=SOFT_MUTED, font=self._hint_font)
        style.configure("Update.TFrame", background=WARNING_SOFT)
        style.configure("Update.TLabel", background=WARNING_SOFT, foreground=WARNING, font=(ui, 9))
        style.configure(
            "Primary.TButton", background=BRAND, foreground="#ffffff", padding=(22, 12),
            borderwidth=1, bordercolor=BRAND, lightcolor=BRAND, darkcolor=BRAND,
            focusthickness=0, font=(ui, 10, "bold"),
        )
        style.map(
            "BrandLogo.TButton",
            background=[("active", BRAND_SOFT), ("focus", BRAND_SOFT)],
        )
        style.map(
            "Address.TButton",
            background=[("active", BRAND_SOFT), ("focus", BRAND_SOFT), ("disabled", SURFACE)],
            foreground=[("active", BRAND_DARK), ("disabled", INK)],
        )
        style.map(
            "Primary.TButton",
            background=[("active", BRAND_DARK), ("disabled", "#e4e7ec")],
            foreground=[("disabled", SOFT_MUTED)],
            bordercolor=[("active", BRAND_DARK), ("disabled", "#e4e7ec")],
            lightcolor=[("active", BRAND_DARK), ("disabled", "#e4e7ec")],
            darkcolor=[("active", BRAND_DARK), ("disabled", "#e4e7ec")],
        )
        style.configure(
            "BrowserMenu.TButton", background=BRAND, foreground="#ffffff",
            padding=(13, 12), borderwidth=1, bordercolor=BRAND,
            lightcolor=BRAND, darkcolor=BRAND, focusthickness=0,
            font=(ui, 10, "bold"),
        )
        style.map(
            "BrowserMenu.TButton",
            background=[("active", BRAND_DARK), ("disabled", "#e4e7ec")],
            foreground=[("disabled", SOFT_MUTED)],
            bordercolor=[("active", BRAND_DARK), ("disabled", "#e4e7ec")],
            lightcolor=[("active", BRAND_DARK), ("disabled", "#e4e7ec")],
            darkcolor=[("active", BRAND_DARK), ("disabled", "#e4e7ec")],
        )
        style.configure(
            "Update.TButton", background=WARNING_SOFT, foreground=BRAND_DARK,
            padding=(10, 5), borderwidth=0, focusthickness=0, font=(ui, 9, "bold"),
        )
        style.map(
            "Update.TButton",
            background=[("active", "#ffedd5"), ("focus", "#ffedd5")],
        )
        borderless_button_layout = button_layout_without_focus(border=False)
        bordered_button_layout = button_layout_without_focus(border=True)
        for button_style in (
            "BrandLogo.TButton",
            "Language.TButton",
            "LanguageArrow.TButton",
            "Address.TButton",
            "Update.TButton",
        ):
            style.layout(button_style, borderless_button_layout)
        for button_style in ("Primary.TButton", "BrowserMenu.TButton"):
            style.layout(button_style, bordered_button_layout)
        style.configure(
            "Horizontal.TProgressbar", troughcolor="#e8ecf2", background=BRAND,
            borderwidth=0, thickness=6,
        )

    def _load_image(self, name: str) -> PhotoImage | None:
        try:
            path = resource_root() / "assets" / name
            if path.is_file():
                return PhotoImage(file=str(path))
        except Exception:
            pass
        return None

    def _make_browser_arrow(self) -> PhotoImage:
        """Create a centered white down-triangle with a transparent surround."""
        arrow = PhotoImage(width=14, height=10)
        for row, (left, right) in enumerate(
            ((1, 13), (2, 12), (3, 11), (4, 10), (5, 9), (6, 8)),
            start=2,
        ):
            arrow.put("#ffffff", to=(left, row, right, row + 1))
        return arrow

    def _make_language_arrow(self) -> PhotoImage:
        """Create a medium muted triangle centered in a square image."""
        arrow = PhotoImage(width=14, height=14)
        for row, (left, right) in enumerate(
            ((2, 12), (3, 11), (4, 10), (5, 9), (6, 8)),
            start=5,
        ):
            arrow.put(MUTED, to=(left, row, right, row + 1))
        return arrow

    def _build_shell(self) -> None:
        self.root.title(APP_NAME)
        self.root.geometry("760x480")
        self.root.minsize(700, 460)
        self.root.configure(bg=BACKGROUND)
        self.root.protocol("WM_DELETE_WINDOW", self._handle_window_close)
        try:
            self.root.createcommand("tk::mac::Quit", self.quit_app)
            self.root.createcommand("tk::mac::ReopenApplication", self.show_main_window)
        except TclError:
            pass

        shell = ttk.Frame(self.root, style="Root.TFrame")
        shell.pack(fill=BOTH, expand=True)
        self._build_sidebar(shell)
        self._build_content(shell)
        self._tick_uptime()
        self.root.after(100, self._poll_queue)

    def _build_sidebar(self, shell: ttk.Frame) -> None:
        sidebar_border = ttk.Frame(shell, width=221, style="Border.TFrame")
        sidebar_border.pack(side=LEFT, fill=Y)
        sidebar_border.pack_propagate(False)
        sidebar = ttk.Frame(sidebar_border, width=220, style="Sidebar.TFrame")
        sidebar.pack(fill=BOTH, expand=True, padx=(0, 1))
        sidebar.pack_propagate(False)

        brand = ttk.Frame(sidebar, style="Sidebar.TFrame")
        brand.pack(fill=X, padx=20, pady=(22, 20))
        self._logo = self._load_image("logo-blue.png")
        if self._logo is not None:
            ttk.Button(
                brand,
                image=self._logo,
                command=self.open_official_website,
                cursor="hand2",
                style="BrandLogo.TButton",
            ).pack(side=LEFT)
        brand_copy = ttk.Frame(brand, style="Sidebar.TFrame")
        brand_copy.pack(side=LEFT, padx=(11, 0))
        ttk.Label(brand_copy, text="DSH", style="SidebarBrand.TLabel").pack(anchor="w")
        ttk.Label(brand_copy, text="LAUNCHER", style="SidebarSub.TLabel").pack(anchor="w", pady=(2, 0))

        divider = ttk.Frame(sidebar, style="Border.TFrame", height=1)
        divider.pack(fill=X)
        divider.pack_propagate(False)
        startup_flow_var = self._copy_var("startup_flow", "startup_flow")
        ttk.Label(
            sidebar, textvariable=startup_flow_var, style="SidebarCaption.TLabel",
        ).pack(anchor="w", padx=24, pady=(24, 10))

        steps = ttk.Frame(sidebar, style="Sidebar.TFrame")
        steps.pack(fill=X, padx=12)
        for index, (number, title_key, description_key) in enumerate(STEP_DATA):
            row = ttk.Frame(steps, style="StepRow.TFrame", padding=(12, 11))
            row.pack(fill=X, pady=3)
            number_label = ttk.Label(row, text=number, width=3, style="StepNumber.TLabel")
            number_label.pack(side=LEFT, anchor="n", pady=(1, 0))
            labels = ttk.Frame(row, style="StepRow.TFrame")
            labels.pack(side=LEFT, fill=X, expand=True)
            title_var = self._copy_var(f"step_title_{index}", title_key)
            title_label = ttk.Label(labels, textvariable=title_var, style="StepTitle.TLabel")
            title_label.pack(anchor="w")
            description_var = self._copy_var(f"step_description_{index}", description_key)
            description_label = ttk.Label(
                labels, textvariable=description_var, style="StepDescription.TLabel",
                wraplength=135, justify=LEFT,
            )
            description_label.pack(anchor="w", pady=(3, 0))
            self.step_rows.append(row)
            self.step_label_frames.append(labels)
            self.step_numbers.append(number_label)
            self.step_titles.append(title_label)
            self.step_descriptions.append(description_label)

        sidebar_footer = ttk.Frame(sidebar, style="Sidebar.TFrame")
        sidebar_footer.pack(side="bottom", fill=X, padx=20, pady=(0, 20))
        self.language_choice_var = StringVar(value=self.language)
        self.language_menu = Menu(sidebar_footer, tearoff=False)
        for language, label in (("zh", "中文"), ("en", "English")):
            self.language_menu.add_radiobutton(
                label=label,
                variable=self.language_choice_var,
                value=language,
                command=lambda selected=language: self._set_language(selected),
            )
        language_label_var = self._copy_var("language_label", "language_label")
        self.language_control = ttk.Frame(sidebar_footer, style="Sidebar.TFrame")
        self.language_control.pack(anchor="w", fill=X, pady=(0, 29))
        self.language_control.grid_columnconfigure(1, weight=1)
        ttk.Frame(
            self.language_control, style="LanguageBorder.TFrame", height=1,
        ).grid(row=0, column=0, columnspan=4, sticky="ew")
        ttk.Frame(
            self.language_control, style="LanguageBorder.TFrame", width=1,
        ).grid(row=1, column=0, sticky="ns")
        self.language_menu_button = ttk.Button(
            self.language_control,
            textvariable=language_label_var,
            command=self._show_language_menu,
            style="Language.TButton",
        )
        self.language_menu_button.grid(row=1, column=1, sticky="nsew")
        self.language_menu_button.bind("<Down>", lambda _event: self._show_language_menu())
        self._language_arrow = self._make_language_arrow()
        ttk.Button(
            self.language_control,
            image=self._language_arrow,
            takefocus=False,
            command=self._show_language_menu,
            style="LanguageArrow.TButton",
        ).grid(row=1, column=2, sticky="ns")
        ttk.Frame(
            self.language_control, style="LanguageBorder.TFrame", width=1,
        ).grid(row=1, column=3, sticky="ns")
        ttk.Frame(
            self.language_control, style="LanguageBorder.TFrame", height=1,
        ).grid(row=2, column=0, columnspan=4, sticky="ew")
        ttk.Label(sidebar_footer, text=f"DESKTOP  ·  v{APP_VERSION}", style="SidebarSub.TLabel").pack(anchor="w")
        self.version_var = StringVar(value=f"HARNESS  ·  v{installed_version(self.paths) or APP_VERSION}")
        ttk.Label(
            sidebar_footer, textvariable=self.version_var, style="SidebarSub.TLabel",
        ).pack(anchor="w", pady=(4, 0))

    def _build_content(self, shell: ttk.Frame) -> None:
        content = ttk.Frame(shell, style="Root.TFrame", padding=(36, 30, 36, 28))
        content.pack(side=RIGHT, fill=BOTH, expand=True)
        content.grid_columnconfigure(0, weight=1)
        content.grid_rowconfigure(5, weight=1)

        eyebrow_var = self._copy_var("workspace_eyebrow", "workspace_eyebrow")
        ttk.Label(content, textvariable=eyebrow_var, style="Eyebrow.TLabel").grid(row=0, column=0, sticky="w")
        self.status_var = self._copy_var("status", "status_preparing")
        ttk.Label(content, textvariable=self.status_var, style="Title.TLabel").grid(
            row=1, column=0, sticky="w", pady=(5, 0),
        )
        self.detail_var = self._copy_var("detail", "detail_preparing")
        ttk.Label(
            content, textvariable=self.detail_var, style="Subtitle.TLabel", wraplength=470,
        ).grid(row=2, column=0, sticky="w", pady=(7, 22))

        card_border = ttk.Frame(content, style="Border.TFrame")
        card_border.grid(row=3, column=0, sticky="ew")
        card = ttk.Frame(card_border, style="Surface.TFrame", padding=(22, 20))
        card.pack(fill=BOTH, expand=True, padx=1, pady=1)
        card.grid_columnconfigure(1, weight=1)
        service_title_var = self._copy_var("service_title", "service_title")
        ttk.Label(card, textvariable=service_title_var, style="CardTitle.TLabel").grid(row=0, column=0, sticky="w")
        self.badge_var = self._copy_var("badge", "badge_preparing")
        self.badge = ttk.Label(card, textvariable=self.badge_var, style="StatusBadge.TLabel")
        self.badge.grid(row=0, column=1, sticky="e")
        self.card_message_var = self._copy_var("card_message", "checking_components")
        ttk.Label(card, textvariable=self.card_message_var, style="CardBody.TLabel").grid(
            row=1, column=0, columnspan=2, sticky="w", pady=(6, 18),
        )

        self.progress_row = ttk.Frame(card, style="Surface.TFrame")
        self.progress_row.grid(row=2, column=0, columnspan=2, sticky="ew")
        self.progress_row.grid_columnconfigure(0, weight=1)
        self.progress = ttk.Progressbar(self.progress_row, mode="indeterminate")
        self.progress.grid(row=0, column=0, columnspan=2, sticky="ew")
        self.progress_label_var = self._copy_var("progress_label", "preparing")
        ttk.Label(
            self.progress_row, textvariable=self.progress_label_var,
            style="ProgressText.TLabel",
        ).grid(row=1, column=0, sticky="w", pady=(9, 0))
        self.percent_var = StringVar(value="")
        ttk.Label(
            self.progress_row, textvariable=self.percent_var, style="Percent.TLabel",
        ).grid(row=1, column=1, sticky="e", pady=(9, 0))

        divider = ttk.Frame(card, style="Border.TFrame", height=1)
        divider.grid(row=3, column=0, columnspan=2, sticky="ew", pady=(18, 16))
        divider.grid_propagate(False)
        web_ui_address_var = self._copy_var("web_ui_address", "web_ui_address")
        ttk.Label(card, textvariable=web_ui_address_var, style="FieldLabel.TLabel").grid(row=4, column=0, sticky="w")
        self.web_url_var = self._copy_var("web_url", "waiting_address")
        self.web_url_button = ttk.Button(
            card,
            textvariable=self.web_url_var,
            command=self.copy_web_url,
            cursor="hand2",
            state="disabled",
            style="Address.TButton",
        )
        self.web_url_button.grid(
            row=4, column=1, sticky="e",
        )
        runtime_status_var = self._copy_var("runtime_status", "runtime_status")
        ttk.Label(card, textvariable=runtime_status_var, style="FieldLabel.TLabel").grid(
            row=5, column=0, sticky="w", pady=(14, 0),
        )
        self.runtime_var = self._copy_var("runtime", "waiting_service")
        ttk.Label(card, textvariable=self.runtime_var, style="FieldValue.TLabel").grid(
            row=5, column=1, sticky="e", pady=(14, 0),
        )

        self.update_banner = ttk.Frame(content, style="Update.TFrame", padding=(13, 8))
        self.update_label_var = self._copy_var("update_label", "update_available", version="")
        self.update_label = ttk.Label(
            self.update_banner, textvariable=self.update_label_var, style="Update.TLabel",
        )
        self.update_label.pack(side=LEFT)
        update_button_var = self._copy_var("update_button", "update_now")
        self.update_button = ttk.Button(
            self.update_banner, textvariable=update_button_var, style="Update.TButton",
            command=self._update_harness,
        )
        self.update_button.pack(side=RIGHT)

        actions = ttk.Frame(content, style="Root.TFrame")
        actions.grid(row=6, column=0, sticky="ew", pady=(22, 0))
        actions.grid_columnconfigure(0, weight=1)
        close_hint_var = self._copy_var("close_hint", "close_to_tray")
        self.close_hint_display_var = StringVar(value=close_hint_var.get())
        self.close_hint_label = ttk.Label(
            actions, textvariable=self.close_hint_display_var, style="Hint.TLabel",
            justify=LEFT,
        )
        self.close_hint_label.bind("<Configure>", self._resize_close_hint)
        self.close_hint_label.grid(row=0, column=0, sticky="ew", padx=(0, 12))
        self.root.after_idle(self._resize_close_hint)
        if len(self.browser_choices) > 1:
            self.browser_var = StringVar(value=self.selected_browser.key)
            self.browser_menu = Menu(actions, tearoff=False)
            for browser in self.browser_choices:
                self.browser_menu.add_radiobutton(
                    label=self._browser_label(browser),
                    variable=self.browser_var,
                    value=browser.key,
                    command=lambda key=browser.key: self._select_browser(key),
                )
            self._browser_arrow = self._make_browser_arrow()
            self.browser_menu_button = ttk.Button(
                actions,
                image=self._browser_arrow,
                command=self._show_browser_menu,
                style="BrowserMenu.TButton",
            )
            self.browser_menu_button.grid(row=0, column=2, sticky="e")
        self.open_button_var = self._copy_var("primary_button", "open_web_ui")
        self.open_button = ttk.Button(
            actions, textvariable=self.open_button_var, style="Primary.TButton",
            command=self.open_web_ui,
        )
        self.open_button.grid(
            row=0, column=1, sticky="e",
            padx=(0, 2) if self.browser_menu_button else 0,
        )
        self._set_button_disabled("starting")

    def _resize_close_hint(self, event=None) -> None:
        if (
            self.close_hint_label is None
            or self.close_hint_display_var is None
            or self._hint_font is None
        ):
            return
        width = max(
            int(event.width) if event is not None else self.close_hint_label.winfo_width(),
            1,
        )
        source_var, _key, _values = self._copy_bindings["close_hint"]
        wrapped = wrap_text_to_width(source_var.get(), width, self._hint_font.measure)
        if self.close_hint_display_var.get() != wrapped:
            self.close_hint_display_var.set(wrapped)

    def _open_button_copy(self) -> tuple[str, dict[str, object]]:
        if len(self.browser_choices) > 1:
            return "open_with_browser", {"browser": self._browser_label(self.selected_browser)}
        return "open_web_ui", {}

    def _select_browser(self, key: str) -> None:
        selected = next((browser for browser in self.browser_choices if browser.key == key), None)
        if selected is None:
            return
        self.selected_browser = selected
        if self.web_url is not None:
            key, values = self._open_button_copy()
            self._set_copy("primary_button", key, **values)

    def _refresh_browser_menu(self) -> None:
        if self.browser_menu is None:
            return
        self.browser_menu.delete(0, "end")
        for browser in self.browser_choices:
            self.browser_menu.add_radiobutton(
                label=self._browser_label(browser),
                variable=self.browser_var,
                value=browser.key,
                command=lambda key=browser.key: self._select_browser(key),
            )
        if self.web_url is not None:
            key, values = self._open_button_copy()
            self._set_copy("primary_button", key, **values)

    def _show_browser_menu(self) -> None:
        if self.browser_menu is None or self.browser_menu_button is None:
            return
        x = self.browser_menu_button.winfo_rootx()
        y = self.browser_menu_button.winfo_rooty() + self.browser_menu_button.winfo_height()
        try:
            self.browser_menu.tk_popup(x, y)
        finally:
            self.browser_menu.grab_release()

    def _show_language_menu(self) -> None:
        if self.language_control is None:
            return
        x = self.language_control.winfo_rootx()
        y = self.language_control.winfo_rooty() + self.language_control.winfo_height()
        try:
            self.language_menu.tk_popup(x, y)
        finally:
            self.language_menu.grab_release()

    def _set_button_enabled(
        self, key: str, command, *, browser_selection: bool = False, **values: object,
    ) -> None:
        self._set_copy("primary_button", key, **values)
        self.open_button.configure(command=command, state="normal")
        if self.browser_menu_button is not None:
            self.browser_menu_button.configure(state="normal" if browser_selection else "disabled")

    def _set_button_disabled(self, key: str, **values: object) -> None:
        self._set_copy("primary_button", key, **values)
        self.open_button.configure(state="disabled")
        if self.browser_menu_button is not None:
            self.browser_menu_button.configure(state="disabled")

    def open_web_ui(self) -> bool:
        if self.web_url is None:
            self._set_copy("status", "address_not_ready")
            self._set_copy("detail", "wait_for_service")
            return False
        try:
            opened = open_in_browser(self.selected_browser, self.web_url)
        except Exception:
            opened = False
        if not opened:
            self._set_copy("status", "browser_open_failed")
            self._set_copy("detail", "manual_visit", url=self.web_url)
            return False
        return True

    def open_official_website(self) -> None:
        """Open the DSH Desktop website with the currently selected browser."""
        try:
            opened = open_in_browser(self.selected_browser, OFFICIAL_WEBSITE)
        except Exception:
            opened = False
        if not opened:
            self._set_copy("status", "browser_open_failed")
            self._set_copy("detail", "manual_visit", url=OFFICIAL_WEBSITE)

    def copy_web_url(self) -> None:
        """Copy the announced Web UI address and show brief inline feedback."""
        if self.web_url is None:
            return
        try:
            self.root.clipboard_clear()
            self.root.clipboard_append(self.web_url)
        except TclError:
            return
        if self._copy_feedback_after_id is not None:
            self.root.after_cancel(self._copy_feedback_after_id)
        self._set_copy("web_url", "address_copied", url=self.web_url)
        self._copy_feedback_after_id = self.root.after(1400, self._restore_web_url_copy)

    def _restore_web_url_copy(self) -> None:
        self._copy_feedback_after_id = None
        if self.web_url is not None:
            self._set_raw_copy("web_url", self.web_url)

    def _start_worker(self, force: bool = False, target_version: str | None = None) -> None:
        if self._worker_thread is not None and self._worker_thread.is_alive():
            return
        self._retry_force = force
        self._retry_target = target_version
        self.service_started_at = None
        self._activity_started_at = None
        self._activity_copy_key = None
        self._activity_values = {}
        self.web_url = None
        if self._copy_feedback_after_id is not None:
            self.root.after_cancel(self._copy_feedback_after_id)
            self._copy_feedback_after_id = None
        self._set_copy("web_url", "waiting_address")
        self._refresh_tray_menu()
        if self.web_url_button is not None:
            self.web_url_button.configure(state="disabled")
        self._set_button_disabled("starting")
        self.update_banner.grid_remove()
        self.progress_row.grid()
        self.badge.configure(style="StatusBadge.TLabel")
        self._set_copy("badge", "badge_preparing")
        self._set_copy("runtime", "waiting_service")
        self._set_progress_indeterminate()
        controller = DeploymentController()
        self._deployment_controller = controller
        self._worker_thread = threading.Thread(
            target=self._worker,
            args=(force, target_version, controller),
            name="dsh-runtime-deployment",
            daemon=True,
        )
        self._worker_thread.start()

    def _worker(
        self,
        force: bool,
        target_version: str | None,
        controller: DeploymentController,
    ) -> None:
        try:
            if force:
                self.server.stop()
            deploy_runtime(
                self.paths,
                on_step=lambda key: self.queue.put(("step", key)),
                on_progress=lambda done, total: self.queue.put(("progress", (done, total))),
                on_activity=lambda key, values: self.queue.put(("activity", (key, values))),
                force=force,
                target_version=target_version,
                controller=controller,
            )
            self.queue.put(("step", "start"))
            controller.check()
            self.server.start(
                on_ready=lambda message: self.queue.put(("ready", message)),
                on_error=lambda message: self.queue.put(("error", message)),
            )
            if self.server.is_running:
                self._maybe_check_update(controller)
        except DeploymentCancelled:
            if not self._closing:
                self.queue.put(("error", LocalizedText("deployment_cancelled", {})))
        except LocalizedError as error:
            self.queue.put(("error", error.text))
        except Exception as error:  # unexpected deployment failures remain actionable
            self.queue.put(("error", str(error)))

    def _update_harness(self) -> None:
        if self._available_version is None:
            return
        self._set_copy("status", "updating_harness")
        self._set_copy("detail", "update_restart_detail")
        self._set_copy("card_message", "installing_version")
        self._set_copy("progress_label", "updating")
        self._start_worker(force=True, target_version=self._available_version)

    def _maybe_check_update(self, controller: DeploymentController) -> None:
        try:
            current = installed_version(self.paths)
            if current:
                self.queue.put(("version", current))
            latest = latest_harness_version(controller=controller)
            if current and is_newer_version(latest, current):
                self.queue.put(("update", latest))
        except Exception:
            pass  # version checks never block startup

    def _set_progress_indeterminate(self) -> None:
        self.progress.stop()
        self.progress.configure(
            mode="indeterminate",
            maximum=INDETERMINATE_PROGRESS_MAXIMUM,
            value=0,
        )
        self._progress_indeterminate = True
        self.percent_var.set("")
        self.progress.start(INDETERMINATE_PROGRESS_INTERVAL_MS)

    def _begin_activity(self, key: str, values: dict[str, object]) -> None:
        self._activity_copy_key = ACTIVITY_COPY_KEYS[key]
        self._activity_values = dict(values)
        self._activity_started_at = time.monotonic()
        self._set_progress_indeterminate()
        self._render_activity()

    def _render_activity(self) -> None:
        if self._activity_started_at is None or self._activity_copy_key is None:
            return
        elapsed = format_uptime(time.monotonic() - self._activity_started_at)
        self._set_copy(
            "progress_label",
            self._activity_copy_key,
            **self._activity_values,
            elapsed=elapsed,
        )

    def _set_progress_determinate(self, value: int, total: int) -> None:
        if self._progress_indeterminate:
            self.progress.stop()
        self._progress_indeterminate = False
        self.progress.configure(mode="determinate", maximum=total, value=value)
        if total > 0:
            self.percent_var.set(f"{int(value * 100 / total)}%")

    def _set_step(self, index: int) -> None:
        for i in range(len(self.step_rows)):
            is_done = i < index
            is_active = i == index
            row_style = "StepRowActive.TFrame" if is_active else "StepRow.TFrame"
            self.step_rows[i].configure(style=row_style)
            self.step_label_frames[i].configure(style=row_style)
            if is_done:
                self.step_numbers[i].configure(text="✓", style="StepNumberDone.TLabel")
                self.step_titles[i].configure(style="StepTitleDone.TLabel")
                self.step_descriptions[i].configure(style="StepDescriptionDone.TLabel")
            elif is_active:
                self.step_numbers[i].configure(text=STEP_DATA[i][0], style="StepNumberActive.TLabel")
                self.step_titles[i].configure(style="StepTitleActive.TLabel")
                self.step_descriptions[i].configure(style="StepDescriptionActive.TLabel")
            else:
                self.step_numbers[i].configure(text=STEP_DATA[i][0], style="StepNumber.TLabel")
                self.step_titles[i].configure(style="StepTitle.TLabel")
                self.step_descriptions[i].configure(style="StepDescription.TLabel")
        if index == 0:
            self._set_copy("status", "status_preparing")
            self._set_copy("detail", "preparing_detail")
            self._set_copy("card_message", "preparing_components")
            self._set_copy("progress_label", "preparing_runtime")
            self._set_progress_indeterminate()
        else:
            self._set_copy("status", "starting_service")
            self._set_copy("detail", "starting_detail")
            self._set_copy("card_message", "starting_web_service")
            self._begin_activity("starting_service", {})

    def _show_started(self, web_url: str) -> None:
        for i in range(len(self.step_rows)):
            self.step_rows[i].configure(style="StepRow.TFrame")
            self.step_label_frames[i].configure(style="StepRow.TFrame")
            self.step_numbers[i].configure(text="✓", style="StepNumberDone.TLabel")
            self.step_titles[i].configure(style="StepTitleDone.TLabel")
            self.step_descriptions[i].configure(style="StepDescriptionDone.TLabel")
        self.service_started_at = time.monotonic()
        self._activity_started_at = None
        self._activity_copy_key = None
        self._activity_values = {}
        self.web_url = web_url
        self._set_raw_copy("web_url", web_url)
        if self.web_url_button is not None:
            self.web_url_button.configure(state="normal")
        self._set_copy("status", "workspace_ready")
        self._set_copy("detail", "workspace_ready_detail")
        self._set_copy("badge", "badge_running")
        self.badge.configure(style="StatusBadgeSuccess.TLabel")
        self._set_copy("card_message", "workspace_available")
        if self._progress_indeterminate:
            self.progress.stop()
        self.progress_row.grid_remove()
        self._set_copy("runtime", "runtime_running", elapsed="00:00:00")
        button_key, button_values = self._open_button_copy()
        self._set_button_enabled(
            button_key,
            self.open_web_ui,
            browser_selection=True,
            **button_values,
        )
        self._refresh_tray_menu()

    def _show_error(self, error: object) -> None:
        self._activity_started_at = None
        self._activity_copy_key = None
        self._activity_values = {}
        if self._progress_indeterminate:
            self.progress.stop()
        self.progress_row.grid_remove()
        self._set_copy("status", "startup_problem")
        if isinstance(error, LocalizedText):
            self._set_copy("detail", error.key, **error.values)
        else:
            self._set_raw_copy("detail", str(error))
        self._set_copy("badge", "badge_attention")
        self.badge.configure(style="StatusBadgeError.TLabel")
        self._set_copy("card_message", "service_failed")
        self.web_url = None
        self.service_started_at = None
        self._refresh_tray_menu()
        self._set_copy("web_url", "address_unavailable")
        if self.web_url_button is not None:
            self.web_url_button.configure(state="disabled")
        self._set_copy("runtime", "not_running")
        self._set_button_enabled(
            "retry",
            lambda: self._start_worker(self._retry_force, self._retry_target),
        )

    def _show_update(self, latest: str) -> None:
        self._available_version = latest
        self._set_copy("update_label", "update_available", version=latest)
        self.update_banner.grid(row=4, column=0, sticky="ew", pady=(12, 0))

    def _poll_queue(self) -> None:
        try:
            while True:
                kind, payload = self.queue.get_nowait()
                if kind == "step":
                    self._set_step(STEP_INDEX[str(payload)])
                elif kind == "activity":
                    key, values = payload
                    self._begin_activity(str(key), values)
                elif kind == "progress":
                    done, total = payload
                    if total > 0:
                        self._set_progress_determinate(done, total)
                    else:
                        self._set_progress_indeterminate()
                elif kind == "ready":
                    self._show_started(str(payload))
                elif kind == "error":
                    self._show_error(payload)
                elif kind == "version":
                    self.version_var.set(f"HARNESS  ·  v{payload}")
                elif kind == "update":
                    self._show_update(str(payload))
                elif kind == "tray_show":
                    self.show_main_window()
                elif kind == "tray_open":
                    if not self.open_web_ui():
                        self.show_main_window()
                elif kind == "tray_quit":
                    self.quit_app()
                    return
        except queue.Empty:
            pass
        if not self._closing:
            self.root.after(100, self._poll_queue)

    def _tick_uptime(self) -> None:
        if self.service_started_at is not None:
            elapsed = format_uptime(time.monotonic() - self.service_started_at)
            self._set_copy("runtime", "runtime_running", elapsed=elapsed)
        elif self._activity_started_at is not None:
            self._render_activity()
        self.root.after(1000, self._tick_uptime)

    def _start_tray(self) -> None:
        self.tray = TrayController(
            resource_root() / "assets" / "tray-icon.png",
            on_show=lambda: self.queue.put(("tray_show", None)),
            on_open_web_ui=lambda: self.queue.put(("tray_open", None)),
            on_quit=lambda: self.queue.put(("tray_quit", None)),
        )
        self._tray_ready = self.tray.start(self.language, self.web_url is not None)
        if not self._tray_ready:
            self._set_copy("close_hint", "tray_unavailable_close_exits")
            self.root.after_idle(self._resize_close_hint)

    def _refresh_tray_menu(self) -> None:
        if self.tray is not None and self._tray_ready:
            self.tray.refresh(self.language, self.web_url is not None)

    def _handle_window_close(self) -> None:
        if not self._tray_ready:
            self.quit_app()
            return
        try:
            self.root.withdraw()
        except TclError:
            pass

    def show_main_window(self) -> None:
        if self._closing:
            return
        try:
            self.root.deiconify()
            self.root.state("normal")
            self.root.lift()
            self.root.focus_force()
        except TclError:
            pass

    def quit_app(self) -> None:
        if self._closing:
            return
        self._closing = True
        if self.tray is not None:
            self.tray.stop()
        self._tray_ready = False
        controller = self._deployment_controller
        worker = self._worker_thread
        if controller is not None:
            controller.cancel()
        self.server.stop()
        if worker is not None and worker.is_alive():
            worker.join(timeout=4)
        if worker is not None and worker.is_alive() and controller is not None:
            controller.cancel(force=True)
            worker.join(timeout=deployment_shutdown_timeout_seconds())
        try:
            self.root.destroy()
        except TclError:
            pass

    def close(self) -> None:
        """Compatibility alias for callers that request a full application exit."""
        self.quit_app()


def _print_check() -> int:
    """Print resolved launcher configuration without starting the GUI."""
    from app_paths import NODE_VERSION, node_dist_bases
    from runtime import npm_registries

    print(f"app: {APP_NAME} v{APP_VERSION}")
    print("server: discovered from dsh web output")
    print(f"app_home: {APPLICATION_PATHS.app_home}")
    print(f"node_bin: {APPLICATION_PATHS.node_bin}")
    print(f"dsh_bin: {APPLICATION_PATHS.dsh_bin}")
    print(f"install_log: {APPLICATION_PATHS.install_log}")
    print(f"language: {load_language(APPLICATION_PATHS.language_file)}")
    print(f"installed: {installed_version(APPLICATION_PATHS)}")
    print("harness_resolution: highest valid latest from configured registries")
    print(f"pinned_node: {NODE_VERSION}")
    print(f"node_dist_bases: {','.join(node_dist_bases())}")
    print(f"npm_registries: {','.join(npm_registries())}")
    print(f"runtime_ready: {is_runtime_ready(APPLICATION_PATHS)}")
    try:
        check_tray_dependencies(resource_root() / "assets" / "tray-icon.png")
    except Exception as error:
        print(f"tray_dependencies: unavailable ({error})")
        return 1
    print("tray_dependencies: ready")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description="DSH Launcher for DeepSeek Harness")
    parser.add_argument("--check", action="store_true", help="print configuration and exit")
    args = parser.parse_args()
    if args.check:
        return _print_check()
    if not HAS_TK:
        language = load_language(APPLICATION_PATHS.language_file)
        print(translate(language, "missing_tk"), file=sys.stderr)
        return 1

    root = Tk()
    app = DesktopApp(root)
    try:
        root.mainloop()
    finally:
        app.quit_app()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
