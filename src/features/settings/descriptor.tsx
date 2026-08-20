import { lazy } from "react";
import { Settings } from "lucide-react";
import type { FeatureDescriptor } from "@/app/feature";

const SettingsPage = lazy(async () => {
  const module = await import("./pages/SettingsPage");
  return { default: module.SettingsPage };
});

export const settingsFeature: FeatureDescriptor = {
  id: "settings",
  routes: [{ path: "settings", element: <SettingsPage /> }],
  navigation: {
    labelKey: "nav.settings",
    path: "/settings",
    icon: Settings,
    order: 20,
  },
};
