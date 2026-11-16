import type { ReactNode } from "react";

/** A single centered question with a row of buttons, in the popup's palette. */
export function Dialog({ title, children, actions }: { title: string; children?: ReactNode; actions: ReactNode }) {
  return (
    <main role="alertdialog" aria-labelledby="dialog-title" className="flex h-full flex-col p-6">
      <h1 id="dialog-title" className="text-[15px] font-semibold text-balance text-fg">
        {title}
      </h1>
      <div className="leading-normal">{children}</div>
      <div className="mt-auto flex flex-wrap justify-end gap-2 pt-5">{actions}</div>
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
