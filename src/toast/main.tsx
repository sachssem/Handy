import React from "react";
import ReactDOM from "react-dom/client";
import { listen } from "@tauri-apps/api/event";
import LearnedToast from "./LearnedToast";
import {
  applyTheme,
  getStoredTheme,
  syncThemeFromSettings,
} from "@/lib/utils/theme";
import type { Theme } from "@/bindings";
import "@/i18n";

// Own webview, so the toast sets `data-theme` on its own document, exactly like
// the recording overlay: last-known theme before render (shared localStorage)
// to avoid a flash, reconcile with the persisted setting, then follow changes.
applyTheme(getStoredTheme());
syncThemeFromSettings();
listen<Theme>("theme-changed", (event) => applyTheme(event.payload));

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <LearnedToast />
  </React.StrictMode>,
);
