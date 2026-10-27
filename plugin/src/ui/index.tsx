import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import {
  copyTextToClipboard,
  useHostNavigation,
  usePluginAction,
  usePluginData,
  usePluginToast,
  type PluginDetailTabProps,
  type PluginPageProps,
  type PluginSidebarProps,
} from "@paperclipai/plugin-sdk/ui";

import { agentState, removeConnection, restApi, sync, type Bridge, type ConnectionRecord, type SyncResult } from "./mcp-connection.js";

// Styling uses the host's Tailwind classes: plugin UI runs same-origin inside Paperclip.

const PAGE_ROUTE = "nextcloud-identities";

interface Info {
  domain: string;
  nextcloudUrl: string;
  group: string;
  mcpUrl: string | null;
  sharedMailboxes: { key: string; address: string; label: string }[];
}

interface Identity {
  name: string;
  address: string;
  displayName: string;
  state: "provisioning" | "active" | "disabled";
  sharedMailboxes: string[];
  nextcloudUser: string;
}

type Hints = Partial<Record<"nextcloudLoginPassword" | "mcpAppPassword" | "mcpAuthorization", string>>;

// Mirrors the worker's `identity` payload. Contains no secrets, only masked hints.
interface IdentityData {
  agent: { name: string };
  suggestedName: string;
  configured: boolean;
  error: string | null;
  pending: { op: string; startedAt: string } | null;
  lastOperation: { op: string; ok: boolean; finishedAt: string; error?: string } | null;
  info?: Info;
  identity?: Identity | null;
  credentials: Hints;
  credentialsUpdatedAt: string | null;
  connection: ConnectionRecord | null;
}

interface OverviewData {
  configured: boolean;
  error: string | null;
  info?: Info;
  agents: { id: string; name: string; status: string; identity: Identity | null; pending: { op: string } | null; mcpConnected: boolean }[];
}

const OP_LABEL: Record<string, string> = {
  provision: "Creating the Nextcloud ID",
  disable: "Deleting (disabling) the Nextcloud ID",
  enable: "Re-enabling the Nextcloud ID",
  rotate: "Rotating credentials",
  purge: "Purging the Nextcloud ID",
};

type Variant = "default" | "primary" | "danger";

function Button(props: { onClick: () => void; children: ReactNode; variant?: Variant; disabled?: boolean; title?: string }) {
  const variant = {
    default: "border border-border bg-background hover:bg-accent",
    primary: "bg-primary text-primary-foreground hover:bg-primary/90",
    danger: "bg-destructive text-white hover:bg-destructive/90",
  }[props.variant ?? "default"];
  return (
    <button
      type="button"
      title={props.title}
      disabled={props.disabled}
      onClick={props.onClick}
      className={`inline-flex items-center gap-1.5 rounded-md px-3 py-1.5 text-sm font-medium transition-colors disabled:pointer-events-none disabled:opacity-50 ${variant}`}
    >
      {props.children}
    </button>
  );
}

// Lucide icons (ISC), the set Paperclip itself uses. All are drawn in `currentColor`, so they
// follow Paperclip's light and dark themes. Not the Nextcloud logo: Nextcloud's trademark
// guidelines allow it only as a link to nextcloud.com.
const ICONS = {
  copy: <><rect width="14" height="14" x="8" y="8" rx="2" ry="2" /><path d="M4 16c-1.1 0-2-.9-2-2V4c0-1.1.9-2 2-2h10c1.1 0 2 .9 2 2" /></>,
  rotate: <><path d="M21 12a9 9 0 1 1-9-9c2.52 0 4.93 1 6.74 2.74L21 8" /><path d="M21 3v5h-5" /></>,
  trash: <><path d="M3 6h18" /><path d="M19 6v14c0 1-1 2-2 2H7c-1 0-2-1-2-2V6" /><path d="M8 6V4c0-1 1-2 2-2h4c1 0 2 1 2 2v2" /><path d="M10 11v6" /><path d="M14 11v6" /></>,
  userPlus: <><path d="M16 21v-2a4 4 0 0 0-4-4H6a4 4 0 0 0-4 4v2" /><circle cx="9" cy="7" r="4" /><path d="M19 8v6" /><path d="M22 11h-6" /></>,
  undo: <><path d="M3 12a9 9 0 1 0 9-9 9.75 9.75 0 0 0-6.74 2.74L3 8" /><path d="M3 3v5h5" /></>,
  check: <path d="M20 6 9 17l-5-5" />,
  manage: <><path d="M20 7h-9" /><path d="M14 17H5" /><circle cx="17" cy="17" r="3" /><circle cx="7" cy="7" r="3" /></>,
  close: <><path d="M18 6 6 18" /><path d="m6 6 12 12" /></>,
  cloud: <path d="M17.5 19H9a7 7 0 1 1 6.71-9h1.79a4.5 4.5 0 1 1 0 9Z" />,
  refresh: <><path d="M3 12a9 9 0 0 1 9-9 9.75 9.75 0 0 1 6.74 2.74L21 8" /><path d="M21 3v5h-5" /><path d="M21 12a9 9 0 0 1-9 9 9.75 9.75 0 0 1-6.74-2.74L3 16" /><path d="M8 16H3v5" /></>,
  external: <><path d="M15 3h6v6" /><path d="M10 14 21 3" /><path d="M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6" /></>,
  plug: <><path d="M12 22v-5" /><path d="M9 8V2" /><path d="M15 8V2" /><path d="M18 8v5a4 4 0 0 1-4 4h-4a4 4 0 0 1-4-4V8Z" /></>,
};

