import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import "./index.css";
import { initClient } from "./api/client";
import { App } from "./App";
import { applyTheme } from "./state/prefs";

applyTheme();
await initClient();

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
