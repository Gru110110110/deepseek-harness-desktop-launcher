import { Navigate, createHashRouter } from "react-router-dom";
import { AppShell } from "./AppShell";
import { features } from "@/features/registry";

export const router = createHashRouter([
  {
    path: "/",
    element: <AppShell />,
    children: [
      { index: true, element: <Navigate to="/launcher" replace /> },
      ...features.flatMap((feature) => feature.routes),
      { path: "*", element: <Navigate to="/launcher" replace /> },
    ],
  },
]);
