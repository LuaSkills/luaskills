use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::runtime::path::render_host_visible_path;
use crate::skill::dependencies::{
    NodeRuntimeDependencySpec, NodeRuntimePackageManager, PythonRuntimeDependencySpec,
    PythonRuntimePackageManager,
};

/// Schema version used by managed runtime environment markers.
/// 受管运行时环境标记文件使用的 schema 版本。
pub const MANAGED_RUNTIME_ENV_MARKER_SCHEMA_VERSION: u32 = 1;

/// Render one managed runtime filesystem path for user-facing error messages.
/// 为面向用户的受管运行时错误消息渲染单个文件系统路径。
fn render_managed_runtime_path(path: &Path) -> String {
    render_host_visible_path(path)
}

/// Managed child runtime kind that Lua can invoke through `vulcan.runtime.*`.
/// Lua 可通过 `vulcan.runtime.*` 调用的受管子运行时类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagedRuntimeKind {
    /// Managed Python runtime backed by a host-provided CPython bundle.
    /// 由宿主提供的 CPython 包支撑的受管 Python 运行时。
    Python,
    /// Managed Node.js runtime backed by a host-provided Node bundle.
    /// 由宿主提供的 Node 包支撑的受管 Node.js 运行时。
    Node,
}

impl ManagedRuntimeKind {
    /// Return the stable lowercase identifier used in paths and hashes.
    /// 返回用于路径与哈希的稳定小写标识符。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Python => "python",
            Self::Node => "node",
        }
    }
}

/// Request used to compute one reusable managed runtime environment identity.
/// 用于计算一个可复用受管运行时环境身份的请求。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedRuntimeEnvHashInput {
    /// Managed runtime kind used by the target environment.
    /// 目标环境使用的受管运行时类型。
    pub runtime: ManagedRuntimeKind,
    /// Exact interpreter runtime version.
    /// 精确解释器运行时版本。
    pub runtime_version: String,
    /// Normalized platform key such as `windows-x64`.
    /// 标准平台键，例如 `windows-x64`。
    pub platform: String,
    /// Package manager name such as `uv` or `pnpm`.
    /// 包管理器名称，例如 `uv` 或 `pnpm`。
    pub package_manager: String,
    /// Exact package-manager version.
    /// 精确包管理器版本。
    pub package_manager_version: String,
    /// SHA-256 digest of the dependency lockfile content.
    /// 依赖锁文件内容的 SHA-256 摘要。
    pub lock_hash: String,
    /// Optional SHA-256 digest of an additional package manifest such as package.json.
    /// package.json 等附加包清单的可选 SHA-256 摘要。
    pub package_manifest_hash: Option<String>,
}

/// Stable marker written into every managed runtime environment directory.
/// 写入每个受管运行时环境目录的稳定标记文件。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedRuntimeEnvMarker {
    /// Marker schema version.
    /// 标记文件 schema 版本。
    pub schema_version: u32,
    /// Managed runtime kind stored as a stable lowercase identifier.
    /// 以稳定小写标识符保存的受管运行时类型。
    pub runtime: String,
    /// Exact interpreter runtime version.
    /// 精确解释器运行时版本。
    pub runtime_version: String,
    /// Package manager name such as `uv` or `pnpm`.
    /// 包管理器名称，例如 `uv` 或 `pnpm`。
    pub package_manager: String,
    /// Exact package-manager version.
    /// 精确包管理器版本。
    pub package_manager_version: String,
    /// Normalized platform key such as `windows-x64`.
    /// 标准平台键，例如 `windows-x64`。
    pub platform: String,
    /// SHA-256 digest of the dependency lockfile content.
    /// 依赖锁文件内容的 SHA-256 摘要。
    pub lock_hash: String,
    /// Optional SHA-256 digest of an additional package manifest such as package.json.
    /// package.json 等附加包清单的可选 SHA-256 摘要。
    pub package_manifest_hash: Option<String>,
    /// Environment identity hash derived from all reproducibility inputs.
    /// 由全部可复现输入派生出的环境身份哈希。
    pub env_hash: String,
}

/// Manifest written next to each managed runtime or package-manager installation.
/// 写入每个受管运行时或包管理器安装目录旁的清单。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedRuntimeInstallManifest {
    /// Manifest schema version.
    /// 清单 schema 版本。
    pub schema_version: u32,
    /// Runtime or package-manager identifier such as `python`, `node`, `uv`, or `pnpm`.
    /// 运行时或包管理器标识，例如 `python`、`node`、`uv` 或 `pnpm`。
    pub runtime: String,
    /// Exact installed version.
    /// 已安装的精确版本。
    pub version: String,
    /// Platform key stored by the installation script.
    /// 安装脚本写入的平台键。
    pub platform: String,
    /// Executable path relative to the installation directory.
    /// 相对安装目录的可执行文件路径。
    pub executable: String,
}

/// Resolved managed runtime environment plan used before creation or invocation.
/// 创建环境或调用前使用的受管运行时环境解析计划。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedRuntimeEnvPlan {
    /// Managed runtime kind described by this plan.
    /// 当前计划描述的受管运行时类型。
    pub runtime: ManagedRuntimeKind,
    /// Current platform key.
    /// 当前平台键。
    pub platform: String,
    /// Canonical runtime root used to locate shared stores and runtime assets.
    /// 用于定位共享存储与运行时资产的规范运行时根目录。
    pub runtime_root: PathBuf,
    /// Exact interpreter runtime version.
    /// 精确解释器运行时版本。
    pub runtime_version: String,
    /// Absolute interpreter executable path.
    /// 解释器可执行文件绝对路径。
    pub runtime_executable: PathBuf,
    /// Package manager name such as `uv` or `pnpm`.
    /// 包管理器名称，例如 `uv` 或 `pnpm`。
    pub package_manager: String,
    /// Exact package-manager version.
    /// 精确包管理器版本。
    pub package_manager_version: String,
    /// Absolute package-manager executable path.
    /// 包管理器可执行文件绝对路径。
    pub package_manager_executable: PathBuf,
    /// Optional package manifest path such as package.json.
    /// package.json 等可选包清单路径。
    pub package_manifest_path: Option<PathBuf>,
    /// Required lockfile path.
    /// 必需的锁文件路径。
    pub lockfile_path: PathBuf,
    /// SHA-256 digest of the dependency lockfile content.
    /// 依赖锁文件内容的 SHA-256 摘要。
    pub lock_hash: String,
    /// Optional SHA-256 digest of an additional package manifest such as package.json.
    /// package.json 等附加包清单的可选 SHA-256 摘要。
    pub package_manifest_hash: Option<String>,
    /// Environment identity hash.
    /// 环境身份哈希。
    pub env_hash: String,
    /// Directory that stores this reusable environment.
    /// 保存当前可复用环境的目录。
    pub env_dir: PathBuf,
    /// Marker expected after environment creation.
    /// 环境创建完成后期望写入的标记。
    pub expected_marker: ManagedRuntimeEnvMarker,
}

impl ManagedRuntimeEnvMarker {
    /// Build one expected marker from a hash input and precomputed environment hash.
    /// 基于哈希输入与预计算环境哈希构造一个期望标记。
    pub fn expected(input: &ManagedRuntimeEnvHashInput, env_hash: String) -> Self {
        Self {
            schema_version: MANAGED_RUNTIME_ENV_MARKER_SCHEMA_VERSION,
            runtime: input.runtime.as_str().to_string(),
            runtime_version: input.runtime_version.clone(),
            package_manager: input.package_manager.clone(),
            package_manager_version: input.package_manager_version.clone(),
            platform: input.platform.clone(),
            lock_hash: input.lock_hash.clone(),
            package_manifest_hash: input.package_manifest_hash.clone(),
            env_hash,
        }
    }
}

impl PythonRuntimePackageManager {
    /// Return the stable lowercase package-manager identifier.
    /// 返回稳定小写包管理器标识符。
    fn as_managed_runtime_str(self) -> &'static str {
        match self {
            Self::Uv => "uv",
        }
    }
}

impl NodeRuntimePackageManager {
    /// Return the stable lowercase package-manager identifier.
    /// 返回稳定小写包管理器标识符。
    fn as_managed_runtime_str(self) -> &'static str {
        match self {
            Self::Pnpm => "pnpm",
        }
    }
}

/// Return the current platform key used by managed runtime assets.
/// 返回受管运行时资产使用的当前平台键。
pub fn current_managed_runtime_platform_key() -> Result<String, String> {
    let arch_key = match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        other => {
            return Err(format!(
                "unsupported managed runtime architecture: {}",
                other
            ));
        }
    };
    let os_key = match std::env::consts::OS {
        "windows" => {
            if arch_key != "x64" {
                return Err("managed runtime assets currently support Windows x64 only".to_string());
            }
            "windows"
        }
        "linux" => "linux",
        "macos" => "macos",
        other => {
            return Err(format!(
                "unsupported managed runtime operating system: {}",
                other
            ));
        }
    };
    Ok(format!("{}-{}", os_key, arch_key))
}

