import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type {
  Language,
  LauncherSnapshot,
  ThemePreference,
} from "./generated/bindings";

const command = <T>(name: string, args?: Record<string, unknown>): Promise<T> =>
  invoke<T>(name, args);
const action = (name: string, args?: Record<string, unknown>): Promise<void> =>
  invoke(name, args);

export const launcherApi = {
  snapshot: () => command<LauncherSnapshot>("launcher_get_snapshot"),
  retry: () => action("launcher_retry"),
  updateHarness: () => action("launcher_update_harness"),
  approveMigration: () => action("migration_approve"),
  skipMigration: () => action("migration_skip"),
  selectBrowser: (browserId: string) =>
    action("launcher_select_browser", { browserId }),
  openWebUi: () => action("launcher_open_web_ui"),
  openWebsite: () => action("application_open_website"),
  copyWebUrl: () => action("application_copy_web_url"),
  setLanguage: (language: Language) =>
    action("preferences_set_language", { language }),
  setTheme: (theme: ThemePreference) =>
    action("preferences_set_theme", { theme }),
  checkDesktopUpdate: () => command<string | null>("application_check_update"),
  installDesktopUpdate: () => action("application_install_update"),
  onState: (
    handler: (snapshot: LauncherSnapshot) => void,
  ): Promise<UnlistenFn> =>
    listen<LauncherSnapshot>("launcher://state", ({ payload }) => {
      handler(payload);
    }),
};
