param(
    # Target platform key used in archive and manifest names.
    # 用于归档文件与清单文件命名的目标平台标识。
    [string]$Platform = "",
    # Source third_party directory produced by the build pipeline.
    # 构建流水线生成的 third_party 源目录。
    [string]$ThirdPartyDir = "third_party",
    # Runtime staging directory assembled before compression.
    # 压缩前用于组装运行期目录的暂存目录。
    [string]$StagingDir = "target\lua-runtime-package",
    # Output directory that receives the final runtime archive.
    # 接收最终 runtime 压缩包的输出目录。
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

# ExcludedRuntimeLibraryNames prevents build-only LuaJIT shims from leaking into runtime packages.
# ExcludedRuntimeLibraryNames 防止仅用于构建的 LuaJIT 兼容库泄漏到运行期包中。
$ExcludedRuntimeLibraryNames = @("lua51.dll", "luajit.exe", "lua.exe")

# BundledNativeDependencyPatterns identifies system-linked runtime libraries that must travel with packages.
# BundledNativeDependencyPatterns 标识需要随包携带的系统链接运行库。
$BundledNativeDependencyPatterns = @(
    "libz.so*",
    "zlib*.dll",
    "libz*.dylib",
    "libcurl.so*",
    "libcurl*.dll",
    "libcurl*.dylib",
    "libssl.so*",
    "libssl*.dll",
    "libssl*.dylib",
    "libcrypto.so*",
    "libcrypto*.dll",
    "libcrypto*.dylib",
    "libpcre2-*.so*",
    "pcre2*.dll",
    "libpcre2-*.dylib",
    "libyaml*.so*",
    "yaml*.dll",
    "libyaml*.dylib"
)

$script:BundledLibraries = New-Object 'System.Collections.Generic.List[object]'

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

function Copy-DirectoryContent {
    <#
    .SYNOPSIS
    Copy all children from one directory to another directory.
    将一个目录下的全部子项复制到另一个目录。

    .PARAMETER Source
    Existing source directory.
    已存在的源目录。

    .PARAMETER Destination
    Destination directory to create and populate.
    需要创建并填充的目标目录。
    #>
    param(
        [string]$Source,
        [string]$Destination
    )

    if (-not (Test-Path -LiteralPath $Source)) {
        return
    }

    Ensure-Dir $Destination
    Copy-Item -Recurse -Force -Path (Join-Path $Source "*") -Destination $Destination -ErrorAction SilentlyContinue
}

function Copy-LuaPackagesRuntimeTree {
    <#
    .SYNOPSIS
    Copy only Lua package runtime directories into the package.
    仅将 Lua package 运行期目录复制到产物包。

    .PARAMETER LuaPackagesDir
    Source Lua package tree under third_party.
    third_party 下的 Lua package 源目录。

    .PARAMETER RuntimeRoot
    Runtime package root directory.
    runtime 包根目录。
    #>
    param(
        [string]$LuaPackagesDir,
        [string]$RuntimeRoot
    )

    $RuntimeLuaPackages = Join-Path $RuntimeRoot "lua_packages"
    Copy-LuaPackageRuntimeDirectory -Source (Join-Path $LuaPackagesDir "lib\lua") -Destination (Join-Path $RuntimeLuaPackages "lib\lua")
    Copy-LuaPackageRuntimeDirectory -Source (Join-Path $LuaPackagesDir "share\lua") -Destination (Join-Path $RuntimeLuaPackages "share\lua")
}

function Copy-LuaPackageRuntimeDirectory {
    <#
    .SYNOPSIS
    Flatten Lua 5.1 ABI package directory into the runtime default layout.
    将 Lua 5.1 ABI package 目录扁平化到 runtime 默认布局。
    #>
    param(
        [string]$Source,
        [string]$Destination
    )

    if (-not (Test-Path -LiteralPath $Source)) {
        return
    }

    Ensure-Dir $Destination
    $VersionedSource = Join-Path $Source "5.1"
    if (Test-Path -LiteralPath $VersionedSource) {
        Copy-Item -Recurse -Force -Path (Join-Path $VersionedSource "*") -Destination $Destination -ErrorAction SilentlyContinue
    }

    Get-ChildItem -Force -LiteralPath $Source -ErrorAction SilentlyContinue |
        Where-Object { $_.Name -ne "5.1" } |
        ForEach-Object {
            Copy-Item -Recurse -Force -LiteralPath $_.FullName -Destination $Destination
        }
}

function Copy-NativeRuntimeLibraries {
    <#
    .SYNOPSIS
    Copy native runtime libraries and skip build-only LuaJIT compatibility files.
    复制原生运行库，并跳过仅用于构建的 LuaJIT 兼容文件。

    .PARAMETER DepsDir
    Source native dependency directory.
    原生依赖源目录。

    .PARAMETER RuntimeRoot
    Runtime package root directory.
    runtime 包根目录。
    #>
    param(
        [string]$DepsDir,
        [string]$RuntimeRoot
    )

    $LibsDir = Join-Path $RuntimeRoot "libs"
    Ensure-Dir $LibsDir

    if (-not (Test-Path -LiteralPath $DepsDir)) {
        return
    }

    Get-ChildItem -Recurse -File -Path $DepsDir -ErrorAction SilentlyContinue | Where-Object {
        Test-NativeRuntimeLibraryName -Name $_.Name
    } | ForEach-Object {
        $Name = $_.Name.ToLowerInvariant()
        if ($ExcludedRuntimeLibraryNames -contains $Name) {
            return
        }
        $Destination = Join-Path $LibsDir $_.Name
        Copy-Item -Force -LiteralPath $_.FullName -Destination $Destination
        Add-BundledLibraryRecord -SourcePath $_.FullName -DestinationPath $Destination
    }
}

function Test-NativeRuntimeLibraryName {
    <#
    .SYNOPSIS
    Check whether one file name is a supported native runtime library.
    检查一个文件名是否为受支持的原生运行时库。

    .PARAMETER Name
    File name evaluated once after directory enumeration.
    目录枚举后仅评估一次的文件名。

    .OUTPUTS
    Boolean value indicating whether the file belongs to the runtime dependency queue.
    表示文件是否属于运行时依赖队列的布尔值。
    #>
    param([string]$Name)

    $LowerName = $Name.ToLowerInvariant()
    return $LowerName.EndsWith(".dll") -or
        $LowerName.EndsWith(".so") -or
        $LowerName.Contains(".so.") -or
        $LowerName.EndsWith(".dylib")
}

function Test-BundledNativeDependencyName {
    <#
    .SYNOPSIS
    Check whether a native dependency name should be bundled into runtime libs.
    检查原生依赖名称是否应该打入 runtime libs。

    .PARAMETER Name
    File name to test against the allowlist.
    需要匹配白名单的文件名。

    .OUTPUTS
    Boolean value indicating whether the file should be copied.
    表示该文件是否需要复制的布尔值。
    #>
    param([string]$Name)

    foreach ($Pattern in $BundledNativeDependencyPatterns) {
        if ($Name -like $Pattern) {
            return $true
        }
    }
    return $false
}

function Get-NativeDependencyComponent {
    <#
    .SYNOPSIS
    Map a native library filename to its component name.
    将原生库文件名映射到组件名称。
    #>
    param([string]$Name)

    $Lower = $Name.ToLowerInvariant()
    if ($Lower -like "libz.so*" -or $Lower -like "zlib*.dll" -or $Lower -like "libz*.dylib") { return "zlib" }
    if ($Lower -like "libcurl.so*" -or $Lower -like "libcurl*.dll" -or $Lower -like "libcurl*.dylib") { return "curl" }
    if ($Lower -like "libssl.so*" -or $Lower -like "libssl*.dll" -or $Lower -like "libssl*.dylib" -or $Lower -like "libcrypto.so*" -or $Lower -like "libcrypto*.dll" -or $Lower -like "libcrypto*.dylib") { return "openssl" }
    if ($Lower -like "libpcre2-*.so*" -or $Lower -like "pcre2*.dll" -or $Lower -like "libpcre2-*.dylib") { return "pcre2" }
    if ($Lower -like "libyaml*.so*" -or $Lower -like "yaml*.dll" -or $Lower -like "libyaml*.dylib") { return "libyaml" }
    return "unknown"
}

function Add-BundledLibraryRecord {
    <#
    .SYNOPSIS
    Record one copied runtime library source path for manifests and license references.
    记录一个已复制运行库的来源路径，用于清单与授权引用。
    #>
    param(
        [string]$SourcePath,
        [string]$DestinationPath
    )

    $Name = Split-Path -Leaf $DestinationPath
    $script:BundledLibraries.Add([ordered]@{
        name = $Name
        component = Get-NativeDependencyComponent -Name $Name
        source_path = $SourcePath
    }) | Out-Null
}

function Get-LinkedDependencyPaths {
    <#
    .SYNOPSIS
    Read linked native dependency paths from ldd or otool.
    通过 ldd 或 otool 读取已链接的原生依赖路径。

    .PARAMETER BinaryPath
    Native binary to inspect.
    需要检查的原生二进制文件。

    .OUTPUTS
    Absolute file paths reported by the platform dependency tool.
    平台依赖工具报告的绝对文件路径。
    #>
    param([string]$BinaryPath)

    $Ldd = Get-Command ldd -ErrorAction SilentlyContinue
    if ($Ldd) {
        & $Ldd.Source $BinaryPath 2>$null | ForEach-Object {
            $Line = $_.Trim()
            if ($Line -match '=>\s+(/\S+)') {
                $Matches[1]
            } elseif ($Line -match '^(/\S+)') {
                $Matches[1]
            }
        }
        return
    }

    $Otool = Get-Command otool -ErrorAction SilentlyContinue
    if ($Otool) {
        & $Otool.Source -L $BinaryPath 2>$null | Select-Object -Skip 1 | ForEach-Object {
            $Line = $_.Trim()
            if ($Line -match '^(/\S+)') {
                $Matches[1]
            }
        }
    }
}

function Copy-LinkedRuntimeDependencies {
    <#
    .SYNOPSIS
    Iteratively copy allowlisted linked system libraries into runtime libs.
    迭代复制白名单内的已链接系统库到 runtime libs。

    .PARAMETER ScanRoots
    Distinct directory roots containing native binaries to inspect as one closure.
    作为单一闭包检查、包含原生二进制文件的不同目录根集合。

    .PARAMETER LibsDir
    Destination libs directory.
    目标 libs 目录。
    #>
    param(
        [string[]]$ScanRoots,
        [string]$LibsDir
    )

    Ensure-Dir $LibsDir
    $Queue = New-Object 'System.Collections.Generic.Queue[string]'
    $Seen = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::OrdinalIgnoreCase)
    # Canonical root set preventing nested runtime libs from becoming a duplicate scan entry.
    # 防止嵌套 runtime libs 成为重复扫描入口的规范根集合。
    $RootSeen = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::OrdinalIgnoreCase)

    foreach ($Root in $ScanRoots) {
        if (-not (Test-Path -LiteralPath $Root)) {
            continue
        }
        $CanonicalRoot = (Resolve-Path -LiteralPath $Root).Path
        if (-not $RootSeen.Add($CanonicalRoot)) {
            continue
        }
        Get-ChildItem -Recurse -File -LiteralPath $CanonicalRoot -ErrorAction SilentlyContinue | Where-Object {
            Test-NativeRuntimeLibraryName -Name $_.Name
        } | ForEach-Object {
            $Queue.Enqueue($_.FullName)
        }
    }

    while ($Queue.Count -gt 0) {
        $BinaryPath = $Queue.Dequeue()
        if (-not (Test-Path -LiteralPath $BinaryPath)) {
            continue
        }
        $CanonicalBinaryPath = (Resolve-Path -LiteralPath $BinaryPath).Path
        if (-not $Seen.Add($CanonicalBinaryPath)) {
            continue
        }

        foreach ($DependencyPath in (Get-LinkedDependencyPaths -BinaryPath $CanonicalBinaryPath)) {
            if (-not $DependencyPath -or -not (Test-Path -LiteralPath $DependencyPath)) {
                continue
            }
            $DependencyName = Split-Path -Leaf $DependencyPath
            if (Test-BundledNativeDependencyName -Name $DependencyName) {
                $Destination = Join-Path $LibsDir $DependencyName
                if (-not (Test-Path -LiteralPath $Destination)) {
                    Copy-Item -Force -LiteralPath $DependencyPath -Destination $Destination
                    Add-BundledLibraryRecord -SourcePath $DependencyPath -DestinationPath $Destination
                    $Queue.Enqueue($Destination)
                }
            }
        }
    }
}

