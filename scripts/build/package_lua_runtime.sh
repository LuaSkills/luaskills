#!/usr/bin/env bash
set -euo pipefail

# ProjectRoot points at the repository root regardless of the caller location.
# ProjectRoot 指向仓库根目录，避免调用方当前位置影响路径解析。
PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

# Platform stores the release asset platform key.
# Platform 保存发布资产使用的平台标识。
PLATFORM="${1:-}"

# ThirdPartyDir stores build outputs produced before packaging.
# ThirdPartyDir 保存包装前已经生成的构建产物。
THIRD_PARTY_DIR="${THIRD_PARTY_DIR:-third_party}"

# StagingDir stores the runtime tree before compression.
# StagingDir 保存压缩前的 runtime 目录树。
STAGING_DIR="${STAGING_DIR:-target/lua-runtime-package}"

# OutputDir stores final release archives.
# OutputDir 保存最终发布压缩包。
OUTPUT_DIR="${2:-${OUTPUT_DIR:-target/release-packages}}"

normalize_output_dir() {
  # Convert Windows drive paths into shell-native paths before tar sees a colon.
  # 在 tar 看到冒号前，将 Windows 盘符路径转换为 shell 原生路径。
  local raw_path="$1"
  case "$raw_path" in
    [A-Za-z]:/*)
      local drive_lower rest
      drive_lower="$(printf '%s' "${raw_path:0:1}" | tr '[:upper:]' '[:lower:]')"
      rest="${raw_path:3}"
      if [ -d "/mnt/$drive_lower" ]; then
        printf '/mnt/%s/%s\n' "$drive_lower" "$rest"
      else
        printf '/%s/%s\n' "$drive_lower" "$rest"
      fi
      ;;
    *) printf '%s\n' "$raw_path" ;;
  esac
}

OUTPUT_DIR="$(normalize_output_dir "$OUTPUT_DIR")"

ensure_dir() {
  # Create one directory when it does not exist.
  # 在目录不存在时创建该目录。
  mkdir -p "$1"
}

create_tar_from_dir() {
  # Archive top-level children without adding a leading ./ entry.
  # 按一级子项打包，避免归档内出现 ./ 前缀。
  local source_dir="$1"
  local archive_path="$2"
  local members=()
  while IFS= read -r entry; do
    members+=("$(basename "$entry")")
  done < <(find "$source_dir" -mindepth 1 -maxdepth 1)
  if [ "${#members[@]}" -eq 0 ]; then
    echo "Cannot create archive from empty directory: $source_dir" >&2
    return 1
  fi
  tar -czf "$archive_path" -C "$source_dir" "${members[@]}"
}

record_bundled_library() {
  # Record one bundled native library source for manifests and license references.
  # 记录一个已打包原生库的来源，用于清单与授权引用。
  local source="$1"
  local destination="$2"
  local component
  component="$(component_for_library "$destination")"
  printf '%s\t%s\t%s\n' "$(basename "$destination")" "$component" "$source" >> "$BUNDLED_LIBS_TSV"
}

component_for_library() {
  # Map one native library filename to its component name.
  # 将原生库文件名映射到组件名称。
  local name
  name="$(basename "$1" | tr '[:upper:]' '[:lower:]')"
  case "$name" in
    libz.so*|zlib*.dll|libz.*.dylib|libz.dylib) echo "zlib" ;;
    libcurl.so*|libcurl*.dll|libcurl.*.dylib|libcurl.dylib) echo "curl" ;;
    libssl.so*|libssl*.dll|libssl.*.dylib|libssl.dylib|libcrypto.so*|libcrypto*.dll|libcrypto.*.dylib|libcrypto.dylib) echo "openssl" ;;
    libpcre2-*.so*|pcre2*.dll|libpcre2-*.dylib) echo "pcre2" ;;
    libyaml*.so*|yaml*.dll|libyaml*.dylib) echo "libyaml" ;;
    *) echo "unknown" ;;
  esac
}

copy_tree_if_exists() {
  # Copy one directory tree when the source exists.
  # 在源目录存在时复制整棵目录树。
  local source="$1"
  local destination="$2"
  if [ -d "$source" ]; then
    ensure_dir "$destination"
    cp -a "$source"/. "$destination"/
  fi
}

copy_lua_package_runtime_dir() {
  # Flatten Lua 5.1 ABI package directories into the runtime default layout.
  # 将 Lua 5.1 ABI 包目录扁平化到 runtime 默认布局。
  local source="$1"
  local destination="$2"
  [ -d "$source" ] || return 0
  ensure_dir "$destination"
  if [ -d "$source/5.1" ]; then
    cp -a "$source/5.1"/. "$destination"/
  fi
  find "$source" -mindepth 1 -maxdepth 1 ! -name '5.1' -exec cp -a {} "$destination"/ \;
}

copy_native_runtime_libraries() {
  # Copy native runtime libraries and skip build-only LuaJIT files.
  # 复制原生运行库，并跳过仅用于构建的 LuaJIT 文件。
  local deps_dir="$1"
  local runtime_root="$2"
  local libs_dir="$runtime_root/libs"
  ensure_dir "$libs_dir"
  [ -d "$deps_dir" ] || return 0
  find "$deps_dir" \( -type f -o -type l \) \( -name '*.dll' -o -name '*.so' -o -name '*.so.*' -o -name '*.dylib' \) | while IFS= read -r file; do
    local name
    name="$(basename "$file" | tr '[:upper:]' '[:lower:]')"
    case "$name" in
      lua51.dll|luajit.exe|lua.exe) continue ;;
    esac
    local destination="$libs_dir/$(basename "$file")"
    cp -f "$file" "$destination"
    record_bundled_library "$file" "$destination"
  done
}

canonical_existing_path() {
  # Print one canonical existing path for dependency-queue deduplication.
  # 输出一个用于依赖队列去重的规范现有路径。
  local path="$1"
  if command -v realpath >/dev/null 2>&1; then
    realpath "$path"
    return 0
  fi
  local directory name
  directory="$(cd "$(dirname "$path")" && pwd -P)"
  name="$(basename "$path")"
  printf '%s/%s\n' "$directory" "$name"
}

is_bundled_native_dependency() {
  # Check whether one linked native library belongs to the runtime dependency set.
  # 判断一个已链接原生库是否属于需要随 runtime 携带的依赖集合。
  local name
  name="$(basename "$1" | tr '[:upper:]' '[:lower:]')"
  case "$name" in
    libz.so*|zlib*.dll|libz.*.dylib|libz.dylib) return 0 ;;
    libcurl.so*|libcurl*.dll|libcurl.*.dylib|libcurl.dylib) return 0 ;;
    libssl.so*|libssl*.dll|libssl.*.dylib|libssl.dylib) return 0 ;;
    libcrypto.so*|libcrypto*.dll|libcrypto.*.dylib|libcrypto.dylib) return 0 ;;
    libpcre2-*.so*|pcre2*.dll|libpcre2-*.dylib) return 0 ;;
    libyaml*.so*|yaml*.dll|libyaml*.dylib) return 0 ;;
    *) return 1 ;;
  esac
}

linked_dependency_paths() {
  # Print absolute linked dependency paths reported by ldd or otool.
  # 输出 ldd 或 otool 报告的已链接依赖绝对路径。
  local binary="$1"
  if command -v ldd >/dev/null 2>&1; then
    (ldd "$binary" 2>/dev/null || true) | awk '{ for (i = 1; i <= NF; i++) if ($i ~ /^\//) print $i }'
    return 0
  fi
  if command -v otool >/dev/null 2>&1; then
    otool -L "$binary" 2>/dev/null | awk 'NR > 1 { print $1 }' | grep '^/' || true
    return 0
  fi
}

copy_linked_runtime_dependencies() {
  # Copy one complete allowlisted dependency closure from distinct initial scan roots.
  # 从不同初始扫描根复制一个完整的白名单依赖闭包。
  local libs_dir="$1"
  shift
  ensure_dir "$libs_dir"
  local queue_file seen_file pending_file roots_file
  queue_file="$(mktemp)"
  seen_file="$(mktemp)"
  pending_file="$(mktemp)"
  roots_file="$(mktemp)"
  trap 'rm -f "$queue_file" "$seen_file" "$pending_file" "$roots_file"' RETURN

  # Canonical initial roots prevent runtime/libs from being enumerated twice through separate calls.
  # 规范初始根防止 runtime/libs 通过不同调用被重复枚举。
  local scan_root canonical_root
  for scan_root in "$@"; do
    [ -d "$scan_root" ] || continue
    canonical_root="$(canonical_existing_path "$scan_root")"
    if grep -Fxq "$canonical_root" "$roots_file" 2>/dev/null; then
      continue
    fi
    printf '%s\n' "$canonical_root" >> "$roots_file"
    find "$canonical_root" -type f \( -name '*.dll' -o -name '*.so' -o -name '*.so.*' -o -name '*.dylib' \) 2>/dev/null >> "$queue_file" || true
  done

  while [ -s "$queue_file" ]; do
    : > "$pending_file"
    while IFS= read -r binary; do
      [ -f "$binary" ] || continue
      local canonical_binary
      canonical_binary="$(canonical_existing_path "$binary")"
      if grep -Fxq "$canonical_binary" "$seen_file" 2>/dev/null; then
        continue
      fi
      printf '%s\n' "$canonical_binary" >> "$seen_file"
      linked_dependency_paths "$canonical_binary" | while IFS= read -r dependency; do
        [ -f "$dependency" ] || continue
        is_bundled_native_dependency "$dependency" || continue
        local destination
        destination="$libs_dir/$(basename "$dependency")"
        if [ ! -f "$destination" ]; then
          cp -f "$dependency" "$destination"
          record_bundled_library "$dependency" "$destination"
          printf '%s\n' "$destination" >> "$pending_file"
        fi
      done
    done < "$queue_file"
    mv "$pending_file" "$queue_file"
  done
}

copy_license_candidates() {
  # Copy license-like files from one source directory to one component directory.
  # 将一个源目录中的授权类文件复制到组件授权目录。
  local source="$1"
  local destination="$2"
  [ -d "$source" ] || return 0
  ensure_dir "$destination"
  local destination_real
  destination_real="$(cd "$destination" && pwd -P)"
  find "$source" -maxdepth 5 -type f \( -iname 'LICENSE*' -o -iname 'LICENCE*' -o -iname 'COPYING*' -o -iname 'NOTICE*' \) -print | while IFS= read -r license_file; do
    local license_real
    license_real="$(cd "$(dirname "$license_file")" && pwd -P)/$(basename "$license_file")"
    case "$license_real" in
      "$destination_real"|"$destination_real"/*) continue ;;
    esac
    cp -f "$license_file" "$destination/"
  done
}

download_official_license() {
  # Download one official license file for a fixed native dependency.
  # 为固定原生依赖下载一个官方授权文件。
  local component="$1"
  local file_name="$2"
  local url="$3"
  local destination="$RUNTIME_ROOT/licenses/native/$component"
  ensure_dir "$destination"
  curl -fSL --retry 3 "$url" -o "$destination/$file_name"
  printf '%s\n' "$url" > "$destination/$file_name.url.txt"
}

download_official_native_licenses() {
  # Always include official licenses for the fixed native dependency set.
  # 始终为固定原生依赖集合带入官方授权文件。
  download_official_license "openssl" "LICENSE.official.txt" "https://raw.githubusercontent.com/openssl/openssl/openssl-3.4.1/LICENSE.txt"
  download_official_license "curl" "COPYING.official.txt" "https://raw.githubusercontent.com/curl/curl/curl-8_13_0/COPYING"
  download_official_license "zlib" "LICENSE.official.txt" "https://raw.githubusercontent.com/madler/zlib/v1.3.1/LICENSE"
  download_official_license "pcre2" "LICENCE.official.md" "https://raw.githubusercontent.com/PCRE2Project/pcre2/pcre2-10.45/LICENCE.md"
  download_official_license "libyaml" "License.official" "https://raw.githubusercontent.com/yaml/libyaml/0.2.5/License"
}

write_license_reference_if_missing() {
  # Provide a license reference when a copied system library has no nearby license file.
  # 当复制的系统库没有随源目录提供授权文件时，写入授权引用。
  local component="$1"
  local source_path="$2"
  local destination="$RUNTIME_ROOT/licenses/native/$component"
  ensure_dir "$destination"
  if find "$destination" -maxdepth 1 -type f \( -iname 'LICENSE*' -o -iname 'LICENCE*' -o -iname 'COPYING*' -o -iname 'NOTICE*' -o -iname 'README*' \) | grep -q .; then
    return 0
  fi
  local license
  case "$component" in
    openssl) license="Apache-2.0" ;;
    curl) license="curl" ;;
    zlib) license="Zlib" ;;
    pcre2) license="BSD-3-Clause" ;;
    libyaml) license="MIT" ;;
    *) license="See upstream project" ;;
  esac
  cat > "$destination/LICENSE.reference.txt" <<EOF
Component: $component
License: $license
Bundled library source path: $source_path

No license file was found next to the copied system library during packaging.
This package records the upstream license identifier and the source path used by the build runner.
EOF
}

write_loader_env_scripts() {
  # Add small opt-in environment helpers for hosts that launch the runtime package.
  # 为启动 runtime 包的宿主提供可选环境辅助脚本。
  if [[ "$PLATFORM" == windows-* ]]; then
    cat > "$RUNTIME_ROOT/resources/runtime-env.ps1" <<'PS1'
$RuntimeRoot = if ($env:RUNTIME_ROOT) { $env:RUNTIME_ROOT } else { Split-Path -Parent $PSScriptRoot }
$Libs = Join-Path $RuntimeRoot "libs"
$env:PATH = "$Libs;$env:PATH"
PS1
    return
  fi

  cat > "$RUNTIME_ROOT/resources/runtime-env.sh" <<'SH'
#!/usr/bin/env bash
RUNTIME_ROOT="${RUNTIME_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
case "$(uname -s)" in
  Darwin) export DYLD_LIBRARY_PATH="$RUNTIME_ROOT/libs${DYLD_LIBRARY_PATH:+:$DYLD_LIBRARY_PATH}" ;;
  Linux) export LD_LIBRARY_PATH="$RUNTIME_ROOT/libs${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}" ;;
esac
SH
  chmod +x "$RUNTIME_ROOT/resources/runtime-env.sh"
}

if [ -z "$PLATFORM" ]; then
  case "$(uname -s)" in
    Linux) os_key="linux" ;;
    Darwin) os_key="macos" ;;
    *) os_key="unknown" ;;
  esac
  case "$(uname -m)" in
    x86_64|amd64) arch_key="x64" ;;
    aarch64|arm64) arch_key="arm64" ;;
    *) arch_key="$(uname -m)" ;;
  esac
  PLATFORM="${os_key}-${arch_key}"
fi

cd "$PROJECT_ROOT"

RUNTIME_ROOT="$STAGING_DIR/lua-runtime"
BUNDLED_LIBS_TSV="$STAGING_DIR/bundled-libs.tsv"
rm -rf "$RUNTIME_ROOT"
rm -f "$BUNDLED_LIBS_TSV"
ensure_dir "$RUNTIME_ROOT/resources"
ensure_dir "$RUNTIME_ROOT/licenses"
ensure_dir "$OUTPUT_DIR"

copy_lua_package_runtime_dir "$THIRD_PARTY_DIR/lua_packages/lib/lua" "$RUNTIME_ROOT/lua_packages/lib/lua"
copy_lua_package_runtime_dir "$THIRD_PARTY_DIR/lua_packages/share/lua" "$RUNTIME_ROOT/lua_packages/share/lua"
copy_native_runtime_libraries "$THIRD_PARTY_DIR/deps" "$RUNTIME_ROOT"
copy_linked_runtime_dependencies "$RUNTIME_ROOT/libs" "$RUNTIME_ROOT" "$PROJECT_ROOT/target/release"

write_loader_env_scripts
copy_license_candidates "$PROJECT_ROOT" "$RUNTIME_ROOT/licenses/luaskills"

for component in openssl curl zlib pcre2 libyaml; do
  case "$component" in
    libyaml) patterns=("yaml-*" "libyaml-*" "$THIRD_PARTY_DIR/deps/libyaml" "target/lua_deps_build/libyaml" "target/lua_deps_build/libyaml/"*) ;;
    *) patterns=("$component-*" "$THIRD_PARTY_DIR/deps/$component" "target/lua_deps_build/$component" "target/lua_deps_build/$component/"*) ;;
  esac
  for pattern in "${patterns[@]}"; do
    for path in $pattern; do
      copy_license_candidates "$path" "$RUNTIME_ROOT/licenses/native/$component"
    done
  done
done

download_official_native_licenses

if [ -f "$BUNDLED_LIBS_TSV" ]; then
  while IFS=$'\t' read -r lib_name component source_path; do
    [ -n "$component" ] && [ "$component" != "unknown" ] || continue
    write_license_reference_if_missing "$component" "$source_path"
  done < "$BUNDLED_LIBS_TSV"
fi

python3 - "$BUNDLED_LIBS_TSV" "$RUNTIME_ROOT/resources/bundled-libs.json" <<'PY'
import json
import sys
from pathlib import Path
tsv = Path(sys.argv[1])
items = []
if tsv.exists():
    seen = set()
    for line in tsv.read_text(encoding="utf-8").splitlines():
        name, component, source = line.split("\t", 2)
        key = (name, component, source)
        if key in seen:
            continue
        seen.add(key)
        items.append({"name": name, "component": component, "source_path": source})
Path(sys.argv[2]).write_text(json.dumps(items, indent=2) + "\n", encoding="utf-8")
PY

cat > "$RUNTIME_ROOT/resources/lua-runtime-manifest.json" <<JSON
{
  "schema_version": 1,
  "package_name": "lua-runtime-${PLATFORM}",
  "platform": "${PLATFORM}",
  "layout": "luaskills-runtime-v1",
  "exports": ["lua_packages/lib/lua", "lua_packages/share/lua", "libs", "resources", "licenses"],
  "packages_manifest": "resources/luaskills-packages-manifest.json",
  "loader_env": {
    "linux": "LD_LIBRARY_PATH=<runtime>/libs",
    "macos": "DYLD_LIBRARY_PATH=<runtime>/libs",
    "windows": "PATH=<runtime>\\\\libs;%PATH%"
  },
  "excludes": ["third_party/tools", "third_party/luajit", "lua51.dll", "luajit.exe", "build directories"]
}
JSON

cat > "$RUNTIME_ROOT/licenses/manifest.json" <<JSON
{
  "schema_version": 1,
  "package_name": "lua-runtime-${PLATFORM}",
  "components": [
    { "name": "luaskills", "type": "runtime", "license": "MIT", "license_files": ["licenses/luaskills/LICENSE"] },
    { "name": "openssl", "type": "native-lib", "license": "Apache-2.0", "license_files": ["licenses/native/openssl"] },
    { "name": "curl", "type": "native-lib", "license": "curl", "license_files": ["licenses/native/curl"] },
    { "name": "zlib", "type": "native-lib", "license": "Zlib", "license_files": ["licenses/native/zlib"] },
    { "name": "pcre2", "type": "native-lib", "license": "BSD-3-Clause", "license_files": ["licenses/native/pcre2"] },
    { "name": "libyaml", "type": "native-lib", "license": "MIT", "license_files": ["licenses/native/libyaml"] },
    { "name": "luaskills-packages", "type": "lua-packages", "license": "per-bundle-metadata", "license_files": ["resources/luaskills-packages/THIRD_PARTY_LICENSES.json", "licenses/luaskills-packages"] }
  ]
}
JSON

# Generate the runtime-facing luaskills-packages metadata tree after license manifests exist.
# 在授权清单就绪后生成面向运行时的 luaskills-packages 元数据目录树。
python3 "$PROJECT_ROOT/scripts/build/generate_runtime_packages_metadata.py" \
  --project-root "$PROJECT_ROOT" \
  --runtime-root "$RUNTIME_ROOT" \
  --platform "$PLATFORM"

ARCHIVE_NAME="lua-runtime-${PLATFORM}.tar.gz"
create_tar_from_dir "$RUNTIME_ROOT" "$OUTPUT_DIR/$ARCHIVE_NAME"
echo "Lua runtime package created: $OUTPUT_DIR/$ARCHIVE_NAME"
