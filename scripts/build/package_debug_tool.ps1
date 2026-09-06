param(
    # Target platform key used in archive and manifest names.
    # 用于归档文件与清单文件命名的目标平台标识。
    [string]$Platform = "",
    # Output directory that receives the final archive.
    # 接收最终压缩包的输出目录。
    [string]$OutputDir = "target\release-packages",
    # LuaSkills release tag used by dependency bootstrap scripts.
    # 依赖初始化脚本使用的 LuaSkills 发布标签。
    [string]$ReleaseTag = ""
)

$ErrorActionPreference = "Stop"

# ScriptDir points at the current script directory when PowerShell exposes it.
# ScriptDir 在 PowerShell 提供脚本路径时指向当前脚本目录。
$ScriptDir = if ($PSScriptRoot) { $PSScriptRoot } elseif ($PSCommandPath) { Split-Path -Parent $PSCommandPath } elseif ($MyInvocation.MyCommand.Path) { Split-Path -Parent $MyInvocation.MyCommand.Path } else { "" }

# ProjectRootHelperPath selects the shared build-script root resolver from script or repository context.
# ProjectRootHelperPath 从脚本或仓库上下文选择共享构建脚本根解析器。
$ProjectRootHelperPath = if ($ScriptDir) { Join-Path $ScriptDir "project_root.ps1" } else { Join-Path (Get-Location).Path "scripts\build\project_root.ps1" }
. $ProjectRootHelperPath

# ArchiveHelperPath selects the shared checked archive helper from script or repository context.
# ArchiveHelperPath 从脚本或仓库上下文选择共享的受检归档辅助脚本。
$ArchiveHelperPath = if ($ScriptDir) { Join-Path $ScriptDir "archive_helpers.ps1" } else { Join-Path (Get-Location).Path "scripts\build\archive_helpers.ps1" }
. $ArchiveHelperPath

# ProjectRoot points at the repository root regardless of the caller location.
# ProjectRoot 指向仓库根目录，避免调用方当前位置影响路径解析。
$ProjectRoot = Resolve-ProjectRoot -ScriptDirectory $ScriptDir
Set-Location $ProjectRoot

function Ensure-Dir {
    <#
    .SYNOPSIS
    Create one directory when it does not exist.
    在目录不存在时创建该目录。

    .PARAMETER Path
    Directory path to create.
    需要创建的目录路径。
    #>
    param([string]$Path)

    if ([string]::IsNullOrWhiteSpace($Path)) {
        throw "Ensure-Dir requires a non-empty path."
    }
    if (-not (Test-Path -LiteralPath $Path)) {
        New-Item -ItemType Directory -Path $Path -Force | Out-Null
    }
}

function Test-WindowsPackagePlatform {
    <#
    .SYNOPSIS
    Check whether one package platform key targets Windows.
    检查一个包平台标识是否面向 Windows。

    .PARAMETER PlatformKey
    Platform key such as windows-x64, linux-x64, or macos-arm64.
    形如 windows-x64、linux-x64 或 macos-arm64 的平台标识。

    .OUTPUTS
    Boolean value indicating whether the platform is Windows.
    表示平台是否为 Windows 的布尔值。
    #>
    param([string]$PlatformKey)

    return $PlatformKey -like "windows-*"
}

function Get-DebugBinaryName {
    <#
    .SYNOPSIS
    Resolve the release-mode debug binary file name for one package platform.
    解析单个目标平台对应的 release 模式调试二进制文件名。

    .PARAMETER PlatformKey
    Platform key used by the release package.
    发布包使用的平台标识。

    .OUTPUTS
    Platform-specific debug binary file name.
    平台专属的调试二进制文件名。
    #>
    param([string]$PlatformKey)

    if (Test-WindowsPackagePlatform -PlatformKey $PlatformKey) {
        return "luaskills-debug.exe"
    }
    return "luaskills-debug"
}

