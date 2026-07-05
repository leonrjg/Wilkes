import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import "./styles.css";
import App from "./App";
import { ToastProvider } from "./components/Toast";
import GlobalErrorReporter from "./components/GlobalErrorReporter";

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <ToastProvider>
      <GlobalErrorReporter />
      <App />
    </ToastProvider>
  </StrictMode>,
);
