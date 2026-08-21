import { describe, expect, it } from "vitest";
import { getDesktopUpdateAction, getDesktopUpdateDetail } from "./presentation";

describe("settings presentation", () => {
  it("shows the available desktop version", () => {
    expect(
      getDesktopUpdateDetail({ kind: "available", version: "0.3.0" }),
    ).toEqual({
      key: "update.desktop.available",
      values: { version: "0.3.0" },
    });
    expect(
      getDesktopUpdateAction({ kind: "available", version: "0.3.0" }),
    ).toEqual({
      appearance: "primary",
      disabled: false,
      spinning: false,
      operation: "install",
      label: {
        key: "action.updateDesktopVersion",
        values: { version: "0.3.0" },
      },
    });
  });

  it("uses a disabled checking-style action as soon as updating starts", () => {
    expect(
      getDesktopUpdateAction({ kind: "preparing", version: "0.3.0" }),
    ).toEqual({
      appearance: "default",
      disabled: true,
      spinning: true,
      operation: null,
      label: { key: "action.updatingDesktop" },
    });
    expect(
      getDesktopUpdateAction({ kind: "installing", version: "0.3.0" }),
    ).toEqual({
      appearance: "default",
      disabled: true,
      spinning: true,
      operation: null,
      label: { key: "action.updatingDesktop" },
    });
  });

  it("shows bounded download progress when the total is known", () => {
    expect(
      getDesktopUpdateDetail({
        kind: "downloading",
        version: "0.3.0",
        done: 75,
        total: 100,
      }),
    ).toEqual({
      key: "update.desktop.downloading",
      values: { version: "0.3.0", percent: 75 },
    });
    expect(
      getDesktopUpdateDetail({
        kind: "downloading",
        version: "0.3.0",
        done: 150,
        total: 100,
      }).values?.percent,
    ).toBe(100);
    expect(
      getDesktopUpdateAction({
        kind: "downloading",
        version: "0.3.0",
        done: 10,
        total: 100,
      }),
    ).toMatchObject({
      appearance: "default",
      disabled: true,
      spinning: true,
      operation: null,
      label: {
        key: "action.updatingDesktopProgress",
        values: { percent: 10 },
      },
    });
  });

  it("does not invent progress when the total is unknown", () => {
    expect(
      getDesktopUpdateDetail({
        kind: "downloading",
        version: "0.3.0",
        done: 75,
        total: null,
      }),
    ).toEqual({
      key: "update.desktop.downloadingUnknown",
      values: { version: "0.3.0" },
    });
    expect(
      getDesktopUpdateAction({
        kind: "downloading",
        version: "0.3.0",
        done: 75,
        total: null,
      }).label,
    ).toEqual({ key: "action.updatingDesktop" });
  });

  it("keeps check and retry actions reachable after terminal states", () => {
    expect(getDesktopUpdateAction({ kind: "idle" })).toMatchObject({
      appearance: "default",
      disabled: false,
      operation: "check",
    });
    expect(
      getDesktopUpdateAction({ kind: "failed", version: null }),
    ).toMatchObject({
      appearance: "default",
      disabled: false,
      operation: "check",
    });
    expect(
      getDesktopUpdateAction({ kind: "failed", version: "0.3.0" }),
    ).toMatchObject({
      appearance: "primary",
      disabled: false,
      operation: "install",
      label: {
        key: "action.updateDesktopVersion",
        values: { version: "0.3.0" },
      },
    });
  });
});
