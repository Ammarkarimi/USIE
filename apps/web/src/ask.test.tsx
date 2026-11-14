import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import { App } from "./App";
import { AuthProvider } from "./auth";

window.matchMedia = window.matchMedia ?? ((() => ({ matches: false, addEventListener() {}, removeEventListener() {} })) as unknown as typeof window.matchMedia);
// Some engines return a promise from scroll APIs; effects must not leak it.
Element.prototype.scrollIntoView = (() => Promise.resolve()) as unknown as typeof Element.prototype.scrollIntoView;

const principal = (role: string) => ({ user_id: "u", email: "a@b.test", display_name: "Ada", organization_id: "o", organization_name: "Acme", role, kind: "session" });
const version = { id: "v1", repository_id: "r1", sequence: 1, status: "ready", requested_ref: "", commit_sha: "", branch: "", tree_hash: "", parent_version_id: null, stats: {}, error: "", created_at: "2026-01-01T00:00:00Z", analyzed_at: "2026-01-01T00:00:00Z" };
const repo = { id: "r1", workspace_id: "w", name: "shop", source_kind: "upload", source_location: "", default_ref: "", created_at: "2026-01-01T00:00:00Z", latest_version: version };
const enabled = { enabled: true, provider: "anthropic", model: "claude-opus-5-5", daily_token_budget: 1000000, tokens_used_today: 1250, data_notice: "Questions and source excerpts are sent to anthropic." };

const conversation = {
  id: "c1", repository_id: "r1", version_id: "v1", title: "What calls total?", created_by: "u", created_at: "2026-01-01T00:00:00Z", updated_at: "2026-01-01T00:00:00Z",
  messages: [
    { id: "m1", seq: 1, role: "user", content: "What calls total?", focus_key: "", status: "ok", answer: {}, context: [], provider: "", model: "", input_tokens: 0, output_tokens: 0, cache_read_tokens: 0, latency_ms: 0, error: "", created_at: "", completed_at: null },
    {
      id: "m2", seq: 2, role: "assistant", content: "", focus_key: "", status: "ok", provider: "anthropic", model: "claude-opus-5-5",
      input_tokens: 900, output_tokens: 120, cache_read_tokens: 100, latency_ms: 4200, error: "", created_at: "", completed_at: "",
      answer: {
        answerable: "partially",
        summary: "`checkout` calls `total` [C2]. <img src=x onerror=alert(1)>",
        claims: [
          { statement: "checkout calls total", support: "evidence", citations: ["C2"] },
          { statement: "billing also uses it", support: "unsupported", citations: [], invalid_citations: ["C9"] },
          { statement: "probably a hot path", support: "inference", citations: [] },
        ],
        limitations: ["The billing module was not in the evidence."],
        followups: ["Where is total defined?"],
        validation_notes: ["1 claim(s) were presented as evidence-based but cite no evidence that was provided; they are marked unsupported"],
      },
      context: [
        { id: "C1", kind: "overview", title: "Repository overview", content: "Knowledge graph: 7 nodes" },
        { id: "C2", kind: "symbol", key: "sym:shop/cart.py#Cart.checkout", path: "shop/cart.py", start_line: 4, end_line: 7, title: "Cart.checkout", content: "    6 |         return total(items)" },
      ],
    },
  ],
};

function mock(role: string, status: object, conversations: object[] = [], pendingPolls = 0) {
  let polls = 0;
  vi.stubGlobal("fetch", vi.fn(async (url: string) => {
    const body = (() => {
      if (url.endsWith("/auth/me")) return { principal: principal(role), csrf_token: "t" };
      if (url.includes("/ai/status")) return status;
      if (url.includes("/ai/conversations/c1")) {
        if (polls++ < pendingPolls) {
          const pending = { ...conversation.messages[1], status: "pending", answer: {}, context: [] };
          return { ...conversation, messages: [conversation.messages[0], pending] };
        }
        return conversation;
      }
      if (url.includes("/ai/conversations")) return { conversations };
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

describe("Ask page", () => {
  it("explains how to enable AI when no provider is configured", async () => {
    mock("member", { enabled: false, provider: "", model: "", daily_token_budget: 0, tokens_used_today: 0, data_notice: "disabled" });
    renderAt("/repositories/r1/ask");
    expect(await screen.findByText("AI answers are not enabled on this installation")).toBeTruthy();
    expect(screen.queryByLabelText("Question")).toBeNull();
  });

  it("shows how each claim is backed and never renders answer text as markup", async () => {
    mock("member", enabled, [conversation]);
    const { container } = renderAt("/repositories/r1/ask?conversation=c1");
    expect(await screen.findByText("AI-generated")).toBeTruthy();
    expect(screen.getByText("partial answer")).toBeTruthy();
    expect(screen.getByText("cited")).toBeTruthy();
    expect(screen.getByText("unsupported")).toBeTruthy();
    expect(screen.getByText("inference")).toBeTruthy();
    expect(screen.getByText(/cited missing evidence: C9/)).toBeTruthy();
    expect(screen.getByText("The billing module was not in the evidence.")).toBeTruthy();
    expect(container.querySelector("article")).toBeTruthy();
    expect(container.querySelector("article img")).toBeNull();
    expect(screen.getByText(/onerror=alert/)).toBeTruthy();
    // A citation opens the evidence it refers to, with a link to the code.
    fireEvent.click(screen.getAllByRole("button", { name: "C2" })[0]!);
    const link = (await screen.findAllByRole("link", { name: "shop/cart.py:4-7" }))[0]!;
    expect(link.getAttribute("href")).toBe("/repositories/r1/code?version=v1&path=shop%2Fcart.py&line=4");
    expect(screen.getByText(/claude-opus-5-5 · 1,000 input \/ 120 output tokens/)).toBeTruthy();
    expect(screen.getByText(/Questions and source excerpts are sent to anthropic/)).toBeTruthy();
  });

  it("moves from pending to the answer without crashing", async () => {
    mock("member", enabled, [conversation], 1);
    renderAt("/repositories/r1/ask?conversation=c1");
    expect(await screen.findByText(/Collecting evidence and asking claude-opus-5-5/)).toBeTruthy();
    expect(await screen.findByText("AI-generated", {}, { timeout: 4000 })).toBeTruthy();
  });

  it("lets viewers read but not ask", async () => {
    mock("viewer", enabled, [conversation]);
    renderAt("/repositories/r1/ask?conversation=c1");
    await screen.findByText("AI-generated");
    expect(screen.queryByLabelText("Question")).toBeNull();
  });
});
