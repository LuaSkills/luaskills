#!/usr/bin/env bash
set -euo pipefail

# ProjectRoot points at the repository root regardless of the caller location.
# ProjectRoot 指向仓库根目录，避免调用方当前位置影响路径解析。
PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

# Platform stores the release asset platform key.
# Platform 保存发布资产使用的平台标识。
PLATFORM="${1:-}"

# OutputDir stores final release archives.
# OutputDir 保存最终发布压缩包。
OUTPUT_DIR="${3:-${OUTPUT_DIR:-target/release-packages}}"

# ReleaseTag stores the LuaSkills release tag recorded in package manifests.
# ReleaseTag 保存写入包清单的 LuaSkills 发布标签。
RELEASE_TAG="${2:-${RELEASE_TAG:-}}"

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

debug_binary_name() {
  # Resolve the release-mode debug binary file name for the target package platform.
  # 解析目标包平台对应的 release 模式调试二进制文件名。
  if platform_is_windows; then
    printf 'luaskills-debug.exe\n'
  else
    printf 'luaskills-debug\n'
  fi
}

write_debug_runtime_setup_scripts() {
  # Write runtime bootstrap scripts that fetch Lua runtime packages into the debug workspace.
  # 写入运行时初始化脚本，将 Lua runtime packages 拉取到调试工作区。
  local package_root="$1"

  if platform_is_windows; then
    cat > "$package_root/setup_runtime.ps1" <<PS1
param(
    # Dependency target to fetch for this debug package.
    # 当前调试包需要拉取的依赖目标。
    [ValidateSet("all", "lua", "vldb", "vldb-controller", "vldb-direct")]
    [string]\$Target = "lua",
    # Optional database dependency preset used only by all/vldb targets.
    # 仅 all/vldb 目标使用的可选数据库依赖预设。
    [ValidateSet("none", "vldb-controller", "vldb-direct", "host-callback")]
    [string]\$Database = "none"
)

\$ErrorActionPreference = "Stop"

# PackageRoot points at the extracted debug tool package root.
# PackageRoot 指向解压后的调试工具包根目录。
\$PackageRoot = if (\$PSScriptRoot) { \$PSScriptRoot } else { (Get-Location).Path }

# RuntimeRoot receives Lua runtime packages and optional native helpers.
# RuntimeRoot 接收 Lua runtime packages 与可选原生辅助工具。
\$RuntimeRoot = Join-Path \$PackageRoot "runtime"

New-Item -ItemType Directory -Force -Path \$RuntimeRoot | Out-Null
powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path \$PackageRoot "scripts\\deps\\fetch_deps.ps1") -Target \$Target -Database \$Database -RuntimeRoot \$RuntimeRoot
\$FetchExitCode = \$LASTEXITCODE
if (\$FetchExitCode -ne 0) {
    exit \$FetchExitCode
}
Write-Host "Debug runtime dependencies are ready under \$RuntimeRoot"
PS1
  cat > "$package_root/upgrade_deps.bat" <<'BAT'
@echo off
chcp 65001 >nul
setlocal
REM Target selects which dependency group to download.
REM Target 选择要下载的依赖分组。
set "TARGET=%~1"
if "%TARGET%"=="" set "TARGET=lua"

REM Database selects the optional database dependency preset.
REM Database 选择可选数据库依赖预设。
set "DATABASE=%~2"
if "%DATABASE%"=="" set "DATABASE=none"

powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0setup_runtime.ps1" -Target "%TARGET%" -Database "%DATABASE%"
if errorlevel 1 (
  echo Failed to prepare debug runtime dependencies.
  pause
  exit /b 1
)
echo Debug runtime dependencies are ready.
pause
BAT

    cat > "$package_root/scripts/deps/fetch_deps.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail

