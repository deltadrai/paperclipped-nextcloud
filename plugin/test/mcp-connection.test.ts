import { randomUUID } from "node:crypto";

import { beforeEach, describe, expect, it } from "vitest";

import { initialPolicy, sync, type AgentState, type Api, type Bridge, type ConnectionRecord } from "../src/ui/mcp-connection.js";

/** The slice of Paperclip's REST API the sync uses, with its archive-on-delete behaviour. */
class FakePaperclip {
  secrets: { id: string; name: string; key: string; value: string; latestVersion: number; deleted?: boolean }[] = [];
  connections: Record<string, any>[] = [];
  applications: { id: string; name: string; status: string }[] = [];
  finished: Record<string, any>[] = [];
  calls: string[] = [];
  catalog = [
    { id: randomUUID(), toolName: "talk_list_conversations", status: "active", riskLevel: "read" },
    { id: randomUUID(), toolName: "talk_send_message", status: "active", riskLevel: "write" },
    { id: randomUUID(), toolName: "nc_mail_delete_message", status: "active", riskLevel: "destructive" },
  ];

  api: Api = async <T>(method: string, path: string, body?: any): Promise<T> => {
    this.calls.push(`${method} ${path}`);
    const m = (re: RegExp) => path.match(re);
    let r: RegExpMatchArray | null;
    if (method === "GET" && m(/^\/companies\/[^/]+\/secrets$/)) return this.secrets.filter((s) => !s.deleted) as T;
    if (method === "POST" && m(/^\/companies\/[^/]+\/secrets$/)) {
      if (this.secrets.some((s) => !s.deleted && s.name === body.name)) throw new Error("409 Secret already exists");
      const s = { id: randomUUID(), name: body.name, key: body.key, value: body.value, latestVersion: 1 };
      this.secrets.push(s);
      return s as T;
    }
    if (method === "POST" && (r = m(/^\/secrets\/([^/]+)\/rotate$/))) {
      const s = this.secrets.find((x) => x.id === r![1])!;
      s.value = body.value;
      s.latestVersion++;
      return s as T;
    }
    if (method === "DELETE" && (r = m(/^\/secrets\/([^/]+)$/))) {
      this.secrets.find((x) => x.id === r![1])!.deleted = true;
      return null as T;
    }
    if (method === "GET" && m(/^\/companies\/[^/]+\/tools\/connections$/)) return { connections: this.connections } as T;
    if (method === "GET" && m(/^\/companies\/[^/]+\/tools\/applications$/)) return { applications: this.applications } as T;
    if (method === "PATCH" && (r = m(/^\/tool-applications\/([^/]+)$/))) {
      Object.assign(this.applications.find((a) => a.id === r![1])!, body);
      return null as T;
    }
    if (method === "POST" && m(/^\/companies\/[^/]+\/tools\/connections$/)) {
      let app = this.applications.find((a) => a.id === body.applicationId);
      if (!app) {
        if (this.applications.some((a) => a.name === body.name)) throw new Error("409 application name taken");
        app = { id: randomUUID(), name: body.name, status: "active" };
        this.applications.push(app);
      }
      const c = { ...body, id: randomUUID(), applicationId: app.id, status: "draft", enabled: false, healthStatus: "unknown" };
      this.connections.push(c);
      return c as T;
    }
    if ((r = m(/^\/tool-connections\/([^/]+)$/))) {
      const c = this.connections.find((x) => x.id === r![1])!;
      if (method === "GET") return c as T;
      if (method === "PATCH") return Object.assign(c, body) as T;
      if (method === "DELETE") {
        c.status = "archived";
        c.enabled = false;
        this.applications.find((a) => a.id === c.applicationId)!.status = "archived";
        return null as T;
      }
    }
    if (method === "POST" && m(/^\/tool-connections\/[^/]+\/health-check$/)) return {} as T;
    if (method === "POST" && m(/^\/tool-connections\/[^/]+\/catalog\/refresh$/)) return { catalog: this.catalog } as T;
    if (method === "POST" && (r = m(/^\/companies\/[^/]+\/tools\/apps\/([^/]+)\/finish$/))) {
      Object.assign(this.connections.find((x) => x.id === r![1])!, { status: "active", enabled: true });
      this.finished.push(body);
      return {} as T;
    }
    throw new Error(`unexpected ${method} ${path}`);
  };

  live() {
    return this.connections.filter((c) => c.status !== "archived");
  }
}

const CID = randomUUID();
const AID = randomUUID();

