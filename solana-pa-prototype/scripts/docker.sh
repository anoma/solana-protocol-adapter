#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"

cd "$PROJECT_DIR"

DOCKER_BIN="${DOCKER_BIN:-docker}"
DOCKER_CMD=()

if [[ -z "${HOST_UID:-}" ]]; then
  export HOST_UID
  HOST_UID="$(id -u)"
fi

if [[ -z "${HOST_GID:-}" ]]; then
  export HOST_GID
  HOST_GID="$(id -g)"
fi

if [[ -n "${DOCKER_HOST:-}" ]]; then
  DOCKER_CMD=("$DOCKER_BIN")
else
  ROOTLESS_SOCK="/run/user/$(id -u)/docker.sock"
  if [[ -S "$ROOTLESS_SOCK" && -r "$ROOTLESS_SOCK" && -w "$ROOTLESS_SOCK" ]]; then
    export DOCKER_HOST="unix://$ROOTLESS_SOCK"
    DOCKER_CMD=("$DOCKER_BIN")
  elif [[ -S "/var/run/docker.sock" && -r "/var/run/docker.sock" && -w "/var/run/docker.sock" ]]; then
    DOCKER_CMD=("$DOCKER_BIN")
  else
    echo "ERROR: Docker daemon is not accessible." >&2
    echo "Ensure Docker is running and accessible to this user." >&2
    exit 1
  fi
fi

if [[ -z "${CONTAINER_UID:-}" || -z "${CONTAINER_GID:-}" ]]; then
  if "${DOCKER_CMD[@]}" info --format '{{json .SecurityOptions}}' 2>/dev/null | grep -q rootless; then
    export CONTAINER_UID=0
    export CONTAINER_GID=0
  else
    export CONTAINER_UID="$HOST_UID"
    export CONTAINER_GID="$HOST_GID"
  fi
fi

exec "${DOCKER_CMD[@]}" "$@"
