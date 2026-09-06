#!/usr/bin/env bash
set -euo pipefail

# ProjectRoot points at the repository root regardless of the caller location.
# ProjectRoot 指向仓库根目录，避免调用方当前位置影响路径解析。
PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

# RuntimeRoot receives installed FFI headers, libraries, and license files.
# RuntimeRoot 接收已安装的 FFI 头文件、动态库与授权文件。
RUNTIME_ROOT="${RUNTIME_ROOT:-output}"

# LuaSkillsRepo stores the GitHub repository for LuaSkills FFI SDK assets.
# LuaSkillsRepo 保存 LuaSkills FFI SDK 资产所在的 GitHub 仓库。
LUASKILLS_REPO="${LUASKILLS_REPO:-LuaSkills/luaskills}"

# LuaSkillsVersion stores the GitHub Release tag for LuaSkills FFI SDK assets.
# LuaSkillsVersion 保存 LuaSkills FFI SDK 资产的 GitHub Release 标签。
LUASKILLS_VERSION="${LUASKILLS_VERSION:-}"

ensure_dir() {
  # Create one directory when it does not exist.
  # 在目录不存在时创建该目录。
  mkdir -p "$1"
}

platform_key() {
  # Resolve the current luaskills FFI SDK asset platform key.
  # 解析当前 luaskills FFI SDK 资产平台标识。
  case "$(uname -s)" in
    Linux) os_key="linux" ;;
    Darwin) os_key="macos" ;;
    *) echo "Unsupported operating system: $(uname -s)" >&2; return 1 ;;
  esac
  case "$(uname -m)" in
    x86_64|amd64) arch_key="x64" ;;
    aarch64|arm64) arch_key="arm64" ;;
    *) echo "Unsupported architecture: $(uname -m)" >&2; return 1 ;;
  esac
  printf '%s-%s\n' "$os_key" "$arch_key"
}

release_asset_info() {
  # Find one exact GitHub Release asset download URL and API digest.
  # 查找一个精确 GitHub Release 资产下载地址与 API 摘要。
  local repo="$1"
  local tag="$2"
  local asset_name="$3"
  local api_url="https://api.github.com/repos/${repo}/releases/tags/${tag}"
  curl -fsSL "$api_url" | python3 -c '
import json
import sys
asset_name = sys.argv[1]
data = json.load(sys.stdin)
for asset in data.get("assets", []):
    if asset.get("name") == asset_name:
        print(asset.get("browser_download_url", ""))
        print(asset.get("digest", ""))
        raise SystemExit(0)
print(f"asset not found: {asset_name}", file=sys.stderr)
raise SystemExit(2)
' "$asset_name"
}

save_release_asset_with_digest() {
  # Download one GitHub Release asset and verify its GitHub API digest.
  # 下载单个 GitHub Release 资产并校验其 GitHub API 摘要。
  local repo="$1"
  local tag="$2"
  local asset_name="$3"
  local destination="$4"
  # Lookup payload and status kept separately so only an exact missing-asset result may use compatibility fallback.
  # 分开保存查询载荷与状态，确保只有明确的资产缺失结果可以使用兼容回退。
  local asset_url asset_digest expected actual asset_info_text lookup_status
  if asset_info_text="$(release_asset_info "$repo" "$tag" "$asset_name")"; then
    lookup_status=0
  else
    lookup_status=$?
  fi
  if [ "$lookup_status" -ne 0 ]; then
    if [ "$lookup_status" -eq 2 ]; then
      return 2
    fi
    return 1
  fi
  mapfile -t asset_info <<< "$asset_info_text"
  asset_url="${asset_info[0]:-}"
  asset_digest="${asset_info[1]:-}"
  if [[ "$asset_digest" != sha256:* ]]; then
    echo "GitHub API digest for $asset_name is missing or unsupported: $asset_digest" >&2
    return 1
  fi
  expected="${asset_digest#sha256:}"
  curl -fSL "$asset_url" -o "$destination"
  actual="$(python3 - "$destination" <<'PY'
import hashlib
import sys
digest = hashlib.sha256()
with open(sys.argv[1], "rb") as handle:
    for chunk in iter(lambda: handle.read(1024 * 1024), b""):
        digest.update(chunk)
print(digest.hexdigest())
PY
)"
  if [ "$expected" != "$actual" ]; then
    echo "SHA-256 mismatch for $asset_name. Expected $expected, got $actual" >&2
    return 1
  fi
}

