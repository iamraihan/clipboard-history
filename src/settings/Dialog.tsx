import type { ReactNode } from "react";

/** A single centered question with a row of buttons, in the popup's palette. */
export function Dialog({ title, children, actions }: { title: string; children?: ReactNode; actions: ReactNode }) {
  return (
    <main role="alertdialog" aria-labelledby="dialog-title" className="flex h-full items-center justify-center p-6">
      <div className="w-full max-w-md">
        <h1 id="dialog-title" className="text-base font-semibold text-fg">
          {title}
        </h1>
        {children}
        <div className="mt-5 flex flex-wrap justify-end gap-2">{actions}</div>
      </div>
    </main>
  );
}

const BASE = "h-8 rounded-md border px-3 disabled:opacity-60";

export function Button({
  primary = false,
  ...props
}: React.ButtonHTMLAttributes<HTMLButtonElement> & { primary?: boolean }) {
  const look = primary
    ? "border-accent bg-accent text-bg hover:opacity-90"
    : "border-line bg-bg text-fg hover:bg-hover";
  return <button type="button" {...props} className={`${BASE} ${look}`} />;
}
