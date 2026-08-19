import { Suspense, useEffect } from "react";
import { NavLink, Outlet } from "react-router-dom";
import { ChevronDown, ExternalLink, Moon, RefreshCw, Sun } from "lucide-react";
import * as DropdownMenu from "@radix-ui/react-dropdown-menu";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { features } from "@/features/registry";
import { launcherApi } from "@/platform/launcherApi";
import { useLauncherSnapshot } from "@/platform/launcherStore";
import type { Language, ThemePreference } from "@/platform/generated/bindings";
import logoUrl from "../../assets/logo-blue.png";
import deepseekIconUrl from "../../assets/external/deepseek.png";
import githubIconUrl from "../../assets/external/github.svg";
import { showTimedError } from "@/shared/errorToast";
import { ThemeProvider } from "./ThemeProvider";

const navigation = features
  .flatMap((feature) => (feature.navigation ? [feature.navigation] : []))
  .sort((left, right) => left.order - right.order);

function PreferenceMenu({
  label,
  value,
  items,
  onChange,
}: {
  label: string;
  value: string;
  items: readonly { value: string; label: string }[];
  onChange: (value: string) => Promise<void>;
}) {
  const selected = items.find((item) => item.value === value)?.label ?? value;
  return (
    <DropdownMenu.Root>
      <DropdownMenu.Trigger className="preference-trigger" aria-label={label}>
        <span>{selected}</span>
        <ChevronDown size={14} aria-hidden />
      </DropdownMenu.Trigger>
      <DropdownMenu.Portal>
        <DropdownMenu.Content
          className="dropdown-content"
          side="top"
          align="start"
          sideOffset={6}
        >
          <DropdownMenu.Label className="dropdown-label">
            {label}
          </DropdownMenu.Label>
          <DropdownMenu.RadioGroup
            value={value}
            onValueChange={(next) => void onChange(next)}
          >
            {items.map((item) => (
              <DropdownMenu.RadioItem
                key={item.value}
                className="dropdown-item"
                value={item.value}
              >
                <DropdownMenu.ItemIndicator className="dropdown-indicator">
                  ✓
                </DropdownMenu.ItemIndicator>
                {item.label}
              </DropdownMenu.RadioItem>
            ))}
          </DropdownMenu.RadioGroup>
        </DropdownMenu.Content>
      </DropdownMenu.Portal>
    </DropdownMenu.Root>
  );
}

