#!/usr/bin/env bash
set -euo pipefail

# ProjectRoot points at the repository root regardless of the caller location.
# ProjectRoot 指向仓库根目录，避免调用方当前位置影响路径解析。
PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

# Mode stores the demo integration mode.
# Mode 保存 demo 集成模式。
MODE="${1:?Usage: package_demo.sh ffi|rust [platform] [release_tag] [output_dir]}"

# Platform stores the release asset platform key.
# Platform 保存发布资产使用的平台标识。
PLATFORM="${2:-}"

# OutputDir stores final release archives.
# OutputDir 保存最终发布压缩包。
OUTPUT_DIR="${4:-${OUTPUT_DIR:-target/release-packages}}"

# ReleaseTag stores the Git tag used by packaged Rust and FFI demo assets.
# ReleaseTag 保存发布 Rust 与 FFI demo 资产使用的 Git 标签。
RELEASE_TAG="${3:-${RELEASE_TAG:-}}"

# Optional SourceCommit freezes candidate Rust demos; existing tagged invocations may omit it.
# 可选 SourceCommit 冻结候选 Rust demo；既有标签调用可以省略。
SOURCE_COMMIT="${5:-}"
if [ -n "$SOURCE_COMMIT" ] && [[ ! "$SOURCE_COMMIT" =~ ^[0-9a-f]{40}$ ]]; then
  echo "Candidate SourceCommit must be a full lowercase Git SHA." >&2
  exit 1
fi

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

