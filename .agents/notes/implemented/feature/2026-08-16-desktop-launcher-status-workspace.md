# Agent Note: Desktop launcher status workspace

Status: implemented

English | [中文](2026-08-16-desktop-launcher-status-workspace.zh.md)

## Problem

The desktop launcher needs to communicate environment preparation, service startup, readiness, failure, and update availability in a small cross-platform Tk window. A centered sequence of labels leaves the relationship between those states unclear, wastes most of the window, and relies on bitmap buttons that render poorly at different display scales.

## Decision

The launcher uses a persistent two-column workspace. The left rail holds product identity, the two startup stages, and desktop and Harness versions. The right side gives the current state a clear heading and keeps one bordered service card for progress, the local URL, uptime, and update availability. Each runtime transition updates the heading, explanation, stage treatment, badge, card details, and primary action together.

The launcher first starts the official default address. When that attempt reports `EADDRINUSE`, it retries once with the official `--port 0` option so the operating system selects an available loopback port. The service's official `dsh web: <URL>` output is the launcher's readiness signal and sole URL source. The output reader tees every line to `server.log`, extracts the canonical HTTP or HTTPS URL from that line, and passes it to the card and browser action. Any other process exit or a 60-second timeout before the readiness line stops the child and reports failure; the desktop shell never substitutes its own host or port.

The only primary action opens the Web UI after readiness or retries after a failure. Native themed buttons replace bitmap-backed controls so disabled, hover, and high-density rendering remain consistent across macOS and Windows. Closing the window remains the explicit service-lifetime signal.

The launcher discovers common installed browsers once at startup. One detected browser leaves the primary action as a single button; multiple browsers add a compact menu beside it and include the selected browser's name in the action. Changing the selection affects only the browser, while the URL remains the exact service-announced value. Explicit browser launches pass that URL as a separate process argument without a shell. The system default is the sole fallback when no recognized browser is installed.

## Alternatives considered

**Keep the centered status screen.** This preserves the smallest implementation, but it cannot establish a durable information hierarchy and leaves progress, versions, state, and action visually disconnected.

**Replace Tkinter with a browser or native framework.** A new UI runtime would add packaging and lifecycle work without improving the launcher's limited interaction model. Tkinter already supports the required cards, status treatments, and native controls.

**Keep image-backed rounded buttons.** Pre-rendered images provide exact corners at one scale, but produce visible borders and soft pixels on high-density displays and do not adapt naturally to disabled or hover states.

**Display the current default URL.** A desktop-owned `127.0.0.1:3080` value is simpler, but drifts when the official runner changes its default, fails when that port is occupied, and can falsely identify another process on that address as ready.

**Always use the system default browser.** This avoids browser discovery, but prevents users with several installed browsers from choosing where to open the local workspace.

## Consequences

The window is wider than the original status panel, but its state remains readable without opening another view. Startup and ready states share one stable layout, preventing elements from jumping between unrelated positions. The visual system remains intentionally small and depends only on bundled logo artwork and Tk themed widgets. Address discovery now depends on the official readiness line remaining stable; missing or malformed output fails visibly instead of opening a guessed endpoint. Browser discovery recognizes a maintained list of common applications per platform and does not persist the choice between launcher sessions.

## Testing

The desktop unit suite covers uptime presentation, default-then-free-port command selection, occupied-address classification, URL parsing with a dynamic port and LAN suffix, rejection of unrelated or unsafe output, log teeing, service behavior, browser discovery, system-default fallback, shell-free argument separation, and owned child-process termination. The [runtime deployment decision](../bug-fix/2026-08-16-desktop-runtime-deployment-reliability.md#testing) owns the isolated first-install process smoke. Packaging CI runs the source suite and each frozen launcher's headless configuration check. Startup, ready, multi-browser, and single-browser GUI rendering still depend on manual packaged-launcher inspection.
