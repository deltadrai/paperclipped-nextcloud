# paperclipped-nextcloud (Paperclip plugin)

Give your [Paperclip](https://github.com/paperclipai/paperclip) agents their own identity in
Nextcloud. Each digital worker gets:

- a Nextcloud user;
- its own mailbox, set up in Nextcloud Mail;
- optionally, shared company mailboxes;
- the group's team folder and Talk lobby;
- an app password for the
  [Nextcloud MCP server](https://github.com/cbcoutinho/nextcloud-mcp-server).

The plugin is the Paperclip side of
[paperclipped-nextcloud](https://github.com/deltadrai/paperclipped-nextcloud). It needs that
project's **server**, which runs next to your mail server and Nextcloud and does the actual
provisioning.

## What you get in Paperclip

- **Nextcloud identities** in the sidebar: a company page listing every agent with its
  identity state. *Manage* opens the panel for one agent.
- **A Nextcloud tab on each agent's page**, with the same panel. This needs a Paperclip release
  after 2026.1023.0: that release accepts the slot but does not render agent tabs yet.

The panel walks through the lifecycle:

```
(no ID) ──Create Nextcloud ID──▶ active ──Delete Nextcloud ID──▶ disabled ──Purge──▶ (no ID)
                                   ▲                                │
                                   └────────────Re-enable───────────┘
```

| Panel area | What it does |
|---|---|
| **Create Nextcloud ID** | Name (suggested from the agent's name), display name, and the shared mailboxes to grant. |
| **Active** | Shared mailboxes are editable. The **credentials** (Nextcloud login password, MCP app password) are shown masked with a copy button. **Rotate credentials** issues new ones and revokes the old MCP password. |
| **Connect Nextcloud MCP** | The MCP server URL and the agent's `Authorization` header, for a *Custom remote MCP* connection that you grant to that agent only. |
| **Delete** | Disables the ID and keeps its data. From there, **Re-enable** or **Purge**; purge asks you to type the user name. |

**Agents cannot be deleted while they still have an ID.** The plugin takes part in Paperclip's
agent lifecycle: during `cleaning_up` it waits until the agent's ID is purged.

## Install

1. Run the [server](https://github.com/deltadrai/paperclipped-nextcloud#setup). Paperclip must
   reach its HTTP API, and agents must reach the MCP server it points to (`mcp.url`).
2. In Paperclip, go to **Settings → Plugins → Install from npm** and enter
   `paperclipped-nextcloud@<version>`.
3. Create two company secrets:
   - the server's `PCNC_API_TOKEN`;
   - a random **credential key** (`openssl rand -base64 32`). The plugin uses it to encrypt the
     credentials it keeps.
4. Open the plugin's settings **for each company** and set:

   | Setting | Value |
   |---|---|
   | **Server URL** | e.g. `http://100.64.0.10:8091` |
   | **API token** | the secret from step 3 |
   | **Credential encryption key** | the secret from step 3 |

## Security model

- **Only signed-in users can manage identities.** Agents can call plugin actions too, so every
  action checks its caller and refuses agents and the system. The data the panel reads contains
  no secrets, only masked hints such as `Xk3f…a9Q2`.
- **Credentials are stored encrypted.** The plugin keeps each agent's credentials in Paperclip's
  plugin state, encrypted with AES-256-GCM. The key comes from the credential-key secret, and
  each value is bound to its agent and field.
  - The full value leaves the plugin only through the *Copy* action, to a signed-in user, and
    every copy is logged.
  - The server never stores the login password. It keeps the MCP app password sealed, only so
    *Rotate* can revoke it.
- **The server URL is admin-configured.** The worker calls it with Node's own `fetch`, not the
  host's `ctx.http`. The host's SSRF guard refuses private address ranges, and this server
  usually sits on one. The URL comes from the plugin settings only, never from users or agents.

## Operational notes

- **Background operations.** Creating an ID can take up to about a minute: the server waits
  until the mail server accepts the new mailbox. Operations therefore run in the background,
  and the panel polls. Every operation is idempotent: if one fails, run it again.
- **Secret resolutions are rate-limited.** Paperclip allows 30 per minute per company. The
  plugin resolves secrets only when an action starts or the panel first loads, never while
  polling.
- **The lifecycle hook gates every agent.** If the plugin is enabled but broken (for example,
  the server is unreachable), agents stay in `cleaning_up` and cannot be deleted. To proceed
  anyway, disable the plugin for that company.
- **Upgrades.** After installing a new plugin version, restart Paperclip: it caches imported
  plugin modules.

## Compatibility

| Component | Version |
|---|---|
| Paperclip / `@paperclipai/plugin-sdk` | 2026.1023.0 |
| paperclipped-nextcloud server | 0.1.x |
| Node (inside Paperclip) | ≥ 24.11 |

## License

MIT
