#!/usr/bin/env bash
# End-to-end test of the Paperclip plugin against the spike pod (spike/up.sh, with Paperclip):
# the server runs as a container in the pod, the packed plugin is installed into Paperclip,
# and the whole Identity-tab lifecycle runs through Paperclip's plugin API: create, reveal,
# MCP access, agent callers refused, agent deletion blocked until purge. The panel's part, the
# agent's Paperclip MCP connection, runs from Node with the plugin's own module (mcp-sync.mjs),
# and agents call Nextcloud through Paperclip's tool gateway.
#
#   tests/plugin/spike-plugin-e2e.sh          # run the test
#   KEEP_DEMO=1 tests/plugin/spike-plugin-e2e.sh   # also leave a demo agent for the browser
set -euo pipefail
cd "$(dirname "$0")/../.."
ROOT=$PWD
PC=http://127.0.0.1:13100/api
NODE=docker.io/library/node:24-slim
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

pass=0
ok() { pass=$((pass + 1)); echo "  ok  $*"; }
fail() { echo "FAIL  $*" >&2; exit 1; }
expect() { local what=$1 got=$2 want=$3; [[ "$got" == "$want" ]] && ok "$what" || fail "$what: got [$got], want [$want]"; }
api() { curl -s -X "$1" -H 'Content-Type: application/json' "$PC$2" ${3:+-d "$3"}; }