resolve_luaskills_version() {
  # Resolve the FFI SDK release tag from input or the local Cargo.toml.
  # 从输入参数或本地 Cargo.toml 解析 FFI SDK 发布标签。
  local explicit_tag="$1"
  if [ -n "$explicit_tag" ]; then
    printf '%s\n' "$explicit_tag"
    return 0
  fi
  if [ -f "$PROJECT_ROOT/Cargo.toml" ]; then
    python3 - "$PROJECT_ROOT/Cargo.toml" <<'PY'
from pathlib import Path
import re
import sys
text = Path(sys.argv[1]).read_text(encoding="utf-8")
match = re.search(r'(?m)^version\s*=\s*"([^"]+)"', text)
if not match:
    raise SystemExit("Unable to resolve fallback LuaSkills version from Cargo.toml.")
print(f"v{match.group(1)}")
PY
    return 0
  fi
  echo "LuaSkills FFI SDK version was not provided. Set LUASKILLS_VERSION or pass -LuaSkillsVersion in packaged PowerShell usage." >&2
  return 1
}

luaskills_library_candidates() {
  # Return candidate LuaSkills dynamic library names for the current platform.
  # 返回当前平台对应的 LuaSkills 动态库候选名称。
  local platform
  platform="$(platform_key)"
  case "$platform" in
    linux-x64|linux-arm64) printf '%s\n' "libluaskills.so" "luaskills.so" ;;
    macos-x64|macos-arm64) printf '%s\n' "libluaskills.dylib" "luaskills.dylib" ;;
    *) echo "Unsupported LuaSkills FFI platform: $platform" >&2; return 1 ;;
  esac
}

has_existing_luaskills_ffi_content() {
  # Check whether the runtime root already contains one LuaSkills core dynamic library.
  # 检查运行根目录是否已经包含一个 LuaSkills core 动态库。
  local candidate=""
  while IFS= read -r candidate; do
    [ -n "$candidate" ] || continue
    if [ -f "$RUNTIME_ROOT/libs/$candidate" ]; then
      return 0
    fi
  done < <(luaskills_library_candidates)
  return 1
}

install_luaskills_ffi() {
  # Download and install one luaskills FFI SDK archive into the runtime root.
  # 下载并安装一个 luaskills FFI SDK 归档到运行根目录。
  local platform
  platform="$(platform_key)"
  local asset_name="luaskills-ffi-sdk-${platform}.tar.gz"
  local temp_dir
  temp_dir="$(mktemp -d)"
  trap 'rm -rf "$temp_dir"' RETURN
  local archive="$temp_dir/$asset_name"
  local extract_dir="$temp_dir/extract"
  ensure_dir "$extract_dir"
  # Classified download result used to separate exact asset absence from integrity or transport failures.
  # 用于区分明确资产缺失与完整性或传输失败的分类下载结果。
  local download_status=0
  save_release_asset_with_digest "$LUASKILLS_REPO" "$LUASKILLS_VERSION" "$asset_name" "$archive" || download_status=$?
  if [ "$download_status" -ne 0 ]; then
    if [ "$download_status" -eq 2 ] && has_existing_luaskills_ffi_content; then
      echo "WARNING: LuaSkills FFI SDK asset '$asset_name' was not found in $LUASKILLS_REPO@$LUASKILLS_VERSION. Existing packaged LuaSkills core content will be used." >&2
      return 0
    fi
    return 1
  fi
  tar -xzf "$archive" -C "$extract_dir"
  # Exact extracted core library required before any SDK content is copied into an existing runtime root.
  # 在任何 SDK 内容复制到现有运行根之前必须存在的精确解压核心库。
  local extracted_library="" candidate=""
  while IFS= read -r candidate; do
    if [ -f "$extract_dir/lib/$candidate" ]; then
      extracted_library="$extract_dir/lib/$candidate"
      break
    fi
  done < <(luaskills_library_candidates)
  if [ -z "$extracted_library" ]; then
    echo "LuaSkills dynamic library was not found in downloaded $asset_name" >&2
    return 1
  fi
  if [ -d "$extract_dir/include" ]; then
    ensure_dir "$RUNTIME_ROOT/include"
    cp -a "$extract_dir/include"/. "$RUNTIME_ROOT/include"/
  fi
  if [ -d "$extract_dir/lib" ]; then
    ensure_dir "$RUNTIME_ROOT/libs"
    cp -a "$extract_dir/lib"/. "$RUNTIME_ROOT/libs"/
  fi
  if [ -d "$extract_dir/licenses" ]; then
    ensure_dir "$RUNTIME_ROOT/licenses/luaskills-ffi"
    cp -a "$extract_dir/licenses"/. "$RUNTIME_ROOT/licenses/luaskills-ffi"/
  fi
}

cd "$PROJECT_ROOT"
LUASKILLS_VERSION="$(resolve_luaskills_version "$LUASKILLS_VERSION")"
ensure_dir "$RUNTIME_ROOT"
install_luaskills_ffi
echo "LuaSkills FFI SDK installed into $RUNTIME_ROOT"
