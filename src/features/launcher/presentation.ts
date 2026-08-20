import type { LauncherSnapshot } from "@/platform/generated/bindings";

type CopyValues = Record<string, string | number>;

type CopySpec = {
  key: string;
  values?: CopyValues;
};

type UpdateNotice = {
  message: CopySpec;
  tone: "info" | "error";
  actionLabel: string;
};

export function getServiceCopy(snapshot: LauncherSnapshot): {
  title: string;
  badge: string;
  busyAction: string;
} {
  const updating = snapshot.harnessUpdate.kind === "installing";

  return {
    title: updating
      ? "service.updateTitle"
      : snapshot.step === "prepare"
        ? "service.environmentTitle"
        : "service.title",
    badge:
      snapshot.phase === "ready"
        ? "service.running"
        : snapshot.phase === "failed"
          ? "service.attention"
          : updating
            ? "service.updating"
            : snapshot.phase === "starting"
              ? "service.starting"
              : snapshot.phase === "stopping"
                ? "service.stopping"
                : "service.preparing",
    busyAction: updating
      ? "action.updating"
      : snapshot.phase === "stopping"
        ? "action.stopping"
        : "action.starting",
  };
}

export function getHarnessUpdateNotice(
  snapshot: LauncherSnapshot,
): UpdateNotice | null {
  const harness = snapshot.harnessUpdate;
  switch (harness.kind) {
    case "available":
      return {
        message: {
          key: "update.harness.available",
          values: { version: harness.version },
        },
        tone: "info",
        actionLabel: "action.updateHarness",
      };
    case "failed":
      return {
        message: {
          key: "update.harness.failed",
          values: { version: harness.version },
        },
        tone: "error",
        actionLabel: "action.retryUpdate",
      };
    case "installing":
    case "checking":
    case "none":
      return null;
  }
}
