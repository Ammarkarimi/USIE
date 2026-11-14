import { type FormEvent, useState } from "react";
import { Link, useNavigate } from "react-router-dom";
import { Button, Card, EmptyState, ErrorBanner, Field, Input, Loading, Select, StatusBadge, Table, formatRelative } from "@usie/design-system";
import type { SourceKind } from "@usie/sdk";
import { api } from "../api";
import { useCan } from "../auth";
import { useAsync, useInterval } from "../hooks";
import { PageHeader } from "../App";
import { sourceLabel, useDocumentTitle } from "../components/shared";

export function Repositories() {
  useDocumentTitle("Repositories");
  const repos = useAsync(() => api.listRepositories(), []);
  const system = useAsync(() => api.system(), []);
  const canCreate = useCan("member");
  const [open, setOpen] = useState(false);
  const active = repos.data?.some((r) => r.latest_version && ["pending", "analyzing"].includes(r.latest_version.status)) ?? false;
  useInterval(repos.reload, 3000, active);

  return (
    <>
      <PageHeader
        title="Repositories"
        actions={canCreate && <Button variant="primary" onClick={() => setOpen((o) => !o)}>{open ? "Close" : "Add repository"}</Button>}
      />
      <div className="space-y-6 p-6">
        {open && system.data && <AddRepository localEnabled={system.data.sources.local} maxUpload={system.data.limits.max_upload_bytes ?? 0} onDone={() => { setOpen(false); repos.reload(); }} />}
        <ErrorBanner error={repos.error} onRetry={repos.reload} />
        {!repos.data ? (
          !repos.error && <Loading />
        ) : repos.data.length === 0 ? (
          <Card>
            <EmptyState title="No repositories">Add a repository to inventory, parse and map it.</EmptyState>
          </Card>
        ) : (
          <Card padded={false}>
            <Table>
              <thead>
                <tr>
                  <th>Name</th>
                  <th>Source</th>
                  <th>Latest version</th>
                  <th className="text-right">Files</th>
                  <th className="text-right">Symbols</th>
                  <th className="text-right">Updated</th>
                </tr>
              </thead>
              <tbody>
                {repos.data.map((r) => (
                  <tr key={r.id}>
                    <td><Link className="font-medium text-accent hover:underline" to={`/repositories/${r.id}`}>{r.name}</Link></td>
                    <td className="max-w-xs truncate text-xs text-muted" title={sourceLabel(r)}>{sourceLabel(r)}</td>
                    <td>{r.latest_version ? <StatusBadge status={r.latest_version.status} /> : <span className="text-xs text-muted">not analyzed</span>}</td>
                    <td className="text-right tabular-nums">{r.latest_version?.stats.files?.toLocaleString() ?? "—"}</td>
                    <td className="text-right tabular-nums">{r.latest_version?.stats.symbols?.toLocaleString() ?? "—"}</td>
                    <td className="text-right text-xs text-muted">{formatRelative(r.latest_version?.created_at ?? r.created_at)}</td>
                  </tr>
                ))}
              </tbody>
            </Table>
          </Card>
        )}
      </div>
    </>
  );
}

function AddRepository({ localEnabled, maxUpload, onDone }: { localEnabled: boolean; maxUpload: number; onDone: () => void }) {
  const navigate = useNavigate();
  const isAdmin = useCan("admin");
  const [kind, setKind] = useState<SourceKind>("git");
  const [name, setName] = useState("");
  const [location, setLocation] = useState("");
  const [ref, setRef] = useState("");
  const [file, setFile] = useState<File | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);

  async function submit(e: FormEvent) {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      if (kind === "upload" && file && maxUpload && file.size > maxUpload) throw new Error(`The archive exceeds the upload limit of ${Math.round(maxUpload / 1048576)} MB.`);
      const repo = await api.createRepository({
        name, source_kind: kind,
        source_location: kind === "upload" ? undefined : location.trim(),
        default_ref: kind === "git" && ref.trim() ? ref.trim() : undefined,
      });
      if (kind === "upload") {
        if (file) await api.uploadArchive(repo.id, file);
      } else {
        await api.analyzeRepository(repo.id);
      }
      onDone();
      navigate(`/repositories/${repo.id}`);
    } catch (err) {
      setError(err);
    } finally {
      setBusy(false);
    }
  }

  return (
    <Card title="Add repository">
      <form onSubmit={submit} className="grid max-w-2xl gap-4">
        <ErrorBanner error={error} />
        <div className="grid gap-4 sm:grid-cols-2">
          <Field label="Name">
            <Input required maxLength={200} value={name} onChange={(e) => setName(e.target.value)} placeholder="my-service" />
          </Field>
          <Field label="Source">
            <Select value={kind} onChange={(e) => setKind(e.target.value as SourceKind)} className="w-full">
              <option value="git">Git repository (HTTPS)</option>
              <option value="upload">ZIP archive upload</option>
              {localEnabled && isAdmin && <option value="local">Local directory on the server</option>}
            </Select>
          </Field>
        </div>
        {kind === "git" && (
          <div className="grid gap-4 sm:grid-cols-[1fr_12rem]">
            <Field label="Clone URL" hint="Public HTTPS URL without embedded credentials. Submodules, hooks and LFS content are not fetched.">
              <Input required type="url" value={location} onChange={(e) => setLocation(e.target.value)} placeholder="https://github.com/org/repo.git" />
            </Field>
            <Field label="Branch, tag or commit" hint="Optional; defaults to the remote's default branch.">
              <Input value={ref} onChange={(e) => setRef(e.target.value)} placeholder="main" />
            </Field>
          </div>
        )}
        {kind === "local" && (
          <Field label="Absolute path" hint="Must lie under one of the server's configured allowed roots.">
            <Input required value={location} onChange={(e) => setLocation(e.target.value)} placeholder="/srv/repos/my-service" />
          </Field>
        )}
        {kind === "upload" && (
          <Field label="ZIP archive" hint={`Optional now; you can upload later. Limit: ${Math.round(maxUpload / 1048576)} MB.`}>
            <input type="file" accept=".zip,application/zip" onChange={(e) => setFile(e.target.files?.[0] ?? null)} className="block text-sm" />
          </Field>
        )}
        <div>
          <Button type="submit" variant="primary" busy={busy}>
            {kind === "upload" && !file ? "Create repository" : "Create and analyze"}
          </Button>
        </div>
      </form>
    </Card>
  );
}
