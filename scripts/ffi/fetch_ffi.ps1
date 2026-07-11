param(
    # Runtime root that receives FFI headers, libraries, and license files.
    # 接收 FFI 头文件、动态库与授权文件的运行根目录。
    [string]$RuntimeRoot = "output",
    # GitHub repository that publishes LuaSkills FFI SDK assets.
    # 发布 LuaSkills FFI SDK 资产的 GitHub 仓库。
    [string]$LuaSkillsRepo = "LuaSkills/luaskills",
    # GitHub Release tag that contains the LuaSkills FFI SDK asset.
    # 包含 LuaSkills FFI SDK 资产的 GitHub Release 标签。
    [string]$LuaSkillsVersion = ""
)

$ErrorActionPreference = "Stop"

function Resolve-ProjectRoot {
    <#
    .SYNOPSIS
    Resolve the repository or packaged demo root from script metadata or the caller location.
    从脚本元数据或调用方位置解析仓库根目录或已发布 demo 包根目录。

    .PARAMETER ScriptDirectory
    Directory that contains the current script when PowerShell exposes it.
    PowerShell 可用时当前脚本所在的目录。

    .OUTPUTS
    Root path that contains either Cargo.toml plus scripts, or packaged scripts.
    包含 Cargo.toml 与 scripts 的仓库根路径，或包含发布脚本的包根路径。
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
            $RepositoryMarkers = @("Cargo.toml", "pyproject.toml", "package.json", "go.mod")
            $IsRepositoryRoot = (Test-Path -LiteralPath (Join-Path $Current "scripts")) -and ($RepositoryMarkers | Where-Object {
                Test-Path -LiteralPath (Join-Path $Current $_)
            } | Select-Object -First 1)
            if ($IsRepositoryRoot) {
                return $Current
            }
            $PackagedFetchScript = Join-Path $Current "scripts\ffi\fetch_ffi.ps1"
            if ((Test-Path -LiteralPath $PackagedFetchScript) -and (Test-Path -LiteralPath (Join-Path $Current "runtime"))) {
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

function Get-PlatformKey {
    <#
    .SYNOPSIS
    Resolve the current platform key used by luaskills FFI SDK assets.
    解析当前平台对应的 luaskills FFI SDK 资产标识。

    .OUTPUTS
    Platform key such as windows-x64 or linux-arm64.
    类似 windows-x64 或 linux-arm64 的平台标识。
    #>
    $Arch = [System.Runtime.InteropServices.RuntimeInformation]::ProcessArchitecture.ToString().ToLowerInvariant()
    switch ($Arch) {
        "x64" { $ArchKey = "x64" }
        "arm64" { $ArchKey = "arm64" }
        default { throw "Unsupported architecture: $Arch" }
    }

    if ([System.Runtime.InteropServices.RuntimeInformation]::IsOSPlatform([System.Runtime.InteropServices.OSPlatform]::Windows)) {
        if ($ArchKey -ne "x64") {
            throw "Windows FFI SDK assets currently support x64 only."
        }
        return "windows-x64"
    }
    if ([System.Runtime.InteropServices.RuntimeInformation]::IsOSPlatform([System.Runtime.InteropServices.OSPlatform]::OSX)) {
        return "macos-$ArchKey"
    }
    if ([System.Runtime.InteropServices.RuntimeInformation]::IsOSPlatform([System.Runtime.InteropServices.OSPlatform]::Linux)) {
        return "linux-$ArchKey"
    }
    throw "Unsupported operating system."
}

function Get-ReleaseAssetInfo {
    <#
    .SYNOPSIS
    Find one exact GitHub Release asset download URL and API digest.
    查找一个精确 GitHub Release 资产下载地址与 API 摘要。

    .PARAMETER Repo
    GitHub repository in owner/name form.
    owner/name 形式的 GitHub 仓库。

    .PARAMETER Tag
    Release tag name.
    Release 标签名。

    .PARAMETER AssetName
    Exact asset file name.
    精确资产文件名。
    #>
    param(
        [string]$Repo,
        [string]$Tag,
        [string]$AssetName
    )

    $ApiUrl = "https://api.github.com/repos/$Repo/releases/tags/$Tag"
    $Release = Invoke-RestMethod -Uri $ApiUrl -UseBasicParsing
    $Asset = $Release.assets | Where-Object { $_.name -eq $AssetName } | Select-Object -First 1
    if (-not $Asset) {
        $Available = ($Release.assets | ForEach-Object { $_.name }) -join ", "
        throw "Asset '$AssetName' not found in $Repo@$Tag. Available: $Available"
    }
    return [PSCustomObject]@{
        Url = $Asset.browser_download_url
        Digest = [string]$Asset.digest
    }
}

function Get-FileSha256Hex {
    <#
    .SYNOPSIS
    Resolve one file SHA-256 digest with a portable fallback for older Windows PowerShell hosts.
    使用兼容旧版 Windows PowerShell 宿主的回退路径解析单个文件的 SHA-256 摘要。

    .PARAMETER Path
    File path whose SHA-256 digest should be returned as lowercase hexadecimal text.
    需要以小写十六进制文本返回 SHA-256 摘要的文件路径。
    #>
    param([string]$Path)

    $GetFileHashCommand = Get-Command -Name "Get-FileHash" -ErrorAction SilentlyContinue
    if ($GetFileHashCommand) {
        return (Get-FileHash -Algorithm SHA256 -LiteralPath $Path).Hash.ToLowerInvariant()
    }

    $Sha256 = [System.Security.Cryptography.SHA256]::Create()
    try {
        $Stream = [System.IO.File]::OpenRead($Path)
        try {
            $DigestBytes = $Sha256.ComputeHash($Stream)
        } finally {
            $Stream.Dispose()
        }
    } finally {
        $Sha256.Dispose()
    }

    $Builder = New-Object -TypeName System.Text.StringBuilder
    foreach ($Byte in $DigestBytes) {
        [void]$Builder.AppendFormat("{0:x2}", $Byte)
    }
    return $Builder.ToString()
}

function Save-ReleaseAssetWithDigest {
    <#
    .SYNOPSIS
    Download one GitHub Release asset and verify its GitHub API digest.
    下载单个 GitHub Release 资产并校验其 GitHub API 摘要。

    .PARAMETER Repo
    GitHub repository in owner/name form.
    owner/name 形式的 GitHub 仓库。

    .PARAMETER Tag
    Release tag name.
    Release 标签名。

    .PARAMETER AssetName
    Exact asset file name.
    精确资产文件名。

    .PARAMETER Destination
    Local destination file path.
    本地目标文件路径。
    #>
    param(
        [string]$Repo,
        [string]$Tag,
        [string]$AssetName,
        [string]$Destination
    )

    $AssetInfo = Get-ReleaseAssetInfo -Repo $Repo -Tag $Tag -AssetName $AssetName
    if ($AssetInfo.Digest -notlike "sha256:*") {
        throw "GitHub API digest for $AssetName is missing or unsupported: $($AssetInfo.Digest)"
    }
    Invoke-WebRequest -Uri $AssetInfo.Url -OutFile $Destination -UseBasicParsing
    $Expected = $AssetInfo.Digest.Substring("sha256:".Length).ToLowerInvariant()
    $Actual = Get-FileSha256Hex -Path $Destination
    if ($Expected -ne $Actual) {
        throw "SHA-256 mismatch for $AssetName. Expected $Expected, got $Actual"
    }
}

function Expand-ArchiveSmart {
    <#
    .SYNOPSIS
    Extract a .zip or .tar.gz archive.
    解压 .zip 或 .tar.gz 压缩包。

    .PARAMETER ArchivePath
    Archive path to extract.
    需要解压的压缩包路径。

    .PARAMETER Destination
    Destination directory.
    目标目录。
    #>
    param(
        [string]$ArchivePath,
        [string]$Destination
    )

    Ensure-Dir $Destination
    if ($ArchivePath.EndsWith(".zip", [System.StringComparison]::OrdinalIgnoreCase)) {
        Expand-Archive -Path $ArchivePath -DestinationPath $Destination -Force
    } else {
        tar -xzf $ArchivePath -C $Destination
    }
}

function Get-LuaSkillsLibraryCandidates {
    <#
    .SYNOPSIS
    Resolve the candidate LuaSkills dynamic library names for the current platform.
    解析当前平台对应的 LuaSkills 动态库候选名称。

    .OUTPUTS
    Ordered string array of candidate dynamic library file names.
    按顺序返回候选动态库文件名字符串数组。
    #>
    $Platform = Get-PlatformKey
    switch ($Platform) {
        "windows-x64" { return @("luaskills.dll", "libluaskills.dll") }
        "linux-x64" { return @("libluaskills.so", "luaskills.so") }
        "linux-arm64" { return @("libluaskills.so", "luaskills.so") }
        "macos-x64" { return @("libluaskills.dylib", "luaskills.dylib") }
        "macos-arm64" { return @("libluaskills.dylib", "luaskills.dylib") }
        default { throw "Unsupported LuaSkills FFI platform: $Platform" }
    }
}

function Test-ExistingLuaSkillsFfiContent {
    <#
    .SYNOPSIS
    Check whether the runtime root already contains one LuaSkills core dynamic library.
    检查运行根目录是否已经包含一个 LuaSkills core 动态库。

    .PARAMETER RuntimeRootPath
    Runtime root to inspect for installed LuaSkills libraries.
    需要检查已安装 LuaSkills 动态库的运行根目录。

    .OUTPUTS
    Boolean value that indicates whether one candidate library already exists.
    表示候选动态库是否已存在的布尔值。
    #>
    param([string]$RuntimeRootPath)

    $LibsDir = Join-Path $RuntimeRootPath "libs"
    foreach ($Candidate in Get-LuaSkillsLibraryCandidates) {
        if (Test-Path -LiteralPath (Join-Path $LibsDir $Candidate)) {
            return $true
        }
    }
    return $false
}

function Install-LuaSkillsFfi {
    <#
    .SYNOPSIS
    Download and install one luaskills FFI SDK archive into the runtime root.
    下载并安装一个 luaskills FFI SDK 归档到运行根目录。

    .PARAMETER RuntimeRootPath
    Runtime root that receives include, libs, and luaskills-ffi license material.
    接收 include、libs 与 luaskills-ffi 授权材料的运行根目录。
    #>
    param([string]$RuntimeRootPath)

    $Platform = Get-PlatformKey
    $AssetName = "luaskills-ffi-sdk-$Platform.tar.gz"
    $TempDir = Join-Path $env:TEMP "luaskills_ffi_sdk_$PID"
    $ArchivePath = Join-Path $TempDir $AssetName
    $ExtractDir = Join-Path $TempDir "extract"

    if (Test-Path -LiteralPath $TempDir) {
        Remove-Item -LiteralPath $TempDir -Recurse -Force
    }
    Ensure-Dir $TempDir

    try {
        try {
            Save-ReleaseAssetWithDigest -Repo $LuaSkillsRepo -Tag $LuaSkillsVersion -AssetName $AssetName -Destination $ArchivePath
        } catch {
            if (Test-ExistingLuaSkillsFfiContent -RuntimeRootPath $RuntimeRootPath) {
                Write-Warning "LuaSkills FFI SDK asset '$AssetName' was not found in $LuaSkillsRepo@$LuaSkillsVersion. Existing packaged LuaSkills core content will be used."
                return
            }
            throw
        }
        Expand-ArchiveSmart -ArchivePath $ArchivePath -Destination $ExtractDir

        $IncludeSource = Join-Path $ExtractDir "include"
        if (Test-Path -LiteralPath $IncludeSource) {
            $IncludeDest = Join-Path $RuntimeRootPath "include"
            Ensure-Dir $IncludeDest
            Copy-Item -Recurse -Force -Path (Join-Path $IncludeSource "*") -Destination $IncludeDest -ErrorAction SilentlyContinue
        }

        $LibrarySource = Join-Path $ExtractDir "lib"
        if (Test-Path -LiteralPath $LibrarySource) {
            $LibraryDest = Join-Path $RuntimeRootPath "libs"
            Ensure-Dir $LibraryDest
            Copy-Item -Recurse -Force -Path (Join-Path $LibrarySource "*") -Destination $LibraryDest -ErrorAction SilentlyContinue
        }

        $LicenseSource = Join-Path $ExtractDir "licenses"
        if (Test-Path -LiteralPath $LicenseSource) {
            $LicenseDest = Join-Path $RuntimeRootPath "licenses\luaskills-ffi"
            Ensure-Dir $LicenseDest
            Copy-Item -Recurse -Force -Path (Join-Path $LicenseSource "*") -Destination $LicenseDest -ErrorAction SilentlyContinue
        }

        if (-not (Test-ExistingLuaSkillsFfiContent -RuntimeRootPath $RuntimeRootPath)) {
            throw "LuaSkills dynamic library was not found after installing $AssetName"
        }
    } finally {
        Remove-Item -LiteralPath $TempDir -Recurse -Force -ErrorAction SilentlyContinue
    }
}

$ScriptDir = if ($PSScriptRoot) { $PSScriptRoot } elseif ($PSCommandPath) { Split-Path -Parent $PSCommandPath } elseif ($MyInvocation.MyCommand.Path) { Split-Path -Parent $MyInvocation.MyCommand.Path } else { "" }
$ProjectRoot = Resolve-ProjectRoot -ScriptDirectory $ScriptDir
Set-Location $ProjectRoot
if ([string]::IsNullOrWhiteSpace($LuaSkillsVersion)) {
    $VersionPath = Join-Path $ProjectRoot "VERSION"
    if (Test-Path -LiteralPath $VersionPath) {
        $LuaSkillsVersion = "v$((Get-Content -Raw -LiteralPath $VersionPath).Trim())"
    } else {
        $CargoTomlPath = Join-Path $ProjectRoot "Cargo.toml"
        if (-not (Test-Path -LiteralPath $CargoTomlPath)) {
            throw "LuaSkills FFI SDK version was not provided. Pass -LuaSkillsVersion when running from a packaged runtime."
        }
        $CargoTomlText = Get-Content -Raw -LiteralPath $CargoTomlPath
        $CargoVersionMatch = [regex]::Match($CargoTomlText, '(?m)^version\s*=\s*"([^"]+)"')
        if (-not $CargoVersionMatch.Success) {
            throw "Unable to resolve fallback LuaSkills version from Cargo.toml."
        }
        $LuaSkillsVersion = "v$($CargoVersionMatch.Groups[1].Value)"
    }
}
Ensure-Dir $RuntimeRoot
Install-LuaSkillsFfi -RuntimeRootPath $RuntimeRoot
Write-Host "LuaSkills FFI SDK installed into $RuntimeRoot"
