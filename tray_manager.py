# SPDX-License-Identifier: MIT
"""System-tray integration kept separate from the tkinter application."""
from __future__ import annotations

import io
import platform
import sys
from collections.abc import Callable
from pathlib import Path
from typing import Any

from app_paths import APP_ID, APP_NAME
from localization import translate


TrayCallback = Callable[[], None]


def _load_backend() -> tuple[Any, Any, dict[str, object]]:
    """Load optional tray dependencies only for an actual GUI launch."""
    import pystray
    from PIL import Image

    options: dict[str, object] = {}
    if platform.system() == "Darwin":
        from AppKit import NSApplication

        options["darwin_nsapplication"] = NSApplication.sharedApplication()
    return pystray, Image, options


def check_tray_dependencies(icon_path: Path) -> None:
    """Raise when packaged modules or the icon are unavailable, without starting GUI state."""
    import pystray  # noqa: F401 - import verifies the selected packaged backend
    from PIL import Image

    if platform.system() == "Darwin":
        import AppKit  # noqa: F401 - NSApplication requires a Tk root before use
        import Quartz  # noqa: F401
    with Image.open(icon_path) as source:
        source.convert("RGBA")


def _configure_macos_retina_icon(icon: Any, image_module: Any) -> None:
    """Replace pystray's 1x menu-bar bitmap with a backing-scale-aware NSImage."""
    import AppKit
    import Foundation

    thickness = float(icon._status_bar.thickness())
    screen = AppKit.NSScreen.mainScreen()
    scale = float(screen.backingScaleFactor()) if screen is not None else 2.0
    pixel_size = max(1, round(thickness * max(scale, 1.0)))
    source = icon.icon.resize(
        (pixel_size, pixel_size),
        image_module.Resampling.LANCZOS,
    )
    buffer = io.BytesIO()
    source.save(buffer, "PNG")
    native_image = AppKit.NSImage.alloc().initWithData_(Foundation.NSData(buffer.getvalue()))
    native_image.setSize_(AppKit.NSMakeSize(thickness, thickness))
    icon._icon_image = native_image
    icon._status_item.button().setImage_(native_image)


class TrayController:
    """Own a pystray icon and its localized three-item menu."""

    def __init__(
        self,
        icon_path: Path,
        on_show: TrayCallback,
        on_open_web_ui: TrayCallback,
        on_quit: TrayCallback,
    ) -> None:
        self.icon_path = icon_path
        self.on_show = on_show
        self.on_open_web_ui = on_open_web_ui
        self.on_quit = on_quit
        self._backend: Any | None = None
        self._icon: Any | None = None
        self._language = "zh"
        self._web_ui_ready = False

    @property
    def is_running(self) -> bool:
        return self._icon is not None

    def start(self, language: str, web_ui_ready: bool) -> bool:
        """Create and start the tray icon without taking over the Tk main loop."""
        if self._icon is not None:
            return True
        self._language = language
        self._web_ui_ready = web_ui_ready
        icon: Any | None = None
        try:
            backend, image_module, options = _load_backend()
            with image_module.open(self.icon_path) as source:
                image = source.convert("RGBA")
            icon = backend.Icon(
                APP_ID,
                image,
                APP_NAME,
                menu=self._build_menu(backend),
                **options,
            )
            self._backend = backend
            self._icon = icon
            icon.run_detached(setup=lambda _icon: None)
            icon.visible = True
            if "darwin_nsapplication" in options:
                _configure_macos_retina_icon(icon, image_module)
        except Exception as error:
            if icon is not None:
                try:
                    icon.stop()
                except Exception:
                    pass
            self._backend = None
            self._icon = None
            print(f"could not start system tray: {error}", file=sys.stderr)
            return False
        return True

    def refresh(self, language: str, web_ui_ready: bool) -> None:
        """Refresh localized menu text and the Web UI enabled state."""
        self._language = language
        self._web_ui_ready = web_ui_ready
        if self._icon is None or self._backend is None:
            return
        try:
            self._icon.menu = self._build_menu(self._backend)
            self._icon.update_menu()
        except Exception as error:
            print(f"could not refresh system tray: {error}", file=sys.stderr)

    def stop(self) -> None:
        """Remove the tray icon; repeated calls are harmless."""
        icon = self._icon
        self._icon = None
        self._backend = None
        if icon is None:
            return
        try:
            icon.stop()
        except Exception as error:
            print(f"could not stop system tray cleanly: {error}", file=sys.stderr)

    def _build_menu(self, backend: Any) -> Any:
        return backend.Menu(
            backend.MenuItem(
                translate(self._language, "tray_show_launcher"),
                lambda _icon, _item: self.on_show(),
            ),
            backend.MenuItem(
                translate(self._language, "tray_open_web_ui"),
                lambda _icon, _item: self.on_open_web_ui(),
                enabled=self._web_ui_ready,
            ),
            backend.MenuItem(
                translate(self._language, "tray_quit"),
                lambda _icon, _item: self.on_quit(),
            ),
        )
