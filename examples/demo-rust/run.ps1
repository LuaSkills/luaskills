param(
    # Dependency target to fetch before running the demo.
    # 运行 demo 前需要拉取的依赖目标。
    [ValidateSet("none", "all", "lua", "vldb")]
    [string]$Fetch = "none"
)

$ErrorActionPreference = "Stop"

function Resolve-ProjectRoot {
    <#
    .SYNOPSIS
    Resolve the repository root from script metadata or the caller location.
    从脚本元数据或调用方位置解析仓库根目录。

    .PARAMETER ScriptDirectory
    Directory that contains the current script when PowerShell exposes it.
    PowerShell 可用时当前脚本所在的目录。

    .OUTPUTS
    Repository root path that contains Cargo.toml and scripts.
    包含 Cargo.toml 与 scripts 目录的仓库根路径。
    #>
    param([string]$ScriptDirectory)

    $Candidates = @()
    if ($ScriptDirectory) {
        $Candidates += $ScriptDirectory
    }
    $Candidates += (Get-Location).Path

    foreach ($Candidate in $Candidates) {
        $Current = $Candidate
        while ($Current) {
            if ((Test-Path -LiteralPath (Join-Path $Current "Cargo.toml")) -and (Test-Path -LiteralPath (Join-Path $Current "scripts"))) {
                return $Current
            }
            $Parent = Split-Path -Parent $Current
            if (-not $Parent -or $Parent -eq $Current) {
                break
            }
            $Current = $Parent
        }
    }

    throw "Unable to resolve project root from script or current directory."
}

# ScriptDir points at the current script directory when PowerShell exposes it.
# ScriptDir 在 PowerShell 提供脚本路径时指向当前脚本目录。
$ScriptDir = if ($PSScriptRoot) { $PSScriptRoot } elseif ($PSCommandPath) { Split-Path -Parent $PSCommandPath } elseif ($MyInvocation.MyCommand.Path) { Split-Path -Parent $MyInvocation.MyCommand.Path } else { "" }

# ProjectRoot points at the repository root regardless of the caller location.
# ProjectRoot 指向仓库根目录，避免调用方当前位置影响路径解析。
$ProjectRoot = Resolve-ProjectRoot -ScriptDirectory $ScriptDir

# RuntimeRoot is the shared demo runtime root.
# RuntimeRoot 是共享 demo 运行根目录。
$RuntimeRoot = Join-Path $ProjectRoot "examples\ffi\standard_runtime\runtime_root"

if ($Fetch -ne "none") {
    & (Join-Path $ProjectRoot "scripts\deps\fetch_deps.ps1") -Target $Fetch -RuntimeRoot $RuntimeRoot
}

if (Test-Path -LiteralPath (Join-Path $RuntimeRoot "resources\runtime-env.ps1")) {
    . (Join-Path $RuntimeRoot "resources\runtime-env.ps1")
}

cargo run --manifest-path (Join-Path $ScriptDir "Cargo.toml")