function Write-DebugRuntimeSetupScripts {
    <#
    .SYNOPSIS
    Write runtime bootstrap scripts that fetch Lua runtime packages into the debug workspace.
    写入运行时初始化脚本，将 Lua runtime packages 拉取到调试工作区。

    .PARAMETER PackageRoot
    Package root that receives setup scripts.
    接收初始化脚本的包根目录。

    .PARAMETER PlatformKey
    Target package platform used to choose launcher scripts.
    用于选择启动脚本的目标包平台。

    .PARAMETER ReleaseTag
    LuaSkills release tag forwarded to dependency fetch scripts.
    转发给依赖拉取脚本的 LuaSkills 发布标签。
    #>
    param(
        [string]$PackageRoot,
        [string]$PlatformKey,
        [string]$ReleaseTag
    )

    if (Test-WindowsPackagePlatform -PlatformKey $PlatformKey) {
        $SetupScript = @'
param(
    # Dependency target to fetch for this debug package.
    [ValidateSet("all", "lua", "vldb", "vldb-controller", "vldb-direct")]
    [string]$Target = "lua",
    # Optional database dependency preset used only by all/vldb targets.
    [ValidateSet("none", "vldb-controller", "vldb-direct", "host-callback")]
    [string]$Database = "none"
)

$ErrorActionPreference = "Stop"

# PackageRoot points at the extracted debug tool package root.
$PackageRoot = if ($PSScriptRoot) { $PSScriptRoot } else { (Get-Location).Path }

# RuntimeRoot receives Lua runtime packages and optional native helpers.
$RuntimeRoot = Join-Path $PackageRoot "runtime"

New-Item -ItemType Directory -Force -Path $RuntimeRoot | Out-Null
powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $PackageRoot "scripts\deps\fetch_deps.ps1") -Target $Target -Database $Database -RuntimeRoot $RuntimeRoot
$FetchExitCode = $LASTEXITCODE
if ($FetchExitCode -ne 0) {
    exit $FetchExitCode
}
Write-Host "Debug runtime dependencies are ready under $RuntimeRoot"
'@
        $SetupScript = $SetupScript.Replace("__LUASKILLS_RELEASE_TAG__", $ReleaseTag)
        [System.IO.File]::WriteAllText((Join-Path $PackageRoot "setup_runtime.ps1"), $SetupScript, [System.Text.UTF8Encoding]::new($false))

        $UpgradeScript = @'
@echo off
chcp 65001 >nul
setlocal
REM Target selects which dependency group to download.
set "TARGET=%~1"
if "%TARGET%"=="" set "TARGET=lua"

REM Database selects the optional database dependency preset.
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
'@
        [System.IO.File]::WriteAllText((Join-Path $PackageRoot "upgrade_deps.bat"), $UpgradeScript, [System.Text.UTF8Encoding]::new($false))

        $FetchShellWrapper = @'
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
'@
        [System.IO.File]::WriteAllText((Join-Path $PackageRoot "scripts\deps\fetch_deps.sh"), $FetchShellWrapper, [System.Text.UTF8Encoding]::new($false))

        $SetupShellWrapper = @'
#!/usr/bin/env bash
set -euo pipefail

PACKAGE_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TARGET="${1:-lua}"
DATABASE="${2:-none}"
RUNTIME_ROOT="${RUNTIME_ROOT:-$PACKAGE_ROOT/runtime}"

mkdir -p "$RUNTIME_ROOT"
RUNTIME_ROOT="$RUNTIME_ROOT" bash "$PACKAGE_ROOT/scripts/deps/fetch_deps.sh" "$TARGET" "$DATABASE"
echo "Debug runtime dependencies are ready under $RUNTIME_ROOT"
'@
        $SetupShellWrapper = $SetupShellWrapper.Replace("__LUASKILLS_RELEASE_TAG__", $ReleaseTag)
        [System.IO.File]::WriteAllText((Join-Path $PackageRoot "setup_runtime.sh"), $SetupShellWrapper, [System.Text.UTF8Encoding]::new($false))

        $UpgradeShellWrapper = @'
#!/usr/bin/env bash
set -euo pipefail

PACKAGE_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TARGET="${1:-lua}"
DATABASE="${2:-none}"

bash "$PACKAGE_ROOT/setup_runtime.sh" "$TARGET" "$DATABASE"
'@
        [System.IO.File]::WriteAllText((Join-Path $PackageRoot "upgrade_deps.sh"), $UpgradeShellWrapper, [System.Text.UTF8Encoding]::new($false))
        return
    }

    $SetupScript = @'
#!/usr/bin/env bash
set -euo pipefail

