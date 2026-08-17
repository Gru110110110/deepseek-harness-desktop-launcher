import { useMemo } from "react";
import {
  Check,
  ChevronDown,
  Copy,
  ExternalLink,
  LoaderCircle,
  RefreshCw,
  ShieldCheck,
} from "lucide-react";
import * as DropdownMenu from "@radix-ui/react-dropdown-menu";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { launcherApi } from "@/platform/launcherApi";
import { useLauncherSnapshot } from "@/platform/launcherStore";
import { formatDuration } from "@/shared/time";
import { useNow } from "@/shared/useNow";
import { presentError } from "@/shared/presentError";
import {
  getHeaderCopy,
  getServiceCopy,
  getUpdateNotice,
} from "../presentation";

export function LauncherPage() {
  const snapshot = useLauncherSnapshot();
  const { t } = useTranslation(undefined, { lng: snapshot.language });
  const now = useNow();
  const headerCopy = getHeaderCopy(snapshot);
  const serviceCopy = getServiceCopy(snapshot);
  const updateNotice = getUpdateNotice(snapshot);
  const selectedBrowser = snapshot.browsers.find(
    (item) => item.id === snapshot.selectedBrowserId,
  );
  const browserLabel = (id: string, label: string) =>
    id === "system" ? t("browser.default") : label;
  const running = snapshot.phase === "ready";
  const migrationPlan =
    snapshot.migration.kind === "pending" ? snapshot.migration.plan : null;
  const awaitingMigration = migrationPlan !== null;
  const elapsed = snapshot.serviceStartedAtMs
    ? formatDuration(now - snapshot.serviceStartedAtMs)
    : null;
  const activityElapsed = snapshot.activity
    ? formatDuration(now - snapshot.activity.startedAtMs)
    : null;
  const activityText = snapshot.activity
    ? t(`activity.${snapshot.activity.code}`, {
        ...snapshot.activity.values,
        elapsed: activityElapsed,
      })
    : null;
  const progressPercent =
    snapshot.progress.kind === "determinate" && snapshot.progress.total > 0
      ? Math.min(
          100,
          Math.floor((snapshot.progress.done * 100) / snapshot.progress.total),
        )
      : null;

  const errorDetail = useMemo(() => {
    if (!snapshot.error) return null;
    return presentError(snapshot.error, (key, values) => t(key, values));
  }, [snapshot.error, t]);

  const run = (task: Promise<unknown>): void => {
    void task.catch((error: unknown) => {
      toast.error(presentError(error, (key, values) => t(key, values)));
    });
  };
  const primary = () => {
    if (running) {
      run(launcherApi.openWebUi());
    } else if (snapshot.phase === "failed") {
      run(launcherApi.retry());
    }
  };

  return (
    <section className="launcher-page">
      <header className="page-header">
        <span className="eyebrow">{t("launcher.eyebrow")}</span>
        <h1>{t(headerCopy.title.key, headerCopy.title.values)}</h1>
        <p>
          {snapshot.phase === "failed" && errorDetail
            ? errorDetail
            : t(headerCopy.detail.key, headerCopy.detail.values)}
        </p>
      </header>

      {awaitingMigration && (
        <div className="migration-card">
          <div className="migration-icon" aria-hidden>
            <ShieldCheck size={22} />
          </div>
          <div className="migration-copy">
            <h2>{t("migration.title")}</h2>
            <p>{t("migration.detail")}</p>
            <ul>
              {migrationPlan.sourceEntries > 0 && (
                <li>
                  {t("migration.sourceEntries", {
                    count: migrationPlan.sourceEntries,
                  })}
                </li>
              )}
              {migrationPlan.workspaceAvailable && (
                <li>{t("migration.workspace")}</li>
              )}
              {migrationPlan.ccSwitchProviders > 0 && (
                <li>
                  {t("migration.ccSwitch", {
                    count: migrationPlan.ccSwitchProviders,
                  })}
                </li>
              )}
            </ul>
            <p className="migration-safety">{t("migration.safety")}</p>
            <div className="migration-actions">
              <button
                className="secondary-button"
                onClick={() => {
                  run(launcherApi.skipMigration());
                }}
              >
                {t("action.skipMigration")}
              </button>
              <button
                className="primary-button"
                onClick={() => {
                  run(launcherApi.approveMigration());
                }}
              >
                {t("action.approveMigration")}
              </button>
            </div>
          </div>
        </div>
      )}

      {!awaitingMigration && (
        <div className="service-card">
          <div className="card-heading">
            <h2>{t(serviceCopy.title)}</h2>
            <span
              className={`status-badge ${running ? "success" : snapshot.phase === "failed" ? "error" : "busy"}`}
            >
              {running ? (
                <Check size={14} />
              ) : snapshot.phase === "failed" ? (
                "!"
              ) : (
                <LoaderCircle size={14} className="spin" />
              )}
              {t(serviceCopy.badge)}
            </span>
          </div>

          {!running && snapshot.phase !== "failed" && (
            <div className="progress-block">
              <div
                className={`progress-track${progressPercent === null ? " indeterminate" : ""}`}
              >
                <span
                  style={
                    progressPercent === null
                      ? undefined
                      : { width: `${String(progressPercent)}%` }
                  }
                />
              </div>
              <div className="progress-meta">
                <span>{activityText}</span>
                <strong>
                  {progressPercent === null
                    ? ""
                    : `${String(progressPercent)}%`}
                </strong>
              </div>
            </div>
          )}

          <div className="service-fields">
            <div>
              <span>{t("service.address")}</span>
              <button
                disabled={!snapshot.webUrl}
                onClick={() => {
                  run(
                    launcherApi.copyWebUrl().then(() => {
                      toast.success(t("action.copied"));
                    }),
                  );
                }}
              >
                {snapshot.webUrl ?? t("service.waitingAddress")}{" "}
                {snapshot.webUrl && <Copy size={14} />}
              </button>
            </div>
            <div>
              <span>{t("service.runtime")}</span>
              <strong>
                {elapsed
                  ? t("service.uptime", { time: elapsed })
                  : running
                    ? t("service.running")
                    : snapshot.phase === "failed"
                      ? t("service.notRunning")
                      : t("service.waiting")}
              </strong>
            </div>
          </div>
        </div>
      )}

      {updateNotice && (
        <div
          className={`update-banner${updateNotice.tone === "error" ? " error" : ""}`}
        >
          <span>
            {t(updateNotice.message.key, updateNotice.message.values)}
          </span>
          {updateNotice.action === "installDesktop" && (
            <button
              onClick={() => {
                run(launcherApi.installDesktopUpdate());
              }}
            >
              {t(updateNotice.actionLabel ?? "action.updateDesktop")}
            </button>
          )}
          {updateNotice.action === "checkDesktop" && (
            <button
              onClick={() => {
                run(launcherApi.checkDesktopUpdate());
              }}
            >
              {t(updateNotice.actionLabel ?? "action.retryCheckUpdate")}
            </button>
          )}
          {updateNotice.action === "updateHarness" && (
            <button
              onClick={() => {
                run(launcherApi.updateHarness());
              }}
            >
              {t(updateNotice.actionLabel ?? "action.updateHarness")}
            </button>
          )}
        </div>
      )}

      {!awaitingMigration && (
        <footer className="page-actions">
          <p>
            {t(snapshot.trayAvailable ? "footer.closeHint" : "footer.noTray")}
          </p>
          <div className="split-button">
            <button
              className="primary-button"
              disabled={
                snapshot.phase === "preparing" ||
                snapshot.phase === "awaitingMigration" ||
                snapshot.phase === "starting" ||
                snapshot.phase === "stopping"
              }
              onClick={primary}
            >
              {snapshot.phase === "failed" ? (
                <RefreshCw size={17} />
              ) : snapshot.phase === "ready" ? (
                <ExternalLink size={17} />
              ) : (
                <LoaderCircle size={17} className="spin" />
              )}
              {t(
                snapshot.phase === "failed"
                  ? "action.retry"
                  : snapshot.phase === "ready"
                    ? snapshot.browsers.length > 1
                      ? "action.openWith"
                      : "action.open"
                    : serviceCopy.busyAction,
                {
                  browser: selectedBrowser
                    ? browserLabel(selectedBrowser.id, selectedBrowser.label)
                    : t("browser.default"),
                },
              )}
            </button>
            {snapshot.browsers.length > 1 && (
              <DropdownMenu.Root>
                <DropdownMenu.Trigger
                  className="split-menu"
                  disabled={!running}
                  aria-label={t("action.chooseBrowser")}
                >
                  <ChevronDown size={18} />
                </DropdownMenu.Trigger>
                <DropdownMenu.Portal>
                  <DropdownMenu.Content
                    className="dropdown-content"
                    align="end"
                    sideOffset={6}
                  >
                    <DropdownMenu.RadioGroup
                      value={snapshot.selectedBrowserId}
                      onValueChange={(id) => {
                        run(launcherApi.selectBrowser(id));
                      }}
                    >
                      {snapshot.browsers.map((browser) => (
                        <DropdownMenu.RadioItem
                          className="dropdown-item"
                          value={browser.id}
                          key={browser.id}
                        >
                          <DropdownMenu.ItemIndicator className="dropdown-indicator">
                            ✓
                          </DropdownMenu.ItemIndicator>
                          {browserLabel(browser.id, browser.label)}
                        </DropdownMenu.RadioItem>
                      ))}
                    </DropdownMenu.RadioGroup>
                  </DropdownMenu.Content>
                </DropdownMenu.Portal>
              </DropdownMenu.Root>
            )}
          </div>
        </footer>
      )}
    </section>
  );
}
