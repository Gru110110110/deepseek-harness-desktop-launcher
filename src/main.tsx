import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import "@/i18n";
import "./styles.css";
import { AppBootstrap } from "@/app/AppBootstrap";
import { initializeLauncherStore } from "@/platform/launcherStore";

void initializeLauncherStore();

const root = document.getElementById("root");
if (!root) throw new Error("Missing root element");
createRoot(root).render(
  <StrictMode>
    <AppBootstrap />
  </StrictMode>,
);
