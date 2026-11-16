import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import { App } from "./App";
import { AuthProvider } from "./auth";

window.matchMedia = window.matchMedia ?? ((() => ({ matches: false, addEventListener() {}, removeEventListener() {} })) as unknown as typeof window.matchMedia);

const admin = { user_id: "u1", email: "ada@acme.test", display_name: "Ada", organization_id: "o1", organization_name: "Acme", role: "admin", kind: "session" };
const members = [
  { user_id: "u0", email: "olga@acme.test", display_name: "Olga", role: "owner", disabled: false, joined_at: "2026-01-01T00:00:00Z" },
  { user_id: "u1", email: "ada@acme.test", display_name: "Ada", role: "admin", disabled: false, joined_at: "2026-01-02T00:00:00Z" },
  { user_id: "u2", email: "bo@acme.test", display_name: "Bo", role: "member", disabled: false, joined_at: "2026-01-03T00:00:00Z" },
];

// Empty lists for the pages a successful sign-in lands on.
function fallback(url: string): { status?: number; body: unknown } {
  for (const k of ["repositories", "jobs", "workers"]) if (url.includes(`/${k}`)) return { body: { [k]: [] } };
  return { body: {} };
}

type Call = { url: string; method: string; body: unknown };

function mock(routes: (url: string, method: string) => { status?: number; body: unknown } | undefined) {
  const calls: Call[] = [];
  vi.stubGlobal("fetch", vi.fn(async (url: string, init?: RequestInit) => {
    const method = init?.method ?? "GET";
    calls.push({ url, method, body: init?.body ? JSON.parse(String(init.body)) : undefined });
    const r = routes(url, method) ?? fallback(url);
    return new Response(r.status === 204 ? null : JSON.stringify(r.body), { status: r.status ?? 200, headers: { "Content-Type": "application/json" } });
  }));
  return calls;
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
  window.history.replaceState(null, "", "/");
});

const unauthenticated = { status: 401, body: { error: { code: "unauthenticated", message: "authentication required" } } };

describe("team settings", () => {
  it("lets an admin manage non-owners and shows an invitation link once", async () => {
    const calls = mock((url, method) => {
      if (url.endsWith("/auth/me")) return { body: { principal: admin, csrf_token: "t" } };
      if (url.endsWith("/auth/organizations")) return { body: { organizations: [{ id: "o1", name: "Acme", role: "admin" }], current: "o1" } };
      if (url.endsWith("/members")) return { body: { members } };
      if (url.endsWith("/invitations") && method === "POST") {
        return { status: 201, body: { invitation: { id: "i1", email: "new@acme.test", role: "member", status: "open" }, emailed: false, accept_path: "/accept-invite#token=usie_secret" } };
      }
      if (url.endsWith("/invitations")) return { body: { invitations: [], email_enabled: false } };
      return undefined;
    });
    renderAt("/settings?tab=team");
    expect(await screen.findByText("Members of Acme")).toBeTruthy();
    await screen.findByText("Bo");

    // The owner's role is not editable by an admin, nor is the admin's own.
    expect(screen.queryByLabelText("Role of Olga")).toBeNull();
    expect(screen.queryByLabelText("Role of Ada")).toBeNull();
    const bo = screen.getByLabelText("Role of Bo") as HTMLSelectElement;
    expect(Array.from(bo.options).map((o) => o.value)).toEqual(["viewer", "member", "admin"]);
    expect(screen.getByText(/does not send email: you get the invitation link once/)).toBeTruthy();

    fireEvent.change(screen.getByLabelText("Email"), { target: { value: "new@acme.test" } });
    fireEvent.click(screen.getByRole("button", { name: "Invite" }));
    expect(await screen.findByText(`${window.location.origin}/accept-invite#token=usie_secret`)).toBeTruthy();
    expect(calls.find((c) => c.method === "POST" && c.url.endsWith("/invitations"))?.body).toEqual({ email: "new@acme.test", role: "member" });

    fireEvent.change(bo, { target: { value: "viewer" } });
    await waitFor(() => expect(calls.some((c) => c.method === "PATCH" && c.url.endsWith("/members/u2"))).toBe(true));
    expect(calls.find((c) => c.method === "PATCH")?.body).toEqual({ role: "viewer" });
  });
});

describe("public account pages", () => {
  it("accepts an invitation with the token from the fragment and clears it from the URL", async () => {
    window.history.replaceState(null, "", "/accept-invite#token=usie_abc");
    const calls = mock((url) => {
      if (url.endsWith("/auth/me")) return unauthenticated;
      if (url.endsWith("/invitations/preview")) return { body: { organization_name: "Acme", email: "new@acme.test", role: "member", existing_account: false } };
      if (url.endsWith("/invitations/accept")) return { body: { principal: { ...admin, user_id: "u9", email: "new@acme.test", role: "member" }, csrf_token: "c" } };
      if (url.endsWith("/auth/organizations")) return { body: { organizations: [], current: "o1" } };
      return undefined;
    });
    renderAt("/accept-invite");
    expect(await screen.findByText("Join Acme")).toBeTruthy();
    expect(window.location.hash).toBe("");
    fireEvent.change(screen.getByLabelText("Your name"), { target: { value: "Nia" } });
    fireEvent.change(screen.getByLabelText(/^Choose a password/), { target: { value: "correct horse battery" } });
    fireEvent.click(screen.getByRole("button", { name: "Create account and join" }));
    await waitFor(() => expect(calls.some((c) => c.url.endsWith("/invitations/accept"))).toBe(true));
    expect(calls.find((c) => c.url.endsWith("/invitations/accept"))?.body).toEqual({ token: "usie_abc", password: "correct horse battery", display_name: "Nia" });
    expect(calls.find((c) => c.url.endsWith("/invitations/preview"))?.body).toEqual({ token: "usie_abc" });
  });

  it("explains an invalid invitation link", async () => {
    window.history.replaceState(null, "", "/accept-invite#token=usie_used");
    mock((url) => {
      if (url.endsWith("/auth/me")) return unauthenticated;
      if (url.endsWith("/invitations/preview")) return { status: 404, body: { error: { code: "invalid_link", message: "this link is invalid, expired or already used" } } };
      return undefined;
    });
    renderAt("/accept-invite");
    expect(await screen.findByText(/invalid, has expired or was already used/)).toBeTruthy();
  });

  it("points to an administrator when the installation cannot send email", async () => {
    mock((url) => {
      if (url.endsWith("/auth/me")) return unauthenticated;
      if (url.endsWith("/system")) return { body: { version: "t", signup_allowed: false, features: [], sources: {}, limits: {}, engine: null, email: false } };
      return undefined;
    });
    renderAt("/login");
    fireEvent.click(await screen.findByRole("link", { name: "Forgot password?" }));
    expect(await screen.findByText(/does not send email. Ask an administrator/)).toBeTruthy();
  });
});