function Copy-LicenseCandidates {
    <#
    .SYNOPSIS
    Copy available license-like files for one component into the package.
    将某个组件可发现的授权文件复制到产物包。

    .PARAMETER ComponentName
    Component directory name under licenses.
    licenses 下的组件目录名。

    .PARAMETER SearchRoots
    Directories to scan for license files.
    需要扫描授权文件的目录集合。

    .PARAMETER LicenseRoot
    Package license root directory.
    产物包授权根目录。
    #>
    param(
        [string]$ComponentName,
        [string[]]$SearchRoots,
        [string]$LicenseRoot
    )

    $ComponentDir = Join-Path $LicenseRoot $ComponentName
    Ensure-Dir $ComponentDir
    $ResolvedLicenseRoot = (Resolve-Path -LiteralPath $LicenseRoot).Path.TrimEnd([System.IO.Path]::DirectorySeparatorChar, [System.IO.Path]::AltDirectorySeparatorChar)
    $ResolvedComponentDir = (Resolve-Path -LiteralPath $ComponentDir).Path.TrimEnd([System.IO.Path]::DirectorySeparatorChar, [System.IO.Path]::AltDirectorySeparatorChar)
    $LicenseRootPrefix = $ResolvedLicenseRoot + [System.IO.Path]::DirectorySeparatorChar
    $ComponentDirPrefix = $ResolvedComponentDir + [System.IO.Path]::DirectorySeparatorChar

    foreach ($SearchRoot in $SearchRoots) {
        if (-not (Test-Path -LiteralPath $SearchRoot)) {
            continue
        }

        Get-ChildItem -Recurse -File -Path $SearchRoot -Depth 5 -ErrorAction SilentlyContinue |
            Where-Object { $_.Name -match '^(LICENSE|LICENCE|COPYING|NOTICE)(\.|$)' } |
            ForEach-Object {
                $SourceFullPath = [System.IO.Path]::GetFullPath($_.FullName)
                $DestinationPath = Join-Path $ComponentDir $_.Name
                $DestinationFullPath = [System.IO.Path]::GetFullPath($DestinationPath)
                $CopiesIntoItself = $SourceFullPath.Equals($DestinationFullPath, [System.StringComparison]::OrdinalIgnoreCase)
                $CopiesFromPackageLicenses = $SourceFullPath.StartsWith($ComponentDirPrefix, [System.StringComparison]::OrdinalIgnoreCase) -or $SourceFullPath.StartsWith($LicenseRootPrefix, [System.StringComparison]::OrdinalIgnoreCase)
                if (-not $CopiesIntoItself -and -not $CopiesFromPackageLicenses) {
                    Copy-Item -Force -LiteralPath $_.FullName -Destination $DestinationPath
                }
            }
    }
}

