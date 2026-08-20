import { describe, expect, it } from "vitest";
import { getDesktopUpdateDetail } from "./presentation";

describe("settings presentation", () => {
  it("shows the available desktop version", () => {
    expect(
      getDesktopUpdateDetail({ kind: "available", version: "0.3.0" }),
    ).toEqual({
      key: "update.desktop.available",
      values: { version: "0.3.0" },
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
  });
});
