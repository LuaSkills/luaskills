param(
    [string]$Mode = "ffi",
    [string]$Platform = "",
    [string]$OutputDir = "target\release-packages",
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

if ($Mode -ne "ffi" -and $Mode -ne "rust") {
    throw "Mode must be 'ffi' or 'rust'."
}

if ([string]::IsNullOrWhiteSpace($ReleaseTag)) {
    $CargoTomlText = Get-Content -Raw -LiteralPath "Cargo.toml"
    $CargoVersionMatch = [regex]::Match($CargoTomlText, '(?m)^version\s*=\s*"([^"]+)"')
    if (-not $CargoVersionMatch.Success) {
        throw "Unable to resolve fallback release tag from Cargo.toml."
    }
    $ReleaseTag = "v$($CargoVersionMatch.Groups[1].Value)"
}

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

function Remove-NonPlatformDemoScripts {
    <#
    .SYNOPSIS
    Remove demo launchers that do not match the target package platform.
    移除与目标包平台不匹配的 demo 启动脚本。

    .PARAMETER PackageRoot
    Package root that contains generated launchers.
    包含已生成启动脚本的包根目录。

    .PARAMETER Platform
    Target package platform used to choose the launcher family.
    用于选择启动脚本类型的目标包平台。
    #>
    param(
        [string]$PackageRoot,
        [string]$Platform
    )

    if (Test-WindowsPackagePlatform -PlatformKey $Platform) {
        Remove-Item -Force -LiteralPath (Join-Path $PackageRoot "run.sh") -ErrorAction SilentlyContinue
        Remove-Item -Force -LiteralPath (Join-Path $PackageRoot "upgrade_deps.sh") -ErrorAction SilentlyContinue
    } else {
        Remove-Item -Force -LiteralPath (Join-Path $PackageRoot "run.ps1") -ErrorAction SilentlyContinue
        Remove-Item -Force -LiteralPath (Join-Path $PackageRoot "upgrade_deps.bat") -ErrorAction SilentlyContinue
    }
}

function Write-PackagedDemoScripts {
    <#
    .SYNOPSIS
    Write run scripts that work from the packaged demo root.
    写入可从发布 demo 包根目录直接运行的脚本。

    .PARAMETER Mode
    Demo mode to generate scripts for.
    需要生成脚本的 demo 模式。

    .PARAMETER PackageRoot
    Package root that receives run scripts.
    接收运行脚本的包根目录。

    .PARAMETER Platform
    Target package platform used to keep only one launcher family.
    用于仅保留一类启动脚本的目标包平台。
    #>
    param(
        [string]$Mode,
        [string]$PackageRoot,
        [string]$Platform
    )

    if ($Mode -eq "rust") {
        @'
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
'@ | Set-Content -Path (Join-Path $PackageRoot "run.ps1") -Encoding UTF8

        @'
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
'@ | Set-Content -Path (Join-Path $PackageRoot "run.sh") -Encoding UTF8
        Write-PackagedDependencyUpgradeScripts -PackageRoot $PackageRoot
        Remove-NonPlatformDemoScripts -PackageRoot $PackageRoot -Platform $Platform
        return
    }

    @'
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
'@ | Set-Content -Path (Join-Path $PackageRoot "run.ps1") -Encoding UTF8

    @'
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
'@ | Set-Content -Path (Join-Path $PackageRoot "run.sh") -Encoding UTF8
    Write-PackagedDependencyUpgradeScripts -PackageRoot $PackageRoot
    Remove-NonPlatformDemoScripts -PackageRoot $PackageRoot -Platform $Platform
}

function Copy-FfiExampleSources {
    <#
    .SYNOPSIS
    Copy every FFI demo source directory into the packaged demo.
    将所有 FFI demo 源码目录复制到发布 demo 包中。

    .PARAMETER PackageRoot
    Package root that receives examples/ffi.
    接收 examples/ffi 的发布包根目录。
    #>
    param([string]$PackageRoot)

    $ExamplesRoot = Join-Path $PackageRoot "examples"
    $FfiExamplesRoot = Join-Path $ExamplesRoot "ffi"
    Ensure-Dir $ExamplesRoot
    if (Test-Path -LiteralPath $FfiExamplesRoot) {
        Remove-Item -LiteralPath $FfiExamplesRoot -Recurse -Force
    }
    Copy-Item -Recurse -Force -Path "examples\ffi" -Destination $ExamplesRoot
    # Generated runtime caches are useful after local smoke tests but should not ship in demo archives.
    # 本地烟测后的运行缓存有调试价值，但不应该进入 demo 发布包。
    Get-ChildItem -Recurse -Directory -Path $FfiExamplesRoot -Filter "__pycache__" -ErrorAction SilentlyContinue | Remove-Item -Recurse -Force
    Get-ChildItem -Recurse -Directory -Path $FfiExamplesRoot -Filter "node_modules" -ErrorAction SilentlyContinue | Remove-Item -Recurse -Force
    Get-ChildItem -Recurse -File -Path $FfiExamplesRoot -Include "*.pyc","*.pyo" -ErrorAction SilentlyContinue | Remove-Item -Force
    Get-ChildItem -Recurse -File -Path $FfiExamplesRoot -Include "*.zip","*.exe","*.db" -ErrorAction SilentlyContinue | Remove-Item -Force
    Get-ChildItem -Recurse -Directory -Path $FfiExamplesRoot -ErrorAction SilentlyContinue |
        Where-Object {
            $_.FullName -match '[\\/]runtime_root[\\/]temp[\\/]downloads$' -or
            $_.FullName -match '[\\/]runtime_root[\\/]state[\\/]install_tmp$' -or
            $_.FullName -match '[\\/]runtime_root[\\/]state[\\/]installs$' -or
            $_.FullName -match '[\\/]runtime_root[\\/]dependencies[\\/]tools$'
        } |
        ForEach-Object {
            Get-ChildItem -Force -LiteralPath $_.FullName -ErrorAction SilentlyContinue |
                Where-Object { $_.Name -ne ".gitkeep" } |
                Remove-Item -Recurse -Force
        }
}

function Write-PackagedDependencyUpgradeScripts {
    <#
    .SYNOPSIS
    Write standalone dependency upgrade launchers for packaged demos.
    写入发布 demo 包专用的独立依赖升级入口。

    .PARAMETER PackageRoot
    Package root that receives upgrade scripts.
    接收升级脚本的发布包根目录。
    #>
    param([string]$PackageRoot)

    $WindowsUpgradeScript = @'
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
  if /I "%TARGET%"=="all" powershell -NoProfile -ExecutionPolicy Bypass -File "%PACKAGE_ROOT%scripts\ffi\fetch_ffi.ps1" -RuntimeRoot "%RUNTIME_ROOT%" -LuaSkillsVersion "__LUASKILLS_RELEASE_TAG__"
  if /I "%TARGET%"=="ffi" powershell -NoProfile -ExecutionPolicy Bypass -File "%PACKAGE_ROOT%scripts\ffi\fetch_ffi.ps1" -RuntimeRoot "%RUNTIME_ROOT%" -LuaSkillsVersion "__LUASKILLS_RELEASE_TAG__"
)
if errorlevel 1 (
  echo Failed to upgrade FFI dependencies.
  pause
  exit /b 1
)
echo Dependencies are ready.
pause
'@
    $WindowsUpgradeScript = $WindowsUpgradeScript.Replace("__LUASKILLS_RELEASE_TAG__", $ReleaseTag)
    [System.IO.File]::WriteAllText((Join-Path $PackageRoot "upgrade_deps.bat"), $WindowsUpgradeScript, [System.Text.UTF8Encoding]::new($false))

    @'
#!/usr/bin/env bash
set -euo pipefail

# PackageRoot points at the extracted demo package root.
# PackageRoot 指向解压后的 demo 包根目录。
PACKAGE_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Target selects which dependency group to download.
# Target 选择要下载的依赖分组。
TARGET="${1:-all}"

# RuntimeRoot is the packaged runtime root that receives dependencies.
# RuntimeRoot 是接收依赖的包内运行根目录。
RUNTIME_ROOT="${RUNTIME_ROOT:-$PACKAGE_ROOT/runtime}"

if [ "$TARGET" != "ffi" ]; then
  RUNTIME_ROOT="$RUNTIME_ROOT" bash "$PACKAGE_ROOT/scripts/deps/fetch_deps.sh" "$TARGET"
fi
if [ -f "$PACKAGE_ROOT/scripts/ffi/fetch_ffi.sh" ] && { [ "$TARGET" = "all" ] || [ "$TARGET" = "ffi" ]; }; then
  LUASKILLS_VERSION="${LUASKILLS_VERSION:-__LUASKILLS_RELEASE_TAG__}" RUNTIME_ROOT="$RUNTIME_ROOT" bash "$PACKAGE_ROOT/scripts/ffi/fetch_ffi.sh"
fi
'@ | Set-Content -Path (Join-Path $PackageRoot "upgrade_deps.sh") -Encoding UTF8
    (Get-Content -Raw -Path (Join-Path $PackageRoot "upgrade_deps.sh")).Replace("__LUASKILLS_RELEASE_TAG__", $ReleaseTag) |
        Set-Content -Path (Join-Path $PackageRoot "upgrade_deps.sh") -Encoding UTF8
}