resolve_release_tag() {
  # Resolve the LuaSkills release tag from input or Cargo.toml.
  # 从输入参数或 Cargo.toml 解析 LuaSkills 发布标签。
  local explicit_tag="$1"
  if [ -n "$explicit_tag" ]; then
    printf '%s\n' "$explicit_tag"
    return 0
  fi
  python3 - "$PROJECT_ROOT/Cargo.toml" <<'PY'
from pathlib import Path
import re
import sys
text = Path(sys.argv[1]).read_text(encoding="utf-8")
match = re.search(r'(?m)^version\s*=\s*"([^"]+)"', text)
if not match:
    raise SystemExit("Unable to resolve fallback release tag from Cargo.toml.")
print(f"v{match.group(1)}")
PY
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

platform_is_windows() {
  # Return success when the package target is Windows.
  # 当包目标平台是 Windows 时返回成功。
  [[ "$PLATFORM" == windows-* ]]
}

prune_packaged_demo_scripts() {
  # Remove launcher scripts that do not match the target package platform.
  # 移除与目标平台不匹配的 demo 启动脚本。
  local package_root="$1"
  if platform_is_windows; then
    rm -f "$package_root/run.sh"
    rm -f "$package_root/upgrade_deps.sh"
  else
    rm -f "$package_root/run.ps1"
    rm -f "$package_root/upgrade_deps.bat"
  fi
}

write_packaged_demo_scripts() {
  # Write run scripts that work from the packaged demo root.
  # 写入可从发布 demo 包根目录直接运行的脚本。
  local mode="$1"
  local package_root="$2"

  if [ "$mode" = "rust" ]; then
    cat > "$package_root/run.ps1" <<'PS1'
$ErrorActionPreference = "Stop"

# PackageRoot points at the extracted demo package root.
# PackageRoot 指向解压后的 demo 包根目录。
$PackageRoot = if ($PSScriptRoot) { $PSScriptRoot } else { (Get-Location).Path }

# RuntimeRoot is the packaged runtime root consumed by the demo.
# RuntimeRoot 是 demo 使用的包内运行根目录。
$RuntimeRoot = Join-Path $PackageRoot "runtime"

if (Test-Path -LiteralPath (Join-Path $RuntimeRoot "resources\runtime-env.ps1")) {
    . (Join-Path $RuntimeRoot "resources\runtime-env.ps1")
}

$env:LUASKILLS_RUNTIME_ROOT = $RuntimeRoot
cargo run --manifest-path (Join-Path $PackageRoot "Cargo.toml")
PS1
    cat > "$package_root/run.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail

# PackageRoot points at the extracted demo package root.
# PackageRoot 指向解压后的 demo 包根目录。
PACKAGE_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# RuntimeRoot is the packaged runtime root consumed by the demo.
# RuntimeRoot 是 demo 使用的包内运行根目录。
RUNTIME_ROOT="$PACKAGE_ROOT/runtime"

if [ -f "$RUNTIME_ROOT/resources/runtime-env.sh" ]; then
  # shellcheck source=/dev/null
  RUNTIME_ROOT="$RUNTIME_ROOT" . "$RUNTIME_ROOT/resources/runtime-env.sh"
fi

LUASKILLS_RUNTIME_ROOT="$RUNTIME_ROOT" cargo run --manifest-path "$PACKAGE_ROOT/Cargo.toml"
SH
    chmod +x "$package_root/run.sh"
    write_packaged_dependency_upgrade_scripts "$package_root"
    prune_packaged_demo_scripts "$package_root"
    return
  fi

  cat > "$package_root/run.ps1" <<'PS1'
$ErrorActionPreference = "Stop"

# PackageRoot points at the extracted demo package root.
# PackageRoot 指向解压后的 demo 包根目录。
$PackageRoot = if ($PSScriptRoot) { $PSScriptRoot } else { (Get-Location).Path }

# RuntimeRoot is the packaged runtime root consumed by the demo.
# RuntimeRoot 是 demo 使用的包内运行根目录。
$RuntimeRoot = Join-Path $PackageRoot "runtime"

if (Test-Path -LiteralPath (Join-Path $RuntimeRoot "resources\runtime-env.ps1")) {
    . (Join-Path $RuntimeRoot "resources\runtime-env.ps1")
}

$Library = Get-ChildItem -File -Path (Join-Path $PackageRoot "lib\*") -Include "*.dll","*.so","*.dylib" -ErrorAction SilentlyContinue | Select-Object -First 1
if (-not $Library) {
    throw "No LuaSkills FFI library found under package lib directory."
}

$CompatRuntime = Join-Path $PackageRoot "examples\ffi\standard_runtime\runtime_root"
New-Item -ItemType Directory -Force -Path $CompatRuntime | Out-Null
Copy-Item -Recurse -Force -Path (Join-Path $RuntimeRoot "*") -Destination $CompatRuntime -ErrorAction SilentlyContinue
$LibDir = Join-Path $PackageRoot "lib"
if ($IsWindows -or $env:OS -eq "Windows_NT") {
    $env:PATH = "$LibDir;$env:PATH"
} elseif ($IsMacOS) {
    $env:DYLD_LIBRARY_PATH = "$LibDir" + $(if ($env:DYLD_LIBRARY_PATH) { ":$env:DYLD_LIBRARY_PATH" } else { "" })
} else {
    $env:LD_LIBRARY_PATH = "$LibDir" + $(if ($env:LD_LIBRARY_PATH) { ":$env:LD_LIBRARY_PATH" } else { "" })
}
$env:LUASKILLS_LIB = $Library.FullName
python (Join-Path $PackageRoot "examples\ffi\python\demo.py")
PS1
  cat > "$package_root/run.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail

# PackageRoot points at the extracted demo package root.
# PackageRoot 指向解压后的 demo 包根目录。
PACKAGE_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# RuntimeRoot is the packaged runtime root consumed by the demo.
# RuntimeRoot 是 demo 使用的包内运行根目录。
RUNTIME_ROOT="$PACKAGE_ROOT/runtime"

if [ -f "$RUNTIME_ROOT/resources/runtime-env.sh" ]; then
  # shellcheck source=/dev/null
  RUNTIME_ROOT="$RUNTIME_ROOT" . "$RUNTIME_ROOT/resources/runtime-env.sh"
fi

LIBRARY="$(find "$PACKAGE_ROOT/lib" -maxdepth 1 -type f \( -name '*.so' -o -name '*.dylib' -o -name '*.dll' \) | head -1)"
[ -n "$LIBRARY" ] || { echo "No LuaSkills FFI library found under package lib directory." >&2; exit 1; }
mkdir -p "$PACKAGE_ROOT/examples/ffi/standard_runtime/runtime_root"
cp -a "$RUNTIME_ROOT"/. "$PACKAGE_ROOT/examples/ffi/standard_runtime/runtime_root"/
case "$(uname -s)" in
  Darwin) export DYLD_LIBRARY_PATH="$PACKAGE_ROOT/lib${DYLD_LIBRARY_PATH:+:$DYLD_LIBRARY_PATH}" ;;
  Linux) export LD_LIBRARY_PATH="$PACKAGE_ROOT/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}" ;;
esac
LUASKILLS_LIB="$LIBRARY" python3 "$PACKAGE_ROOT/examples/ffi/python/demo.py"
SH
  chmod +x "$package_root/run.sh"
  write_packaged_dependency_upgrade_scripts "$package_root"
  prune_packaged_demo_scripts "$package_root"
}

