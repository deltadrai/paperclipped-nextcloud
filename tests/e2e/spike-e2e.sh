#!/usr/bin/env bash
# End-to-end test of the server against the P0 spike pod (spike/up.sh): bootstrap, the full
# identity lifecycle through the HTTP API, and the agent's view through nextcloud-mcp-server.
#
# The server runs under `podman unshare`, so it can write docker-mailserver's volumes with the
# ids the containers expect, while reaching Nextcloud on the pod's published port.
set -euo pipefail
cd "$(dirname "$0")/../.."
HERE=tests/e2e
cargo build -q -p paperclipped-nextcloud
BIN=$PWD/target/debug/paperclipped-nextcloud

WORK=$(mktemp -d)
SERVER=
stop_server() { pkill -f "$BIN serve" 2>/dev/null || true; }
trap 'stop_server; podman unshare rm -rf "$WORK"' EXIT
DMS_CONFIG=$(podman volume inspect pcnc-spike-dms-config --format '{{.Mountpoint}}')
DMS_MAIL=$(podman volume inspect pcnc-spike-dms-mail --format '{{.Mountpoint}}')
NC=http://127.0.0.1:18080
API=http://127.0.0.1:18091
ADMIN=(-u "admin:${NC_ADMIN_PW:-spike-admin-pw}")
OCS=(-H "OCS-APIRequest: true" -H "Accept: application/json")

cat > "$WORK/config.yaml" <<YAML
domain: example.test
nextcloud: { url: $NC, subadmin_user: workforce-provisioner, group: digital workforce, quota: 5 GB }
mail:
  backend: docker-mailserver
  config_dir: $DMS_CONFIG
  mail_dir: $DMS_MAIL
  imap: { host: localhost, port: 143, security: none }
  smtp: { host: localhost, port: 587, security: none }
  probe: { host: 127.0.0.1, port: 18143, security: none }
shared_mailboxes:
  - { key: contact, address: contact@example.test, label: contact@ (shared) }
