const RECENT_ERROR_WINDOW_MS = 1000;

const recentErrors = new Map<string, number>();

export function formatErrorMessage(value: unknown): string {
  if (value instanceof Error) {
    return value.message || value.name || "Unknown error";
  }

  if (typeof value === "string") {
    return value;
  }

  if (value && typeof value === "object") {
    const maybeMessage = "message" in value ? value.message : null;
    if (typeof maybeMessage === "string" && maybeMessage.trim()) {
      return maybeMessage;
    }

    try {
      return JSON.stringify(value);
    } catch {
      return String(value);
    }
  }

  return String(value ?? "Unknown error");
}

export function formatConsoleError(args: unknown[]): string {
  const message = args.map(formatErrorMessage).filter(Boolean).join(" ");
  return message || "Unknown error";
}

export function shouldReportError(message: string, now = Date.now()): boolean {
  const lastSeen = recentErrors.get(message);
  recentErrors.set(message, now);

  for (const [key, timestamp] of recentErrors) {
    if (now - timestamp > RECENT_ERROR_WINDOW_MS) {
      recentErrors.delete(key);
    }
  }

  return lastSeen === undefined || now - lastSeen >= RECENT_ERROR_WINDOW_MS;
}

export function resetErrorReportingForTests() {
  recentErrors.clear();
}
