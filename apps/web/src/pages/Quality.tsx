import { type FormEvent, useEffect, useMemo, useState } from "react";
import { Link, useParams, useSearchParams } from "react-router-dom";
import { Badge, Button, Card, EmptyState, ErrorBanner, Field, Input, Loading, Select, Stat, Table, type Tone, cx } from "@usie/design-system";
import type { Finding, FindingCategory, FindingQuery, FindingSeverity, GateCondition, GateMetric, QualityRule, QualitySummary } from "@usie/sdk";
import { api } from "../api";
import { PageHeader } from "../App";
import { useCan } from "../auth";
import { RepoLinks, VersionPicker, useDocumentTitle, useRepoVersion } from "../components/shared";
import { useAsync } from "../hooks";

const CATEGORIES: { id: FindingCategory; label: string; grade?: "reliability" | "security" | "security_review"; help: string }[] = [
  { id: "bug", label: "Bugs", grade: "reliability", help: "Code that is likely wrong." },
  { id: "vulnerability", label: "Vulnerabilities", grade: "security", help: "Code that is likely exploitable." },
  { id: "hotspot", label: "Security hotspots", grade: "security_review", help: "Security-sensitive code a person should review." },
  { id: "smell", label: "Code smells", help: "Code that is hard to understand or change." },
];
const SEVERITIES: FindingSeverity[] = ["blocker", "critical", "major", "minor", "info"];
const severityTone: Record<FindingSeverity, Tone> = { blocker: "danger", critical: "danger", major: "warn", minor: "info", info: "neutral" };
const categoryLabel: Record<FindingCategory, string> = { bug: "Bug", vulnerability: "Vulnerability", hotspot: "Hotspot", smell: "Code smell" };
const statusLabel: Record<string, string> = { open: "Open", accepted: "Accepted", false_positive: "False positive", safe: "Reviewed: safe" };

const gradeClass: Record<string, string> = {
  A: "bg-ok/15 text-ok border-ok/40",
  B: "bg-ok/10 text-ok border-ok/30",
  C: "bg-warn/15 text-warn border-warn/40",
  D: "bg-danger/10 text-danger border-danger/30",
  E: "bg-danger/15 text-danger border-danger/40",
};

const gradeHelp: Record<string, string> = {
  reliability: "A: no open bugs. B: minor or info only. C: at least one major. D: at least one critical. E: at least one blocker.",
  security: "A: no open vulnerabilities. B: minor or info only. C: at least one major. D: at least one critical. E: at least one blocker.",
  security_review: "Share of security hotspots reviewed. A: at least 80%. B: 70%. C: 50%. D: 30%. E: less.",
};

function Grade({ value, kind }: { value: string; kind: string }) {
  return (
    <span title={gradeHelp[kind]} className={cx("inline-flex h-8 w-8 items-center justify-center rounded-md border text-base font-semibold", gradeClass[value])}>
      {value}
    </span>
  );
}

function metricLabel(m: string, metrics: GateMetric[] | undefined): string {
  return metrics?.find((x) => x.metric === m)?.description ?? m;
}

