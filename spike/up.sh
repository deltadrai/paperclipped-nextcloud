#!/usr/bin/env bash
# Local test environment: one rootless podman pod with docker-mailserver, Nextcloud on
# PostgreSQL (+ Mail, Talk, Team folders), nextcloud-mcp-server (multi_user_basic) and Paperclip
# (local_trusted). Idempotent; down.sh removes it. Images are pinned to the digests deployed on
# deltadrai's nodes on 2026-10-26. Everything inside the pod talks over localhost.
#
# Published on 127.0.0.1: Nextcloud :18080, MCP :18000, IMAP :18143. Paperclip binds loopback inside the pod.
set -euo pipefail
POD=pcnc-spike
DMS_IMG=ghcr.io/docker-mailserver/docker-mailserver@sha256:a69ddc47bbcd085a2a6f23fd31f371d54b2f8495f7266a7803a3e272e049556b
PG_IMG=docker.io/library/postgres@sha256:ed5a1fad193768f89265c7c297999bab9aa116e82142f6e38bc33b8587b2f2da
NC_IMG=docker.io/library/nextcloud@sha256:82285b28c116c0dd13b2641ec9425830ebfb6c22abf6583810dab3efa3fc25ec
MCP_IMG=ghcr.io/cbcoutinho/nextcloud-mcp-server@sha256:55073a7e88ded2177e2599c7f06d7a0cbf8049e0b5745397a68982c609f3ffb2
PC_IMG=ghcr.io/paperclipai/paperclip@sha256:40f46940c4612ead131aebe6079cd4651f8cd1434af49f7c621887f4a452a62e
BUSYBOX=docker.io/library/busybox:latest
NC_ADMIN_PW=${NC_ADMIN_PW:-spike-admin-pw}

podman pod exists "$POD" || podman pod create --name "$POD" --hostname mail.example.test \
  -p 127.0.0.1:18080:80 -p 127.0.0.1:18000:8000 -p 127.0.0.1:18143:143

run() { local name=$1; shift; podman container exists "$POD-$name" || podman run -d --pod "$POD" --name "$POD-$name" "$@"; }

# docker-mailserver refuses to start without an account: seed postmaster@ and contact@.
podman volume exists pcnc-spike-dms-config || {
  podman volume create pcnc-spike-dms-config >/dev/null
  podman run --rm -v pcnc-spike-dms-config:/c "$BUSYBOX" sh -c '
    printf "postmaster@example.test|{SHA512-CRYPT}%s\ncontact@example.test|{SHA512-CRYPT}%s\n" \
      "$(mkpasswd -m sha512 postmaster-spike-pw)" "$(mkpasswd -m sha512 contact-spike-pw)" > /c/postfix-accounts.cf'
}
run dms \
  -e PERMIT_DOCKER=none -e SSL_TYPE= -e POSTFIX_INET_PROTOCOLS=ipv4 -e SPOOF_PROTECTION=1 \
  -e ENABLE_RSPAMD=0 -e ENABLE_CLAMAV=0 -e ENABLE_FAIL2BAN=0 -e ENABLE_POSTGREY=0 \
  -e ENABLE_AMAVIS=0 -e ENABLE_SPAMASSASSIN=0 -e ENABLE_OPENDKIM=0 -e ENABLE_OPENDMARC=0 \
  -v pcnc-spike-dms-config:/tmp/docker-mailserver -v pcnc-spike-dms-mail:/var/mail \
  "$DMS_IMG"

run db -e POSTGRES_DB=nextcloud -e POSTGRES_USER=nextcloud -e POSTGRES_PASSWORD=spike-db-pw \
  -v pcnc-spike-db:/var/lib/postgresql/data "$PG_IMG"

run nc \
  -e POSTGRES_HOST=127.0.0.1 -e POSTGRES_DB=nextcloud -e POSTGRES_USER=nextcloud -e POSTGRES_PASSWORD=spike-db-pw \
  -e NEXTCLOUD_ADMIN_USER=admin -e NEXTCLOUD_ADMIN_PASSWORD="$NC_ADMIN_PW" \
  -e NEXTCLOUD_TRUSTED_DOMAINS="localhost 127.0.0.1" \
  -v pcnc-spike-nc:/var/www/html \
  "$NC_IMG"

occ() { podman exec -u www-data "$POD-nc" php occ "$@"; }
echo "waiting for Nextcloud to install…"
for _ in $(seq 120); do
  occ status --output=json 2>/dev/null | grep -q '"installed":true' && break
  sleep 5
done
occ status --output=json | grep -q '"installed":true' || { echo "Nextcloud did not install" >&2; exit 1; }
for app in mail spreed groupfolders; do
  occ app:list --output=json | grep -q "\"$app\":" || occ app:install "$app"
done
# Nextcloud Mail reaches docker-mailserver on localhost inside the pod.
occ config:system:set allow_local_remote_servers --value true --type bool >/dev/null
# No outgoing mail server in the spike: do not try to send welcome mails.
occ config:app:set core newUser.sendEmail --value no >/dev/null

run mcp \
  -e MCP_DEPLOYMENT_MODE=multi_user_basic -e NEXTCLOUD_HOST=http://localhost:80 \
  "$MCP_IMG" --transport streamable-http --port 8000 --log-level info

# SKIP_PAPERCLIP=1: the server's tests do not need Paperclip (a large image).
[[ -n ${SKIP_PAPERCLIP:-} ]] || run pc \
  -e PAPERCLIP_DEPLOYMENT_MODE=local_trusted -e HOST=127.0.0.1 -e PORT=3100 \
  -v pcnc-spike-pc:/paperclip \
  "$PC_IMG"

podman pod ps --filter name="$POD"
