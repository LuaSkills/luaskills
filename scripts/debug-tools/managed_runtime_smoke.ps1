param(
    # RuntimeRoot stores an isolated LuaSkills runtime root for this smoke run.
    # RuntimeRoot 保存本次冒烟运行使用的隔离 LuaSkills 运行时根目录。
    [string]$RuntimeRoot = "",
    # SkipFetch allows reusing an already prepared runtime root during local iteration.
    # SkipFetch 允许本地迭代时复用已经准备好的运行时根目录。
    [switch]$SkipFetch,
    # KeepRuntimeRoot keeps the isolated runtime root after a successful smoke run.
    # KeepRuntimeRoot 在冒烟运行成功后保留隔离运行时根目录。
    [switch]$KeepRuntimeRoot
)

$ErrorActionPreference = "Stop"

# CurrentDirectory stores the caller's working directory for repository-root detection.
# CurrentDirectory 保存调用方工作目录，用于识别仓库根目录。
$CurrentDirectory = (Resolve-Path -LiteralPath ".").Path

# RepoRoot stores the repository root inferred from cwd first and script location second.
# RepoRoot 优先根据当前目录，其次根据脚本位置推导仓库根目录。
$RepoRoot = $null
if (Test-Path -LiteralPath (Join-Path $CurrentDirectory "Cargo.toml")) {
    $RepoRoot = $CurrentDirectory
} else {
    # ScriptPath stores the current script file path across Windows PowerShell and PowerShell Core.
    # ScriptPath 保存兼容 Windows PowerShell 与 PowerShell Core 的当前脚本路径。
    $ScriptPath = $PSCommandPath
    if ([string]::IsNullOrWhiteSpace($ScriptPath)) {
        $ScriptPath = $MyInvocation.MyCommand.Path
    }
    # ScriptDir stores the current script directory.
    # ScriptDir 保存当前脚本目录。
    $ScriptDir = Split-Path -Parent $ScriptPath
    $RepoRoot = Convert-Path -LiteralPath (Join-Path $ScriptDir "..\..")
}

