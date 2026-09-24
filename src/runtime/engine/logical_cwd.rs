//! System lease paths are resolved without changing the process working directory.
//! 系统租约路径解析不改变进程工作目录。

use super::*;

/// Immutable host-owned directory of one System VM; never sourced from Lua globals.
/// 单个系统虚拟机不可变的宿主目录；绝不从 Lua 全局变量读取。
#[derive(Clone)]
pub(crate) struct LogicalCwd(pub(crate) PathBuf);

/// Monotonic deadline attached to a System evaluation, unavailable to writable Lua globals.
/// 附加到系统执行的单调截止时间，不存放在可写 Lua 全局变量中。
pub(crate) struct EvaluationDeadline(pub(crate) Instant);

/// Returns the remaining System budget or rejects an expired invocation before host work begins.
/// 返回剩余系统预算，或在宿主任务开始前拒绝已到期调用。
/// Ordinary VMs without an evaluation deadline return none.
/// 没有执行截止时间的普通虚拟机返回空值。
pub(crate) fn remaining_timeout_ms(lua: &Lua) -> mlua::Result<Option<u64>> {
    let Some(deadline) = lua.app_data_ref::<EvaluationDeadline>() else {
        return Ok(None);
    };
    let remaining = deadline
        .0
        .saturating_duration_since(Instant::now())
        .as_millis();
    if remaining == 0 {
        return Err(mlua::Error::runtime("System evaluation deadline exceeded."));
    }
    Ok(Some(remaining.min(u64::MAX as u128) as u64))
}

/// Returns the System directory, or none for ordinary VMs that retain their existing contract.
/// 返回系统目录；保留原契约的普通虚拟机返回空值。
pub(crate) fn directory(lua: &Lua) -> Option<PathBuf> {
    lua.app_data_ref::<LogicalCwd>().map(|cwd| cwd.0.clone())
}

/// Resolves a validated path against an explicit base, rejecting drive-relative Windows paths.
/// 基于明确基准解析已校验路径，并拒绝 Windows 盘符相对路径。
/// Returns an absolute path without consulting process state.
/// 返回绝对路径，且不读取进程状态。
pub(crate) fn resolve(base: &Path, value: &str) -> mlua::Result<String> {
    let normalized = normalize_host_input_path_text(value).map_err(mlua::Error::runtime)?;
    let path = Path::new(&normalized);
    let resolved = if path.is_absolute() {
        path.to_path_buf()
    } else {
        if path.has_root() || matches!(path.components().next(), Some(Component::Prefix(_))) {
            return Err(mlua::Error::runtime(
                "System lease paths must be absolute or relative to their logical directory.",
            ));
        }
        base.join(path)
    };
    Ok(render_host_visible_path(&resolved))
}

/// Wraps the named API's path arguments while preserving its original validation and return tuple.
/// 包装指定接口的路径参数，同时保留原有校验与返回元组。
/// Non-string optional handles are passed to the owning API unchanged.
/// 可选的非字符串句柄原样传入所属接口。
fn wrap_paths(
    lua: &Lua,
    table: &Table,
    name: &'static str,
    indices: &'static [usize],
    base: &Path,
) -> mlua::Result<()> {
    let original: Function = table.get(name)?;
    let base = base.to_path_buf();
    table.set(
        name,
        lua.create_function(move |lua, mut args: MultiValue| {
            for &index in indices {
                if let Some(LuaValue::String(value)) = args.get(index) {
                    let resolved = resolve(&base, &value.to_str()?)?;
                    args[index] = LuaValue::String(lua.create_string(&resolved)?);
                }
            }
            original.call::<MultiValue>(args)
        })?,
    )
}

/// Rebases every module search template so a concurrent file execution cannot redirect loading.
/// 将每个模块搜索模板转为绝对路径，避免并发文件执行改变加载目标。
fn search_path(base: &Path, path: &str) -> mlua::Result<String> {
    path.split(';')
        .filter(|entry| !entry.is_empty())
        .map(|entry| resolve(base, entry))
        .collect::<mlua::Result<Vec<_>>>()
        .map(|entries| entries.join(";"))
}

