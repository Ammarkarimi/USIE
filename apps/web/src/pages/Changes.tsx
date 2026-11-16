import { type FormEvent, useEffect, useState } from "react";
import { Link, useParams, useSearchParams } from "react-router-dom";
import { Badge, Button, Card, EmptyState, ErrorBanner, Field, Loading, Select, Spinner, StatusBadge, Table, cx, formatRelative } from "@usie/design-system";
import type { CodeChange, StepResult, TreeResult, VerificationProfile, VerificationRun } from "@usie/sdk";
import { keyLabel } from "@usie/sdk";
import { api } from "../api";
import { useCan } from "../auth";
import { useAsync, useInterval } from "../hooks";
import { PageHeader } from "../App";
import { RepoLinks, VersionPicker, useDocumentTitle, useRepoVersion } from "../components/shared";

/**
 * AI-proposed code changes. A proposal is a diff produced by applying the
 * model's edits to the analyzed sources; it is labelled unverified until a
 * sandboxed verification run passes. Nothing is written to the repository.
 */
export function ChangesPage() {
  const { repoId = "" } = useParams();
  const { repo, versions, version, select } = useRepoVersion(repoId);
  const [params, setParams] = useSearchParams();
  const changeId = params.get("change") ?? "";
  const focus = params.get("focus") ?? "";
  const status = useAsync(() => api.aiStatus(), []);
  const list = useAsync(() => api.listChanges({ repository_id: repoId, limit: 100 }), [repoId]);
  const canPropose = useCan("member");
  useDocumentTitle(`Changes · ${repo.data?.name ?? ""}`);
  const open = (id: string) => {
    const next = new URLSearchParams(params);
    next.set("change", id);
    setParams(next);
  };
  useInterval(() => list.reload(), 2000, !!list.data?.some((c) => c.status === "pending"));

  if (repo.error) return <div className="p-6"><ErrorBanner error={repo.error} /></div>;
  if (!repo.data || !versions.data || !status.data) return <Loading />;
  return (
    <div className="flex h-full flex-col">
      <PageHeader
        title={repo.data.name}
        subtitle="Code changes proposed by an AI model from the analysis. Every proposal is unverified until its build and tests pass in the sandbox; USIE never writes to your repository."
      />
      <div className="flex flex-wrap items-center justify-between gap-3 border-b border-border px-6 py-2">
        <RepoLinks repoId={repoId} versionId={version?.id} active="changes" />
        <VersionPicker versions={versions.data} value={version?.id} onChange={select} />
      </div>
      <div className="grid min-h-0 flex-1 grid-cols-1 lg:grid-cols-[22rem_minmax(0,1fr)]">
        <aside className="min-h-0 overflow-auto border-r border-border p-4">
          {!status.data.enabled ? (
            <EmptyState title="AI is not configured">Proposals need an AI provider. An operator sets OPENAI_API_KEY (or ANTHROPIC_API_KEY) on the API service.</EmptyState>
          ) : canPropose && version ? (
            <ProposeForm versionId={version.id} focus={focus} notice={status.data.data_notice} onCreated={(c) => { list.reload(); open(c.id); }} />
          ) : !version ? (
            <p className="text-sm text-muted">Analyze the repository first.</p>
          ) : (
            <p className="text-sm text-muted">Your role can view proposals but not request them.</p>
          )}
          <h2 className="mb-2 mt-6 text-xs font-semibold uppercase tracking-wide text-muted">Proposals</h2>
          {list.error != null && <ErrorBanner error={list.error} />}
          {list.data && list.data.length === 0 && <p className="text-sm text-muted">None yet.</p>}
          <ul className="space-y-1">
            {list.data?.map((c) => (
              <li key={c.id}>
                <button
                  type="button"
                  onClick={() => open(c.id)}
                  className={cx("w-full rounded-md px-3 py-2 text-left text-sm hover:bg-surface-2", c.id === changeId && "bg-accent/10")}
                  aria-current={c.id === changeId ? "true" : undefined}
                >
                  <div className="line-clamp-2">{c.request}</div>
                  <div className="mt-1 flex items-center gap-2 text-xs text-muted">
                    <StatusBadge status={c.status === "proposed" ? "unverified" : c.status} />
                    <span>{formatRelative(c.created_at)}</span>
                  </div>
                </button>
              </li>
            ))}
          </ul>
        </aside>
        <section className="min-h-0 overflow-auto p-6">
          {changeId ? (
            <ChangeDetail key={changeId} id={changeId} repoId={repoId} onChanged={() => list.reload()} />
          ) : (
            <EmptyState title="No proposal selected">Describe a change on the left, or open an earlier proposal.</EmptyState>
          )}
        </section>
      </div>
    </div>
  );
}

