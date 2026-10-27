import type { PaperclipPluginManifestV1 } from "@paperclipai/plugin-sdk";

export const PLUGIN_ID = "deltadrai.paperclipped-nextcloud";
export const PLUGIN_VERSION = "0.2.0";

const manifest: PaperclipPluginManifestV1 = {
  id: PLUGIN_ID,
  apiVersion: 1,
  version: PLUGIN_VERSION,
  displayName: "Nextcloud identity",
  description:
    "Gives an agent its own Nextcloud user and mailbox through a paperclipped-nextcloud server, and connects it to the Nextcloud MCP server as that user.",
  author: "deltadrai",
  categories: ["connector", "ui"],
  capabilities: [
    "ui.detailTab.register",
    "ui.page.register",
    "ui.sidebar.register",
    "instance.settings.register",
    "plugin.state.read",
    "plugin.state.write",
    "secrets.read-ref",
    "agents.read",
    "agents.lifecycle.manage",
  ],
  // Required for every agent while enabled: `cleaning_up` waits until the agent's identity is
  // purged, so no Nextcloud user or mailbox outlives its agent.
  agentLifecycle: true,
  instanceConfigSchema: {
    type: "object",
    required: ["serverUrl", "apiToken", "credentialKey"],
    properties: {
      serverUrl: {
        type: "string",
        title: "Server URL",
        description: "Base URL of the paperclipped-nextcloud server, e.g. http://100.64.0.10:8091",
      },
      // A `{ type: "secret_ref", secretId }` binding: no `type`, so the schema accepts the object.
      apiToken: {
        format: "secret-ref",
        title: "API token",
        description: "The server's PCNC_API_TOKEN, stored as a company secret.",
      },
      // A `{ type: "secret_ref", secretId }` binding: no `type`, so the schema accepts the object.
      credentialKey: {
        format: "secret-ref",
        title: "Credential encryption key",
        description:
          "A random string of at least 16 characters (e.g. `openssl rand -base64 32`), stored as a company secret. Encrypts the agents' credentials this plugin keeps.",
      },
    },
  },
  entrypoints: {
    worker: "./dist/worker.js",
    ui: "./dist/ui",
  },
  ui: {
    slots: [
      // A company page listing every agent with its identity. Works on every Paperclip version;
      // agent detail tabs are rendered by Paperclip releases after 2026.1023.0 only.
      {
        type: "page",
        id: "nextcloud-identities",
        displayName: "Nextcloud identities",
        exportName: "IdentitiesPage",
        routePath: "nextcloud-identities",
      },
      {
        type: "sidebar",
        id: "nextcloud-identities-link",
        displayName: "Nextcloud identities",
        exportName: "IdentitiesSidebarLink",
      },
      {
        type: "detailTab",
        id: "nextcloud-identity",
        displayName: "Nextcloud",
        exportName: "IdentityTab",
        entityTypes: ["agent"],
      },
    ],
  },
};

export default manifest;
