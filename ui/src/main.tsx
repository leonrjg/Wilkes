import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import "./styles.css";
import App from "./App";
import { ToastProvider } from "./components/Toast";
import GlobalErrorReporter from "./components/GlobalErrorReporter";
import StartupGate from "./components/StartupGate";
import DocumentApp from "./DocumentApp";
import { useViewerStore } from "./stores/useViewerStore";

const documentMode = new URLSearchParams(window.location.search).get("mode") === "document";
if (documentMode) useViewerStore.getState().enterStandaloneMode();

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <ToastProvider>
      <GlobalErrorReporter />
      <StartupGate>
        {documentMode ? <DocumentApp /> : <App />}
      </StartupGate>
    </ToastProvider>
  </StrictMode>,
);
