import { useEffect, useRef, type ReactNode } from "react";
import { t } from "./i18n";

// Secondary and destructive actions live behind one disclosure so each row keeps
// a single obvious next step. Closes on outside click, Escape, or after a choice.
export function MoreMenu({ children }: { children: ReactNode }) {
  const ref = useRef<HTMLDetailsElement>(null);
  useEffect(() => {
    const element = ref.current!;
    const outside = (event: PointerEvent) => {
      if (element.open && !element.contains(event.target as Node))
        element.open = false;
    };
    const escape = (event: KeyboardEvent) => {
      if (event.key === "Escape" && element.open) {
        element.open = false;
        element.querySelector("summary")?.focus();
      }
    };
    document.addEventListener("pointerdown", outside);
    document.addEventListener("keydown", escape);
    return () => {
      document.removeEventListener("pointerdown", outside);
      document.removeEventListener("keydown", escape);
    };
  }, []);
  return (
    <details className="more-menu" ref={ref}>
      <summary>{t("More")}</summary>
      <div
        className="more-items"
        onClick={(event) => {
          if ((event.target as HTMLElement).closest("button"))
            ref.current!.open = false;
        }}
      >
        {children}
      </div>
    </details>
  );
}
