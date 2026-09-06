function New-TarFromDirectory {
    <#
    .SYNOPSIS
    Archive top-level children and fail when the native tar command fails.
    归档一级子项，并在原生 tar 命令失败时终止。

    .PARAMETER SourceDir
    Existing source directory whose top-level children are archived.
    需要归档其一级子项的已存在源目录。

    .PARAMETER ArchivePath
    Final archive path created by tar.
    由 tar 创建的最终归档路径。

    .OUTPUTS
    No value is returned; failures are raised as terminating errors.
    不返回值；失败会作为终止错误抛出。
    #>
    param(
        [string]$SourceDir,
        [string]$ArchivePath
    )

    # Members is the stable top-level archive member list without a leading dot entry.
    # Members 是不含前导点目录项的稳定一级归档成员列表。
    $Members = @(Get-ChildItem -Force -LiteralPath $SourceDir | ForEach-Object { $_.Name })
    if ($Members.Count -eq 0) {
        throw "Cannot create archive from empty directory: $SourceDir"
    }

    Push-Location $SourceDir
    try {
        tar -czf $ArchivePath @Members
        # TarExitCode captures native-command failure because Windows PowerShell does not promote it automatically.
        # TarExitCode 捕获原生命令失败，因为 Windows PowerShell 不会自动提升该退出码。
        $TarExitCode = $LASTEXITCODE
        if ($TarExitCode -ne 0) {
            throw "Failed to create archive '$ArchivePath' (exit $TarExitCode)"
        }
    } finally {
        Pop-Location
    }
}
