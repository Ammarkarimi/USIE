import { type FormEvent, useState } from "react";
import { Badge, Button, Card, EmptyState, ErrorBanner, Field, Input, Loading, Select, StatusBadge, Table, formatRelative } from "@usie/design-system";
import type { Role } from "@usie/sdk";
import { api } from "../api";
import { useAuth, useCan } from "../auth";
import { useAsync } from "../hooks";

const ROLES: Role[] = ["viewer", "member", "admin", "owner"];

const roleHelp: Record<Role, string> = {
  viewer: "reads repositories, graphs, impact and changes",
  member: "also adds and analyzes repositories, asks the AI, proposes and verifies changes",
  admin: "also manages members, invitations and the audit log",
  owner: "also manages owners",
};

/** Roles the signed-in user may grant: admins grant up to admin, owners any role. */
function grantable(actor: Role): Role[] {
  return actor === "owner" ? ROLES : actor === "admin" ? ROLES.filter((r) => r !== "owner") : [];
}

function absolute(path: string): string {
  return window.location.origin + path;
}

function OneTimeLink({ title, link, note }: { title: string; link: string; note: string }) {
  return (
    <div role="status" className="rounded-md border border-ok/40 bg-ok/10 p-3 text-sm">
      <p className="font-medium text-ok">{title}</p>
      <code className="mt-1 block break-all rounded bg-surface p-2 font-mono text-xs">{link}</code>
      <div className="mt-2 flex items-center gap-3">
        <Button size="sm" onClick={() => void navigator.clipboard?.writeText(link)}>Copy link</Button>
        <span className="text-xs text-muted">{note}</span>
      </div>
    </div>
  );
}

export function Team() {
  const { principal } = useAuth();
  const isAdmin = useCan("admin");
  const members = useAsync(() => api.listMembers(), []);
  const [error, setError] = useState<unknown>(null);
  const [resetLink, setResetLink] = useState<{ who: string; link: string } | null>(null);
  const me = principal!;
  const roles = grantable(me.role);

  async function act(fn: () => Promise<unknown>) {
    setError(null);
    try {
      await fn();
      members.reload();
    } catch (e) {
      setError(e);
    }
  }

  return (
    <div className="space-y-6">
      {isAdmin && <Invitations />}
      <Card title={`Members of ${me.organization_name}`} padded={false}>
        <div className="space-y-3 p-3 empty:hidden">
          <ErrorBanner error={error ?? members.error} />
          {resetLink && (
            <OneTimeLink
              title={`Password reset link for ${resetLink.who}. It is shown once and works for 24 hours.`}
              link={resetLink.link}
              note="Give it to them through a channel you trust."
            />
          )}
        </div>
        {!members.data ? <Loading /> : (
          <Table>
            <thead><tr><th>Name</th><th>Email</th><th>Role</th><th>Joined</th>{isAdmin && <th />}</tr></thead>
            <tbody>
              {members.data.map((m) => {
                const self = m.user_id === me.user_id;
                const editable = isAdmin && !self && (me.role === "owner" || m.role !== "owner");
                return (
                  <tr key={m.user_id}>
                    <td>{m.display_name} {self && <Badge>you</Badge>} {m.disabled && <Badge tone="warn">disabled</Badge>}</td>
                    <td className="text-xs text-muted">{m.email}</td>
                    <td>
                      {editable ? (
                        <Select aria-label={`Role of ${m.display_name}`} value={m.role} className="h-7 text-xs"
                          onChange={(e) => void act(() => api.setMemberRole(m.user_id, e.target.value as Role))}>
                          {roles.map((r) => <option key={r} value={r}>{r}</option>)}
                        </Select>
                      ) : m.role}
                    </td>
                    <td className="text-xs text-muted" title={m.joined_at}>{formatRelative(m.joined_at)}</td>
                    {isAdmin && (
                      <td className="whitespace-nowrap text-right">
                        {editable && (
                          <>
                            <Button size="sm" variant="ghost" title="Create a one-time password reset link (only for people in no other organization)"
                              onClick={() => void act(async () => {
                                const out = await api.issuePasswordReset(m.user_id);
                                setResetLink({ who: m.display_name, link: absolute(out.reset_path) });
                              })}>
                              Reset link
                            </Button>
                            <Button size="sm" variant="ghost"
                              onClick={() => {
                                if (window.confirm(`Remove ${m.display_name} from ${me.organization_name}? Their sessions and API tokens here stop working.`)) {
                                  void act(() => api.removeMember(m.user_id));
                                }
                              }}>
                              Remove
                            </Button>
                          </>
                        )}
                      </td>
                    )}
                  </tr>
                );
              })}
            </tbody>
          </Table>
        )}
      </Card>
      <Card title="Roles">
        <ul className="space-y-1 text-sm">
          {ROLES.map((r) => <li key={r}><span className="font-medium">{r}</span> <span className="text-muted">— {roleHelp[r]}</span></li>)}
        </ul>
        <p className="mt-2 text-xs text-muted">Nobody changes their own role, and an organization always keeps at least one owner. The server enforces these rules.</p>
      </Card>
    </div>
  );
}

