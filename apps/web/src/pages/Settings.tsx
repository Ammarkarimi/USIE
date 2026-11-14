import { type FormEvent, useState } from "react";
import { Badge, Button, Card, EmptyState, ErrorBanner, Field, Input, Loading, Select, StatusBadge, Table, Tabs, formatBytes, formatRelative } from "@usie/design-system";
import type { AuditEvent } from "@usie/sdk";
import { api } from "../api";
import { useAuth, useCan } from "../auth";
import { useAsync } from "../hooks";
import { PageHeader } from "../App";
import { useDocumentTitle } from "../components/shared";

type Tab = "capabilities" | "tokens" | "audit";

export function Settings() {
  useDocumentTitle("Settings");
  const isAdmin = useCan("admin");
  const [tab, setTab] = useState<Tab>("capabilities");
  return (
    <>
      <PageHeader title="Settings" />
      <div className="space-y-6 p-6">
        <Tabs<Tab>
          value={tab}
          onChange={setTab}
          items={[
            { value: "capabilities", label: "Capabilities" },
            { value: "tokens", label: "API tokens" },
            ...(isAdmin ? [{ value: "audit" as const, label: "Audit log" }] : []),
          ]}
        />
        {tab === "capabilities" && <Capabilities />}
        {tab === "tokens" && <Tokens />}
        {tab === "audit" && isAdmin && <Audit />}
      </div>
    </>
  );
}

function Capabilities() {
  const system = useAsync(() => api.system(), []);
  if (system.error) return <ErrorBanner error={system.error} onRetry={system.reload} />;
  if (!system.data) return <Loading />;
  const s = system.data;
  return (
    <div className="grid gap-6 xl:grid-cols-2">
      <Card title="What this installation can do" padded={false}>
        <Table>
          <tbody>
            {s.features.map((f) => (
              <tr key={f.id}>
                <td className="whitespace-nowrap font-medium">{f.id.replace(/-/g, " ")}</td>
                <td><StatusBadge status={f.status} /></td>
                <td className="text-xs text-muted">{f.detail}</td>
              </tr>
            ))}
          </tbody>
        </Table>
      </Card>
      <div className="space-y-6">
        <Card title="Installation">
          <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-sm">
            <dt className="text-muted">Version</dt><dd>{s.version}</dd>
            <dt className="text-muted">Sources</dt>
            <dd className="space-x-1">{Object.entries(s.sources).map(([k, on]) => <Badge key={k} tone={on ? "ok" : "neutral"}>{k}{on ? "" : " (disabled)"}</Badge>)}</dd>
            <dt className="text-muted">Upload limit</dt><dd>{formatBytes(s.limits.max_upload_bytes ?? 0)}</dd>
            <dt className="text-muted">Per-file limit</dt><dd>{formatBytes(s.limits.max_file_bytes ?? 0)}</dd>
            <dt className="text-muted">Repository limit</dt><dd>{(s.limits.max_repository_files ?? 0).toLocaleString()} files, {formatBytes(s.limits.max_repository_bytes ?? 0)}</dd>
          </dl>
        </Card>
        <Card title="Analysis engine">
          {s.engine ? (
            <Table>
              <tbody>
                {s.engine.languages.map((l) => (
                  <tr key={l.language}><td>{l.language}</td><td className="font-mono text-xs text-muted">{l.fingerprint}</td></tr>
                ))}
              </tbody>
            </Table>
          ) : (
            <p className="text-sm text-muted">The engine binary is not available to the API process. Workers report their own engine on the Workers page.</p>
          )}
        </Card>
      </div>
    </div>
  );
}