# PackageRoot points at the extracted debug tool package root.
# PackageRoot 指向解压后的调试工具包根目录。
PACKAGE_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Target selects which dependency group to fetch for this debug package.
# Target 选择当前调试包需要拉取的依赖分组。
TARGET="${1:-lua}"

# Database selects the optional database dependency preset used by all/vldb targets.
# Database 选择 all/vldb 目标使用的可选数据库依赖预设。
DATABASE="${2:-none}"

# RuntimeRoot receives Lua runtime packages and optional native helpers.
# RuntimeRoot 接收 Lua runtime packages 与可选原生辅助工具。
RUNTIME_ROOT="${RUNTIME_ROOT:-$PACKAGE_ROOT/runtime}"

mkdir -p "$RUNTIME_ROOT"
RUNTIME_ROOT="$RUNTIME_ROOT" bash "$PACKAGE_ROOT/scripts/deps/fetch_deps.sh" "$TARGET" "$DATABASE"
echo "Debug runtime dependencies are ready under $RUNTIME_ROOT"
'@
    $SetupScript = $SetupScript.Replace("__LUASKILLS_RELEASE_TAG__", $ReleaseTag)
    [System.IO.File]::WriteAllText((Join-Path $PackageRoot "setup_runtime.sh"), $SetupScript, [System.Text.UTF8Encoding]::new($false))

    $UpgradeScript = @'
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
'@
    [System.IO.File]::WriteAllText((Join-Path $PackageRoot "upgrade_deps.sh"), $UpgradeScript, [System.Text.UTF8Encoding]::new($false))
}