/// Compute the SHA-256 digest of one byte slice as lowercase hex.
/// 将一个字节切片计算为小写十六进制 SHA-256 摘要。
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    format!("{:x}", digest)
}

/// Compute the SHA-256 digest of one file.
/// 计算单个文件的 SHA-256 摘要。
pub fn sha256_file(path: &Path) -> Result<String, String> {
    let bytes = fs::read(path).map_err(|error| {
        format!(
            "Failed to read {}: {}",
            render_managed_runtime_path(path),
            error
        )
    })?;
    Ok(sha256_hex(&bytes))
}

/// Compute one stable environment hash from reproducibility inputs.
/// 根据可复现输入计算一个稳定环境哈希。
pub fn compute_managed_runtime_env_hash(input: &ManagedRuntimeEnvHashInput) -> String {
    let mut hasher = Sha256::new();
    hash_field(
        &mut hasher,
        "schema_version",
        MANAGED_RUNTIME_ENV_MARKER_SCHEMA_VERSION.to_string(),
    );
    hash_field(&mut hasher, "runtime", input.runtime.as_str());
    hash_field(&mut hasher, "runtime_version", &input.runtime_version);
    hash_field(&mut hasher, "platform", &input.platform);
    hash_field(&mut hasher, "package_manager", &input.package_manager);
    hash_field(
        &mut hasher,
        "package_manager_version",
        &input.package_manager_version,
    );
    hash_field(&mut hasher, "lock_hash", &input.lock_hash);
    hash_field(
        &mut hasher,
        "package_manifest_hash",
        input.package_manifest_hash.as_deref().unwrap_or(""),
    );
    format!("{:x}", hasher.finalize())
}

/// Return the managed environment root for one runtime version and environment hash.
/// 返回单个运行时版本与环境哈希对应的受管环境根目录。
pub fn managed_env_dir(
    runtime_root: &Path,
    runtime: ManagedRuntimeKind,
    runtime_version: &str,
    env_hash: &str,
) -> PathBuf {
    runtime_root
        .join("dependencies")
        .join("envs")
        .join(runtime.as_str())
        .join(format!("{}-{}", runtime_prefix(runtime), runtime_version))
        .join(env_hash)
}

/// Return the marker path used by one managed runtime environment directory.
/// 返回单个受管运行时环境目录使用的标记文件路径。
pub fn managed_env_marker_path(env_dir: &Path) -> PathBuf {
    env_dir.join(".luaskills-env.json")
}

