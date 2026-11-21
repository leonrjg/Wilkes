import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import "./styles.css";
import App from "./App";
import { ToastProvider } from "./components/Toast";
import GlobalErrorReporter from "./components/GlobalErrorReporter";
import StartupGate from "./components/StartupGate";
import ApiLoginGate from "./components/ApiLoginGate";
import DocumentApp from "./DocumentApp";
import { useViewerStore } from "./stores/useViewerStore";
import { routeLinkClicks } from "./lib/openLink";

const documentMode = new URLSearchParams(window.location.search).get("mode") === "document";
if (documentMode) useViewerStore.getState().enterStandaloneMode();
routeLinkClicks();

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <ToastProvider>
      <GlobalErrorReporter />
      <ApiLoginGate>
        <StartupGate>
          {documentMode ? <DocumentApp /> : <App />}
        </StartupGate>
      </ApiLoginGate>
    </ToastProvider>
  </StrictMode>,
);