function Invitations() {
  const { principal } = useAuth();
  const list = useAsync(() => api.listInvitations(), []);
  const [email, setEmail] = useState("");
  const [role, setRole] = useState<Role>("member");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const [result, setResult] = useState<{ email: string; link?: string; emailed: boolean; emailError?: string } | null>(null);

  async function invite(e: FormEvent) {
    e.preventDefault();
    setBusy(true);
    setError(null);
    setResult(null);
    try {
      const out = await api.invite(email.trim(), role);
      setResult({ email: email.trim(), emailed: out.emailed, link: out.accept_path ? absolute(out.accept_path) : undefined, emailError: out.email_error });
      setEmail("");
      list.reload();
    } catch (err) {
      setError(err);
    } finally {
      setBusy(false);
    }
  }

  const invitations = list.data?.invitations ?? [];
  return (
    <Card title="Invite people">
      <form onSubmit={invite} className="flex flex-wrap items-end gap-3">
        <Field label="Email"><Input type="email" required maxLength={320} value={email} onChange={(e) => setEmail(e.target.value)} className="w-72" /></Field>
        <Field label="Role">
          <Select value={role} onChange={(e) => setRole(e.target.value as Role)}>
            {grantable(principal!.role).map((r) => <option key={r} value={r}>{r}</option>)}
          </Select>
        </Field>
        <Button type="submit" variant="primary" busy={busy}>Invite</Button>
      </form>
      <p className="mt-2 text-xs text-muted">
        {list.data?.email_enabled
          ? "The invitation link is emailed and valid for 7 days."
          : "This installation does not send email: you get the invitation link once, to share yourself. It is valid for 7 days."}
      </p>
      <div className="mt-3 space-y-3">
        <ErrorBanner error={error ?? list.error} />
        {result?.emailed && <p role="status" className="text-sm text-ok">Invitation emailed to {result.email}.</p>}
        {result?.link && (
          <OneTimeLink
            title={result.emailError ? `${result.emailError}.` : `Invitation link for ${result.email}. It is shown once.`}
            link={result.link}
            note="Anyone with this link can join as the invited role."
          />
        )}
      </div>
      {invitations.length === 0 ? (
        list.data && <div className="mt-4"><EmptyState title="No invitations yet" /></div>
      ) : (
        <div className="-mx-4 mt-4 border-t border-border">
          <Table>
            <thead><tr><th>Email</th><th>Role</th><th>Status</th><th>Expires</th><th /></tr></thead>
            <tbody>
              {invitations.map((i) => (
                <tr key={i.id}>
                  <td>{i.email}</td>
                  <td>{i.role}</td>
                  <td><StatusBadge status={i.status} /></td>
                  <td className="text-xs text-muted" title={i.expires_at}>{i.status === "open" ? formatRelative(i.expires_at) : "—"}</td>
                  <td className="text-right">
                    {i.status === "open" && (
                      <Button size="sm" variant="ghost" onClick={() => void api.revokeInvitation(i.id).then(list.reload, setError)}>Revoke</Button>
                    )}
                  </td>
                </tr>
              ))}
            </tbody>
          </Table>
        </div>
      )}
    </Card>
  );
}

export function Account() {
  const { principal, setPrincipal } = useAuth();
  const me = principal!;
  const [form, setForm] = useState({ current: "", next: "", confirm: "" });
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const [done, setDone] = useState(false);
  const [leaveError, setLeaveError] = useState<unknown>(null);

  async function change(e: FormEvent) {
    e.preventDefault();
    setError(null);
    setDone(false);
    if (form.next !== form.confirm) {
      setError(new Error("The new passwords do not match."));
      return;
    }
    setBusy(true);
    try {
      await api.changePassword(form.current, form.next);
      setForm({ current: "", next: "", confirm: "" });
      setDone(true);
    } catch (err) {
      setError(err);
    } finally {
      setBusy(false);
    }
  }

  async function leave() {
    if (!window.confirm(`Leave ${me.organization_name}? You lose access to its repositories, and your API tokens for it stop working.`)) return;
    setLeaveError(null);
    try {
      await api.removeMember(me.user_id);
      // The session belonged to that organization and has ended.
      setPrincipal(null);
    } catch (err) {
      setLeaveError(err);
    }
  }

  const set = (k: keyof typeof form) => (e: React.ChangeEvent<HTMLInputElement>) => setForm({ ...form, [k]: e.target.value });
  return (
    <div className="grid gap-6 xl:grid-cols-2">
      <Card title="Change password">
        {me.kind !== "session" ? (
          <p className="text-sm text-muted">Passwords can only be changed from a browser session.</p>
        ) : (
          <form onSubmit={change} className="max-w-sm space-y-3">
            <Field label="Current password"><Input type="password" autoComplete="current-password" required value={form.current} onChange={set("current")} /></Field>
            <Field label="New password" hint="At least 10 characters."><Input type="password" autoComplete="new-password" required minLength={10} value={form.next} onChange={set("next")} /></Field>
            <Field label="Repeat the new password"><Input type="password" autoComplete="new-password" required minLength={10} value={form.confirm} onChange={set("confirm")} /></Field>
            <ErrorBanner error={error} />
            {done && <p role="status" className="text-sm text-ok">Password changed. Your other sessions were signed out.</p>}
            <Button type="submit" variant="primary" busy={busy}>Change password</Button>
          </form>
        )}
      </Card>
      <Card title="Organization membership">
        <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-sm">
          <dt className="text-muted">Signed in as</dt><dd>{me.display_name} ({me.email})</dd>
          <dt className="text-muted">Organization</dt><dd>{me.organization_name}</dd>
          <dt className="text-muted">Role</dt><dd>{me.role}</dd>
        </dl>
        {me.kind === "session" && (
          <div className="mt-4 space-y-2">
            <ErrorBanner error={leaveError} />
            <Button variant="danger" onClick={() => void leave()}>Leave {me.organization_name}</Button>
          </div>
        )}
      </Card>
    </div>
  );
}
