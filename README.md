# paperclipped-nextcloud

Give the AI agents you run in [Paperclip](https://github.com/paperclipai/paperclip) their own
identity in your Nextcloud and on your mail server. Each digital worker gets:

- a **Nextcloud user** in a dedicated group (default `digital workforce`): its own Talk handle,
  files, calendar;
- its **own mailbox**, `name@your-domain`, already set up in its Nextcloud Mail;
- optionally, access to **shared company mailboxes** (`contact@`, `info@`, …) in its Nextcloud
  Mail;
- the group's **team folder** and **Talk lobby**, through group membership;
- an **app password for the [Nextcloud MCP server](https://github.com/cbcoutinho/nextcloud-mcp-server)**,
  which is how the agent works in Nextcloud, as itself.

It has two parts:

| Part | What it is |
|---|---|
| **Server** (`paperclipped-nextcloud`, Rust) | Runs next to your mail server and holds the privileged access. HTTP API and CLI. |
| **Paperclip plugin** (npm `paperclipped-nextcloud`, [`plugin/`](plugin/README.md)) | A *Nextcloud identities* page (and, on newer Paperclip releases, a tab on each agent's page): create, delete (disable), re-enable, purge; masked credentials with copy; blocks deleting an agent until its ID is purged. |

## Requirements

| Component | Tested with | Role |
|---|---|---|
| [docker-mailserver](https://github.com/docker-mailserver/docker-mailserver) | v16.0.1 | Mail. The server edits DMS's config volume, as `setup email …` does. |
| Nextcloud | 34.0.5 | Users, group, files |
| Nextcloud Mail | 5.12.5 | The workers' webmail |
| Nextcloud Talk | 24.0.6 | The lobby |
| Team folders (`groupfolders`) | 22.0.6 | Optional; without it, a folder shared to the group |
| nextcloud-mcp-server | 0.198.3 | A **separate instance** in `MCP_DEPLOYMENT_MODE=multi_user_basic`, never exposed publicly |
| Paperclip | 2026.1023.0 | The plugin |

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

1. **Run a second nextcloud-mcp-server** in `multi_user_basic` mode, reachable only from
   Paperclip.
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
