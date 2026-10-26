# P0 spike: findings (2026-10-26)

Environment: `spike/up.sh`, one rootless podman pod on an x86_64 laptop.

| Component | Version |
|---|---|
| docker-mailserver | v16.0.1 (Dovecot 2.4.1, Postfix 3.10.13) |
| Nextcloud | 34.0.5 (SQLite) |
| Nextcloud Mail | 5.12.5 |
| Nextcloud Talk (spreed) | 24.0.6 |
| Team folders (groupfolders) | 22.0.6 |
| nextcloud-mcp-server | digest `55073a7e…` (0.198.3) |
| Paperclip | 2026.1023.0 |

The images use the digests deployed on 2026-10-26.

**Verdict: GO.** Two design changes follow from these findings: (c) and (g).

## (a) Nextcloud Mail account as the worker: GO

- `POST /index.php/apps/mail/api/accounts` works with:
  - Basic auth `<user>:<app password>`;
  - header `OCS-APIRequest: true`;
  - JSON body `accountName, emailAddress, imap{Host,Port,SslMode,User,Password},
    smtp{Host,Port,SslMode,User,Password}`.
- Results: create → **201**; `GET` lists accounts (`id`, `emailAddress`); `DELETE …/{id}` → 200.
- The same call works for a shared mailbox, logged in with that mailbox's own credentials.
- No fallback Nextcloud app is needed.
- The spike set `allow_local_remote_servers=true`, because IMAP/SMTP was on localhost. Production
  uses the public `mail.<domain>`.

## (b) docker-mailserver driven through its config volume: GO

- **Format.** One line per account in `postfix-accounts.cf`: `addr|{SHA512-CRYPT}$6$…`. Busybox
  `mkpasswd -m sha512` produced valid hashes, so the Rust `sha-crypt` crate will too.
- **Pick-up.** An atomic replace (write a temp file in the same directory, then rename) goes
  live within seconds. `check-for-changes.sh` polls every `DMS_CONFIG_POLL`, default 2 s.
  `doveadm auth test` then succeeds with the right password and fails with exit 77 on a wrong one.
- **Locks are per script, not per file.** `helpers/lock.sh` writes
  `/tmp/docker-mailserver/<script>.lock` holding a UUID, and treats a lock older than 1 minute as
  stale. The backend takes `addmailuser.lock`, `updatemailuser.lock` and `delmailuser.lock`, with
  the same stale rule, so it never races `setup email …`.
- **Cold start.** DMS refuses to start without at least one account, so the spike seeds
  `postmaster@`.

## (c) Nextcloud group sub-admin: GO, with a change

- **Design change.** On Nextcloud 34 the user-management endpoints are
  `#[PasswordConfirmationRequired]`. Every app password fails with 403 "Password confirmation is
  required", whether occ-minted or minted through `core/getapppassword`. **Basic auth with the
  sub-admin's login password works.**
  - The secret becomes `NEXTCLOUD_SUBADMIN_PASSWORD`, a login password.
  - That user must be **exempt from enforced 2FA**.
- **Scope is enforced by Nextcloud:**
  - create without the group → 106 "No group specified (required for sub-admins)";
  - another user (edit or disable) → 998, not visible;
  - a name that already exists, a human's included → **102 "User already exists"**. This is the
    collision guard.
- **What works:** create (with email, display name, `groups[]`, `quota=5GB`), get, disable,
  enable, set quota.
- **App passwords for a worker** come from `GET /ocs/v2.php/core/getapppassword` with that
  worker's login password. `occ user:auth-tokens:add` without a password yields "limited
  capabilities".

## (d) Paperclip plugin SDK: partial; the UI is tested in P2

- `PLUGIN_UI_SLOT_ENTITY_TYPES` includes `agent`, so a `detailTab` on the agent page is possible.
- **`ctx.secrets` is resolve-only** (`PluginSecretsClient.resolve`). A plugin **cannot create
  company secrets**. Credentials therefore go into the plugin's own storage (plugin DB namespace
  or `ctx.state`), encrypted with a key from a plugin-settings secret ref.
- **Agent lifecycle** (`server/dist/services/agent-lifecycle-driver.js`):
  - Every installed plugin with `agentLifecycle` + `agents.lifecycle.manage` that is not disabled
    for the company is **required for every agent, in every phase**. Returning `pending` in
    `cleaning_up` therefore blocks removal until the ID is purged.
  - The plugin must answer `complete` immediately in all other phases.
  - **Risk:** if the plugin is unavailable, lifecycle operations for **all** agents fail with
    `required_plugin_unavailable`.

