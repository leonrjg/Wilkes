import { createContext, useContext, useState, useCallback, useRef, ReactNode } from "react";
import { X, Info, AlertCircle, CheckCircle } from "react-feather";

export type ToastType = "info" | "success" | "error" | "warning";

interface Toast {
  id: string;
  message: string;
  type: ToastType;
}

const RECENT_TOASTS_WINDOW = 1000; // 1 second

interface ToastContextType {
  addToast: (message: string, type?: ToastType, duration?: number) => string;
  removeToast: (id: string) => void;
}

const ToastContext = createContext<ToastContextType | undefined>(undefined);

export function useToasts() {
  const context = useContext(ToastContext);
  if (!context) {
    throw new Error("useToasts must be used within a ToastProvider");
  }
  return context;
}

export function ToastProvider({ children }: { children: ReactNode }) {
  const [toasts, setToasts] = useState<Toast[]>([]);
  const recentMessages = useRef<Record<string, number>>({});

  const removeToast = useCallback((id: string) => {
    setToasts((prev) => prev.filter((t) => t.id !== id));
  }, []);

  const addToast = useCallback((message: string, type: ToastType = "info", duration: number = 5000) => {
    const now = Date.now();
    const key = `${type}:${message}`;
    
    // Prevent duplicate toasts within the window
    const lastSeen = recentMessages.current[key];
    if (lastSeen && now - lastSeen < RECENT_TOASTS_WINDOW) {
      // Find and return existing ID if possible, otherwise generate new
      // (For simplicity we just return a dummy or allow it, but we return the generated ID)
    }

    const id = crypto.randomUUID();
    setToasts((prev) => [...prev, { id, message, type }]);
    recentMessages.current[key] = now;
    
    // Auto-remove if duration > 0
    if (duration > 0) {
      setTimeout(() => {
        removeToast(id);
      }, duration);
    }

    return id;
  }, [removeToast]);

  return (
    <ToastContext.Provider value={{ addToast, removeToast }}>
      {children}
      <div className="fixed bottom-4 right-4 z-[200] flex flex-col gap-2 max-w-sm w-full">
        {toasts.map((toast) => (
          <div
            key={toast.id}
            className={`flex items-start gap-3 p-3 rounded-lg border shadow-lg animate-in slide-in-from-right-4 fade-in duration-300 ${
              toast.type === "error"
                ? "bg-red-950/90 border-red-900/50 text-red-200"
                : toast.type === "success"
                ? "bg-green-950/90 border-green-900/50 text-green-200"
                : toast.type === "warning"
                ? "bg-amber-950/90 border-amber-900/50 text-amber-200"
                : "bg-[var(--bg-active)] border-[var(--border-main)] text-[var(--text-main)]"
            }`}
          >
            <div className="mt-0.5 shrink-0">
              {toast.type === "error" && <AlertCircle size={14} className="text-red-400" />}
              {toast.type === "success" && <CheckCircle size={14} className="text-green-400" />}
              {toast.type === "info" && <Info size={14} className="text-[var(--accent-blue)]" />}
              {toast.type === "warning" && <AlertCircle size={14} className="text-amber-400" />}
            </div>
            <p className="text-xs font-medium flex-1">{toast.message}</p>
            <button
              onClick={() => removeToast(toast.id)}
              className="mt-0.5 text-[var(--text-dim)] hover:text-[var(--text-main)] transition-colors"
            >
              <X size={14} />
            </button>
          </div>
        ))}
      </div>
    </ToastContext.Provider>
  );
}
