import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import { App } from "./App";
import { AuthProvider } from "./auth";

type Handler = (url: string, init?: RequestInit) => { status: number; body?: unknown };

function mockApi(handler: Handler) {
  const fetchMock = vi.fn(async (url: string, init?: RequestInit) => {
    const { status, body } = handler(url, init);
    return new Response(body === undefined ? null : JSON.stringify(body), { status, headers: { "Content-Type": "application/json" } });
  });
  vi.stubGlobal("fetch", fetchMock);
  return fetchMock;
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

window.matchMedia = window.matchMedia ?? ((() => ({ matches: false, addEventListener() {}, removeEventListener() {} })) as unknown as typeof window.matchMedia);

const principal = { user_id: "u", email: "a@b.test", display_name: "Ada", organization_id: "o", organization_name: "Acme", role: "viewer", kind: "session" };
const system = {
  version: "test", signup_allowed: false, sources: { git: true, upload: true, local: false }, limits: {}, engine: null,
  features: [
    { id: "parsing", status: "implemented", detail: "Python, JavaScript, TypeScript." },
    { id: "ai-reasoning", status: "planned", detail: "Not implemented." },
  ],
};

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe("App", () => {
  it("sends anonymous users to the sign-in page", async () => {
    mockApi((url) => (url.endsWith("/auth/me") ? { status: 401, body: { error: { code: "unauthenticated", message: "x", request_id: "r" } } } : { status: 200, body: system }));
    renderAt("/repositories");
    expect(await screen.findByRole("heading", { name: "Sign in to USIE" })).toBeTruthy();
    // Registration is hidden when the server says signup is closed.
    await waitFor(() => expect(screen.queryByText("Create an account")).toBeNull());
  });

  it("reports an unreachable API instead of showing an empty app", async () => {
    mockApi(() => ({ status: 502, body: { error: { code: "internal", message: "bad gateway", request_id: "r" } } }));
    renderAt("/");
    expect((await screen.findByRole("alert")).textContent).toContain("not reachable");
  });

  it("navigation contains only implemented sections", async () => {
    mockApi((url) => {
      if (url.endsWith("/auth/me")) return { status: 200, body: { principal, csrf_token: "c" } };
      if (url.includes("/repositories")) return { status: 200, body: { repositories: [] } };
      if (url.includes("/jobs")) return { status: 200, body: { jobs: [] } };
      if (url.includes("/workers")) return { status: 200, body: { workers: [] } };
      return { status: 200, body: system };
    });
    renderAt("/");
    const nav = await screen.findByRole("navigation", { name: "Main" });
    const labels = Array.from(nav.querySelectorAll("a")).map((a) => a.textContent);
    expect(labels).toEqual(["Overview", "Repositories", "Analysis jobs", "Workers", "Settings"]);
    // With no workers online, the overview says so rather than implying progress.
    expect(await screen.findByText(/No analysis worker is online/)).toBeTruthy();
  });

  it("shows planned capabilities as planned", async () => {
    mockApi((url) => (url.endsWith("/auth/me") ? { status: 200, body: { principal, csrf_token: "c" } } : { status: 200, body: system }));
    renderAt("/settings");
    const row = (await screen.findByText("ai reasoning")).closest("tr")!;
    expect(row.textContent).toContain("planned");
    expect(row.textContent).toContain("Not implemented.");
  });

  it("hides write actions from viewers", async () => {
    mockApi((url) => {
      if (url.endsWith("/auth/me")) return { status: 200, body: { principal, csrf_token: "c" } };
      if (url.includes("/repositories")) return { status: 200, body: { repositories: [] } };
      return { status: 200, body: system };
    });
    renderAt("/repositories");
    await screen.findByText("No repositories");
    expect(screen.queryByRole("button", { name: "Add repository" })).toBeNull();
  });
});
