#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)"

printf 'kley-session.sh now forwards to the agnostic runner.\n' >&2
exec "$SCRIPT_DIR/kley-run.sh" "$@"
