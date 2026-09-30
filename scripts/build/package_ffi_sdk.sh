#!/usr/bin/env bash
set -euo pipefail

# RepositoryRoot comes from this script's known location.
# RepositoryRoot 来自此脚本的已知位置。
REPOSITORY_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
# Arguments are forwarded unchanged to the single cross-platform candidate schema.
# 参数原样转发给唯一跨平台候选结构。
exec python3 "$REPOSITORY_ROOT/scripts/release/candidate.py" package --root "$REPOSITORY_ROOT" "$@"