/** Bugs, vulnerabilities, hotspots and smells of a version, with reviews and the quality gate. */
export function QualityPage() {
  const { repoId = "" } = useParams();
  const { repo, versions, version, select } = useRepoVersion(repoId);
  const [params, setParams] = useSearchParams();
  const summary = useAsync(() => (version ? api.quality(version.id) : Promise.resolve(undefined)), [version?.id]);
  const catalog = useAsync(() => api.qualityRules(), []);
  useDocumentTitle(`Quality · ${repo.data?.name ?? ""}`);

  const filter: FindingQuery = {
    category: (params.get("category") as FindingCategory) || undefined,
    severity: (params.get("severity") as FindingSeverity) || undefined,
    rule: params.get("rule") || undefined,
    status: (params.get("status") as FindingQuery["status"]) || "open",
    new: params.get("new") === "true",
    path: params.get("path") || undefined,
  };
  const setFilter = (patch: Record<string, string | undefined>) => {
    const next = new URLSearchParams(params);
    for (const [k, v] of Object.entries(patch)) {
      if (v) next.set(k, v);
      else next.delete(k);
    }
    setParams(next, { replace: true });
  };

  if (repo.error) return <div className="p-6"><ErrorBanner error={repo.error} /></div>;
  if (!repo.data || !versions.data) return <Loading />;
  const s = summary.data;

  return (
    <div className="flex h-full flex-col">
      <PageHeader
        title={repo.data.name}
        subtitle="Bugs, vulnerabilities, security hotspots and maintainability issues found by rules. Rules match code patterns one file at a time, without types or runtime values: review a finding before acting on it."
      />
      <div className="flex flex-wrap items-center justify-between gap-3 border-b border-border px-6 py-2">
        <RepoLinks repoId={repoId} versionId={version?.id} active="quality" />
        <VersionPicker versions={versions.data} value={version?.id} onChange={select} />
      </div>
      <div className="min-h-0 flex-1 overflow-auto p-6">
        {!version ? (
          <EmptyState title="No analyzed version">Analyze the repository first; quality checks run as part of the analysis.</EmptyState>
        ) : summary.error ? (
          <ErrorBanner error={summary.error} onRetry={summary.reload} />
        ) : !s ? (
          <Loading />
        ) : !s.available ? (
          <EmptyState title="No quality results for this version" action={<Link className="text-accent hover:underline" to={`/repositories/${repoId}`}>Go to the repository to analyze it again</Link>}>
            {s.reason}
          </EmptyState>
        ) : (
          <div className="mx-auto max-w-7xl space-y-6">
            <div className="grid gap-6 xl:grid-cols-[2fr_3fr]">
              <GateCard repoId={repoId} summary={s} metrics={catalog.data?.metrics} onChanged={summary.reload} />
              <div className="grid gap-3 sm:grid-cols-2">
                {CATEGORIES.map((c) => (
                  <CategoryCard key={c.id} category={c} summary={s} baseline={!!s.baseline_version_id}
                    active={filter.category === c.id}
                    onSelect={() => setFilter({ category: filter.category === c.id ? undefined : c.id, severity: undefined, rule: undefined })} />
                ))}
              </div>
            </div>
            <Measures summary={s} />
            <FindingList key={version.id} repoId={repoId} versionId={version.id} filter={filter} setFilter={setFilter} summary={s}
              rules={catalog.data?.rules} onReviewed={summary.reload} />
          </div>
        )}
      </div>
    </div>
  );
}

function CategoryCard({ category, summary, baseline, active, onSelect }: {
  category: (typeof CATEGORIES)[number]; summary: QualitySummary; baseline: boolean; active: boolean; onSelect: () => void;
}) {
  const c = summary.categories![category.id];
  const grade = category.grade ? summary.grades![category.grade] : undefined;
  return (
    <button type="button" onClick={onSelect} aria-pressed={active}
      className={cx("rounded-lg border bg-surface px-4 py-3 text-left transition-colors hover:border-accent/60", active ? "border-accent ring-2 ring-accent/20" : "border-border")}>
      <div className="flex items-start justify-between gap-2">
        <div>
          <div className="text-xs font-medium uppercase tracking-wide text-muted">{category.label}</div>
          <div className="mt-1 text-2xl font-semibold tabular-nums">{c.open}</div>
        </div>
        {grade && <Grade value={grade} kind={category.grade!} />}
      </div>
      <div className="mt-1 flex flex-wrap gap-x-3 text-xs text-muted">
        {baseline && <span><span className={cx("tabular-nums", c.new > 0 && "font-medium text-danger")}>{c.new}</span> new</span>}
        <span><span className="tabular-nums">{c.reviewed}</span> reviewed</span>
        {category.id === "hotspot" && summary.hotspots_reviewed_percent != null && <span>{summary.hotspots_reviewed_percent}% reviewed</span>}
      </div>
      <p className="mt-1 text-xs text-muted">{category.help}</p>
    </button>
  );
}

function Measures({ summary }: { summary: QualitySummary }) {
  const m = summary.metrics!;
  return (
    <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-4">
      <Stat label="Functions" value={m.functions.toLocaleString()} hint="Named functions, methods and constructors in non-test files of parsed languages." />
      <Stat label="Cyclomatic complexity" value={m.complexity.toLocaleString()} hint={`Sum over all functions. The most complex function scores ${m.max_complexity}.`} />
      <Stat label="Most complex function" value={m.max_complexity} hint="Functions above 15 are reported as code smells." />
      <Stat
        label="Duplicated lines"
        value={m.duplication_skipped ? "not measured" : m.duplicated_lines_percent == null ? "—" : `${m.duplicated_lines_percent}%`}
        hint={m.duplication_skipped
          ? "The repository was too large for the duplication detector's memory budget."
          : `${m.duplicated_lines.toLocaleString()} of ${m.duplication_compared_lines.toLocaleString()} compared code lines, in ${m.duplicated_blocks} blocks of 10 or more identical lines. Test files are not compared.`}
      />
    </div>
  );
}

