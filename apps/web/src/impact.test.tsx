import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import { App } from "./App";
import { AuthProvider } from "./auth";

window.matchMedia = window.matchMedia ?? ((() => ({ matches: false, addEventListener() {}, removeEventListener() {} })) as unknown as typeof window.matchMedia);

const principal = { user_id: "u", email: "a@b.test", display_name: "Ada", organization_id: "o", organization_name: "Acme", role: "viewer", kind: "session" };
const version = { id: "v1", repository_id: "r1", sequence: 1, status: "ready", requested_ref: "", commit_sha: "", branch: "", tree_hash: "", parent_version_id: null, stats: {}, error: "", created_at: "2026-01-01T00:00:00Z", analyzed_at: "2026-01-01T00:00:00Z" };
const repo = { id: "r1", workspace_id: "w", name: "shop", source_kind: "upload", source_location: "", default_ref: "", created_at: "2026-01-01T00:00:00Z", latest_version: version };
const node = (key: string, name: string, path: string, kind = "function") => ({ id: 1, key, kind, name, path, start_line: 1, end_line: 2 });

const impact = {
  changed: [{ node: node("sym:shop/pricing.py#total", "total", "shop/pricing.py"), reason: "lines 2–2 changed" }],
  unmatched: ["new.py: new file, not part of the analyzed version"],
  affected: [
    { node: node("sym:shop/cart.py#Cart.checkout", "checkout", "shop/cart.py", "method"), distance: 1, confidence: "confirmed", via: ["sym:shop/cart.py#Cart.checkout", "sym:shop/pricing.py#total"], kinds: ["CALLS"], test: false },
    { node: node("sym:tests/test_cart.py#test_checkout", "test_checkout", "tests/test_cart.py"), distance: 2, confidence: "probable", via: ["sym:tests/test_cart.py#test_checkout", "sym:shop/cart.py#Cart.checkout", "sym:shop/pricing.py#total"], kinds: ["CALLS", "CALLS"], test: true },
  ],
  tests: [] as unknown[],
  untested_changes: [] as string[],
  affected_files: 1,
  risk: {
    score: 31, level: "medium", note: "A heuristic for ranking changes. It is not a probability of failure.",
    factors: [{ id: "blast_radius", label: "Dependents", value: 0.13, weight: 30, points: 3.9, explanation: "1 non-test symbols and files depend on the change" }],
  },
  depth: 3, min_confidence: "probable", truncated: false, limitations: ["Dependencies are found through the relationships USIE resolved."],
};
impact.tests = [impact.affected[1]];

let lastBody: unknown = null;
function mock() {
  vi.stubGlobal("fetch", vi.fn(async (url: string, init?: RequestInit) => {
    const body = (() => {
      if (url.endsWith("/auth/me")) return { principal, csrf_token: "t" };
      if (url.includes("/impact")) {
        lastBody = JSON.parse(String(init?.body ?? "{}"));
        return { version_id: "v1", impact };
      }
      if (url.includes("/versions")) return { versions: [version] };
      if (url.includes("/repositories/r1")) return repo;
      return {};
    })();
    return new Response(JSON.stringify(body), { status: 200, headers: { "Content-Type": "application/json" } });
  }));
}

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  lastBody = null;
});

describe("impact page", () => {
  it("sends a diff and explains the result", async () => {
    mock();
    render(
      <MemoryRouter initialEntries={["/repositories/r1/impact?version=v1"]}>
        <AuthProvider>
          <App />
        </AuthProvider>
      </MemoryRouter>,
    );
    const textarea = await screen.findByPlaceholderText(/--- a\/src\/app.py/);
    fireEvent.change(textarea, { target: { value: "--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n+b\n" } });
    fireEvent.click(screen.getByRole("button", { name: "Analyze impact" }));
    expect(await screen.findByLabelText("Risk score 31 of 100")).toBeTruthy();
    expect(lastBody).toMatchObject({ diff: expect.stringContaining("+b"), depth: 3, min_confidence: "probable" });
    // Every factor of the score is shown with its explanation.
    expect(screen.getByText("1 non-test symbols and files depend on the change")).toBeTruthy();
    expect(screen.getByText(/not a probability of failure/)).toBeTruthy();
    // Tests and dependents are listed separately.
    const tests = screen.getByText("Tests that reach the change (1)").closest("section, div")!;
    expect(within(tests as HTMLElement).getByText("test_checkout")).toBeTruthy();
    expect(screen.getByText("Dependents (1)")).toBeTruthy();
    expect(screen.getByText("new.py: new file, not part of the analyzed version")).toBeTruthy();
    // The path to the change opens on demand.
    fireEvent.click(screen.getAllByRole("button", { name: "CALLS" })[0]!);
    expect(screen.getAllByText(/Cart.checkout \(shop\/cart.py\)/).length).toBeGreaterThan(0);
  });

  it("starts from a symbol given in the URL", async () => {
    mock();
    render(
      <MemoryRouter initialEntries={["/repositories/r1/impact?version=v1&key=sym%3Ashop%2Fpricing.py%23total"]}>
        <AuthProvider>
          <App />
        </AuthProvider>
      </MemoryRouter>,
    );
    expect(await screen.findByDisplayValue("sym:shop/pricing.py#total")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Analyze impact" }));
    await screen.findByLabelText("Risk score 31 of 100");
    expect(lastBody).toMatchObject({ symbols: ["sym:shop/pricing.py#total"] });
  });
});
