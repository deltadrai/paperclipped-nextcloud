// The agent's Paperclip connection to the Nextcloud MCP server.
//
// Paperclip's plugin SDK has no API for tool connections; its REST API has, for signed-in board
// users. The panel therefore drives these routes in the user's own browser session, exactly as
// Paperclip's UI does. Agents never reach this code: it runs in the browser only, and the
// worker's `reveal` and `recordConnection` actions refuse non-user callers.
//
// Per agent, everything converges to:
// - a company secret holding `Authorization: Basic …` for the agent's Nextcloud user;
// - a remote-MCP connection "Nextcloud MCP (<user>)" that sends that header, granted to that
//   agent only, enabled while the identity is active;
// - its tools discovered, read and write tools allowed, destructive tools "ask first".
//
// No React or SDK imports: the e2e test drives this module from Node against a real Paperclip.

export type Api = <T = unknown>(method: string, path: string, body?: unknown) => Promise<T>;

export class ApiError extends Error {
  constructor(
    readonly status: number,
    message: string,
  ) {
    super(message);
    this.name = "ApiError";
  }
}

/** Paperclip's REST API with the browser's session cookie. */
export function restApi(base = "/api", fetchImpl: typeof fetch = fetch): Api {
  return async <T>(method: string, path: string, body?: unknown): Promise<T> => {
    const res = await fetchImpl(`${base}${path}`, {
      method,
      credentials: "same-origin",
      headers: body === undefined ? {} : { "Content-Type": "application/json" },
      body: body === undefined ? undefined : JSON.stringify(body),
    });
    const text = await res.text();
    let json: unknown = null;
    try {
      json = text ? JSON.parse(text) : null;
    } catch {
      json = null;
    }
    if (!res.ok) {
      const err = (json ?? {}) as { error?: unknown; message?: unknown };
      const msg = typeof err.error === "string" ? err.error : typeof err.message === "string" ? err.message : text.slice(0, 200);
      throw new ApiError(res.status, `${method} ${path}: ${res.status} ${msg || res.statusText}`);
    }
    return json as T;
  };
}

/** What the plugin keeps per agent (no secrets): which Paperclip objects belong to it. */
export interface ConnectionRecord {
  connectionId: string;
  secretId: string;
  /** `credentialsUpdatedAt` of the credentials last written into the secret. */
  syncedCredentialsAt: string | null;
}

/** The panel's view of one agent, from the worker's `identity` data. */
export interface AgentState {
  companyId: string;
  agentId: string;
  identity: { state: "provisioning" | "active" | "disabled"; nextcloudUser: string } | null;
  mcpUrl: string | null;
  /** Whether the plugin holds the agent's MCP app password. */
  hasMcpCredential: boolean;
  credentialsUpdatedAt: string | null;
  connection: ConnectionRecord | null;
}

/** The parts of the worker's `identity` data the sync reads. */
export interface IdentityDataLike {
  identity?: { state: "provisioning" | "active" | "disabled"; nextcloudUser: string } | null;
  info?: { mcpUrl: string | null };
  credentials: { mcpAppPassword?: string };
  credentialsUpdatedAt: string | null;
  connection: ConnectionRecord | null;
}

export function agentState(data: IdentityDataLike, agentId: string, companyId: string): AgentState {
  return {
    companyId,
    agentId,
    identity: data.identity ? { state: data.identity.state, nextcloudUser: data.identity.nextcloudUser } : null,
    mcpUrl: data.info?.mcpUrl ?? null,
    hasMcpCredential: !!data.credentials.mcpAppPassword,
    credentialsUpdatedAt: data.credentialsUpdatedAt,
    connection: data.connection,
  };
}

/** The two worker actions the sync needs; both refuse anyone but a signed-in user. */
export interface Bridge {
  /** The full `Authorization` header value for the agent's MCP app password. */
  revealHeader(): Promise<string>;
  record(rec: ConnectionRecord | null): Promise<void>;
}

export interface ConnectionView {
  id: string;
  name: string;
  status: string;
  enabled: boolean;
  healthStatus: string | null;
  healthMessage: string | null;
}

export interface SyncResult {
  connection: ConnectionView | null;
  /** Why there is no connection, when there is none. */
  note: string | null;
  /** What changed, for the activity line; empty when everything was in place. */
  changes: string[];
}

interface Secret {
  id: string;
  name: string;
  latestVersion?: number;
}

interface Connection {
  id: string;
  name: string;
  status: string;
  enabled: boolean;
  applicationId: string | null;
  config?: Record<string, unknown> | null;
  transportConfig?: Record<string, unknown> | null;
  credentialRefs?: { secretId: string }[] | null;
  healthStatus?: string | null;
  healthMessage?: string | null;
}

