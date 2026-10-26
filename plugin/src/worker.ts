import { AsyncResource } from "node:async_hooks";

import { definePlugin, runWorker, type PluginContext } from "@paperclipai/plugin-sdk";

import {
  CREDENTIAL_KINDS,
  PcncClient,
  ServerError,
  assertHumanCaller,
  basicAuthorization,
  deriveKey,
  lifecycleStatus,
  mergeCredentials,
  reveal,
  suggestName,
  type CredentialKind,
  type LastOperation,
  type Operation,
  type OperationResult,
  type StoredCredentials,
} from "./core.js";

type SecretRef = Parameters<PluginContext["secrets"]["resolve"]>[0];

interface Config {
  serverUrl?: string;
  apiToken?: SecretRef;
  credentialKey?: SecretRef;
}

// The host allows 30 secret resolutions per minute per company. The rules that follow from it:
// - an action resolves what it needs before it changes anything, so a refused resolution can
//   never strand fresh credentials the server has already issued;
// - the tab's data handler resolves nothing while an operation runs (that is when it polls), and
//   shows masked hints stored at write time instead of unsealing;
// - the lifecycle hook caches its answer briefly.

// State is company-scoped (one key per agent): background work runs outside any host
// invocation, and the host admits such "proactive" calls only when they name a company.
const NS = "paperclipped-nextcloud";
const credentialsKey = (companyId: string, agentId: string) => ({
  scopeKind: "company" as const,
  scopeId: companyId,
  namespace: NS,
  stateKey: `credentials:${agentId}`,
});
const lastOpKey = (companyId: string, agentId: string) => ({
  scopeKind: "company" as const,
  scopeId: companyId,
  namespace: NS,
  stateKey: `lastOperation:${agentId}`,
});

/** Operations run in the background: they can outlast the host's 30 s action timeout. */
const pending = new Map<string, { op: Operation; startedAt: string }>();

/** `cleaning_up` is polled by the host; remember the answer for a while. */
const existence = new Map<string, { exists: boolean; at: number }>();
const EXISTENCE_TTL_MS = 30_000;

/**
 * Runs a function outside the current host invocation. Bound in `setup()`, which is not an
 * invocation, so background work does not inherit an action's invocation scope (that scope
 * expires when the action returns, and host calls made with it are rejected).
 */
let outsideInvocation: (fn: () => void) => void = (fn) => fn();

let context: PluginContext | null = null;

function str(v: unknown, name: string): string {
  if (typeof v !== "string" || v.trim() === "") throw new Error(`${name} is required`);
  return v.trim();
}

function describe(e: unknown): string {
  if (e instanceof ServerError) return `${e.code}: ${e.message}`;
  return e instanceof Error ? e.message : String(e);
}

async function config(ctx: PluginContext, companyId: string): Promise<Config> {
  return (await ctx.config.get(companyId)) as Config;
}

/** One secret resolution. `null` when the plugin is not configured for this company. */
async function client(ctx: PluginContext, companyId: string): Promise<PcncClient | null> {
  const c = await config(ctx, companyId);
  if (!c.serverUrl || !c.apiToken) return null;
  const token = await ctx.secrets.resolve(c.apiToken, { companyId, configPath: "apiToken" });
  return new PcncClient(c.serverUrl, token);
}

/** One secret resolution. */
async function vaultKey(ctx: PluginContext, companyId: string): Promise<Buffer> {
  const c = await config(ctx, companyId);
  if (!c.credentialKey) throw new Error("The plugin's credential encryption key is not configured");
  return deriveKey(await ctx.secrets.resolve(c.credentialKey, { companyId, configPath: "credentialKey" }));
}

async function requireAgent(ctx: PluginContext, agentId: string, companyId: string) {
  const agent = await ctx.agents.get(agentId, companyId);
  if (!agent) throw new Error("Agent not found in this company");
  return agent;
}

async function storedCredentials(ctx: PluginContext, companyId: string, agentId: string) {
  return ((await ctx.state.get(credentialsKey(companyId, agentId))) as StoredCredentials | null) ?? null;
}

/**
 * Starts `op` in the background. `c` and `key` are resolved by the caller, inside the action,
 * before anything changes on the server.
 */
