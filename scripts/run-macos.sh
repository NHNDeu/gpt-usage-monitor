#!/bin/sh
set -eu

PROJECT_ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
"$PROJECT_ROOT/scripts/package-macos.sh"
open "$PROJECT_ROOT/dist/Codex Usage Monitor.app"