function GateCard({ repoId, summary, metrics, onChanged }: { repoId: string; summary: QualitySummary; metrics?: GateMetric[]; onChanged: () => void }) {
  const gate = summary.gate!;
  const isAdmin = useCan("admin");
  const [editing, setEditing] = useState(false);
  const passed = gate.status === "passed";
  return (
    <Card
      title="Quality gate"
      actions={isAdmin && !editing ? <Button size="sm" variant="ghost" onClick={() => setEditing(true)}>Edit conditions</Button> : undefined}
    >
      <div className="flex items-center gap-3">
        <span className={cx("rounded-md border px-3 py-1 text-lg font-semibold", passed ? "border-ok/40 bg-ok/10 text-ok" : "border-danger/40 bg-danger/10 text-danger")}>
          {passed ? "Passed" : "Failed"}
        </span>
        <span className="text-xs text-muted">
          {gate.default ? "Default gate." : "Custom gate for this repository."}{" "}
          {summary.baseline_version_id ? "New findings are compared with the previous analyzed version." : "First analysis with quality checks: there is no previous version to compare with, so conditions on new findings are not evaluated yet."}
        </span>
      </div>
      {editing ? (
        <GateEditor repoId={repoId} initial={gate.conditions} isDefault={gate.default} metrics={metrics ?? []}
          onDone={(changed) => { setEditing(false); if (changed) onChanged(); }} />
      ) : (
        <ul className="mt-3 space-y-1.5 text-sm">
          {gate.conditions.map((c) => (
            <li key={c.metric} className="flex items-center justify-between gap-3">
              <span className="text-muted">
                {metricLabel(c.metric, metrics)} <span className="whitespace-nowrap">must be ≤ {c.threshold}</span>
              </span>
              <span className="flex items-center gap-2 whitespace-nowrap">
                <span className="tabular-nums">{c.actual ?? "—"}</span>
                <Badge tone={c.status === "failed" ? "danger" : c.status === "passed" ? "ok" : "neutral"}>
                  {c.status === "no_baseline" ? "no baseline" : c.status}
                </Badge>
              </span>
            </li>
          ))}
          {gate.conditions.length === 0 && <li className="text-muted">No conditions: the gate always passes.</li>}
        </ul>
      )}
    </Card>
  );
}

function GateEditor({ repoId, initial, isDefault, metrics, onDone }: {
  repoId: string; initial: GateCondition[]; isDefault: boolean; metrics: GateMetric[]; onDone: (changed: boolean) => void;
}) {
  const [conds, setConds] = useState<GateCondition[]>(initial.map((c) => ({ metric: c.metric, threshold: c.threshold })));
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const unused = metrics.filter((m) => !conds.some((c) => c.metric === m.metric));
  const save = async (body: { conditions: GateCondition[] } | { default: true }) => {
    setBusy(true);
    setError(null);
    try {
      await api.setQualityGate(repoId, body);
      onDone(true);
    } catch (err) {
      setError(err);
    } finally {
      setBusy(false);
    }
  };
  return (
    <form className="mt-3 space-y-3" onSubmit={(e: FormEvent) => { e.preventDefault(); void save({ conditions: conds }); }}>
      <ErrorBanner error={error} />
      {conds.map((c, i) => (
        <div key={i} className="flex items-end gap-2">
          <Field label={i === 0 ? "Metric" : ""}>
            <Select aria-label={`Metric ${i + 1}`} value={c.metric} className="w-full"
              onChange={(e) => setConds(conds.map((x, j) => (j === i ? { ...x, metric: e.target.value } : x)))}>
              {[...metrics.filter((m) => m.metric === c.metric), ...unused].map((m) => <option key={m.metric} value={m.metric}>{m.description}</option>)}
            </Select>
          </Field>
          <Field label={i === 0 ? "At most" : ""}>
            <Input aria-label={`Threshold ${i + 1}`} type="number" min={0} step="any" className="w-24" value={c.threshold}
              onChange={(e) => setConds(conds.map((x, j) => (j === i ? { ...x, threshold: Number(e.target.value) } : x)))} />
          </Field>
          <Button size="sm" variant="ghost" aria-label={`Remove condition ${i + 1}`} onClick={() => setConds(conds.filter((_, j) => j !== i))}>Remove</Button>
        </div>
      ))}
      <div className="flex flex-wrap gap-2">
        {unused.length > 0 && <Button size="sm" onClick={() => setConds([...conds, { metric: unused[0]!.metric, threshold: 0 }])}>Add condition</Button>}
        <Button size="sm" variant="primary" type="submit" busy={busy}>Save</Button>
        {!isDefault && <Button size="sm" onClick={() => void save({ default: true })} disabled={busy}>Use the default gate</Button>}
        <Button size="sm" variant="ghost" onClick={() => onDone(false)}>Cancel</Button>
      </div>
    </form>
  );
}

