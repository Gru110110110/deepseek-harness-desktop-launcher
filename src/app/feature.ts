import type { ComponentType } from "react";
import type { LucideProps } from "lucide-react";
import type { RouteObject } from "react-router-dom";

export interface FeatureDescriptor {
  id: string;
  routes: RouteObject[];
  navigation?: {
    labelKey: string;
    path: string;
    icon: ComponentType<LucideProps>;
    order: number;
  };
}
