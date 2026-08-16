# Agent Note: Desktop launcher language selection

Status: implemented

English | [中文](2026-08-16-desktop-launcher-language-selection.zh.md)

## Problem

The desktop launcher is a user-facing window before the browser workspace opens, but Chinese literals in its status, actions, and diagnostics make an English system start in Chinese. The browser workspace has its own locale service and cannot translate the separate Python/Tk launcher.

## Decision

The launcher ships complete Simplified Chinese and English dictionaries for its standing labels, lifecycle states, actions, and owned diagnostics. UI bindings retain a copy key and interpolation values, so changing the active language re-renders the current startup, ready, update, or failure state without restarting the managed service. External exception details remain verbatim inside the translated diagnostic that owns their context.

A valid `~/.dsh-desktop/language` value is the explicit launcher preference. Without one, macOS reads the ordered `AppleLanguages` preference, Windows reads the user's ordered preferred UI languages, and other platforms read standard locale environment values. Resolution chooses the first shipped primary language from that ordered list and falls back to Chinese when neither `zh` nor `en` is present. The sidebar menu switches immediately and saves `zh` or `en` through an atomic replacement.

The launcher preference is independent of the Web UI's `locale.preference` under the Harness home. The two programs run in different UI processes, and changing one does not silently rewrite the other.

Runtime deployment raises `LocalizedError` values, while the service manager returns deferred `LocalizedText` diagnostics. This keeps background work independent of the language active when it started and lets a failure already on screen follow a later language switch.

## Alternatives considered

**Follow the system on every launch without a selector.** This fixes the first English launch but gives bilingual users no in-product override and discards their explicit choice whenever the operating-system order changes.

**Reuse the Web UI preference.** This would couple the launcher to a settings document owned by the service it is responsible for starting; failures before service readiness could not read or change that preference reliably.

**Translate only the normal startup path.** English users would still receive Chinese text during installation, update, browser, migration, and service failures, which are the states where clear instructions matter most.

**Rebuild the window after a language change.** Destroying and recreating Tk widgets risks interrupting the visual state around a running worker. Keyed bindings update the existing widgets and preserve service ownership and progress.

## Consequences

The desktop home gains one non-sensitive `language` file outside the Harness service home. macOS system-language detection starts one bounded, shell-free `defaults` read; Windows uses the native preferred-UI-language API; other platforms depend on the process locale environment. Product names, installed-browser names, URLs, versions, and raw third-party exception details are intentionally not translated.

## Testing

The desktop unit suite verifies ordered primary-language matching, English and Chinese system preferences, explicit-choice precedence, invalid saved-value fallback, atomic persistence validation, macOS preference parsing, dictionary key parity, and bilingual status and error rendering. The existing launcher lifecycle suite continues to cover runtime, service, browser, and data-import behavior.