to_windows_path() {
  local raw_path="$1"
  case "$raw_path" in
    [A-Za-z]:/*|[A-Za-z]:\\*)
      printf '%s' "$raw_path" | sed 's#/#\\#g'
      printf '\n'
      return 0
      ;;
    /[A-Za-z]/*)
      local drive_letter=""
      local remainder=""
      drive_letter="${raw_path:1:1}"
      remainder="${raw_path:3}"
      remainder="$(printf '%s' "$remainder" | sed 's#/#\\#g')"
      printf '%s:\\%s\n' "$drive_letter" "$remainder"
      return 0
      ;;
  esac
  if command -v cygpath >/dev/null 2>&1; then
    cygpath -w "$raw_path"
    return 0
  fi
  if command -v wslpath >/dev/null 2>&1; then
    wslpath -w "$raw_path"
    return 0
  fi
  if [ -d "$raw_path" ]; then
    (cd "$raw_path" && pwd -W)
    return 0
  fi
  if [ -e "$raw_path" ]; then
    local parent_dir=""
    parent_dir="$(cd "$(dirname "$raw_path")" && pwd -W)"
    printf '%s\%s\n' "$parent_dir" "$(basename "$raw_path")"
    return 0
  fi
  printf '%s\n' "$raw_path"
}

resolve_powershell_host() {
  local candidate=""
  for candidate in powershell.exe pwsh.exe powershell pwsh; do
    if command -v "$candidate" >/dev/null 2>&1; then
      command -v "$candidate"
      return 0
    fi
  done
  for candidate in \
    "/mnt/c/Windows/System32/WindowsPowerShell/v1.0/powershell.exe" \
    "/mnt/c/Program Files/PowerShell/7/pwsh.exe"; do
    if [ -x "$candidate" ]; then
      printf '%s\n' "$candidate"
      return 0
    fi
  done
  echo "No PowerShell host was found for the Windows debug package shell wrapper." >&2
  return 1
}

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PACKAGE_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
PACKAGE_ROOT_WIN="$(to_windows_path "$PACKAGE_ROOT")"
TARGET="${1:-all}"
DATABASE="${2:-vldb-controller}"
RUNTIME_ROOT="${RUNTIME_ROOT:-$PACKAGE_ROOT/runtime}"
RUNTIME_ROOT_WIN="$(to_windows_path "$RUNTIME_ROOT")"
POWERSHELL_HOST="$(resolve_powershell_host)"

FORWARDED_ARGS=(
  -NoProfile
  -ExecutionPolicy
  Bypass
  -File
  "${PACKAGE_ROOT_WIN}\scripts\deps\fetch_deps.ps1"
  -Target
  "$TARGET"
  -Database
  "$DATABASE"
  -RuntimeRoot
  "$RUNTIME_ROOT_WIN"
)

if [ "${LUA_PACKAGES_ONLY:-0}" = "1" ]; then
  FORWARDED_ARGS+=(-LuaPackagesOnly)
fi
if [ "${SKIP_LUA_RUNTIME_LIBS:-0}" = "1" ]; then
  FORWARDED_ARGS+=(-SkipLuaRuntimeLibs)
fi

exec "$POWERSHELL_HOST" "${FORWARDED_ARGS[@]}"
SH
    chmod +x "$package_root/scripts/deps/fetch_deps.sh"

    cat > "$package_root/setup_runtime.sh" <<SH
#!/usr/bin/env bash
set -euo pipefail

PACKAGE_ROOT="\$(cd "\$(dirname "\${BASH_SOURCE[0]}")" && pwd)"
TARGET="\${1:-lua}"
DATABASE="\${2:-none}"
RUNTIME_ROOT="\${RUNTIME_ROOT:-\$PACKAGE_ROOT/runtime}"

mkdir -p "\$RUNTIME_ROOT"
RUNTIME_ROOT="\$RUNTIME_ROOT" bash "\$PACKAGE_ROOT/scripts/deps/fetch_deps.sh" "\$TARGET" "\$DATABASE"
echo "Debug runtime dependencies are ready under \$RUNTIME_ROOT"
SH
    chmod +x "$package_root/setup_runtime.sh"

    cat > "$package_root/upgrade_deps.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail

PACKAGE_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TARGET="${1:-lua}"
DATABASE="${2:-none}"

exec bash "$PACKAGE_ROOT/setup_runtime.sh" "$TARGET" "$DATABASE"
SH
    chmod +x "$package_root/upgrade_deps.sh"
    return
  fi

  cat > "$package_root/setup_runtime.sh" <<SH
#!/usr/bin/env bash
set -euo pipefail

# PackageRoot points at the extracted debug tool package root.
# PackageRoot 指向解压后的调试工具包根目录。
PACKAGE_ROOT="\$(cd "\$(dirname "\${BASH_SOURCE[0]}")" && pwd)"

# Target selects which dependency group to fetch for this debug package.
# Target 选择当前调试包需要拉取的依赖分组。
TARGET="\${1:-lua}"

# Database selects the optional database dependency preset used by all/vldb targets.
# Database 选择 all/vldb 目标使用的可选数据库依赖预设。
DATABASE="\${2:-none}"

# RuntimeRoot receives Lua runtime packages and optional native helpers.
# RuntimeRoot 接收 Lua runtime packages 与可选原生辅助工具。
RUNTIME_ROOT="\${RUNTIME_ROOT:-\$PACKAGE_ROOT/runtime}"

mkdir -p "\$RUNTIME_ROOT"
RUNTIME_ROOT="\$RUNTIME_ROOT" bash "\$PACKAGE_ROOT/scripts/deps/fetch_deps.sh" "\$TARGET" "\$DATABASE"
echo "Debug runtime dependencies are ready under \$RUNTIME_ROOT"
SH
  chmod +x "$package_root/setup_runtime.sh"

  cat > "$package_root/upgrade_deps.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail

# PackageRoot points at the extracted debug tool package root.
# PackageRoot 指向解压后的调试工具包根目录。
PACKAGE_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Target selects which dependency group to download.
# Target 选择要下载的依赖分组。
TARGET="${1:-lua}"

# Database selects the optional database dependency preset.
# Database 选择可选数据库依赖预设。
DATABASE="${2:-none}"

bash "$PACKAGE_ROOT/setup_runtime.sh" "$TARGET" "$DATABASE"
SH
  chmod +x "$package_root/upgrade_deps.sh"
}

write_debug_launcher_scripts() {
  # Write package-root launchers that auto-detect one skill under skills/.
  # 写入包根启动脚本，自动发现 skills/ 下的单个 skill。
  local package_root="$1"

  if platform_is_windows; then
    cat > "$package_root/debug.ps1" <<'PS1'
param(
    # Debug command forwarded to luaskills-debug.
    # 转发给 luaskills-debug 的调试命令。
    [ValidateSet("sync", "inspect", "list-tools", "call")]
    [string]$Command = "inspect",
    # Optional explicit skill package directory.
    # 可选显式 skill 包目录。
    [string]$SkillPath = "",
    # Optional synchronized skill id used by run-only commands.
    # 运行命令使用的可选已同步 skill 标识符。
    [string]$SkillId = "",
    # Tool name used by the call command.
    # call 命令使用的工具名称。
    [string]$Tool = "",
    # Inline JSON payload used by the call command.
    # call 命令使用的内联 JSON 入参。
    [string]$ArgsJson = "",
    # JSON file path used by the call command.
    # call 命令使用的 JSON 文件路径。
    [string]$ArgsFile = "",
    # Output rendering mode forwarded to luaskills-debug.
    # 转发给 luaskills-debug 的输出渲染模式。
    [ValidateSet("pretty", "json", "content")]
    [string]$Output = "pretty",
    # Whether to enable the host_result bridge for the call command.
    # 是否为 call 命令启用 host_result 桥接。
    [switch]$EnableHostResult
)

$ErrorActionPreference = "Stop"

function Resolve-DefaultSkillPath {
    <#
    .SYNOPSIS
    Resolve the only skill package placed under the package skills directory.
    解析放在包内 skills 目录下的唯一 skill 包。

    .PARAMETER SkillsRoot
    Package-local skills directory.
    包内 skills 目录。
    #>
    param([string]$SkillsRoot)

    if (Test-Path -LiteralPath (Join-Path $SkillsRoot "skill.yaml")) {
        return (Resolve-Path -LiteralPath $SkillsRoot).Path
    }

    $Candidates = @(Get-ChildItem -Directory -LiteralPath $SkillsRoot -ErrorAction SilentlyContinue | Where-Object {
        Test-Path -LiteralPath (Join-Path $_.FullName "skill.yaml")
    })
    if ($Candidates.Count -eq 1) {
        return $Candidates[0].FullName
    }
    if ($Candidates.Count -eq 0) {
        throw "No skill package was found. Put one skill directory under '$SkillsRoot' or pass -SkillPath."
    }
    $Names = ($Candidates | ForEach-Object { $_.Name }) -join ", "
    throw "Multiple skill packages were found under '$SkillsRoot': $Names. Pass -SkillPath to choose one."
}

# PackageRoot points at the extracted debug tool package root.
# PackageRoot 指向解压后的调试工具包根目录。
$PackageRoot = if ($PSScriptRoot) { $PSScriptRoot } else { (Get-Location).Path }

# RuntimeRoot is the package-local runtime root used for debug execution.
# RuntimeRoot 是调试执行使用的包内运行根目录。
$RuntimeRoot = Join-Path $PackageRoot "runtime"

# SkillsRoot is where developers can drop one skill directory for quick debugging.
# SkillsRoot 是开发者可直接放入单个 skill 目录进行快速调试的位置。
$SkillsRoot = Join-Path $PackageRoot "skills"

# BinaryPath points at the packaged release-mode debug binary.
# BinaryPath 指向包内 release 模式调试二进制程序。
$BinaryPath = Join-Path $PackageRoot "bin\luaskills-debug.exe"

if (-not (Test-Path -LiteralPath $BinaryPath)) {
    throw "luaskills-debug executable was not found: $BinaryPath"
}
if (-not (Test-Path -LiteralPath (Join-Path $RuntimeRoot "resources\luaskills-packages-manifest.json"))) {
    Write-Warning "Lua runtime packages do not appear to be installed. Run '.\setup_runtime.ps1' first if the skill needs packaged Lua dependencies."
}

$EffectiveSkillPath = if ([string]::IsNullOrWhiteSpace($SkillPath) -and [string]::IsNullOrWhiteSpace($SkillId)) {
    Resolve-DefaultSkillPath -SkillsRoot $SkillsRoot
} elseif (-not [string]::IsNullOrWhiteSpace($SkillPath)) {
    (Resolve-Path -LiteralPath $SkillPath).Path
} else {
    ""
}

$ForwardedArgs = @($Command, "--runtime-root", $RuntimeRoot)
if ($EffectiveSkillPath) {
    $ForwardedArgs += @("--skill-path", $EffectiveSkillPath)
}
if ($SkillId) {
    $ForwardedArgs += @("--skill-id", $SkillId)
}
if ($Tool) {
    $ForwardedArgs += @("--tool", $Tool)
}
if ($ArgsJson) {
    $ForwardedArgs += @("--args-json", $ArgsJson)
}
if ($ArgsFile) {
    $ForwardedArgs += @("--args-file", (Resolve-Path -LiteralPath $ArgsFile).Path)
}
if ($Output) {
    $ForwardedArgs += @("--output", $Output)
}
if ($EnableHostResult) {
    $ForwardedArgs += "--enable-host-result"
}

& $BinaryPath @ForwardedArgs
exit $LASTEXITCODE
PS1
  cat > "$package_root/debug.bat" <<'BAT'
@echo off
chcp 65001 >nul
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0debug.ps1" %*
exit /b %ERRORLEVEL%
BAT
    cat > "$package_root/debug.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail

to_windows_path() {
  local raw_path="$1"
  case "$raw_path" in
    [A-Za-z]:/*|[A-Za-z]:\\*)
      printf '%s' "$raw_path" | sed 's#/#\\#g'
      printf '\n'
      return 0
      ;;
    /[A-Za-z]/*)
      local drive_letter=""
      local remainder=""
      drive_letter="${raw_path:1:1}"
      remainder="${raw_path:3}"
      remainder="$(printf '%s' "$remainder" | sed 's#/#\\#g')"
      printf '%s:\\%s\n' "$drive_letter" "$remainder"
      return 0
      ;;
  esac
  if command -v cygpath >/dev/null 2>&1; then
    cygpath -w "$raw_path"
    return 0
  fi
  if command -v wslpath >/dev/null 2>&1; then
    wslpath -w "$raw_path"
    return 0
  fi
  if [ -d "$raw_path" ]; then
    (cd "$raw_path" && pwd -W)
    return 0
  fi
  if [ -e "$raw_path" ]; then
    local parent_dir=""
    parent_dir="$(cd "$(dirname "$raw_path")" && pwd -W)"
    printf '%s\%s\n' "$parent_dir" "$(basename "$raw_path")"
    return 0
  fi
  printf '%s\n' "$raw_path"
}

resolve_powershell_host() {
  local candidate=""
  for candidate in powershell.exe pwsh.exe powershell pwsh; do
    if command -v "$candidate" >/dev/null 2>&1; then
      command -v "$candidate"
      return 0
    fi
  done
  for candidate in \
    "/mnt/c/Windows/System32/WindowsPowerShell/v1.0/powershell.exe" \
    "/mnt/c/Program Files/PowerShell/7/pwsh.exe"; do
    if [ -x "$candidate" ]; then
      printf '%s\n' "$candidate"
      return 0
    fi
  done
  echo "No PowerShell host was found for the Windows debug package shell wrapper." >&2
  return 1
}

PACKAGE_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DEBUG_PS1_WIN="$(to_windows_path "$PACKAGE_ROOT/debug.ps1")"
POWERSHELL_HOST="$(resolve_powershell_host)"
FORWARDED_ARGS=()

if [ "${1:-}" = "inspect" ] || [ "${1:-}" = "list-tools" ] || [ "${1:-}" = "call" ]; then
  FORWARDED_ARGS+=("-Command" "$1")
  shift
fi

while [ "$#" -gt 0 ]; do
  case "$1" in
    -SkillPath|--skill-path)
      FORWARDED_ARGS+=("-SkillPath" "$(to_windows_path "${2:?--skill-path requires a value}")")
      shift 2
      ;;
    -ArgsFile|--args-file)
      FORWARDED_ARGS+=("-ArgsFile" "$(to_windows_path "${2:?--args-file requires a value}")")
      shift 2
      ;;
    *)
      FORWARDED_ARGS+=("$1")
      shift
      ;;
  esac
done

exec "$POWERSHELL_HOST" -NoProfile -ExecutionPolicy Bypass -File "$DEBUG_PS1_WIN" "${FORWARDED_ARGS[@]}"
SH
    chmod +x "$package_root/debug.sh"
    return
  fi

  cat > "$package_root/debug.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail

# PackageRoot points at the extracted debug tool package root.
# PackageRoot 指向解压后的调试工具包根目录。
PACKAGE_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# RuntimeRoot is the package-local runtime root used for debug execution.
# RuntimeRoot 是调试执行使用的包内运行根目录。
RUNTIME_ROOT="$PACKAGE_ROOT/runtime"

# SkillsRoot is where developers can drop one skill directory for quick debugging.
# SkillsRoot 是开发者可直接放入单个 skill 目录进行快速调试的位置。
SKILLS_ROOT="$PACKAGE_ROOT/skills"

# BinaryPath points at the packaged release-mode debug binary.
# BinaryPath 指向包内 release 模式调试二进制程序。
BINARY_PATH="$PACKAGE_ROOT/bin/luaskills-debug"

resolve_default_skill_path() {
  # Resolve the only skill package placed under the package skills directory.
  # 解析放在包内 skills 目录下的唯一 skill 包。
  if [ -f "$SKILLS_ROOT/skill.yaml" ]; then
    cd "$SKILLS_ROOT" && pwd
    return 0
  fi

  local candidates=()
  local manifest=""
  for manifest in "$SKILLS_ROOT"/*/skill.yaml; do
    [ -f "$manifest" ] || continue
    candidates+=("$(cd "$(dirname "$manifest")" && pwd)")
  done

  if [ "${#candidates[@]}" -eq 1 ]; then
    printf '%s\n' "${candidates[0]}"
    return 0
  fi
  if [ "${#candidates[@]}" -eq 0 ]; then
    echo "No skill package was found. Put one skill directory under '$SKILLS_ROOT' or pass --skill-path." >&2
    return 1
  fi
  printf "Multiple skill packages were found under '%s'. Pass --skill-path to choose one.\n" "$SKILLS_ROOT" >&2
  return 1
}

