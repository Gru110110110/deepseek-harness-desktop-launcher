import { describe, expect, it } from "vitest";
import type { LauncherSnapshot } from "@/platform/generated/bindings";
import { getHeaderCopy, getServiceCopy, getUpdateNotice } from "./presentation";

function snapshot(overrides: Partial<LauncherSnapshot> = {}): LauncherSnapshot {
  return {
    revision: 1,
    phase: "preparing",
    step: "prepare",
    activity: null,
    progress: { kind: "indeterminate" },
    error: null,
    webUrl: null,
    serviceStartedAtMs: null,
    browsers: [],
    selectedBrowserId: "system",
    language: "zh",
    theme: "system",
    desktopVersion: "0.2.0",
    harnessVersion: null,
    desktopUpdate: { kind: "idle" },
    harnessUpdate: { kind: "none" },
    migration: { kind: "notRequired" },
    trayAvailable: true,
    ...overrides,
  };
}

describe("launcher presentation", () => {
  it("reserves first-install copy for a missing runtime", () => {
    expect(getHeaderCopy(snapshot())).toEqual({
      title: { key: "launcher.installing.title" },
      detail: { key: "launcher.installing.detail" },
    });

    expect(getHeaderCopy(snapshot({ harnessVersion: "0.1.0-rc.6" }))).toEqual({
      title: { key: "launcher.preparing.title" },
      detail: { key: "launcher.preparing.existingDetail" },
    });
  });

  it("uses update copy while a Harness update is installing", () => {
    const value = snapshot({
      harnessVersion: "0.1.0-rc.6",
      harnessUpdate: { kind: "installing", version: "0.1.0-rc.7" },
    });

    expect(getHeaderCopy(value)).toEqual({
      title: { key: "launcher.updating.title" },
      detail: {
        key: "launcher.updating.detail",
        values: { version: "0.1.0-rc.7" },
      },
    });
    expect(getServiceCopy(value)).toEqual({
      title: "service.updateTitle",
      badge: "service.updating",
      busyAction: "action.updating",
    });
  });

  it("uses migration copy while an import is being applied", () => {
    const value = snapshot({
      harnessVersion: "0.1.0-rc.6",
      migration: {
        kind: "applying",
        plan: {
          sourceEntries: 1,
          workspaceAvailable: false,
          ccSwitchProviders: 0,
        },
      },
    });

    expect(getHeaderCopy(value)).toEqual({
      title: { key: "launcher.migrating.title" },
      detail: { key: "launcher.migrating.detail" },
    });
  });

  it("does not repeat an active Harness update in a banner", () => {
    const value = snapshot({
      harnessUpdate: { kind: "installing", version: "0.1.0-rc.7" },
    });

    expect(getUpdateNotice(value)).toBeNull();
  });

  it("shows exactly one update notice with desktop updates taking precedence", () => {
    const value = snapshot({
      desktopUpdate: {
        kind: "downloading",
        version: "0.3.0",
        done: 50,
        total: 100,
      },
      harnessUpdate: { kind: "available", version: "0.1.0-rc.7" },
    });

    expect(getUpdateNotice(value)).toEqual({
      message: {
        key: "update.desktop.downloading",
        values: { version: "0.3.0", percent: 50 },
      },
      tone: "info",
      action: null,
      actionLabel: null,
    });
  });

  it("prompts before downloading an available desktop update", () => {
    const value = snapshot({
      desktopUpdate: { kind: "available", version: "0.3.0" },
    });

    expect(getUpdateNotice(value)).toEqual({
      message: {
        key: "update.desktop.available",
        values: { version: "0.3.0" },
      },
      tone: "info",
      action: "installDesktop",
      actionLabel: "action.updateDesktop",
    });
  });

  it("offers the correct recovery action for check and install failures", () => {
    expect(
      getUpdateNotice(
        snapshot({ desktopUpdate: { kind: "failed", version: null } }),
      ),
    ).toMatchObject({
      action: "checkDesktop",
      actionLabel: "action.retryCheckUpdate",
    });
    expect(
      getUpdateNotice(
        snapshot({
          desktopUpdate: { kind: "failed", version: "0.3.0" },
        }),
      ),
    ).toMatchObject({
      action: "installDesktop",
      actionLabel: "action.retryUpdate",
    });
  });

  it("does not show a false percentage when download size is unknown", () => {
    const value = snapshot({
      desktopUpdate: {
        kind: "downloading",
        version: "0.3.0",
        done: 1024,
        total: null,
      },
    });

    expect(getUpdateNotice(value)?.message).toEqual({
      key: "update.desktop.downloadingUnknown",
      values: { version: "0.3.0", percent: 0 },
    });
  });

  it("labels stopping independently from preparing or starting", () => {
    expect(
      getServiceCopy(snapshot({ phase: "stopping", step: "start" })),
    ).toEqual({
      title: "service.title",
      badge: "service.stopping",
      busyAction: "action.stopping",
    });
  });
});