function Write-DebugLauncherScripts {
    <#
    .SYNOPSIS
    Write package-root launchers that auto-detect one skill under skills/.
    写入包根启动脚本，自动发现 skills/ 下的单个 skill。

    .PARAMETER PackageRoot
    Package root that receives launcher scripts.
    接收启动脚本的包根目录。

    .PARAMETER PlatformKey
    Target package platform used to choose launcher scripts.
    用于选择启动脚本的目标包平台。
    #>
    param(
        [string]$PackageRoot,
        [string]$PlatformKey
    )

    if (Test-WindowsPackagePlatform -PlatformKey $PlatformKey) {
        $LauncherScript = @'
param(
    # Debug command forwarded to luaskills-debug.
    [ValidateSet("sync", "inspect", "list-tools", "call")]
    [string]$Command = "inspect",
    # Optional explicit skill package directory.
    [string]$SkillPath = "",
    # Optional synchronized skill id used by run-only commands.
    [string]$SkillId = "",
    # Tool name used by the call command.
    [string]$Tool = "",
    # Inline JSON payload used by the call command.
    [string]$ArgsJson = "",
    # JSON file path used by the call command.
    [string]$ArgsFile = "",
    # Output rendering mode forwarded to luaskills-debug.
    [ValidateSet("pretty", "json", "content")]
    [string]$Output = "pretty",
    # Whether to enable the host_result bridge for the call command.
    [switch]$EnableHostResult
)

$ErrorActionPreference = "Stop"

function Resolve-DefaultSkillPath {
    <#
    .SYNOPSIS
    Resolve the only skill package placed under the package skills directory.

    .PARAMETER SkillsRoot
    Package-local skills directory.
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

function Show-DebugSkillGuidance {
    <#
    .SYNOPSIS
    Show one friendly guidance block when the package does not yet contain a debug target skill.
    当调试包中还没有目标 skill 时显示一段友好的引导说明。

    .PARAMETER SkillsRoot
    Package-local skills directory inspected by the launcher.
    启动脚本检查的包内 skills 目录。

    .PARAMETER Reason
    Specific resolution error that explains why auto-detection failed.
    解释自动发现失败原因的具体解析错误。
    #>
    param(
        [string]$SkillsRoot,
        [string]$Reason
    )

    Write-Host "No debug target skill is available yet." -ForegroundColor Yellow
    Write-Host $Reason -ForegroundColor Yellow
    Write-Host ""
    Write-Host "Put exactly one skill directory that contains skill.yaml under:"
    Write-Host "  $SkillsRoot"
    Write-Host ""
    Write-Host "or rerun with -SkillPath, for example:"
    Write-Host "  .\debug.ps1 -Command inspect -SkillPath D:\path\to\your-skill"
    Write-Host "  .\debug.ps1 -Command list-tools -SkillPath D:\path\to\your-skill"
    Write-Host "  .\debug.ps1 -Command call -SkillPath D:\path\to\your-skill -Tool ping -ArgsJson ""{}"""
}

# PackageRoot points at the extracted debug tool package root.
$PackageRoot = if ($PSScriptRoot) { $PSScriptRoot } else { (Get-Location).Path }

# RuntimeRoot is the package-local runtime root used for debug execution.
$RuntimeRoot = Join-Path $PackageRoot "runtime"

# SkillsRoot is where developers can drop one skill directory for quick debugging.
$SkillsRoot = Join-Path $PackageRoot "skills"

# BinaryPath points at the packaged release-mode debug binary.
$BinaryPath = Join-Path $PackageRoot "bin\luaskills-debug.exe"

if (-not (Test-Path -LiteralPath $BinaryPath)) {
    throw "luaskills-debug executable was not found: $BinaryPath"
}
if (-not (Test-Path -LiteralPath (Join-Path $RuntimeRoot "resources\luaskills-packages-manifest.json"))) {
    Write-Warning "Lua packages do not appear to be installed. Run '.\setup_runtime.ps1' first if the skill needs packaged Lua dependencies."
}

$EffectiveSkillPath = if ([string]::IsNullOrWhiteSpace($SkillPath) -and [string]::IsNullOrWhiteSpace($SkillId)) {
    try {
        Resolve-DefaultSkillPath -SkillsRoot $SkillsRoot
    } catch {
        Show-DebugSkillGuidance -SkillsRoot $SkillsRoot -Reason $_.Exception.Message
        exit 2
    }
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
'@
        [System.IO.File]::WriteAllText((Join-Path $PackageRoot "debug.ps1"), $LauncherScript, [System.Text.UTF8Encoding]::new($false))

        $BatchLauncher = @'
@echo off
chcp 65001 >nul
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0debug.ps1" %*
exit /b %ERRORLEVEL%
'@
        [System.IO.File]::WriteAllText((Join-Path $PackageRoot "debug.bat"), $BatchLauncher, [System.Text.UTF8Encoding]::new($false))

        $ShellLauncher = @'
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

if [ "${1:-}" = "sync" ] || [ "${1:-}" = "inspect" ] || [ "${1:-}" = "list-tools" ] || [ "${1:-}" = "call" ]; then
  FORWARDED_ARGS+=("-Command" "$1")
  shift
fi

while [ "$#" -gt 0 ]; do
  case "$1" in
    -SkillPath|--skill-path)
      FORWARDED_ARGS+=("-SkillPath" "$(to_windows_path "${2:?--skill-path requires a value}")")
      shift 2
      ;;
    -SkillId|--skill-id)
      FORWARDED_ARGS+=("-SkillId" "${2:?--skill-id requires a value}")
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
'@
        [System.IO.File]::WriteAllText((Join-Path $PackageRoot "debug.sh"), $ShellLauncher, [System.Text.UTF8Encoding]::new($false))
        return
    }

    $LauncherScript = @'
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

show_debug_skill_guidance() {
  # Show one friendly guidance block when the package does not yet contain a debug target skill.
  # 当调试包中还没有目标 skill 时显示一段友好的引导说明。
  local reason="$1"
  echo "No debug target skill is available yet." >&2
  echo "$reason" >&2
  echo >&2
  echo "Put exactly one skill directory that contains skill.yaml under:" >&2
  echo "  $SKILLS_ROOT" >&2
  echo >&2
  echo "or rerun with --skill-path, for example:" >&2
  echo "  ./debug.sh inspect --skill-path /path/to/your-skill" >&2
  echo "  ./debug.sh list-tools --skill-path /path/to/your-skill" >&2
  echo "  ./debug.sh call --skill-path /path/to/your-skill --tool ping --args-json \"{}\"" >&2
}

COMMAND="inspect"
if [ "${1:-}" = "inspect" ] || [ "${1:-}" = "list-tools" ] || [ "${1:-}" = "call" ]; then
  COMMAND="$1"
  shift
fi

SKILL_PATH=""
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
if [ ! -d "$RUNTIME_ROOT/lua_packages" ]; then
  echo "WARNING: Lua packages do not appear to be installed. Run './setup_runtime.sh' first if the skill needs packaged Lua dependencies." >&2
fi

if [ -z "$SKILL_PATH" ]; then
  if ! SKILL_PATH="$(resolve_default_skill_path 2>&1)"; then
    show_debug_skill_guidance "$SKILL_PATH"
    exit 2
  fi
else
  SKILL_PATH="$(cd "$SKILL_PATH" && pwd)"
fi

FORWARDED_ARGS=("$COMMAND" "--runtime-root" "$RUNTIME_ROOT" "--skill-path" "$SKILL_PATH")
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
'@
    [System.IO.File]::WriteAllText((Join-Path $PackageRoot "debug.sh"), $LauncherScript, [System.Text.UTF8Encoding]::new($false))
}