function ProposeForm({ versionId, focus, notice, onCreated }: { versionId: string; focus: string; notice: string; onCreated: (c: CodeChange) => void }) {
  const [request, setRequest] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      const c = await api.proposeChange({ version_id: versionId, request, focus_key: focus || undefined });
      setRequest("");
      onCreated(c);
    } catch (err) {
      setError(err);
    } finally {
      setBusy(false);
    }
  };
  return (
    <form onSubmit={submit} className="space-y-3">
      <Field label="Describe the change" hint={focus ? `About ${keyLabel(focus)}` : "Name the functions or files involved to give the model the right files."}>
        <textarea
          value={request}
          onChange={(e) => setRequest(e.target.value)}
          rows={5}
          maxLength={4000}
          className="w-full rounded-md border border-border bg-surface px-3 py-2 text-sm"
          placeholder="Validate that quantity is positive in Cart.add and raise ValueError otherwise."
        />
      </Field>
      {error != null && <ErrorBanner error={error} />}
      <Button type="submit" disabled={busy || request.trim() === ""}>{busy ? <Spinner label="Sending" /> : "Propose change"}</Button>
      <p className="text-xs text-muted">{notice}</p>
    </form>
  );
}

function ChangeDetail({ id, repoId, onChanged }: { id: string; repoId: string; onChanged: () => void }) {
  const data = useAsync(() => api.getChange(id), [id]);
  const canAct = useCan("member");
  const change = data.data?.change;
  const runs = data.data?.verifications ?? [];
  const active = change?.status === "pending" || runs.some((r) => r.status === "queued" || r.status === "running");
  useInterval(() => data.reload(), 2000, active);
  const wasPending = change?.status === "pending";
  useEffect(() => {
    if (!wasPending) onChanged();
  }, [wasPending]); // eslint-disable-line react-hooks/exhaustive-deps

  if (data.error) return <ErrorBanner error={data.error} />;
  if (!change) return <Loading />;
  const passed = runs.some((r) => r.status === "passed");
  return (
    <div className="mx-auto max-w-5xl space-y-6">
      <div>
        <div className="flex flex-wrap items-center gap-2">
          {change.status === "proposed" ? (
            passed ? <Badge tone="ok">verified: a sandbox run passed</Badge> : <Badge tone="warn">AI-proposed, unverified</Badge>
          ) : (
            <StatusBadge status={change.status} />
          )}
          <span className="text-xs text-muted">{change.model} · {formatRelative(change.created_at)}</span>
        </div>
        <h2 className="mt-2 text-lg font-semibold">{change.request}</h2>
      </div>
      {change.status === "pending" && (
        <Card><div className="flex items-center gap-3 text-sm"><Spinner /> The model is preparing the change.</div></Card>
      )}
      {change.error && <div role="alert" className="rounded-md border border-danger/40 bg-danger/10 px-4 py-2 text-sm">{change.error}</div>}
      {change.summary && (
        <Card title="Proposal">
          <p className="text-sm">{change.summary}</p>
          {change.rationale && <p className="mt-3 whitespace-pre-wrap text-sm text-muted">{change.rationale}</p>}
          <div className="mt-4 grid gap-4 md:grid-cols-3">
            <ListBlock title="Risks" items={change.risks} />
            <ListBlock title="Test plan" items={change.test_plan} />
            <ListBlock title="Limitations" items={change.limitations} />
          </div>
        </Card>
      )}
      {change.diff && (
        <Card
          title={`Diff · ${change.files_changed} file${change.files_changed === 1 ? "" : "s"}, +${change.lines_added} −${change.lines_removed}`}
          actions={
            <div className="flex gap-2">
              <a className="text-sm text-accent hover:underline" href={api.changePatchURL(change.id)} download>Download patch</a>
              {canAct && change.status === "proposed" && (
                <Button size="sm" variant="ghost" onClick={async () => { await api.discardChange(change.id); data.reload(); onChanged(); }}>Discard</Button>
              )}
            </div>
          }
          padded={false}
        >
          <DiffView diff={change.diff} />
        </Card>
      )}
      {change.impact?.risk && (
        <Card title="Impact of the change">
          <div className="flex flex-wrap items-center gap-4 text-sm">
            <span>Risk <strong className="tabular-nums">{change.impact.risk.score}</strong>/100</span>
            <Badge tone={change.impact.risk.level === "high" ? "danger" : change.impact.risk.level === "medium" ? "warn" : "ok"}>{change.impact.risk.level}</Badge>
            <span>{change.impact.affected?.filter((a) => !a.test).length ?? 0} dependents</span>
            <span>{change.impact.tests?.length ?? 0} tests reach it</span>
            <Link className="text-accent hover:underline" to={`/repositories/${repoId}/impact?version=${change.version_id}`}>Open impact analysis</Link>
          </div>
        </Card>
      )}
      {change.status === "proposed" && <Verification change={change} runs={runs} canAct={canAct} onStarted={() => data.reload()} />}
    </div>
  );
}

