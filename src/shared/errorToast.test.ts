import { beforeEach, describe, expect, it, vi } from "vitest";
import { showMigrationWarning, showTimedError } from "./errorToast";

const mocks = vi.hoisted(() => ({
  error: vi.fn(),
}));

vi.mock("sonner", () => ({
  toast: { error: mocks.error },
}));

const translate = (key: string, options?: Record<string, unknown>) =>
  key === "error.writeFailed"
    ? `Write failed: ${String(options?.detail)}`
    : key;

describe("timed error toasts", () => {
  beforeEach(() => {
    mocks.error.mockReset();
  });

  it("presents structured errors for three seconds with a stable identity", () => {
    const error = {
      code: "writeFailed",
      values: { target: "settings" },
      safeDetail: "access denied",
    };

    showTimedError(error, translate);
    showTimedError(error, translate);

    const expectedOptions = {
      id: 'error:writeFailed:{"target":"settings"}:access denied',
      duration: 3000,
    };
    expect(mocks.error).toHaveBeenNthCalledWith(
      1,
      "Write failed: access denied",
      expectedOptions,
    );
    expect(mocks.error).toHaveBeenNthCalledWith(
      2,
      "Write failed: access denied",
      expectedOptions,
    );
  });

  it("uses one fixed timed toast for migration warnings", () => {
    showMigrationWarning("Import skipped");

    expect(mocks.error).toHaveBeenCalledWith("Import skipped", {
      id: "migration-warning",
      duration: 3000,
    });
  });
});