echo "== server in the pod"
podman build -q -t localhost/paperclipped-nextcloud:dev . >/dev/null
cat > "$WORK/config.yaml" <<'YAML'
domain: example.test
nextcloud: { url: http://localhost:80, subadmin_user: workforce-provisioner }
mail:
  backend: docker-mailserver
  config_dir: /dms-config
  mail_dir: /dms-mail
  imap: { host: localhost, port: 143, security: none }
  smtp: { host: localhost, port: 587, security: none }
shared_mailboxes:
  - { key: contact, address: contact@example.test, label: contact@ (shared) }
mcp: { url: http://localhost:8000/mcp }
server: { listen: 127.0.0.1:8091, state_dir: /state }
YAML
chmod 644 "$WORK/config.yaml"
CFG=(-v "$WORK/config.yaml:/etc/paperclipped-nextcloud/config.yaml:ro,Z" -v pcnc-spike-api-state:/state)
SUB=$(podman run --rm --pod pcnc-spike "${CFG[@]}" -e PCNC_BOOTSTRAP_ADMIN_USER=admin -e PCNC_BOOTSTRAP_ADMIN_PASSWORD="${NC_ADMIN_PW:-spike-admin-pw}" \
  localhost/paperclipped-nextcloud:dev bootstrap --reset-subadmin-password 2>/dev/null | jq -r .subadmin_password)
TOKEN=$(head -c 30 /dev/urandom | base64 | tr -d '=+/')
# The registry volume outlives a run, so its state key must too (test-only fixed key).
STATE_KEY=$(printf 'spike-state-key-0123456789abcdef' | base64)
podman rm -f pcnc-spike-api >/dev/null 2>&1 || true
# The config file is copied in: the server keeps running after this script removes $WORK.
podman run -d --name pcnc-spike-api --pod pcnc-spike -v pcnc-spike-api-state:/state \
  -v pcnc-spike-dms-config:/dms-config -v pcnc-spike-dms-mail:/dms-mail \
  -e PCNC_CONFIG=/state/config.yaml -e PCNC_API_TOKEN="$TOKEN" -e PCNC_STATE_KEY="$STATE_KEY" \
  -e NEXTCLOUD_SUBADMIN_PASSWORD="$SUB" -e SHARED_MAILBOX_CONTACT_PASSWORD=contact-spike-pw \
  --entrypoint /usr/local/bin/paperclipped-nextcloud localhost/paperclipped-nextcloud:dev serve >/dev/null 2>&1 || true
podman cp "$WORK/config.yaml" pcnc-spike-api:/state/config.yaml >/dev/null
podman restart pcnc-spike-api >/dev/null
healthy=no
for _ in $(seq 20); do podman exec pcnc-spike-api /usr/local/bin/paperclipped-nextcloud --config /state/config.yaml health 2>/dev/null && { healthy=yes; break; }; sleep 1; done
expect "server healthy" "$healthy" yes

echo "== install the packed plugin"
podman run --rm -v "$ROOT/plugin":/work:Z -w /work "$NODE" sh -c \
  'npm ci --no-fund --no-audit >/dev/null 2>&1 && npx tsc --noEmit && npx vitest run >/dev/null && node build.mjs >/dev/null 2>&1 && rm -f *.tgz && npm pack --silent --ignore-scripts \
   && npx esbuild src/ui/mcp-connection.ts --bundle --format=esm --platform=node --log-level=warning --outfile=.e2e/mcp-connection.mjs' >/dev/null
TGZ=$(ls plugin/*.tgz)
podman cp "$TGZ" pcnc-spike-pc:/tmp/pcnc.tgz >/dev/null
podman exec -u node pcnc-spike-pc sh -c 'mkdir -p /paperclip/plugins-dev && cd /paperclip/plugins-dev && rm -rf paperclipped-nextcloud && tar xzf /tmp/pcnc.tgz && mv package paperclipped-nextcloud && cd paperclipped-nextcloud && npm install --omit=dev --no-fund --no-audit >/dev/null 2>&1'
PID=$(api GET /plugins | jq -r '.[]|select(.pluginKey=="deltadrai.paperclipped-nextcloud").id')
if [[ -z "$PID" ]]; then
  PID=$(api POST /plugins/install '{"packageName":"/paperclip/plugins-dev/paperclipped-nextcloud","isLocalPath":true}' | jq -r .id)
else
  # The host caches imported plugin modules: restart so the new build loads.
  podman restart pcnc-spike-pc >/dev/null
  for _ in $(seq 60); do curl -sf "$PC/health" >/dev/null && break; sleep 2; done
  api POST "/plugins/$PID/upgrade" '{}' >/dev/null
fi
expect "plugin ready" "$(api GET /plugins | jq -r --arg id "$PID" '.[]|select(.id==$id).status')" ready

echo "== company, secrets, plugin config, agent"
CID=$(api GET /companies | jq -r '.[]|select(.name=="Spike Co").id' | head -1)
[[ -n "$CID" ]] || CID=$(api POST /companies '{"name":"Spike Co"}' | jq -r .id)
RUNID=$(date +%s)
S1=$(api POST "/companies/$CID/secrets" "{\"name\":\"PCNC_API_TOKEN_$RUNID\",\"value\":\"$TOKEN\"}" | jq -r .id)
S2=$(api POST "/companies/$CID/secrets" "{\"name\":\"PCNC_CREDENTIAL_KEY_$RUNID\",\"value\":\"$(head -c32 /dev/urandom | base64)\"}" | jq -r .id)
R=$(api POST "/plugins/$PID/config" "{\"companyId\":\"$CID\",\"configJson\":{\"serverUrl\":\"http://127.0.0.1:8091\",\"apiToken\":{\"type\":\"secret_ref\",\"secretId\":\"$S1\"},\"credentialKey\":{\"type\":\"secret_ref\",\"secretId\":\"$S2\"}}}")
expect "config accepted" "$(jq -r '.error // "none"' <<<"$R")" none
AID=$(api POST "/companies/$CID/agents" "{\"name\":\"Scout $RUNID\",\"role\":\"engineer\",\"adapterType\":\"process\",\"adapterConfig\":{\"command\":\"true\"}}" | jq -r .id)
BYSTANDER=$(api POST "/companies/$CID/agents" "{\"name\":\"Bystander $RUNID\",\"role\":\"engineer\",\"adapterType\":\"process\",\"adapterConfig\":{\"command\":\"true\"}}" | jq -r .id)
NAME="scout.$RUNID"

data() { api POST "/plugins/$PID/data/identity" "{\"companyId\":\"$CID\",\"params\":{\"agentId\":\"$AID\",\"companyId\":\"$CID\"}}"; }
act() { api POST "/plugins/$PID/actions/$1" "{\"companyId\":\"$CID\",\"params\":{\"agentId\":\"$AID\",\"companyId\":\"$CID\"${2:+,$2}}}"; }
# The host allows 30 secret resolutions per minute per company; while an operation runs the
# tab's data costs none, afterwards one. `settle` keeps the final payload in $D.
settle() { for _ in $(seq 90); do D=$(data); jq -e '.data.pending == null' >/dev/null <<<"$D" && return; sleep 2; done; fail "operation still pending"; }
last() { jq -r ".data.lastOperation.$1" <<<"$D"; }
field() { jq -r ".data.$1" <<<"$D"; }
# The panel's part: sync or remove the agent's Paperclip MCP connection.
mcp() {
  podman run --rm --pod pcnc-spike -v "$ROOT/plugin/.e2e:/m:ro,Z" -v "$ROOT/tests/plugin/mcp-sync.mjs:/r.mjs:ro,Z" "$NODE" \
    node /r.mjs /m/mcp-connection.mjs http://127.0.0.1:3100/api "$PID" "$CID" "$AID" "$1"
}
conn() { data | jq -r '.data.connection.connectionId // empty'; }
# A tool call through Paperclip's gateway, as an agent.
gateway() { api POST "/tool-connections/$(conn)/test-calls" "{\"agentId\":\"$1\",\"toolName\":\"$2\",\"parameters\":{}}"; }
mailboxes() { gateway "$AID" nc_mail_list_accounts | jq -r '[.result.data.structuredContent.results[].emailAddress]|sort|join(",")' 2>/dev/null; }
tools_of() { api GET "/companies/$CID/tools/profiles/effective/agents/$1" | jq '[.allowedToolNames[]? | select(startswith("nc_") or startswith("talk_"))] | length'; }
secret_version() { api GET "/companies/$CID/secrets" | jq -r --arg n "Nextcloud MCP ($NAME)" '[.[]|select(.name==$n)][0].latestVersion // "none"'; }

expect "tab data: configured" "$(data | jq -r .data.configured)" true
expect "tab data: suggested name" "$(data | jq -r .data.suggestedName)" "$NAME"

echo "== create through the plugin"
expect "create starts" "$(act provision "\"name\":\"$NAME\",\"displayName\":\"Scout\",\"sharedMailboxes\":[\"contact\"]" | jq -r .data.started)" true
settle
expect "create ok" "$(last ok)" true
expect "identity active" "$(field identity.state)" active
expect "masked hints only" "$(jq -r '.data.credentials.mcpAppPassword | test("^.{4}….{4}$")' <<<"$D")" true
MCPPW=$(act reveal '"kind":"mcpAppPassword"' | jq -r .data.value)
expect "reveal returns the full value" "${#MCPPW}" 72
expect "agent works through MCP with it" "$(tests/e2e/mcp-call.sh "$NAME" "$MCPPW" nc_mail_list_accounts '{}' | jq -r '.text|fromjson|[.results[].emailAddress]|sort|join(",")')" "contact@example.test,$NAME@example.test"

echo "== the agent's MCP connection in Paperclip"
S=$(mcp sync)
expect "connection created and on" "$(jq -r .connection.enabled <<<"$S")" true
expect "named after the user" "$(jq -r .connection.name <<<"$S")" "Nextcloud MCP ($NAME)"
expect "recorded by the plugin" "$(conn)" "$(jq -r .connection.id <<<"$S")"
expect "repeat sync changes nothing" "$(mcp sync | jq -c .changes)" "[]"
expect "agent calls Nextcloud through Paperclip's gateway" "$(mailboxes)" "contact@example.test,$NAME@example.test"
[[ $(tools_of "$AID") -gt 0 ]] && ok "agent has the Nextcloud tools" || fail "agent has no Nextcloud tools"
expect "other agents do not" "$(tools_of "$BYSTANDER")" 0
expect "destructive tools ask first" "$(gateway "$AID" nc_mail_delete_message | jq -r .decision)" ask_first

echo "== agents are refused"
KEY=$(api POST "/agents/$AID/keys" '{"name":"e2e"}' | jq -r '.token // .key // .apiKey')
agent_act() { curl -s -X POST -H "Authorization: Bearer $KEY" -H 'Content-Type: application/json' "$PC/plugins/$PID/actions/$1" -d "{\"companyId\":\"$CID\",\"params\":{\"agentId\":\"$AID\",\"companyId\":\"$CID\",\"kind\":\"mcpAppPassword\"}}"; }
expect "agent cannot reveal" "$(agent_act reveal | jq -r '.error.message // .message')" "Only a signed-in user can manage Nextcloud identities"
expect "agent cannot purge" "$(agent_act purge | jq -r '.error.message // .message')" "Only a signed-in user can manage Nextcloud identities"

echo "== rotate"
act rotate >/dev/null; settle
expect "rotate ok" "$(last ok)" true
NEWPW=$(act reveal '"kind":"mcpAppPassword"' | jq -r .data.value)
[[ "$NEWPW" != "$MCPPW" ]] && ok "new MCP password stored" || fail "MCP password unchanged"
expect "old MCP password revoked" "$(tests/e2e/mcp-call.sh "$NAME" "$MCPPW" talk_list_conversations '{}' | jq -r .http)" 401
expect "sync writes the new credential into the secret" "$(mcp sync | jq -c .changes)" '["secret updated"]'
expect "secret has a second version" "$(secret_version)" 2
expect "gateway works with the rotated credential" "$(mailboxes)" "contact@example.test,$NAME@example.test"

echo "== purge removes the connection; a new ID reuses its archived app"
act disable >/dev/null; settle
expect "disabled ID: connection off" "$(mcp sync | jq -r .connection.enabled)" false
FIRST_APP=$(api GET "/tool-connections/$(conn)" | jq -r .applicationId)
mcp remove >/dev/null
act purge >/dev/null; settle
expect "purged" "$(field identity)" null
expect "no live connection left" "$(api GET "/companies/$CID/tools/connections" | jq --arg n "Nextcloud MCP ($NAME)" '[.connections[]|select(.name==$n and .status!="archived")]|length')" 0
expect "secret deleted" "$(secret_version)" none
expect "record cleared" "$(field connection)" null
act provision "\"name\":\"$NAME\",\"displayName\":\"Scout\",\"sharedMailboxes\":[\"contact\"]" >/dev/null; settle
expect "re-created" "$(field identity.state)" active
expect "sync reuses the archived app" "$(mcp sync | jq -r '.changes|index("connection created (existing app reused)") != null')" true
expect "same tool application" "$(api GET "/tool-connections/$(conn)" | jq -r .applicationId)" "$FIRST_APP"
expect "gateway works after re-creation" "$(mailboxes)" "contact@example.test,$NAME@example.test"

echo "== delete (disable) and re-enable switch the connection"
NEWPW=$(act reveal '"kind":"mcpAppPassword"' | jq -r .data.value)
act disable >/dev/null; settle
expect "delete (disable) ok" "$(field identity.state)" disabled
expect "connection off" "$(mcp sync | jq -r .connection.enabled)" false
[[ $(gateway "$AID" nc_mail_list_accounts | jq -r '.result.data.isError // "refused"') != false ]] && ok "gateway refuses while disabled" || fail "gateway still serves a disabled ID"
act enable >/dev/null; settle
expect "re-enable ok" "$(field identity.state)" active
expect "re-enabled: same MCP password works" "$(tests/e2e/mcp-call.sh "$NAME" "$NEWPW" talk_list_conversations '{}' | jq -r .isError)" false
expect "connection on again" "$(mcp sync | jq -r .connection.enabled)" true
expect "gateway works again" "$(mailboxes)" "contact@example.test,$NAME@example.test"

echo "== agent deletion waits for purge"
api POST "/agents/$AID/terminate" '{}' >/dev/null
for _ in $(seq 20); do [[ $(api GET "/agents/$AID" | jq -r .lifecycleState) == cleaning_up ]] && break; sleep 2; done
sleep 6
expect "cleanup blocked while the ID exists" "$(api GET "/agents/$AID" | jq -r .lifecycleState)" cleaning_up
expect "agent cannot be deleted yet" "$(api DELETE "/agents/$AID" | jq -r '.error')" "Complete termination before deleting the agent"
act purge >/dev/null; settle
expect "purge refused while active" "$(last error | cut -d: -f1)" not_disabled
# Paperclip refuses tool calls for a terminated agent; only the ID and its connection remain.
act disable >/dev/null; settle
expect "terminated agent: disable ok" "$(field identity.state)" disabled
expect "terminated agent: connection off" "$(mcp sync | jq -r .connection.enabled)" false
mcp remove >/dev/null
act purge >/dev/null; settle
expect "purge ok" "$(last ok)" true
expect "no identity left" "$(field identity)" null
expect "stored credentials gone" "$(jq -r '.data.credentials | length' <<<"$D")" 0
expect "connection gone" "$(api GET "/companies/$CID/tools/connections" | jq --arg n "Nextcloud MCP ($NAME)" '[.connections[]|select(.name==$n and .status!="archived")]|length')" 0
for _ in $(seq 30); do [[ $(api GET "/agents/$AID" | jq -r .lifecycleState) != cleaning_up ]] && break; sleep 2; done
expect "agent can be deleted now" "$(api DELETE "/agents/$AID" | jq -r '.error // "none"')" none
api POST "/agents/$BYSTANDER/terminate" '{}' >/dev/null

if [[ -n ${KEEP_DEMO:-} ]]; then
  DEMO=$(api POST "/companies/$CID/agents" '{"name":"Ada Lovelace","role":"engineer","adapterType":"process","adapterConfig":{"command":"true"}}' | jq -r .id)
  echo "demo agent: http://127.0.0.1:13100 → agent $DEMO"
fi
echo "PASS: $pass checks"
