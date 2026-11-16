import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import { App } from "./App";
import { AuthProvider } from "./auth";

window.matchMedia = window.matchMedia ?? ((() => ({ matches: false, addEventListener() {}, removeEventListener() {} })) as unknown as typeof window.matchMedia);

const member = { user_id: "u", email: "a@b.test", display_name: "Ada", organization_id: "o", organization_name: "Acme", role: "member", kind: "session" };
const version = { id: "v2", repository_id: "r1", sequence: 2, status: "ready", requested_ref: "", commit_sha: "", branch: "", tree_hash: "", parent_version_id: "v1", stats: {}, error: "", created_at: "2026-01-01T00:00:00Z", analyzed_at: "2026-01-01T00:00:00Z" };
const repo = { id: "r1", workspace_id: "w", name: "svc", source_kind: "upload", source_location: "", default_ref: "", created_at: "2026-01-01T00:00:00Z", latest_version: version };
const cat = (open: number, nw: number, reviewed = 0, by_severity = {}) => ({ open, new: nw, reviewed, by_severity });

const summary = {
  version_id: "v2", available: true, baseline_version_id: "v1",
  gate: { status: "failed", default: true, conditions: [
    { metric: "new_vulnerabilities", threshold: 0, actual: 1, status: "failed" },
    { metric: "blocker_findings", threshold: 0, actual: 0, status: "passed" },
  ] },
  categories: { bug: cat(0, 0), vulnerability: cat(2, 1, 0, { critical: 2 }), hotspot: cat(1, 0, 1, { critical: 1 }), smell: cat(3, 0) },
  grades: { reliability: "A", security: "D", security_review: "C" },
  hotspots_reviewed_percent: 50,
  metrics: { functions: 12, complexity: 40, max_complexity: 9, duplication_compared_lines: 400, duplicated_lines: 26, duplicated_blocks: 2, duplicated_lines_percent: 6.5, duplication_skipped: false },
  rules: [{ rule: "sql-injection", name: "SQL queries should not be built from untrusted strings", category: "vulnerability", severity: "critical", count: 2 }],
};
const finding = {
  seq: 0, rule: "sql-injection", category: "vulnerability", severity: "critical", path: "app/db.py", start_line: 15, start_col: 4, end_line: 15, end_col: 60,
  message: "`cursor.execute` receives SQL built by interpolation. Use query parameters instead.", function: "remove",
  fingerprint: "0123456789abcdef0123456789abcdef", is_new: true, in_test: false, status: "open", review: null,
};
const rules = [{ id: "sql-injection", name: "SQL queries should not be built from untrusted strings", category: "vulnerability", severity: "critical", languages: ["python"],
  description: "A database query method receives SQL built by concatenation.", remediation: "Pass values as query parameters.", source: "engine" }];
const metrics = [{ metric: "new_vulnerabilities", description: "Open vulnerabilities not present in the previous version", new_code: true },
  { metric: "blocker_findings", description: "Open findings of blocker severity, any category", new_code: false }];

type Call = { url: string; method: string; body: unknown };
function mock() {
  const calls: Call[] = [];
  vi.stubGlobal("fetch", vi.fn(async (url: string, init?: RequestInit) => {
    const method = init?.method ?? "GET";
    calls.push({ url, method, body: init?.body ? JSON.parse(String(init.body)) : undefined });
    if (method === "PUT" && url.includes("/review")) return new Response(null, { status: 204 });
    const body = (() => {
      if (url.endsWith("/auth/me")) return { principal: member, csrf_token: "t" };
      if (url.includes("/quality/rules")) return { rules, metrics, engine_rules_available: true };
      if (url.includes("/versions/v2/quality")) return summary;
      if (url.includes("/versions/v2/findings")) return { findings: [finding], total: 1 };
      if (url.includes("/repositories/r1/versions")) return { versions: [version] };
      if (url.includes("/repositories/r1")) return repo;
      return {};
    })();
    return new Response(JSON.stringify(body), { status: 200, headers: { "Content-Type": "application/json" } });
  }));
  return calls;
}

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe("quality page", () => {
  it("shows the gate, grades and findings, and requires a reason for a false positive", async () => {
    const calls = mock();
    render(
      <MemoryRouter initialEntries={["/repositories/r1/quality?version=v2"]}>
        <AuthProvider>
          <App />
        </AuthProvider>
      </MemoryRouter>,
    );
    expect(await screen.findByText("Failed")).toBeTruthy();
    expect(screen.getByText("Open vulnerabilities not present in the previous version")).toBeTruthy();
    expect(screen.getByText("6.5%")).toBeTruthy();
    expect(screen.getByTitle(/no open vulnerabilities/).textContent).toBe("D");
    expect(await screen.findByText(finding.message)).toBeTruthy();
    expect(screen.getByText("app/db.py:15").getAttribute("href")).toBe("/repositories/r1/code?version=v2&path=app%2Fdb.py&line=15");
    // The category cards say "new" too; the finding carries a "new" badge.
    expect(screen.getAllByText("new").some((el) => el.closest("tr"))).toBe(true);

    // The open-findings request is the default.
    const list = calls.find((c) => c.url.includes("/findings"))!;
    expect(list.url).toContain("status=open");

    // Expanding shows what the rule matches.
    fireEvent.click(screen.getByText(finding.message));
    expect(screen.getByText(/A database query method receives SQL/)).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "False positive" }));
    const comment = screen.getByLabelText("Review comment") as HTMLInputElement;
    expect(comment.required).toBe(true);
    fireEvent.change(comment, { target: { value: "  uid is an integer from the router  " } });
    fireEvent.click(screen.getAllByRole("button", { name: "False positive" }).at(-1)!);
    await waitFor(() => expect(calls.some((c) => c.method === "PUT")).toBe(true));
    const put = calls.find((c) => c.method === "PUT")!;
    expect(put.url).toContain(`/repositories/r1/findings/${finding.fingerprint}/review`);
    expect(put.body).toEqual({ status: "false_positive", comment: "uid is an integer from the router" });
  });

  it("filters by category from the summary cards", async () => {
    const calls = mock();
    render(
      <MemoryRouter initialEntries={["/repositories/r1/quality?version=v2"]}>
        <AuthProvider>
          <App />
        </AuthProvider>
      </MemoryRouter>,
    );
    fireEvent.click(await screen.findByRole("button", { name: /Security hotspots/ }));
    await waitFor(() => expect(calls.some((c) => c.url.includes("/findings") && c.url.includes("category=hotspot"))).toBe(true));
  });
});
