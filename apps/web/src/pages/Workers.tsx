import { Badge, Card, EmptyState, ErrorBanner, Loading, StatusBadge, Table, formatRelative } from "@usie/design-system";
import { api } from "../api";
import { useAsync, useInterval } from "../hooks";
import { PageHeader } from "../App";
import { useDocumentTitle } from "../components/shared";

export function Workers() {
  useDocumentTitle("Workers");
  const workers = useAsync(() => api.listWorkers(), []);
  useInterval(workers.reload, 10000, true);
  return (
    <>
      <PageHeader title="Workers" subtitle="Analysis worker processes seen in the last 24 hours. A worker is online if it sent a heartbeat in the last 45 seconds." />
      <div className="p-6">
        <ErrorBanner error={workers.error} onRetry={workers.reload} />
        {!workers.data ? !workers.error && <Loading /> : workers.data.length === 0 ? (
          <Card><EmptyState title="No workers have connected">Start <code className="font-mono">usie-worker</code> to process analysis jobs.</EmptyState></Card>
        ) : (
          <Card padded={false}>
            <Table>
              <thead><tr><th>Host</th><th>State</th><th>Languages</th><th>Jobs</th><th>Version</th><th>Last heartbeat</th></tr></thead>
              <tbody>
                {workers.data.map((w) => {
                  const langs = (w.capabilities.languages as string[] | undefined) ?? [];
                  return (
                    <tr key={w.id}>
                      <td className="font-mono text-xs">{w.hostname} <span className="text-muted">pid {w.pid}</span></td>
                      <td>{w.online ? <StatusBadge status={w.status} /> : <StatusBadge status={w.status === "stopped" ? "stopped" : "offline"} />}</td>
                      <td className="space-x-1">{langs.map((l) => <Badge key={l}>{l}</Badge>)}</td>
                      <td className="tabular-nums text-xs">{w.jobs_completed} ok · {w.jobs_failed} failed</td>
                      <td className="text-xs">{w.version}</td>
                      <td className="text-xs text-muted">{formatRelative(w.last_heartbeat_at)}</td>
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
