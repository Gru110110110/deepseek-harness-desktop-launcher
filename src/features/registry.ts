import type { FeatureDescriptor } from "@/app/feature";
import { launcherFeature } from "./launcher/descriptor";
import { settingsFeature } from "./settings/descriptor";

export const features: readonly FeatureDescriptor[] = [
  launcherFeature,
  settingsFeature,
];
