import { type FormEvent, type ReactNode, useState } from "react";
import { Link, useNavigate } from "react-router-dom";
import { ApiError } from "@usie/sdk";
import { Button, ErrorBanner, Field, Input, Loading } from "@usie/design-system";
import { api } from "../api";
import { useAuth } from "../auth";
import { useAsync } from "../hooks";

/**
 * Reads the one-time token from the URL fragment (never sent to servers or
 * in Referer headers) and removes it from the address bar and history.
 */
function useFragmentToken(): string {
  const [token] = useState(() => {
    const t = new URLSearchParams(window.location.hash.replace(/^#/, "")).get("token") ?? "";
    if (t) window.history.replaceState(window.history.state, "", window.location.pathname);
    return t;
  });
  return token;
}

function Panel({ title, children }: { title: string; children: ReactNode }) {
  return (
    <div className="flex min-h-full items-center justify-center p-6">
      <div className="w-full max-w-sm space-y-4 rounded-lg border border-border bg-surface p-6 shadow-sm">
        <div className="flex items-center gap-2">
          <img src="/favicon.svg" alt="" className="h-7 w-7" />
          <h1 className="text-lg font-semibold">{title}</h1>
        </div>
        {children}
      </div>
    </div>
  );
}

const invalidLink = (
  <>
    <p className="text-sm text-muted">This link is invalid, has expired or was already used. Ask for a new one.</p>
    <Link to="/login" className="block text-sm text-accent hover:underline">Go to sign in</Link>
  </>
);

export function AcceptInvite() {
  const token = useFragmentToken();
  const { principal, setPrincipal } = useAuth();
  const navigate = useNavigate();
  const preview = useAsync(() => (token ? api.previewInvitation(token) : Promise.reject(new ApiError(404, "invalid_link", "", ""))), [token]);
  const [form, setForm] = useState({ password: "", display_name: "" });
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);

  if (preview.error) {
    const missing = preview.error instanceof ApiError && preview.error.status === 404;
    return <Panel title="Invitation">{missing ? invalidLink : <ErrorBanner error={preview.error} onRetry={preview.reload} />}</Panel>;
  }
  if (!preview.data) return <Loading label="Checking the invitation" />;
  const inv = preview.data;
  const existing = inv.existing_account;

  async function accept(e: FormEvent) {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      const session = await api.acceptInvitation({ token, password: form.password, ...(existing ? {} : { display_name: form.display_name.trim() }) });
      setPrincipal(session.principal);
      navigate("/", { replace: true });
    } catch (err) {
      setError(err);
    } finally {
      setBusy(false);
    }
  }

  return (
    <Panel title={`Join ${inv.organization_name}`}>
      <p className="text-sm text-muted">
        You were invited to <span className="font-medium text-fg">{inv.organization_name}</span> as <span className="font-medium text-fg">{inv.role}</span>, at {inv.email}.
      </p>
      {principal && principal.email.toLowerCase() !== inv.email.toLowerCase() && (
        <p className="rounded-md border border-warn/40 bg-warn/10 p-2 text-xs text-warn">
          You are signed in as {principal.email}. Accepting signs you in as {inv.email} instead.
        </p>
      )}
      <form onSubmit={accept} className="space-y-4">
        <ErrorBanner error={error} />
        {!existing && (
          <Field label="Your name">
            <Input required maxLength={100} value={form.display_name} onChange={(e) => setForm({ ...form, display_name: e.target.value })} />
          </Field>
        )}
        <Field label={existing ? "Password of your existing account" : "Choose a password"} hint={existing ? "This address already has a USIE account." : "At least 10 characters."}>
          <Input type="password" autoComplete={existing ? "current-password" : "new-password"} required minLength={existing ? undefined : 10}
            value={form.password} onChange={(e) => setForm({ ...form, password: e.target.value })} />
        </Field>
        <Button type="submit" variant="primary" className="w-full" busy={busy}>{existing ? "Join organization" : "Create account and join"}</Button>
      </form>
    </Panel>
  );
}

export function ResetPassword() {
  const token = useFragmentToken();
  const { setPrincipal } = useAuth();
  const [form, setForm] = useState({ password: "", confirm: "" });
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const [done, setDone] = useState(false);
  const [invalid, setInvalid] = useState(!token);

  async function submit(e: FormEvent) {
    e.preventDefault();
    setError(null);
    if (form.password !== form.confirm) {
      setError(new Error("The passwords do not match."));
      return;
    }
    setBusy(true);
    try {
      await api.confirmPasswordReset(token, form.password);
      setDone(true);
      // Every session of the account ended, possibly including this one.
      setPrincipal(null);
    } catch (err) {
      if (err instanceof ApiError && err.code === "invalid_link") setInvalid(true);
      else setError(err);
    } finally {
      setBusy(false);
    }
  }

  if (invalid) return <Panel title="Reset password">{invalidLink}</Panel>;
  if (done) {
    return (
      <Panel title="Password changed">
        <p role="status" className="text-sm text-muted">Your password was changed and every session was signed out.</p>
        <Link to="/login" className="block text-sm text-accent hover:underline">Sign in with the new password</Link>
      </Panel>
    );
  }
  return (
    <Panel title="Choose a new password">
      <form onSubmit={submit} className="space-y-4">
        <ErrorBanner error={error} />
        <Field label="New password" hint="At least 10 characters.">
          <Input type="password" autoComplete="new-password" required minLength={10} value={form.password} onChange={(e) => setForm({ ...form, password: e.target.value })} />
        </Field>
        <Field label="Repeat the new password">
          <Input type="password" autoComplete="new-password" required minLength={10} value={form.confirm} onChange={(e) => setForm({ ...form, confirm: e.target.value })} />
        </Field>
        <Button type="submit" variant="primary" className="w-full" busy={busy}>Set password</Button>
      </form>
    </Panel>
  );
}

export function ForgotPassword() {
  const system = useAsync(() => api.system(), []);
  const [email, setEmail] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const [sent, setSent] = useState<string | null>(null);

  async function submit(e: FormEvent) {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      const out = await api.requestPasswordReset(email.trim());
      setSent(out.message);
    } catch (err) {
      setError(err);
    } finally {
      setBusy(false);
    }
  }

  if (!system.data && !system.error) return <Loading />;
  return (
    <Panel title="Forgot your password?">
      {system.data && !system.data.email ? (
        <p className="text-sm text-muted">
          This installation does not send email. Ask an administrator of your organization for a password reset link
          (Settings → Team → Reset link).
        </p>
      ) : sent ? (
        <p role="status" className="text-sm text-muted">{sent} The link is valid for one hour.</p>
      ) : (
        <form onSubmit={submit} className="space-y-4">
          <ErrorBanner error={error ?? system.error} />
          <Field label="Email">
            <Input type="email" autoComplete="email" required value={email} onChange={(e) => setEmail(e.target.value)} />
          </Field>
          <Button type="submit" variant="primary" className="w-full" busy={busy}>Email me a reset link</Button>
        </form>
      )}
      <Link to="/login" className="block text-sm text-accent hover:underline">Back to sign in</Link>
    </Panel>
  );
}
