import React from "react";
import ReactDOM from "react-dom/client";
import { App } from "./App";
import { OverlayApp } from "./OverlayApp";
import "./styles.css";

/** Both windows load this same bundle; the window label decides which app to mount.
 *  `#overlay` in the URL is the browser-dev fallback (no Tauri metadata there). */
function isOverlayWindow() {
  if (window.location.hash === "#overlay") return true;
  try {
    const internals = (window as unknown as {
      __TAURI_INTERNALS__?: { metadata?: { currentWindow?: { label?: string } } };
    }).__TAURI_INTERNALS__;
    return internals?.metadata?.currentWindow?.label === "overlay";
  } catch {
    return false;
  }
}

const overlay = isOverlayWindow();
if (overlay) {
  // Lets overlay.css punch the page background out so the window can be see-through.
  document.documentElement.classList.add("ovWindow");
}

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>{overlay ? <OverlayApp /> : <App />}</React.StrictMode>,
);