# DefaultRuntimeRoot stores one per-run runtime root under target when the caller omits RuntimeRoot.
# DefaultRuntimeRoot 保存调用方未传 RuntimeRoot 时 target 下的单次运行目录。
$DefaultRuntimeRoot = Join-Path $RepoRoot ("target\managed-runtime-smoke\run-" + [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds())
if ([string]::IsNullOrWhiteSpace($RuntimeRoot)) {
    $RuntimeRoot = $DefaultRuntimeRoot
}

# RuntimeRootPath stores the absolute runtime root path used by fetch and debug calls.
# RuntimeRootPath 保存拉取与 debug 调用共同使用的绝对运行时根目录。
$RuntimeRootPath = ""
if ([System.IO.Path]::IsPathRooted($RuntimeRoot)) {
    $RuntimeRootPath = [System.IO.Path]::GetFullPath($RuntimeRoot)
} else {
    $RuntimeRootPath = [System.IO.Path]::GetFullPath((Join-Path $RepoRoot $RuntimeRoot))
}

# SkillPath stores the example skill used for end-to-end managed runtime verification.
# SkillPath 保存用于端到端受管运行时验证的示例 skill。
$SkillPath = Join-Path $RepoRoot "examples\managed_runtime\managed-child-runtime-debug"

# FetchScript stores the managed runtime dependency fetcher.
# FetchScript 保存受管运行时依赖拉取脚本。
$FetchScript = Join-Path $RepoRoot "scripts\deps\fetch_managed_runtimes.ps1"

# LayoutCheckScript stores the managed runtime layout validator.
# LayoutCheckScript 保存受管运行时布局校验器。
$LayoutCheckScript = Join-Path $RepoRoot "scripts\debug-tools\managed_runtime_layout_check.py"

function Invoke-CheckedProcess {
    <#
    .SYNOPSIS
    Run one external process and fail with captured output when it exits non-zero.
    运行一个外部进程，并在非零退出时携带捕获输出失败。

    .PARAMETER FilePath
    Executable path to launch.
    需要启动的可执行文件路径。

    .PARAMETER ArgumentList
    Argument array passed to the executable.
    传递给可执行文件的参数数组。

    .PARAMETER WorkingDirectory
    Working directory used by the process.
    进程使用的工作目录。

    .OUTPUTS
    Captured stdout text.
    捕获到的标准输出文本。
    #>
    param(
        [string]$FilePath,
        [string[]]$ArgumentList,
        [string]$WorkingDirectory
    )

    # StartInfo stores process launch settings with redirected output.
    # StartInfo 保存带输出重定向的进程启动设置。
    $StartInfo = New-Object System.Diagnostics.ProcessStartInfo
    $StartInfo.FileName = $FilePath
    $StartInfo.Arguments = ($ArgumentList | ForEach-Object { ConvertTo-ProcessArgument $_ }) -join " "
    $StartInfo.WorkingDirectory = $WorkingDirectory
    $StartInfo.RedirectStandardOutput = $true
    $StartInfo.RedirectStandardError = $true
    $StartInfo.UseShellExecute = $false

    # Process stores the launched child process.
    # Process 保存已启动的子进程。
    $Process = New-Object System.Diagnostics.Process
    $Process.StartInfo = $StartInfo
    [void]$Process.Start()
    $Stdout = $Process.StandardOutput.ReadToEnd()
    $Stderr = $Process.StandardError.ReadToEnd()
    $Process.WaitForExit()

    if ($Process.ExitCode -ne 0) {
        throw "Process failed: $FilePath $($ArgumentList -join ' ')`nstdout:`n$Stdout`nstderr:`n$Stderr"
    }

    if (-not [string]::IsNullOrWhiteSpace($Stderr)) {
        Write-Host $Stderr.Trim()
    }
    return $Stdout
}

function ConvertTo-ProcessArgument {
    <#
    .SYNOPSIS
    Quote one process argument for Windows PowerShell 5.1 ProcessStartInfo.Arguments.
    为 Windows PowerShell 5.1 的 ProcessStartInfo.Arguments 引用单个进程参数。

    .PARAMETER Value
    Raw argument value.
    原始参数值。

    .OUTPUTS
    Quoted argument text.
    引用后的参数文本。
    #>
    param([string]$Value)

    if ($Value -notmatch '[\s"]') {
        return $Value
    }
    return '"' + ($Value -replace '"', '\"') + '"'
}

try {
    if (-not $SkipFetch) {
        Write-Host "Fetching managed runtimes into $RuntimeRootPath"
        & powershell -NoProfile -ExecutionPolicy Bypass -File $FetchScript -RuntimeRoot $RuntimeRootPath -Target all -Force
        if ($LASTEXITCODE -ne 0) {
            throw "managed runtime fetch failed"
        }
    }

    Write-Host "Validating managed runtime layout"
    & python $LayoutCheckScript $RuntimeRootPath
    if ($LASTEXITCODE -ne 0) {
        throw "managed runtime layout validation failed"
    }

    Write-Host "Calling managed runtime debug skill"
    $Output = Invoke-CheckedProcess `
        -FilePath "cargo" `
        -ArgumentList @(
            "run", "--bin", "luaskills-debug", "--",
            "call",
            "--runtime-root", $RuntimeRootPath,
            "--skill-path", $SkillPath,
            "--tool", "smoke",
            "--args-json", '{"text":"smoke-script"}',
            "--output", "content"
        ) `
        -WorkingDirectory $RepoRoot

    # Payload stores the JSON object returned by the Lua smoke entry.
    # Payload 保存 Lua 冒烟入口返回的 JSON 对象。
    $Payload = $Output | ConvertFrom-Json
    if (-not $Payload.python_first.ok) { throw "python_first did not return ok=true" }
    if (-not $Payload.python_second.worker_reused) { throw "python worker was not reused" }
    if (-not $Payload.python_status_after.ready) { throw "python environment is not ready after call" }
    if ($Payload.python_first.value.dependency -ne "24.2") { throw "python dependency did not load" }
    if ($Payload.python_first.value.text -ne "smoke-script") { throw "python text argument did not round-trip" }
    if ($Payload.python_first.value.number -ne 41) { throw "python numeric result mismatch" }

    if (-not $Payload.node_first.ok) { throw "node_first did not return ok=true" }
    if (-not $Payload.node_second.worker_reused) { throw "node worker was not reused" }
    if (-not $Payload.node_status_after.ready) { throw "node environment is not ready after call" }
    if ($Payload.node_first.value.dependency -ne "is-odd") { throw "node dependency did not load" }
    if ($Payload.node_first.value.namedImport -ne "is-number-named") { throw "node named import did not load" }
    if ($Payload.node_first.value.namespaceImport -ne "is-number-namespace") { throw "node namespace import did not load" }
    if ($Payload.node_first.value.relativeImport -ne "local-helper") { throw "node relative import did not load" }
    if ($Payload.node_first.value.sideEffectImport -ne "side-effect") { throw "node side-effect import did not load" }
    if ($Payload.node_first.value.text -ne "smoke-script") { throw "node text argument did not round-trip" }
    if ($Payload.node_first.value.number -ne 42) { throw "node numeric result mismatch" }

    Write-Host "Managed runtime smoke passed"
    Write-Host "Runtime root: $RuntimeRootPath"
}
finally {
    if ((-not $KeepRuntimeRoot) -and ($RuntimeRootPath.StartsWith((Join-Path $RepoRoot "target\managed-runtime-smoke")))) {
        if (Test-Path -LiteralPath $RuntimeRootPath) {
            try {
                Remove-Item -Recurse -Force -LiteralPath $RuntimeRootPath -ErrorAction Stop
            }
            catch {
                Write-Warning "Managed runtime smoke passed, but cleanup failed for ${RuntimeRootPath}: $($_.Exception.Message)"
            }
        }
    }
}
