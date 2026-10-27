# Sign and verify production files with the pinned Certum cloud identity.
# 使用固定的 Certum 云证书身份签名和验证正式产品文件。
# Release tooling accepts Install, Sign, or Verify; EvidenceDirectory holds public installation evidence and Files names exact product paths.
# 发行工具接收安装、签名或验签阶段；EvidenceDirectory 保存公开安装证据，Files 指定精确产品路径。
param(
    [Parameter(Mandatory)][ValidateSet('Install', 'Sign', 'Verify')][string]$Phase,
    [Parameter(Mandatory)][string]$EvidenceDirectory,
    [string[]]$Files = @()
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# Invoke a native tool with bounded execution; captured output is never logged automatically.
# 限时运行原生工具，捕获的输出绝不自动写入日志。
# Executable and Arguments define a shell-free invocation; TimeoutSeconds limits execution. Returns exit code and captured streams.
# Executable 与 Arguments 定义不经过命令解释器的调用；TimeoutSeconds 限制执行时间。返回退出码和捕获的输出流。
function Invoke-BoundedTool {
    param([string]$Executable, [string[]]$Arguments, [int]$TimeoutSeconds = 120)
    $start = [System.Diagnostics.ProcessStartInfo]::new($Executable)
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.WindowStyle = [System.Diagnostics.ProcessWindowStyle]::Hidden
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    foreach ($argument in $Arguments) { $start.ArgumentList.Add($argument) }
    $process = [System.Diagnostics.Process]::Start($start)
    $stdout = $process.StandardOutput.ReadToEndAsync()
    $stderr = $process.StandardError.ReadToEndAsync()
    try {
        if (-not $process.WaitForExit($TimeoutSeconds * 1000)) {
            $process.Kill($true)
            $process.WaitForExit()
            throw 'native_tool_timeout'
        }
        return [pscustomobject]@{ ExitCode = $process.ExitCode; Stdout = $stdout.Result; Stderr = $stderr.Result }
    } finally {
        $process.Dispose()
    }
}

# Return valid Certum code-signing candidates from CurrentUser/My that have an associated private key; accepts no parameters.
# 返回当前用户个人存储中有效且关联私钥的 Certum 代码签名候选证书；不接收参数。
function Get-CertumSigningCertificates {
    $now = Get-Date
    return @(Get-ChildItem Cert:\CurrentUser\My | Where-Object {
        $_.HasPrivateKey -and $_.NotBefore -le $now -and $_.NotAfter -gt $now -and
        $_.Issuer -match 'Certum' -and
        @($_.EnhancedKeyUsageList | Where-Object ObjectId -eq '1.3.6.1.5.5.7.3.3').Count -gt 0
    })
}

# These pinned values come from the official Certum download page and verified MSI metadata.
# 以下固定值来自 Certum 官方下载页面和已验证的 MSI 元数据。
$installerUrl = 'https://files.certum.eu/software/SimplySignDesktop/Windows/9.4.4.92/SimplySignDesktop-9.4.4.92-64-bit-en.msi'
$installerSha256 = '8ec420fc27798b86078b7bd02fe7152097e1b3005bab51820eaca8e57df84da3'
$installerSigner = 'AC9C643063CD501E851A7B6A9762E295FBABB012'
$desktop = Join-Path $env:ProgramFiles 'Certum\SimplySign Desktop\SimplySignDesktop.exe'
$evidence = [IO.Path]::GetFullPath($EvidenceDirectory)
New-Item -ItemType Directory -Path $evidence -Force | Out-Null

if ($Phase -eq 'Install') {
    # Install only the exact official binary whose hash and Authenticode signature were checked.
    # 仅安装哈希与 Authenticode 签名均通过校验的指定官方安装包。
    $installer = Join-Path $env:RUNNER_TEMP 'SimplySignDesktop-9.4.4.92.msi'
    Invoke-WebRequest -Uri $installerUrl -OutFile $installer
    if ((Get-FileHash -LiteralPath $installer -Algorithm SHA256).Hash.ToLowerInvariant() -ne $installerSha256) {
        throw 'simplysign_installer_hash_mismatch'
    }
    $signature = Get-AuthenticodeSignature -LiteralPath $installer
    if ($signature.Status -ne 'Valid' -or $signature.SignerCertificate.Thumbprint -ne $installerSigner) {
        throw 'simplysign_installer_signature_invalid'
    }
    $installed = Invoke-BoundedTool -Executable 'msiexec.exe' -Arguments @('/i', $installer, '/qn', '/norestart') -TimeoutSeconds 240
    if ($installed.ExitCode -notin @(0, 3010)) { throw "simplysign_install_failed_$($installed.ExitCode)" }
    if (-not (Test-Path -LiteralPath $desktop -PathType Leaf) -or
        -not (Test-Path -LiteralPath "$env:WINDIR\System32\SimplySignPKCS.dll" -PathType Leaf)) {
        throw 'simplysign_installed_files_missing'
    }
    # Match CodeSignAuto's close-before-login sequence without passing credentials to this process.
    # 按 CodeSignAuto 的顺序在登录前关闭客户端，此时进程不接收凭据。
    $closed = Invoke-BoundedTool -Executable $desktop -Arguments @('/close') -TimeoutSeconds 15
    $closeDeadline = [DateTime]::UtcNow.AddSeconds(15)
    do {
        $existing = @(Get-Process -Name SimplySignDesktop -ErrorAction SilentlyContinue |
            Where-Object SessionId -eq ([System.Diagnostics.Process]::GetCurrentProcess().SessionId))
        if ($existing.Count -eq 0) { break }
        Start-Sleep -Seconds 1
    } while ([DateTime]::UtcNow -lt $closeDeadline)
    if ($existing.Count -ne 0) { throw 'simplysign_close_before_login_failed' }
    [pscustomobject]@{
        installerSha256 = $installerSha256
        installerSigner = $installerSigner
        version = (Get-Item -LiteralPath $desktop).VersionInfo.FileVersion
        installerExitCode = $installed.ExitCode
    } | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $evidence 'installer-result.json') -Encoding utf8
    Write-Output 'Verified official SimplySign Desktop installation completed'
    exit 0
}


