#!/usr/bin/env bash
set -euo pipefail

# ProjectRoot points at the repository root regardless of the caller location.
# ProjectRoot 指向仓库根目录，避免调用方当前位置影响路径解析。
PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

# Target selects the debug runtime dependency group.
# Target 选择调试 runtime 需要拉取的依赖分组。
TARGET="${1:-lua}"


# RuntimeRoot receives Lua runtime packages for local debug runs.
# RuntimeRoot 接收本地调试运行所需的 Lua runtime packages。
RUNTIME_ROOT="${RUNTIME_ROOT:-output}"

mkdir -p "$RUNTIME_ROOT"
RUNTIME_ROOT="$RUNTIME_ROOT" bash "$PROJECT_ROOT/scripts/deps/fetch_deps.sh" "$TARGET"
echo "Debug runtime dependencies are ready under $RUNTIME_ROOT"
