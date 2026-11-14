import { Link } from "react-router-dom";
import { Card, EmptyState, ErrorBanner, Loading, Stat, StatusBadge, Table, formatRelative } from "@usie/design-system";
import { api } from "../api";
import { useAsync, useInterval } from "../hooks";
import { PageHeader } from "../App";
import { useDocumentTitle } from "../components/shared";

export function Overview() {
  useDocumentTitle("Overview");
  const data = useAsync(async () => {
    const [repos, jobs, workers] = await Promise.all([api.listRepositories(), api.listJobs({ limit: 10 }), api.listWorkers()]);
    return { repos, jobs, workers };
  }, []);
  const active = data.data?.jobs.some((j) => j.status === "queued" || j.status === "running") ?? false;
  useInterval(data.reload, 3000, active);

  if (!data.data) return data.error ? <div className="p-6"><ErrorBanner error={data.error} onRetry={data.reload} /></div> : <Loading />;
  const { repos, jobs, workers } = data.data;
  const ready = repos.filter((r) => r.latest_version?.status === "ready");
  const totals = ready.reduce(
    (acc, r) => {
      const s = r.latest_version!.stats;
      acc.files += s.files ?? 0;
      acc.symbols += s.symbols ?? 0;
      acc.edges += s.graph?.edges ?? 0;
      return acc;
    },
    { files: 0, symbols: 0, edges: 0 },
  );
  const online = workers.filter((w) => w.online).length;

  return (
    <>
      <PageHeader title="Overview" subtitle="Totals are taken from the latest analyzed version of each repository." />
      <div className="space-y-6 p-6">
        <div className="grid grid-cols-2 gap-3 lg:grid-cols-5">
          <Stat label="Repositories" value={repos.length} />
          <Stat label="Files analyzed" value={totals.files.toLocaleString()} />
          <Stat label="Symbols" value={totals.symbols.toLocaleString()} />
          <Stat label="Relationships" value={totals.edges.toLocaleString()} />
          <Stat label="Workers online" value={online} hint="Workers with a heartbeat in the last 45 seconds." />
        </div>
        {online === 0 && (
          <div role="status" className="rounded-md border border-warn/40 bg-warn/10 px-3 py-2 text-sm text-warn">
            No analysis worker is online. Queued analyses will wait until a <code className="font-mono">usie-worker</code> process is running.
          </div>
        )}
        <div className="grid gap-6 xl:grid-cols-2">
          <Card title="Repositories" actions={<Link className="text-sm text-accent hover:underline" to="/repositories">All repositories</Link>} padded={false}>
            {repos.length === 0 ? (
              <EmptyState title="No repositories yet" action={<Link className="text-sm text-accent hover:underline" to="/repositories">Add a repository</Link>}>
                Register a git repository, a local directory, or upload a ZIP archive to analyze it.
              </EmptyState>
            ) : (
              <Table>
                <tbody>
                  {repos.slice(0, 8).map((r) => (
                    <tr key={r.id}>
                      <td>
                        <Link className="font-medium text-accent hover:underline" to={`/repositories/${r.id}`}>{r.name}</Link>
                      </td>
                      <td>{r.latest_version ? <StatusBadge status={r.latest_version.status} /> : <span className="text-xs text-muted">not analyzed</span>}</td>
                      <td className="text-right text-xs text-muted">{formatRelative(r.latest_version?.analyzed_at ?? r.created_at)}</td>
                    </tr>
                  ))}
                </tbody>
              </Table>
            )}
          </Card>
          <Card title="Recent analysis jobs" actions={<Link className="text-sm text-accent hover:underline" to="/jobs">All jobs</Link>} padded={false}>
            {jobs.length === 0 ? (
              <EmptyState title="No jobs yet" />
            ) : (
              <Table>
                <tbody>
                  {jobs.map((j) => (
                    <tr key={j.id}>
                      <td>
                        <Link className="text-accent hover:underline" to={`/repositories/${j.repository_id}`}>{j.repository_name}</Link>
                      </td>
                      <td><StatusBadge status={j.status} /></td>
                      <td className="max-w-[16rem] truncate text-xs text-muted" title={j.error || j.stage}>{j.error || j.stage}</td>
                      <td className="text-right text-xs text-muted">{formatRelative(j.updated_at)}</td>
                    </tr>
                  ))}
                </tbody>
              </Table>
            )}
          </Card>
        </div>
      </div>
    </>
  );
}
