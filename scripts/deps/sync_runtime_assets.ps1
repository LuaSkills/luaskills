param(
    # Asset group to synchronize into the runtime root.
    # 要同步到运行根目录的资产分组。
    [ValidateSet("all", "luaskills", "lua", "vldb")]
    [string]$Target = "all",
    # Optional VLDB integration preset used by all or vldb targets.
    # all 或 vldb 目标使用的可选 VLDB 集成预设。
    [ValidateSet("none", "vldb-controller", "vldb-direct", "host-callback")]
    [string]$Database = "vldb-controller",
    # Runtime root that receives all synchronized assets.
    # 接收全部同步资产的运行根目录。
    [string]$RuntimeRoot = "output",
    # Exact LuaSkills release tag used for the FFI SDK asset.
    # FFI SDK 资产使用的精确 LuaSkills 发布标签。
    [string]$LuaSkillsVersion = "v0.5.3",
    # Compatible Lua runtime packages release series.
    # 兼容的 Lua runtime packages 发布协议线。
    [string]$LuaRuntimeSeries = "0.1",
    # Optional exact Lua runtime packages release tag.
    # 可选的精确 Lua runtime packages 发布标签。
    [string]$LuaRuntimeVersion = "",
    # Exact vldb-controller release tag.
    # 精确的 vldb-controller 发布标签。
    [string]$VldbControllerVersion = "v0.2.3",
    # Exact vldb-sqlite release tag.
    # 精确的 vldb-sqlite 发布标签。
    [string]$VldbSQLiteVersion = "v0.1.6",
    # Exact vldb-lancedb release tag.
    # 精确的 vldb-lancedb 发布标签。
    [string]$VldbLanceDBVersion = "v0.1.5"
)

$ErrorActionPreference = "Stop"

# ScriptRoot identifies the shared dependency script directory.
# ScriptRoot 标识共享依赖脚本目录。
$ScriptRoot = $PSScriptRoot
# RepositoryRoot identifies the SDK or LuaSkills repository root.
# RepositoryRoot 标识 SDK 或 LuaSkills 仓库根目录。
$RepositoryRoot = (Resolve-Path -LiteralPath (Join-Path $ScriptRoot "..\..")).Path
# FfiScript identifies the canonical LuaSkills FFI synchronization implementation.
# FfiScript 标识 LuaSkills FFI 同步的规范实现。
$FfiScript = Join-Path $RepositoryRoot "scripts\ffi\fetch_ffi.ps1"
# DependenciesScript identifies the canonical Lua runtime and VLDB synchronization implementation.
# DependenciesScript 标识 Lua runtime 与 VLDB 同步的规范实现。
$DependenciesScript = Join-Path $ScriptRoot "fetch_deps.ps1"

if ($Target -eq "all" -or $Target -eq "luaskills") {
    & $FfiScript -RuntimeRoot $RuntimeRoot -LuaSkillsVersion $LuaSkillsVersion
}
if ($Target -eq "all" -or $Target -eq "lua") {
    & $DependenciesScript -Target lua -Database none -RuntimeRoot $RuntimeRoot -LuaRuntimeSeries $LuaRuntimeSeries -LuaRuntimeVersion $LuaRuntimeVersion
}
if ($Target -eq "all" -or $Target -eq "vldb") {
    & $DependenciesScript -Target vldb -Database $Database -RuntimeRoot $RuntimeRoot -VldbControllerVersion $VldbControllerVersion -VldbSQLiteVersion $VldbSQLiteVersion -VldbLanceDBVersion $VldbLanceDBVersion
}

Write-Host "Runtime asset target '$Target' synchronized into $RuntimeRoot"