function Save-OfficialLicense {
    <#
    .SYNOPSIS
    Download one official license file for a fixed native dependency.
    为固定原生依赖下载一个官方授权文件。
    #>
    param(
        [string]$ComponentName,
        [string]$FileName,
        [string]$Url,
        [string]$LicenseRoot
    )

    $ComponentDir = Join-Path $LicenseRoot ("native\" + $ComponentName)
    Ensure-Dir $ComponentDir
    $Destination = Join-Path $ComponentDir $FileName
    Invoke-WebRequest -Uri $Url -OutFile $Destination -UseBasicParsing
    Set-Content -Path (Join-Path $ComponentDir "$FileName.url.txt") -Value $Url -Encoding UTF8
}

function Save-OfficialNativeLicenses {
    <#
    .SYNOPSIS
    Always include official licenses for the fixed native dependency set.
    始终为固定原生依赖集合带入官方授权文件。
    #>
    param([string]$LicenseRoot)

    Save-OfficialLicense -ComponentName "openssl" -FileName "LICENSE.official.txt" -Url "https://raw.githubusercontent.com/openssl/openssl/openssl-3.4.1/LICENSE.txt" -LicenseRoot $LicenseRoot
    Save-OfficialLicense -ComponentName "curl" -FileName "COPYING.official.txt" -Url "https://raw.githubusercontent.com/curl/curl/curl-8_13_0/COPYING" -LicenseRoot $LicenseRoot
    Save-OfficialLicense -ComponentName "zlib" -FileName "LICENSE.official.txt" -Url "https://raw.githubusercontent.com/madler/zlib/v1.3.1/LICENSE" -LicenseRoot $LicenseRoot
    Save-OfficialLicense -ComponentName "pcre2" -FileName "LICENCE.official.md" -Url "https://raw.githubusercontent.com/PCRE2Project/pcre2/pcre2-10.45/LICENCE.md" -LicenseRoot $LicenseRoot
    Save-OfficialLicense -ComponentName "libyaml" -FileName "License.official" -Url "https://raw.githubusercontent.com/yaml/libyaml/0.2.5/License" -LicenseRoot $LicenseRoot
}

function Write-LicenseReferenceIfMissing {
    <#
    .SYNOPSIS
    Write a license reference when the copied system library has no nearby license file.
    当复制的系统库没有随源目录提供授权文件时写入授权引用。
    #>
    param(
        [string]$ComponentName,
        [string]$SourcePath,
        [string]$LicenseRoot
    )

    if (-not $ComponentName -or $ComponentName -eq "unknown") {
        return
    }

    $ComponentDir = Join-Path $LicenseRoot ("native\" + $ComponentName)
    Ensure-Dir $ComponentDir
    $Existing = Get-ChildItem -File -Path $ComponentDir -ErrorAction SilentlyContinue |
        Where-Object { $_.Name -match '^(LICENSE|LICENCE|COPYING|NOTICE|README)(\.|$)' } |
        Select-Object -First 1
    if ($Existing) {
        return
    }

    $License = switch ($ComponentName) {
        "openssl" { "Apache-2.0" }
        "curl" { "curl" }
        "zlib" { "Zlib" }
        "pcre2" { "BSD-3-Clause" }
        "libyaml" { "MIT" }
        default { "See upstream project" }
    }

    @"
Component: $ComponentName
License: $License
Bundled library source path: $SourcePath

No license file was found next to the copied system library during packaging.
This package records the upstream license identifier and the source path used by the build runner.
"@ | Set-Content -Path (Join-Path $ComponentDir "LICENSE.reference.txt") -Encoding UTF8
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

function Write-RuntimeEnvScripts {
    <#
    .SYNOPSIS
    Write helper scripts that let hosts include runtime/libs in the native loader path.
    写入帮助宿主把 runtime/libs 加入原生加载路径的辅助脚本。

    .PARAMETER RuntimeRoot
    Runtime package root that receives the helper script.
    接收辅助脚本的 runtime 包根目录。

    .PARAMETER Platform
    Target package platform used to choose PowerShell or shell helpers.
    用于选择 PowerShell 或 shell 辅助脚本的目标包平台。
    #>
    param(
        [string]$RuntimeRoot,
        [string]$Platform
    )

    $ResourcesDir = Join-Path $RuntimeRoot "resources"
    Ensure-Dir $ResourcesDir

    if (Test-WindowsPackagePlatform -PlatformKey $Platform) {
        @'
$RuntimeRoot = if ($env:RUNTIME_ROOT) { $env:RUNTIME_ROOT } else { Split-Path -Parent $PSScriptRoot }
$Libs = Join-Path $RuntimeRoot "libs"
$env:PATH = "$Libs;$env:PATH"
'@ | Set-Content -Path (Join-Path $ResourcesDir "runtime-env.ps1") -Encoding UTF8
        return
    }

    @'
#!/usr/bin/env bash
RUNTIME_ROOT="${RUNTIME_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
case "$(uname -s)" in
  Darwin) export DYLD_LIBRARY_PATH="$RUNTIME_ROOT/libs${DYLD_LIBRARY_PATH:+:$DYLD_LIBRARY_PATH}" ;;
  Linux) export LD_LIBRARY_PATH="$RUNTIME_ROOT/libs${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}" ;;
esac
'@ | Set-Content -Path (Join-Path $ResourcesDir "runtime-env.sh") -Encoding UTF8
}

function Write-JsonFile {
    <#
    .SYNOPSIS
    Write one object as pretty JSON.
    将对象以格式化 JSON 写入文件。

    .PARAMETER Path
    Destination JSON file path.
    目标 JSON 文件路径。

    .PARAMETER Value
    Object to serialize.
    需要序列化的对象。
    #>
    param(
        [string]$Path,
        [object]$Value
    )

    ConvertTo-Json -InputObject $Value -Depth 12 | Set-Content -Path $Path -Encoding UTF8
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

$ThirdPartyPath = Resolve-Path -LiteralPath $ThirdPartyDir -ErrorAction SilentlyContinue
if (-not $ThirdPartyPath) {
    throw "Third-party directory not found: $ThirdPartyDir"
}

$RuntimeRoot = Join-Path $StagingDir "lua-runtime"
if (Test-Path -LiteralPath $RuntimeRoot) {
    Remove-Item -LiteralPath $RuntimeRoot -Recurse -Force
}

Ensure-Dir $RuntimeRoot
Ensure-Dir (Join-Path $RuntimeRoot "resources")
Ensure-Dir (Join-Path $RuntimeRoot "licenses")
Ensure-Dir $OutputDir

Copy-LuaPackagesRuntimeTree -LuaPackagesDir (Join-Path $ThirdPartyPath "lua_packages") -RuntimeRoot $RuntimeRoot
Copy-NativeRuntimeLibraries -DepsDir (Join-Path $ThirdPartyPath "deps") -RuntimeRoot $RuntimeRoot
Copy-LinkedRuntimeDependencies -ScanRoots @($RuntimeRoot, (Join-Path $ProjectRoot "target\release")) -LibsDir (Join-Path $RuntimeRoot "libs")

Write-RuntimeEnvScripts -RuntimeRoot $RuntimeRoot -Platform $Platform
Copy-LicenseCandidates -ComponentName "luaskills" -SearchRoots @($ProjectRoot) -LicenseRoot (Join-Path $RuntimeRoot "licenses")

$NativeLicenseRoots = @(
    @{ name = "openssl"; roots = @("openssl-*", "deps\openssl", "target\lua_deps_build\openssl") },
    @{ name = "curl"; roots = @("curl-*", "deps\curl", "target\lua_deps_build\curl") },
    @{ name = "zlib"; roots = @("zlib-*", "deps\zlib", "target\lua_deps_build\zlib") },
    @{ name = "pcre2"; roots = @("pcre2-*", "deps\pcre2", "target\lua_deps_build\pcre2") },
    @{ name = "libyaml"; roots = @("yaml-*", "libyaml-*", "deps\libyaml", "target\lua_deps_build\libyaml") }
)

foreach ($Component in $NativeLicenseRoots) {
    $Roots = @()
    foreach ($RootPattern in $Component.roots) {
        if ($RootPattern -like "*\*" -or $RootPattern -like "*/*") {
            $ProjectCandidate = Join-Path $ProjectRoot $RootPattern
            if (Test-Path -LiteralPath $ProjectCandidate) {
                $Roots += $ProjectCandidate
            }
            $ThirdPartyCandidate = Join-Path $ThirdPartyPath $RootPattern
            if (Test-Path -LiteralPath $ThirdPartyCandidate) {
                $Roots += $ThirdPartyCandidate
            }
        } else {
            $Roots += Get-ChildItem -Path $ProjectRoot -Directory -Filter $RootPattern -ErrorAction SilentlyContinue | ForEach-Object { $_.FullName }
            $Candidate = Join-Path $ThirdPartyPath $RootPattern
            if (Test-Path -LiteralPath $Candidate) {
                $Roots += $Candidate
            }
        }
    }
    Copy-LicenseCandidates -ComponentName ("native\" + $Component.name) -SearchRoots $Roots -LicenseRoot (Join-Path $RuntimeRoot "licenses")
}

Save-OfficialNativeLicenses -LicenseRoot (Join-Path $RuntimeRoot "licenses")

foreach ($Library in ($script:BundledLibraries | Sort-Object name, component, source_path -Unique)) {
    Write-LicenseReferenceIfMissing -ComponentName $Library.component -SourcePath $Library.source_path -LicenseRoot (Join-Path $RuntimeRoot "licenses")
}

$RuntimeManifest = [ordered]@{
    schema_version = 1
    package_name = "lua-runtime-$Platform"
    platform = $Platform
    layout = "luaskills-runtime-v1"
    exports = @("lua_packages/lib/lua", "lua_packages/share/lua", "libs", "resources", "licenses")
    packages_manifest = "resources/luaskills-packages-manifest.json"
    loader_env = [ordered]@{
        linux = "LD_LIBRARY_PATH=<runtime>/libs"
        macos = "DYLD_LIBRARY_PATH=<runtime>/libs"
        windows = "PATH=<runtime>\libs;%PATH%"
    }
    excludes = @("third_party/tools", "third_party/luajit", "lua51.dll", "luajit.exe", "build directories")
}

$LicenseManifest = [ordered]@{
    schema_version = 1
    package_name = "lua-runtime-$Platform"
    components = @(
        @{ name = "luaskills"; type = "runtime"; license = "MIT"; license_files = @("licenses/luaskills/LICENSE") },
        @{ name = "openssl"; type = "native-lib"; license = "Apache-2.0"; license_files = @("licenses/native/openssl") },
        @{ name = "curl"; type = "native-lib"; license = "curl"; license_files = @("licenses/native/curl") },
        @{ name = "zlib"; type = "native-lib"; license = "Zlib"; license_files = @("licenses/native/zlib") },
        @{ name = "pcre2"; type = "native-lib"; license = "BSD-3-Clause"; license_files = @("licenses/native/pcre2") },
        @{ name = "libyaml"; type = "native-lib"; license = "MIT"; license_files = @("licenses/native/libyaml") },
        @{ name = "luaskills-packages"; type = "lua-packages"; license = "per-bundle-metadata"; license_files = @("resources/luaskills-packages/THIRD_PARTY_LICENSES.json", "licenses/luaskills-packages") }
    )
}

Write-JsonFile -Path (Join-Path $RuntimeRoot "resources\lua-runtime-manifest.json") -Value $RuntimeManifest
Write-JsonFile -Path (Join-Path $RuntimeRoot "resources\bundled-libs.json") -Value @($script:BundledLibraries | Sort-Object name, component, source_path -Unique)
Write-JsonFile -Path (Join-Path $RuntimeRoot "licenses\manifest.json") -Value $LicenseManifest

# Generate the runtime-facing luaskills-packages metadata tree after license manifests exist.
# 在授权清单就绪后生成面向运行时的 luaskills-packages 元数据目录树。
& python (Join-Path $ProjectRoot "scripts\build\generate_runtime_packages_metadata.py") `
    --project-root $ProjectRoot `
    --runtime-root $RuntimeRoot `
    --platform $Platform
# MetadataExitCode captures metadata generation failure before any archive is published.
# MetadataExitCode 在发布任何归档前捕获元数据生成失败。
$MetadataExitCode = $LASTEXITCODE
if ($MetadataExitCode -ne 0) {
    throw "Failed to generate runtime package metadata (exit $MetadataExitCode)"
}

$ArchiveName = "lua-runtime-$Platform.tar.gz"
$ArchivePath = Join-Path $OutputDir $ArchiveName
if (Test-Path -LiteralPath $ArchivePath) {
    Remove-Item -LiteralPath $ArchivePath -Force
}

$ResolvedOutput = (Resolve-Path -LiteralPath $OutputDir).Path
New-TarFromDirectory -SourceDir $RuntimeRoot -ArchivePath (Join-Path $ResolvedOutput $ArchiveName)

Write-Host "Lua runtime package created: $ArchivePath"