function start(
  ctx: PluginContext,
  companyId: string,
  agentId: string,
  op: Operation,
  c: PcncClient,
  key: Buffer,
  run: (c: PcncClient) => Promise<OperationResult>,
) {
  if (pending.has(agentId)) throw new Error(`A ${pending.get(agentId)!.op} operation is still running`);
  pending.set(agentId, { op, startedAt: new Date().toISOString() });
  outsideInvocation(
    () =>
      void (async () => {
        let last: LastOperation;
        try {
          const result = await run(c);
          const now = new Date().toISOString();
          if (op === "purge") {
            existence.set(agentId, { exists: false, at: Date.now() });
            await ctx.state.delete(credentialsKey(companyId, agentId));
          } else {
            existence.set(agentId, { exists: true, at: Date.now() });
            if (result.credentials && Object.keys(result.credentials).length > 0 && result.identity) {
              const stored = await storedCredentials(ctx, companyId, agentId);
              const merged = mergeCredentials(key, agentId, result.identity.nextcloudUser, stored, result.credentials, now);
              await ctx.state.set(credentialsKey(companyId, agentId), merged);
            }
          }
          last = { op, ok: true, finishedAt: now, steps: result.steps };
        } catch (e) {
          ctx.logger.warn(`${op} failed for agent ${agentId}: ${describe(e)}`);
          last = { op, ok: false, finishedAt: new Date().toISOString(), error: describe(e) };
        } finally {
          pending.delete(agentId);
        }
        await ctx.state
          .set(lastOpKey(companyId, agentId), last)
          .catch((e) => ctx.logger.warn(`cannot record the ${op} result: ${describe(e)}`));
      })(),
  );
}