function Icon({ name, className = "h-4 w-4" }: { name: keyof typeof ICONS; className?: string }) {
  return (
    <svg viewBox="0 0 24 24" className={`shrink-0 ${className}`} fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
      {ICONS[name]}
    </svg>
  );
}

/** Icon-only button; the label is its tooltip and accessible name, as in Paperclip. */
function IconButton(props: { icon: keyof typeof ICONS; label: string; onClick: () => void; disabled?: boolean }) {
  return (
    <button
      type="button"
      aria-label={props.label}
      title={props.label}
      disabled={props.disabled}
      onClick={props.onClick}
      className="inline-flex h-8 w-8 items-center justify-center rounded-md border border-border bg-background transition-colors hover:bg-accent disabled:pointer-events-none disabled:opacity-50"
    >
      <Icon name={props.icon} />
    </button>
  );
}

/** A value that copies on click, like Paperclip's own keys and hashes. */
function CopyValue(props: { value: string; label: string; onCopy: () => void }) {
  return (
    <button
      type="button"
      aria-label={`Copy ${props.label}`}
      title={`Copy ${props.label}`}
      onClick={props.onCopy}
      className="inline-flex cursor-copy items-center gap-1.5 rounded bg-muted px-1.5 py-0.5 font-mono text-[13px] text-foreground/90 transition-colors hover:text-foreground"
    >
      <span>{props.value}</span>
      <Icon name="copy" className="h-3.5 w-3.5 opacity-60" />
    </button>
  );
}

function Card(props: { title: string; children: ReactNode; tone?: "danger"; action?: ReactNode }) {
  return (
    <section className={`rounded-lg border p-4 ${props.tone === "danger" ? "border-destructive/40" : "border-border"}`}>
      <div className="mb-3 flex items-center justify-between gap-2">
        <h3 className="text-sm font-semibold">{props.title}</h3>
        {props.action}
      </div>
      <div className="flex flex-col gap-3">{props.children}</div>
    </section>
  );
}

function Row(props: { label: string; children: ReactNode }) {
  return (
    <div className="flex flex-wrap items-center gap-2">
      <span className="w-48 shrink-0 text-xs text-muted-foreground">{props.label}</span>
      {props.children}
    </div>
  );
}

const mono = "font-mono text-[13px]";
const muted = "text-sm text-muted-foreground";
const input = "rounded-md border border-border bg-background px-2 py-1 text-sm min-w-56";

function StateBadge({ identity, pending }: { identity: Identity | null | undefined; pending?: { op: string } | null }) {
  const [label, cls] = pending
    ? ["working…", "bg-amber-500/15 text-amber-700 dark:text-amber-400"]
    : !identity
      ? ["no ID", "bg-muted text-muted-foreground"]
      : identity.state === "disabled"
        ? ["disabled", "bg-destructive/15 text-destructive"]
        : identity.state === "active"
          ? ["active", "bg-emerald-500/15 text-emerald-700 dark:text-emerald-400"]
          : ["provisioning", "bg-amber-500/15 text-amber-700 dark:text-amber-400"];
  return <span className={`rounded-full px-2 py-0.5 text-xs font-medium ${cls}`}>{label}</span>;
}

