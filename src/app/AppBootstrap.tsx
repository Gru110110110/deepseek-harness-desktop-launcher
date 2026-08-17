import {
  Component,
  Suspense,
  type ErrorInfo,
  type PropsWithChildren,
} from "react";
import { RouterProvider } from "react-router-dom";
import { Toaster } from "sonner";
import { router } from "./router";

class AppErrorBoundary extends Component<
  PropsWithChildren,
  { failed: boolean }
> {
  override state = { failed: false };
  static getDerivedStateFromError() {
    return { failed: true };
  }
  override componentDidCatch(error: Error, info: ErrorInfo) {
    console.error("Application render failed", error, info.componentStack);
  }
  override render() {
    if (this.state.failed) {
      return (
        <div className="fatal-error">
          <h1>DSH Launcher</h1>
          <p>The interface could not be loaded.</p>
        </div>
      );
    }
    return this.props.children;
  }
}

export function AppBootstrap() {
  return (
    <AppErrorBoundary>
      <Suspense fallback={<div className="app-loading" aria-label="Loading" />}>
        <RouterProvider router={router} />
        <Toaster richColors closeButton position="top-right" />
      </Suspense>
    </AppErrorBoundary>
  );
}