const plugin = definePlugin({
  async setup(ctx) {
    context = ctx;
    outsideInvocation = AsyncResource.bind((fn: () => void) => fn());

    // Status for the Identity tab. Contains no secrets: data handlers do not know their caller.
    ctx.data.register("identity", async (params) => {
      const agentId = str(params.agentId, "agentId");
      const companyId = str(params.companyId, "companyId");
      const agent = await requireAgent(ctx, agentId, companyId);
      const stored = await storedCredentials(ctx, companyId, agentId);
      const base = {
        agent: { name: agent.name },
        suggestedName: suggestName(agent.name),
        pending: pending.get(agentId) ?? null,
        lastOperation: ((await ctx.state.get(lastOpKey(companyId, agentId))) as LastOperation | null) ?? null,
        credentials: stored?.hints ?? {},
        credentialsUpdatedAt: stored?.updatedAt ?? null,
      };
      // The tab polls while an operation runs: answer without touching secrets or the server.
      if (base.pending) return { ...base, configured: true, error: null, polling: true };
      let c: PcncClient | null;
      try {
        c = await client(ctx, companyId);
      } catch (e) {
        return { ...base, configured: false, error: describe(e) };
      }
      if (!c) return { ...base, configured: false, error: null };
      try {
        const [info, identity] = await Promise.all([c.info(), c.get(agentId)]);
        existence.set(agentId, { exists: identity !== null, at: Date.now() });
        return { ...base, configured: true, error: null, info, identity, credentials: identity ? base.credentials : {} };
      } catch (e) {
        return { ...base, configured: true, error: describe(e) };
      }
    });

    // Every agent of the company with its identity, for the company page. No secrets.
    ctx.data.register("overview", async (params) => {
      const companyId = str(params.companyId, "companyId");
      const agents = await ctx.agents.list({ companyId, limit: 500 });
      const rows = (identities: Map<string, unknown>) =>
        agents
          .map((a) => ({ id: a.id, name: a.name, status: a.status, identity: identities.get(a.id) ?? null, pending: pending.get(a.id) ?? null }))
          .sort((x, y) => x.name.localeCompare(y.name));
      let c: PcncClient | null;
      try {
        c = await client(ctx, companyId);
      } catch (e) {
        return { configured: false, error: describe(e), agents: rows(new Map()) };
      }
      if (!c) return { configured: false, error: null, agents: rows(new Map()) };
      try {
        const [info, identities] = await Promise.all([c.info(), c.list()]);
        return { configured: true, error: null, info, agents: rows(new Map(identities.map((i) => [i.externalId, i]))) };
      } catch (e) {
        return { configured: true, error: describe(e), agents: rows(new Map()) };
      }
    });

    const action = (op: Operation, run: (c: PcncClient, agentId: string, params: Record<string, unknown>) => Promise<OperationResult>) =>
      ctx.actions.register(op, async (params, actx) => {
        assertHumanCaller(actx);
        const agentId = str(params.agentId, "agentId");
        const companyId = str(actx.companyId ?? params.companyId, "companyId");
        await requireAgent(ctx, agentId, companyId);
        const c = await client(ctx, companyId);
        if (!c) throw new Error("The plugin is not configured for this company");
        const key = await vaultKey(ctx, companyId);
        start(ctx, companyId, agentId, op, c, key, (cl) => run(cl, agentId, params));
        return { started: true };
      });

    action("provision", (c, agentId, p) =>
      c.provision(agentId, {
        name: str(p.name, "name"),
        displayName: str(p.displayName, "displayName"),
        sharedMailboxes: Array.isArray(p.sharedMailboxes) ? p.sharedMailboxes.map(String) : [],
      }),
    );
    action("disable", (c, agentId) => c.disable(agentId));
    action("enable", (c, agentId) => c.enable(agentId));
    action("rotate", (c, agentId) => c.rotate(agentId));
    action("purge", (c, agentId) => c.purge(agentId));

    // The only way a stored credential leaves the plugin: to a signed-in user, for copying.
    ctx.actions.register("reveal", async (params, actx) => {
      assertHumanCaller(actx);
      const agentId = str(params.agentId, "agentId");
      const companyId = str(actx.companyId ?? params.companyId, "companyId");
      await requireAgent(ctx, agentId, companyId);
      const kind = str(params.kind, "kind");
      if (kind !== "mcpAuthorization" && !(CREDENTIAL_KINDS as string[]).includes(kind)) {
        throw new Error(`Unknown credential ${kind}`);
      }
      const stored = await storedCredentials(ctx, companyId, agentId);
      if (!stored) throw new Error("No credentials stored for this agent");
      const key = await vaultKey(ctx, companyId);
      let value: string | null;
      if (kind === "mcpAuthorization") {
        const mcp = reveal(key, agentId, stored, "mcpAppPassword");
        value = mcp && stored.user ? basicAuthorization(stored.user, mcp) : null;
      } else {
        value = reveal(key, agentId, stored, kind as CredentialKind);
      }
      if (!value) throw new Error("No such credential stored for this agent");
      ctx.logger.info(`credential ${kind} of agent ${agentId} revealed to user ${actx.actor.userId ?? "?"}`);
      return { value };
    });
  },

  async onAgentLifecycle(req) {
    const result = (status: "complete" | "pending") => ({ operationId: req.operationId, version: req.version, status });
    if (req.phase !== "cleaning_up" || !context) return result("complete");
    const cached = existence.get(req.agentId);
    if (cached && Date.now() - cached.at < EXISTENCE_TTL_MS) return result(lifecycleStatus(req.phase, cached.exists));
    let c: PcncClient | null;
    try {
      c = await client(context, req.companyId);
    } catch {
      return result("pending");
    }
    // Not configured for this company: nothing can have been provisioned through it.
    if (!c) return result("complete");
    try {
      const exists = (await c.get(req.agentId)) !== null;
      existence.set(req.agentId, { exists, at: Date.now() });
      return result(lifecycleStatus(req.phase, exists));
    } catch {
      return result(lifecycleStatus(req.phase, "unknown"));
    }
  },

  async onValidateConfig(config) {
    const errors: string[] = [];
    try {
      const u = new URL(String((config as Config).serverUrl));
      if (!["http:", "https:"].includes(u.protocol)) errors.push("Server URL must be http or https");
    } catch {
      errors.push("Server URL is not a valid URL");
    }
    return { ok: errors.length === 0, errors };
  },

  async onHealth() {
    return { status: "ok", message: `${pending.size} operation(s) running` };
  },
});

export default plugin;
runWorker(plugin, import.meta.url);