const PAGE = 50;

function FindingList({ repoId, versionId, filter, setFilter, summary, rules, onReviewed }: {
  repoId: string; versionId: string; filter: FindingQuery; setFilter: (p: Record<string, string | undefined>) => void;
  summary: QualitySummary; rules?: QualityRule[]; onReviewed: () => void;
}) {
  const [limit, setLimit] = useState(PAGE);
  const key = JSON.stringify(filter);
  useEffect(() => setLimit(PAGE), [key]);
  const list = useAsync(() => api.findings(versionId, { ...filter, limit }), [versionId, key, limit]);
  const ruleIndex = useMemo(() => new Map((rules ?? []).map((r) => [r.id, r])), [rules]);
  const [open, setOpen] = useState<string | null>(null);
  const baseline = !!summary.baseline_version_id;
  const reviewed = () => {
    list.reload();
    onReviewed();
  };

  return (
    <Card title={`Findings${list.data ? ` (${list.data.total.toLocaleString()})` : ""}`} padded={false}>
      <div className="flex flex-wrap items-end gap-3 border-b border-border p-3">
        <Field label="Category">
          <Select aria-label="Category" value={filter.category ?? ""} onChange={(e) => setFilter({ category: e.target.value || undefined })}>
            <option value="">All</option>
            {CATEGORIES.map((c) => <option key={c.id} value={c.id}>{c.label}</option>)}
          </Select>
        </Field>
        <Field label="Severity">
          <Select aria-label="Severity" value={filter.severity ?? ""} onChange={(e) => setFilter({ severity: e.target.value || undefined })}>
            <option value="">All</option>
            {SEVERITIES.map((s) => <option key={s} value={s}>{s}</option>)}
          </Select>
        </Field>
        <Field label="Rule">
          <Select aria-label="Rule" value={filter.rule ?? ""} onChange={(e) => setFilter({ rule: e.target.value || undefined })} className="max-w-xs">
            <option value="">All</option>
            {(summary.rules ?? []).map((r) => <option key={r.rule} value={r.rule}>{r.name || r.rule} ({r.count})</option>)}
          </Select>
        </Field>
        <Field label="Status">
          <Select aria-label="Status" value={filter.status} onChange={(e) => setFilter({ status: e.target.value === "open" ? undefined : e.target.value })}>
            <option value="open">Open</option>
            <option value="reviewed">Reviewed</option>
            <option value="all">All</option>
          </Select>
        </Field>
        <Field label="Path">
          <Input aria-label="Path" placeholder="src/ or src/app.py" defaultValue={filter.path ?? ""} className="w-56"
            onKeyDown={(e) => { if (e.key === "Enter") setFilter({ path: (e.target as HTMLInputElement).value.trim() || undefined }); }} />
        </Field>
        {baseline && (
          <label className="flex h-9 items-center gap-2 text-sm">
            <input type="checkbox" checked={!!filter.new} onChange={(e) => setFilter({ new: e.target.checked ? "true" : undefined })} />
            New only
          </label>
        )}
      </div>
      <div className="p-3 empty:hidden"><ErrorBanner error={list.error} onRetry={list.reload} /></div>
      {!list.data ? (
        <Loading />
      ) : list.data.findings.length === 0 ? (
        <EmptyState title="No findings match">Absence of findings means none of the rules matched, not that the code is free of defects.</EmptyState>
      ) : (
        <>
          <Table>
            <thead>
              <tr><th>Severity</th><th>Finding</th><th>Location</th><th>Status</th><th /></tr>
            </thead>
            <tbody>
              {list.data.findings.map((f) => (
                <FindingRow key={f.seq} f={f} repoId={repoId} versionId={versionId} rule={ruleIndex.get(f.rule)} baseline={baseline}
                  expanded={open === f.fingerprint + f.seq} onToggle={() => setOpen(open === f.fingerprint + f.seq ? null : f.fingerprint + f.seq)}
                  onReviewed={reviewed} />
              ))}
            </tbody>
          </Table>
          {list.data.total > list.data.findings.length && (
            <div className="border-t border-border p-3 text-center">
              <Button size="sm" onClick={() => setLimit(limit + PAGE)} busy={list.loading}>
                Show more ({(list.data.total - list.data.findings.length).toLocaleString()} left)
              </Button>
            </div>
          )}
        </>
      )}
    </Card>
  );
}

