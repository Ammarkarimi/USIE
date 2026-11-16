import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import { App } from "./App";
import { AuthProvider } from "./auth";

window.matchMedia = window.matchMedia ?? ((() => ({ matches: false, addEventListener() {}, removeEventListener() {} })) as unknown as typeof window.matchMedia);

const principal = { user_id: "u", email: "a@b.test", display_name: "Ada", organization_id: "o", organization_name: "Acme", role: "member", kind: "session" };
const version = { id: "v1", repository_id: "r1", sequence: 1, status: "ready", requested_ref: "", commit_sha: "", branch: "", tree_hash: "", parent_version_id: null, stats: {}, error: "", created_at: "2026-01-01T00:00:00Z", analyzed_at: "2026-01-01T00:00:00Z" };
const repo = { id: "r1", workspace_id: "w", name: "calc", source_kind: "upload", source_location: "", default_ref: "", created_at: "2026-01-01T00:00:00Z", latest_version: version };
const change = {
  id: "c1", repository_id: "r1", version_id: "v1", request: "add() subtracts; make it add", focus_key: "", status: "proposed",
  summary: "Fix add.", rationale: "add subtracts [C1].", risks: ["callers relying on the old result"], test_plan: ["run test_calc"], limitations: [],
  edits: [], diff: "--- a/calc.py\n+++ b/calc.py\n@@ -1,2 +1,2 @@\n def add(a, b):\n-    return a - b\n+    return a + b <script>alert(1)</script>\n",
  files_changed: 1, lines_added: 1, lines_removed: 1, impact: {}, provider: "scripted", model: "scripted-model",
  input_tokens: 1, output_tokens: 1, cache_read_tokens: 0, latency_ms: 10, error: "", created_by: "u", created_at: "2026-01-01T00:00:00Z", completed_at: "2026-01-01T00:00:00Z",
};
const step = (exit: number, passed: number, failed: number) => ({ name: "test", argv: ["python3", "-m", "unittest", "discover", "-v"], exit_code: exit, timed_out: false, duration_seconds: 0.4, output: "Ran 1 test", output_clipped: false, tests: { passed, failed, skipped: 0 } });
const run = {
  id: "run1", change_id: "c1", version_id: "v1", profile: "unittest", baseline: true, status: "passed", error: "", created_by: "u",
  created_at: "2026-01-01T00:00:00Z", started_at: null, finished_at: null,
  result: { sandbox: "bubblewrap", network: false, baseline: { steps: [step(1, 0, 1)], passed: false }, change: { steps: [step(0, 1, 0)], passed: true }, fixed: ["test_add (test_calc.CalcTest.test_add)"], newly_failing: [], notes: ["The sandbox has no network access."] },
};

function mock(runs: object[]) {
  vi.stubGlobal("fetch", vi.fn(async (url: string) => {
    const body = (() => {
      if (url.endsWith("/auth/me")) return { principal, csrf_token: "t" };
      if (url.includes("/ai/status")) return { enabled: true, provider: "scripted", model: "scripted-model", daily_token_budget: 1, tokens_used_today: 0, data_notice: "Requests are sent to scripted." };
      if (url.includes("/verification-profiles")) return { sandbox: { sandbox: "bubblewrap", network: false, programs: ["python3"] }, profiles: [{ id: "unittest", label: "Python (unittest)", description: "unittest discover", requires: ["python3"], steps: [], detected: true, available: true }] };
      if (url.includes("/changes/c1")) return { change, verifications: runs };
      if (url.includes("/changes")) return { changes: [change] };
      if (url.includes("/versions")) return { versions: [version] };
      if (url.includes("/repositories/r1")) return repo;
      return {};
    })();
    return new Response(JSON.stringify(body), { status: 200, headers: { "Content-Type": "application/json" } });
  }));
}

function renderAt(path: string) {
  return render(
    <MemoryRouter initialEntries={[path]}>
      <AuthProvider>
        <App />
      </AuthProvider>
    </MemoryRouter>,
  );
}

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe("changes page", () => {
  it("labels an unverified proposal and renders the diff as text", async () => {
    mock([]);
    renderAt("/repositories/r1/changes?version=v1&change=c1");
    expect(await screen.findByText("AI-proposed, unverified")).toBeTruthy();
    const diff = screen.getByLabelText("Diff");
    expect(diff.textContent).toContain("+    return a + b <script>alert(1)</script>");
    expect(diff.querySelector("script")).toBeNull();
    expect(screen.getByText("callers relying on the old result")).toBeTruthy();
    expect(screen.getByRole("link", { name: "Download patch" }).getAttribute("href")).toBe("/api/v1/changes/c1/patch");
    expect(await screen.findByRole("button", { name: "Run verification" })).toBeTruthy();
    expect(screen.getByText(/no network access, no credentials/)).toBeTruthy();
  });

  it("shows a passed run against the baseline", async () => {
    mock([run]);
    renderAt("/repositories/r1/changes?version=v1&change=c1");
    expect(await screen.findByText("verified: a sandbox run passed")).toBeTruthy();
    expect(screen.getByText("Without the change")).toBeTruthy();
    expect(screen.getByText("0 passed, 1 failed")).toBeTruthy();
    expect(screen.getByText("1 passed, 0 failed")).toBeTruthy();
    expect(screen.getByText(/test_add \(test_calc/)).toBeTruthy();
  });
});