/// Finds a module using explicit templates and Unicode-aware filesystem metadata.
/// 使用明确模板及支持 Unicode 的文件系统元数据定位模块。
/// Returns the resolved filename or Lua-compatible accumulated search diagnostics.
/// 返回解析后的文件名或符合 Lua 语义的累计查找诊断。
fn find_module(
    base: &Path,
    name: &str,
    templates: &str,
    separator: &str,
    replacement: &str,
) -> mlua::Result<(Option<String>, Option<String>)> {
    let module = name.replace(separator, replacement);
    let mut errors = String::new();
    for template in templates.split(';').filter(|entry| !entry.is_empty()) {
        let path = resolve(base, &template.replace('?', &module))?;
        if Path::new(&path).is_file() {
            return Ok((Some(path), None));
        }
        errors.push_str(&format!("\n\tno file '{path}'"));
    }
    Ok((None, Some(errors)))
}

/// Reads a script through Unicode-aware host APIs and compiles it in the requesting VM.
/// 通过支持 Unicode 的宿主接口读取脚本，并在请求虚拟机中编译。
/// The explicit path is resolved against the lease; mode and environment retain Lua load semantics.
/// 明确路径按租约解析；模式和环境保留 Lua 加载语义。
fn load_script(
    lua: &Lua,
    base: &Path,
    path: &str,
    mode: Option<String>,
    env: Option<Table>,
) -> mlua::Result<Function> {
    let path = resolve(base, path)?;
    let bytes = std::fs::read(&path).map_err(mlua::Error::external)?;
    let mut chunk = lua.load(bytes).set_name(format!("@{path}"));
    match mode.as_deref().unwrap_or("bt") {
        "bt" | "tb" => {}
        "b" => chunk = chunk.set_mode(mlua::chunk::ChunkMode::Binary),
        "t" => chunk = chunk.set_mode(mlua::chunk::ChunkMode::Text),
        _ => return Err(mlua::Error::runtime("Invalid script load mode.")),
    }
    if let Some(env) = env {
        chunk = chunk.set_environment(env);
    }
    chunk.into_function()
}

