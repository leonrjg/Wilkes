import { useEffect } from "react";
import { formatConsoleError, formatErrorMessage, shouldReportError } from "../lib/errorReporting";
import { useToasts } from "./Toast";

const ERROR_TOAST_DURATION_MS = 10000;

export default function GlobalErrorReporter() {
  const { addToast } = useToasts();

  useEffect(() => {
    const report = (message: string) => {
      const trimmed = message.trim();
      if (!trimmed || !shouldReportError(trimmed)) return;
      addToast(trimmed, { type: "error", duration: ERROR_TOAST_DURATION_MS });
    };

    const previousConsoleError = console.error;

    const handleError = (event: ErrorEvent) => {
      report(formatErrorMessage(event.error ?? event.message));
    };

    const handleUnhandledRejection = (event: PromiseRejectionEvent) => {
      report(formatErrorMessage(event.reason));
    };

    const wrappedConsoleError = (...args: unknown[]) => {
      previousConsoleError(...args);
      report(formatConsoleError(args));
    };
    console.error = wrappedConsoleError;

    window.addEventListener("error", handleError);
    window.addEventListener("unhandledrejection", handleUnhandledRejection);

    return () => {
      window.removeEventListener("error", handleError);
      window.removeEventListener("unhandledrejection", handleUnhandledRejection);
      if (console.error === wrappedConsoleError) {
        console.error = previousConsoleError;
      }
    };
  }, [addToast]);

  return null;
}
