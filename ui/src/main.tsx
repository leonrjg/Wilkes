import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import "./styles.css";
import App from "./App";
import { ToastProvider } from "./components/Toast";
import GlobalErrorReporter from "./components/GlobalErrorReporter";
import StartupGate from "./components/StartupGate";

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <ToastProvider>
      <GlobalErrorReporter />
      <StartupGate>
        <App />
      </StartupGate>
    </ToastProvider>
  </StrictMode>,
);
