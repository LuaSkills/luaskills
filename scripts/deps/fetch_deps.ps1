param(
    [ValidateSet("all", "lua", "vldb", "vldb-controller", "vldb-direct")]
    [string]$Target = "all",
    [ValidateSet("none", "vldb-controller", "vldb-direct", "host-callback")]
    [string]$Database = "vldb-controller",
    [string]$RuntimeRoot = "output",
    [string]$LuaRuntimeRepo = "LuaSkills/luaskills-packages",
    [string]$LuaRuntimeSeries = "0.1",
    [string]$LuaRuntimeVersion = "",
    [string]$VldbControllerRepo = "OpenVulcan/vldb-controller",
    [string]$VldbControllerVersion = "v0.2.1",
    [string]$VldbSQLiteRepo = "OpenVulcan/vldb-sqlite",
    [string]$VldbSQLiteVersion = "v0.1.5",
    [string]$VldbLanceDBRepo = "OpenVulcan/vldb-lancedb",
    [string]$VldbLanceDBVersion = "v0.1.5",
    [switch]$LuaPackagesOnly,
    [switch]$SkipLuaRuntimeLibs,
    [switch]$DevCache
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
    Root path that contains either Cargo.toml plus scripts, or packaged demo scripts.
    包含 Cargo.toml 与 scripts 的仓库根路径，或包含发布 demo 脚本的包根路径。
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
            $PackagedFetchScript = Join-Path $Current "scripts\deps\fetch_deps.ps1"
            $PackagedManifest = Join-Path $Current "demo-manifest.json"
            $PackagedRuntime = Join-Path $Current "runtime"
            if ((Test-Path -LiteralPath $PackagedFetchScript) -and ((Test-Path -LiteralPath $PackagedManifest) -or (Test-Path -LiteralPath $PackagedRuntime))) {
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
if (-not $ProjectRoot) {
    $ProjectRoot = (Resolve-Path -LiteralPath (Get-Location).Path).Path
}
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

    if (-not (Test-Path -LiteralPath $Path)) {
        New-Item -ItemType Directory -Path $Path -Force | Out-Null
    }
}

function Get-PlatformKey {
    <#
    .SYNOPSIS
    Resolve the current platform key used by luaskills runtime assets.
    解析当前平台对应的 luaskills runtime 资产标识。

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
            throw "Windows runtime assets currently support x64 only."
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

function Get-VldbAssetInfo {
    <#
    .SYNOPSIS
    Resolve VLDB asset metadata for the current platform.
    解析当前平台的 VLDB 资产元数据。

    .OUTPUTS
    Hashtable containing target, archive_ext, binary_name, and dynamic library names.
    包含 target、archive_ext、binary_name 与动态库名称的哈希表。
    #>
    $Arch = [System.Runtime.InteropServices.RuntimeInformation]::ProcessArchitecture.ToString().ToLowerInvariant()
    switch ($Arch) {
        "x64" { $ArchKey = "x86_64" }
        "arm64" { $ArchKey = "aarch64" }
        default { throw "Unsupported architecture for vldb-controller: $Arch" }
    }

    if ([System.Runtime.InteropServices.RuntimeInformation]::IsOSPlatform([System.Runtime.InteropServices.OSPlatform]::Windows)) {
        if ($ArchKey -ne "x86_64") {
            throw "vldb-controller currently supports Windows x86_64 only."
        }
        return @{ target = "x86_64-pc-windows-msvc"; archive_ext = ".zip"; binary_name = "vldb-controller.exe"; dynamic_ext = ".dll"; sqlite_library = "vldb_sqlite.dll"; lancedb_library = "vldb_lancedb.dll" }
    }
    if ([System.Runtime.InteropServices.RuntimeInformation]::IsOSPlatform([System.Runtime.InteropServices.OSPlatform]::OSX)) {
        return @{ target = "$ArchKey-apple-darwin"; archive_ext = ".tar.gz"; binary_name = "vldb-controller"; dynamic_ext = ".dylib"; sqlite_library = "libvldb_sqlite.dylib"; lancedb_library = "libvldb_lancedb.dylib" }
    }
    if ([System.Runtime.InteropServices.RuntimeInformation]::IsOSPlatform([System.Runtime.InteropServices.OSPlatform]::Linux)) {
        return @{ target = "$ArchKey-unknown-linux-gnu"; archive_ext = ".tar.gz"; binary_name = "vldb-controller"; dynamic_ext = ".so"; sqlite_library = "libvldb_sqlite.so"; lancedb_library = "libvldb_lancedb.so" }
    }
    throw "Unsupported operating system for vldb-controller."
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

function Convert-TagToSemVer {
    <#
    .SYNOPSIS
    Convert one Git tag such as v0.1.6 into a semantic-version object.
    将形如 v0.1.6 的 Git 标签转换为语义化版本对象。

    .PARAMETER Tag
    Git tag text to normalize and parse.
    需要规范化并解析的 Git 标签文本。
    #>
    param([string]$Tag)

    $Normalized = if ($Tag.StartsWith("v")) { $Tag.Substring(1) } else { $Tag }
    if ($Normalized -notmatch '^\d+\.\d+\.\d+$') {
        throw "Unsupported semantic version tag: $Tag"
    }
    return [System.Version]$Normalized
}

function Resolve-ReleaseTagForSeries {
    <#
    .SYNOPSIS
    Resolve the newest published GitHub release tag inside one major.minor series.
    解析一个 major.minor 协议线内最新的已发布 GitHub release 标签。

    .PARAMETER Repo
    GitHub repository in owner/name form.
    owner/name 形式的 GitHub 仓库。

    .PARAMETER Series
    Major.minor series such as 0.1.
    形如 0.1 的 major.minor 协议线。
    #>
    param(
        [string]$Repo,
        [string]$Series
    )

    if ($Series -notmatch '^\d+\.\d+$') {
        throw "Unsupported packages series: $Series"
    }

    $ApiUrl = "https://api.github.com/repos/$Repo/releases?per_page=100"
    $Releases = Invoke-RestMethod -Uri $ApiUrl -UseBasicParsing
    $Matches = @()
    foreach ($Release in $Releases) {
        if ($Release.draft -or $Release.prerelease) {
            continue
        }
        $TagName = [string]$Release.tag_name
        try {
            $Version = Convert-TagToSemVer -Tag $TagName
        } catch {
            continue
        }
        $ReleaseSeries = "$($Version.Major).$($Version.Minor)"
        if ($ReleaseSeries -ne $Series) {
            continue
        }
        $Matches += [PSCustomObject]@{
            Tag = $TagName
            Version = $Version
        }
    }

    if (-not $Matches -or $Matches.Count -eq 0) {
        throw "No published release found for $Repo series $Series"
    }

    return ($Matches | Sort-Object Version -Descending | Select-Object -First 1).Tag
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

function Get-FileSha256Hex {
    <#
    .SYNOPSIS
    Resolve one file SHA-256 digest with a portable fallback for older Windows PowerShell hosts.
    使用兼容旧版 Windows PowerShell 宿主的回退路径解析单个文件的 SHA-256 摘要。

    .PARAMETER Path
    File path whose SHA-256 digest should be returned as lowercase hexadecimal text.
    需要以小写十六进制文本返回 SHA-256 摘要的文件路径。

    .OUTPUTS
    Lowercase hexadecimal SHA-256 digest of the target file.
    目标文件的小写十六进制 SHA-256 摘要。
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

function Test-ExistingRuntimeContent {
    <#
    .SYNOPSIS
    Check whether a packaged demo already contains runnable runtime content.
    检查已发布 demo 包是否已经包含可运行的 runtime 内容。

    .PARAMETER RuntimeRootPath
    Runtime root to inspect.
    需要检查的运行根目录。

    .OUTPUTS
    Boolean value indicating whether local runtime content already exists.
    表示本地 runtime 内容是否已经存在的布尔值。
    #>
    param([string]$RuntimeRootPath)

    $SkillsDir = Join-Path $RuntimeRootPath "skills"
    $LuaPackagesDir = Join-Path $RuntimeRootPath "lua_packages"
    return (Test-Path -LiteralPath $SkillsDir) -or (Test-Path -LiteralPath $LuaPackagesDir)
}

function Install-LuaRuntime {
    <#
    .SYNOPSIS
    Download and install one luaskills runtime-packages archive.
    下载并安装一个 luaskills runtime-packages 归档。

    .PARAMETER RuntimeRootPath
    Runtime root that receives lua_packages plus optional packaged-runtime metadata.
    接收 lua_packages 以及可选打包运行时元数据的运行根目录。

    .PARAMETER SkipRuntimeLibraries
    Whether the runtime package install should omit the bundled libs directory.
    当前 runtime package 安装是否应省略包内 libs 目录。

    .PARAMETER LuaPackagesOnly
    Whether the runtime package install should stage only lua_packages and skip resources, licenses, and libs.
    当前 runtime package 安装是否应只暂存 lua_packages，并跳过 resources、licenses 与 libs。
    #>
    param(
        [string]$RuntimeRootPath,
        [switch]$SkipRuntimeLibraries,
        [switch]$LuaPackagesOnly
    )

    $Platform = Get-PlatformKey
    $AssetName = "lua-runtime-packages-$Platform.tar.gz"
    $ResolvedLuaRuntimeTag = if ([string]::IsNullOrWhiteSpace($LuaRuntimeVersion)) { Resolve-ReleaseTagForSeries -Repo $LuaRuntimeRepo -Series $LuaRuntimeSeries } else { $LuaRuntimeVersion }
    $TempDir = Join-Path $env:TEMP "luaskills_lua_runtime_$PID"
    $ArchivePath = Join-Path $TempDir $AssetName
    $ExtractDir = Join-Path $TempDir "extract"

    if (Test-Path -LiteralPath $TempDir) {
        Remove-Item -LiteralPath $TempDir -Recurse -Force
    }
    Ensure-Dir $TempDir

    try {
        try {
            Save-ReleaseAssetWithDigest -Repo $LuaRuntimeRepo -Tag $ResolvedLuaRuntimeTag -AssetName $AssetName -Destination $ArchivePath
        } catch {
            if (Test-ExistingRuntimeContent -RuntimeRootPath $RuntimeRootPath) {
                Write-Warning "Lua runtime packages asset '$AssetName' was not found in $LuaRuntimeRepo@$ResolvedLuaRuntimeTag. Existing packaged runtime content will be used."
                return
            }
            throw
        }
        Expand-ArchiveSmart -ArchivePath $ArchivePath -Destination $ExtractDir

        if ($LuaPackagesOnly) {
            $RuntimeDirNames = @("lua_packages")
        } else {
            $RuntimeDirNames = @("lua_packages", "resources")
            if (-not $SkipRuntimeLibraries) {
                $RuntimeDirNames += "libs"
            }
        }
        foreach ($DirName in $RuntimeDirNames) {
            $Source = Join-Path $ExtractDir $DirName
            if (Test-Path -LiteralPath $Source) {
                Ensure-Dir (Join-Path $RuntimeRootPath $DirName)
                Copy-Item -Recurse -Force -Path (Join-Path $Source "*") -Destination (Join-Path $RuntimeRootPath $DirName) -ErrorAction SilentlyContinue
            }
        }

        if (-not $LuaPackagesOnly) {
            $LicenseSource = Join-Path $ExtractDir "licenses"
            if (Test-Path -LiteralPath $LicenseSource) {
                $LicenseDest = Join-Path $RuntimeRootPath "licenses"
                Ensure-Dir $LicenseDest
                Copy-Item -Recurse -Force -Path (Join-Path $LicenseSource "*") -Destination $LicenseDest -ErrorAction SilentlyContinue
            }

            $RuntimeManifestPath = Join-Path $RuntimeRootPath "resources\lua-runtime-manifest.json"
            $PackagesManifestPath = Join-Path $RuntimeRootPath "resources\luaskills-packages-manifest.json"
            if (-not (Test-Path -LiteralPath $RuntimeManifestPath)) {
                throw "Lua runtime manifest was not found after installing $AssetName"
            }
            if (-not (Test-Path -LiteralPath $PackagesManifestPath)) {
                throw "LuaSkills packages manifest was not found after installing $AssetName"
            }
        }
    } finally {
        Remove-Item -LiteralPath $TempDir -Recurse -Force -ErrorAction SilentlyContinue
    }
}

function Install-VldbController {
    <#
    .SYNOPSIS
    Download and install vldb-controller into the demo runtime bin directory.
    下载 vldb-controller 并安装到 demo 运行期 bin 目录。

    .PARAMETER RuntimeRootPath
    Runtime root that receives bin/vldb-controller.
    接收 bin/vldb-controller 的运行根目录。
    #>
    param([string]$RuntimeRootPath)

    $AssetInfo = Get-VldbAssetInfo
    $AssetName = "vldb-controller-$VldbControllerVersion-$($AssetInfo.target)$($AssetInfo.archive_ext)"
    $TempDir = Join-Path $env:TEMP "luaskills_vldb_$PID"
    $ArchivePath = Join-Path $TempDir $AssetName
    $ExtractDir = Join-Path $TempDir "extract"

    if (Test-Path -LiteralPath $TempDir) {
        Remove-Item -LiteralPath $TempDir -Recurse -Force
    }
    Ensure-Dir $TempDir

    try {
        Save-ReleaseAssetWithDigest -Repo $VldbControllerRepo -Tag $VldbControllerVersion -AssetName $AssetName -Destination $ArchivePath
        Expand-ArchiveSmart -ArchivePath $ArchivePath -Destination $ExtractDir

        $Binary = Get-ChildItem -Recurse -File -Path $ExtractDir -Filter $AssetInfo.binary_name | Select-Object -First 1
        if (-not $Binary) {
            throw "vldb-controller binary not found in $AssetName"
        }

        $RuntimeBin = Join-Path $RuntimeRootPath "bin"
        Ensure-Dir $RuntimeBin
        Copy-Item -Force -LiteralPath $Binary.FullName -Destination (Join-Path $RuntimeBin $AssetInfo.binary_name)

        if ($DevCache) {
            $DevBin = Join-Path $ProjectRoot "third_party\vldb_controller\bin"
            Ensure-Dir $DevBin
            Copy-Item -Force -LiteralPath $Binary.FullName -Destination (Join-Path $DevBin $AssetInfo.binary_name)
        }

        $LicenseDest = Join-Path $RuntimeRootPath "licenses\vldb-controller"
        Ensure-Dir $LicenseDest
        [ordered]@{
            schema_version = 1
            name = "vldb-controller"
            version = $VldbControllerVersion
            asset = $AssetName
            installed_binary = "bin/$($AssetInfo.binary_name)"
        } | ConvertTo-Json -Depth 6 | Set-Content -Path (Join-Path $RuntimeRootPath "resources\vldb-controller-manifest.json") -Encoding UTF8
    } finally {
        Remove-Item -LiteralPath $TempDir -Recurse -Force -ErrorAction SilentlyContinue
    }
}

function Install-VldbLibraryAsset {
    <#
    .SYNOPSIS
    Download and install one VLDB dynamic library asset.
    下载并安装单个 VLDB 动态库资产。

    .PARAMETER RuntimeRootPath
    Runtime root that receives libs content.
    接收 libs 内容的运行根目录。

    .PARAMETER Repo
    GitHub repository in owner/name form.
    owner/name 形式的 GitHub 仓库。

    .PARAMETER Version
    Release tag name.
    Release 标签名。

    .PARAMETER Prefix
    Release asset prefix such as vldb-sqlite-lib.
    发布资产前缀，例如 vldb-sqlite-lib。

    .PARAMETER NameHint
    Lowercase library name hint used for recursive lookup.
    递归查找动态库时使用的小写名称提示。
    #>
    param(
        [string]$RuntimeRootPath,
        [string]$Repo,
        [string]$Version,
        [string]$Prefix,
        [string]$NameHint
    )

    $AssetInfo = Get-VldbAssetInfo
    $AssetName = "$Prefix-$Version-$($AssetInfo.target)$($AssetInfo.archive_ext)"
    $TempDir = Join-Path $env:TEMP "luaskills_$($Prefix)_$PID"
    $ArchivePath = Join-Path $TempDir $AssetName
    $ExtractDir = Join-Path $TempDir "extract"

    if (Test-Path -LiteralPath $TempDir) {
        Remove-Item -LiteralPath $TempDir -Recurse -Force
    }
    Ensure-Dir $TempDir

    try {
        Save-ReleaseAssetWithDigest -Repo $Repo -Tag $Version -AssetName $AssetName -Destination $ArchivePath
        Expand-ArchiveSmart -ArchivePath $ArchivePath -Destination $ExtractDir

        $Library = Get-ChildItem -Recurse -File -Path $ExtractDir | Where-Object {
            $_.Name.ToLowerInvariant().Contains($NameHint) -and $_.Name.EndsWith($AssetInfo.dynamic_ext, [System.StringComparison]::OrdinalIgnoreCase)
        } | Select-Object -First 1
        if (-not $Library) {
            throw "VLDB dynamic library matching '$NameHint' not found in $AssetName"
        }

        $RuntimeLibs = Join-Path $RuntimeRootPath "libs"
        Ensure-Dir $RuntimeLibs
        Copy-Item -Force -LiteralPath $Library.FullName -Destination (Join-Path $RuntimeLibs $Library.Name)
        return [ordered]@{
            asset = $AssetName
            installed_path = "libs/$($Library.Name)"
        }
    } finally {
        Remove-Item -LiteralPath $TempDir -Recurse -Force -ErrorAction SilentlyContinue
    }
}

function Install-VldbDirectLibraries {
    <#
    .SYNOPSIS
    Download and install vldb-sqlite-lib and vldb-lancedb-lib assets.
    下载并安装 vldb-sqlite-lib 与 vldb-lancedb-lib 资产。

    .PARAMETER RuntimeRootPath
    Runtime root that receives libs content.
    接收 libs 内容的运行根目录。
    #>
    param([string]$RuntimeRootPath)

    $SQLite = Install-VldbLibraryAsset -RuntimeRootPath $RuntimeRootPath -Repo $VldbSQLiteRepo -Version $VldbSQLiteVersion -Prefix "vldb-sqlite-lib" -NameHint "sqlite"
    $LanceDB = Install-VldbLibraryAsset -RuntimeRootPath $RuntimeRootPath -Repo $VldbLanceDBRepo -Version $VldbLanceDBVersion -Prefix "vldb-lancedb-lib" -NameHint "lancedb"
    [ordered]@{
        schema_version = 1
        database_mode = "vldb-direct"
        sqlite = $SQLite
        lancedb = $LanceDB
    } | ConvertTo-Json -Depth 8 | Set-Content -Path (Join-Path $RuntimeRootPath "resources\vldb-direct-manifest.json") -Encoding UTF8
}

Ensure-Dir $RuntimeRoot
Ensure-Dir (Join-Path $RuntimeRoot "resources")

if ($Target -eq "all" -or $Target -eq "lua") {
    Install-LuaRuntime -RuntimeRootPath $RuntimeRoot -SkipRuntimeLibraries:$SkipLuaRuntimeLibs -LuaPackagesOnly:$LuaPackagesOnly
}

if (($Target -eq "all" -and $Database -eq "vldb-controller") -or ($Target -eq "vldb" -and $Database -eq "vldb-controller") -or $Target -eq "vldb-controller") {
    Install-VldbController -RuntimeRootPath $RuntimeRoot
}

if (($Target -eq "all" -and $Database -eq "vldb-direct") -or ($Target -eq "vldb" -and $Database -eq "vldb-direct") -or $Target -eq "vldb-direct") {
    Install-VldbDirectLibraries -RuntimeRootPath $RuntimeRoot
}

Write-Host "Runtime dependency target '$Target' with database preset '$Database' installed into $RuntimeRoot"