## (e) Talk lobby and team folder: GO

- **Lobby.** `POST /ocs/v2.php/apps/spreed/api/v4/room` with `roomType=2`, `source=groups` and
  `invite=<group>`, as admin:
  - a user created **later** in the group is in the room automatically;
  - renaming through `PUT …/room/{token}` keeps the token.
- **Team folder.** `occ groupfolders:create`, then `groupfolders:group <id> <group> read write
  share delete` (permissions 31). A new group member sees the folder in their WebDAV root.
- Both need admin rights, so they belong to `bootstrap` only. Nothing is done per worker.

## (f) Shared mailboxes in the target deployment (`info@`, `office@`): ANSWERED

Answered by the admin: both are **aliases**, not mailboxes. Only `contact@` can be shared. An
alias cannot be shared as a mailbox; turning one into a mailbox is an admin step on the mail
server, outside this tool.

## (g) Plugin-managed Paperclip MCP connections: NO, but a better path exists

- **No connections client.** `PluginContext` has no connections or gateway API. A plugin cannot
  create a remote-MCP connection.
- **Plugin tools carry the calling agent.** `ctx.tools.register` handlers receive
  `ToolRunContext { agentId, runId, companyId, projectId }`, filled in by the host. The plugin can
  therefore be the **per-agent bridge** to nextcloud-mcp-server:
  - it attaches the calling agent's own Basic credential;
  - no credential ever leaves the plugin;
  - there is no per-agent connection setup.
- **Constraint:** tool names must be declared statically in the manifest.

## (h) nextcloud-mcp-server, `multi_user_basic`: GO

- 117 tools, Talk, Mail, Files, Calendar, Deck and the rest among them.
- **As ada:**
  - `talk_list_conversations` shows the lobby;
  - `talk_send_message` posts with `actorId: ada`;
  - `nc_mail_list_accounts` lists her mailbox.
- **Auth checks:**
  - `initialize` succeeds without credentials, but every tool call without them is refused:
    "BasicAuth credentials not found".
  - A disabled user → **HTTP 401**. After re-enabling, the **same app password works again**, so
    Re-enable needs no credential rotation.

## Other

- Paperclip `local_trusted` requires `HOST=127.0.0.1` ("local_trusted requires
  server.bind=loopback"). In the pod, other containers reach it over the shared loopback.

## Found while building P1 (2026-10-26)

- **SHA-512-crypt salt length.**
  - The `sha-crypt` crate's default salt is 16 random **bytes**, which encode to 22
    characters. crypt(3), and with it Dovecot, uses at most 16 characters.
  - Such hashes verify in Rust but are rejected by Dovecot. The cascade: every Nextcloud Mail
    login fails, Nextcloud's brute-force protection kicks in, and the API answers 429.
  - Fix: a 12-byte salt (16 characters). A regression test pins the length, and a
    Dovecot-verified hash is a test vector.
- **Uncached mailboxes.**
  - Nextcloud Mail answers `MailboxNotCachedException` (HTTP 400) for a mailbox it has never
    synced. nextcloud-mcp-server does not trigger that sync.
  - The server therefore syncs every mailbox of each Mail account it creates. This is best
    effort, so the engine never retries a creation that already succeeded.
  - Mail arriving later appears after Mail's background job. Operators should set
    `app.mail.background-sync-interval` to 300 (the minimum).
- **SQLite is not a test database for Nextcloud.** User creation failed intermittently with
  `database is locked` (CardDAV sync) and was reported as OCS 101. The spike now runs
  Nextcloud on PostgreSQL 16, with production's digest.
- **docker-mailserver applies changes asynchronously; Nextcloud throttles failed logins.**
  - What CI showed: on a slower GitHub runner, Nextcloud Mail logged in before DMS had loaded
    the new account. The failed logins tripped Nextcloud's brute-force protection, and every
    retry got HTTP 429.
  - The fix: after changing a mailbox password, the server logs in by IMAP itself (plain,
    implicit TLS or STARTTLS), and only then does Nextcloud Mail use the password. The probe
    endpoint is `mail.probe` (default `mail.imap`).