COMMAND="inspect"
if [ "${1:-}" = "sync" ] || [ "${1:-}" = "inspect" ] || [ "${1:-}" = "list-tools" ] || [ "${1:-}" = "call" ]; then
  COMMAND="$1"
  shift
fi

SKILL_PATH=""
SKILL_ID=""
TOOL=""
ARGS_JSON=""
ARGS_FILE=""
OUTPUT="pretty"
ENABLE_HOST_RESULT="false"

while [ "$#" -gt 0 ]; do
  case "$1" in
    --skill-path)
      SKILL_PATH="${2:?--skill-path requires a value}"
      shift 2
      ;;
    --skill-id)
      SKILL_ID="${2:?--skill-id requires a value}"
      shift 2
      ;;
    --tool)
      TOOL="${2:?--tool requires a value}"
      shift 2
      ;;
    --args-json)
      ARGS_JSON="${2:?--args-json requires a value}"
      shift 2
      ;;
    --args-file)
      ARGS_FILE="${2:?--args-file requires a value}"
      shift 2
      ;;
    --output)
      OUTPUT="${2:?--output requires a value}"
      shift 2
      ;;
    --enable-host-result)
      ENABLE_HOST_RESULT="true"
      shift
      ;;
    -h|--help)
      "$BINARY_PATH" --help
      exit 0
      ;;
    *)
      echo "Unknown debug launcher argument: $1" >&2
      exit 2
      ;;
  esac