copy_ffi_example_sources() {
  # Copy every FFI demo source directory into the packaged demo.
  # 将所有 FFI demo 源码目录复制到发布 demo 包中。
  local package_root="$1"
  mkdir -p "$package_root/examples"
  rm -rf "$package_root/examples/ffi"
  cp -a examples/ffi "$package_root/examples/"
  # Generated runtime caches are useful after local smoke tests but should not ship in demo archives.
  # 本地烟测后的运行缓存有调试价值，但不应该进入 demo 发布包。
  find "$package_root/examples/ffi" -type d \( -name '__pycache__' -o -name 'node_modules' \) -prune -exec rm -rf {} +
  find "$package_root/examples/ffi" -type f \( -name '*.pyc' -o -name '*.pyo' \) -delete
  find "$package_root/examples/ffi" -type f \( -name '*.zip' -o -name '*.exe' -o -name '*.db' \) -delete
  while IFS= read -r generated_dir; do
    find "$generated_dir" -mindepth 1 ! -name '.gitkeep' -exec rm -rf {} +
  done < <(
    find "$package_root/examples/ffi" -type d \( \
      -path '*/runtime_root/temp/downloads' -o \
      -path '*/runtime_root/state/install_tmp' -o \
      -path '*/runtime_root/state/installs' -o \
      -path '*/runtime_root/dependencies/tools' \
    \)
  )
}

write_packaged_dependency_upgrade_scripts() {
  # Write standalone dependency upgrade launchers for packaged demos.
  # 写入发布 demo 包专用的独立依赖升级入口。
  local package_root="$1"

  cat > "$package_root/upgrade_deps.bat" <<BAT
@echo off
chcp 65001 >nul
setlocal
REM Target selects which dependency group to download.
REM Target 选择要下载的依赖分组。
set "TARGET=%~1"
if "%TARGET%"=="" set "TARGET=all"

REM PackageRoot points at the extracted demo package root.
REM PackageRoot 指向解压后的 demo 包根目录。
set "PACKAGE_ROOT=%~dp0"
set "RUNTIME_ROOT=%PACKAGE_ROOT%runtime"

if /I not "%TARGET%"=="ffi" (
  powershell -NoProfile -ExecutionPolicy Bypass -File "%PACKAGE_ROOT%scripts\deps\fetch_deps.ps1" -Target "%TARGET%" -RuntimeRoot "%RUNTIME_ROOT%"
)
if errorlevel 1 (
  echo Failed to upgrade dependencies.
  pause
  exit /b 1
)
if exist "%PACKAGE_ROOT%scripts\ffi\fetch_ffi.ps1" (
  if /I "%TARGET%"=="all" powershell -NoProfile -ExecutionPolicy Bypass -File "%PACKAGE_ROOT%scripts\ffi\fetch_ffi.ps1" -RuntimeRoot "%RUNTIME_ROOT%" -LuaSkillsVersion "$RELEASE_TAG"
  if /I "%TARGET%"=="ffi" powershell -NoProfile -ExecutionPolicy Bypass -File "%PACKAGE_ROOT%scripts\ffi\fetch_ffi.ps1" -RuntimeRoot "%RUNTIME_ROOT%" -LuaSkillsVersion "$RELEASE_TAG"
)
if errorlevel 1 (
  echo Failed to upgrade FFI dependencies.
  pause
  exit /b 1
)
echo Dependencies are ready.
pause
BAT

  cat > "$package_root/upgrade_deps.sh" <<SH
#!/usr/bin/env bash
set -euo pipefail

# PackageRoot points at the extracted demo package root.
# PackageRoot 指向解压后的 demo 包根目录。
PACKAGE_ROOT="\$(cd "\$(dirname "\${BASH_SOURCE[0]}")" && pwd)"

# Target selects which dependency group to download.
# Target 选择要下载的依赖分组。
TARGET="\${1:-all}"

# RuntimeRoot is the packaged runtime root that receives dependencies.
# RuntimeRoot 是接收依赖的包内运行根目录。
RUNTIME_ROOT="\${RUNTIME_ROOT:-\$PACKAGE_ROOT/runtime}"

if [ "\$TARGET" != "ffi" ]; then
  RUNTIME_ROOT="\$RUNTIME_ROOT" bash "\$PACKAGE_ROOT/scripts/deps/fetch_deps.sh" "\$TARGET"
fi
if [ -f "\$PACKAGE_ROOT/scripts/ffi/fetch_ffi.sh" ] && { [ "\$TARGET" = "all" ] || [ "\$TARGET" = "ffi" ]; }; then
  LUASKILLS_VERSION="\${LUASKILLS_VERSION:-$RELEASE_TAG}" RUNTIME_ROOT="\$RUNTIME_ROOT" bash "\$PACKAGE_ROOT/scripts/ffi/fetch_ffi.sh"
fi
SH
  chmod +x "$package_root/upgrade_deps.sh"
}