function Tokens() {
  const { principal } = useAuth();
  const tokens = useAsync(() => api.listTokens(), []);
  const [name, setName] = useState("");
  const [role, setRole] = useState<"viewer" | "member" | "admin">("viewer");
  const [days, setDays] = useState(90);
  const [secret, setSecret] = useState<string | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const roles = (["viewer", "member", "admin"] as const).filter((r) => ({ viewer: 1, member: 2, admin: 3 })[r] <= ({ viewer: 1, member: 2, admin: 3, owner: 4 })[principal!.role]);

  async function create(e: FormEvent) {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      const out = await api.createToken({ name, role, expires_in_days: days });
      setSecret(out.secret);
      setName("");
      tokens.reload();
    } catch (err) {
      setError(err);
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="space-y-6">
      <Card title="Create an API token">
        <form onSubmit={create} className="flex flex-wrap items-end gap-3">
          <Field label="Name"><Input required maxLength={100} value={name} onChange={(e) => setName(e.target.value)} placeholder="ci-pipeline" className="w-56" /></Field>
          <Field label="Role">
            <Select value={role} onChange={(e) => setRole(e.target.value as typeof role)}>{roles.map((r) => <option key={r}>{r}</option>)}</Select>
          </Field>
          <Field label="Expires after (days)"><Input type="number" min={0} max={366} value={days} onChange={(e) => setDays(Number(e.target.value))} className="w-28" /></Field>
          <Button type="submit" variant="primary" busy={busy}>Create token</Button>
        </form>
        <p className="mt-2 text-xs text-muted">Tokens act as you, with at most the chosen role. 0 days means no expiry. Use them as <code className="font-mono">Authorization: Bearer usie_…</code>.</p>
        <div className="mt-3"><ErrorBanner error={error} /></div>
        {secret && (
          <div role="status" className="mt-3 rounded-md border border-ok/40 bg-ok/10 p-3 text-sm">
            <p className="font-medium text-ok">Copy this token now. It will not be shown again.</p>
            <code className="mt-1 block break-all rounded bg-surface p-2 font-mono text-xs">{secret}</code>
            <Button size="sm" className="mt-2" onClick={() => void navigator.clipboard?.writeText(secret)}>Copy</Button>
          </div>
        )}
      </Card>
      <Card title="Your tokens" padded={false}>
        <ErrorBanner error={tokens.error} />
        {!tokens.data ? <Loading /> : tokens.data.length === 0 ? <EmptyState title="No tokens" /> : (
          <Table>
            <thead><tr><th>Name</th><th>Prefix</th><th>Role</th><th>Last used</th><th>Expires</th><th /></tr></thead>
            <tbody>
              {tokens.data.map((t) => (
                <tr key={t.id}>
                  <td>{t.name}</td>
                  <td className="font-mono text-xs">{t.prefix}…</td>
                  <td>{t.role}</td>
                  <td className="text-xs text-muted">{formatRelative(t.last_used_at)}</td>
                  <td className="text-xs text-muted">{t.expires_at ? formatRelative(t.expires_at) : "never"}</td>
                  <td className="text-right">
                    {t.revoked_at ? <Badge>revoked</Badge> : <Button size="sm" variant="ghost" onClick={() => void api.revokeToken(t.id).then(tokens.reload, setError)}>Revoke</Button>}
                  </td>
                </tr>
              ))}
            </tbody>
          </Table>
        )}
      </Card>
    </div>
  );
}

function Audit() {
  const [pages, setPages] = useState<AuditEvent[][]>([]);
  const first = useAsync(() => api.listAudit({ limit: 100 }), []);
  const [error, setError] = useState<unknown>(null);
  const events = [...(first.data ?? []), ...pages.flat()];
  const more = async () => {
    const last = events[events.length - 1];
    if (!last) return;
    try {
      const next = await api.listAudit({ before: last.id, limit: 100 });
      setPages((p) => [...p, next]);
    } catch (e) {
      setError(e);
    }
  };
  if (first.error) return <ErrorBanner error={first.error} onRetry={first.reload} />;
  if (!first.data) return <Loading />;
  return (
    <Card padded={false} title="Audit log" actions={<span className="text-xs text-muted">Security-relevant actions in this organization, newest first.</span>}>
      <ErrorBanner error={error} />
      <Table>
        <thead><tr><th>When</th><th>Actor</th><th>Action</th><th>Target</th><th>Outcome</th><th>IP</th></tr></thead>
        <tbody>
          {events.map((e) => (
            <tr key={e.id}>
              <td className="whitespace-nowrap text-xs text-muted" title={e.created_at}>{formatRelative(e.created_at)}</td>
              <td className="text-xs">{e.actor_email || "—"} <span className="text-muted">({e.actor_kind})</span></td>
              <td className="font-mono text-xs">{e.action}</td>
              <td className="max-w-[16rem] truncate font-mono text-xs text-muted" title={`${e.target_type} ${e.target_id}`}>{e.target_type} {e.target_id}</td>
              <td>{e.outcome === "success" ? <Badge tone="ok">success</Badge> : <Badge tone="danger">{e.outcome}</Badge>}</td>
              <td className="font-mono text-xs text-muted">{e.ip}</td>
            </tr>
          ))}
        </tbody>
      </Table>
      {events.length > 0 && events.length % 100 === 0 && <div className="p-3"><Button size="sm" onClick={() => void more()}>Load older events</Button></div>}
    </Card>
  );
}
