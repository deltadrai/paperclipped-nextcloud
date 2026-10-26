import { describe, expect, it } from "vitest";

import {
  PcncClient,
  ServerError,
  assertHumanCaller,
  basicAuthorization,
  deriveKey,
  lifecycleStatus,
  mask,
  mergeCredentials,
  open,
  reveal,
  seal,
  suggestName,
} from "../src/core.js";

const key = deriveKey("a-sufficiently-long-test-key");

describe("sealing", () => {
  it("round-trips and binds to its agent and field", () => {
    const sealed = seal(key, "agent-1/mcpAppPassword", "s3cret");
    expect(sealed.startsWith("v1:")).toBe(true);
    expect(open(key, "agent-1/mcpAppPassword", sealed)).toBe("s3cret");
    expect(() => open(key, "agent-2/mcpAppPassword", sealed)).toThrow();
    expect(() => open(deriveKey("another-long-test-key!!"), "agent-1/mcpAppPassword", sealed)).toThrow();
  });

  it("rejects short keys", () => {
    expect(() => deriveKey("short")).toThrow();
  });

  it("merges only fresh credentials and keeps the rest", () => {
    const first = mergeCredentials(key, "a1", "ada", null, { nextcloudLoginPassword: "login-password-1", mcpAppPassword: "mcp-password-1" }, "t1");
    const second = mergeCredentials(key, "a1", "ada", first, { mcpAppPassword: "mcp-password-2" }, "t2");
    expect(reveal(key, "a1", second, "nextcloudLoginPassword")).toBe("login-password-1");
    expect(reveal(key, "a1", second, "mcpAppPassword")).toBe("mcp-password-2");
    expect(second.updatedAt).toBe("t2");
    expect(second.user).toBe("ada");
    expect(second.hints).toEqual({
      nextcloudLoginPassword: "logi…rd-1",
      mcpAppPassword: "mcp-…rd-2",
      mcpAuthorization: `Basic ${mask(basicAuthorization("ada", "mcp-password-2").slice(6))}`,
    });
    expect(JSON.stringify(second)).not.toContain("mcp-password-2");
    const unchanged = mergeCredentials(key, "a1", "ada", second, {}, "t3");
    expect(unchanged.updatedAt).toBe("t2");
  });
});

describe("helpers", () => {
  it("masks", () => {
    expect(mask("Xk3fABCDEFGHa9Q2")).toBe("Xk3f…a9Q2");
    expect(mask("short")).toBe("••••••");
  });

  it("builds the MCP authorization header", () => {
    expect(basicAuthorization("ada", "pw")).toBe(`Basic ${Buffer.from("ada:pw").toString("base64")}`);
  });

  it("suggests names like the server", () => {
    expect(suggestName("Ada Lovelace")).toBe("ada.lovelace");
    expect(suggestName("  Jürgen   (CEO) ")).toBe("jurgen.ceo");
    expect(suggestName("007 Bond")).toBe("agent.007.bond");
  });

  it("lets only users manage identities", () => {
    expect(() => assertHumanCaller({ actor: { type: "user" } })).not.toThrow();
    expect(() => assertHumanCaller({ actor: { type: "agent" } })).toThrow(/signed-in user/);
    expect(() => assertHumanCaller({ actor: { type: "system" } })).toThrow();
  });

  it("blocks agent cleanup while an identity exists", () => {
    expect(lifecycleStatus("cleaning_up", true)).toBe("pending");
    expect(lifecycleStatus("cleaning_up", "unknown")).toBe("pending");
    expect(lifecycleStatus("cleaning_up", false)).toBe("complete");
    for (const phase of ["preparing", "verifying", "pausing", "resuming", "terminating"]) {
      expect(lifecycleStatus(phase, true)).toBe("complete");
    }
  });
});

describe("server client", () => {
  function fakeFetch(handler: (method: string, url: string, init: RequestInit) => [number, unknown]) {
    const calls: { method: string; url: string; auth: string | null; body: unknown }[] = [];
    const f = (async (url: string, init: RequestInit = {}) => {
      const method = init.method ?? "GET";
      const headers = new Headers(init.headers);
      calls.push({ method, url, auth: headers.get("authorization"), body: init.body ? JSON.parse(String(init.body)) : undefined });
      const [status, body] = handler(method, url, init);
      return new Response(body === undefined ? "" : JSON.stringify(body), { status });
    }) as unknown as typeof fetch;
    return { f, calls };
  }

  it("sends the bearer token and maps routes", async () => {
    const { f, calls } = fakeFetch(() => [200, { identity: null, steps: [] }]);
    const c = new PcncClient("http://srv:8091/", "tok", f);
    await c.provision("agent:1", { name: "ada", displayName: "Ada", sharedMailboxes: ["contact"] });
    await c.disable("agent:1");
    await c.purge("agent:1");
    expect(calls.map((x) => `${x.method} ${x.url}`)).toEqual([
      "PUT http://srv:8091/v1/identities/agent%3A1",
      "POST http://srv:8091/v1/identities/agent%3A1/disable",
      "DELETE http://srv:8091/v1/identities/agent%3A1",
    ]);
    expect(calls[0].auth).toBe("Bearer tok");
    expect(calls[0].body).toEqual({ name: "ada", displayName: "Ada", sharedMailboxes: ["contact"] });
  });

  it("returns null for a missing identity and raises server errors", async () => {
    const { f } = fakeFetch((_m, url) =>
      url.endsWith("/missing") ? [404, { error: "not_found", message: "identity not found" }] : [409, { error: "name_taken", message: "taken" }],
    );
    const c = new PcncClient("http://srv", "tok", f);
    expect(await c.get("missing")).toBeNull();
    await expect(c.provision("x", { name: "ada", displayName: "A", sharedMailboxes: [] })).rejects.toMatchObject({
      status: 409,
      code: "name_taken",
    });
    await expect(c.get("other")).rejects.toBeInstanceOf(ServerError);
  });
});
