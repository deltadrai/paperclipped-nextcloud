// Pure logic of the plugin: the paperclipped-nextcloud server client, sealing of credentials,
// masking and naming. No Paperclip SDK imports, so everything here is unit-testable.

import { createCipheriv, createDecipheriv, createHash, randomBytes } from "node:crypto";

export type IdentityState = "provisioning" | "active" | "disabled";

export interface ServerIdentity {
  externalId: string;
  name: string;
  address: string;
  displayName: string;
  state: IdentityState;
  sharedMailboxes: string[];
  nextcloudUser: string;
  createdAt: string;
  updatedAt: string;
}

export interface SharedMailboxInfo {
  key: string;
  address: string;
  label: string;
}

export interface ServerInfo {
  version: string;
  domain: string;
  nextcloudUrl: string;
  group: string;
  mcpUrl: string | null;
  sharedMailboxes: SharedMailboxInfo[];
}

export interface Step {
  step: string;
  outcome: "created" | "updated" | "unchanged" | "removed";
}

export interface ServerCredentials {
  nextcloudLoginPassword?: string;
  mcpAppPassword?: string;
}

export interface OperationResult {
  identity: ServerIdentity | null;
  steps: Step[];
  credentials?: ServerCredentials;
}

export interface ProvisionBody {
  name: string;
  displayName: string;
  sharedMailboxes: string[];
}

export class ServerError extends Error {
  constructor(
    readonly status: number,
    readonly code: string,
    message: string,
  ) {
    super(message);
    this.name = "ServerError";
  }
}

type Fetch = typeof fetch;

/**
 * Client for the paperclipped-nextcloud HTTP API.
 *
 * Uses Node's own `fetch`, not the host's `ctx.http`: the host refuses private address ranges
 * (an SSRF guard for user-supplied URLs), while this server usually sits on a private network.
 * The URL comes from the operator's plugin settings only, never from users or agents.
 */
export class PcncClient {
  private readonly base: string;

  constructor(
    baseUrl: string,
    private readonly token: string,
    private readonly fetchImpl: Fetch = fetch,
    private readonly timeoutMs = 180_000,
  ) {
    this.base = baseUrl.replace(/\/+$/, "");
  }

  private async request<T>(method: string, path: string, body?: unknown): Promise<T> {
    const res = await this.fetchImpl(`${this.base}${path}`, {
      method,
      headers: {
        Authorization: `Bearer ${this.token}`,
        ...(body === undefined ? {} : { "Content-Type": "application/json" }),
      },
      body: body === undefined ? undefined : JSON.stringify(body),
      signal: AbortSignal.timeout(this.timeoutMs),
    });
    const text = await res.text();
    let json: unknown = null;
    try {
      json = text ? JSON.parse(text) : null;
    } catch {
      json = null;
    }
    if (!res.ok) {
      const err = (json ?? {}) as { error?: string; message?: string };
      throw new ServerError(res.status, err.error ?? `http_${res.status}`, err.message ?? (text.slice(0, 200) || res.statusText));
    }
    return json as T;
  }

  private path(externalId: string, suffix = ""): string {
    return `/v1/identities/${encodeURIComponent(externalId)}${suffix}`;
  }

  info(): Promise<ServerInfo> {
    return this.request("GET", "/v1/info");
  }

  list(): Promise<ServerIdentity[]> {
    return this.request("GET", "/v1/identities");
  }

  async get(externalId: string): Promise<ServerIdentity | null> {
    try {
      return await this.request<ServerIdentity>("GET", this.path(externalId));
    } catch (e) {
      if (e instanceof ServerError && e.status === 404) return null;
      throw e;
    }
  }

  provision(externalId: string, body: ProvisionBody): Promise<OperationResult> {
    return this.request("PUT", this.path(externalId), body);
  }

  disable(externalId: string): Promise<OperationResult> {
    return this.request("POST", this.path(externalId, "/disable"));
  }

  enable(externalId: string): Promise<OperationResult> {
    return this.request("POST", this.path(externalId, "/enable"));
  }

  rotate(externalId: string): Promise<OperationResult> {
    return this.request("POST", this.path(externalId, "/rotate"));
  }

  purge(externalId: string): Promise<OperationResult> {
    return this.request("DELETE", this.path(externalId));
  }
}

// ------------------------------------------------------------------ credentials at rest

/** Any operator-chosen secret string becomes a 256-bit key. */
export function deriveKey(secret: string): Buffer {
  if (secret.trim().length < 16) throw new Error("credential key must be at least 16 characters");
  return createHash("sha256").update(secret.trim()).digest();
}