/** Plugin actions reject with plain `{ code, message }` objects, not `Error`s. */
function errorText(e: unknown): string {
  if (e instanceof Error) return e.message;
  if (e && typeof e === "object" && typeof (e as { message?: unknown }).message === "string") return (e as { message: string }).message;
  return String(e);
}

// ------------------------------------------------------------------ slot exports

/** Agent detail tab (rendered by Paperclip releases after 2026.1023.0). */
export function IdentityTab({ context }: PluginDetailTabProps) {
  return <IdentityPanel agentId={context.entityId} companyId={context.companyId} />;
}

export function IdentitiesSidebarLink(_: PluginSidebarProps) {
  const nav = useHostNavigation();
  const href = nav.resolveHref(`/${PAGE_ROUTE}`);
  const active = typeof window !== "undefined" && window.location.pathname === href;
  return (
    <a
      {...nav.linkProps(`/${PAGE_ROUTE}`)}
      aria-current={active ? "page" : undefined}
      className={`mx-2 flex items-center gap-2.5 rounded-lg px-2 py-1.5 text-(length:--text-compact) font-medium transition-colors pointer-coarse:py-1 ${
        active ? "bg-sidebar-accent text-sidebar-accent-foreground" : "text-foreground/80 hover:bg-sidebar-accent hover:text-sidebar-accent-foreground"
      }`}
    >
      <Icon name="cloud" />
      <span className="flex-1 truncate">Nextcloud identities</span>
    </a>
  );
}

/** Company page: every agent with its identity, and the panel for the selected one. */
export function IdentitiesPage({ context }: PluginPageProps) {
  const companyId = context.companyId;
  const { data, loading, error, refresh } = usePluginData<OverviewData>("overview", { companyId });
  const [selected, setSelected] = useState<string | null>(null);
  if (!companyId) return <div className={muted}>Select a company.</div>;
  if (loading && !data) return <div className={muted}>Loading…</div>;
  if (error && !data) return <div className="text-sm text-destructive">Could not load: {error.message}</div>;
  if (!data) return null;
  return (
    <div className="mx-auto flex max-w-5xl flex-col gap-6 p-6">
      <header>
        <h1 className="flex items-center gap-2 text-lg font-semibold">
          <Icon name="cloud" className="h-5 w-5" />
          Nextcloud identities
        </h1>
        <p className={muted}>
          Agents with their own Nextcloud user{data.info ? ` in the group "${data.info.group}"` : ""}, mailbox and Nextcloud MCP access.
        </p>
      </header>
      {!data.configured && <NotConfigured error={data.error} />}
      {data.configured && data.error && <div className="text-sm text-destructive">{data.error}</div>}
      <div className="overflow-hidden rounded-lg border border-border">
        <table className="w-full text-sm">
          <thead className="bg-muted/50 text-left text-xs text-muted-foreground">
            <tr>
              <th className="px-3 py-2 font-medium">Agent</th>
              <th className="px-3 py-2 font-medium">Nextcloud user</th>
              <th className="px-3 py-2 font-medium">Email</th>
              <th className="px-3 py-2 font-medium">ID</th>
              <th className="px-3 py-2 font-medium">MCP</th>
              <th className="px-3 py-2" />
            </tr>
          </thead>
          <tbody>
            {data.agents.map((a) => (
              <tr key={a.id} className={`border-t border-border ${selected === a.id ? "bg-accent/40" : ""}`}>
                <td className="px-3 py-2">
                  {a.name}
                  {a.status === "terminated" && <span className="ml-2 text-xs text-muted-foreground">(terminated)</span>}
                </td>
                <td className={`px-3 py-2 ${mono}`}>{a.identity?.nextcloudUser ?? "—"}</td>
                <td className={`px-3 py-2 ${mono}`}>{a.identity?.address ?? "—"}</td>
                <td className="px-3 py-2">
                  <StateBadge identity={a.identity} pending={a.pending} />
                </td>
                <td className="px-3 py-2">
                  {a.mcpConnected ? (
                    <span title="Connected to the Nextcloud MCP server in Paperclip" aria-label="MCP connected" className="inline-flex text-emerald-700 dark:text-emerald-400">
                      <Icon name="plug" />
                    </span>
                  ) : (
                    <span className={muted}>—</span>
                  )}
                </td>
                <td className="px-3 py-2 text-right">
                  <IconButton
                    icon={selected === a.id ? "close" : "manage"}
                    label={selected === a.id ? "Close" : `Manage the Nextcloud ID of ${a.name}`}
                    onClick={() => setSelected(selected === a.id ? null : a.id)}
                  />
                </td>
              </tr>
            ))}
            {data.agents.length === 0 && (
              <tr>
                <td colSpan={6} className={`px-3 py-6 text-center ${muted}`}>
                  No agents in this company yet.
                </td>
              </tr>
            )}
          </tbody>
        </table>
      </div>
      {selected && (
        <div>
          <h2 className="mb-3 text-base font-semibold">{data.agents.find((a) => a.id === selected)?.name}</h2>
          <IdentityPanel key={selected} agentId={selected} companyId={companyId} onChange={refresh} />
        </div>
      )}
    </div>
  );
}

