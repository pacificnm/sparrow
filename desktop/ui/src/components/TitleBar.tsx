import { useEffect, useState, type ReactNode } from "react";
import { WindowControls } from "./WindowControls";
import { isTauri } from "../lib/tauri";

type TitleBarMenu = "file" | "help" | null;

type TitleBarProps = {
  /** Centered window title (usually the app's display name). */
  title: string;
  /** File → Quit. */
  onQuit: () => void;
  /** Help → About. */
  onAbout: () => void;
};

const menuButtonClass = "h-full px-2.5 text-[12px] text-nest-foreground hover:bg-nest-muted/12";

const menuDropdownClass =
  "absolute left-0 top-full z-[80] min-w-48 rounded-nest-md border border-nest-border bg-nest-background py-1 shadow-lg";

const menuItemClass = "flex w-full items-center px-3 py-1.5 text-left text-[12px] hover:bg-nest-muted/10";

/**
 * Frameless title bar: File/Help menus, centered app title, window controls.
 * Pairs with `"decorations": false` in `tauri.conf.json`.
 */
export function TitleBar({ title, onQuit, onAbout }: TitleBarProps) {
  const [openMenu, setOpenMenu] = useState<TitleBarMenu>(null);
  const close = () => setOpenMenu(null);
  const showWindowChrome = isTauri();

  useEffect(() => {
    if (!openMenu) {
      return;
    }
    const onPointerDown = (event: MouseEvent) => {
      const target = event.target;
      if (!(target instanceof Element)) {
        return;
      }
      if (target.closest("[data-titlebar-menu]")) {
        return;
      }
      close();
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        close();
      }
    };
    window.addEventListener("mousedown", onPointerDown, true);
    window.addEventListener("keydown", onKeyDown);
    return () => {
      window.removeEventListener("mousedown", onPointerDown, true);
      window.removeEventListener("keydown", onKeyDown);
    };
  }, [openMenu]);

  const toggleMenu = (menu: Exclude<TitleBarMenu, null>) => {
    setOpenMenu((current) => (current === menu ? null : menu));
  };

  return (
    <header className="relative flex h-8 shrink-0 items-stretch border-b border-nest-border bg-nest-surface text-[13px]">
      <div className="relative z-10 flex h-full shrink-0 items-stretch pl-2" data-titlebar-menu>
        <MenuDropdown label="File" open={openMenu === "file"} onToggle={() => toggleMenu("file")}>
          <MenuItem
            label="Quit"
            onClick={() => {
              onQuit();
              close();
            }}
          />
        </MenuDropdown>

        <MenuDropdown label="Help" open={openMenu === "help"} onToggle={() => toggleMenu("help")}>
          <MenuItem
            label="About"
            onClick={() => {
              onAbout();
              close();
            }}
          />
        </MenuDropdown>
      </div>

      {showWindowChrome ? (
        <div className="min-w-0 flex-1" data-tauri-drag-region />
      ) : (
        <div className="min-w-0 flex-1" />
      )}

      <div className="relative z-10 flex h-full shrink-0 items-stretch">
        {showWindowChrome ? <WindowControls /> : null}
      </div>

      <p
        className="pointer-events-none absolute inset-0 z-0 flex items-center justify-center px-28"
        aria-hidden
      >
        <span className="truncate text-[12px] font-medium text-nest-foreground">{title}</span>
      </p>
    </header>
  );
}

function MenuDropdown({
  label,
  open,
  onToggle,
  children,
}: {
  label: string;
  open: boolean;
  onToggle: () => void;
  children: ReactNode;
}) {
  return (
    <div className="relative flex h-full items-stretch">
      <button type="button" className={menuButtonClass} onClick={onToggle}>
        {label}
      </button>
      {open ? (
        <div className={menuDropdownClass} role="menu" data-titlebar-menu>
          {children}
        </div>
      ) : null}
    </div>
  );
}

function MenuItem({ label, onClick }: { label: string; onClick: () => void }) {
  return (
    <button type="button" role="menuitem" className={menuItemClass} onClick={onClick}>
      {label}
    </button>
  );
}