/** AES-256-GCM; `aad` binds the value to its agent and field. Format: `v1:` + base64(iv|tag|ct). */
export function seal(key: Buffer, aad: string, plaintext: string): string {
  const iv = randomBytes(12);
  const cipher = createCipheriv("aes-256-gcm", key, iv);
  cipher.setAAD(Buffer.from(aad));
  const ct = Buffer.concat([cipher.update(plaintext, "utf8"), cipher.final()]);
  return `v1:${Buffer.concat([iv, cipher.getAuthTag(), ct]).toString("base64")}`;
}

export function open(key: Buffer, aad: string, sealed: string): string {
  if (!sealed.startsWith("v1:")) throw new Error("unknown sealed format");
  const raw = Buffer.from(sealed.slice(3), "base64");
  const decipher = createDecipheriv("aes-256-gcm", key, raw.subarray(0, 12));
  decipher.setAAD(Buffer.from(aad));
  decipher.setAuthTag(raw.subarray(12, 28));
  return Buffer.concat([decipher.update(raw.subarray(28)), decipher.final()]).toString("utf8");
}

export type CredentialKind = "nextcloudLoginPassword" | "mcpAppPassword";
export const CREDENTIAL_KINDS: CredentialKind[] = ["nextcloudLoginPassword", "mcpAppPassword"];

export type HintKind = CredentialKind | "mcpAuthorization";

/**
 * What the plugin keeps per agent: sealed values, plus masked hints and the Nextcloud user so
 * the tab can show them without unsealing anything (each unseal costs a rate-limited secret
 * resolution of the key).
 */
export type StoredCredentials = Partial<Record<CredentialKind, string>> & {
  hints?: Partial<Record<HintKind, string>>;
  user?: string;
  updatedAt?: string;
};

export function mergeCredentials(
  key: Buffer,
  agentId: string,
  user: string,
  stored: StoredCredentials | null,
  fresh: ServerCredentials | undefined,
  now: string,
): StoredCredentials {
  const out: StoredCredentials = { ...(stored ?? {}), hints: { ...(stored?.hints ?? {}) }, user };
  let changed = false;
  for (const kind of CREDENTIAL_KINDS) {
    const value = fresh?.[kind];
    if (value) {
      out[kind] = seal(key, `${agentId}/${kind}`, value);
      out.hints![kind] = mask(value);
      if (kind === "mcpAppPassword") out.hints!.mcpAuthorization = `Basic ${mask(basicAuthorization(user, value).slice(6))}`;
      changed = true;
    }
  }
  if (changed) out.updatedAt = now;
  return out;
}

export function reveal(key: Buffer, agentId: string, stored: StoredCredentials | null, kind: CredentialKind): string | null {
  const sealed = stored?.[kind];
  return sealed ? open(key, `${agentId}/${kind}`, sealed) : null;
}

/** `Xk3f…a9Q2`: enough to recognise a value, too little to use it. */
export function mask(value: string): string {
  return value.length <= 12 ? "••••••" : `${value.slice(0, 4)}…${value.slice(-4)}`;
}

/** The `Authorization` header an MCP client sends for this worker. */
export function basicAuthorization(user: string, password: string): string {
  return `Basic ${Buffer.from(`${user}:${password}`, "utf8").toString("base64")}`;
}

// ------------------------------------------------------------------ naming and guards

/** "Ada Lovelace" → "ada.lovelace"; mirrors the server's suggestion. */
export function suggestName(display: string): string {
  const map: Record<string, string> = { ä: "a", ö: "o", ü: "u", ß: "s" };
  let out = "";
  for (const raw of display.trim().toLowerCase()) {
    const c = map[raw] ?? (/[a-z0-9]/.test(raw) ? raw : ".");
    if (c === "." && (out === "" || out.endsWith("."))) continue;
    out += c;
  }
  out = out.replace(/\.+$/, "").slice(0, 48);
  return /^[a-z]/.test(out) ? out : `agent.${out}`.replace(/\.+$/, "");
}

export interface ActorLike {
  actor: { type: "user" | "agent" | "system" };
}

/**
 * Identities are created and destroyed by people only. Agents can call plugin actions too, so
 * every action checks its caller.
 */
export function assertHumanCaller(ctx: ActorLike): void {
  if (ctx.actor.type !== "user") {
    throw new Error("Only a signed-in user can manage Nextcloud identities");
  }
}

export type Operation = "provision" | "disable" | "enable" | "rotate" | "purge";

export interface LastOperation {
  op: Operation;
  ok: boolean;
  finishedAt: string;
  steps?: Step[];
  error?: string;
}

/** `cleaning_up` waits while an identity exists; every other phase completes at once. */
export function lifecycleStatus(phase: string, identityExists: boolean | "unknown"): "complete" | "pending" {
  if (phase !== "cleaning_up") return "complete";
  return identityExists === false ? "complete" : "pending";
}
