#!/usr/bin/env bash
set -euo pipefail

# ProjectRoot points at the repository root regardless of the caller location.
# ProjectRoot 指向仓库根目录，避免调用方当前位置影响路径解析。
PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

# Target selects which dependency group to fetch before running.
# Target 选择运行前需要拉取的依赖分组。
TARGET="${1:-none}"

# RuntimeRoot is the shared demo runtime root.
# RuntimeRoot 是共享 demo 运行根目录。
RUNTIME_ROOT="$PROJECT_ROOT/examples/ffi/standard_runtime/runtime_root"

if [ "$TARGET" != "none" ]; then
  RUNTIME_ROOT="$RUNTIME_ROOT" bash "$PROJECT_ROOT/scripts/deps/fetch_deps.sh" "$TARGET"
fi

if [ -f "$RUNTIME_ROOT/resources/runtime-env.sh" ]; then
  # shellcheck source=/dev/null
  RUNTIME_ROOT="$RUNTIME_ROOT" . "$RUNTIME_ROOT/resources/runtime-env.sh"
fi

cargo run --manifest-path "$PROJECT_ROOT/examples/demo-rust/Cargo.toml"
