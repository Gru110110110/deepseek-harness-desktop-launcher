import type { FeatureDescriptor } from "@/app/feature";
import { launcherFeature } from "./launcher/descriptor";

export const features: readonly FeatureDescriptor[] = [launcherFeature];
