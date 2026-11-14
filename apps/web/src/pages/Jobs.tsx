import { useState } from "react";
import { Link } from "react-router-dom";
import { Button, Card, EmptyState, ErrorBanner, Loading, Select, StatusBadge, Table, formatDuration, formatRelative } from "@usie/design-system";
import type { Job, JobStatus } from "@usie/sdk";
import { api } from "../api";
import { useCan } from "../auth";
import { useAsync, useInterval } from "../hooks";
import { PageHeader } from "../App";
import { useDocumentTitle } from "../components/shared";

function progressText(j: Job): string {
  const p = j.progress as { parsed?: number; to_parse?: number; files?: number; symbols?: number; relationships?: number };
  if (p.to_parse) return `${p.parsed ?? 0} / ${p.to_parse} files parsed`;
  if (j.status === "succeeded" && p.files !== undefined) return `${p.files} files, ${p.symbols ?? 0} symbols, ${p.relationships ?? 0} relationships`;
  return "";
}

export function Jobs() {
  useDocumentTitle("Analysis jobs");
  const [status, setStatus] = useState<JobStatus | "">("");
  const jobs = useAsync(() => api.listJobs({ status: status || undefined, limit: 100 }), [status]);
  const canCancel = useCan("member");
  const active = jobs.data?.some((j) => j.status === "queued" || j.status === "running") ?? false;
  useInterval(jobs.reload, 2000, active);

  return (
    <>
      <PageHeader
        title="Analysis jobs"
        subtitle="Each analysis runs as a job on a worker. Failed jobs are retried with backoff unless the failure is permanent."
        actions={
          <Select aria-label="Filter by status" value={status} onChange={(e) => setStatus(e.target.value as JobStatus | "")}>
            <option value="">All statuses</option>
            {["queued", "running", "succeeded", "failed", "cancelled"].map((s) => <option key={s}>{s}</option>)}
          </Select>
        }
      />
      <div className="p-6">
        <ErrorBanner error={jobs.error} onRetry={jobs.reload} />
        {!jobs.data ? !jobs.error && <Loading /> : jobs.data.length === 0 ? <Card><EmptyState title="No jobs" /></Card> : (
          <Card padded={false}>
            <Table>
              <thead>
                <tr><th>Repository</th><th>Status</th><th>Stage</th><th>Attempt</th><th>Duration</th><th>Created</th><th /></tr>
              </thead>
              <tbody>
                {jobs.data.map((j) => {
                  const end = j.finished_at ? Date.parse(j.finished_at) : Date.now();
                  const duration = j.started_at ? end - Date.parse(j.started_at) : NaN;
                  return (
                    <tr key={j.id}>
                      <td><Link className="text-accent hover:underline" to={`/repositories/${j.repository_id}?version=${j.version_id}`}>{j.repository_name}</Link></td>
                      <td>
                        <StatusBadge status={j.status} />
                        {j.cancel_requested && j.status === "running" && <span className="ml-1 text-xs text-warn">cancelling…</span>}
                      </td>
                      <td className="max-w-md text-xs">
                        <div>{j.stage || "—"}</div>
                        {progressText(j) && <div className="text-muted">{progressText(j)}</div>}
                        {j.error && <div className="text-danger" title={j.error}>{j.error}</div>}
                      </td>
                      <td className="tabular-nums text-xs">{j.attempts} / {j.max_attempts}</td>
                      <td className="text-xs">{formatDuration(duration)}</td>
                      <td className="text-xs text-muted">{formatRelative(j.created_at)}</td>
                      <td className="text-right">
                        {canCancel && (j.status === "queued" || (j.status === "running" && !j.cancel_requested)) && (
                          <Button size="sm" variant="ghost" onClick={() => void api.cancelJob(j.id).then(jobs.reload, () => jobs.reload())}>Cancel</Button>
                        )}
                      </td>
                    </tr>
                  );
                })}
              </tbody>
            </Table>
          </Card>
        )}
      </div>
    </>
  );
}
