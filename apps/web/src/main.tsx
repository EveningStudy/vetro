import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import "./index.css";
import { initClient } from "./api/client";
import { takeLocalToken } from "./api/session";
import { App } from "./App";
import { takePairingCode } from "./components/PairingPage";
import { ensureLocale } from "./i18n";
import { applyLanguage, applyTheme, resolvedLanguage, usePrefs } from "./state/prefs";

applyTheme();
applyLanguage();
const pairingCode = takePairingCode();
const localToken = takeLocalToken();
await Promise.all([
  initClient(),
  Promise.race([
    ensureLocale(resolvedLanguage(usePrefs.getState().language)),
    new Promise<void>((resolve) => setTimeout(resolve, 500)),
  ]),
]);

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App pairingCode={pairingCode} localToken={localToken} />
  </StrictMode>,
);