/// Inspect whether one managed environment marker path is a file without hiding filesystem probe errors.
/// 检查单个受管环境标记路径是否为文件，同时不隐藏文件系统探测错误。
///
/// The marker_path parameter is the concrete `.luaskills-env.json` path for one managed environment.
/// marker_path 参数是单个受管环境对应的具体 `.luaskills-env.json` 路径。
///
/// Return true for an existing marker file, false for a confirmed missing marker, or an explicit probe/type error.
/// 已存在标记文件返回 true，确认缺失标记返回 false；探测或类型异常时返回显式错误。
fn managed_env_marker_path_is_file(marker_path: &Path) -> Result<bool, String> {
    match fs::metadata(marker_path) {
        Ok(metadata) if metadata.is_file() => Ok(true),
        Ok(_) => Err(format!(
            "Managed runtime env marker is not a file: {}",
            render_managed_runtime_path(marker_path)
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!(
            "Failed to inspect managed runtime env marker {}: {}",
            render_managed_runtime_path(marker_path),
            error
        )),
    }
}

/// Inspect whether one managed-runtime directory path is a directory without hiding filesystem probe errors.
/// 检查单个受管运行时目录路径是否为目录，同时不隐藏文件系统探测错误。
///
/// The path parameter is the concrete managed runtime directory path used by an environment lifecycle step.
/// path 参数是环境生命周期步骤使用的具体受管运行时目录路径。
///
/// Return true for an existing directory, false for a confirmed missing directory, or an explicit probe/type error.
/// 已存在目录返回 true，确认缺失目录返回 false；探测或类型异常时返回显式错误。
fn managed_runtime_directory_path_is_directory(
    path: &Path,
    directory_label: &str,
) -> Result<bool, String> {
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_dir() => Ok(true),
        Ok(_) => Err(format!(
            "{} is not a directory: {}",
            directory_label,
            render_managed_runtime_path(path)
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!(
            "Failed to inspect {} {}: {}",
            directory_label,
            render_managed_runtime_path(path),
            error
        )),
    }
}

/// Read one managed runtime environment marker from disk.
/// 从磁盘读取一个受管运行时环境标记。
pub fn read_managed_env_marker(path: &Path) -> Result<ManagedRuntimeEnvMarker, String> {
    let text = fs::read_to_string(path).map_err(|error| {
        format!(
            "Failed to read {}: {}",
            render_managed_runtime_path(path),
            error
        )
    })?;
    serde_json::from_str(&text).map_err(|error| {
        format!(
            "Failed to parse {}: {}",
            render_managed_runtime_path(path),
            error
        )
    })
}

/// Return whether an environment marker matches the expected marker exactly.
/// 返回某个环境标记是否与期望标记完全匹配。
pub fn managed_env_marker_matches(
    actual: &ManagedRuntimeEnvMarker,
    expected: &ManagedRuntimeEnvMarker,
) -> bool {
    actual == expected
}

/// Ensure one managed runtime environment exists and matches its expected marker.
/// 确保一个受管运行时环境存在并且匹配其期望标记。
pub fn ensure_managed_env(plan: &ManagedRuntimeEnvPlan) -> Result<(), String> {
    if managed_env_is_ready(plan)? {
        return Ok(());
    }
    match plan.runtime {
        ManagedRuntimeKind::Python => create_python_env(plan),
        ManagedRuntimeKind::Node => create_node_env(plan),
    }
}

/// Return whether a managed runtime environment already matches its expected marker.
/// 返回一个受管运行时环境是否已经匹配其期望标记。
pub fn managed_env_is_ready(plan: &ManagedRuntimeEnvPlan) -> Result<bool, String> {
    let marker_path = managed_env_marker_path(&plan.env_dir);
    if !managed_env_marker_path_is_file(&marker_path)? {
        return Ok(false);
    }
    let actual = read_managed_env_marker(&marker_path)?;
    Ok(managed_env_marker_matches(&actual, &plan.expected_marker))
}

/// Create one Python virtual environment and synchronize it from the lockfile.
/// 创建一个 Python 虚拟环境并按锁文件同步依赖。
fn create_python_env(plan: &ManagedRuntimeEnvPlan) -> Result<(), String> {
    let build_dir = prepare_build_dir(plan)?;
    let venv_dir = build_dir.join(".venv");
    run_command(
        Command::new(&plan.package_manager_executable)
            .arg("venv")
            .arg("--python")
            .arg(&plan.runtime_executable)
            .arg(&venv_dir),
        "create managed Python virtual environment",
    )?;
    let python_executable = python_venv_executable(&venv_dir);
    run_command(
        Command::new(&plan.package_manager_executable)
            .arg("pip")
            .arg("sync")
            .arg(&plan.lockfile_path)
            .arg("--python")
            .arg(&python_executable)
            .arg("--cache-dir")
            .arg(package_store_dir_for_plan(plan)),
        "synchronize managed Python environment",
    )?;
    finish_build_dir(plan, build_dir)
}

/// Create one Node dependency environment and install dependencies from the lockfile.
/// 创建一个 Node 依赖环境并按锁文件安装依赖。
fn create_node_env(plan: &ManagedRuntimeEnvPlan) -> Result<(), String> {
    if managed_runtime_directory_path_is_directory(&plan.env_dir, "managed env directory")? {
        fs::remove_dir_all(&plan.env_dir).map_err(|error| {
            format!(
                "Failed to remove {}: {}",
                render_managed_runtime_path(&plan.env_dir),
                error
            )
        })?;
    }
    fs::create_dir_all(&plan.env_dir).map_err(|error| {
        format!(
            "Failed to create {}: {}",
            render_managed_runtime_path(&plan.env_dir),
            error
        )
    })?;
    let package_json = plan.package_manifest_path.as_ref().ok_or_else(|| {
        "node package_json is required to create a managed Node environment".to_string()
    })?;
    fs::copy(package_json, plan.env_dir.join("package.json")).map_err(|error| {
        format!(
            "Failed to copy {} into {}: {}",
            render_managed_runtime_path(package_json),
            render_managed_runtime_path(&plan.env_dir),
            error
        )
    })?;
    fs::copy(&plan.lockfile_path, plan.env_dir.join("pnpm-lock.yaml")).map_err(|error| {
        format!(
            "Failed to copy {} into {}: {}",
            render_managed_runtime_path(&plan.lockfile_path),
            render_managed_runtime_path(&plan.env_dir),
            error
        )
    })?;
    let install_result = run_command(
        Command::new(&plan.runtime_executable)
            .arg(&plan.package_manager_executable)
            .arg("install")
            .arg("--frozen-lockfile")
            .arg("--store-dir")
            .arg(package_store_dir_for_plan(plan))
            .current_dir(&plan.env_dir),
        "install managed Node environment",
    )
    .and_then(|()| write_expected_marker(&plan.env_dir, &plan.expected_marker));
    if install_result.is_err() {
        let _ = fs::remove_dir_all(&plan.env_dir);
    }
    install_result
}

/// Prepare one clean temporary build directory next to the target environment.
/// 在目标环境旁准备一个干净的临时构建目录。
fn prepare_build_dir(plan: &ManagedRuntimeEnvPlan) -> Result<PathBuf, String> {
    let parent = plan.env_dir.parent().ok_or_else(|| {
        format!(
            "managed env directory has no parent: {}",
            render_managed_runtime_path(&plan.env_dir)
        )
    })?;
    fs::create_dir_all(parent).map_err(|error| {
        format!(
            "Failed to create {}: {}",
            render_managed_runtime_path(parent),
            error
        )
    })?;
    let build_dir = parent.join(format!(
        ".building-{}-{}",
        plan.env_hash,
        std::process::id()
    ));
    if managed_runtime_directory_path_is_directory(&build_dir, "managed build directory")? {
        fs::remove_dir_all(&build_dir).map_err(|error| {
            format!(
                "Failed to remove {}: {}",
                render_managed_runtime_path(&build_dir),
                error
            )
        })?;
    }
    fs::create_dir_all(&build_dir).map_err(|error| {
        format!(
            "Failed to create {}: {}",
            render_managed_runtime_path(&build_dir),
            error
        )
    })?;
    Ok(build_dir)
}

/// Finalize one temporary build directory into its stable environment directory.
/// 将一个临时构建目录收尾成稳定环境目录。
fn finish_build_dir(plan: &ManagedRuntimeEnvPlan, build_dir: PathBuf) -> Result<(), String> {
    write_expected_marker(&build_dir, &plan.expected_marker)?;
    if managed_runtime_directory_path_is_directory(&plan.env_dir, "managed env directory")? {
        fs::remove_dir_all(&plan.env_dir).map_err(|error| {
            format!(
                "Failed to remove {}: {}",
                render_managed_runtime_path(&plan.env_dir),
                error
            )
        })?;
    }
    fs::rename(&build_dir, &plan.env_dir).or_else(|rename_error| {
        copy_dir_recursive(&build_dir, &plan.env_dir)
            .and_then(|()| {
                fs::remove_dir_all(&build_dir).map_err(|cleanup_error| {
                    format!(
                        "Failed to remove {} after copy fallback: {}",
                        render_managed_runtime_path(&build_dir),
                        cleanup_error
                    )
                })
            })
            .map_err(|copy_error| {
                format!(
                    "Failed to move {} to {}: {}; copy fallback also failed: {}",
                    render_managed_runtime_path(&build_dir),
                    render_managed_runtime_path(&plan.env_dir),
                    rename_error,
                    copy_error
                )
            })
    })?;
    Ok(())
}

/// Write the expected marker into one environment directory.
/// 将期望标记写入一个环境目录。
fn write_expected_marker(env_dir: &Path, marker: &ManagedRuntimeEnvMarker) -> Result<(), String> {
    let marker_path = managed_env_marker_path(env_dir);
    let text = serde_json::to_string_pretty(marker)
        .map_err(|error| format!("Failed to serialize managed env marker: {}", error))?;
    fs::write(&marker_path, format!("{}\n", text)).map_err(|error| {
        format!(
            "Failed to write {}: {}",
            render_managed_runtime_path(&marker_path),
            error
        )
    })
}

/// Run one process command and convert failures into stable error text.
/// 运行一个进程命令并将失败转换为稳定错误文本。
fn run_command(command: &mut Command, operation: &str) -> Result<(), String> {
    let output = command
        .output()
        .map_err(|error| format!("Failed to {}: {}", operation, error))?;
    if output.status.success() {
        return Ok(());
    }
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    Err(format!(
        "Failed to {}: status={} stdout={} stderr={}",
        operation, output.status, stdout, stderr
    ))
}

/// Return the Python executable inside one venv directory.
/// 返回单个 venv 目录中的 Python 可执行文件。
fn python_venv_executable(venv_dir: &Path) -> PathBuf {
    if cfg!(windows) {
        venv_dir.join("Scripts").join("python.exe")
    } else {
        venv_dir.join("bin").join("python")
    }
}

/// Return the package-store directory used by one environment plan.
/// 返回单个环境计划使用的包存储目录。
fn package_store_dir_for_plan(plan: &ManagedRuntimeEnvPlan) -> PathBuf {
    let family = match plan.runtime {
        ManagedRuntimeKind::Python => "python",
        ManagedRuntimeKind::Node => "node",
    };
    let store_name = match plan.runtime {
        ManagedRuntimeKind::Python => "uv-cache",
        ManagedRuntimeKind::Node => "pnpm-store",
    };
    plan.runtime_root
        .join("dependencies")
        .join("package_store")
        .join(family)
        .join(store_name)
}

/// Recursively copy one directory tree.
/// 递归复制一个目录树。
fn copy_dir_recursive(source: &Path, destination: &Path) -> Result<(), String> {
    fs::create_dir_all(destination).map_err(|error| {
        format!(
            "Failed to create {}: {}",
            render_managed_runtime_path(destination),
            error
        )
    })?;
    for entry in fs::read_dir(source).map_err(|error| {
        format!(
            "Failed to read {}: {}",
            render_managed_runtime_path(source),
            error
        )
    })? {
        let entry = entry.map_err(|error| {
            format!(
                "Failed to read directory entry under {}: {}",
                render_managed_runtime_path(source),
                error
            )
        })?;
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        // Entry type read from the directory iterator without following symlinks.
        // 从目录迭代器读取且不跟随符号链接的目录项类型。
        let file_type = entry.file_type().map_err(|error| {
            format!(
                "Failed to inspect {} under {}: {}",
                render_managed_runtime_path(&source_path),
                render_managed_runtime_path(source),
                error
            )
        })?;
        if file_type.is_dir() {
            copy_dir_recursive(&source_path, &destination_path)?;
        } else if file_type.is_file() {
            fs::copy(&source_path, &destination_path).map_err(|error| {
                format!(
                    "Failed to copy {} to {}: {}",
                    render_managed_runtime_path(&source_path),
                    render_managed_runtime_path(&destination_path),
                    error
                )
            })?;
        } else {
            return Err(format!(
                "Failed to copy {} to {}: unsupported file type",
                render_managed_runtime_path(&source_path),
                render_managed_runtime_path(&destination_path)
            ));
        }
    }
    Ok(())
}

/// Resolve one Python managed runtime environment plan from a skill declaration.
/// 根据 skill 声明解析一个 Python 受管运行时环境计划。
pub fn resolve_python_env_plan(
    runtime_root: &Path,
    skill_dir: &Path,
    spec: &PythonRuntimeDependencySpec,
) -> Result<ManagedRuntimeEnvPlan, String> {
    let platform = current_managed_runtime_platform_key()?;
    let runtime_dir = runtime_install_dir(
        runtime_root,
        "python",
        &format!("cpython-{}-{}", spec.version, platform),
    );
    let runtime_executable =
        resolve_install_executable(&runtime_dir, "python", &spec.version, &platform)?;
    let package_manager = spec.package_manager.as_managed_runtime_str().to_string();
    let package_manager_install_dir = runtime_install_dir(
        runtime_root,
        "python",
        &format!("uv-{}-{}", spec.package_manager_version, platform),
    );
    let package_manager_executable = resolve_install_executable(
        &package_manager_install_dir,
        "uv",
        &spec.package_manager_version,
        &platform,
    )?;
    let lockfile_path =
        resolve_required_skill_file(skill_dir, &spec.lockfile, "python_runtime.lockfile")?;
    let lock_hash = sha256_file(&lockfile_path)?;
    let hash_input = ManagedRuntimeEnvHashInput {
        runtime: ManagedRuntimeKind::Python,
        runtime_version: spec.version.clone(),
        platform,
        package_manager,
        package_manager_version: spec.package_manager_version.clone(),
        lock_hash,
        package_manifest_hash: None,
    };
    build_env_plan(
        runtime_root,
        ManagedRuntimeKind::Python,
        runtime_executable,
        package_manager_executable,
        None,
        lockfile_path,
        hash_input,
    )
}

/// Resolve one Node.js managed runtime environment plan from a skill declaration.
/// 根据 skill 声明解析一个 Node.js 受管运行时环境计划。
pub fn resolve_node_env_plan(
    runtime_root: &Path,
    skill_dir: &Path,
    spec: &NodeRuntimeDependencySpec,
) -> Result<ManagedRuntimeEnvPlan, String> {
    let platform = current_managed_runtime_platform_key()?;
    let runtime_dir = runtime_install_dir(
        runtime_root,
        "node",
        &format!("node-{}-{}", spec.version, platform),
    );
    let runtime_executable =
        resolve_install_executable(&runtime_dir, "node", &spec.version, &platform)?;
    let package_manager = spec.package_manager.as_managed_runtime_str().to_string();
    let package_manager_install_dir = runtime_install_dir(
        runtime_root,
        "node",
        &format!("pnpm-{}", spec.package_manager_version),
    );
    let package_manager_executable = resolve_install_executable(
        &package_manager_install_dir,
        "pnpm",
        &spec.package_manager_version,
        "any",
    )?;
    let package_manifest_path =
        resolve_optional_skill_file(skill_dir, &spec.package_json, "node_runtime.package_json")?;
    let package_manifest_hash = package_manifest_path
        .as_ref()
        .map(|path| sha256_file(path))
        .transpose()?;
    let lockfile_path =
        resolve_required_skill_file(skill_dir, &spec.lockfile, "node_runtime.lockfile")?;
    let lock_hash = sha256_file(&lockfile_path)?;
    let hash_input = ManagedRuntimeEnvHashInput {
        runtime: ManagedRuntimeKind::Node,
        runtime_version: spec.version.clone(),
        platform,
        package_manager,
        package_manager_version: spec.package_manager_version.clone(),
        lock_hash,
        package_manifest_hash,
    };
    build_env_plan(
        runtime_root,
        ManagedRuntimeKind::Node,
        runtime_executable,
        package_manager_executable,
        package_manifest_path,
        lockfile_path,
        hash_input,
    )
}

/// Read one managed runtime installation manifest from an installation directory.
/// 从安装目录读取一个受管运行时安装清单。
pub fn read_install_manifest(install_dir: &Path) -> Result<ManagedRuntimeInstallManifest, String> {
    let manifest_path = install_dir.join("runtime-manifest.json");
    managed_runtime_install_manifest_path_is_file(&manifest_path)?;
    let text = fs::read_to_string(&manifest_path).map_err(|error| {
        format!(
            "Failed to read {}: {}",
            render_managed_runtime_path(&manifest_path),
            error
        )
    })?;
    // PowerShell 5.1 may write UTF-8 JSON files with a BOM when Set-Content is used.
    // PowerShell 5.1 使用 Set-Content 写 UTF-8 JSON 时可能带有 BOM。
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    serde_json::from_str(text).map_err(|error| {
        format!(
            "Failed to parse {}: {}",
            render_managed_runtime_path(&manifest_path),
            error
        )
    })
}

/// Inspect whether one managed-runtime install manifest path is a file without hiding filesystem probe errors.
/// 检查单个受管运行时安装清单路径是否为文件，同时不隐藏文件系统探测错误。
///
/// The manifest_path parameter is the concrete `runtime-manifest.json` path under one install directory.
/// manifest_path 参数是单个安装目录下的具体 `runtime-manifest.json` 路径。
///
/// Return unit for an existing manifest file, or an explicit missing/probe/type error.
/// 已存在清单文件时返回 unit；缺失、探测或类型异常时返回显式错误。
fn managed_runtime_install_manifest_path_is_file(manifest_path: &Path) -> Result<(), String> {
    match fs::metadata(manifest_path) {
        Ok(metadata) if metadata.is_file() => Ok(()),
        Ok(_) => Err(format!(
            "managed runtime install manifest is not a file: {}",
            render_managed_runtime_path(manifest_path)
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Err(format!(
            "managed runtime install manifest not found: {}",
            render_managed_runtime_path(manifest_path)
        )),
        Err(error) => Err(format!(
            "Failed to inspect managed runtime install manifest {}: {}",
            render_managed_runtime_path(manifest_path),
            error
        )),
    }
}

/// Build one complete managed runtime environment plan from normalized inputs.
/// 基于规范化输入构造一个完整受管运行时环境计划。
fn build_env_plan(
    runtime_root: &Path,
    runtime: ManagedRuntimeKind,
    runtime_executable: PathBuf,
    package_manager_executable: PathBuf,
    package_manifest_path: Option<PathBuf>,
    lockfile_path: PathBuf,
    hash_input: ManagedRuntimeEnvHashInput,
) -> Result<ManagedRuntimeEnvPlan, String> {
    let env_hash = compute_managed_runtime_env_hash(&hash_input);
    let env_dir = managed_env_dir(
        runtime_root,
        runtime,
        &hash_input.runtime_version,
        &env_hash,
    );
    let expected_marker = ManagedRuntimeEnvMarker::expected(&hash_input, env_hash.clone());
    Ok(ManagedRuntimeEnvPlan {
        runtime,
        platform: hash_input.platform,
        runtime_root: runtime_root.to_path_buf(),
        runtime_version: hash_input.runtime_version,
        runtime_executable,
        package_manager: hash_input.package_manager,
        package_manager_version: hash_input.package_manager_version,
        package_manager_executable,
        package_manifest_path,
        lockfile_path,
        lock_hash: hash_input.lock_hash,
        package_manifest_hash: hash_input.package_manifest_hash,
        env_hash,
        env_dir,
        expected_marker,
    })
}

/// Return one managed runtime installation directory under `runtime_root`.
/// 返回 `runtime_root` 下的单个受管运行时安装目录。
fn runtime_install_dir(runtime_root: &Path, family: &str, name: &str) -> PathBuf {
    runtime_root
        .join("dependencies")
        .join("runtimes")
        .join(family)
        .join(name)
}

/// Resolve and validate one executable from an installation manifest.
/// 从安装清单中解析并校验一个可执行文件。
fn resolve_install_executable(
    install_dir: &Path,
    expected_runtime: &str,
    expected_version: &str,
    expected_platform: &str,
) -> Result<PathBuf, String> {
    let manifest = read_install_manifest(install_dir)?;
    if manifest.schema_version != 1 {
        return Err(format!(
            "managed runtime manifest {} uses unsupported schema_version {}",
            render_managed_runtime_path(install_dir),
            manifest.schema_version
        ));
    }
    if manifest.runtime != expected_runtime {
        return Err(format!(
            "managed runtime manifest {} has runtime '{}', expected '{}'",
            render_managed_runtime_path(install_dir),
            manifest.runtime,
            expected_runtime
        ));
    }
    if manifest.version != expected_version {
        return Err(format!(
            "managed runtime manifest {} has version '{}', expected '{}'",
            render_managed_runtime_path(install_dir),
            manifest.version,
            expected_version
        ));
    }
    if manifest.platform != expected_platform {
        return Err(format!(
            "managed runtime manifest {} has platform '{}', expected '{}'",
            render_managed_runtime_path(install_dir),
            manifest.platform,
            expected_platform
        ));
    }
    let executable = install_dir.join(manifest.executable);
    if !managed_runtime_executable_path_is_file(&executable)? {
        return Err(format!(
            "managed runtime executable not found: {}",
            render_managed_runtime_path(&executable)
        ));
    }
    Ok(executable)
}

/// Inspect whether one managed-runtime executable path is a file without hiding filesystem probe errors.
/// 检查单个受管运行时可执行文件路径是否为文件，同时不隐藏文件系统探测错误。
///
/// The executable parameter is the install-manifest declared executable path resolved under its install directory.
/// executable 参数是安装清单声明并在安装目录下解析后的可执行文件路径。
///
/// Return true for an existing executable file, false for a confirmed missing executable, or an explicit probe/type error.
/// 已存在可执行文件返回 true，确认缺失可执行文件返回 false；探测或类型异常时返回显式错误。
fn managed_runtime_executable_path_is_file(executable: &Path) -> Result<bool, String> {
    match fs::metadata(executable) {
        Ok(metadata) if metadata.is_file() => Ok(true),
        Ok(_) => Err(format!(
            "managed runtime executable is not a file: {}",
            render_managed_runtime_path(executable)
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!(
            "Failed to inspect managed runtime executable {}: {}",
            render_managed_runtime_path(executable),
            error
        )),
    }
}

/// Resolve one required file path under a skill directory.
/// 解析 skill 目录下的一个必需文件路径。
fn resolve_required_skill_file(
    skill_dir: &Path,
    relative_path: &str,
    field_label: &str,
) -> Result<PathBuf, String> {
    if relative_path.trim().is_empty() {
        return Err(format!("{} is required", field_label));
    }
    let path = resolve_skill_file(skill_dir, relative_path, field_label)?;
    if !managed_runtime_skill_file_path_is_file(&path, field_label)? {
        return Err(format!(
            "{} not found: {}",
            field_label,
            render_managed_runtime_path(&path)
        ));
    }
    Ok(path)
}

/// Resolve one optional file path under a skill directory.
/// 解析 skill 目录下的一个可选文件路径。
fn resolve_optional_skill_file(
    skill_dir: &Path,
    relative_path: &str,
    field_label: &str,
) -> Result<Option<PathBuf>, String> {
    if relative_path.trim().is_empty() {
        return Ok(None);
    }
    let path = resolve_skill_file(skill_dir, relative_path, field_label)?;
    if !managed_runtime_skill_file_path_is_file(&path, field_label)? {
        return Err(format!(
            "{} not found: {}",
            field_label,
            render_managed_runtime_path(&path)
        ));
    }
    Ok(Some(path))
}

/// Inspect whether one managed-runtime skill file path is a file without hiding filesystem probe errors.
/// 检查单个受管运行时 skill 文件路径是否为文件，同时不隐藏文件系统探测错误。
///
/// The path parameter is the resolved skill-relative file path declared by one runtime dependency field.
/// path 参数是单个运行时依赖字段声明并解析后的 skill 相对文件路径。
///
/// Return true for an existing file, false for a confirmed missing file, or an explicit probe/type error.
/// 已存在文件返回 true，确认缺失文件返回 false；探测或类型异常时返回显式错误。
fn managed_runtime_skill_file_path_is_file(path: &Path, field_label: &str) -> Result<bool, String> {
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => Ok(true),
        Ok(_) => Err(format!(
            "{} is not a file: {}",
            field_label,
            render_managed_runtime_path(path)
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!(
            "Failed to inspect {} {}: {}",
            field_label,
            render_managed_runtime_path(path),
            error
        )),
    }
}

/// Resolve a skill-relative path and reject parent-directory traversal.
/// 解析一个 skill 相对路径，并拒绝父目录逃逸。
fn resolve_skill_file(
    skill_dir: &Path,
    relative_path: &str,
    field_label: &str,
) -> Result<PathBuf, String> {
    let path = Path::new(relative_path);
    if path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(format!(
            "{} must be a safe path under the skill directory",
            field_label
        ));
    }
    Ok(skill_dir.join(path))
}

/// Feed one labeled string field into a stable SHA-256 hasher.
/// 将一个带标签的字符串字段写入稳定 SHA-256 hasher。
fn hash_field(hasher: &mut Sha256, label: &str, value: impl AsRef<str>) {
    hasher.update(label.as_bytes());
    hasher.update([0]);
    hasher.update(value.as_ref().as_bytes());
    hasher.update([0xff]);
}

/// Return the directory prefix used for runtime-version environment groups.
/// 返回运行时版本环境分组使用的目录前缀。
fn runtime_prefix(runtime: ManagedRuntimeKind) -> &'static str {
    match runtime {
        ManagedRuntimeKind::Python => "py",
        ManagedRuntimeKind::Node => "node",
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ManagedRuntimeEnvHashInput, ManagedRuntimeEnvMarker, ManagedRuntimeEnvPlan,
        ManagedRuntimeKind, compute_managed_runtime_env_hash, copy_dir_recursive,
        current_managed_runtime_platform_key, managed_env_dir, managed_env_is_ready,
        managed_env_marker_matches, managed_env_marker_path, prepare_build_dir,
        read_install_manifest, resolve_node_env_plan, resolve_python_env_plan, sha256_hex,
        write_expected_marker,
    };
    use crate::runtime::path::render_host_visible_path;
    use crate::skill::dependencies::{
        NodeRuntimeDependencySpec, NodeRuntimePackageManager, PythonRuntimeDependencySpec,
        PythonRuntimePackageManager,
    };
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::symlink as create_unix_symlink;
    #[cfg(windows)]
    use std::os::windows::fs::symlink_file as create_windows_file_symlink;
    use std::path::{Path, PathBuf};

    /// Create one test file symlink that points at the requested target path.
    /// 创建一个指向指定目标路径的测试文件符号链接。
    #[cfg(unix)]
    fn create_test_file_symlink(link_path: &Path, target_path: &Path) -> bool {
        create_unix_symlink(target_path, link_path).expect("create test file symlink");
        true
    }

    /// Return whether one Windows symlink-dependent test should be skipped because the host lacks symlink privileges.
    /// 返回当前 Windows 符号链接相关测试是否应因宿主缺少符号链接权限而跳过。
    #[cfg(windows)]
    fn should_skip_windows_symlink_test(error: &std::io::Error) -> bool {
        /// Windows privilege error returned when symlink creation requires elevation or Developer Mode.
        /// 当符号链接创建需要管理员权限或开发者模式时 Windows 返回的权限错误码。
        const ERROR_PRIVILEGE_NOT_HELD: i32 = 1314;

        error.kind() == std::io::ErrorKind::PermissionDenied
            || error.raw_os_error() == Some(ERROR_PRIVILEGE_NOT_HELD)
    }

    /// Create one test file symlink that points at the requested target path.
    /// 创建一个指向指定目标路径的测试文件符号链接。
    #[cfg(windows)]
    fn create_test_file_symlink(link_path: &Path, target_path: &Path) -> bool {
        match create_windows_file_symlink(target_path, link_path) {
            Ok(()) => true,
            Err(error) if should_skip_windows_symlink_test(&error) => {
                eprintln!(
                    "skip symlink-dependent test because Windows symlink privileges are unavailable: {error}"
                );
                false
            }
            Err(error) => panic!("create test file symlink: {error}"),
        }
    }

    /// Verify that environment hashes are stable and include lockfile content changes.
    /// 验证环境哈希保持稳定，并且会纳入锁文件内容变化。
    #[test]
    fn env_hash_is_stable_and_lock_sensitive() {
        let input = ManagedRuntimeEnvHashInput {
            runtime: ManagedRuntimeKind::Python,
            runtime_version: "3.12.8".to_string(),
            platform: "windows-x64".to_string(),
            package_manager: "uv".to_string(),
            package_manager_version: "0.11.17".to_string(),
            lock_hash: sha256_hex(b"requests==2.32.3"),
            package_manifest_hash: None,
        };
        let same_hash = compute_managed_runtime_env_hash(&input);
        let repeated_hash = compute_managed_runtime_env_hash(&input);
        let changed_hash = compute_managed_runtime_env_hash(&ManagedRuntimeEnvHashInput {
            lock_hash: sha256_hex(b"requests==2.32.4"),
            ..input
        });

        assert_eq!(same_hash, repeated_hash);
        assert_ne!(same_hash, changed_hash);
    }

    /// Verify that expected markers match only exact environment identities.
    /// 验证期望标记只匹配完全一致的环境身份。
    #[test]
    fn marker_match_requires_exact_identity() {
        let input = ManagedRuntimeEnvHashInput {
            runtime: ManagedRuntimeKind::Node,
            runtime_version: "22.11.0".to_string(),
            platform: "linux-x64".to_string(),
            package_manager: "pnpm".to_string(),
            package_manager_version: "9.15.0".to_string(),
            lock_hash: "lock".to_string(),
            package_manifest_hash: Some("package".to_string()),
        };
        let env_hash = compute_managed_runtime_env_hash(&input);
        let expected = ManagedRuntimeEnvMarker::expected(&input, env_hash);
        let mut actual = expected.clone();

        assert!(managed_env_marker_matches(&actual, &expected));
        actual.lock_hash = "other".to_string();
        assert!(!managed_env_marker_matches(&actual, &expected));
    }

    /// Verify that managed environment directories use runtime and version group names.
    /// 验证受管环境目录会使用运行时与版本分组名称。
    #[test]
    fn managed_env_dir_uses_runtime_version_group() {
        let path = managed_env_dir(
            Path::new("runtime-root"),
            ManagedRuntimeKind::Python,
            "3.12.8",
            "abc",
        );
        assert!(path.ends_with(Path::new("dependencies/envs/python/py-3.12.8/abc")));
    }

    /// Verify that marker readiness checks return true only after writing the expected marker.
    /// 验证 marker 就绪检查只有在写入期望标记后才返回 true。
    #[test]
    fn managed_env_ready_checks_expected_marker() {
        let platform = current_managed_runtime_platform_key().expect("platform should resolve");
        let root = make_test_root("ready-marker");
        let runtime_root = root.join("runtime");
        let skill_dir = root.join("skill");
        fs::create_dir_all(skill_dir.join("python")).unwrap();
        fs::write(
            skill_dir.join("python/requirements.lock"),
            b"requests==2.32.3",
        )
        .unwrap();
        write_install_manifest(
            &runtime_root
                .join("dependencies/runtimes/python")
                .join(format!("cpython-3.12.7-{}", platform)),
            "python",
            "3.12.7",
            &platform,
            platform_executable("python"),
        );
        write_install_manifest(
            &runtime_root
                .join("dependencies/runtimes/python")
                .join(format!("uv-0.11.17-{}", platform)),
            "uv",
            "0.11.17",
            &platform,
            platform_executable("uv"),
        );
        let plan = resolve_python_env_plan(
            &runtime_root,
            &skill_dir,
            &PythonRuntimeDependencySpec {
                version: "3.12.7".to_string(),
                package_manager: PythonRuntimePackageManager::Uv,
                package_manager_version: "0.11.17".to_string(),
                lockfile: "python/requirements.lock".to_string(),
                required: true,
            },
        )
        .expect("python env plan should resolve");

        assert!(!managed_env_is_ready(&plan).expect("ready check should work"));
        fs::create_dir_all(&plan.env_dir).unwrap();
        write_expected_marker(&plan.env_dir, &plan.expected_marker).unwrap();
        assert!(managed_env_is_ready(&plan).expect("ready check should work"));
        let _ = fs::remove_dir_all(root);
    }

    /// Verify marker readiness checks reject marker paths that exist as directories.
    /// 验证 marker 就绪检查会拒绝以目录形式存在的 marker 路径。
    #[test]
    fn managed_env_ready_rejects_directory_marker() {
        // Platform key used to create matching runtime install manifests.
        // 用于创建匹配运行时安装清单的平台键。
        let platform = current_managed_runtime_platform_key().expect("platform should resolve");
        // Temporary root that isolates the directory marker fixture.
        // 隔离目录型 marker 夹具的临时根目录。
        let root = make_test_root("directory-marker");
        // Runtime root containing the fake installed runtime manifests.
        // 包含伪造已安装运行时清单的运行时根目录。
        let runtime_root = root.join("runtime");
        // Skill directory containing the Python lockfile fixture.
        // 包含 Python lockfile 夹具的 skill 目录。
        let skill_dir = root.join("skill");
        fs::create_dir_all(skill_dir.join("python")).unwrap();
        fs::write(
            skill_dir.join("python/requirements.lock"),
            b"requests==2.32.3",
        )
        .unwrap();
        write_install_manifest(
            &runtime_root
                .join("dependencies/runtimes/python")
                .join(format!("cpython-3.12.7-{}", platform)),
            "python",
            "3.12.7",
            &platform,
            platform_executable("python"),
        );
        write_install_manifest(
            &runtime_root
                .join("dependencies/runtimes/python")
                .join(format!("uv-0.11.17-{}", platform)),
            "uv",
            "0.11.17",
            &platform,
            platform_executable("uv"),
        );
        // Environment plan whose marker path will be occupied by a directory.
        // 其 marker 路径将被目录占用的环境计划。
        let plan = resolve_python_env_plan(
            &runtime_root,
            &skill_dir,
            &PythonRuntimeDependencySpec {
                version: "3.12.7".to_string(),
                package_manager: PythonRuntimePackageManager::Uv,
                package_manager_version: "0.11.17".to_string(),
                lockfile: "python/requirements.lock".to_string(),
                required: true,
            },
        )
        .expect("python env plan should resolve");
        // Directory occupying the exact marker file path.
        // 占据精确 marker 文件路径的目录。
        let marker_path = managed_env_marker_path(&plan.env_dir);
        fs::create_dir_all(&marker_path).expect("directory marker should be created");

        // Error returned before the directory marker can behave like a missing marker.
        // 在目录型 marker 表现得像缺失 marker 之前返回的错误。
        let error =
            managed_env_is_ready(&plan).expect_err("directory marker readiness check should fail");

        assert!(
            error.contains("Managed runtime env marker is not a file"),
            "unexpected error: {}",
            error
        );
        assert!(
            error.contains(&render_host_visible_path(&marker_path)),
            "unexpected error: {}",
            error
        );
        let _ = fs::remove_dir_all(root);
    }

    /// Verify install manifest readers reject directory manifests before JSON reading.
    /// 验证安装清单读取器会在 JSON 读取前拒绝目录型清单。
    #[test]
    fn read_install_manifest_rejects_directory_manifest() {
        // Temporary root that isolates the directory install-manifest fixture.
        // 隔离目录型安装清单夹具的临时根目录。
        let root = make_test_root("directory-install-manifest");
        // Install directory whose runtime-manifest path will be occupied by a directory.
        // 其 runtime-manifest 路径将被目录占用的安装目录。
        let install_dir = root.join("runtime-install");
        // Directory occupying the exact runtime-manifest.json path.
        // 占据精确 runtime-manifest.json 路径的目录。
        let manifest_path = install_dir.join("runtime-manifest.json");
        fs::create_dir_all(&manifest_path).expect("directory install manifest should be created");

        // Error returned before the directory manifest can be passed to read_to_string.
        // 在目录型清单被传递给 read_to_string 之前返回的错误。
        let error =
            read_install_manifest(&install_dir).expect_err("directory manifest should fail");

        assert!(
            error.contains("managed runtime install manifest is not a file"),
            "unexpected error: {}",
            error
        );
        assert!(
            error.contains(&render_host_visible_path(&manifest_path)),
            "unexpected error: {}",
            error
        );
        let _ = fs::remove_dir_all(root);
    }

    /// Verify build preparation rejects file paths before directory cleanup.
    /// 验证构建准备会在目录清理前拒绝文件路径。
    #[test]
    fn prepare_build_dir_rejects_file_build_dir() {
        // Temporary root that isolates the file-backed build directory fixture.
        // 隔离文件型构建目录夹具的临时根目录。
        let root = make_test_root("file-build-dir");
        // Reproducibility input used to build the expected marker for this synthetic plan.
        // 用于为这个合成计划构造期望标记的可复现输入。
        let hash_input = ManagedRuntimeEnvHashInput {
            runtime: ManagedRuntimeKind::Python,
            runtime_version: "3.12.7".to_string(),
            platform: "test-platform".to_string(),
            package_manager: "uv".to_string(),
            package_manager_version: "0.11.17".to_string(),
            lock_hash: "lock".to_string(),
            package_manifest_hash: None,
        };
        // Fixed environment hash that lets the test occupy the exact derived build directory.
        // 固定环境哈希，便于测试占用精确派生出的构建目录。
        let env_hash = "file-build-hash".to_string();
        // Expected marker matching the synthetic environment plan.
        // 与合成环境计划匹配的期望标记。
        let expected_marker = ManagedRuntimeEnvMarker::expected(&hash_input, env_hash.clone());
        // Environment plan whose build directory is deterministic from env_hash and process id.
        // 其构建目录由 env_hash 与进程号确定的环境计划。
        let plan = ManagedRuntimeEnvPlan {
            runtime: ManagedRuntimeKind::Python,
            platform: hash_input.platform.clone(),
            runtime_root: root.clone(),
            runtime_version: hash_input.runtime_version.clone(),
            runtime_executable: PathBuf::from("python"),
            package_manager: hash_input.package_manager.clone(),
            package_manager_version: hash_input.package_manager_version.clone(),
            package_manager_executable: PathBuf::from("uv"),
            package_manifest_path: None,
            lockfile_path: root.join("requirements.lock"),
            lock_hash: hash_input.lock_hash.clone(),
            package_manifest_hash: None,
            env_hash: env_hash.clone(),
            env_dir: root.join("env"),
            expected_marker,
        };
        // Exact temporary build directory that prepare_build_dir will inspect.
        // prepare_build_dir 将要检查的精确临时构建目录。
        let build_dir = root.join(format!(".building-{}-{}", env_hash, std::process::id()));
        // File occupying the build directory path that must be a directory.
        // 占据构建目录路径的文件，该路径必须是目录。
        fs::write(&build_dir, b"not a directory").expect("file build dir should be written");

        // Error returned before remove_dir_all can see a file-backed build path.
        // 在 remove_dir_all 看到文件型构建路径之前返回的错误。
        let error = prepare_build_dir(&plan).expect_err("file build dir should fail");

        assert!(
            error.contains("managed build directory is not a directory"),
            "unexpected error: {}",
            error
        );
        assert!(
            error.contains(&render_host_visible_path(&build_dir)),
            "unexpected error: {}",
            error
        );
        let _ = fs::remove_dir_all(root);
    }

    /// Verify copy fallback rejects symlink entries instead of following them.
    /// 验证复制降级会拒绝符号链接目录项，而不是跟随它们。
    #[test]
    fn copy_dir_recursive_rejects_symlink_entry() {
        // Temporary root that isolates the symlink copy fixture.
        // 隔离符号链接复制夹具的临时根目录。
        let root = make_test_root("copy-symlink-entry");
        // Source build directory consumed by the recursive copy fallback.
        // 递归复制降级使用的源构建目录。
        let source_dir = root.join("source");
        // Destination env directory consumed by the recursive copy fallback.
        // 递归复制降级使用的目标环境目录。
        let destination_dir = root.join("destination");
        fs::create_dir_all(&source_dir).expect("copy source dir should be created");
        // Real file target used only to create the symlink fixture.
        // 仅用于创建符号链接夹具的真实文件目标。
        let real_file_path = root.join("real-handler.py");
        fs::write(&real_file_path, b"print('ok')").expect("real file should be written");
        // Symlink entry inside the source build directory.
        // 源构建目录内的符号链接目录项。
        let symlink_path = source_dir.join("handler-link.py");
        if !create_test_file_symlink(&symlink_path, &real_file_path) {
            let _ = fs::remove_dir_all(root);
            return;
        }

        // Error returned before the symlink source entry can be silently followed.
        // 在符号链接源目录项被静默跟随之前返回的错误。
        let error = copy_dir_recursive(&source_dir, &destination_dir)
            .expect_err("symlink entry should fail managed runtime copy");

        assert!(
            error.contains("unsupported file type"),
            "unexpected error: {}",
            error
        );
        assert!(
            error.contains(&render_host_visible_path(&symlink_path)),
            "unexpected error: {}",
            error
        );
        let _ = fs::remove_dir_all(root);
    }

    /// Verify copy fallback rejects unsupported Unix filesystem entry types explicitly.
    /// 验证复制降级会显式拒绝不支持的 Unix 文件系统目录项类型。
    #[cfg(unix)]
    #[test]
    fn copy_dir_recursive_rejects_unsupported_unix_file_type() {
        use std::os::unix::fs::FileTypeExt;

        // Temporary root that isolates the unsupported-file-type copy fixture.
        // 隔离不支持文件类型复制夹具的临时根目录。
        let root = make_test_root("copy-unsupported-type");
        // Source build directory consumed by the recursive copy fallback.
        // 递归复制降级使用的源构建目录。
        let source_dir = root.join("source");
        // Destination env directory consumed by the recursive copy fallback.
        // 递归复制降级使用的目标环境目录。
        let destination_dir = root.join("destination");
        fs::create_dir_all(&source_dir).expect("copy source dir should be created");
        // FIFO path that is neither a regular file nor a directory.
        // 既不是普通文件也不是目录的 FIFO 路径。
        let fifo_path = source_dir.join("events.pipe");
        // Command status returned by mkfifo for the FIFO fixture creation.
        // mkfifo 为创建 FIFO 夹具返回的命令状态。
        let status = std::process::Command::new("mkfifo")
            .arg(&fifo_path)
            .status()
            .expect("mkfifo should run");
        assert!(status.success(), "mkfifo should create FIFO fixture");
        assert!(
            fs::metadata(&fifo_path)
                .expect("FIFO metadata should be readable")
                .file_type()
                .is_fifo(),
            "fixture should be FIFO"
        );

        // Error returned before the unsupported entry can reach fs::copy.
        // 在不支持的目录项进入 fs::copy 之前返回的错误。
        let error = copy_dir_recursive(&source_dir, &destination_dir)
            .expect_err("unsupported FIFO entry should fail managed runtime copy");

        assert!(
            error.contains("unsupported file type"),
            "unexpected error: {}",
            error
        );
        assert!(
            error.contains(&render_host_visible_path(&fifo_path)),
            "unexpected error: {}",
            error
        );
        let _ = fs::remove_dir_all(root);
    }

    /// Verify that Python environment plans resolve runtime manifests, lockfiles, and env markers.
    /// 验证 Python 环境计划会解析运行时清单、锁文件与环境标记。
    #[test]
    fn python_env_plan_resolves_manifests_and_lockfile() {
        let platform = current_managed_runtime_platform_key().expect("platform should resolve");
        let root = make_test_root("python-plan");
        let runtime_root = root.join("runtime");
        let skill_dir = root.join("skill");
        fs::create_dir_all(skill_dir.join("python")).unwrap();
        fs::write(
            skill_dir.join("python/requirements.lock"),
            b"requests==2.32.3",
        )
        .unwrap();
        write_install_manifest(
            &runtime_root
                .join("dependencies/runtimes/python")
                .join(format!("cpython-3.12.7-{}", platform)),
            "python",
            "3.12.7",
            &platform,
            platform_executable("python"),
        );
        write_install_manifest(
            &runtime_root
                .join("dependencies/runtimes/python")
                .join(format!("uv-0.11.17-{}", platform)),
            "uv",
            "0.11.17",
            &platform,
            platform_executable("uv"),
        );

        let plan = resolve_python_env_plan(
            &runtime_root,
            &skill_dir,
            &PythonRuntimeDependencySpec {
                version: "3.12.7".to_string(),
                package_manager: PythonRuntimePackageManager::Uv,
                package_manager_version: "0.11.17".to_string(),
                lockfile: "python/requirements.lock".to_string(),
                required: true,
            },
        )
        .expect("python env plan should resolve");

        assert_eq!(plan.runtime, ManagedRuntimeKind::Python);
        assert_eq!(plan.package_manager, "uv");
        assert_eq!(plan.lock_hash, sha256_hex(b"requests==2.32.3"));
        assert_eq!(plan.expected_marker.env_hash, plan.env_hash);
        assert!(
            plan.env_dir
                .starts_with(runtime_root.join("dependencies/envs/python"))
        );
        let _ = fs::remove_dir_all(root);
    }

    /// Verify directory executables are reported as type errors instead of missing executables.
    /// 验证目录型 executable 会被报告为类型错误，而不是缺失可执行文件。
    #[test]
    fn python_env_plan_rejects_directory_runtime_executable() {
        // Platform key used to create matching runtime install manifests.
        // 用于创建匹配运行时安装清单的平台键。
        let platform = current_managed_runtime_platform_key().expect("platform should resolve");
        // Temporary root that isolates the directory executable fixture.
        // 隔离目录型 executable 夹具的临时根目录。
        let root = make_test_root("directory-runtime-executable");
        // Runtime root containing the fake installed runtime manifests.
        // 包含伪造已安装运行时清单的运行时根目录。
        let runtime_root = root.join("runtime");
        // Skill directory containing the Python lockfile fixture.
        // 包含 Python lockfile 夹具的 skill 目录。
        let skill_dir = root.join("skill");
        fs::create_dir_all(skill_dir.join("python")).unwrap();
        fs::write(
            skill_dir.join("python/requirements.lock"),
            b"requests==2.32.3",
        )
        .unwrap();
        // Python runtime install directory whose manifest executable path is occupied by a directory.
        // 其清单 executable 路径被目录占据的 Python 运行时安装目录。
        let python_install_dir = runtime_root
            .join("dependencies")
            .join("runtimes")
            .join("python")
            .join(format!("cpython-3.12.7-{}", platform));
        // Manifest-declared executable path that must be a file.
        // 清单声明且必须为文件的 executable 路径。
        let executable_path = python_install_dir.join(platform_executable("python"));
        fs::create_dir_all(&executable_path).expect("directory executable should be created");
        // Runtime manifest declaring the directory-backed executable path.
        // 声明目录型 executable 路径的运行时清单。
        let payload = serde_json::json!({
            "schema_version": 1,
            "runtime": "python",
            "version": "3.12.7",
            "platform": platform,
            "executable": platform_executable("python"),
        });
        fs::write(
            python_install_dir.join("runtime-manifest.json"),
            serde_json::to_string_pretty(&payload).unwrap(),
        )
        .unwrap();
        write_install_manifest(
            &runtime_root
                .join("dependencies/runtimes/python")
                .join(format!("uv-0.11.17-{}", platform)),
            "uv",
            "0.11.17",
            &platform,
            platform_executable("uv"),
        );

        // Error returned by the full Python environment plan resolver.
        // 完整 Python 环境计划解析器返回的错误。
        let error = resolve_python_env_plan(
            &runtime_root,
            &skill_dir,
            &PythonRuntimeDependencySpec {
                version: "3.12.7".to_string(),
                package_manager: PythonRuntimePackageManager::Uv,
                package_manager_version: "0.11.17".to_string(),
                lockfile: "python/requirements.lock".to_string(),
                required: true,
            },
        )
        .expect_err("directory runtime executable should be rejected");
        // Error text expected from the shared host-visible path formatter.
        // 由共享宿主可见路径渲染器生成的期望错误文本。
        let expected_error = format!(
            "managed runtime executable is not a file: {}",
            render_host_visible_path(&executable_path)
        );

        assert_eq!(error, expected_error);
        // Cleanup result is intentionally ignored for best-effort temporary test artifacts.
        // 对临时测试产物的清理结果按最佳努力原则有意忽略。
        let _ = fs::remove_dir_all(root);
    }

    /// Verify that Node environment plans include package.json in the environment identity.
    /// 验证 Node 环境计划会把 package.json 纳入环境身份。
    #[test]
    fn node_env_plan_includes_package_manifest_hash() {
        let platform = current_managed_runtime_platform_key().expect("platform should resolve");
        let root = make_test_root("node-plan");
        let runtime_root = root.join("runtime");
        let skill_dir = root.join("skill");
        fs::create_dir_all(skill_dir.join("node")).unwrap();
        fs::write(
            skill_dir.join("node/package.json"),
            br#"{"dependencies":{}}"#,
        )
        .unwrap();
        fs::write(
            skill_dir.join("node/pnpm-lock.yaml"),
            b"lockfileVersion: '9.0'",
        )
        .unwrap();
        write_install_manifest(
            &runtime_root
                .join("dependencies/runtimes/node")
                .join(format!("node-22.11.0-{}", platform)),
            "node",
            "22.11.0",
            &platform,
            platform_executable("node"),
        );
        write_install_manifest(
            &runtime_root.join("dependencies/runtimes/node/pnpm-9.15.0"),
            "pnpm",
            "9.15.0",
            "any",
            "bin/pnpm.cjs",
        );

        let plan = resolve_node_env_plan(
            &runtime_root,
            &skill_dir,
            &NodeRuntimeDependencySpec {
                version: "22.11.0".to_string(),
                package_manager: NodeRuntimePackageManager::Pnpm,
                package_manager_version: "9.15.0".to_string(),
                package_json: "node/package.json".to_string(),
                lockfile: "node/pnpm-lock.yaml".to_string(),
                required: true,
            },
        )
        .expect("node env plan should resolve");

        assert_eq!(plan.runtime, ManagedRuntimeKind::Node);
        assert_eq!(plan.package_manager, "pnpm");
        assert_eq!(plan.lock_hash, sha256_hex(b"lockfileVersion: '9.0'"));
        assert_eq!(
            plan.package_manifest_hash,
            Some(sha256_hex(br#"{"dependencies":{}}"#))
        );
        assert!(
            plan.env_dir
                .starts_with(runtime_root.join("dependencies/envs/node"))
        );
        let _ = fs::remove_dir_all(root);
    }

    /// Verify that managed runtime declarations cannot escape the skill directory.
    /// 验证受管运行时声明不能逃逸 skill 目录。
    #[test]
    fn managed_runtime_plan_rejects_skill_path_traversal() {
        let platform = current_managed_runtime_platform_key().expect("platform should resolve");
        let root = make_test_root("path-traversal");
        let runtime_root = root.join("runtime");
        write_install_manifest(
            &runtime_root
                .join("dependencies/runtimes/python")
                .join(format!("cpython-3.12.7-{}", platform)),
            "python",
            "3.12.7",
            &platform,
            platform_executable("python"),
        );
        write_install_manifest(
            &runtime_root
                .join("dependencies/runtimes/python")
                .join(format!("uv-0.11.17-{}", platform)),
            "uv",
            "0.11.17",
            &platform,
            platform_executable("uv"),
        );
        let error = resolve_python_env_plan(
            &runtime_root,
            &root.join("skill"),
            &PythonRuntimeDependencySpec {
                version: "3.12.7".to_string(),
                package_manager: PythonRuntimePackageManager::Uv,
                package_manager_version: "0.11.17".to_string(),
                lockfile: "../outside.lock".to_string(),
                required: true,
            },
        )
        .expect_err("path traversal should be rejected");

        assert!(error.contains("skill directory"));
        let _ = fs::remove_dir_all(root);
    }

    /// Verify that missing lockfile errors render paths through the host-visible path formatter.
    /// 验证缺失锁文件错误会通过宿主可见路径渲染器输出路径。
    #[test]
    fn python_env_plan_missing_lockfile_error_uses_host_visible_path() {
        // Platform key used to create matching runtime install manifests.
        // 用于创建匹配运行时安装清单的平台键。
        let platform = current_managed_runtime_platform_key().expect("platform should resolve");
        // Temporary root that isolates all filesystem fixtures for this test.
        // 隔离当前测试所有文件系统夹具的临时根目录。
        let root = make_test_root("missing-lockfile-path");
        // Runtime root containing the fake installed runtime manifests.
        // 包含伪造已安装运行时清单的运行时根目录。
        let runtime_root = root.join("runtime");
        // Skill directory used as the base for the missing relative lockfile.
        // 作为缺失相对锁文件基准目录的 skill 目录。
        let skill_dir = root.join("skill");
        write_install_manifest(
            &runtime_root
                .join("dependencies/runtimes/python")
                .join(format!("cpython-3.12.7-{}", platform)),
            "python",
            "3.12.7",
            &platform,
            platform_executable("python"),
        );
        write_install_manifest(
            &runtime_root
                .join("dependencies/runtimes/python")
                .join(format!("uv-0.11.17-{}", platform)),
            "uv",
            "0.11.17",
            &platform,
            platform_executable("uv"),
        );

        // Missing lockfile path expected after skill-relative resolution.
        // 经过 skill 相对路径解析后期望缺失的锁文件路径。
        let expected_lockfile = skill_dir.join("python/missing.lock");
        // Error returned by the full Python environment plan resolver.
        // 完整 Python 环境计划解析器返回的错误。
        let error = resolve_python_env_plan(
            &runtime_root,
            &skill_dir,
            &PythonRuntimeDependencySpec {
                version: "3.12.7".to_string(),
                package_manager: PythonRuntimePackageManager::Uv,
                package_manager_version: "0.11.17".to_string(),
                lockfile: "python/missing.lock".to_string(),
                required: true,
            },
        )
        .expect_err("missing lockfile should be reported");
        // Error text expected from the shared host-visible path formatter.
        // 由共享宿主可见路径渲染器生成的期望错误文本。
        let expected_error = format!(
            "python_runtime.lockfile not found: {}",
            render_host_visible_path(&expected_lockfile)
        );

        assert_eq!(error, expected_error);
        // Cleanup result is intentionally ignored for best-effort temporary test artifacts.
        // 对临时测试产物的清理结果按最佳努力原则有意忽略。
        let _ = fs::remove_dir_all(root);
    }

    /// Verify directory lockfiles are reported as type errors instead of missing files.
    /// 验证目录型 lockfile 会被报告为类型错误，而不是缺失文件。
    #[test]
    fn python_env_plan_rejects_directory_lockfile() {
        // Platform key used to create matching runtime install manifests.
        // 用于创建匹配运行时安装清单的平台键。
        let platform = current_managed_runtime_platform_key().expect("platform should resolve");
        // Temporary root that isolates the directory lockfile fixture.
        // 隔离目录型 lockfile 夹具的临时根目录。
        let root = make_test_root("directory-lockfile");
        // Runtime root containing the fake installed runtime manifests.
        // 包含伪造已安装运行时清单的运行时根目录。
        let runtime_root = root.join("runtime");
        // Skill directory used as the base for the directory lockfile.
        // 作为目录型 lockfile 基准目录的 skill 目录。
        let skill_dir = root.join("skill");
        write_install_manifest(
            &runtime_root
                .join("dependencies/runtimes/python")
                .join(format!("cpython-3.12.7-{}", platform)),
            "python",
            "3.12.7",
            &platform,
            platform_executable("python"),
        );
        write_install_manifest(
            &runtime_root
                .join("dependencies/runtimes/python")
                .join(format!("uv-0.11.17-{}", platform)),
            "uv",
            "0.11.17",
            &platform,
            platform_executable("uv"),
        );
        // Directory occupying the lockfile path declared by the Python runtime dependency.
        // 占据 Python 运行时依赖声明 lockfile 路径的目录。
        let lockfile_path = skill_dir.join("python/requirements.lock");
        fs::create_dir_all(&lockfile_path).expect("directory lockfile should be created");

        // Error returned by the full Python environment plan resolver.
        // 完整 Python 环境计划解析器返回的错误。
        let error = resolve_python_env_plan(
            &runtime_root,
            &skill_dir,
            &PythonRuntimeDependencySpec {
                version: "3.12.7".to_string(),
                package_manager: PythonRuntimePackageManager::Uv,
                package_manager_version: "0.11.17".to_string(),
                lockfile: "python/requirements.lock".to_string(),
                required: true,
            },
        )
        .expect_err("directory lockfile should be rejected");
        // Error text expected from the shared host-visible path formatter.
        // 由共享宿主可见路径渲染器生成的期望错误文本。
        let expected_error = format!(
            "python_runtime.lockfile is not a file: {}",
            render_host_visible_path(&lockfile_path)
        );

        assert_eq!(error, expected_error);
        // Cleanup result is intentionally ignored for best-effort temporary test artifacts.
        // 对临时测试产物的清理结果按最佳努力原则有意忽略。
        let _ = fs::remove_dir_all(root);
    }

    /// Create a unique temporary test root.
    /// 创建一个唯一的临时测试根目录。
    fn make_test_root(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "luaskills-managed-runtime-{}-{}-{}",
            label,
            std::process::id(),
            unique_suffix()
        ));
        fs::create_dir_all(&root).unwrap();
        root
    }

    /// Return a simple unique suffix for temporary test paths.
    /// 返回用于临时测试路径的简单唯一后缀。
    fn unique_suffix() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    }

    /// Write one install manifest and create its declared executable.
    /// 写入一个安装清单并创建其声明的可执行文件。
    fn write_install_manifest(
        install_dir: &Path,
        runtime: &str,
        version: &str,
        platform: &str,
        executable: &str,
    ) {
        fs::create_dir_all(
            install_dir.join(Path::new(executable).parent().unwrap_or(Path::new(""))),
        )
        .unwrap();
        fs::write(install_dir.join(executable), b"executable").unwrap();
        let payload = serde_json::json!({
            "schema_version": 1,
            "runtime": runtime,
            "version": version,
            "platform": platform,
            "executable": executable,
        });
        fs::write(
            install_dir.join("runtime-manifest.json"),
            serde_json::to_string_pretty(&payload).unwrap(),
        )
        .unwrap();
    }

    /// Return a platform-shaped executable path for install manifest tests.
    /// 返回用于安装清单测试的平台形态可执行路径。
    fn platform_executable(kind: &str) -> &'static str {
        match (std::env::consts::OS, kind) {
            ("windows", "python") => "python.exe",
            ("windows", "uv") => "uv.exe",
            ("windows", "node") => "node.exe",
            (_, "python") => "bin/python3",
            (_, "uv") => "uv",
            (_, "node") => "bin/node",
            _ => "tool",
        }
    }
}