function FindingRow({ f, repoId, versionId, rule, baseline, expanded, onToggle, onReviewed }: {
  f: Finding; repoId: string; versionId: string; rule?: QualityRule; baseline: boolean; expanded: boolean; onToggle: () => void; onReviewed: () => void;
}) {
  const canReview = useCan("member");
  const [form, setForm] = useState<{ status: "accepted" | "false_positive" | "safe"; comment: string } | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const run = async (fn: () => Promise<unknown>) => {
    setBusy(true);
    setError(null);
    try {
      await fn();
      setForm(null);
      onReviewed();
    } catch (err) {
      setError(err);
    } finally {
      setBusy(false);
    }
  };
  const hotspot = f.category === "hotspot";
  const choices: { status: "accepted" | "false_positive" | "safe"; label: string }[] = hotspot
    ? [{ status: "safe", label: "Mark safe" }, { status: "accepted", label: "Accept risk" }]
    : [{ status: "accepted", label: "Accept" }, { status: "false_positive", label: "False positive" }];
  const lines = f.end_line > f.start_line ? `${f.start_line}–${f.end_line}` : `${f.start_line}`;

  return (
    <>
      <tr className="align-top">
        <td className="whitespace-nowrap">
          <Badge tone={severityTone[f.severity]}>{f.severity}</Badge>
          <div className="mt-1 text-xs text-muted">{categoryLabel[f.category]}</div>
        </td>
        <td className="max-w-xl">
          <button type="button" onClick={onToggle} aria-expanded={expanded} className="text-left hover:underline">{f.message}</button>
          <div className="mt-0.5 flex flex-wrap items-center gap-1.5 text-xs text-muted">
            <span>{rule?.name ?? f.rule}</span>
            {baseline && f.is_new && <Badge tone="accent">new</Badge>}
            {f.in_test && <Badge>test code</Badge>}
            {f.function && <span>in <span className="font-mono">{f.function}</span></span>}
          </div>
        </td>
        <td className="whitespace-nowrap text-xs">
          <Link className="font-mono text-accent hover:underline" to={`/repositories/${repoId}/code?version=${versionId}&path=${encodeURIComponent(f.path)}&line=${f.start_line}`}>
            {f.path}:{lines}
          </Link>
        </td>
        <td className="whitespace-nowrap text-xs">
          {f.review ? (
            <span title={f.review.comment}>
              {statusLabel[f.review.status]}
              <span className="block text-muted">by {f.review.reviewed_by || "a former member"}</span>
            </span>
          ) : <span className="text-muted">Open</span>}
        </td>
        <td className="whitespace-nowrap text-right">
          {canReview && (f.review ? (
            <Button size="sm" variant="ghost" busy={busy} onClick={() => void run(() => api.reopenFinding(repoId, f.fingerprint))}>Reopen</Button>
          ) : (
            choices.map((c) => (
              <Button key={c.status} size="sm" variant="ghost" onClick={() => setForm({ status: c.status, comment: "" })}>{c.label}</Button>
            ))
          ))}
        </td>
      </tr>
      {(expanded || form || !!error) && (
        <tr>
          <td />
          <td colSpan={4} className="bg-surface-2/40">
            <div className="space-y-2 text-sm">
              <ErrorBanner error={error} />
              {expanded && (rule ? (
                <div className="space-y-1">
                  <p><span className="font-medium">What the rule matches: </span><span className="text-muted">{rule.description}</span></p>
                  <p><span className="font-medium">How to fix: </span><span className="text-muted">{rule.remediation}</span></p>
                </div>
              ) : <p className="text-muted">Rule {f.rule} is not described by this server.</p>)}
              {expanded && f.review?.comment && <p><span className="font-medium">Review comment: </span><span className="text-muted">{f.review.comment}</span></p>}
              {form && (
                <form className="flex flex-wrap items-end gap-2" onSubmit={(e) => { e.preventDefault(); void run(() => api.reviewFinding(repoId, f.fingerprint, form.status, form.comment.trim())); }}>
                  <Field label={form.status === "false_positive" ? "Why is this a false positive? (required)" : "Comment (optional)"}>
                    <Input aria-label="Review comment" autoFocus maxLength={2000} className="w-96" value={form.comment} required={form.status === "false_positive"}
                      onChange={(e) => setForm({ ...form, comment: e.target.value })} />
                  </Field>
                  <Button size="sm" variant="primary" type="submit" busy={busy}>{statusLabel[form.status]}</Button>
                  <Button size="sm" variant="ghost" onClick={() => setForm(null)}>Cancel</Button>
                  <span className="text-xs text-muted">Applies to this finding in every version of the repository.</span>
                </form>
              )}
            </div>
          </td>
        </tr>
      )}
    </>
  );
}

export default QualityPage;