describe("sync", () => {
  let pc: FakePaperclip;
  let stored: ConnectionRecord | null;
  let reveals: number;
  let header: string;
  let state: AgentState;
  const bridge: Bridge = {
    revealHeader: async () => {
      reveals++;
      return header;
    },
    record: async (rec) => {
      stored = rec;
    },
  };
  const run = (patch: Partial<AgentState> = {}) => {
    state = { ...state, ...patch, connection: stored };
    return sync(pc.api, state, bridge);
  };

  beforeEach(() => {
    pc = new FakePaperclip();
    stored = null;
    reveals = 0;
    header = "Basic aaa";
    state = {
      companyId: CID,
      agentId: AID,
      identity: { state: "active", nextcloudUser: "ada" },
      mcpUrl: "http://100.64.0.10:8000/mcp",
      hasMcpCredential: true,
      credentialsUpdatedAt: "t1",
      connection: null,
    };
  });

  it("creates the secret and the connection, grants the tools to this agent only, and records it", async () => {
    const r = await run();
    expect(r.connection).toMatchObject({ name: "Nextcloud MCP (ada)", enabled: true, status: "active" });
    expect(pc.secrets).toHaveLength(1);
    expect(pc.secrets[0]).toMatchObject({ name: "Nextcloud MCP (ada)", key: "nextcloud-mcp.ada", value: "Basic aaa" });
    const c = pc.live()[0];
    expect(c).toMatchObject({
      transport: "mcp_remote",
      authKind: "api_key",
      config: { url: state.mcpUrl, mcpSessionRequired: true },
      credentialRefs: [{ name: "Authorization", secretId: pc.secrets[0].id, placement: "header", key: "Authorization" }],
      credentialSecretRefs: [{ secretId: pc.secrets[0].id, configPath: "Authorization" }],
    });
    expect(pc.finished[0].access).toEqual({ agentIds: [AID] });
    expect(stored).toEqual({ connectionId: c.id, secretId: pc.secrets[0].id, syncedCredentialsAt: "t1" });
  });

  it("changes nothing and reveals nothing on a repeat run", async () => {
    await run();
    pc.calls = [];
    const r = await run();
    expect(r.changes).toEqual([]);
    expect(reveals).toBe(1);
    expect(pc.calls.every((c) => c.startsWith("GET"))).toBe(true);
  });

  it("writes rotated credentials into the same secret", async () => {
    await run();
    header = "Basic bbb";
    const r = await run({ credentialsUpdatedAt: "t2" });
    expect(r.changes).toEqual(["secret updated"]);
    expect(pc.secrets[0]).toMatchObject({ value: "Basic bbb", latestVersion: 2 });
    expect(pc.live()).toHaveLength(1);
    expect(stored?.syncedCredentialsAt).toBe("t2");
  });

  it("switches the connection off while the ID is disabled and on again when re-enabled", async () => {
    await run();
    expect((await run({ identity: { state: "disabled", nextcloudUser: "ada" } })).connection?.enabled).toBe(false);
    expect((await run({ identity: { state: "active", nextcloudUser: "ada" } })).connection?.enabled).toBe(true);
    expect(pc.live()).toHaveLength(1);
  });

  it("never creates a connection for a disabled ID", async () => {
    const r = await run({ identity: { state: "disabled", nextcloudUser: "ada" } });
    expect(r).toMatchObject({ connection: null, note: "Created when the ID is re-enabled." });
    expect(pc.connections).toHaveLength(0);
    expect(pc.secrets).toHaveLength(0);
  });

  it("removes the connection and the secret once the ID is gone", async () => {
    await run();
    const r = await run({ identity: null });
    expect(r.changes).toEqual(["connection removed"]);
    expect(pc.live()).toHaveLength(0);
    expect(pc.secrets.every((s) => s.deleted)).toBe(true);
    expect(stored).toBeNull();
  });

  it("reuses the archived tool application when the same user is connected again", async () => {
    await run();
    await run({ identity: null });
    const app = pc.applications[0];
    expect(app.status).toBe("archived");
    const r = await run({ identity: { state: "active", nextcloudUser: "ada" }, credentialsUpdatedAt: "t3" });
    expect(r.changes).toContain("connection created (existing app reused)");
    expect(pc.applications).toHaveLength(1);
    expect(app.status).toBe("active");
    expect(pc.live()[0].applicationId).toBe(app.id);
  });

  it("adopts a connection and secret it lost track of, rewriting the secret", async () => {
    await run();
    const before = pc.live()[0].id;
    stored = null;
    header = "Basic ccc";
    const r = await run();
    expect(r.changes).toEqual(["secret updated"]);
    expect(pc.secrets[0].value).toBe("Basic ccc");
    expect((stored as ConnectionRecord | null)?.connectionId).toBe(before);
  });

  it("replaces a connection that points at another secret", async () => {
    await run();
    const old = pc.live()[0].id;
    pc.secrets[0].deleted = true;
    const r = await run();
    expect(r.changes).toEqual(["secret created", "connection replaced (new secret)", "connection created (existing app reused)", "3 tools granted to this agent"]);
    expect(pc.live()).toHaveLength(1);
    expect(pc.live()[0].id).not.toBe(old);
  });

  it("repairs drifted connection settings", async () => {
    await run();
    pc.live()[0].config = { url: state.mcpUrl };
    expect((await run()).changes).toEqual(["connection settings updated"]);
    expect(pc.live()[0].config.mcpSessionRequired).toBe(true);
  });

  it("does nothing without an MCP URL", async () => {
    const r = await run({ mcpUrl: null });
    expect(r.note).toMatch(/mcp\.url/);
    expect(pc.calls).toEqual([]);
  });
});

describe("initialPolicy", () => {
  it("allows read and write tools, asks first for destructive ones, reviews quarantined ones", () => {
    const e = (riskLevel: string, status = "active") => ({ id: randomUUID(), toolName: riskLevel, status, riskLevel });
    const [r, w, d, h, q, gone] = [e("read"), e("write"), e("destructive"), e("high"), e("read", "quarantined"), e("write", "removed")];
    expect(initialPolicy([r, w, d, h, q, gone])).toEqual({
      enabledCatalogEntryIds: [r.id, w.id, q.id],
      askFirstCatalogEntryIds: [d.id, h.id],
      reviewedCatalogEntryIds: [q.id],
    });
    expect(initialPolicy([r])).not.toHaveProperty("reviewedCatalogEntryIds");
  });
});
