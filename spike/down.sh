#!/usr/bin/env bash
set -euo pipefail
podman pod rm -f pcnc-spike 2>/dev/null || true
podman volume rm -f pcnc-spike-dms-config pcnc-spike-dms-mail pcnc-spike-db pcnc-spike-nc pcnc-spike-pc 2>/dev/null || true