function ListBlock({ title, items }: { title: string; items: string[] }) {
  return (
    <div>
      <h3 className="text-xs font-semibold uppercase tracking-wide text-muted">{title}</h3>
      {items.length === 0 ? <p className="mt-1 text-sm text-muted">None given.</p> : (
        <ul className="mt-1 list-disc space-y-1 pl-5 text-sm">{items.map((i) => <li key={i}>{i}</li>)}</ul>
      )}
    </div>
  );
}

/** Renders a unified diff as text; repository content is never markup. */
export function DiffView({ diff }: { diff: string }) {
  const lines = diff.split("\n");
  if (lines[lines.length - 1] === "") lines.pop();
  return (
    <pre className="max-h-[32rem] overflow-auto text-xs leading-5" aria-label="Diff">
      {lines.map((l, i) => (
        <div
          key={i}
          className={cx(
            "px-4",
            l.startsWith("+++") || l.startsWith("---") ? "font-semibold" : l.startsWith("@@") ? "bg-surface-2 text-muted" : l.startsWith("+") ? "bg-ok/10" : l.startsWith("-") ? "bg-danger/10" : "",
          )}
        >
          {l || " "}
        </div>
      ))}
    </pre>
  );
}

function Verification({ change, runs, canAct, onStarted }: { change: CodeChange; runs: VerificationRun[]; canAct: boolean; onStarted: () => void }) {
  const profiles = useAsync(() => api.verificationProfiles(change.id), [change.id]);
  const [profile, setProfile] = useState("");
  const [baseline, setBaseline] = useState(true);
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const usable = (profiles.data?.profiles ?? []).filter((p) => p.available);
  const chosen = profile || usable.find((p) => p.detected)?.id || usable[0]?.id || "";
  const run = async () => {
    setBusy(true);
    setError(null);
    try {
      await api.verify(change.id, { profile: chosen, baseline });
      onStarted();
    } catch (e) {
      setError(e);
    } finally {
      setBusy(false);
    }
  };
  return (
    <Card title="Verification">
      {profiles.error != null && <ErrorBanner error={profiles.error} />}
      {profiles.data && !profiles.data.sandbox && (
        <p className="text-sm text-muted">No online worker has a verification sandbox. An operator enables it with USIE_SANDBOX=bubblewrap on the workers.</p>
      )}
      {profiles.data?.sandbox && (
        <div className="space-y-3">
          <p className="text-xs text-muted">
            Runs in a {profiles.data.sandbox.sandbox} sandbox on a worker: {profiles.data.sandbox.network ? "network access is ENABLED by the operator" : "no network access"}, no credentials, an unprivileged user, and time and memory limits. Commands are fixed per profile.
          </p>
          {canAct ? (
            <div className="flex flex-wrap items-end gap-3">
              <Field label="Profile">
                <Select value={chosen} onChange={(e) => setProfile(e.target.value)} aria-label="Profile">
                  {profiles.data.profiles.map((p: VerificationProfile) => (
                    <option key={p.id} value={p.id} disabled={!p.available}>
                      {p.label}{p.detected ? "" : " (not detected)"}{p.available ? "" : ` – ${p.reason}`}
                    </option>
                  ))}
                </Select>
              </Field>
              <label className="flex items-center gap-2 text-sm">
                <input type="checkbox" checked={baseline} onChange={(e) => setBaseline(e.target.checked)} />
                Also run without the change, to compare
              </label>
              <Button onClick={run} disabled={busy || !chosen}>{busy ? <Spinner label="Queuing" /> : "Run verification"}</Button>
            </div>
          ) : (
            <p className="text-sm text-muted">Your role cannot start verification runs.</p>
          )}
          {chosen && <p className="text-xs text-muted">{profiles.data.profiles.find((p) => p.id === chosen)?.description}</p>}
        </div>
      )}
      {error != null && <div className="mt-3"><ErrorBanner error={error} /></div>}
      {runs.length > 0 && (
        <div className="mt-6 space-y-4">
          {runs.map((r) => <RunView key={r.id} run={r} />)}
        </div>
      )}
    </Card>
  );
}

