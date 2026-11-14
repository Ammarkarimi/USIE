import { type ButtonHTMLAttributes, type InputHTMLAttributes, type ReactNode, type SelectHTMLAttributes, forwardRef } from "react";

export function cx(...parts: (string | false | null | undefined)[]): string {
  return parts.filter(Boolean).join(" ");
}

type Variant = "primary" | "secondary" | "ghost" | "danger";

const variants: Record<Variant, string> = {
  primary: "bg-accent text-accent-fg hover:bg-accent/90",
  secondary: "bg-surface border border-border text-fg hover:bg-surface-2",
  ghost: "text-fg hover:bg-surface-2",
  danger: "bg-danger text-white hover:bg-danger/90",
};

export interface ButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: Variant;
  size?: "sm" | "md";
  busy?: boolean;
}

export const Button = forwardRef<HTMLButtonElement, ButtonProps>(function Button(
  { variant = "secondary", size = "md", busy = false, className, children, disabled, type = "button", ...rest },
  ref,
) {
  return (
    <button
      ref={ref}
      type={type}
      disabled={disabled || busy}
      aria-busy={busy || undefined}
      className={cx(
        "inline-flex items-center justify-center gap-1.5 rounded-md font-medium transition-colors",
        "focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent",
        "disabled:cursor-not-allowed disabled:opacity-50",
        size === "sm" ? "h-7 px-2.5 text-xs" : "h-9 px-3.5 text-sm",
        variants[variant],
        className,
      )}
      {...rest}
    >
      {busy && <Spinner size={12} />}
      {children}
    </button>
  );
});

export const Input = forwardRef<HTMLInputElement, InputHTMLAttributes<HTMLInputElement>>(function Input({ className, ...rest }, ref) {
  return (
    <input
      ref={ref}
      className={cx(
        "h-9 w-full rounded-md border border-border bg-surface px-3 text-sm text-fg placeholder:text-muted",
        "focus:border-accent focus:outline-none focus:ring-2 focus:ring-accent/30",
        className,
      )}
      {...rest}
    />
  );
});

export const Select = forwardRef<HTMLSelectElement, SelectHTMLAttributes<HTMLSelectElement>>(function Select({ className, ...rest }, ref) {
  return (
    <select
      ref={ref}
      className={cx("h-9 rounded-md border border-border bg-surface px-2 text-sm text-fg focus:border-accent focus:outline-none", className)}
      {...rest}
    />
  );
});

export function Field({ label, hint, children }: { label: string; hint?: ReactNode; children: ReactNode }) {
  return (
    <label className="block space-y-1">
      <span className="text-sm font-medium text-fg">{label}</span>
      {children}
      {hint && <span className="block text-xs text-muted">{hint}</span>}
    </label>
  );
}

export function Card({ title, actions, children, className, padded = true }: { title?: ReactNode; actions?: ReactNode; children: ReactNode; className?: string; padded?: boolean }) {
  return (
    <section className={cx("rounded-lg border border-border bg-surface", className)}>
      {(title || actions) && (
        <header className="flex items-center justify-between gap-2 border-b border-border px-4 py-2.5">
          <h2 className="text-sm font-semibold text-fg">{title}</h2>
          {actions && <div className="flex items-center gap-2">{actions}</div>}
        </header>
      )}
      <div className={padded ? "p-4" : undefined}>{children}</div>
    </section>
  );
}

export type Tone = "neutral" | "ok" | "warn" | "danger" | "info" | "accent";

const tones: Record<Tone, string> = {
  neutral: "bg-surface-2 text-muted border-border",
  ok: "bg-ok/10 text-ok border-ok/30",
  warn: "bg-warn/10 text-warn border-warn/30",
  danger: "bg-danger/10 text-danger border-danger/30",
  info: "bg-info/10 text-info border-info/30",
  accent: "bg-accent/10 text-accent border-accent/30",
};

export function Badge({ tone = "neutral", children, title }: { tone?: Tone; children: ReactNode; title?: string }) {
  return (
    <span title={title} className={cx("inline-flex items-center gap-1 whitespace-nowrap rounded border px-1.5 py-0.5 text-xs font-medium", tones[tone])}>
      {children}
    </span>
  );
}

const confidenceText: Record<string, string> = {
  confirmed: "Confirmed: established directly from declarations and imports.",
  probable: "Probable: very likely, but depends on runtime behaviour such as dynamic dispatch or package resolution.",
  inferred: "Inferred: heuristic match (for example a method name that is unique in the repository). Verify before relying on it.",
  unknown: "Unknown: could not be established.",
};

const confidenceClass: Record<string, string> = {
  confirmed: "bg-confirmed/10 text-confirmed border-confirmed/40",
  probable: "bg-probable/10 text-probable border-probable/40",
  inferred: "bg-inferred/10 text-inferred border-inferred/40 border-dashed",
  unknown: "bg-unknown/10 text-unknown border-unknown/40 border-dotted",
};

/** Confidence is communicated with text and border style, never colour alone. */
export function ConfidenceBadge({ level }: { level: string }) {
  return (
    <span title={confidenceText[level] ?? level} className={cx("inline-flex items-center rounded border px-1.5 py-0.5 text-xs font-medium", confidenceClass[level] ?? confidenceClass.unknown)}>
      {level}
    </span>
  );
}