// ------------------------------------------------------------------ the panel

function NotConfigured({ error }: { error: string | null }) {
  return (
    <Card title="Not configured">
      <p className={muted}>
        Set the server URL, API token and credential key for this company under Settings → Plugins → Nextcloud identity.
      </p>
      {error && <p className="text-sm text-destructive">{error}</p>}
    </Card>
  );
}

function IdentityPanel({ agentId, companyId, onChange }: { agentId: string; companyId: string | null; onChange?: () => void }) {
  const { data, loading, error, refresh } = usePluginData<IdentityData>("identity", { agentId, companyId });
  const toast = usePluginToast();
  const actions = {
    provision: usePluginAction("provision"),
    disable: usePluginAction("disable"),
    enable: usePluginAction("enable"),
    rotate: usePluginAction("rotate"),
    purge: usePluginAction("purge"),
    reveal: usePluginAction("reveal"),
  };
  const busy = !!data?.pending;
  const mcp = useMcpConnection(data, agentId, companyId, refresh, onChange);

  // Poll while an operation runs in the background; tell the page when it finishes.
  useEffect(() => {
    if (!busy) return;
    const t = setInterval(refresh, 2000);
    return () => {
      clearInterval(t);
      onChange?.();
    };
  }, [busy, refresh]);

  async function run(op: Exclude<keyof typeof actions, "reveal">, params: Record<string, unknown> = {}) {
    try {
      // A purged ID's connection would point at a deleted user: remove it first.
      if (op === "purge") await mcp.remove();
      await actions[op]({ agentId, companyId, ...params });
      refresh();
      onChange?.();
    } catch (e) {
      toast({ title: "Action failed", body: errorText(e), tone: "error" });
    }
  }

  async function copy(kind: string, what: string) {
    try {
      const res = (await actions.reveal({ agentId, companyId, kind })) as { value: string };
      await copyTextToClipboard(res.value);
      toast({ title: `${what} copied`, tone: "success", ttlMs: 2500 });
    } catch (e) {
      toast({ title: "Copy failed", body: errorText(e), tone: "error" });
    }
  }

  if (loading && !data) return <div className={muted}>Loading…</div>;
  if (error && !data) return <div className="text-sm text-destructive">Could not load: {error.message}</div>;
  if (!data) return null;
  if (!data.configured) return <NotConfigured error={data.error} />;

  const last = data.lastOperation;
  return (
    <div className="flex max-w-3xl flex-col gap-4">
      {busy && (
        <div className="rounded-lg border border-amber-500/40 bg-amber-500/10 px-4 py-3 text-sm">
          {OP_LABEL[data.pending!.op] ?? data.pending!.op}… This can take up to a minute.
        </div>
      )}
      {!busy && last && !last.ok && (
        <div className="rounded-lg border border-destructive/40 bg-destructive/10 px-4 py-3 text-sm">
          {OP_LABEL[last.op] ?? last.op} failed: {last.error}. Every action is safe to repeat.
        </div>
      )}
      {data.error && <div className="text-sm text-destructive">{data.error}</div>}

      {!data.identity && !busy ? (
        <CreateForm data={data} busy={busy} onCreate={(p) => run("provision", p)} />
      ) : data.identity?.state === "disabled" ? (
        <>
          <DisabledCard identity={data.identity} busy={busy} onEnable={() => run("enable")} onPurge={() => run("purge")} />
          <McpCard data={data} mcp={mcp} busy={busy} />
        </>
      ) : data.identity ? (
        <ActiveCards data={data} identity={data.identity} busy={busy} run={run} copy={copy} mcp={mcp} />
      ) : null}
    </div>
  );
}

