import { createContext, useContext, useState, useCallback, useRef, ReactNode, useEffect } from "react";
import { randomId } from "../lib/types";
import { X, Info, AlertCircle, CheckCircle, Clock } from "react-feather";


export type ToastType = "info" | "success" | "error" | "warning";

interface Toast {
  id: string;
  message: string;
  type: ToastType;
  startTime?: number;
  shimmer?: boolean;
}

const RECENT_TOASTS_WINDOW = 1000; // 1 second

interface ToastOptions {
  type?: ToastType;
  duration?: number;
  startTime?: number;
  shimmer?: boolean;
}

interface ToastContextType {
  addToast: (message: string, options?: ToastOptions) => string;
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

  const addToast = useCallback((message: string, options: ToastOptions = {}) => {
    const { type = "info", duration = 5000, startTime, shimmer } = options;
    const now = Date.now();
    const key = `${type}:${message}`;
    
    // Prevent duplicate toasts within the window
    const lastSeen = recentMessages.current[key];
    if (lastSeen && now - lastSeen < RECENT_TOASTS_WINDOW) {
      // Find and return existing ID if possible, otherwise generate new
    }

    const id = randomId();
    setToasts((prev) => [...prev, { id, message, type, startTime, shimmer }]);
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
          <ToastItem key={toast.id} toast={toast} onRemove={removeToast} />
        ))}
      </div>
    </ToastContext.Provider>
  );
}

function ToastItem({ toast, onRemove }: { toast: Toast; onRemove: (id: string) => void }) {
  const [elapsed, setElapsed] = useState(0);

  useEffect(() => {
    if (toast.startTime === undefined) return;
    const interval = setInterval(() => {
      setElapsed(Math.floor((Date.now() - (toast.startTime || 0)) / 1000));
    }, 1000);
    return () => clearInterval(interval);
  }, [toast.startTime]);

  const formatElapsed = (s: number) => {
    const mins = Math.floor(s / 60);
    const secs = s % 60;
    return mins > 0 ? `${mins}m ${secs}s` : `${secs}s`;
  };

  return (
    <div
      className={`flex flex-col rounded-lg border shadow-lg animate-in slide-in-from-right-4 fade-in duration-300 overflow-hidden ${
        toast.type === "error"
          ? "bg-red-950/90 border-red-900/50 text-red-200"
          : toast.type === "success"
            ? "bg-green-950/90 border-green-900/50 text-green-200"
            : toast.type === "warning"
              ? "bg-amber-950/90 border-amber-900/50 text-amber-200"
              : "bg-[var(--bg-active)] border-[var(--border-main)] text-[var(--text-main)]"
      }`}
    >
      <div className="flex items-start gap-3 p-3">
        <div className="mt-0.5 shrink-0">
          {toast.type === "error" && <AlertCircle size={14} className="text-red-400" />}
          {toast.type === "success" && <CheckCircle size={14} className="text-green-400" />}
          {toast.type === "info" && <Info size={14} className="text-[var(--accent-blue)]" />}
          {toast.type === "warning" && <AlertCircle size={14} className="text-amber-400" />}
        </div>
        <div className="flex flex-col gap-1 flex-1">
          <p className="text-xs font-medium">{toast.message}</p>
          {toast.startTime !== undefined && (
            <div className="flex items-center gap-1 text-[10px] font-mono opacity-70">
              <Clock size={10} />
              <span>Elapsed: {formatElapsed(elapsed)}</span>
            </div>
          )}
        </div>
        <button
          onClick={() => onRemove(toast.id)}
          className="mt-0.5 text-[var(--text-dim)] hover:text-[var(--text-main)] transition-colors"
        >
          <X size={14} />
        </button>
      </div>
      {toast.shimmer && (
        <div className="h-[3px] w-full relative overflow-hidden bg-white/5">
          <div className="absolute inset-0 bg-[var(--accent-blue)] animate-shimmer" />
        </div>
      )}
    </div>
  );
}
