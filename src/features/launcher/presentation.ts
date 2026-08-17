import type { LauncherSnapshot } from "@/platform/generated/bindings";

type CopyValues = Record<string, string | number>;

type CopySpec = {
  key: string;
  values?: CopyValues;
};

type HeaderCopy = {
  title: CopySpec;
  detail: CopySpec;
};

type UpdateNotice = {
  message: CopySpec;
  tone: "info" | "error";
  action: "checkDesktop" | "installDesktop" | "updateHarness" | null;
  actionLabel: string | null;
};

export function getHeaderCopy(snapshot: LauncherSnapshot): HeaderCopy {
  if (snapshot.harnessUpdate.kind === "installing") {
    const values = { version: snapshot.harnessUpdate.version };
    return {
      title: { key: "launcher.updating.title" },
      detail: { key: "launcher.updating.detail", values },
    };
  }

  if (snapshot.migration.kind === "applying") {
    return {
      title: { key: "launcher.migrating.title" },
      detail: { key: "launcher.migrating.detail" },
    };
  }

  if (snapshot.phase === "preparing") {
    return snapshot.harnessVersion
      ? {
          title: { key: "launcher.preparing.title" },
          detail: { key: "launcher.preparing.existingDetail" },
        }
      : {
          title: { key: "launcher.installing.title" },
          detail: { key: "launcher.installing.detail" },
        };
  }

  return {
    title: { key: `launcher.${snapshot.phase}.title` },
    detail: { key: `launcher.${snapshot.phase}.detail` },
  };
}

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

export function getUpdateNotice(
  snapshot: LauncherSnapshot,
): UpdateNotice | null {
  const desktop = snapshot.desktopUpdate;
  switch (desktop.kind) {
    case "checking":
      return {
        message: { key: "update.desktop.checking" },
        tone: "info",
        action: null,
        actionLabel: null,
      };
    case "available":
      return {
        message: {
          key: "update.desktop.available",
          values: { version: desktop.version },
        },
        tone: "info",
        action: "installDesktop",
        actionLabel: "action.updateDesktop",
      };
    case "downloading":
      return {
        message: {
          key:
            desktop.total && desktop.total > 0
              ? "update.desktop.downloading"
              : "update.desktop.downloadingUnknown",
          values: {
            version: desktop.version,
            percent:
              desktop.total && desktop.total > 0
                ? Math.min(
                    100,
                    Math.floor((desktop.done * 100) / desktop.total),
                  )
                : 0,
          },
        },
        tone: "info",
        action: null,
        actionLabel: null,
      };
    case "installing":
      return {
        message: {
          key: "update.desktop.installing",
          values: { version: desktop.version },
        },
        tone: "info",
        action: null,
        actionLabel: null,
      };
    case "failed":
      return {
        message: { key: "update.desktop.failed" },
        tone: "error",
        action: desktop.version ? "installDesktop" : "checkDesktop",
        actionLabel: desktop.version
          ? "action.retryUpdate"
          : "action.retryCheckUpdate",
      };
    case "idle":
      break;
  }

  const harness = snapshot.harnessUpdate;
  switch (harness.kind) {
    case "available":
      return {
        message: {
          key: "update.harness.available",
          values: { version: harness.version },
        },
        tone: "info",
        action: "updateHarness",
        actionLabel: "action.updateHarness",
      };
    case "failed":
      return {
        message: {
          key: "update.harness.failed",
          values: { version: harness.version },
        },
        tone: "error",
        action: "updateHarness",
        actionLabel: "action.retryUpdate",
      };
    case "installing":
    case "none":
      return null;
  }
}