write_packaged_demo_readme() {
  # Write a package-root README that matches the generated demo artifact layout.
  # 写入匹配发布 demo 包目录结构的包根 README。
  local mode="$1"
  local package_root="$2"
  local shell_name run_command fetch_all_command fetch_lua_command fetch_vldb_command mode_description examples_section

  if platform_is_windows; then
    shell_name="powershell"
    run_command='.\run.ps1'
    fetch_all_command='.\upgrade_deps.bat'
    fetch_lua_command='.\upgrade_deps.bat lua'
    fetch_vldb_command='.\upgrade_deps.bat vldb'
  else
    shell_name="bash"
    run_command="./run.sh"
    fetch_all_command="./upgrade_deps.sh"
    fetch_lua_command="./upgrade_deps.sh lua"
    fetch_vldb_command="./upgrade_deps.sh vldb"
  fi

  if [ "$mode" = "ffi" ]; then
    mode_description='FFI demo 默认通过 `lib/` 下的动态库运行 `examples/ffi/python/demo.py`，同时包含 C、Go、Python、TypeScript、标准 runtime、安装烟测和宿主 provider 示例。'
    examples_section='- `examples/ffi/`：完整 FFI 示例源码，包含 C、Go、Python、TypeScript 与共享 runtime 夹具。'
  else
    mode_description='Rust demo 通过包内 `Cargo.toml` 直接依赖 `luaskills` 的 `'"$RELEASE_TAG"'` tag，适合验证非 FFI 接入。'
    examples_section=''
  fi

  {
    printf '# LuaSkills %s demo package\n\n' "$mode"
    printf '这是 `luaskills-demo-%s-%s.tar.gz` 解压后的发布包说明，路径与命令均按包根目录设计，不依赖仓库源码布局。\n\n' "$mode" "$PLATFORM"
    printf 'This README describes the extracted `luaskills-demo-%s-%s.tar.gz` package. Paths and commands are package-root based and do not require the source repository layout.\n\n' "$mode" "$PLATFORM"
    printf '## 包内容 / Package Contents\n\n'
    printf -- '- `runtime/`：demo 默认 runtime 根目录，可由分类拉取脚本安装 runtime packages，FFI 模式额外支持 `luaskills-ffi-sdk-%s.tar.gz`。\n' "$PLATFORM"
    printf -- '- `scripts/`：仅包含当前平台可用的依赖拉取脚本。\n'
    printf -- '- `licenses/`：项目与随包组件授权材料。\n'
    printf -- '- `demo-manifest.json`：包模式、平台、runtime 根和可拉取目标清单。\n'
    if [ -n "$examples_section" ]; then
      printf '%s\n' "$examples_section"
    fi
    printf '\n'
    printf '%s\n\n' "$mode_description"
    printf '## 运行 / Run\n\n'
    printf '```%s\n%s\n```\n\n' "$shell_name" "$run_command"
    printf '`run` 脚本只负责运行 demo，不会自动下载依赖。首次使用或升级依赖时请先执行：\n\n'
    printf '```%s\n%s\n```\n\n' "$shell_name" "$fetch_all_command"
    printf '也可以按需单独拉取：\n\n'
    printf '```%s\n%s\n%s\n```\n\n' "$shell_name" "$fetch_lua_command" "$fetch_vldb_command"
    printf 'Windows 包包含 `run.ps1`、`upgrade_deps.bat` 与 `scripts/deps/fetch_deps.ps1`；FFI 包额外包含 `scripts/ffi/fetch_ffi.ps1`。Linux/macOS 包使用对应 `.sh` 脚本。\n\n'
    printf 'Windows packages include `run.ps1`, `upgrade_deps.bat`, and `scripts/deps/fetch_deps.ps1`; FFI packages also include `scripts/ffi/fetch_ffi.ps1`. Linux and macOS packages use the matching `.sh` scripts.\n'
  } > "$package_root/README.md"
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
RELEASE_TAG="$(resolve_release_tag "$RELEASE_TAG")"
PACKAGE_ROOT="target/demo-package/luaskills-demo-$MODE"
rm -rf "$PACKAGE_ROOT"
ensure_dir "$PACKAGE_ROOT/scripts/deps"
ensure_dir "$PACKAGE_ROOT/runtime"
ensure_dir "$PACKAGE_ROOT/licenses"
ensure_dir "$OUTPUT_DIR"

cp -a "examples/demo-$MODE"/. "$PACKAGE_ROOT"/

# Build caches are useful locally but must not be shipped in the demo source package.
# 构建缓存仅对本地开发有用，不能进入 demo 源码包产物。
rm -rf "$PACKAGE_ROOT/target"

cp -a "examples/ffi/standard_runtime/runtime_root"/. "$PACKAGE_ROOT/runtime"/
if platform_is_windows; then
  cp -f scripts/deps/fetch_deps.ps1 "$PACKAGE_ROOT/scripts/deps/fetch_deps.ps1"
else
  cp -f scripts/deps/fetch_deps.sh "$PACKAGE_ROOT/scripts/deps/fetch_deps.sh"
  chmod +x "$PACKAGE_ROOT/scripts/deps/fetch_deps.sh"
fi
cp -f LICENSE "$PACKAGE_ROOT/licenses/LICENSE"

if [ "$MODE" = "ffi" ]; then
  ensure_dir "$PACKAGE_ROOT/scripts/ffi"
  if platform_is_windows; then
    cp -f scripts/ffi/fetch_ffi.ps1 "$PACKAGE_ROOT/scripts/ffi/fetch_ffi.ps1"
  else
    cp -f scripts/ffi/fetch_ffi.sh "$PACKAGE_ROOT/scripts/ffi/fetch_ffi.sh"
    chmod +x "$PACKAGE_ROOT/scripts/ffi/fetch_ffi.sh"
  fi
  ensure_dir "$PACKAGE_ROOT/include"
  ensure_dir "$PACKAGE_ROOT/lib"
  cp -f include/*.h "$PACKAGE_ROOT/include/"
  copy_ffi_example_sources "$PACKAGE_ROOT"
  find target/release -maxdepth 1 -type f \( -name '*.dll' -o -name '*.lib' -o -name '*.so' -o -name '*.dylib' -o -name '*.a' \) -exec cp -f {} "$PACKAGE_ROOT/lib/" \; 2>/dev/null || true
else
  if [ -f "$PACKAGE_ROOT/Cargo.toml" ]; then
    python3 - "$PACKAGE_ROOT/Cargo.toml" "$RELEASE_TAG" "$SOURCE_COMMIT" <<'PY'
from pathlib import Path
import sys
# Path identifies the packaged Rust dependency manifest, never the repository's root manifest.
# Path 标识已打包 Rust 依赖清单，绝不是仓库根清单。
path = Path(sys.argv[1])
# Text preserves the existing example's actual dependency declaration.
# Text 保留既有示例实际依赖声明。
text = path.read_text(encoding="utf-8")
# RevisionKind and Revision use the explicit candidate SHA when requested by the frozen workflow.
# RevisionKind 及 Revision 使用冻结工作流请求的显式候选 SHA。
revision_kind = "rev" if sys.argv[3] else "tag"
revision = sys.argv[3] if sys.argv[3] else sys.argv[2]
text = text.replace(
    'luaskills = { path = "../.." }',
    f'luaskills = {{ git = "https://github.com/LuaSkills/luaskills.git", {revision_kind} = "{revision}" }}',
)
path.write_text(text, encoding="utf-8")
PY
  fi
fi

write_packaged_demo_scripts "$MODE" "$PACKAGE_ROOT"
write_packaged_demo_readme "$MODE" "$PACKAGE_ROOT"

cat > "$PACKAGE_ROOT/demo-manifest.json" <<JSON
{
  "schema_version": 1,
  "package_name": "luaskills-demo-${MODE}-${PLATFORM}",
  "platform": "${PLATFORM}",
  "mode": "${MODE}",
  "runtime_root": "runtime",
  "release_tag": "${RELEASE_TAG}",
  "fetch_targets": ["all", "lua", "vldb"]
}
JSON

ARCHIVE_NAME="luaskills-demo-${MODE}-${PLATFORM}.tar.gz"
create_tar_from_dir "$PACKAGE_ROOT" "$OUTPUT_DIR/$ARCHIVE_NAME"
echo "Demo package created: $OUTPUT_DIR/$ARCHIVE_NAME"
