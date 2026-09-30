param(
    # Exact supported native platform key; no platform identity is guessed.
    # 精确受支持原生平台键；不猜测平台身份。
    [Parameter(Mandatory = $true)][string]$Platform,
    # Directory receiving immutable candidate archives.
    # 接收不可变候选归档的目录。
    [string]$OutputDir = "target/release-packages",
    # Full frozen Git commit validated against a clean checkout.
    # 对照干净检出验证的完整冻结 Git 提交。
    [Parameter(Mandatory = $true)][string]$SourceCommit,
    # Current Cargo package version, without a tag prefix.
    # 当前 Cargo 包版本，不含标签前缀。
    [Parameter(Mandatory = $true)][string]$Version,
    # Cargo build --message-format=json output from this locked build.
    # 本次锁定构建的 Cargo build --message-format=json 输出。
    [Parameter(Mandatory = $true)][string]$BuildLog,
    # Exact cargo metadata --locked --format-version=1 output.
    # 精确 cargo metadata --locked --format-version=1 输出。
    [Parameter(Mandatory = $true)][string]$Metadata,
    # Actual UTF-8 cargo --version --verbose output from the release build's Cargo executable.
    # 发布构建 Cargo 可执行文件的实际 UTF-8 cargo --version --verbose 输出。
    [Parameter(Mandatory = $true)][string]$CargoVersion,
    # Validate all inputs and print the manifest without creating candidate files.
    # 验证全部输入并打印清单，不创建候选文件。
    [switch]$DryRun
)

# Native command failures are explicitly checked on both PowerShell 5.1 and pwsh.
# 在 PowerShell 5.1 及 pwsh 上显式检查原生命令失败。
$ErrorActionPreference = "Stop"
# RepositoryRoot comes from this script's known location.
# RepositoryRoot 来自此脚本的已知位置。
$RepositoryRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot "../.."))
# Arguments delegate the single candidate schema to the shared cross-platform implementation.
# 参数将唯一候选结构委托给共享跨平台实现。
$CandidateArguments = @(
    (Join-Path $RepositoryRoot "scripts/release/candidate.py"), "package",
    "--root", $RepositoryRoot, "--platform", $Platform, "--output", $OutputDir,
    "--source-commit", $SourceCommit, "--version", $Version,
    "--build-log", $BuildLog, "--metadata", $Metadata, "--cargo-version", $CargoVersion
)
if ($DryRun) {
    $CandidateArguments += "--dry-run"
}
& python @CandidateArguments
if ($LASTEXITCODE -ne 0) {
    throw "FFI candidate validation failed (exit $LASTEXITCODE)"
}