done

[ -x "$BINARY_PATH" ] || { echo "luaskills-debug executable was not found or is not executable: $BINARY_PATH" >&2; exit 1; }
if [ ! -f "$RUNTIME_ROOT/resources/luaskills-packages-manifest.json" ]; then
  echo "WARNING: Lua packages do not appear to be installed. Run './setup_runtime.sh' first if the skill needs packaged Lua dependencies." >&2
fi

if [ -z "$SKILL_PATH" ] && [ -z "$SKILL_ID" ]; then
  SKILL_PATH="$(resolve_default_skill_path)"
elif [ -n "$SKILL_PATH" ]; then
  SKILL_PATH="$(cd "$SKILL_PATH" && pwd)"
fi

FORWARDED_ARGS=("$COMMAND" "--runtime-root" "$RUNTIME_ROOT")
if [ -n "$SKILL_PATH" ]; then
  FORWARDED_ARGS+=("--skill-path" "$SKILL_PATH")
fi
if [ -n "$SKILL_ID" ]; then
  FORWARDED_ARGS+=("--skill-id" "$SKILL_ID")
fi
if [ -n "$TOOL" ]; then
  FORWARDED_ARGS+=("--tool" "$TOOL")
fi
if [ -n "$ARGS_JSON" ]; then
  FORWARDED_ARGS+=("--args-json" "$ARGS_JSON")