function Write-PackagedDemoReadme {
    <#
    .SYNOPSIS
    Write a package-root README that matches the generated demo artifact layout.
    写入匹配发布 demo 包目录结构的包根 README。

    .PARAMETER Mode
    Demo mode represented by the package.
    当前发布包对应的 demo 模式。

    .PARAMETER Platform
    Target package platform used to describe platform-specific commands.
    用于描述平台专属命令的目标平台。

    .PARAMETER PackageRoot
    Package root that receives README.md.
    接收 README.md 的发布包根目录。

    .PARAMETER ReleaseTag
    Git release tag used by fetch scripts and Rust demo dependency.
    拉取脚本与 Rust demo 依赖使用的 Git 发布标签。
    #>
    param(
        [string]$Mode,
        [string]$Platform,
        [string]$PackageRoot,
        [string]$ReleaseTag
    )

    $ShellName = if (Test-WindowsPackagePlatform -PlatformKey $Platform) { "powershell" } else { "bash" }
    $RunCommand = if (Test-WindowsPackagePlatform -PlatformKey $Platform) { ".\run.ps1" } else { "./run.sh" }
    $FetchAllCommand = if (Test-WindowsPackagePlatform -PlatformKey $Platform) { ".\upgrade_deps.bat" } else { "./upgrade_deps.sh" }
    $FetchLuaCommand = if (Test-WindowsPackagePlatform -PlatformKey $Platform) { ".\upgrade_deps.bat lua" } else { "./upgrade_deps.sh lua" }
    $FetchVldbCommand = if (Test-WindowsPackagePlatform -PlatformKey $Platform) { ".\upgrade_deps.bat vldb" } else { "./upgrade_deps.sh vldb" }
    $PackageFile = "luaskills-demo-$Mode-$Platform.tar.gz"
    $ModeDescription = if ($Mode -eq "ffi") {
        "This FFI demo runs examples/ffi/python/demo.py through the packaged dynamic library and also includes the C, Go, Python, TypeScript, standard runtime, install smoke test, and host provider examples."
    } else {
        "This Rust demo depends on the luaskills crate at tag $ReleaseTag through the packaged Cargo.toml and is intended for validating the non-FFI integration path."
    }

    $ReadmeLines = @(
        "# LuaSkills $Mode demo package",
        "",
        "This README describes the extracted $PackageFile package. Paths and commands are package-root based and do not require the source repository layout.",
        "",
        "## Package Contents",
        "",
        "- runtime/: default demo runtime root; categorized fetch scripts install runtime packages, and FFI packages can additionally install luaskills-ffi-sdk-$Platform.tar.gz.",
        "- scripts/: dependency fetch scripts for the current platform only.",
        "- licenses/: project and bundled component license files.",
        "- demo-manifest.json: package mode, platform, runtime root, and supported fetch targets."
    )

    if ($Mode -eq "ffi") {
        $ReadmeLines += "- examples/ffi/: complete FFI examples for C, Go, Python, TypeScript, and the shared runtime fixture."
    }

    $ReadmeLines += @(
        "",
        $ModeDescription,
        "",
        "## Run",
        "",
        ('```' + $ShellName),
        $RunCommand,
        '```',
        "",
        "The run script only executes the demo and does not download dependencies automatically. Run this first when dependencies are missing or need to be refreshed:",
        "",
        ('```' + $ShellName),
        $FetchAllCommand,
        '```',
        "",
        "You can also fetch subsets on demand:",
        "",
        ('```' + $ShellName),
        $FetchLuaCommand,
        $FetchVldbCommand,
        '```',
        "",
        "Windows packages include run.ps1, upgrade_deps.bat, and scripts/deps/fetch_deps.ps1. FFI packages also include scripts/ffi/fetch_ffi.ps1. Linux and macOS packages use the matching .sh scripts."
    )

    ($ReadmeLines -join [Environment]::NewLine) | Set-Content -Path (Join-Path $PackageRoot "README.md") -Encoding UTF8
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

$PackageRoot = "target\demo-package\luaskills-demo-$Mode"
if (Test-Path -LiteralPath $PackageRoot) {
    Remove-Item -LiteralPath $PackageRoot -Recurse -Force
}

Ensure-Dir $PackageRoot
Ensure-Dir (Join-Path $PackageRoot "scripts\deps")
Ensure-Dir (Join-Path $PackageRoot "runtime")
Ensure-Dir (Join-Path $PackageRoot "licenses")
Ensure-Dir $OutputDir

Copy-Item -Recurse -Force -Path "examples\demo-$Mode\*" -Destination $PackageRoot -ErrorAction SilentlyContinue

# Build caches are useful locally but must not be shipped in the demo source package.
# 构建缓存仅对本地开发有用，不能进入 demo 源码包产物。
Remove-Item -Recurse -Force -LiteralPath (Join-Path $PackageRoot "target") -ErrorAction SilentlyContinue

Copy-Item -Recurse -Force -Path "examples\ffi\standard_runtime\runtime_root\*" -Destination (Join-Path $PackageRoot "runtime") -ErrorAction SilentlyContinue
if (Test-WindowsPackagePlatform -PlatformKey $Platform) {
    Copy-Item -Force -LiteralPath "scripts\deps\fetch_deps.ps1" -Destination (Join-Path $PackageRoot "scripts\deps\fetch_deps.ps1")
} else {
    Copy-Item -Force -LiteralPath "scripts\deps\fetch_deps.sh" -Destination (Join-Path $PackageRoot "scripts\deps\fetch_deps.sh")
}
Copy-Item -Force -LiteralPath "LICENSE" -Destination (Join-Path $PackageRoot "licenses\LICENSE")

if ($Mode -eq "ffi") {
    Ensure-Dir (Join-Path $PackageRoot "scripts\ffi")
    if (Test-WindowsPackagePlatform -PlatformKey $Platform) {
        Copy-Item -Force -LiteralPath "scripts\ffi\fetch_ffi.ps1" -Destination (Join-Path $PackageRoot "scripts\ffi\fetch_ffi.ps1")
    } else {
        Copy-Item -Force -LiteralPath "scripts\ffi\fetch_ffi.sh" -Destination (Join-Path $PackageRoot "scripts\ffi\fetch_ffi.sh")
    }
    Ensure-Dir (Join-Path $PackageRoot "include")
    Ensure-Dir (Join-Path $PackageRoot "lib")
    Copy-Item -Force -Path "include\*.h" -Destination (Join-Path $PackageRoot "include")
    Copy-FfiExampleSources -PackageRoot $PackageRoot
    Get-ChildItem -File -Path "target\release\*" -Include "*.dll","*.lib","*.so","*.dylib","*.a" -ErrorAction SilentlyContinue | ForEach-Object {
        Copy-Item -Force -LiteralPath $_.FullName -Destination (Join-Path $PackageRoot "lib")
    }
} else {
    $CargoTomlPath = Join-Path $PackageRoot "Cargo.toml"
    if (Test-Path -LiteralPath $CargoTomlPath) {
        (Get-Content -Raw -Path $CargoTomlPath).Replace('luaskills = { path = "../.." }', ('luaskills = {{ git = "https://github.com/LuaSkills/luaskills.git", tag = "{0}" }}' -f $ReleaseTag)) |
            Set-Content -Path $CargoTomlPath -Encoding UTF8
    }
}

Write-PackagedDemoScripts -Mode $Mode -PackageRoot $PackageRoot -Platform $Platform
Write-PackagedDemoReadme -Mode $Mode -Platform $Platform -PackageRoot $PackageRoot -ReleaseTag $ReleaseTag

[ordered]@{
    schema_version = 1
    package_name = "luaskills-demo-$Mode-$Platform"
    platform = $Platform
    mode = $Mode
    runtime_root = "runtime"
    release_tag = $ReleaseTag
    fetch_targets = @("all", "lua", "vldb")
} | ConvertTo-Json -Depth 8 | Set-Content -Path (Join-Path $PackageRoot "demo-manifest.json") -Encoding UTF8

$ArchiveName = "luaskills-demo-$Mode-$Platform.tar.gz"
$ResolvedOutput = (Resolve-Path -LiteralPath $OutputDir).Path
New-TarFromDirectory -SourceDir $PackageRoot -ArchivePath (Join-Path $ResolvedOutput $ArchiveName)

Write-Host "Demo package created: $(Join-Path $OutputDir $ArchiveName)"