interface Application {
  id: string;
  name: string;
  status: string;
}

interface CatalogEntry {
  id: string;
  toolName: string;
  status: string;
  riskLevel: string;
}

export const connectionName = (user: string) => `Nextcloud MCP (${user})`;
export const secretName = (user: string) => `Nextcloud MCP (${user})`;
/** Secret keys allow `[A-Za-z0-9_.-]`; Nextcloud user names here are a subset. */
export const secretKey = (user: string) => `nextcloud-mcp.${user.replace(/[^A-Za-z0-9_.-]/g, "-")}`;

const view = (c: Connection): ConnectionView => ({
  id: c.id,
  name: c.name,
  status: c.status,
  enabled: c.enabled,
  healthStatus: c.healthStatus ?? null,
  healthMessage: c.healthMessage ?? null,
});

/**
 * The tool permissions a new connection starts with. Agents need the write tools to do their
 * job (post in Talk, send mail, save files); anything destructive asks first. Quarantined
 * entries must be reviewed explicitly in the same call.
 */
export function initialPolicy(catalog: CatalogEntry[]) {
  const live = catalog.filter((e) => e.status !== "removed" && e.status !== "disabled");
  const askFirst = new Set(["destructive", "high", "critical"]);
  return {
    enabledCatalogEntryIds: live.filter((e) => !askFirst.has(e.riskLevel)).map((e) => e.id),
    askFirstCatalogEntryIds: live.filter((e) => askFirst.has(e.riskLevel)).map((e) => e.id),
    ...(live.some((e) => e.status === "quarantined")
      ? { reviewedCatalogEntryIds: live.filter((e) => e.status === "quarantined").map((e) => e.id) }
      : {}),
  };
}

/** The connection settings that matter, as Paperclip stores them. */
function connectionSettings(mcpUrl: string, secretId: string) {
  return {
    transportConfig: { url: mcpUrl },
    // The Nextcloud MCP server runs stateful streamable HTTP: without an MCP session, Paperclip's
    // gateway sends a bare `tools/call` and the server answers 400 "Missing session ID".
    config: { url: mcpUrl, mcpSessionRequired: true },
    credentialRefs: [{ name: "Authorization", secretId, placement: "header", key: "Authorization" }],
    // The gateway reads the credential from the connection's grant, not from `credentialRefs`.
    credentialSecretRefs: [{ secretId, configPath: "Authorization" }],
  };
}

function drifted(c: Connection, mcpUrl: string): boolean {
  return c.config?.url !== mcpUrl || c.config?.mcpSessionRequired !== true || c.transportConfig?.url !== mcpUrl;
}

async function listSecrets(api: Api, companyId: string): Promise<Secret[]> {
  return api<Secret[]>("GET", `/companies/${companyId}/secrets`);
}

async function listConnections(api: Api, companyId: string): Promise<Connection[]> {
  return (await api<{ connections: Connection[] }>("GET", `/companies/${companyId}/tools/connections`)).connections;
}

/** The agent's live connection: by recorded id, else by name (an interrupted setup). */
function findConnection(all: Connection[], rec: ConnectionRecord | null, user: string): Connection | undefined {
  const live = all.filter((c) => c.status !== "archived");
  return live.find((c) => c.id === rec?.connectionId) ?? live.find((c) => c.name === connectionName(user));
}

/**
 * Brings the agent's connection in line with its identity. Idempotent: run it whenever the panel
 * opens and after every operation.
 */
