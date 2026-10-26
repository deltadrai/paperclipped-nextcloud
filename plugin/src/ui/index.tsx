import { useEffect, useState, type ReactNode } from "react";
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
}

interface OverviewData {
  configured: boolean;
  error: string | null;
  info?: Info;
  agents: { id: string; name: string; status: string; identity: Identity | null; pending: { op: string } | null }[];
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

function Card(props: { title: string; children: ReactNode; tone?: "danger" }) {
  return (
    <section className={`rounded-lg border p-4 ${props.tone === "danger" ? "border-destructive/40" : "border-border"}`}>
      <h3 className="mb-3 text-sm font-semibold">{props.title}</h3>
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
      className={`flex items-center gap-2.5 px-3 py-2 text-[13px] font-medium transition-colors ${
        active ? "bg-accent text-foreground" : "text-foreground/80 hover:bg-accent/50 hover:text-foreground"
      }`}
    >
      <svg viewBox="0 0 24 24" className="h-4 w-4 shrink-0" fill="none" stroke="currentColor" strokeWidth="1.9" strokeLinecap="round" aria-hidden="true">
        <path d="M7 18a4 4 0 0 1-.6-7.96A5.5 5.5 0 0 1 17 9.5a3.75 3.75 0 0 1 .5 7.47V17" />
        <circle cx="12" cy="15" r="2.2" />
        <path d="M8.5 21a3.5 3.5 0 0 1 7 0" />
      </svg>
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
        <h1 className="text-lg font-semibold">Nextcloud identities</h1>
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
                <td className="px-3 py-2 text-right">
                  <Button onClick={() => setSelected(selected === a.id ? null : a.id)}>{selected === a.id ? "Close" : "Manage"}</Button>
                </td>
              </tr>
            ))}
            {data.agents.length === 0 && (
              <tr>
                <td colSpan={5} className={`px-3 py-6 text-center ${muted}`}>
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
      await actions[op]({ agentId, companyId, ...params });
      refresh();
      onChange?.();
    } catch (e) {
      toast({ title: "Action failed", body: e instanceof Error ? e.message : String(e), tone: "error" });
    }
  }

  async function copy(kind: string, what: string) {
    try {
      const res = (await actions.reveal({ agentId, companyId, kind })) as { value: string };
      await copyTextToClipboard(res.value);
      toast({ title: `${what} copied`, tone: "success", ttlMs: 2500 });
    } catch (e) {
      toast({ title: "Copy failed", body: e instanceof Error ? e.message : String(e), tone: "error" });
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
        <DisabledCard identity={data.identity} busy={busy} onEnable={() => run("enable")} onPurge={() => run("purge")} />
      ) : data.identity ? (
        <ActiveCards data={data} identity={data.identity} busy={busy} run={run} copy={copy} />
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
          Create Nextcloud ID
        </Button>
      </div>
    </Card>
  );
}

function Secret(props: { label: string; hint?: string; onCopy: () => void }) {
  return (
    <Row label={props.label}>
      {props.hint ? (
        <>
          <code className={`rounded bg-muted px-1.5 py-0.5 ${mono}`}>{props.hint}</code>
          <Button onClick={props.onCopy} title="Copy to clipboard">
            <svg viewBox="0 0 24 24" className="h-3.5 w-3.5" fill="none" stroke="currentColor" strokeWidth="2" aria-hidden="true">
              <rect x="9" y="9" width="11" height="11" rx="2" />
              <path d="M5 15V6a2 2 0 0 1 2-2h9" />
            </svg>
            Copy
          </Button>
        </>
      ) : (
        <span className={muted}>not stored. Use "Rotate credentials" to get a new one</span>
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
}) {
  const { data, identity: id, busy, run, copy } = props;
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
                Save
              </Button>
            )}
          </Row>
        )}
      </Card>

      <Card title="Credentials">
        <Secret label="Nextcloud login password" hint={c.nextcloudLoginPassword} onCopy={() => copy("nextcloudLoginPassword", "Login password")} />
        <Secret label="MCP app password" hint={c.mcpAppPassword} onCopy={() => copy("mcpAppPassword", "MCP app password")} />
        <div className="flex flex-wrap items-center gap-3">
          <Button disabled={busy} onClick={() => run("rotate")}>
            Rotate credentials
          </Button>
          <span className={muted}>New login password and MCP app password; the old MCP password stops working.</span>
        </div>
      </Card>

      <Card title="Connect Nextcloud MCP">
        <p className={muted}>
          The agent works in Nextcloud (Talk, Mail, Files, Calendar, Deck …) through the Nextcloud MCP server, as <span className={mono}>{id.nextcloudUser}</span>.
        </p>
        <Row label="Server URL">
          {data.info?.mcpUrl ? (
            <>
              <code className={`rounded bg-muted px-1.5 py-0.5 ${mono}`}>{data.info.mcpUrl}</code>
              <Button onClick={() => void copyTextToClipboard(data.info!.mcpUrl!)} title="Copy to clipboard">
                Copy
              </Button>
            </>
          ) : (
            <span className={muted}>not set in the server's config (mcp.url)</span>
          )}
        </Row>
        <Secret label="Authorization header" hint={c.mcpAuthorization} onCopy={() => copy("mcpAuthorization", "Authorization header")} />
        <ol className={`list-decimal pl-5 ${muted}`}>
          <li>Connectors → Custom remote MCP: paste the server URL.</li>
          <li>
            Advanced authentication: header <span className={mono}>Authorization</span>, paste the header value.
          </li>
          <li>Name it "Nextcloud ({id.nextcloudUser})" and grant it to this agent only.</li>
          <li>After "Rotate credentials", paste the new header value into the connection.</li>
        </ol>
      </Card>

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
              Yes, delete (disable) {id.nextcloudUser}
            </Button>
            <Button onClick={() => setConfirm(false)}>Cancel</Button>
          </div>
        ) : (
          <div>
            <Button variant="danger" disabled={busy} onClick={() => setConfirm(true)}>
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
          Re-enable
        </Button>
        <span className={muted}>Restores the user, mailbox and shared mailboxes with the existing credentials.</span>
      </div>
      <div className="flex flex-wrap items-center gap-3">
        <input className={input} placeholder={`type ${id.nextcloudUser} to purge`} value={typed} onChange={(e) => setTyped(e.target.value)} />
        <Button variant="danger" disabled={props.busy || typed !== id.nextcloudUser} onClick={props.onPurge}>
          Purge permanently
        </Button>
      </div>
      <p className={muted}>Purge deletes the Nextcloud user with its files, the mailbox with its mail, and the stored credentials. It cannot be undone.</p>
    </Card>
  );
}
