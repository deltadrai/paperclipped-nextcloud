#!/usr/bin/env bash
# End-to-end test of the Paperclip plugin against the spike pod (spike/up.sh, with Paperclip):
# the server runs as a container in the pod, the packed plugin is installed into Paperclip,
# and the whole Identity-tab lifecycle runs through Paperclip's plugin API: create, reveal,
# MCP access, agent callers refused, agent deletion blocked until purge.
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
  'npm ci --no-fund --no-audit >/dev/null 2>&1 && npx tsc --noEmit && npx vitest run >/dev/null && node build.mjs >/dev/null 2>&1 && rm -f *.tgz && npm pack --silent --ignore-scripts' >/dev/null
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
NAME="scout.$RUNID"

data() { api POST "/plugins/$PID/data/identity" "{\"companyId\":\"$CID\",\"params\":{\"agentId\":\"$AID\",\"companyId\":\"$CID\"}}"; }
act() { api POST "/plugins/$PID/actions/$1" "{\"companyId\":\"$CID\",\"params\":{\"agentId\":\"$AID\",\"companyId\":\"$CID\"${2:+,$2}}}"; }
# The host allows 30 secret resolutions per minute per company; while an operation runs the
# tab's data costs none, afterwards one. `settle` keeps the final payload in $D.
settle() { for _ in $(seq 90); do D=$(data); jq -e '.data.pending == null' >/dev/null <<<"$D" && return; sleep 2; done; fail "operation still pending"; }
last() { jq -r ".data.lastOperation.$1" <<<"$D"; }
field() { jq -r ".data.$1" <<<"$D"; }

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

echo "== agent deletion waits for purge"
api POST "/agents/$AID/terminate" '{}' >/dev/null
for _ in $(seq 20); do [[ $(api GET "/agents/$AID" | jq -r .lifecycleState) == cleaning_up ]] && break; sleep 2; done
sleep 6
expect "cleanup blocked while the ID exists" "$(api GET "/agents/$AID" | jq -r .lifecycleState)" cleaning_up
expect "agent cannot be deleted yet" "$(api DELETE "/agents/$AID" | jq -r '.error')" "Complete termination before deleting the agent"
act purge >/dev/null; settle
expect "purge refused while active" "$(last error | cut -d: -f1)" not_disabled
act disable >/dev/null; settle
expect "delete (disable) ok" "$(field identity.state)" disabled
act enable >/dev/null; settle
expect "re-enable ok" "$(field identity.state)" active
expect "re-enabled: same MCP password works" "$(tests/e2e/mcp-call.sh "$NAME" "$NEWPW" talk_list_conversations '{}' | jq -r .isError)" false
act disable >/dev/null; settle
act purge >/dev/null; settle
expect "purge ok" "$(last ok)" true
expect "no identity left" "$(field identity)" null
expect "stored credentials gone" "$(jq -r '.data.credentials | length' <<<"$D")" 0
for _ in $(seq 30); do [[ $(api GET "/agents/$AID" | jq -r .lifecycleState) != cleaning_up ]] && break; sleep 2; done
expect "agent can be deleted now" "$(api DELETE "/agents/$AID" | jq -r '.error // "none"')" none

if [[ -n ${KEEP_DEMO:-} ]]; then
  DEMO=$(api POST "/companies/$CID/agents" '{"name":"Ada Lovelace","role":"engineer","adapterType":"process","adapterConfig":{"command":"true"}}' | jq -r .id)
  echo "demo agent: http://127.0.0.1:13100 → agent $DEMO"
fi
echo "PASS: $pass checks"
