import { describe, expect, it } from "vitest";
import { ApiError, UsieClient, keyLabel, keyPath, parseKey } from "./index";

function fakeFetch(handler: (url: string, init: RequestInit) => Response) {
  const calls: { url: string; init: RequestInit }[] = [];
  const fn = (async (url: string, init: RequestInit) => {
    calls.push({ url, init });
    return handler(url, init);
  }) as unknown as typeof fetch;
  return { fn, calls };
}

describe("UsieClient", () => {
  it("sends the CSRF token on writes after login, never on reads", async () => {
    const { fn, calls } = fakeFetch((url) =>
      url.endsWith("/auth/login")
        ? new Response(JSON.stringify({ principal: { role: "owner" }, csrf_token: "c1" }), { status: 200 })
        : new Response(JSON.stringify({ repositories: [] }), { status: 200 }),
    );
    const client = new UsieClient({ fetch: fn });
    await client.login("a@b.test", "pw");
    await client.listRepositories();
    await client.createRepository({ name: "r", source_kind: "upload" });
    const headers = (i: number) => calls[i]!.init.headers as Record<string, string>;
    expect(headers(1)["X-CSRF-Token"]).toBeUndefined();
    expect(headers(2)["X-CSRF-Token"]).toBe("c1");
    expect(calls[2]!.init.body).toBe(JSON.stringify({ name: "r", source_kind: "upload" }));
  });

  it("encodes query parameters and skips empty ones", async () => {
    const { fn, calls } = fakeFetch(() => new Response(JSON.stringify({ nodes: [], total: 0 }), { status: 200 }));
    await new UsieClient({ fetch: fn, baseUrl: "/api/v1/" }).searchGraph("v1", { q: "a#b", kind: ["function", "class"], path_prefix: "" });
    expect(calls[0]!.url).toBe("/api/v1/versions/v1/graph/search?q=a%23b&kind=function%2Cclass");
  });

  it("uses bearer tokens when configured", async () => {
    const { fn, calls } = fakeFetch(() => new Response(null, { status: 204 }));
    await new UsieClient({ fetch: fn, token: "usie_x" }).revokeToken("t1");
    expect((calls[0]!.init.headers as Record<string, string>).Authorization).toBe("Bearer usie_x");
  });

  it("surfaces structured API errors", async () => {
    const { fn } = fakeFetch(
      () => new Response(JSON.stringify({ error: { code: "version_not_ready", message: "not yet", request_id: "r1" } }), { status: 409 }),
    );
    const err = await new UsieClient({ fetch: fn }).graphStats("v").catch((e: unknown) => e);
    expect(err).toBeInstanceOf(ApiError);
    expect(err).toMatchObject({ status: 409, code: "version_not_ready", message: "not yet", requestId: "r1" });
  });

  it("reports non-JSON responses as errors", async () => {
    const { fn } = fakeFetch(() => new Response("<html>bad gateway</html>", { status: 502 }));
    await expect(new UsieClient({ fetch: fn }).system()).rejects.toMatchObject({ code: "invalid_response", status: 502 });
  });
});

describe("node keys", () => {
  it("parses every key form", () => {
    expect(parseKey("repo")).toEqual({ type: "repo" });
    expect(parseKey("dir:src/a")).toEqual({ type: "dir", path: "src/a" });
    expect(parseKey("file:src/a.py")).toEqual({ type: "file", path: "src/a.py" });
    expect(parseKey("sym:src/a.py#C.m~2")).toEqual({ type: "symbol", path: "src/a.py", qualifiedName: "C.m" });
    expect(parseKey("ext:npm:@scope/pkg")).toEqual({ type: "external", ecosystem: "npm", name: "@scope/pkg" });
    expect(parseKey("weird")).toEqual({ type: "unknown", raw: "weird" });
    expect(keyPath("sym:a/b.ts#f")).toBe("a/b.ts");
    expect(keyPath("ext:npm:x")).toBeUndefined();
    expect(keyLabel("sym:a/b.ts#C.f")).toBe("C.f (a/b.ts)");
  });
});