export async function sync(api: Api, s: AgentState, bridge: Bridge): Promise<SyncResult> {
  const changes: string[] = [];
  if (!s.identity) {
    if (s.connection) {
      await removeConnection(api, s, bridge);
      changes.push("connection removed");
    }
    return { connection: null, note: null, changes };
  }
  const user = s.identity.nextcloudUser;
  const active = s.identity.state === "active";
  if (!s.mcpUrl) return { connection: null, note: "The server has no MCP URL configured (mcp.url).", changes };

  const existing = findConnection(await listConnections(api, s.companyId), s.connection, user);
  // A disabled identity keeps its connection, switched off; it never gets a new one.
  if (!active && !existing) {
    const note = s.identity.state === "disabled" ? "Created when the ID is re-enabled." : "Created once the ID is active.";
    return { connection: null, note, changes };
  }
  if (!s.hasMcpCredential) {
    return { connection: existing ? view(existing) : null, note: "No MCP app password stored: rotate the credentials.", changes };
  }

  // 1. The secret, holding the current credential.
  const secrets = await listSecrets(api, s.companyId);
  let secret = secrets.find((x) => x.id === s.connection?.secretId) ?? secrets.find((x) => x.name === secretName(user));
  // A secret found by name only (no record) may hold anything: rewrite it.
  let synced = secret && secret.id === s.connection?.secretId ? s.connection.syncedCredentialsAt : null;
  if (!secret) {
    secret = await api<Secret>("POST", `/companies/${s.companyId}/secrets`, {
      name: secretName(user),
      key: secretKey(user),
      value: await bridge.revealHeader(),
      description: `Authorization header of the Nextcloud user ${user} for the Nextcloud MCP server. Managed by the paperclipped-nextcloud plugin.`,
    });
    synced = s.credentialsUpdatedAt;
    changes.push("secret created");
  } else if (synced !== s.credentialsUpdatedAt) {
    await api("POST", `/secrets/${secret.id}/rotate`, { value: await bridge.revealHeader() });
    synced = s.credentialsUpdatedAt;
    changes.push("secret updated");
  }
  // Record as early as possible, so a later failure cannot orphan what exists already.
  const record = async (connectionId: string) => {
    const rec = { connectionId, secretId: secret!.id, syncedCredentialsAt: synced };
    if (JSON.stringify(rec) !== JSON.stringify(s.connection)) await bridge.record(rec);
  };

  // 2. The connection.
  let conn = existing;
  // The credential lives on the connection's grant too; replace rather than patch around it.
  if (conn && conn.credentialRefs?.[0]?.secretId !== secret.id) {
    await api("DELETE", `/tool-connections/${conn.id}`);
    conn = undefined;
    changes.push("connection replaced (new secret)");
  }
  if (conn && drifted(conn, s.mcpUrl)) {
    conn = await api<Connection>("PATCH", `/tool-connections/${conn.id}`, connectionSettings(s.mcpUrl, secret.id));
    changes.push("connection settings updated");
  }
  if (!conn) {
    // Deleting a connection archives its tool application, which keeps the name. Reuse it.
    const apps = (await api<{ applications: Application[] }>("GET", `/companies/${s.companyId}/tools/applications`)).applications;
    const app = apps.find((a) => a.name === connectionName(user));
    if (app && app.status !== "active") await api("PATCH", `/tool-applications/${app.id}`, { status: "active" });
    conn = await api<Connection>("POST", `/companies/${s.companyId}/tools/connections`, {
      name: connectionName(user),
      ...(app ? { applicationId: app.id } : {}),
      transport: "mcp_remote",
      authKind: "api_key",
      ...connectionSettings(s.mcpUrl, secret.id),
    });
    changes.push(app ? "connection created (existing app reused)" : "connection created");
  }
  await record(conn.id);

  // 3. Tools and access, once: a finished setup leaves `draft`.
  if (conn.status === "draft") {
    await api("POST", `/tool-connections/${conn.id}/health-check`, {});
    const { catalog } = await api<{ catalog: CatalogEntry[] }>("POST", `/tool-connections/${conn.id}/catalog/refresh`, {});
    if (!catalog?.length) throw new Error("The Nextcloud MCP server listed no tools for this user");
    await api("POST", `/companies/${s.companyId}/tools/apps/${conn.id}/finish`, {
      access: { agentIds: [s.agentId] },
      ...initialPolicy(catalog),
    });
    changes.push(`${catalog.length} tools granted to this agent`);
    conn = await api<Connection>("GET", `/tool-connections/${conn.id}`);
  }

  // 4. On while the identity is active, off while it is disabled.
  if (conn.enabled !== active) {
    conn = await api<Connection>("PATCH", `/tool-connections/${conn.id}`, { enabled: active });
    changes.push(active ? "connection enabled" : "connection disabled");
  }
  return { connection: view(conn), note: null, changes };
}

/** Deletes the agent's connection and its secret, then forgets them. Idempotent. */
export async function removeConnection(api: Api, s: Pick<AgentState, "companyId" | "connection" | "identity">, bridge: Bridge): Promise<void> {
  const user = s.identity?.nextcloudUser;
  const conns = await listConnections(api, s.companyId);
  const conn = conns.find((c) => c.status !== "archived" && (c.id === s.connection?.connectionId || (user && c.name === connectionName(user))));
  if (conn) await api("DELETE", `/tool-connections/${conn.id}`);
  const secrets = await listSecrets(api, s.companyId);
  const secret = secrets.find((x) => x.id === s.connection?.secretId || (user && x.name === secretName(user)));
  if (secret) await api("DELETE", `/secrets/${secret.id}`);
  if (s.connection) await bridge.record(null);
}