const statusTone: Record<string, Tone> = {
  ready: "ok", succeeded: "ok", online: "ok", implemented: "ok", parsed: "ok",
  analyzing: "info", running: "info", busy: "info", experimental: "info",
  pending: "neutral", queued: "neutral", idle: "neutral", unsupported: "neutral", skipped: "neutral", planned: "neutral",
  failed: "danger", parsed_with_errors: "warn", cancelled: "warn", stopped: "neutral", offline: "warn",
};

export function StatusBadge({ status }: { status: string }) {
  const tone = statusTone[status] ?? "neutral";
  return (
    <Badge tone={tone}>
      {(status === "running" || status === "analyzing") && <Spinner size={10} />}
      {status.replace(/_/g, " ")}
    </Badge>
  );
}

export function Spinner({ size = 16, label }: { size?: number; label?: string }) {
  return (
    <svg role={label ? "status" : undefined} aria-label={label} aria-hidden={label ? undefined : true} width={size} height={size} viewBox="0 0 24 24" className="animate-spin">
      <circle cx="12" cy="12" r="10" fill="none" stroke="currentColor" strokeOpacity="0.25" strokeWidth="3" />
      <path d="M22 12a10 10 0 0 0-10-10" fill="none" stroke="currentColor" strokeWidth="3" strokeLinecap="round" />
    </svg>
  );
}

export function Loading({ label = "Loading" }: { label?: string }) {
  return (
    <div className="flex items-center gap-2 p-4 text-sm text-muted">
      <Spinner label={label} /> {label}…
    </div>
  );
}

export function ErrorBanner({ error, onRetry }: { error: unknown; onRetry?: () => void }) {
  if (!error) return null;
  const e = error as { message?: string; requestId?: string; code?: string };
  return (
    <div role="alert" className="flex items-start justify-between gap-3 rounded-md border border-danger/40 bg-danger/10 px-3 py-2 text-sm text-danger">
      <div>
        <p className="font-medium">{e.message ?? String(error)}</p>
        {e.requestId && <p className="mt-0.5 text-xs opacity-80">Request id: {e.requestId}</p>}
      </div>
      {onRetry && (
        <Button size="sm" variant="ghost" onClick={onRetry}>
          Retry
        </Button>
      )}
    </div>
  );
}

export function EmptyState({ title, children, action }: { title: string; children?: ReactNode; action?: ReactNode }) {
  return (
    <div className="flex flex-col items-center gap-2 px-6 py-10 text-center">
      <p className="font-medium text-fg">{title}</p>
      {children && <div className="max-w-md text-sm text-muted">{children}</div>}
      {action && <div className="mt-2">{action}</div>}
    </div>
  );
}

export function Stat({ label, value, hint }: { label: string; value: ReactNode; hint?: string }) {
  return (
    <div className="rounded-lg border border-border bg-surface px-4 py-3" title={hint}>
      <div className="text-xs font-medium uppercase tracking-wide text-muted">{label}</div>
      <div className="mt-1 text-2xl font-semibold tabular-nums text-fg">{value}</div>
    </div>
  );
}

export function Table({ children, className }: { children: ReactNode; className?: string }) {
  return (
    <div className={cx("overflow-x-auto", className)}>
      <table className="w-full border-collapse text-left text-sm [&_td]:border-t [&_td]:border-border [&_td]:px-3 [&_td]:py-2 [&_th]:px-3 [&_th]:py-2 [&_th]:text-xs [&_th]:font-medium [&_th]:uppercase [&_th]:tracking-wide [&_th]:text-muted">
        {children}
      </table>
    </div>
  );
}

export function Tabs<T extends string>({ value, onChange, items }: { value: T; onChange: (v: T) => void; items: { value: T; label: ReactNode }[] }) {
  return (
    <div role="tablist" className="flex gap-1 border-b border-border">
      {items.map((it) => (
        <button
          key={it.value}
          role="tab"
          type="button"
          aria-selected={value === it.value}
          onClick={() => onChange(it.value)}
          className={cx(
            "-mb-px border-b-2 px-3 py-2 text-sm font-medium",
            value === it.value ? "border-accent text-fg" : "border-transparent text-muted hover:text-fg",
          )}
        >
          {it.label}
        </button>
      ))}
    </div>
  );
}

/** A horizontal stacked bar of proportions with an accessible text fallback. */
export function Proportions({ items }: { items: { label: string; value: number; className: string }[] }) {
  const total = items.reduce((s, i) => s + i.value, 0);
  if (total === 0) return <p className="text-sm text-muted">No data.</p>;
  return (
    <div>
      <div className="flex h-2.5 overflow-hidden rounded-full bg-surface-2" aria-hidden="true">
        {items.filter((i) => i.value > 0).map((i) => (
          <div key={i.label} className={i.className} style={{ width: `${(i.value / total) * 100}%` }} />
        ))}
      </div>
      <ul className="mt-2 flex flex-wrap gap-x-4 gap-y-1 text-xs text-muted">
        {items.map((i) => (
          <li key={i.label} className="flex items-center gap-1.5">
            <span className={cx("inline-block h-2 w-2 rounded-full", i.className)} aria-hidden="true" />
            {i.label}: <span className="tabular-nums text-fg">{i.value.toLocaleString()}</span> ({((i.value / total) * 100).toFixed(1)}%)
          </li>
        ))}
      </ul>
    </div>
  );
}