/// Installs logical filesystem and loader boundaries once, before any System package code executes.
/// 在任何系统包代码执行前，一次性安装逻辑文件系统和加载边界。
/// `base` is the already authorized canonical lease directory; errors reject lease creation.
/// `base` 是已获授权的规范租约目录；失败拒绝创建租约。
pub(super) fn install(lua: &Lua, base: &Path, encoding: RuntimeTextEncoding) -> mlua::Result<()> {
    lua.set_app_data(LogicalCwd(base.to_path_buf()));
    let vulcan: Table = lua.globals().get("vulcan")?;
    let runtime: Table = vulcan.get("runtime")?;
    runtime.set(
        "remaining_timeout_ms",
        lua.create_function(|lua, ()| remaining_timeout_ms(lua))?,
    )?;
    let fs: Table = vulcan.get("fs")?;
    for name in [
        "list",
        "read",
        "write",
        "write_bytes",
        "remove",
        "mkdir",
        "stat",
        "read_bytes",
        "exists",
        "is_dir",
    ] {
        wrap_paths(lua, &fs, name, &[0], base)?;
    }
    for name in ["rename", "copy"] {
        wrap_paths(lua, &fs, name, &[0, 1], base)?;
    }
    let io: Table = vulcan.get("io")?;
    for name in ["open", "read_text", "write_text", "append_text", "lines"] {
        wrap_paths(lua, &io, name, &[0], base)?;
    }
    // Native stdio retains relative filenames internally; System leases always use managed handles.
    // 原生标准输入输出会在内部保留相对文件名；系统租约始终使用受管句柄。
    install_managed_io_compat(lua, &io, encoding)?;
    let compat: Table = lua.globals().get("io")?;
    for name in ["input", "output"] {
        wrap_paths(lua, &compat, name, &[0], base)?;
    }
    let globals = lua.globals();
    let load_base = base.to_path_buf();
    globals.set("loadfile", lua.create_function(move |lua, (path, mode, env): (String, Option<String>, Option<Table>)| {
        match load_script(lua, &load_base, &path, mode, env) {
            Ok(function) => Ok((Some(function), None)),
            Err(error) => Ok((None, Some(error.to_string()))),
        }
    })?)?;
    let do_base = base.to_path_buf();
    globals.set(
        "dofile",
        lua.create_function(move |lua, path: String| {
            load_script(lua, &do_base, &path, None, None)?.call::<MultiValue>(())
        })?,
    )?;
    let os: Table = globals.get("os")?;
    wrap_paths(lua, &os, "remove", &[0], base)?;
    wrap_paths(lua, &os, "rename", &[0, 1], base)?;
    // Unmanaged native commands and process mutations cannot satisfy this lease contract.
    // 非受管原生命令和进程状态修改不能满足此租约契约。
    for name in ["execute", "exit", "setlocale", "tmpname"] {
        os.set(
            name,
            lua.create_function(move |_, _: MultiValue| -> mlua::Result<()> {
                Err(mlua::Error::runtime(format!(
                    "os.{name} is unavailable in System leases; use managed host services."
                )))
            })?,
        )?;
    }
    let package: Table = globals.get("package")?;
    for field in ["path", "cpath"] {
        let path: String = package.get(field)?;
        package.set(field, search_path(base, &path)?)?;
    }
    wrap_paths(lua, &package, "loadlib", &[0], base)?;
    let search_base = base.to_path_buf();
    package.set(
        "searchpath",
        lua.create_function(
            move |_,
                  (name, path, separator, replacement): (
                String,
                String,
                Option<String>,
                Option<String>,
            )| {
                find_module(
                    &search_base,
                    &name,
                    &path,
                    separator.as_deref().unwrap_or("."),
                    replacement
                        .as_deref()
                        .unwrap_or(std::path::MAIN_SEPARATOR_STR),
                )
            },
        )?,
    )?;
    // Rebase search strings at lookup time as packages may legitimately extend them later.
    // 包可能在之后合法扩展搜索路径，因此每次查找时重新解析。
    let loaders: Table = package.get("loaders")?;
    // Lua's native file loader uses narrow paths on Windows; replace only the script loader.
    // Lua 原生文件加载器在 Windows 使用窄字符路径；仅替换脚本加载器。
    let script_base = base.to_path_buf();
    let script_package = package.clone();
    loaders.set(
        2,
        lua.create_function(move |lua, name: String| {
            let templates: String = script_package.get("path")?;
            let (path, diagnostic) = find_module(
                &script_base,
                &name,
                &templates,
                ".",
                std::path::MAIN_SEPARATOR_STR,
            )?;
            match path {
                Some(path) => Ok(LuaValue::Function(load_script(
                    lua,
                    &script_base,
                    &path,
                    None,
                    None,
                )?)),
                None => Ok(LuaValue::String(
                    lua.create_string(diagnostic.unwrap_or_default())?,
                )),
            }
        })?,
    )?;
    for (index, loader) in loaders.clone().sequence_values::<Function>().enumerate() {
        let loader = loader?;
        let package = package.clone();
        let base = base.to_path_buf();
        loaders.set(
            index + 1,
            lua.create_function(move |_, args: MultiValue| {
                for field in ["path", "cpath"] {
                    let path: String = package.get(field)?;
                    package.set(field, search_path(&base, &path)?)?;
                }
                loader.call::<MultiValue>(args)
            })?,
        )?;
    }
    // FFI can call chdir directly, bypassing every path API; do not expose it to System packages.
    // FFI 可直接调用 chdir 绕过全部路径接口；不向系统包暴露。
    let loaded: Table = package.get("loaded")?;
    let preload: Table = package.get("preload")?;
    loaded.set("ffi", LuaValue::Nil)?;
    globals.set("ffi", LuaValue::Nil)?;
    preload.set("ffi", lua.create_function(|_, _: MultiValue| -> mlua::Result<()> {
        Err(mlua::Error::runtime("FFI is unavailable in System leases; use managed host services or an isolated process."))
    })?)?;
    Ok(())
}