function RunView({ run }: { run: VerificationRun }) {
  const res = run.result;
  return (
    <div className="rounded-md border border-border">
      <div className="flex flex-wrap items-center gap-3 border-b border-border px-4 py-2 text-sm">
        <StatusBadge status={run.status} />
        <span className="font-medium">{run.profile}</span>
        <span className="text-muted">{formatRelative(run.created_at)}</span>
        {run.error && <span className="text-danger">{run.error}</span>}
      </div>
      {(res.baseline || res.change) && (
        <div className="space-y-3 p-4">
          {(res.newly_failing?.length || res.fixed?.length) ? (
            <div className="grid gap-3 md:grid-cols-2 text-sm">
              <div><span className="font-medium">Newly failing:</span> {res.newly_failing?.length ? res.newly_failing.join(", ") : "none"}</div>
              <div><span className="font-medium">Fixed by the change:</span> {res.fixed?.length ? res.fixed.join(", ") : "none"}</div>
            </div>
          ) : null}
          <Table>
            <thead><tr><th>Step</th>{res.baseline && <th>Without the change</th>}<th>With the change</th></tr></thead>
            <tbody>
              {stepNames(res.baseline, res.change).map((name) => (
                <tr key={name}>
                  <td className="font-mono text-xs">{name}</td>
                  {res.baseline && <td><StepCell step={res.baseline.steps.find((s) => s.name === name)} skipped={res.baseline.skipped?.includes(name)} /></td>}
                  <td><StepCell step={res.change?.steps.find((s) => s.name === name)} skipped={res.change?.skipped?.includes(name)} /></td>
                </tr>
              ))}
            </tbody>
          </Table>
          {res.notes?.map((n) => <p key={n} className="text-xs text-muted">{n}</p>)}
          {res.change?.steps.map((s) => (
            <details key={s.name} className="text-xs">
              <summary className="cursor-pointer text-muted">Output of {s.name} with the change ({s.argv.join(" ")})</summary>
              <pre className="mt-1 max-h-80 overflow-auto rounded bg-surface-2 p-2">{s.output || "(no output)"}</pre>
            </details>
          ))}
        </div>
      )}
    </div>
  );
}

function stepNames(a?: TreeResult | null, b?: TreeResult | null): string[] {
  const names: string[] = [];
  for (const t of [a, b]) for (const s of t?.steps ?? []) if (!names.includes(s.name)) names.push(s.name);
  for (const t of [a, b]) for (const s of t?.skipped ?? []) if (!names.includes(s)) names.push(s);
  return names;
}

function StepCell({ step, skipped }: { step?: StepResult; skipped?: boolean }) {
  if (skipped) return <span className="text-xs text-muted">skipped (earlier step failed)</span>;
  if (!step) return <span className="text-xs text-muted">—</span>;
  const ok = step.exit_code === 0;
  return (
    <div className="text-xs">
      <Badge tone={ok ? "ok" : "danger"}>{step.timed_out ? "timed out" : ok ? "passed" : `exit ${step.exit_code}`}</Badge>
      {step.tests && <span className="ml-2 tabular-nums">{step.tests.passed} passed, {step.tests.failed} failed{step.tests.skipped ? `, ${step.tests.skipped} skipped` : ""}</span>}
      <span className="ml-2 text-muted">{step.duration_seconds.toFixed(1)}s</span>
    </div>
  );
}
