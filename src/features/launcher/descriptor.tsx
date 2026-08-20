import { lazy } from "react";
import { Monitor } from "lucide-react";
import type { FeatureDescriptor } from "@/app/feature";

const LauncherPage = lazy(async () => {
  const module = await import("./pages/LauncherPage");
  return { default: module.LauncherPage };
});

export const launcherFeature: FeatureDescriptor = {
  id: "launcher",
  routes: [{ path: "launcher", element: <LauncherPage /> }],
  navigation: {
    labelKey: "nav.harness",
    path: "/launcher",
    icon: Monitor,
    order: 10,
  },
};
