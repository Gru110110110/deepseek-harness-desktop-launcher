import { describe, expect, it } from "vitest";
import { presentError } from "./presentError";

const translate = (key: string, values?: Record<string, unknown>) =>
  key === "error.downloadFailed"
    ? `Download failed: ${String(values?.detail)}`
    : key === "error.unknown"
      ? "Unknown"
      : key;

describe("presentError", () => {
  it("renders a known structured error", () => {
    expect(
      presentError(
        { code: "downloadFailed", values: {}, safeDetail: "offline" },
        translate,
      ),
    ).toBe("Download failed: offline");
  });

  it("uses only an explicitly safe detail for unknown codes", () => {
    expect(
      presentError(
        { code: "futureError", values: {}, safeDetail: "safe" },
        translate,
      ),
    ).toBe("safe");
    expect(presentError(new Error("secret"), translate)).toBe("Unknown");
  });
});