function SharedMailboxPicker(props: { info?: Info; value: string[]; onChange: (v: string[]) => void; disabled?: boolean }) {
  if (!props.info?.sharedMailboxes.length) return null;
  return (
    <div className="flex flex-wrap gap-4">
      {props.info.sharedMailboxes.map((m) => (
        <label key={m.key} className="flex items-center gap-1.5 text-sm">
          <input
            type="checkbox"
            disabled={props.disabled}
            checked={props.value.includes(m.key)}
            onChange={(e) => props.onChange(e.target.checked ? [...props.value, m.key] : props.value.filter((k) => k !== m.key))}
          />
          <span className={mono}>{m.address}</span>
        </label>
      ))}
    </div>
  );
}

function CreateForm(props: { data: IdentityData; busy: boolean; onCreate: (p: Record<string, unknown>) => void }) {
  const { data } = props;
  const [name, setName] = useState(data.suggestedName);
  const [displayName, setDisplayName] = useState(data.agent.name);
  const [shared, setShared] = useState<string[]>([]);
  return (
    <Card title="No Nextcloud ID yet">
      <p className={muted}>
        Creates a Nextcloud user in the group <b>{data.info?.group}</b> with its own mailbox, already set up in Nextcloud Mail, plus the group's
        team folder and Talk lobby.
      </p>
      <Row label="Name">
        <input className={input} value={name} onChange={(e) => setName(e.target.value.toLowerCase())} />
        <span className={mono}>@{data.info?.domain}</span>
      </Row>
      <Row label="Display name">
        <input className={input} value={displayName} onChange={(e) => setDisplayName(e.target.value)} />
      </Row>
      {(data.info?.sharedMailboxes.length ?? 0) > 0 && (
        <Row label="Shared mailboxes">
          <SharedMailboxPicker info={data.info} value={shared} onChange={setShared} />
        </Row>
      )}
      <div>
        <Button variant="primary" disabled={props.busy || !name || !displayName} onClick={() => props.onCreate({ name, displayName, sharedMailboxes: shared })}>
          <Icon name="userPlus" />
          Create Nextcloud ID
        </Button>
      </div>
    </Card>
  );
}

function Secret(props: { label: string; copyLabel: string; hint?: string; onCopy: () => void }) {
  return (
    <Row label={props.label}>
      {props.hint ? (
        <CopyValue value={props.hint} label={props.copyLabel} onCopy={props.onCopy} />
      ) : (
        <span className={muted}>not stored. Rotate the credentials to get a new one</span>
      )}
    </Row>
  );
}