function Write-DebugPackageReadme {
    <#
    .SYNOPSIS
    Write a package-root README for the standalone debug tool package.
    为独立调试工具包写入包根 README。

    .PARAMETER PackageRoot
    Package root that receives README.md.
    接收 README.md 的包根目录。

    .PARAMETER PlatformKey
    Target package platform used in the README examples.
    README 示例中使用的目标包平台。
    #>
    param(
        [string]$PackageRoot,
        [string]$PlatformKey
    )

    $IsWindowsPackage = Test-WindowsPackagePlatform -PlatformKey $PlatformKey
    $ShellName = if ($IsWindowsPackage) { "powershell" } else { "bash" }
    $SetupCommand = if ($IsWindowsPackage) { ".\setup_runtime.ps1" } else { "./setup_runtime.sh" }
    $InspectCommand = if ($IsWindowsPackage) { ".\debug.ps1 inspect" } else { "./debug.sh inspect" }
    $ListCommand = if ($IsWindowsPackage) { ".\debug.ps1 list-tools" } else { "./debug.sh list-tools" }
    $CallCommand = if ($IsWindowsPackage) { '.\debug.ps1 call -Tool ping -ArgsJson "{}"' } else { './debug.sh call --tool ping --args-json "{}"' }
    $BinaryPath = if ($IsWindowsPackage) { "bin/luaskills-debug.exe" } else { "bin/luaskills-debug" }
    $FenceStart = "~~~$ShellName"
    $FenceEnd = "~~~"

    $ReadmeLines = @(
        '# LuaSkills debug tool package',
        '',
        ('This package is a standalone skill-debug workspace for {0}. It contains the release-mode luaskills-debug executable, a package-local runtime/ directory, a skills/ drop-in directory, and scripts that fetch the latest compatible Lua runtime packages.' -f $PlatformKey),
        '',
        '## Package Contents',
        '',
        ('- {0}: standalone debug executable.' -f $BinaryPath),
        '- runtime/: package-local runtime root used by debug commands.',
        '- skills/: place exactly one skill package directory here for quick debugging.',
        '- scripts/: platform-matching dependency fetch script.',
        '- setup_runtime.* / upgrade_deps.*: fetch Lua runtime packages into runtime/.',
        '- debug.*: convenience launcher that auto-detects one skill under skills/.',
        '',
        '## Quick Start',
        '',
        $FenceStart,
        $SetupCommand,
        $InspectCommand,
        $ListCommand,
        $CallCommand,
        $FenceEnd,
        '',
        'The default setup command fetches the lua target and stages runtime/lua_packages/, runtime/libs/, runtime/resources/, and runtime/licenses/ metadata. It does not download the extra LuaSkills FFI SDK.',
        '',
        'Use all only when you also need database helper binaries. The Lua setup path stays metadata-complete so packaged-runtime manifest validation remains consistent.',
        ''
    )

    ($ReadmeLines -join [Environment]::NewLine) | Set-Content -Path (Join-Path $PackageRoot "README.md") -Encoding UTF8
}

if ([string]::IsNullOrWhiteSpace($OutputDir)) {
    $OutputDir = "target\release-packages"
}
if ([string]::IsNullOrWhiteSpace($ReleaseTag)) {
    $CargoTomlText = Get-Content -Raw -LiteralPath "Cargo.toml"
    $CargoVersionMatch = [regex]::Match($CargoTomlText, '(?m)^version\s*=\s*"([^"]+)"')
    if (-not $CargoVersionMatch.Success) {
        throw "Unable to resolve fallback release tag from Cargo.toml."
    }
    $ReleaseTag = "v$($CargoVersionMatch.Groups[1].Value)"
}