bootstrap: { team_folder: digital workforce, lobby: e2e lobby }
mcp: { url: http://127.0.0.1:18000/mcp }
server: { listen: 127.0.0.1:18091, state_dir: $WORK/state, rate_limit_per_minute: 100 }
YAML

export PCNC_CONFIG=$WORK/config.yaml
export PCNC_API_TOKEN=e2e-token-0123456789abcdef0123456789abcdef
export PCNC_STATE_KEY=$(head -c32 /dev/urandom | base64)
export SHARED_MAILBOX_CONTACT_PASSWORD=contact-spike-pw
pcnc() { podman unshare "$BIN" "$@"; }

pass=0
ok() { pass=$((pass + 1)); echo "  ok  $*"; }
fail() { echo "FAIL  $*" >&2; exit 1; }
expect() { local what=$1 got=$2 want=$3; [[ "$got" == "$want" ]] && ok "$what" || fail "$what: got [$got], want [$want]"; }

# Leftovers of an aborted earlier run: the registry lives in $WORK, so they would be refused.
for u in scout r1; do
  curl -s -o /dev/null "${OCS[@]}" "${ADMIN[@]}" -X DELETE "$NC/ocs/v2.php/cloud/users/$u"
  podman unshare sed -i "/^$u@example.test|/d" "$DMS_CONFIG/postfix-accounts.cf"
  podman unshare rm -rf "$DMS_MAIL/example.test/$u"
done

echo "== fixtures: a human user and a worker created by hand, neither by this server"
curl -s -o /dev/null "${OCS[@]}" "${ADMIN[@]}" -X POST "$NC/ocs/v2.php/cloud/users" -d userid=probe1 -d 'password=Probe-login-pw-9x!Q'
curl -s -o /dev/null "${OCS[@]}" "${ADMIN[@]}" -X POST "$NC/ocs/v2.php/cloud/groups" --data-urlencode groupid="digital workforce"
curl -s -o /dev/null "${OCS[@]}" "${ADMIN[@]}" -X POST "$NC/ocs/v2.php/cloud/users" -d userid=ada -d 'password=Ada-login-pw-9x!Q' --data-urlencode 'groups[]=digital workforce'

echo "== bootstrap"
boot() { PCNC_BOOTSTRAP_ADMIN_USER=admin PCNC_BOOTSTRAP_ADMIN_PASSWORD=${NC_ADMIN_PW:-spike-admin-pw} pcnc bootstrap "$@"; }
B1=$(boot --reset-subadmin-password)
export NEXTCLOUD_SUBADMIN_PASSWORD=$(jq -r .subadmin_password <<<"$B1")
expect "bootstrap returns the sub-admin password once" "${#NEXTCLOUD_SUBADMIN_PASSWORD}" 32
expect "bootstrap has a lobby" "$(jq -r '.state.lobby_token != null' <<<"$B1")" true
expect "bootstrap has a team folder" "$(jq -r '.state.team_folder.kind' <<<"$B1")" groupfolder
B2=$(boot)
expect "second bootstrap is all unchanged" "$(jq -r '[.steps[].outcome]|unique|join(",")' <<<"$B2")" unchanged
expect "second bootstrap prints no password" "$(jq -r '.subadmin_password // "none"' <<<"$B2")" none
LOBBY=$(jq -r '.state.lobby_token' <<<"$B2")

echo "== serve"
podman unshare "$BIN" serve >/dev/null 2>"$WORK/serve.log" &
for _ in $(seq 50); do curl -sf $API/healthz >/dev/null && break; sleep 0.2; done
T=(-H "Authorization: Bearer $PCNC_API_TOKEN")
api() { local m=$1 p=$2; shift 2; curl -s -X "$m" "${T[@]}" -H 'Content-Type: application/json' "$API$p" "$@"; }
code() { local m=$1 p=$2; shift 2; curl -s -o /dev/null -w '%{http_code}' -X "$m" "$API$p" "$@"; }

expect "no token → 401" "$(code GET /v1/info)" 401
expect "wrong token → 401" "$(code GET /v1/info -H 'Authorization: Bearer nope')" 401
expect "info lists contact" "$(api GET /v1/info | jq -r '.sharedMailboxes[0].key')" contact

echo "== provision"
R=$(api PUT /v1/identities/agent-scout -d '{"name":"scout","displayName":"Scout (digital worker)","sharedMailboxes":["contact"]}')
jq -e .steps <<<"$R" >/dev/null || fail "provision: $R"
expect "create: every step created" "$(jq -r '[.steps[].outcome]|unique|join(",")' <<<"$R")" created
LOGIN=$(jq -r '.credentials.nextcloudLoginPassword' <<<"$R")
MCPPW=$(jq -r '.credentials.mcpAppPassword' <<<"$R")
[[ ${#LOGIN} -eq 32 && ${#MCPPW} -gt 20 ]] && ok "credentials returned once" || fail "credentials: $R"
R=$(api PUT /v1/identities/agent-scout -d '{"name":"scout","displayName":"Scout (digital worker)","sharedMailboxes":["contact"]}')
expect "re-sync: every step unchanged" "$(jq -r '[.steps[].outcome]|unique|join(",")' <<<"$R")" unchanged
expect "re-sync: no credentials" "$(jq -r '.credentials // "none"' <<<"$R")" none
expect "inspect: all checks ok" "$(api GET /v1/identities/agent-scout/inspect | jq -r '[.checks[].ok]|all')" true

U=$(curl -s "${OCS[@]}" "${ADMIN[@]}" "$NC/ocs/v2.php/cloud/users/scout")
expect "nextcloud: email" "$(jq -r '.ocs.data.email' <<<"$U")" scout@example.test
expect "nextcloud: group" "$(jq -r '.ocs.data.groups|join(",")' <<<"$U")" "digital workforce"
expect "nextcloud: quota 5 GB" "$(jq -r '.ocs.data.quota.quota' <<<"$U")" 5368709120
expect "dms: account line" "$(podman unshare grep -c '^scout@example.test|{SHA512-CRYPT}\$6\$' "$DMS_CONFIG/postfix-accounts.cf")" 1

echo "== the agent's view through MCP"
expect "mcp: mail accounts" "$($HERE/mcp-call.sh scout "$MCPPW" nc_mail_list_accounts '{}' | jq -r '.text|fromjson|[.results[].emailAddress]|sort|join(",")')" "contact@example.test,scout@example.test"
expect "mcp: lobby visible" "$($HERE/mcp-call.sh scout "$MCPPW" talk_list_conversations '{}' | jq -r --arg t "$LOBBY" '.text|fromjson|[.results[]|select(.token==$t)]|length')" 1
expect "mcp: posts in the lobby as scout" "$($HERE/mcp-call.sh scout "$MCPPW" talk_send_message "{\"token\":\"$LOBBY\",\"message\":\"Scout reporting\"}" | jq -r '.text|fromjson|.message.actorId')" scout
expect "webdav: team folder visible" "$(curl -s -u "scout:$LOGIN" -X PROPFIND -H 'Depth: 1' "$NC/remote.php/dav/files/scout/" | grep -c 'digital%20workforce/')" 1

echo "== the worker's mail through MCP"
ACCT=$($HERE/mcp-call.sh scout "$MCPPW" nc_mail_list_accounts '{}' | jq -r '.text|fromjson|.results[]|select(.emailAddress=="scout@example.test").id')
INBOX=$($HERE/mcp-call.sh scout "$MCPPW" nc_mail_list_mailboxes "{\"account_id\":$ACCT}" | jq -r '.text|fromjson|.results[]|select(.specialUse|index("inbox")).databaseId')
expect "new mailbox is readable right away (initial sync)" "$($HERE/mcp-call.sh scout "$MCPPW" nc_mail_list_messages "{\"mailbox_id\":$INBOX}" | jq -r .isError)" false
podman exec pcnc-spike-dms sh -c 'printf "Subject: e2e hello\n\nhello scout\n" | sendmail scout@example.test'
sleep 3
# Stands in for Mail's background sync job (app.mail.background-sync-interval).
podman exec -u www-data pcnc-spike-nc php occ mail:account:sync "$ACCT" >/dev/null
expect "delivered mail readable via MCP" "$($HERE/mcp-call.sh scout "$MCPPW" nc_mail_list_messages "{\"mailbox_id\":$INBOX}" | jq -r '.text|test("e2e hello")')" true

echo "== collisions"
expect "existing worker not created here → 409" "$(api PUT /v1/identities/agent-x -d '{"name":"ada","displayName":"X"}' | jq -r .error)" nextcloud_user_exists
expect "human user outside the group → 409" "$(api PUT /v1/identities/agent-y -d '{"name":"probe1","displayName":"Y"}' | jq -r .error)" nextcloud_user_exists
expect "refused identity leaves no record" "$(code GET /v1/identities/agent-y "${T[@]}")" 404
expect "shared mailbox local part reserved" "$(api PUT /v1/identities/agent-z -d '{"name":"contact","displayName":"Z"}' | jq -r .error)" invalid_name
expect "unknown shared mailbox → 400" "$(api PUT /v1/identities/agent-scout -d '{"name":"scout","displayName":"Scout (digital worker)","sharedMailboxes":["office"]}' | jq -r .error)" unknown_shared_mailbox

echo "== shared mailbox revoke and re-grant"
R=$(api PUT /v1/identities/agent-scout -d '{"name":"scout","displayName":"Scout (digital worker)","sharedMailboxes":[]}')
expect "revoke contact" "$(jq -r '.steps[]|select(.step=="shared:contact").outcome' <<<"$R")" removed
R=$(api PUT /v1/identities/agent-scout -d '{"name":"scout","displayName":"Scout (digital worker)","sharedMailboxes":["contact"]}')
expect "grant contact" "$(jq -r '.steps[]|select(.step=="shared:contact").outcome' <<<"$R")" created

echo "== rotate"
R=$(api POST /v1/identities/agent-scout/rotate)
NEWMCP=$(jq -r '.credentials.mcpAppPassword' <<<"$R")
expect "rotate returns a new login password" "$(jq -r '.credentials.nextcloudLoginPassword|length' <<<"$R")" 32
expect "old MCP password revoked" "$($HERE/mcp-call.sh scout "$MCPPW" talk_list_conversations '{}' | jq -r .http)" 401
expect "new MCP password works" "$($HERE/mcp-call.sh scout "$NEWMCP" talk_list_conversations '{}' | jq -r .isError)" false
R=$(api PUT /v1/identities/agent-scout -d '{"name":"scout","displayName":"Scout (digital worker)","sharedMailboxes":["contact"]}')
expect "after rotate: re-sync unchanged" "$(jq -r '[.steps[].outcome]|unique|join(",")' <<<"$R")" unchanged

echo "== delete (disable), re-enable, purge"
expect "purge while active → 409" "$(api DELETE /v1/identities/agent-scout | jq -r .error)" not_disabled
R=$(api POST /v1/identities/agent-scout/disable)
expect "disable: state" "$(jq -r .identity.state <<<"$R")" disabled
expect "disable: shared removed" "$(jq -r '.steps[]|select(.step=="shared:contact").outcome' <<<"$R")" removed
expect "disabled: MCP rejected" "$($HERE/mcp-call.sh scout "$NEWMCP" talk_list_conversations '{}' | jq -r .http)" 401
expect "disable again: unchanged" "$(api POST /v1/identities/agent-scout/disable | jq -r '[.steps[].outcome]|unique|join(",")')" unchanged
expect "provision while disabled → 409" "$(api PUT /v1/identities/agent-scout -d '{"name":"scout","displayName":"S"}' | jq -r .error)" identity_disabled
expect "inspect disabled: all ok" "$(api GET /v1/identities/agent-scout/inspect | jq -r '[.checks[].ok]|all')" true
R=$(api POST /v1/identities/agent-scout/enable)
expect "enable: state" "$(jq -r .identity.state <<<"$R")" active
expect "enable: no new credentials" "$(jq -r '.credentials // "none"' <<<"$R")" none
expect "enabled: same MCP password works" "$($HERE/mcp-call.sh scout "$NEWMCP" nc_mail_list_accounts '{}' | jq -r '.text|fromjson|.results|length')" 2
api POST /v1/identities/agent-scout/disable >/dev/null
R=$(api DELETE /v1/identities/agent-scout)
expect "purge: registry removed" "$(jq -r '.steps[]|select(.step=="registry").outcome' <<<"$R")" removed
expect "purge: identity gone" "$(code GET /v1/identities/agent-scout "${T[@]}")" 404
expect "purge: nextcloud user gone" "$(curl -s "${OCS[@]}" "${ADMIN[@]}" "$NC/ocs/v2.php/cloud/users/scout" | jq -r .ocs.meta.statuscode)" 404
expect "purge: dms account gone" "$(podman unshare grep -c '^scout@' "$DMS_CONFIG/postfix-accounts.cf" || true)" 0
expect "purge: maildir gone" "$(podman unshare test -e "$DMS_MAIL/example.test/scout" && echo yes || echo no)" no

echo "== reshare (CLI) and audit"
api PUT /v1/identities/agent-r1 -d '{"name":"r1","displayName":"R1","sharedMailboxes":["contact"]}' >/dev/null
stop_server; sleep 1
expect "reshare updates the granted worker" "$(pcnc reshare contact | jq -r '.[0].outcome')" updated
expect "audit log has no secrets" "$(podman unshare grep -c "$LOGIN\|$NEWMCP\|contact-spike-pw" "$WORK/state/audit.log" || true)" 0
pcnc disable agent-r1 >/dev/null && pcnc purge agent-r1 >/dev/null && ok "cleanup r1"

echo "PASS: $pass checks"
