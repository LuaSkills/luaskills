#!/usr/bin/env bash
set -euo pipefail

# ScriptDir stores the current script directory.
# ScriptDir 保存当前脚本目录。
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# RepoRoot stores the repository root inferred from this script location.
# RepoRoot 保存根据当前脚本位置推导出的仓库根目录。
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

# RuntimeRoot stores an isolated LuaSkills runtime root for this smoke run.
# RuntimeRoot 保存本次冒烟运行使用的隔离 LuaSkills 运行时根目录。
RUNTIME_ROOT=""

# SkipFetch allows reusing an already prepared runtime root during local iteration.
# SkipFetch 允许本地迭代时复用已经准备好的运行时根目录。
SKIP_FETCH=0

# KeepRuntimeRoot keeps the isolated runtime root after a successful smoke run.
# KeepRuntimeRoot 在冒烟运行成功后保留隔离运行时根目录。
KEEP_RUNTIME_ROOT=0

usage() {
  # Print command usage for the POSIX managed runtime smoke script.
  # 输出 POSIX 受管运行时冒烟脚本的命令用法。
  cat <<'USAGE'
Usage:
  managed_runtime_smoke.sh [--runtime-root <dir>] [--skip-fetch] [--keep-runtime-root]

Options:
  --runtime-root <dir>   Use this runtime root instead of an isolated target directory.
  --skip-fetch           Reuse an already prepared runtime root.
  --keep-runtime-root    Do not remove an isolated runtime root after success.
USAGE
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --runtime-root)
      RUNTIME_ROOT="${2:?--runtime-root requires a directory}"
      shift 2
      ;;
    --skip-fetch)
      SKIP_FETCH=1
      shift
      ;;
    --keep-runtime-root)
      KEEP_RUNTIME_ROOT=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Unknown argument: $1" >&2
      usage >&2
      exit 1
      ;;
  esac
done

if [ -z "$RUNTIME_ROOT" ]; then
  RUNTIME_ROOT="$REPO_ROOT/target/managed-runtime-smoke/run-$(date +%s%3N)"
elif [[ "$RUNTIME_ROOT" != /* ]]; then
  RUNTIME_ROOT="$REPO_ROOT/$RUNTIME_ROOT"
fi

# RuntimeRoot stores the normalized absolute runtime root path.
# RuntimeRoot 保存规范化后的绝对运行时根目录路径。
RUNTIME_ROOT="$(python3 - "$RUNTIME_ROOT" <<'PY'
import os
import sys

print(os.path.abspath(sys.argv[1]))
PY
)"

# SkillPath stores the example skill used for end-to-end managed runtime verification.
# SkillPath 保存用于端到端受管运行时验证的示例 skill。
SKILL_PATH="$REPO_ROOT/examples/managed_runtime/managed-child-runtime-debug"

# FetchScript stores the managed runtime dependency fetcher.
# FetchScript 保存受管运行时依赖拉取脚本。
FETCH_SCRIPT="$REPO_ROOT/scripts/deps/fetch_managed_runtimes.sh"

# LayoutCheckScript stores the managed runtime layout validator.
# LayoutCheckScript 保存受管运行时布局校验器。
LAYOUT_CHECK_SCRIPT="$REPO_ROOT/scripts/debug-tools/managed_runtime_layout_check.py"

cleanup() {
  # Remove the isolated runtime root after success when it is safe to do so.
  # 在安全情况下于成功后移除隔离运行时根目录。
  local status="$1"
  if [ "$status" -eq 0 ] && [ "$KEEP_RUNTIME_ROOT" -ne 1 ]; then
    case "$RUNTIME_ROOT" in
      "$REPO_ROOT"/target/managed-runtime-smoke/*)
        rm -rf "$RUNTIME_ROOT" || true
        ;;
    esac
  fi
}

trap 'cleanup $?' EXIT

if [ "$SKIP_FETCH" -ne 1 ]; then
  echo "Fetching managed runtimes into $RUNTIME_ROOT"
  RUNTIME_ROOT="$RUNTIME_ROOT" FORCE=1 "$FETCH_SCRIPT" all
fi

echo "Validating managed runtime layout"
python3 "$LAYOUT_CHECK_SCRIPT" "$RUNTIME_ROOT"

echo "Calling managed runtime debug skill"
OUTPUT="$(
  cd "$REPO_ROOT"
  cargo run --bin luaskills-debug -- \
    call \
    --runtime-root "$RUNTIME_ROOT" \
    --skill-path "$SKILL_PATH" \
    --tool smoke \
    --args-json '{"text":"smoke-script"}' \
    --output content
)"

python3 - "$OUTPUT" <<'PY'
import json
import sys

payload = json.loads(sys.argv[1])

checks = [
    (payload["python_first"]["ok"], "python_first did not return ok=true"),
    (payload["python_second"]["worker_reused"], "python worker was not reused"),
    (payload["python_status_after"]["ready"], "python environment is not ready after call"),
    (payload["python_first"]["value"]["dependency"] == "24.2", "python dependency did not load"),
    (payload["python_first"]["value"]["text"] == "smoke-script", "python text argument did not round-trip"),
    (payload["python_first"]["value"]["number"] == 41, "python numeric result mismatch"),
    (payload["node_first"]["ok"], "node_first did not return ok=true"),
    (payload["node_second"]["worker_reused"], "node worker was not reused"),
    (payload["node_status_after"]["ready"], "node environment is not ready after call"),
    (payload["node_first"]["value"]["dependency"] == "is-odd", "node dependency did not load"),
    (payload["node_first"]["value"]["namedImport"] == "is-number-named", "node named import did not load"),
    (payload["node_first"]["value"]["namespaceImport"] == "is-number-namespace", "node namespace import did not load"),
    (payload["node_first"]["value"]["relativeImport"] == "local-helper", "node relative import did not load"),
    (payload["node_first"]["value"]["sideEffectImport"] == "side-effect", "node side-effect import did not load"),
    (payload["node_first"]["value"]["text"] == "smoke-script", "node text argument did not round-trip"),
    (payload["node_first"]["value"]["number"] == 42, "node numeric result mismatch"),
]
for ok, message in checks:
    if not ok:
        raise SystemExit(message)
PY

echo "Managed runtime smoke passed"
echo "Runtime root: $RUNTIME_ROOT"