# Pin certificate identity to the reviewed public certificate from the successful smoke test.
# 将证书身份固定为已成功实测并审核的公开证书。
$policy = Get-Content -LiteralPath (Join-Path $PSScriptRoot 'signing-policy.json') -Raw -Encoding utf8 | ConvertFrom-Json
if ($Files.Count -eq 0) { throw 'release_signing_targets_required' }
foreach ($file in $Files) {
    if (-not (Test-Path -LiteralPath $file -PathType Leaf)) { throw 'release_signing_target_missing' }
    if ((Get-Item -LiteralPath $file).Attributes -band [IO.FileAttributes]::ReparsePoint) { throw 'release_signing_target_is_link' }
}
$sdkRoot = Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10\bin'
$signTools = @(Get-ChildItem -LiteralPath $sdkRoot -Directory |
    Where-Object Name -match '^\d+\.\d+\.\d+\.\d+$' |
    Sort-Object { [version]$_.Name } -Descending |
    ForEach-Object { Join-Path $_.FullName 'x64\signtool.exe' } |
    Where-Object { Test-Path -LiteralPath $_ -PathType Leaf })
if ($signTools.Count -eq 0) { throw 'windows_sdk_signtool_missing' }
$signTool = $signTools[0]
$client = $null
try {
    if ($Phase -eq 'Sign') {
        # Credentials are confined to one login process; child signing tools inherit no seed.
        # 凭据仅用于一次登录，后续签名工具不继承动态口令种子。
        $email = $env:CERTUM_TOTP_EMAIL
        if ([string]::IsNullOrWhiteSpace($email)) { throw 'certum_email_missing' }
        $otp = & python (Join-Path $PSScriptRoot 'certum_totp.py') totp
        if ($LASTEXITCODE -ne 0 -or $otp -notmatch '^\d{6}$') { throw 'certum_totp_failed' }
        Write-Output "::add-mask::$otp"
        Remove-Item Env:CERTUM_TOTP_SECRET -ErrorAction SilentlyContinue
        Remove-Item Env:CERTUM_TOTP_EMAIL -ErrorAction SilentlyContinue
        $start = [System.Diagnostics.ProcessStartInfo]::new($desktop)
        $start.UseShellExecute = $false
        $start.WindowStyle = [System.Diagnostics.ProcessWindowStyle]::Hidden
        $start.RedirectStandardOutput = $true
        $start.RedirectStandardError = $true
        foreach ($argument in @('/autologin', $email, $otp)) { $start.ArgumentList.Add($argument) }
        $client = [System.Diagnostics.Process]::Start($start)
        $clientOutput = $client.StandardOutput.ReadToEndAsync()
        $clientError = $client.StandardError.ReadToEndAsync()
        $email = $null
        $otp = $null
        $deadline = [DateTime]::UtcNow.AddSeconds(90)
        do {
            $certificates = @(Get-CertumSigningCertificates | Where-Object Thumbprint -eq $policy.certificate_thumbprint)
            if ($certificates.Count -eq 1) { break }
            Start-Sleep -Seconds 2
        } while ([DateTime]::UtcNow -lt $deadline)
        if ($certificates.Count -ne 1) { throw 'pinned_certum_certificate_not_ready' }
        if ($certificates[0].Subject -ne $policy.certificate_subject -or $certificates[0].Issuer -ne $policy.certificate_issuer) {
            throw 'certum_certificate_identity_mismatch'
        }
    }
    foreach ($file in $Files) {
        if ($Phase -eq 'Sign') {
            # A certificate already in the store does not prove login finished, so signing retries within a bound.
            # 存储中已有证书不代表登录已完成，因此签名在限定次数内重试。
            for ($attempt = 1; $attempt -le 6; $attempt++) {
                $signed = Invoke-BoundedTool -Executable $signTool -Arguments @(
                    'sign', '/sha1', $policy.certificate_thumbprint, '/fd', 'SHA256',
                    '/tr', 'http://time.certum.pl', '/td', 'SHA256', $file
                ) -TimeoutSeconds 120
                if ($signed.ExitCode -eq 0) { break }
                Start-Sleep -Seconds 10
            }
            if ($signed.ExitCode -ne 0) { throw 'certum_product_signing_failed' }
        }
        # Verify the file bytes, trusted chain, exact publisher, and timestamp before packaging.
        # 打包前验证文件字节、可信证书链、精确发布者身份与时间戳。
        $verified = Invoke-BoundedTool -Executable $signTool -Arguments @('verify', '/pa', '/all', '/tw', $file)
        if ($verified.ExitCode -ne 0) { throw 'product_authenticode_verify_failed' }
        $signature = Get-AuthenticodeSignature -LiteralPath $file
        if ($signature.Status -ne 'Valid' -or $null -eq $signature.TimeStamperCertificate -or
            $signature.SignerCertificate.Thumbprint -ne $policy.certificate_thumbprint -or
            $signature.SignerCertificate.Subject -ne $policy.certificate_subject -or
            $signature.SignerCertificate.Issuer -ne $policy.certificate_issuer) {
            throw 'product_signature_identity_or_timestamp_invalid'
        }
    }
    Write-Output "Certum product signatures verified: $($Files.Count)"
} catch {
    # Never propagate client diagnostics or a credential-bearing process invocation.
    # 不传播客户端诊断或包含凭据的进程调用信息。
    $message = $_.Exception.Message
    if ($message -match '^[A-Za-z0-9_]+$') { throw $message }
    throw 'product_cloud_signing_failed'
} finally {
    Remove-Item Env:CERTUM_TOTP_SECRET -ErrorAction SilentlyContinue
    Remove-Item Env:CERTUM_TOTP_EMAIL -ErrorAction SilentlyContinue
    $email = $null
    $otp = $null
    if ($null -ne $client) {
        try { $null = Invoke-BoundedTool -Executable $desktop -Arguments @('/close') -TimeoutSeconds 10 } catch { }
        if (-not $client.HasExited) { $client.Kill($true) }
        $client.Dispose()
    }
}
