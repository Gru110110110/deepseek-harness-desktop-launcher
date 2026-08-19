import { toast } from "sonner";
import { presentError, type Translate } from "./presentError";

const ERROR_TOAST_DURATION_MS = 3000;

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function errorToastId(error: unknown, message: string): string {
  if (!isRecord(error) || typeof error.code !== "string") {
    return `error:${message}`;
  }

  const detail = typeof error.safeDetail === "string" ? error.safeDetail : "";
  const values = isRecord(error.values) ? error.values : {};
  return `error:${error.code}:${JSON.stringify(values)}:${detail}`;
}

export function showTimedError(error: unknown, translate: Translate): void {
  const message = presentError(error, translate);
  toast.error(message, {
    id: errorToastId(error, message),
    duration: ERROR_TOAST_DURATION_MS,
  });
}

export function showMigrationWarning(message: string): void {
  toast.error(message, {
    id: "migration-warning",
    duration: ERROR_TOAST_DURATION_MS,
  });
}
