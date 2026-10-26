#!/usr/bin/env bash
# mcp-call.sh <user> <app-password> <tool> <json-args>
# Calls one tool on the nextcloud-mcp-server (multi_user_basic) as <user>; prints the result
# JSON ({isError, text}) or {"http":401} when Nextcloud rejects the credentials.
set -euo pipefail
MCP=${MCP_URL:-http://127.0.0.1:18000/mcp}
HH=(-H 'Content-Type: application/json' -H 'Accept: application/json, text/event-stream')
AUTH=(-H "Authorization: Basic $(printf '%s:%s' "$1" "$2" | base64 -w0)")
SID=$(curl -s -D - -o /dev/null "${HH[@]}" "${AUTH[@]}" -X POST "$MCP" \
  -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"e2e","version":"0"}}}' \
  | awk -F': ' 'tolower($1)=="mcp-session-id"{print $2}' | tr -d '\r')
curl -s -o /dev/null "${HH[@]}" "${AUTH[@]}" -H "mcp-session-id: $SID" -X POST "$MCP" -d '{"jsonrpc":"2.0","method":"notifications/initialized"}'
curl -s "${HH[@]}" "${AUTH[@]}" -H "mcp-session-id: $SID" -X POST "$MCP" \
  -d "{\"jsonrpc\":\"2.0\",\"id\":9,\"method\":\"tools/call\",\"params\":{\"name\":\"$3\",\"arguments\":$4}}" \
  | sed -n 's/^data: //p' | jq -c '{isError: .result.isError, text: (.result.content[0].text // "")} | if (.text|test("HTTP 401")) then {http:401} else . end'
