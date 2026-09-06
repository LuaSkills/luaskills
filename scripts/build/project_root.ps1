function Resolve-ProjectRoot {
    <#
    .SYNOPSIS
    Resolve the repository root from script metadata or the caller location.
    从脚本元数据或调用方位置解析仓库根目录。

    .PARAMETER ScriptDirectory
    Directory that contains the importing build script when PowerShell exposes it.
    PowerShell 可用时包含导入方构建脚本的目录。

    .OUTPUTS
    Repository root path that contains Cargo.toml and scripts.
    包含 Cargo.toml 与 scripts 目录的仓库根路径。
    #>
    param([string]$ScriptDirectory)

    # Ordered candidate roots preserving script-local resolution before caller fallback.
    # 保持脚本局部解析优先于调用方回退的有序候选根。
    $RootCandidates = @()
    if ($ScriptDirectory) {
        $RootCandidates += $ScriptDirectory
    }
    $RootCandidates += (Get-Location).Path

    foreach ($RootCandidate in $RootCandidates) {
        # Current ancestor inspected for the unique repository marker pair.
        # 为唯一仓库标记对而检查的当前祖先目录。
        $RootCursor = $RootCandidate
        while ($RootCursor) {
            if ((Test-Path -LiteralPath (Join-Path $RootCursor "Cargo.toml")) -and (Test-Path -LiteralPath (Join-Path $RootCursor "scripts"))) {
                return $RootCursor
            }
            # Parent ancestor used by the next bounded upward traversal step.
            # 下一次有界向上遍历步骤使用的父祖先目录。
            $RootParent = Split-Path -Parent $RootCursor
            if (-not $RootParent -or $RootParent -eq $RootCursor) {
                break
            }
            $RootCursor = $RootParent
        }
    }

    throw "Unable to resolve project root from script or current directory."
}
