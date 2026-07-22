#!/usr/bin/env bash
set -euo pipefail

# ProjectRoot points at the repository root regardless of the caller location.
# ProjectRoot 指向仓库根目录，避免调用方当前位置影响路径解析。
PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

# Target selects which dependency group to install.
# Target 选择需要安装的依赖分组。
TARGET="${1:-all}"

# Database selects the optional VLDB integration mode for all or vldb targets.
# Database 选择 all 或 vldb 目标使用的可选 VLDB 集成模式。
DATABASE="${2:-${DATABASE:-vldb-controller}}"

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

# VldbControllerRepo stores the GitHub repository for vldb-controller assets.
# VldbControllerRepo 保存 vldb-controller 资产所在的 GitHub 仓库。
VLDB_CONTROLLER_REPO="${VLDB_CONTROLLER_REPO:-OpenVulcan/vldb-controller}"

# VldbControllerVersion stores the GitHub Release tag for vldb-controller assets.
# VldbControllerVersion 保存 vldb-controller 资产的 GitHub Release 标签。
VLDB_CONTROLLER_VERSION="${VLDB_CONTROLLER_VERSION:-v0.2.3}"

# VldbSQLiteRepo stores the GitHub repository for vldb-sqlite assets.
# VldbSQLiteRepo 保存 vldb-sqlite 资产所在的 GitHub 仓库。
VLDB_SQLITE_REPO="${VLDB_SQLITE_REPO:-OpenVulcan/vldb-sqlite}"

# VldbSQLiteVersion stores the GitHub Release tag for vldb-sqlite assets.
# VldbSQLiteVersion 保存 vldb-sqlite 资产的 GitHub Release 标签。
VLDB_SQLITE_VERSION="${VLDB_SQLITE_VERSION:-v0.1.6}"

# VldbLanceDBRepo stores the GitHub repository for vldb-lancedb assets.
# VldbLanceDBRepo 保存 vldb-lancedb 资产所在的 GitHub 仓库。
VLDB_LANCEDB_REPO="${VLDB_LANCEDB_REPO:-OpenVulcan/vldb-lancedb}"

# VldbLanceDBVersion stores the GitHub Release tag for vldb-lancedb assets.
# VldbLanceDBVersion 保存 vldb-lancedb 资产的 GitHub Release 标签。
VLDB_LANCEDB_VERSION="${VLDB_LANCEDB_VERSION:-v0.1.5}"

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

vldb_asset_info() {
  # Resolve VLDB target, extension, binary name, and dynamic library names.
  # 解析 VLDB 的目标三元组、扩展名、二进制文件名与动态库名称。
  case "$(uname -m)" in
    x86_64|amd64) arch_key="x86_64" ;;
    aarch64|arm64) arch_key="aarch64" ;;
    *) echo "Unsupported architecture for vldb-controller: $(uname -m)" >&2; return 1 ;;
  esac
  case "$(uname -s)" in
    Linux) printf '%s-unknown-linux-gnu|.tar.gz|vldb-controller|.so|libvldb_sqlite.so|libvldb_lancedb.so\n' "$arch_key" ;;
    Darwin) printf '%s-apple-darwin|.tar.gz|vldb-controller|.dylib|libvldb_sqlite.dylib|libvldb_lancedb.dylib\n' "$arch_key" ;;
    *) echo "Unsupported operating system for vldb-controller: $(uname -s)" >&2; return 1 ;;
  esac
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
raise SystemExit(f"asset not found: {asset_name}")
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
  local asset_url asset_digest expected actual
  mapfile -t asset_info < <(release_asset_info "$repo" "$tag" "$asset_name")
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
  if ! save_release_asset_with_digest "$LUA_RUNTIME_REPO" "$resolved_lua_runtime_tag" "$asset_name" "$archive"; then
    if [ -d "$RUNTIME_ROOT/skills" ] || [ -d "$RUNTIME_ROOT/lua_packages" ]; then
      echo "WARNING: Lua runtime packages asset '$asset_name' was not found in $LUA_RUNTIME_REPO@$resolved_lua_runtime_tag. Existing packaged runtime content will be used." >&2
      return 0
    fi
    return 1
  fi
  tar -xzf "$archive" -C "$extract_dir"
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
    [ -f "$RUNTIME_ROOT/resources/lua-runtime-manifest.json" ] || {
      echo "Lua runtime manifest was not found after installing $asset_name" >&2
      return 1
    }
    [ -f "$RUNTIME_ROOT/resources/luaskills-packages-manifest.json" ] || {
      echo "LuaSkills packages manifest was not found after installing $asset_name" >&2
      return 1
    }
  fi
}

