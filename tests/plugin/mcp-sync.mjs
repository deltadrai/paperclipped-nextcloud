// Plays the panel's part in the plugin e2e: syncs (or removes) an agent's Paperclip MCP
// connection with the plugin's own ui/mcp-connection module, against a real Paperclip. The
// bridge goes through the plugin's actions, as the panel's does.
//
//   node mcp-sync.mjs <bundled mcp-connection.mjs> <paperclip api base> <pluginId> <companyId> <agentId> sync|remove
// Prints the sync result as JSON.

const [modulePath, base, pluginId, companyId, agentId, mode = "sync"] = process.argv.slice(2);
const { agentState, removeConnection, restApi, sync } = await import(modulePath);

async function plugin(kind, name, params) {
  const res = await fetch(`${base}/plugins/${pluginId}/${kind}/${name}`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ companyId, params: { agentId, companyId, ...params } }),
  });
  const json = await res.json();
  if (!res.ok || json.error) throw new Error(`${kind}/${name}: ${JSON.stringify(json.error ?? json)}`);
  return json.data;
}

const bridge = {
  revealHeader: async () => (await plugin("actions", "reveal", { kind: "mcpAuthorization", purpose: "connection" })).value,
  record: async (connection) => void (await plugin("actions", "recordConnection", { connection })),
};

const state = agentState(await plugin("data", "identity", {}), agentId, companyId);
const api = restApi(base);
if (mode === "remove") {
  await removeConnection(api, state, bridge);
  console.log(JSON.stringify({ removed: true }));
} else {
  console.log(JSON.stringify(await sync(api, state, bridge)));
}