fi
if [ -n "$ARGS_FILE" ]; then
  FORWARDED_ARGS+=("--args-file" "$(cd "$(dirname "$ARGS_FILE")" && pwd)/$(basename "$ARGS_FILE")")
fi
if [ -n "$OUTPUT" ]; then
  FORWARDED_ARGS+=("--output" "$OUTPUT")
fi
if [ "$ENABLE_HOST_RESULT" = "true" ]; then
  FORWARDED_ARGS+=("--enable-host-result")
fi

exec "$BINARY_PATH" "${FORWARDED_ARGS[@]}"
SH
  chmod +x "$package_root/debug.sh"
}

write_debug_package_readme() {
  # Write a package-root README for the standalone debug tool package.
  # 为独立调试工具包写入包根 README。
  local package_root="$1"
  local shell_name setup_command inspect_command list_command call_command binary_path

  if platform_is_windows; then
    shell_name="powershell"
    setup_command='.\\setup_runtime.ps1'
    inspect_command='.\\debug.ps1 inspect'
    list_command='.\\debug.ps1 list-tools'
    call_command='.\\debug.ps1 call -Tool ping -ArgsJson "{}"'
    binary_path='bin/luaskills-debug.exe'
  else
    shell_name="bash"
    setup_command="./setup_runtime.sh"
    inspect_command="./debug.sh inspect"
    list_command="./debug.sh list-tools"
    call_command='./debug.sh call --tool ping --args-json "{}"'
    binary_path='bin/luaskills-debug'
  fi

  {
    printf '# LuaSkills debug tool package\n\n'
    printf 'This package is a standalone skill-debug workspace for %s. It contains the release-mode `luaskills-debug` executable, a package-local `runtime/` directory, a `skills/` drop-in directory, and scripts that fetch the latest compatible Lua runtime packages.\n\n' "$PLATFORM"
    printf '## Package Contents\n\n'
    printf -- '- `%s`: standalone debug executable.\n' "$binary_path"
    printf -- '- `runtime/`: package-local runtime root used by debug commands.\n'
    printf -- '- `skills/`: place exactly one skill package directory here for quick debugging.\n'
    printf -- '- `scripts/`: platform-matching dependency fetch script.\n'
    printf -- '- `setup_runtime.*` / `upgrade_deps.*`: fetch Lua runtime packages into `runtime/`.\n'
    printf -- '- `debug.*`: convenience launcher that auto-detects one skill under `skills/`.\n\n'
    printf '## Quick Start\n\n'
    printf '```%s\n%s\n%s\n%s\n%s\n```\n\n' "$shell_name" "$setup_command" "$inspect_command" "$list_command" "$call_command"
    printf 'The default setup command fetches the `lua` target and stages `runtime/lua_packages/`, `runtime/libs/`, `runtime/resources/`, and `runtime/licenses/` metadata. It does not download the extra LuaSkills FFI SDK.\n\n'
    printf 'Use `all` only when you also need database helper binaries. The Lua setup path stays metadata-complete so packaged-runtime manifest validation remains consistent.\n\n'
    printf '## 中文说明\n\n'
    printf '这个包是独立的 skill 调试工作台。解压后先执行初始化脚本拉取 Lua runtime packages，然后把一个 skill 目录放到 `skills/` 下，就可以通过 `debug` 启动脚本执行 `inspect`、`list-tools` 或 `call`。\n\n'
    printf '```%s\n%s\n%s\n%s\n%s\n```\n\n' "$shell_name" "$setup_command" "$inspect_command" "$list_command" "$call_command"
    printf '默认初始化会把 `runtime/lua_packages/`、`runtime/libs/`、`runtime/resources/` 与 `runtime/licenses/` 元数据放进调试工作区，不会额外下载 LuaSkills FFI SDK。只有需要数据库辅助进程时才建议执行 `all`。\n'
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
PACKAGE_ROOT="target/debug-tool-package/luaskills-debug-tool"
rm -rf "$PACKAGE_ROOT"
ensure_dir "$PACKAGE_ROOT/bin"
ensure_dir "$PACKAGE_ROOT/runtime"
ensure_dir "$PACKAGE_ROOT/skills"
ensure_dir "$PACKAGE_ROOT/scripts/deps"
ensure_dir "$PACKAGE_ROOT/licenses"
ensure_dir "$OUTPUT_DIR"

cat > "$PACKAGE_ROOT/skills/README.md" <<'MD'
# Put one skill package directory here

Place exactly one directory that contains skill.yaml under this skills/ folder.

Examples:
- skills/your-skill/skill.yaml
- skills/vulcan-file/skill.yaml

You can also leave this folder empty and pass -SkillPath or --skill-path to the debug launcher.
MD

DEBUG_BINARY_NAME="$(debug_binary_name)"
DEBUG_BINARY_PATH="target/release/$DEBUG_BINARY_NAME"
if [ ! -f "$DEBUG_BINARY_PATH" ]; then
  echo "Missing release debug binary: $DEBUG_BINARY_PATH. Run 'cargo build --release --bin luaskills-debug' first." >&2
  exit 1
fi

cp -f "$DEBUG_BINARY_PATH" "$PACKAGE_ROOT/bin/$DEBUG_BINARY_NAME"
if ! platform_is_windows; then
  chmod +x "$PACKAGE_ROOT/bin/$DEBUG_BINARY_NAME"
fi
# A release build places the detached OpenPGP signature beside the binary; local builds have none.
# 发行构建在二进制旁放置分离式 OpenPGP 签名；本地构建没有签名。
if [ -f "$DEBUG_BINARY_PATH.asc" ]; then
  cp -f "$DEBUG_BINARY_PATH.asc" "$PACKAGE_ROOT/bin/$DEBUG_BINARY_NAME.asc"
fi
cp -f LICENSE "$PACKAGE_ROOT/licenses/LICENSE"
if platform_is_windows; then
  cp -f scripts/deps/fetch_deps.ps1 "$PACKAGE_ROOT/scripts/deps/fetch_deps.ps1"
else
  cp -f scripts/deps/fetch_deps.sh "$PACKAGE_ROOT/scripts/deps/fetch_deps.sh"
  chmod +x "$PACKAGE_ROOT/scripts/deps/fetch_deps.sh"
fi

write_debug_runtime_setup_scripts "$PACKAGE_ROOT"
write_debug_launcher_scripts "$PACKAGE_ROOT"
write_debug_package_readme "$PACKAGE_ROOT"

cat > "$PACKAGE_ROOT/debug-tool-manifest.json" <<JSON
{
  "schema_version": 1,
  "package_name": "luaskills-debug-tool-${PLATFORM}",
  "platform": "${PLATFORM}",
  "binary": "bin/${DEBUG_BINARY_NAME}",
  "runtime_root": "runtime",
  "skills_dir": "skills",
  "release_tag": "${RELEASE_TAG}",
  "default_fetch_target": "lua",
  "fetch_targets": ["lua", "all", "vldb", "vldb-controller", "vldb-direct"]
}
JSON

ARCHIVE_NAME="luaskills-debug-tool-${PLATFORM}.tar.gz"
create_tar_from_dir "$PACKAGE_ROOT" "$OUTPUT_DIR/$ARCHIVE_NAME"
echo "Debug tool package created: $OUTPUT_DIR/$ARCHIVE_NAME"
