#!/usr/bin/env bash
set -euo pipefail

# ProjectRoot points at the repository root regardless of the caller location.
# ProjectRoot 指向仓库根目录，避免调用方当前位置影响路径解析。
PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

# Target selects which dependency group to install.
# Target 选择需要安装的依赖分组。
TARGET="${1:-all}"


# RuntimeRoot receives installed runtime files.
# RuntimeRoot 接收安装后的运行期文件。
RUNTIME_ROOT="${RUNTIME_ROOT:-output}"

# LuaRuntimeRepo stores the GitHub repository for Lua runtime packages assets.
# LuaRuntimeRepo 保存 Lua runtime packages 资产所在的 GitHub 仓库。
LUA_RUNTIME_REPO="${LUA_RUNTIME_REPO:-LuaSkills/luaskills-packages}"

# LuaRuntimeSeries stores the compatible major.minor series for Lua runtime packages assets.
# LuaRuntimeSeries 保存 Lua runtime packages 资产的兼容 major.minor 协议线。
LUA_RUNTIME_SERIES="${LUA_RUNTIME_SERIES:-0.1}"

# LuaRuntimeVersion stores one optional exact GitHub Release tag override for Lua runtime packages assets.
# LuaRuntimeVersion 保存 Lua runtime packages 资产的可选精确 GitHub Release 标签覆盖值。
LUA_RUNTIME_VERSION="${LUA_RUNTIME_VERSION:-}"

# SkipLuaRuntimeLibs disables copying the runtime package libs/ directory into the runtime root.
# SkipLuaRuntimeLibs 用于禁用把 runtime package 的 libs/ 目录复制到 runtime root。
SKIP_LUA_RUNTIME_LIBS="${SKIP_LUA_RUNTIME_LIBS:-0}"

# LuaPackagesOnly narrows runtime setup to lua_packages/ only and skips runtime metadata.
# LuaPackagesOnly 将 runtime 初始化收窄为只放 lua_packages/，并跳过 runtime 元数据。
LUA_PACKAGES_ONLY="${LUA_PACKAGES_ONLY:-0}"

ensure_dir() {
  # Create one directory when it does not exist.
  # 在目录不存在时创建该目录。
  mkdir -p "$1"
}

platform_key() {
  # Resolve the current luaskills runtime asset platform key.
  # 解析当前 luaskills runtime 资产平台标识。
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

resolve_release_tag_for_series() {
  # Resolve the newest published GitHub release tag inside one major.minor series.
  # 解析一个 major.minor 协议线内最新的已发布 GitHub release 标签。
  local repo="$1"
  local series="$2"
  curl -fsSL "https://api.github.com/repos/${repo}/releases?per_page=100" | python3 -c '
import json
import re
import sys

series = sys.argv[1]
repo = sys.argv[2]
if not re.fullmatch(r"\d+\.\d+", series):
    raise SystemExit(f"unsupported packages series: {series}")

matches = []
for release in json.load(sys.stdin):
    if release.get("draft") or release.get("prerelease"):
        continue
    tag = str(release.get("tag_name", ""))
    normalized = tag[1:] if tag.startswith("v") else tag
    if not re.fullmatch(r"\d+\.\d+\.\d+", normalized):
        continue
    major, minor, patch = (int(part) for part in normalized.split("."))
    if f"{major}.{minor}" != series:
        continue
    matches.append(((major, minor, patch), tag))

if not matches:
    raise SystemExit(f"no published release found for {repo} series {series}")

matches.sort(key=lambda item: item[0], reverse=True)
print(matches[0][1])
' "$series" "$repo"
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

install_lua_runtime() {
  # Download and install one luaskills runtime-packages archive.
  # 下载并安装一个 luaskills runtime-packages 归档。
  local platform
  platform="$(platform_key)"
  local asset_name="lua-runtime-packages-${platform}.tar.gz"
  local resolved_lua_runtime_tag="${LUA_RUNTIME_VERSION:-}"
  if [ -z "$resolved_lua_runtime_tag" ]; then
    resolved_lua_runtime_tag="$(resolve_release_tag_for_series "$LUA_RUNTIME_REPO" "$LUA_RUNTIME_SERIES")"
  fi
  local temp_dir
  temp_dir="$(mktemp -d)"
  trap 'rm -rf "$temp_dir"' RETURN
  local archive="$temp_dir/$asset_name"
  local extract_dir="$temp_dir/extract"
  ensure_dir "$extract_dir"
  # Classified download result used to separate exact asset absence from integrity or transport failures.
  # 用于区分明确资产缺失与完整性或传输失败的分类下载结果。
  local download_status=0
  save_release_asset_with_digest "$LUA_RUNTIME_REPO" "$resolved_lua_runtime_tag" "$asset_name" "$archive" || download_status=$?
  if [ "$download_status" -ne 0 ]; then
    if [ "$download_status" -eq 2 ] && { [ -d "$RUNTIME_ROOT/skills" ] || [ -d "$RUNTIME_ROOT/lua_packages" ]; }; then
      echo "WARNING: Lua runtime packages asset '$asset_name' was not found in $LUA_RUNTIME_REPO@$resolved_lua_runtime_tag. Existing packaged runtime content will be used." >&2
      return 0
    fi
    return 1
  fi
  tar -xzf "$archive" -C "$extract_dir"
  [ -d "$extract_dir/lua_packages" ] || {
    echo "Downloaded $asset_name does not contain lua_packages" >&2
    return 1
  }
  if [ "$LUA_PACKAGES_ONLY" != "1" ]; then
    [ -f "$extract_dir/resources/lua-runtime-manifest.json" ] || {
      echo "Downloaded $asset_name does not contain resources/lua-runtime-manifest.json" >&2
      return 1
    }
    [ -f "$extract_dir/resources/luaskills-packages-manifest.json" ] || {
      echo "Downloaded $asset_name does not contain resources/luaskills-packages-manifest.json" >&2
      return 1
    }
  fi
  ensure_dir "$RUNTIME_ROOT"
  local runtime_dirs=(lua_packages)
  if [ "$LUA_PACKAGES_ONLY" != "1" ]; then
    runtime_dirs+=(resources)
    if [ "$SKIP_LUA_RUNTIME_LIBS" != "1" ]; then
      runtime_dirs+=(libs)
    fi
  fi
  local dir_name=""
  for dir_name in "${runtime_dirs[@]}"; do
    if [ -d "$extract_dir/$dir_name" ]; then
      ensure_dir "$RUNTIME_ROOT/$dir_name"
      cp -a "$extract_dir/$dir_name"/. "$RUNTIME_ROOT/$dir_name"/
    fi
  done
  if [ "$LUA_PACKAGES_ONLY" != "1" ]; then
    if [ -d "$extract_dir/licenses" ]; then
      ensure_dir "$RUNTIME_ROOT/licenses"
      cp -a "$extract_dir/licenses"/. "$RUNTIME_ROOT/licenses"/
    fi
  fi
}

case "$TARGET" in
  all|lua) ;;
  *) echo "Usage: $0 [all|lua]" >&2; exit 2 ;;
esac
cd "$PROJECT_ROOT"
ensure_dir "$RUNTIME_ROOT/resources"
install_lua_runtime
echo "Lua runtime dependencies installed into $RUNTIME_ROOT"
