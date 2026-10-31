# paperclipped-nextcloud

Give the AI agents you run in [Paperclip](https://github.com/paperclipai/paperclip) their own
identity in your Nextcloud and on your mail server. Each digital worker gets:

- a **Nextcloud user** in a dedicated group (default `digital workforce`): its own Talk handle,
  files, calendar;
- its **own mailbox**, `name@your-domain`, already set up in its Nextcloud Mail;
- optionally, access to **shared company mailboxes** (`contact@`, `info@`, …) in its Nextcloud
  Mail;
- the group's **team folder** and **Talk lobby**, through group membership;
- a **Paperclip connection to the [Nextcloud MCP server](https://github.com/cbcoutinho/nextcloud-mcp-server)**
  with its own app password, granted to that agent only: this is how the agent works in
  Nextcloud, as itself. Its tools and permissions are managed in Paperclip.

It has two parts:

| Part | What it is |
|---|---|
| **Server** (`paperclipped-nextcloud`, Rust) | Runs next to your mail server and holds the privileged access. HTTP API and CLI. |
| **Paperclip plugin** (npm `paperclipped-nextcloud`, [`plugin/`](plugin/README.md)) | A *Nextcloud identities* page (and, on newer Paperclip releases, a tab on each agent's page): create, delete (disable), re-enable, purge; masked credentials with copy; the agent's MCP connection, set up and kept in line; blocks deleting an agent until its ID is purged. |

## The big picture

This project hires AI agents as if they were new employees. When a new person joins a company,
someone in IT creates their mail account and their intranet login, and hands over the
passwords. That is this project's job — for the digital workers you run in Paperclip. But
before the new-hire paperwork can happen, the company itself must already exist, with four
buildings in place:

- **Paperclip** is the office building: your AI agents live and work there.
- **Nextcloud** is the company's own intranet: files, chat, calendar and a webmail app, all
  self-hosted — like running your own private Google Workspace.
- **docker-mailserver** is the post office: it receives, stores and hands out real email for
  your domain. Inside it, Postfix takes the mail in, and Dovecot hands it out over IMAP.
- **nextcloud-mcp-server** is the interpreter. MCP is the standard plug AI agents use for
  tools; this server translates between an agent and Nextcloud, so the agent can read mail,
  chat and manage files *as itself*.

This project is the IT clerk, in two parts. The **server** does the privileged paperwork:
it creates the mailbox at the post office, the user account in the intranet, and hands out
the passwords. The **plugin** is the counter inside the Paperclip office: buttons to hire
(create an identity), let go (disable), rehire (re-enable) and erase (purge) — and it wires
each agent to the interpreter, so the agent gets its own hands in Nextcloud.

The order matters: the office, the intranet, the post office and the interpreter must all be
running *first*. This tool connects them; it does not install any of them.

## Prerequisites and dependencies

Everything here must already be set up and working before this plugin makes sense. Nothing
below is installed by this project; `spike/up.sh` is the only place a full stack is wired up,
and only for testing. The versions are what this project was tested with.

| Component | Tested with | Required? | Role |
|---|---|---|---|
| [Paperclip](https://github.com/paperclipai/paperclip) | 2026.1023.0 | Yes | The office: where the agents and the plugin run |
| [Nextcloud](https://nextcloud.com) | 34.0.5 | Yes | Users, group, files, Talk, webmail |
| Nextcloud **Mail** app | 5.12.5 | Yes | The workers' webmail |
| Nextcloud **Talk** app (`spreed`) | 24.0.6 | Yes | The lobby |
| Nextcloud **Team folders** app (`groupfolders`) | 22.0.6 | No | Team folder; without it, a plain folder shared to the group |
| [docker-mailserver](https://github.com/docker-mailserver/docker-mailserver) | v16.0.1 | Yes | Mail: Postfix and Dovecot in a container. The server edits DMS's config volume, as `setup email …` does. |
| [nextcloud-mcp-server](https://github.com/cbcoutinho/nextcloud-mcp-server) | 0.198.3 | Yes | The interpreter: the agents' tools in Nextcloud. A **separate instance** in `MCP_DEPLOYMENT_MODE=multi_user_basic`, never exposed publicly. |
| paperclipped-nextcloud **server** | 0.2.2 | Yes | The clerk: this repo's Rust server. Container `ghcr.io/deltadrai/paperclipped-nextcloud` (a static binary on an empty base — no OS packages at all) or the same binary built from source. |

What "working" means, per piece:

**Paperclip**

- Installed and running, with the agents you want to give identities.
- Node ≥ 24.11 inside it: Paperclip runs the plugin with its embedded Node.
- Users who manage identities may manage Paperclip's tool connections (the panel sets up each
  agent's MCP connection in their browser session).
- Exposure `private`, unless the MCP server is on a public address: Paperclip only connects
  to private addresses while its exposure is not `public`.

**Nextcloud**

- A server you administer, on a production database (PostgreSQL or MariaDB; SQLite fell over
  in the spike), with background jobs running — Mail's sync is a background job.
- The **Mail**, **Talk** and (optionally) **Team folders** apps installed and enabled.
- An **admin login** (name and password) for the one-time `bootstrap`.
- Enforced two-factor authentication off for the provisioner user that `bootstrap` creates:
  user management needs its login password, and app passwords are rejected there.

**Mail (docker-mailserver)**

- A mail **domain you control**: MX records pointing at the mail server and TLS certificates,
  because agents' addresses are `name@your-domain`.
- DMS's **config volume** mountable read-write wherever the paperclipped-nextcloud server runs;
  its mail volume too, if `purge` should delete the mail.
- At least one account seeded beforehand: DMS refuses to start with an empty
  `postfix-accounts.cf`.
- Every shared mailbox you want to hand out (`contact@`, …) a **real account with its own
  password**: an alias cannot be shared as a mailbox.

**nextcloud-mcp-server**

- A second instance, `MCP_DEPLOYMENT_MODE=multi_user_basic`, pointed at your Nextcloud and
  reachable from Paperclip only.

**Who must be able to reach whom** (private addresses are the norm):

```
Paperclip ──HTTP──▶ paperclipped-nextcloud server    (the API, Bearer token)
Paperclip ──HTTP──▶ nextcloud-mcp-server             (MCP: the agents' connection)
server ──HTTPS──▶ Nextcloud                           (OCS API, as the sub-admin)
server ──IMAP──▶ docker-mailserver                   (probes new mailbox passwords)
server ──files──▶ DMS config volume                   (read-write, where it runs)
Nextcloud Mail ──IMAP/SMTP──▶ docker-mailserver       (the workers' webmail)
```

Other mail servers can be added as backends: implement `pcnc_core::backend::MailBackend`.

## How it stays safe

- **It only manages what it created.** Every identity lives in the server's registry. Creating
  one is refused if the Nextcloud user, the mailbox or an alias of that name already exists.
- **Nextcloud enforces the scope too.** The server works as a **sub-admin of the workers' group
  only**, so Nextcloud itself stops it from touching any other account.
- **The admin credential is used once.** `bootstrap` runs with an admin's login password for
  that run only; it is never stored.
- **Credentials at rest are sealed.** Workers' management and MCP app passwords and their
  mailbox passwords are kept encrypted (ChaCha20-Poly1305, `PCNC_STATE_KEY`).
  - There is no endpoint that returns stored credentials.
  - The login password and the MCP app password are returned **once**, when created or rotated.
- **Audit.** Every change is appended to `audit.log` (JSON lines), without credentials.
- **Rate limit.** Changes are rate-limited across all clients.

## Lifecycle

```
(no ID) ──Create──▶ active ──Delete──▶ disabled ──Purge──▶ (no ID)
                      ▲                    │
                      └─────Re-enable──────┘
```

| Operation | API | What happens |
|---|---|---|
| Create / re-sync | `PUT /v1/identities/{id}` | Converges the user, app passwords, mailbox and Mail accounts. A repeat run reports every step `unchanged`. |
| Delete | `POST /v1/identities/{id}/disable` | Removes shared mailboxes, disables the Nextcloud user, locks the mailbox. Data stays. |
| Re-enable | `POST /v1/identities/{id}/enable` | Restores everything. The existing credentials keep working. |
| Rotate | `POST /v1/identities/{id}/rotate` | New login password and MCP app password; the old MCP password is revoked. |
| Purge | `DELETE /v1/identities/{id}` | **Disabled identities only.** Deletes the Nextcloud user with its files, the mailbox with its mail, and the record. |
| Inspect | `GET /v1/identities/{id}/inspect` | Compares the record with live state. Changes nothing. |

`{id}` is the caller's id; the Paperclip plugin uses the agent id. Every route except `/healthz`
needs `Authorization: Bearer $PCNC_API_TOKEN`.

## Setup

With every [prerequisite](#prerequisites-and-dependencies) in place:

1. **Run a second nextcloud-mcp-server** in `multi_user_basic` mode, reachable only from
   Paperclip, and set its URL as `mcp.url`. The plugin connects each agent to it. The connection
   is created in a signed-in user's browser session the first time the agent is opened in the
   *Nextcloud identities* panel, so an ID provisioned outside the panel (with the server's CLI,
   for example) needs that one visit before the agent can use its Nextcloud tools.
2. **Configure.** Copy [`deploy/config.example.yaml`](deploy/config.example.yaml) and set these
   environment variables:

   | Variable | Purpose |
   |---|---|
   | `PCNC_API_TOKEN` | At least 32 characters; the Paperclip plugin sends it |
   | `PCNC_STATE_KEY` | `openssl rand -base64 32` |
   | `NEXTCLOUD_SUBADMIN_PASSWORD` | Printed by `bootstrap` when it creates the sub-admin |
   | `SHARED_MAILBOX_<KEY>_PASSWORD` | One per shared mailbox |

3. **Bootstrap** once, as a Nextcloud admin:

   ```sh
   PCNC_BOOTSTRAP_ADMIN_USER=admin PCNC_BOOTSTRAP_ADMIN_PASSWORD='…' paperclipped-nextcloud bootstrap
   ```

   It creates the group, the sub-admin user, the team folder and the lobby. It is idempotent.
4. **Exempt the sub-admin from enforced two-factor authentication.** Nextcloud requires its
   **login password** for user management; app passwords are rejected there.
5. **Serve:** `paperclipped-nextcloud serve`. Mount DMS's config volume at `mail.config_dir`, and
   optionally its mail volume at `mail.mail_dir`.
   - The image is `ghcr.io/deltadrai/paperclipped-nextcloud`: a static binary on an empty base,
     amd64 and arm64.
   - `paperclipped-nextcloud health` exits 0 when the API answers; use it as the container
     healthcheck.
6. **Make new mail show up promptly for agents.** Nextcloud Mail shows agents only the mail it
   has synced. The server syncs every new account once, so an agent can read its mailbox right
   away. Mail that arrives later appears after Mail's background sync. Set it to the 5-minute
   minimum:

   ```sh
   occ config:system:set app.mail.background-sync-interval --value 300 --type integer
   ```

## Development

You need Rust ≥ 1.85 and Node 24; the spike and the e2e tests need rootless `podman`.

```sh
cargo test                          # server unit tests (engine against in-memory fakes, DMS files, registry, IMAP probe)
(cd plugin && npm ci && npm test)   # plugin unit tests (Node 24)
spike/up.sh                         # local pod: DMS, Nextcloud + Mail/Talk/Team folders, MCP, Paperclip (UI on 127.0.0.1:13100)
tests/e2e/spike-e2e.sh              # server end-to-end against that pod
tests/plugin/spike-plugin-e2e.sh    # plugin end-to-end: installs the packed plugin into the pod's Paperclip
spike/down.sh
```

`spike/FINDINGS.md` records what the P0 spike established about each component.

## License

MIT
