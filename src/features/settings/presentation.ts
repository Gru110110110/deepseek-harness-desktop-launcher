import type { DesktopUpdateState } from "@/platform/generated/bindings";

type CopySpec = {
  key: string;
  values?: Record<string, string | number>;
};

export function getDesktopUpdateDetail(
  desktopUpdate: DesktopUpdateState,
): CopySpec {
  switch (desktopUpdate.kind) {
    case "checking":
      return { key: "update.desktop.checking" };
    case "available":
      return {
        key: "update.desktop.available",
        values: { version: desktopUpdate.version },
      };
    case "downloading": {
      const total = desktopUpdate.total;
      const hasTotal = total !== null && total > 0;
      return {
        key: hasTotal
          ? "update.desktop.downloading"
          : "update.desktop.downloadingUnknown",
        values: hasTotal
          ? {
              version: desktopUpdate.version,
              percent: Math.min(
                100,
                Math.floor((desktopUpdate.done * 100) / total),
              ),
            }
          : { version: desktopUpdate.version },
      };
    }
    case "installing":
      return {
        key: "update.desktop.installing",
        values: { version: desktopUpdate.version },
      };
    case "failed":
      return { key: "update.desktop.failed" };
    case "idle":
      return { key: "settings.desktopVersionDetail" };
  }
}
