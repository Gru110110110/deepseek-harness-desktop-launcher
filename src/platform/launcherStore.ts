import { useSyncExternalStore } from "react";
import type { LauncherSnapshot } from "./generated/bindings";
import { launcherApi } from "./launcherApi";

let current: LauncherSnapshot | null = null;
let initializationError: Error | null = null;
const listeners = new Set<() => void>();
let initializePromise: Promise<void> | null = null;

function accept(snapshot: LauncherSnapshot): void {
  if (current && snapshot.revision < current.revision) return;
  current = snapshot;
  for (const listener of listeners) listener();
}

export function initializeLauncherStore(): Promise<void> {
  if (initializePromise) return initializePromise;
  initializePromise = (async () => {
    try {
      await launcherApi.onState(accept);
      accept(await launcherApi.snapshot());
    } catch (error) {
      initializationError =
        error instanceof Error ? error : new Error("Launcher IPC unavailable");
      throw initializationError;
    }
  })();
  return initializePromise;
}

export function useLauncherSnapshot(): LauncherSnapshot {
  const snapshot = useSyncExternalStore(
    (listener) => {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    () => current,
  );
  if (initializationError) throw initializationError;
  if (!snapshot) throw initializeLauncherStore();
  return snapshot;
}

export const __launcherStoreTest = {
  accept,
  reset: () => {
    current = null;
    initializationError = null;
    initializePromise = null;
    listeners.clear();
  },
  current: () => current,
};

export function initializeLauncherPreview(snapshot: LauncherSnapshot): void {
  accept(snapshot);
}
