# paperclipped-nextcloud (Paperclip plugin)

Give your [Paperclip](https://github.com/paperclipai/paperclip) agents their own identity in
Nextcloud. Each digital worker gets:

- a Nextcloud user;
- its own mailbox, set up in Nextcloud Mail;
- optionally, shared company mailboxes;
- the group's team folder and Talk lobby;
- a **Paperclip connection to the
  [Nextcloud MCP server](https://github.com/cbcoutinho/nextcloud-mcp-server)** with its own
  credentials, granted to that agent only. The agent works in Nextcloud as itself, and you manage
  its tools and permissions in Paperclip.

The plugin is the Paperclip side of
[paperclipped-nextcloud](https://github.com/deltadrai/paperclipped-nextcloud). It needs that
project's **server**, which runs next to your mail server and Nextcloud and does the actual
provisioning.

## Prerequisites

Think of this project as hiring AI agents like new employees: the server is the IT clerk that
creates the mailbox, the Nextcloud account and the passwords, and this plugin is the counter
in Paperclip where you press the buttons. Before any of that can happen, the company itself
must already exist — none of these pieces is installed by this plugin:

| Component | Tested with | Required? | Role |
|---|---|---|---|
| [Paperclip](https://github.com/paperclipai/paperclip) | 2026.1023.0 | Yes | The office: where the agents and this plugin run |
| [Nextcloud](https://nextcloud.com) | 34.0.5 | Yes | Users, group, files, Talk, webmail |
| Nextcloud **Mail** app | 5.12.5 | Yes | The workers' webmail |
| Nextcloud **Talk** app (`spreed`) | 24.0.6 | Yes | The lobby |
| Nextcloud **Team folders** app (`groupfolders`) | 22.0.6 | No | Team folder; without it, a plain folder shared to the group |
| [docker-mailserver](https://github.com/docker-mailserver/docker-mailserver) | v16.0.1 | Yes | The post office: Postfix and Dovecot in a container, holding the real mailboxes |
| [nextcloud-mcp-server](https://github.com/cbcoutinho/nextcloud-mcp-server) | 0.198.3 | Yes | The interpreter: the agents' tools in Nextcloud. A separate `multi_user_basic` instance, never exposed publicly. |
| paperclipped-nextcloud **server** | 0.2.x | Yes | The clerk: does the provisioning this plugin asks for |

In short, before installing the plugin you need:

- **Paperclip** installed and running, with Node ≥ 24.11 inside it; the users who manage
  identities may manage Paperclip's tool connections; exposure `private` (Paperclip only
  connects to private addresses while its exposure is not `public`).
- **Nextcloud**: a server you administer, on a production database (PostgreSQL or MariaDB),
  with the Mail, Talk and (optionally) Team folders apps enabled, and background jobs running.
- **docker-mailserver** running the mail for a domain you control (MX records, TLS
  certificates), with its config volume mounted read-write where the server runs.
- **nextcloud-mcp-server** as a separate `multi_user_basic` instance, reachable from
  Paperclip only.
- **The server** running next to your mail server and reachable from Paperclip.

The [server README](https://github.com/deltadrai/paperclipped-nextcloud#prerequisites-and-dependencies)
explains all of it in detail, including who must be able to reach whom.

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
| **MCP connection** | The agent's connection in Paperclip, its state, and a link to its tools and permissions. Set up and kept in line automatically (below). |
| **Delete** | Disables the ID and keeps its data. From there, **Re-enable** or **Purge**; purge asks you to type the user name. |

**Agents cannot be deleted while they still have an ID.** The plugin takes part in Paperclip's
agent lifecycle: during `cleaning_up` it waits until the agent's ID is purged.

## The MCP connection

Creating an ID also connects the agent in Paperclip to the Nextcloud MCP server, as its own
Nextcloud user:

- a company secret *Nextcloud MCP (`<user>`)* holds the `Authorization` header;
- a remote-MCP connection of the same name sends it, with MCP sessions on (the server requires
  them);
- the connection is granted **to that agent only**. Read and write tools start allowed;
  destructive ones (deleting mail, files, events …) start as **ask first**. Change this in
  Paperclip under the connection's tools and permissions, which the panel links to.

The ID's lifecycle carries over:

| ID action | Connection |
|---|---|
| Rotate credentials | The secret gets the new header |
| Delete (disable) | Switched off |
| Re-enable | Switched on |
| Purge | Connection and secret deleted |

**How it works.** Paperclip's plugin API cannot manage connections, but its REST API can, for a
signed-in user. The panel therefore makes these calls in your browser session, exactly as
Paperclip's own UI does, whenever it shows an agent. An interrupted setup is completed the next
time you open the agent; the **Check and repair** button runs the same sync on demand.
Consequences:

- the connection is created the first time a signed-in user opens the agent's panel. An ID
  provisioned outside the panel (with the server's CLI, for example) therefore still needs that
  one visit before the agent can use its Nextcloud tools; the *Nextcloud identities* page lists
  such agents with a **Set up MCP** button. On Paperclip 2026.1023.0 the agent detail tab is not
  rendered yet, so the company page's panel is the place;
- operations started in the panel sync the connection when they finish. Changes made outside
  it, with the server's CLI for example, are synced the next time someone opens the agent in the
  panel;
- the user needs permission to manage Paperclip's tool connections;
- **Paperclip only connects to private addresses** (a tailnet, a LAN) while its deployment
  exposure is not `public`. If the MCP server is reachable only privately, as it should be, keep
  Paperclip's exposure `private`.

## Install

1. Run the [server](https://github.com/deltadrai/paperclipped-nextcloud#setup). Paperclip must
   reach its HTTP API and the MCP server it points to (`mcp.url`).
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
- **The MCP credential lives in a Paperclip secret.** The panel reads it through the *Copy*
  path (a signed-in user only, logged as "for its MCP connection") and writes it into the
  agent's secret. Paperclip's gateway, not the agent, sends it to the MCP server.
- **The server URL is admin-configured.** The worker calls it with Node's own `fetch`, not the
  host's `ctx.http`. The host's SSRF guard refuses private address ranges, and this server
  usually sits on one. The URL comes from the plugin settings only, never from users or agents.

## Operational notes

- **Background operations.** Creating an ID can take up to about a minute: the server waits
  until the mail server accepts the new mailbox. Operations therefore run in the background,
  and the panel polls. Every operation is idempotent: if one fails, run it again.
- **Secret resolutions are rate-limited.** Paperclip allows 30 per minute per company. The
  plugin keeps a resolved value for one minute and resolves nothing while polling. After you
  change the API token or the credential key, allow a minute before it takes effect.
- **The lifecycle hook gates every agent.** If the plugin is enabled but broken (for example,
  the server is unreachable), agents stay in `cleaning_up` and cannot be deleted. To proceed
  anyway, disable the plugin for that company.
- **Upgrades.** After installing a new plugin version, restart Paperclip: it caches imported
  plugin modules.

## Compatibility

| Component | Version |
|---|---|
| Paperclip / `@paperclipai/plugin-sdk` | 2026.1023.0 |
| paperclipped-nextcloud server | 0.1.x or 0.2.x (with `mcp.url` set for the MCP connection) |
| Node (inside Paperclip) | ≥ 24.11 |

## Credits and trademarks

- Icons: [Lucide](https://lucide.dev) (ISC), the set Paperclip uses.
- Nextcloud is a trademark of Nextcloud GmbH. This project is not affiliated with or endorsed by
  Nextcloud GmbH or Paperclip, and does not use the Nextcloud logo.

## License

MIT