function ActiveCards(props: {
  data: IdentityData;
  identity: Identity;
  busy: boolean;
  run: (op: "provision" | "disable" | "rotate", p?: Record<string, unknown>) => Promise<void>;
  copy: (kind: string, what: string) => Promise<void>;
  mcp: McpConnection;
}) {
  const { data, identity: id, busy, run, copy, mcp } = props;
  const [shared, setShared] = useState<string[]>(id.sharedMailboxes);
  const [confirm, setConfirm] = useState(false);
  useEffect(() => setShared(id.sharedMailboxes), [id.sharedMailboxes.join(",")]);
  const changed = [...shared].sort().join(",") !== [...id.sharedMailboxes].sort().join(",");
  const c = data.credentials;
  return (
    <>
      <Card title="Nextcloud ID">
        <Row label="State">
          <StateBadge identity={id} pending={data.pending} />
        </Row>
        <Row label="Nextcloud user">
          <span className={mono}>{id.nextcloudUser}</span>
          <span className={muted}>({id.displayName})</span>
        </Row>
        <Row label="Email">
          <span className={mono}>{id.address}</span>
        </Row>
        {(data.info?.sharedMailboxes.length ?? 0) > 0 && (
          <Row label="Shared mailboxes">
            <SharedMailboxPicker info={data.info} value={shared} onChange={setShared} disabled={busy} />
            {changed && (
              <Button variant="primary" disabled={busy} onClick={() => run("provision", { name: id.name, displayName: id.displayName, sharedMailboxes: shared })}>
                <Icon name="check" />
                Save
              </Button>
            )}
          </Row>
        )}
      </Card>

      <Card
        title="Credentials"
        action={
          <IconButton
            icon="rotate"
            label="Rotate credentials: new login password and MCP app password; the old MCP password stops working"
            disabled={busy}
            onClick={() => void run("rotate")}
          />
        }
      >
        <Secret label="Nextcloud login password" copyLabel="Nextcloud login password" hint={c.nextcloudLoginPassword} onCopy={() => copy("nextcloudLoginPassword", "Login password")} />
        <Secret label="MCP app password" copyLabel="MCP app password" hint={c.mcpAppPassword} onCopy={() => copy("mcpAppPassword", "MCP app password")} />
      </Card>

      <McpCard data={data} mcp={mcp} busy={busy} />

      <Card title="Delete Nextcloud ID" tone="danger">
        <p className={muted}>
          Disables the Nextcloud user and its mailbox and removes shared mailboxes. Files, mail and Talk history are kept; you can re-enable
          or purge afterwards. The agent itself can only be deleted once its ID is purged.
        </p>
        {confirm ? (
          <div className="flex flex-wrap gap-2">
            <Button
              variant="danger"
              disabled={busy}
              onClick={() => {
                setConfirm(false);
                void run("disable");
              }}
            >
              <Icon name="trash" />
              Yes, delete (disable) {id.nextcloudUser}
            </Button>
            <Button onClick={() => setConfirm(false)}>Cancel</Button>
          </div>
        ) : (
          <div>
            <Button variant="danger" disabled={busy} onClick={() => setConfirm(true)}>
              <Icon name="trash" />
              Delete Nextcloud ID
            </Button>
          </div>
        )}
      </Card>
    </>
  );
}

function DisabledCard(props: { identity: Identity; busy: boolean; onEnable: () => void; onPurge: () => void }) {
  const id = props.identity;
  const [typed, setTyped] = useState("");
  return (
    <Card title="Nextcloud ID disabled" tone="danger">
      <p className={muted}>
        <span className={mono}>{id.nextcloudUser}</span> / <span className={mono}>{id.address}</span> is disabled. Its data is kept.
      </p>
      <div className="flex flex-wrap items-center gap-3">
        <Button variant="primary" disabled={props.busy} onClick={props.onEnable}>
          <Icon name="undo" />
          Re-enable
        </Button>
        <span className={muted}>Restores the user, mailbox and shared mailboxes with the existing credentials.</span>
      </div>
      <div className="flex flex-wrap items-center gap-3">
        <input className={input} placeholder={`type ${id.nextcloudUser} to purge`} value={typed} onChange={(e) => setTyped(e.target.value)} />
        <Button variant="danger" disabled={props.busy || typed !== id.nextcloudUser} onClick={props.onPurge}>
          <Icon name="trash" />
          Purge permanently
        </Button>
      </div>
      <p className={muted}>Purge deletes the Nextcloud user with its files, the mailbox with its mail, and the stored credentials. It cannot be undone.</p>
    </Card>
  );
}

// ------------------------------------------------------------------ the MCP connection

interface McpConnection {
  result: SyncResult | null;
  error: string | null;
  syncing: boolean;
  resync: () => void;
  /** Removes the connection and its secret; called before a purge. */
  remove: () => Promise<void>;
}

/**
 * Keeps the agent's Paperclip MCP connection in line with its identity (see mcp-connection.ts).
 * Runs in the signed-in user's session whenever the panel shows a settled identity.
 */
