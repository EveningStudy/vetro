import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import "./index.css";
import { initClient } from "./api/client";
import { takeLocalToken } from "./api/session";
import { App } from "./App";
import { takePairingCode } from "./components/PairingPage";
import { applyTheme } from "./state/prefs";

applyTheme();
const pairingCode = takePairingCode();
const localToken = takeLocalToken();
await initClient();

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App pairingCode={pairingCode} localToken={localToken} />
  </StrictMode>,
);