if (-not $Platform) {
    $Arch = [System.Runtime.InteropServices.RuntimeInformation]::ProcessArchitecture.ToString().ToLowerInvariant()
    if ([System.Runtime.InteropServices.RuntimeInformation]::IsOSPlatform([System.Runtime.InteropServices.OSPlatform]::Windows)) {
        $Platform = "windows-$Arch"
    } elseif ([System.Runtime.InteropServices.RuntimeInformation]::IsOSPlatform([System.Runtime.InteropServices.OSPlatform]::OSX)) {
        $Platform = "macos-$Arch"
    } else {
        $Platform = "linux-$Arch"
    }
}

$PackageRoot = "target\debug-tool-package\luaskills-debug-tool"
if (Test-Path -LiteralPath $PackageRoot) {
    Remove-Item -LiteralPath $PackageRoot -Recurse -Force
}

Ensure-Dir (Join-Path $PackageRoot "bin")
Ensure-Dir (Join-Path $PackageRoot "runtime")
Ensure-Dir (Join-Path $PackageRoot "skills")
Ensure-Dir (Join-Path $PackageRoot "scripts\deps")
Ensure-Dir (Join-Path $PackageRoot "licenses")
Ensure-Dir $OutputDir

@'
# Put one skill package directory here

Place exactly one directory that contains skill.yaml under this skills/ folder.

Examples:
- skills/your-skill/skill.yaml
- skills/vulcan-file/skill.yaml

You can also leave this folder empty and pass -SkillPath or --skill-path to the debug launcher.
'@ | Set-Content -Path (Join-Path $PackageRoot "skills\README.md") -Encoding UTF8

$DebugBinaryName = Get-DebugBinaryName -PlatformKey $Platform
$DebugBinaryPath = Join-Path "target\release" $DebugBinaryName
if (-not (Test-Path -LiteralPath $DebugBinaryPath)) {
    throw "Missing release debug binary: $DebugBinaryPath. Run 'cargo build --release --bin luaskills-debug' first."
}

Copy-Item -Force -LiteralPath $DebugBinaryPath -Destination (Join-Path $PackageRoot "bin\$DebugBinaryName")
Copy-Item -Force -LiteralPath "LICENSE" -Destination (Join-Path $PackageRoot "licenses\LICENSE")
if (Test-WindowsPackagePlatform -PlatformKey $Platform) {
    Copy-Item -Force -LiteralPath "scripts\deps\fetch_deps.ps1" -Destination (Join-Path $PackageRoot "scripts\deps\fetch_deps.ps1")
} else {
    Copy-Item -Force -LiteralPath "scripts\deps\fetch_deps.sh" -Destination (Join-Path $PackageRoot "scripts\deps\fetch_deps.sh")
}

Write-DebugRuntimeSetupScripts -PackageRoot $PackageRoot -PlatformKey $Platform -ReleaseTag $ReleaseTag
Write-DebugLauncherScripts -PackageRoot $PackageRoot -PlatformKey $Platform
Write-DebugPackageReadme -PackageRoot $PackageRoot -PlatformKey $Platform

[ordered]@{
    schema_version = 1
    package_name = "luaskills-debug-tool-$Platform"
    platform = $Platform
    binary = "bin/$DebugBinaryName"
    runtime_root = "runtime"
    skills_dir = "skills"
    release_tag = $ReleaseTag
    default_fetch_target = "lua"
    fetch_targets = @("lua", "all", "vldb", "vldb-controller", "vldb-direct")
} | ConvertTo-Json -Depth 8 | Set-Content -Path (Join-Path $PackageRoot "debug-tool-manifest.json") -Encoding UTF8

$ArchiveName = "luaskills-debug-tool-$Platform.tar.gz"
$ResolvedOutput = (Resolve-Path -LiteralPath $OutputDir).Path
New-TarFromDirectory -SourceDir $PackageRoot -ArchivePath (Join-Path $ResolvedOutput $ArchiveName)

Write-Host "Debug tool package created: $(Join-Path $OutputDir $ArchiveName)"