function useMcpConnection(
  data: IdentityData | null | undefined,
  agentId: string,
  companyId: string | null,
  refresh: () => void,
  onChange?: () => void,
): McpConnection {
  const reveal = usePluginAction("reveal");
  const record = usePluginAction("recordConnection");
  const api = useMemo(() => restApi(), []);
  const [result, setResult] = useState<SyncResult | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [syncing, setSyncing] = useState(false);
  const [nonce, setNonce] = useState(0);
  const running = useRef(false);
  const again = useRef(false);
  const latest = useRef(data);
  latest.current = data;

  const bridge: Bridge = useMemo(
    () => ({
      revealHeader: async () => ((await reveal({ agentId, companyId, kind: "mcpAuthorization", purpose: "connection" })) as { value: string }).value,
      record: async (connection) => {
        await record({ agentId, companyId, connection });
      },
    }),
    [reveal, record, agentId, companyId],
  );

  // Only a complete picture counts: an unreachable server is not "no identity".
  const ready = !!data && !!companyId && data.configured && !data.error && !data.pending && data.identity !== undefined;
  const key = ready
    ? JSON.stringify([data.identity?.state ?? null, data.identity?.nextcloudUser ?? null, data.info?.mcpUrl ?? null, data.credentialsUpdatedAt, !!data.connection])
    : null;

  useEffect(() => {
    if (!key) return;
    if (running.current) {
      again.current = true;
      return;
    }
    running.current = true;
    setSyncing(true);
    setError(null);
    sync(api, agentState(latest.current!, agentId, companyId!), bridge)
      .then((r) => {
        setResult(r);
        if (r.changes.length) {
          refresh();
          onChange?.();
        }
      })
      .catch((e) => setError(errorText(e)))
      .finally(() => {
        running.current = false;
        setSyncing(false);
        if (again.current) {
          again.current = false;
          setNonce((n) => n + 1);
        }
      });
  }, [key, nonce]);

  const remove = useCallback(async () => {
    const d = latest.current;
    if (!d || !companyId) return;
    await removeConnection(api, agentState(d, agentId, companyId), bridge);
    setResult({ connection: null, note: null, changes: [] });
  }, [api, bridge, agentId, companyId]);

  return { result, error, syncing, resync: () => setNonce((n) => n + 1), remove };
}

/** Paperclip's connection health values that need no attention. */
const HEALTH_FINE = new Set(["ok", "healthy", "unknown", "unchecked"]);

function McpCard({ data, mcp, busy }: { data: IdentityData; mcp: McpConnection; busy: boolean }) {
  const nav = useHostNavigation();
  const c = mcp.result?.connection;
  const [label, cls] = !c
    ? ["", ""]
    : !c.enabled
      ? ["off", "bg-muted text-muted-foreground"]
      : !c.healthStatus || HEALTH_FINE.has(c.healthStatus)
        ? ["on", "bg-emerald-500/15 text-emerald-700 dark:text-emerald-400"]
        : [c.healthStatus.replace(/_/g, " "), "bg-amber-500/15 text-amber-700 dark:text-amber-400"];
  return (
    <Card
      title="MCP connection"
      action={<IconButton icon="refresh" label="Check and repair the MCP connection" disabled={busy || mcp.syncing} onClick={mcp.resync} />}
    >
      <p className={muted}>
        The agent works in Nextcloud (Talk, Mail, Files, Calendar …) through the Nextcloud MCP server, as{" "}
        <span className={mono}>{data.identity?.nextcloudUser}</span>. Paperclip holds the credential and grants the tools to this agent only.
      </p>
      <Row label="Connection">
        {c ? (
          <>
            <a {...nav.linkProps(`/apps/${c.id}`)} className="text-sm font-medium underline-offset-2 hover:underline">
              {c.name}
            </a>
            <span className={`rounded-full px-2 py-0.5 text-xs font-medium ${cls}`}>{label}</span>
          </>
        ) : (
          <span className={muted}>{mcp.syncing ? "setting up…" : (mcp.result?.note ?? "—")}</span>
        )}
      </Row>
      {c?.healthMessage && !HEALTH_FINE.has(c.healthStatus ?? "") && (
        <Row label="Health">
          <span className="text-sm">{c.healthMessage}</span>
        </Row>
      )}
      <Row label="Server URL">
        <span className={mono}>{data.info?.mcpUrl ?? "not set in the server's config (mcp.url)"}</span>
      </Row>
      {c && (
        <div>
          <a
            {...nav.linkProps(`/apps/${c.id}`)}
            className="inline-flex items-center gap-1.5 rounded-md border border-border bg-background px-3 py-1.5 text-sm font-medium transition-colors hover:bg-accent"
          >
            <Icon name="external" />
            Tools and permissions
          </a>
        </div>
      )}
      {mcp.error && <p className="text-sm text-destructive">{mcp.error}</p>}
    </Card>
  );
}
