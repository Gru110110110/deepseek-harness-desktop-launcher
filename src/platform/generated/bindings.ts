// Generated from dsh-core Rust types. Do not edit by hand.

export type Language = "zh" | "en";

export type ThemePreference = "system" | "light" | "dark";

export type LauncherPhase =
  | "preparing"
  | "awaitingMigration"
  | "starting"
  | "ready"
  | "stopped"
  | "failed"
  | "stopping";

export type LauncherStep = "prepare" | "start";

export type BrowserChoice = { id: string; label: string };

export type ActivityCode =
  | "waitingForLock"
  | "checkingRuntime"
  | "resolvingVersion"
  | "downloadingNode"
  | "verifyingNode"
  | "checkingSources"
  | "installingHarness"
  | "validatingHarness"
  | "activatingHarness"
  | "migratingData"
  | "startingService";

export type ActivityState = {
  code: ActivityCode;
  values: { [key in string]?: string };
  startedAtMs: number;
};

export type ProgressState =
  | { kind: "indeterminate" }
  | { kind: "determinate"; done: number; total: number };

export type LauncherError = {
  code: string;
  values: { [key in string]?: string };
  safeDetail?: string | null;
};

export type DesktopUpdateState =
  | { kind: "idle" }
  | { kind: "checking" }
  | { kind: "available"; version: string }
  | { kind: "downloading"; version: string; done: number; total: number | null }
  | { kind: "installing"; version: string }
  | { kind: "failed"; version: string | null };

export type HarnessUpdateState =
  | { kind: "none" }
  | { kind: "checking" }
  | { kind: "available"; version: string }
  | { kind: "installing"; version: string }
  | { kind: "failed"; version: string };

export type MigrationPlan = {
  sourceEntries: number;
  workspaceAvailable: boolean;
  ccSwitchProviders: number;
};

export type MigrationState =
  | { kind: "notRequired" }
  | { kind: "pending"; plan: MigrationPlan }
  | { kind: "applying"; plan: MigrationPlan }
  | { kind: "completed" }
  | { kind: "completedWithWarning"; warning: LauncherError }
  | { kind: "skipped" };

export type LauncherSnapshot = {
  revision: number;
  phase: LauncherPhase;
  step: LauncherStep;
  activity: ActivityState | null;
  progress: ProgressState;
  error: LauncherError | null;
  webUrl: string | null;
  serviceStartedAtMs: number | null;
  browsers: Array<BrowserChoice>;
  selectedBrowserId: string;
  language: Language;
  theme: ThemePreference;
  desktopVersion: string;
  harnessVersion: string | null;
  desktopUpdate: DesktopUpdateState;
  harnessUpdate: HarnessUpdateState;
  migration: MigrationState;
  trayAvailable: boolean;
};
