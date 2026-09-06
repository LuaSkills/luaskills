use std::{fs, path::PathBuf};

/// Resolve the repository root used by the launcher contract checks.
/// 解析启动器契约检查所使用的仓库根目录。
///
/// # Returns
/// Absolute repository root derived from Cargo's manifest directory.
/// 从 Cargo 清单目录派生的绝对仓库根目录。
fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Read one launcher source file for an offline contract check.
/// 读取一个启动器源码文件以执行离线契约检查。
///
/// # Parameters
/// - `relative_path`: Repository-relative launcher path.
/// - `relative_path`：相对于仓库根目录的启动器路径。
///
/// # Returns
/// UTF-8 launcher source text.
/// UTF-8 启动器源码文本。
fn read_launcher(relative_path: &str) -> String {
    // launcher_path is the exact source file covered by this contract test.
    // launcher_path 是本契约测试覆盖的确切源码文件。
    let launcher_path = repository_root().join(relative_path);
    fs::read_to_string(&launcher_path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", launcher_path.display()))
}

/// Verify every source demo launcher resolves the maintained dependency fetch entrypoint.
/// 验证每个源码 demo 启动器都解析到受维护的依赖拉取入口。
#[test]
fn source_demo_launchers_reference_existing_dependency_fetchers() {
    // launchers maps each launcher to the platform-specific argument binding it must preserve.
    // launchers 将每个启动器映射到必须保留的平台参数绑定。
    const LAUNCHERS: [(&str, &str, &str); 4] = [
        (
            "examples/demo-ffi/run.ps1",
            "scripts\\deps\\fetch_deps.ps1",
            "-Target $Fetch -RuntimeRoot $RuntimeRoot",
        ),
        (
            "examples/demo-ffi/run.sh",
            "scripts/deps/fetch_deps.sh",
            "\"$PROJECT_ROOT/scripts/deps/fetch_deps.sh\" \"$TARGET\"",
        ),
        (
            "examples/demo-rust/run.ps1",
            "scripts\\deps\\fetch_deps.ps1",
            "-Target $Fetch -RuntimeRoot $RuntimeRoot",
        ),
        (
            "examples/demo-rust/run.sh",
            "scripts/deps/fetch_deps.sh",
            "\"$PROJECT_ROOT/scripts/deps/fetch_deps.sh\" \"$TARGET\"",
        ),
    ];
    // supported_targets is the complete launcher-level dependency target contract.
    // supported_targets 是完整的启动器级依赖目标契约。
    const SUPPORTED_TARGETS: [&str; 4] = ["none", "all", "lua", "vldb"];

    // Each tuple verifies both path resolution and unchanged parameter forwarding without network access.
    // 每个元组均在不访问网络的情况下验证路径解析和参数透传保持不变。
    for (relative_path, fetch_path, argument_binding) in LAUNCHERS {
        // source is the launcher text inspected by the offline smoke test.
        // source 是离线冒烟测试检查的启动器文本。
        let source = read_launcher(relative_path);
        assert!(
            source.contains(fetch_path),
            "{relative_path} must reference {fetch_path}"
        );
        assert!(
            source.contains(argument_binding),
            "{relative_path} must preserve dependency target binding"
        );
        assert!(
            !source.contains("fetch_runtime_deps"),
            "{relative_path} must not reference the removed fetch_runtime_deps entrypoint"
        );

        // PowerShell declares the target set locally; shell launchers forward the same target unchanged.
        // PowerShell 在本地声明目标集合；shell 启动器将同一目标原样透传。
        for supported_target in SUPPORTED_TARGETS {
            if relative_path.ends_with(".ps1") {
                assert!(
                    source.contains(&format!("\"{supported_target}\"")),
                    "{relative_path} must accept target {supported_target}"
                );
            } else if supported_target == "none" {
                assert!(
                    source.contains("${1:-none}"),
                    "{relative_path} must default to the none target"
                );
            }
        }
    }

    assert!(
        repository_root()
            .join("scripts/deps/fetch_deps.ps1")
            .is_file(),
        "PowerShell dependency fetcher must exist"
    );
    assert!(
        repository_root()
            .join("scripts/deps/fetch_deps.sh")
            .is_file(),
        "shell dependency fetcher must exist"
    );
}

/// Verify every PowerShell packager imports one checked archive helper without local copies.
/// 验证每个 PowerShell 打包器导入同一个受检归档辅助脚本且不保留本地副本。
#[test]
fn powershell_packagers_share_checked_archive_helper() {
    // Complete PowerShell packager set covered by the shared archive contract.
    // 共享归档契约覆盖的完整 PowerShell 打包器集合。
    const PACKAGERS: [&str; 4] = [
        "scripts/build/package_demo.ps1",
        "scripts/build/package_ffi_sdk.ps1",
        "scripts/build/package_lua_runtime.ps1",
        "scripts/build/package_debug_tool.ps1",
    ];
    // Shared helper source containing the one authoritative tar invocation.
    // 包含唯一权威 tar 调用的共享辅助脚本源码。
    let helper_source = read_launcher("scripts/build/archive_helpers.ps1");
    assert!(helper_source.contains("function New-TarFromDirectory"));
    assert!(helper_source.contains("$LASTEXITCODE"));
    assert!(helper_source.contains("Failed to create archive"));

    for relative_path in PACKAGERS {
        // Packager source inspected for explicit import and removed local duplication.
        // 用于检查显式导入与本地重复已移除的打包器源码。
        let source = read_launcher(relative_path);
        assert!(
            source.contains("archive_helpers.ps1"),
            "{relative_path} must resolve the shared archive helper"
        );
        assert!(
            source.contains(". $ArchiveHelperPath"),
            "{relative_path} must import the shared archive helper"
        );
        assert!(
            !source.contains("function New-TarFromDirectory"),
            "{relative_path} must not retain a local archive helper copy"
        );
    }
}

/// Verify the C FFI demo creates a requested runtime root before Unix canonicalization requires it to exist.
/// 验证 C FFI 示例会在 Unix 规范化要求目录存在之前创建请求的运行根。
#[test]
fn c_demo_creates_runtime_root_before_unix_realpath() {
    // Source is the exact cross-platform C launcher implementation under contract.
    // Source 是契约覆盖的精确跨平台 C 启动器实现。
    let source = read_launcher("examples/ffi/c/demo.c");
    // CreatePosition identifies the new-root admission step shared by Windows and Unix.
    // CreatePosition 标识 Windows 与 Unix 共用的新根目录准入步骤。
    let create_position = source
        .find("ensure_directory(requested_runtime_root);")
        .expect("C demo must create the requested runtime root");
    // RealpathPosition identifies the Unix operation that rejects a missing target.
    // RealpathPosition 标识会拒绝缺失目标的 Unix 操作。
    let realpath_position = source
        .find("realpath(requested_runtime_root, resolved_runtime_root)")
        .expect("C demo must retain Unix canonicalization");

    assert!(
        create_position < realpath_position,
        "C demo must create a fresh runtime root before calling realpath"
    );
}