install_vldb_controller() {
  # Download and install vldb-controller into runtime bin.
  # 下载 vldb-controller 并安装到运行期 bin 目录。
  local info target archive_ext binary_name dynamic_ext sqlite_library lancedb_library
  info="$(vldb_asset_info)"
  IFS='|' read -r target archive_ext binary_name dynamic_ext sqlite_library lancedb_library <<< "$info"
  local asset_name="vldb-controller-${VLDB_CONTROLLER_VERSION}-${target}${archive_ext}"
  local temp_dir
  temp_dir="$(mktemp -d)"
  trap 'rm -rf "$temp_dir"' RETURN
  local archive="$temp_dir/$asset_name"
  local extract_dir="$temp_dir/extract"
  ensure_dir "$extract_dir"
  save_release_asset_with_digest "$VLDB_CONTROLLER_REPO" "$VLDB_CONTROLLER_VERSION" "$asset_name" "$archive"
  tar -xzf "$archive" -C "$extract_dir"
  local binary
  binary="$(find "$extract_dir" -type f -name "$binary_name" | head -1)"
  [ -n "$binary" ] || { echo "vldb-controller binary not found in $asset_name" >&2; return 1; }
  ensure_dir "$RUNTIME_ROOT/bin"
  cp -f "$binary" "$RUNTIME_ROOT/bin/$binary_name"
  chmod +x "$RUNTIME_ROOT/bin/$binary_name"
  ensure_dir "$RUNTIME_ROOT/resources"
  cat > "$RUNTIME_ROOT/resources/vldb-controller-manifest.json" <<JSON
{
  "schema_version": 1,
  "name": "vldb-controller",
  "version": "${VLDB_CONTROLLER_VERSION}",
  "asset": "${asset_name}",
  "installed_binary": "bin/${binary_name}"
}
JSON
}

install_vldb_library_asset() {
  # Download and install one VLDB dynamic library asset.
  # 下载并安装单个 VLDB 动态库资产。
  local repo="$1"
  local version="$2"
  local prefix="$3"
  local name_hint="$4"
  local info target archive_ext binary_name dynamic_ext sqlite_library lancedb_library
  info="$(vldb_asset_info)"
  IFS='|' read -r target archive_ext binary_name dynamic_ext sqlite_library lancedb_library <<< "$info"
  local asset_name="${prefix}-${version}-${target}${archive_ext}"
  local temp_dir
  temp_dir="$(mktemp -d)"
  trap 'rm -rf "$temp_dir"' RETURN
  local archive="$temp_dir/$asset_name"
  local extract_dir="$temp_dir/extract"
  ensure_dir "$extract_dir"
  save_release_asset_with_digest "$repo" "$version" "$asset_name" "$archive"
  tar -xzf "$archive" -C "$extract_dir"
  local library
  library="$(find "$extract_dir" -type f -name "*${dynamic_ext}" | grep -i "$name_hint" | head -1 || true)"
  [ -n "$library" ] || { echo "VLDB dynamic library matching '$name_hint' not found in $asset_name" >&2; return 1; }
  ensure_dir "$RUNTIME_ROOT/libs"
  cp -f "$library" "$RUNTIME_ROOT/libs/$(basename "$library")"
  printf '%s|libs/%s\n' "$asset_name" "$(basename "$library")"
}

install_vldb_direct_libraries() {
  # Download and install vldb-sqlite-lib and vldb-lancedb-lib assets.
  # 下载并安装 vldb-sqlite-lib 与 vldb-lancedb-lib 资产。
  local sqlite_info lancedb_info sqlite_asset sqlite_path lancedb_asset lancedb_path
  sqlite_info="$(install_vldb_library_asset "$VLDB_SQLITE_REPO" "$VLDB_SQLITE_VERSION" "vldb-sqlite-lib" "sqlite")"
  lancedb_info="$(install_vldb_library_asset "$VLDB_LANCEDB_REPO" "$VLDB_LANCEDB_VERSION" "vldb-lancedb-lib" "lancedb")"
  IFS='|' read -r sqlite_asset sqlite_path <<< "$sqlite_info"
  IFS='|' read -r lancedb_asset lancedb_path <<< "$lancedb_info"
  ensure_dir "$RUNTIME_ROOT/resources"
  cat > "$RUNTIME_ROOT/resources/vldb-direct-manifest.json" <<JSON
{
  "schema_version": 1,
  "database_mode": "vldb-direct",
  "sqlite": {
    "asset": "${sqlite_asset}",
    "installed_path": "${sqlite_path}"
  },
  "lancedb": {
    "asset": "${lancedb_asset}",
    "installed_path": "${lancedb_path}"
  }
}
JSON
}

cd "$PROJECT_ROOT"
ensure_dir "$RUNTIME_ROOT/resources"

case "$DATABASE" in
  none|vldb-controller|vldb-direct|host-callback) ;;
  *)
    echo "Usage: $0 [all|lua|vldb|vldb-controller|vldb-direct] [none|vldb-controller|vldb-direct|host-callback]" >&2
    exit 2
    ;;
esac

case "$TARGET" in
  all)
    install_lua_runtime
    if [ "$DATABASE" = "vldb-controller" ]; then
      install_vldb_controller
    elif [ "$DATABASE" = "vldb-direct" ]; then
      install_vldb_direct_libraries
    fi
    ;;
  lua)
    install_lua_runtime
    ;;
  vldb)
    if [ "$DATABASE" = "vldb-controller" ]; then
      install_vldb_controller
    elif [ "$DATABASE" = "vldb-direct" ]; then
      install_vldb_direct_libraries
    fi
    ;;
  vldb-controller)
    install_vldb_controller
    ;;
  vldb-direct)
    install_vldb_direct_libraries
    ;;
  *)
    echo "Usage: $0 [all|lua|vldb|vldb-controller|vldb-direct] [none|vldb-controller|vldb-direct|host-callback]" >&2
    exit 2
    ;;
esac

echo "Runtime dependency target '$TARGET' with database preset '$DATABASE' installed into $RUNTIME_ROOT"
