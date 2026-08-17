import { useEffect, type PropsWithChildren } from "react";
import type { ThemePreference } from "@/platform/generated/bindings";

export function ThemeProvider({
  theme,
  children,
}: PropsWithChildren<{ theme: ThemePreference }>) {
  useEffect(() => {
    const media = window.matchMedia("(prefers-color-scheme: dark)");
    const apply = () => {
      const resolved =
        theme === "system" ? (media.matches ? "dark" : "light") : theme;
      document.documentElement.dataset.theme = resolved;
      document.documentElement.style.colorScheme = resolved;
    };
    apply();
    media.addEventListener("change", apply);
    return () => {
      media.removeEventListener("change", apply);
    };
  }, [theme]);
  return children;
}
