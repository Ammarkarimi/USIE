import { type FormEvent, useState } from "react";
import { useLocation, useNavigate } from "react-router-dom";
import { Button, ErrorBanner, Field, Input } from "@usie/design-system";
import { api } from "../api";
import { useAuth } from "../auth";
import { useAsync } from "../hooks";

export function Login() {
  const { setPrincipal } = useAuth();
  const navigate = useNavigate();
  const location = useLocation();
  const system = useAsync(() => api.system(), []);
  const [mode, setMode] = useState<"login" | "register">("login");
  const [form, setForm] = useState({ email: "", password: "", display_name: "", organization: "" });
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const signupAllowed = system.data?.signup_allowed ?? false;
  const registering = mode === "register" && signupAllowed;

  async function submit(e: FormEvent) {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      const session = registering ? await api.register(form) : await api.login(form.email, form.password);
      setPrincipal(session.principal);
      const from = (location.state as { from?: string } | null)?.from;
      navigate(from && from !== "/login" ? from : "/", { replace: true });
    } catch (err) {
      setError(err);
    } finally {
      setBusy(false);
    }
  }
  const set = (k: keyof typeof form) => (e: React.ChangeEvent<HTMLInputElement>) => setForm({ ...form, [k]: e.target.value });

  return (
    <div className="flex min-h-full items-center justify-center p-6">
      <form onSubmit={submit} className="w-full max-w-sm space-y-4 rounded-lg border border-border bg-surface p-6 shadow-sm">
        <div className="flex items-center gap-2">
          <img src="/favicon.svg" alt="" className="h-7 w-7" />
          <h1 className="text-lg font-semibold">{registering ? "Create the first account" : "Sign in to USIE"}</h1>
        </div>
        {registering && (
          <p className="text-sm text-muted">This installation has no accounts yet. The first account owns a new organization.</p>
        )}
        <ErrorBanner error={error} />
        <Field label="Email">
          <Input type="email" autoComplete="email" required value={form.email} onChange={set("email")} />
        </Field>
        <Field label="Password" hint={registering ? "At least 10 characters." : undefined}>
          <Input type="password" autoComplete={registering ? "new-password" : "current-password"} required minLength={registering ? 10 : undefined} value={form.password} onChange={set("password")} />
        </Field>
        {registering && (
          <>
            <Field label="Your name">
              <Input required value={form.display_name} onChange={set("display_name")} />
            </Field>
            <Field label="Organization">
              <Input required value={form.organization} onChange={set("organization")} />
            </Field>
          </>
        )}
        <Button type="submit" variant="primary" className="w-full" busy={busy}>
          {registering ? "Create account" : "Sign in"}
        </Button>
        {signupAllowed && (
          <button type="button" className="w-full text-center text-sm text-accent hover:underline" onClick={() => setMode(registering ? "login" : "register")}>
            {registering ? "I already have an account" : "Create an account"}
          </button>
        )}
      </form>
    </div>
  );
}
