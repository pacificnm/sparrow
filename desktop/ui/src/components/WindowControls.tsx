import { useCallback, useEffect, useState, type ReactNode } from "react";
import { Icon } from "./Icon";
import { faMinus, faWindowMaximize, faWindowRestore, faXmark } from "../lib/fontawesome";
import { closeWindow, isTauri, minimizeWindow, toggleMaximizeWindow } from "../lib/tauri";

const CONTROL_WIDTH = 46;

/**
 * Minimize / maximize / close for frameless Tauri windows (`decorations: false`).
 */
export function WindowControls() {
  const [maximized, setMaximized] = useState(false);
  const tauri = isTauri();

  useEffect(() => {
    if (!tauri) {
      return;
    }
    let cancelled = false;
    let unlisten: (() => void) | undefined;

    void (async () => {
      const { getCurrentWindow } = await import("@tauri-apps/api/window");
      const win = getCurrentWindow();
      const current = await win.isMaximized();
      if (!cancelled) {
        setMaximized(current);
      }
      unlisten = await win.onResized(async () => {
        const next = await win.isMaximized();
        if (!cancelled) {
          setMaximized(next);
        }
      });
    })();

    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [tauri]);

  const onMinimize = useCallback(() => {
    void minimizeWindow();
  }, []);

  const onMaximize = useCallback(() => {
    void toggleMaximizeWindow();
  }, []);

  const onClose = useCallback(() => {
    void closeWindow();
  }, []);

  if (!tauri) {
    return null;
  }

  return (
    <div className="flex h-full items-stretch">
      <ControlButton label="Minimize" onClick={onMinimize}>
        <Icon icon={faMinus} className="size-3" />
      </ControlButton>
      <ControlButton label={maximized ? "Restore" : "Maximize"} onClick={onMaximize}>
        <Icon icon={maximized ? faWindowRestore : faWindowMaximize} className="size-3" />
      </ControlButton>
      <ControlButton label="Close" onClick={onClose} danger>
        <Icon icon={faXmark} className="size-3.5" />
      </ControlButton>
    </div>
  );
}

function ControlButton({
  label,
  onClick,
  danger,
  children,
}: {
  label: string;
  onClick: () => void;
  danger?: boolean;
  children: ReactNode;
}) {
  return (
    <button
      type="button"
      title={label}
      aria-label={label}
      onClick={onClick}
      style={{ width: CONTROL_WIDTH }}
      className={[
        "flex h-full items-center justify-center transition-colors",
        danger
          ? "text-nest-foreground/90 hover:bg-red-600 hover:text-white"
          : "text-nest-foreground/85 hover:bg-nest-muted/15 hover:text-nest-foreground",
      ].join(" ")}
    >
      {children}
    </button>
  );
}