function ShellContent() {
  const snapshot = useLauncherSnapshot();
  const { t, i18n } = useTranslation(undefined, { lng: snapshot.language });
  const presentTimedError = (error: unknown) => {
    showTimedError(error, (key, values) => t(key, values));
  };

  useEffect(() => {
    if (i18n.language !== snapshot.language)
      void i18n.changeLanguage(snapshot.language);
    document.documentElement.lang = snapshot.language === "zh" ? "zh-CN" : "en";
  }, [i18n, snapshot.language]);

  const setLanguage = async (value: string) => {
    try {
      await launcherApi.setLanguage(value as Language);
    } catch (error) {
      presentTimedError(error);
    }
  };
  const setTheme = async (value: string) => {
    try {
      await launcherApi.setTheme(value as ThemePreference);
    } catch (error) {
      presentTimedError(error);
    }
  };
  const checkDesktopUpdate = async () => {
    try {
      const version = await launcherApi.checkDesktopUpdate();
      if (!version) toast.success(t("update.desktop.latest"));
    } catch (error) {
      presentTimedError(error);
    }
  };
  const checkHarnessUpdate = async () => {
    try {
      const version = await launcherApi.checkHarnessUpdate();
      if (!version) toast.success(t("update.harness.latest"));
    } catch (error) {
      presentTimedError(error);
    }
  };
  const desktopUpdateBusy =
    snapshot.desktopUpdate.kind === "checking" ||
    snapshot.desktopUpdate.kind === "downloading" ||
    snapshot.desktopUpdate.kind === "installing";
  const harnessUpdateBusy =
    snapshot.harnessUpdate.kind === "checking" ||
    snapshot.harnessUpdate.kind === "installing";
  const openExternalLink = (target: "github" | "deepseek") => {
    void launcherApi.openExternalLink(target).catch((error: unknown) => {
      presentTimedError(error);
    });
  };

  return (
    <ThemeProvider theme={snapshot.theme}>
      <div className="app-shell">
        <aside className="sidebar">
          <button
            className="brand"
            title={t("links.website")}
            aria-label={t("links.website")}
            onClick={() =>
              void launcherApi.openWebsite().catch((error: unknown) => {
                presentTimedError(error);
              })
            }
          >
            <img src={logoUrl} alt="" className="brand-logo" />
            <span>
              <strong>{t("app.shortName")}</strong>
              <small>{t("app.subtitle")}</small>
            </span>
            <ExternalLink size={14} className="brand-link" aria-hidden />
          </button>

          <span className="sidebar-caption">{t("nav.startupFlow")}</span>
          <nav aria-label="Primary">
            {navigation.map(({ path, labelKey, icon: Icon }) => (
              <NavLink
                key={path}
                to={path}
                className={({ isActive }) =>
                  `nav-link${isActive ? " active" : ""}`
                }
              >
                <Icon size={17} aria-hidden />
                <span>{t(labelKey)}</span>
              </NavLink>
            ))}
          </nav>

          <div className="sidebar-steps" aria-label={t("nav.startupFlow")}>
            {(["prepare", "start"] as const).map((step, index) => {
              const activeIndex =
                snapshot.phase === "ready"
                  ? 2
                  : snapshot.step === "prepare"
                    ? 0
                    : 1;
              const done = index < activeIndex;
              const active = index === activeIndex;
              return (
                <div
                  className={`step-row${active ? " active" : ""}`}
                  key={step}
                >
                  <span className={`step-number${done ? " done" : ""}`}>
                    {done ? "✓" : `0${String(index + 1)}`}
                  </span>
                  <span>
                    <strong>{t(`step.${step}.title`)}</strong>
                    <small>{t(`step.${step}.description`)}</small>
                  </span>
                </div>
              );
            })}
          </div>

          <div className="sidebar-footer">
            <div className="preference-grid">
              <PreferenceMenu
                label={t("settings.language")}
                value={snapshot.language}
                items={[
                  { value: "zh", label: "中文" },
                  { value: "en", label: "English" },
                ]}
                onChange={setLanguage}
              />
              <PreferenceMenu
                label={t("settings.theme")}
                value={snapshot.theme}
                items={[
                  { value: "system", label: t("theme.system") },
                  { value: "light", label: t("theme.light") },
                  { value: "dark", label: t("theme.dark") },
                ]}
                onChange={setTheme}
              />
            </div>
            <button
              className="version-row version-action"
              type="button"
              disabled={desktopUpdateBusy}
              title={t("action.checkDesktopUpdate")}
              aria-label={t("action.checkDesktopUpdate")}
              onClick={() => void checkDesktopUpdate()}
            >
              <span>DESKTOP</span>
              <span>
                v{snapshot.desktopVersion}
                <RefreshCw
                  size={10}
                  className={
                    snapshot.desktopUpdate.kind === "checking" ? "spin" : ""
                  }
                  aria-hidden
                />
              </span>
            </button>
            <button
              className="version-row version-action"
              type="button"
              disabled={
                harnessUpdateBusy ||
                snapshot.phase !== "ready" ||
                !snapshot.harnessVersion
              }
              title={t("action.checkHarnessUpdate")}
              aria-label={t("action.checkHarnessUpdate")}
              onClick={() => void checkHarnessUpdate()}
            >
              <span>HARNESS</span>
              <span>
                {snapshot.harnessVersion ? `v${snapshot.harnessVersion}` : "—"}
                <RefreshCw
                  size={10}
                  className={
                    snapshot.harnessUpdate.kind === "checking" ? "spin" : ""
                  }
                  aria-hidden
                />
              </span>
            </button>
            <div className="theme-symbols" aria-hidden>
              <Sun size={13} />
              <Moon size={13} />
            </div>
          </div>
        </aside>
        <main className="main-content">
          <div className="external-links" aria-label={t("links.external")}>
            <button
              className="external-link-button"
              type="button"
              title={t("links.github")}
              aria-label={t("links.github")}
              onClick={() => {
                openExternalLink("github");
              }}
            >
              <img
                className="external-link-icon github-icon"
                src={githubIconUrl}
                alt=""
                aria-hidden
              />
            </button>
            <button
              className="external-link-button"
              type="button"
              title={t("links.deepseekPlatform")}
              aria-label={t("links.deepseekPlatform")}
              onClick={() => {
                openExternalLink("deepseek");
              }}
            >
              <img
                className="external-link-icon"
                src={deepseekIconUrl}
                alt=""
                aria-hidden
              />
            </button>
          </div>
          <Suspense
            fallback={<div className="route-loading" aria-label="Loading" />}
          >
            <Outlet />
          </Suspense>
        </main>
      </div>
    </ThemeProvider>
  );
}

export function AppShell() {
  return <ShellContent />;
}
