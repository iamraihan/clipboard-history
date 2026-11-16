import { LogicalSize, getCurrentWindow } from "@tauri-apps/api/window";
import { useLayoutEffect, useRef, type ReactNode } from "react";

/** The dialog window's width; its height follows the content. */
const DIALOG_WIDTH = 460;

/**
 * A question with a row of buttons, in the popup's palette. The window is
 * resized to the content, so an error or a long list never pushes the
 * buttons out of view.
 */
export function Dialog({ title, children, actions }: { title: string; children?: ReactNode; actions: ReactNode }) {
  const ref = useRef<HTMLElement>(null);

  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const fit = (): void => {
      const height = Math.ceil(el.getBoundingClientRect().height);
      getCurrentWindow()
        .setSize(new LogicalSize(DIALOG_WIDTH, height))
        .catch(() => undefined);
    };
    const observer = new ResizeObserver(fit);
    observer.observe(el);
    return () => observer.disconnect();
  }, []);

  return (
    <main ref={ref} role="alertdialog" aria-labelledby="dialog-title" className="flex flex-col p-6">
      <h1 id="dialog-title" className="text-[15px] font-semibold text-balance text-fg">
        {title}
      </h1>
      <div className="leading-normal">{children}</div>
      <div className="flex flex-wrap justify-end gap-2 pt-6">{actions}</div>
    </main>
  );
}

const BASE = "h-8 rounded-md border px-3.5 font-medium transition-colors disabled:opacity-60";

export function Button({
  primary = false,
  ...props
}: React.ButtonHTMLAttributes<HTMLButtonElement> & { primary?: boolean }) {
  const look = primary
    ? "border-accent bg-accent text-bg hover:bg-accent/90"
    : "border-line bg-bg text-fg hover:bg-hover";
  return <button type="button" {...props} className={`${BASE} ${look}`} />;
}
