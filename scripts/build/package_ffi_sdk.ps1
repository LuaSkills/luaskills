param(
    # Target platform key used in archive and manifest names.
    # 用于归档文件与清单文件命名的目标平台标识。
    [string]$Platform = "",
    # Output directory that receives the final archive.
    # 接收最终压缩包的输出目录。
    [string]$OutputDir = "target\release-packages"
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

if ([string]::IsNullOrWhiteSpace($OutputDir)) {
    $OutputDir = "target\release-packages"
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

$PackageRoot = "target\ffi-sdk-package\luaskills-ffi-sdk"
if (Test-Path -LiteralPath $PackageRoot) {
    Remove-Item -LiteralPath $PackageRoot -Recurse -Force
}

Ensure-Dir (Join-Path $PackageRoot "include")
Ensure-Dir (Join-Path $PackageRoot "lib")
Ensure-Dir (Join-Path $PackageRoot "licenses")
Ensure-Dir $OutputDir

Copy-Item -Force -Path "include\*.h" -Destination (Join-Path $PackageRoot "include")
Get-ChildItem -File -Path "target\release\*" -Include "*.dll","*.lib","*.so","*.dylib","*.a" -ErrorAction SilentlyContinue | ForEach-Object {
    Copy-Item -Force -LiteralPath $_.FullName -Destination (Join-Path $PackageRoot "lib")
}
Copy-Item -Force -LiteralPath "LICENSE" -Destination (Join-Path $PackageRoot "licenses\LICENSE")

[ordered]@{
    schema_version = 1
    package_name = "luaskills-ffi-sdk-$Platform"
    platform = $Platform
    headers = @("include/luaskills_ffi.h", "include/luaskills_json_ffi.h")
    library_dir = "lib"
} | ConvertTo-Json -Depth 8 | Set-Content -Path (Join-Path $PackageRoot "ffi-sdk-manifest.json") -Encoding UTF8

$ArchiveName = "luaskills-ffi-sdk-$Platform.tar.gz"
$ResolvedOutput = (Resolve-Path -LiteralPath $OutputDir).Path
New-TarFromDirectory -SourceDir $PackageRoot -ArchivePath (Join-Path $ResolvedOutput $ArchiveName)

Write-Host "FFI SDK package created: $(Join-Path $OutputDir $ArchiveName)"
