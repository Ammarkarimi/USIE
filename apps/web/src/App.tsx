import { Suspense, lazy, useState } from "react";
import { NavLink, Navigate, Route, Routes, useLocation } from "react-router-dom";
import { Button, Loading, Select, cx } from "@usie/design-system";
import { useAuth } from "./auth";
import { type Theme, useTheme } from "./hooks";
import { Login } from "./pages/Login";
import { Overview } from "./pages/Overview";
import { Repositories } from "./pages/Repositories";
import { RepositoryPage } from "./pages/Repository";
import { Jobs } from "./pages/Jobs";
import { Workers } from "./pages/Workers";
import { Settings } from "./pages/Settings";
import { AskPage } from "./pages/Ask";
import { ImpactPage } from "./pages/Impact";
import { QualityPage } from "./pages/Quality";
import { ChangesPage } from "./pages/Changes";
import { AcceptInvite, ForgotPassword, ResetPassword } from "./pages/PublicAuth";
import { api } from "./api";
import { useAsync } from "./hooks";

// Monaco and Cytoscape are large; load them only when those views open.
const CodePage = lazy(() => import("./pages/Code"));
const GraphPage = lazy(() => import("./pages/Graph"));

/**
 * Navigation lists only sections that are implemented. Planned capabilities
 * (AI reasoning, transformation, verification) are described on the Settings
 * page under "Capabilities", not presented as working screens.
 */
const nav = [
  { to: "/", label: "Overview", end: true },
  { to: "/repositories", label: "Repositories" },
  { to: "/jobs", label: "Analysis jobs" },
  { to: "/workers", label: "Workers" },
  { to: "/settings", label: "Settings" },
];

export function App() {
  const { principal, loading, logout } = useAuth();
  const [theme, setTheme] = useTheme();
  const location = useLocation();
  if (loading) return <Loading label="Starting" />;
  if (!principal) {
    return (
      <Routes>
        <Route path="/login" element={<Login />} />
        <Route path="/forgot-password" element={<ForgotPassword />} />
        <Route path="/accept-invite" element={<AcceptInvite />} />
        <Route path="/reset-password" element={<ResetPassword />} />
        <Route path="*" element={<Navigate to="/login" replace state={{ from: location.pathname + location.search }} />} />
      </Routes>
    );
  }
  return (
    <div className="flex h-full">
      <a href="#main" className="sr-only focus:not-sr-only focus:absolute focus:left-2 focus:top-2 focus:z-50 focus:rounded focus:bg-surface focus:px-3 focus:py-2">
        Skip to content
      </a>
      <aside className="flex w-56 shrink-0 flex-col border-r border-border bg-surface">
        <div className="flex items-center gap-2 px-4 py-4">
          <img src="/favicon.svg" alt="" className="h-6 w-6" />
          <span className="font-semibold tracking-tight">USIE</span>
        </div>
        <nav aria-label="Main" className="flex-1 space-y-0.5 px-2">
          {nav.map((n) => (
            <NavLink
              key={n.to}
              to={n.to}
              end={n.end}
              className={({ isActive }) =>
                cx("block rounded-md px-3 py-1.5 text-sm", isActive ? "bg-accent/10 font-medium text-accent" : "text-muted hover:bg-surface-2 hover:text-fg")
              }
            >
              {n.label}
            </NavLink>
          ))}
        </nav>
        <div className="space-y-2 border-t border-border p-3 text-xs">
          <div className="truncate text-fg" title={principal.email}>{principal.display_name}</div>
          <OrganizationSwitcher />
          <label className="flex items-center justify-between gap-2 text-muted">
            Theme
            <Select aria-label="Theme" value={theme} onChange={(e) => setTheme(e.target.value as Theme)} className="h-7 text-xs">
              <option value="system">System</option>
              <option value="light">Light</option>
              <option value="dark">Dark</option>
            </Select>
          </label>
          <Button size="sm" variant="ghost" className="w-full" onClick={() => void logout()}>
            Sign out
          </Button>
        </div>
      </aside>
      <main id="main" className="min-w-0 flex-1 overflow-auto">
        <Suspense fallback={<Loading />}>
          <Routes>
            <Route path="/" element={<Overview />} />
            <Route path="/repositories" element={<Repositories />} />
            <Route path="/repositories/:repoId" element={<RepositoryPage />} />
            <Route path="/repositories/:repoId/code" element={<CodePage />} />
            <Route path="/repositories/:repoId/graph" element={<GraphPage />} />
            <Route path="/repositories/:repoId/ask" element={<AskPage />} />
            <Route path="/repositories/:repoId/quality" element={<QualityPage />} />
            <Route path="/repositories/:repoId/impact" element={<ImpactPage />} />
            <Route path="/repositories/:repoId/changes" element={<ChangesPage />} />
            <Route path="/jobs" element={<Jobs />} />
            <Route path="/workers" element={<Workers />} />
            <Route path="/settings" element={<Settings />} />
            <Route path="/accept-invite" element={<AcceptInvite />} />
            <Route path="/reset-password" element={<ResetPassword />} />
            <Route path="/login" element={<Navigate to="/" replace />} />
            <Route path="*" element={<div className="p-8 text-muted">Page not found.</div>} />
          </Routes>
        </Suspense>
      </main>
    </div>
  );
}

/**
 * Shows the current organization and role; people in several organizations
 * switch between them here (the session moves to the chosen organization).
 */
function OrganizationSwitcher() {
  const { principal, setPrincipal } = useAuth();
  const orgs = useAsync(() => (principal?.kind === "session" ? api.myOrganizations() : Promise.resolve(null)), [principal?.organization_id]);
  const [error, setError] = useState<string | null>(null);
  if (!principal) return null;
  const list = orgs.data?.organizations ?? [];
  if (list.length < 2) {
    return (
      <div className="truncate text-muted">
        {principal.organization_name} · {principal.role}
      </div>
    );
  }
  return (
    <div className="space-y-1">
      <Select
        aria-label="Organization"
        value={principal.organization_id}
        className="h-7 w-full text-xs"
        onChange={(e) => {
          setError(null);
          api.switchOrganization(e.target.value).then(
            (s) => {
              setPrincipal(s.principal);
              window.location.assign("/");
            },
            (err: unknown) => setError(err instanceof Error ? err.message : String(err)),
          );
        }}
      >
        {list.map((o) => <option key={o.id} value={o.id}>{o.name}</option>)}
      </Select>
      <div className="text-muted">{principal.role}</div>
      {error && <div role="alert" className="text-danger">{error}</div>}
    </div>
  );
}

export function PageHeader({ title, subtitle, actions }: { title: React.ReactNode; subtitle?: React.ReactNode; actions?: React.ReactNode }) {
  return (
    <header className="flex flex-wrap items-start justify-between gap-3 border-b border-border bg-surface px-6 py-4">
      <div className="min-w-0">
        <h1 className="truncate text-lg font-semibold">{title}</h1>
        {subtitle && <div className="mt-0.5 text-sm text-muted">{subtitle}</div>}
      </div>
      {actions && <div className="flex items-center gap-2">{actions}</div>}
    </header>
  );
}
