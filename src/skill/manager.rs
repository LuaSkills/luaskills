use std::collections::BTreeSet;
use std::fmt::Display;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use semver::Version;
use serde::{Deserialize, Serialize};

use crate::download::archive::extract_skill_package_zip;
use crate::download::manager::{DownloadManager, DownloadManagerConfig};
use crate::host::callbacks::{
    RuntimeSkillOperationProgressDetail, RuntimeSkillOperationProgressEmitter,
};
use crate::host::options::RuntimeSkillRoot;
use crate::lua_skill::{SkillMeta, validate_luaskills_identifier, validate_luaskills_version};
use crate::runtime::path::render_host_visible_path;
use crate::skill::resolver::{SkillSourceManifest, parse_skill_source_manifest};
use crate::skill::source::{
    InstalledSkillRecord, InstalledSkillSourceRecord, SkillInstallSourceType,
};

/// Render one skill-manager filesystem path for user-facing error messages.
/// 为面向用户的技能管理器错误消息渲染单个文件系统路径。
fn render_skill_manager_path(path: &Path) -> String {
    render_host_visible_path(path)
}

/// Inspect whether one skill-root path is a directory without hiding filesystem probe errors.
/// 检查单个技能根路径是否为目录，同时不隐藏文件系统探测错误。
///
/// The root parameter is the skill-root path that should be inspected before directory traversal.
/// root 参数是目录遍历前需要检查的技能根路径。
///
/// Return true for an existing directory, false for a confirmed missing path, or an explicit probe/type error.
/// 已存在目录返回 true，确认缺失路径返回 false；探测或类型失败时返回显式错误。
fn skill_root_path_is_directory(root: &Path) -> Result<bool, String> {
    match fs::metadata(root) {
        Ok(metadata) if metadata.is_dir() => Ok(true),
        Ok(_) => Err(format!(
            "Skill root is not a directory: {}",
            render_skill_manager_path(root)
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!(
            "Failed to inspect skill root {}: {}",
            render_skill_manager_path(root),
            error
        )),
    }
}

/// Inspect whether one skill manifest path is a file without hiding filesystem probe errors.
/// 检查单个技能清单路径是否为文件，同时不隐藏文件系统探测错误。
///
/// The skill_yaml parameter is the concrete skill.yaml path derived from one skill directory.
/// skill_yaml 参数是从单个技能目录派生出的具体 skill.yaml 路径。
///
/// Return true for an existing manifest file, false for a confirmed missing manifest, or an explicit probe/type error.
/// 已存在清单文件返回 true，确认缺失清单返回 false；探测或类型失败时返回显式错误。
fn skill_manifest_path_is_file(skill_yaml: &Path) -> Result<bool, String> {
    match fs::metadata(skill_yaml) {
        Ok(metadata) if metadata.is_file() => Ok(true),
        Ok(_) => Err(format!(
            "Skill manifest is not a file: {}",
            render_skill_manager_path(skill_yaml)
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!(
            "Failed to inspect skill manifest {}: {}",
            render_skill_manager_path(skill_yaml),
            error
        )),
    }
}

/// Inspect whether one disabled-state record path is a file without hiding filesystem probe errors.
/// 检查单个停用状态记录路径是否为文件，同时不隐藏文件系统探测错误。
///
/// The path parameter is the concrete disabled-state record path for one skill id.
/// path 参数是单个 skill id 对应的具体停用状态记录路径。
///
/// Return true for an existing record file, false for a confirmed missing record, or an explicit probe/type error.
/// 已存在记录文件返回 true，确认缺失记录返回 false；探测或类型失败时返回显式错误。
fn disabled_record_path_is_file(path: &Path) -> Result<bool, String> {
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => Ok(true),
        Ok(_) => Err(format!(
            "Disabled record is not a file: {}",
            render_skill_manager_path(path)
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!(
            "Failed to inspect disabled record {}: {}",
            render_skill_manager_path(path),
            error
        )),
    }
}

/// Inspect whether one managed install record path is a file without hiding filesystem probe errors.
/// 检查单个受管安装记录路径是否为文件，同时不隐藏文件系统探测错误。
///
/// The path parameter is the concrete install-record path for one skill id.
/// path 参数是单个 skill id 对应的具体安装记录路径。
///
/// Return true for an existing record file, false for a confirmed missing record, or an explicit probe/type error.
/// 已存在记录文件返回 true，确认缺失记录返回 false；探测或类型失败时返回显式错误。
fn install_record_path_is_file(path: &Path) -> Result<bool, String> {
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => Ok(true),
        Ok(_) => Err(format!(
            "Install record is not a file: {}",
            render_skill_manager_path(path)
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!(
            "Failed to inspect install record {}: {}",
            render_skill_manager_path(path),
            error
        )),
    }
}

/// Inspect whether one skill package path is a directory without hiding filesystem probe errors.
/// 检查单个技能包路径是否为目录，同时不隐藏文件系统探测错误。
///
/// The skill_dir parameter is the concrete skill package directory used by one lifecycle action.
/// skill_dir 参数是单个生命周期操作使用的具体技能包目录。
///
/// Return true for an existing package directory, false for a confirmed missing package directory, or an explicit probe/type error.
/// 已存在包目录返回 true，确认缺失包目录返回 false；探测或类型异常时返回显式错误。
fn skill_package_dir_is_directory(skill_dir: &Path) -> Result<bool, String> {
    match fs::metadata(skill_dir) {
        Ok(metadata) if metadata.is_dir() => Ok(true),
        Ok(_) => Err(format!(
            "Skill package path is not a directory: {}",
            render_skill_manager_path(skill_dir)
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!(
            "Failed to inspect skill package directory {}: {}",
            render_skill_manager_path(skill_dir),
            error
        )),
    }
}

/// Inspect whether one lifecycle staging temp root is a directory without hiding filesystem probe errors.
/// 检查单个生命周期暂存临时根是否为目录，同时不隐藏文件系统探测错误。
///
/// The temp_root parameter is the concrete install/update staging root created before archive extraction.
/// temp_root 参数是归档解压前创建的具体安装/更新暂存根目录。
///
/// Return true for an existing temp directory, false for a confirmed missing temp directory, or an explicit probe/type error.
/// 已存在临时目录返回 true，确认缺失临时目录返回 false；探测或类型异常时返回显式错误。
fn staging_temp_root_is_directory(temp_root: &Path) -> Result<bool, String> {
    match fs::metadata(temp_root) {
        Ok(metadata) if metadata.is_dir() => Ok(true),
        Ok(_) => Err(format!(
            "Skill staging temp root is not a directory: {}",
            render_skill_manager_path(temp_root)
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!(
            "Failed to inspect skill staging temp root {}: {}",
            render_skill_manager_path(temp_root),
            error
        )),
    }
}

/// Remove one staging directory without first collapsing metadata errors through an existence probe.
/// 删除单个暂存目录，同时不先通过存在性探测折叠元数据错误。
///
/// The path parameter is the concrete staging directory that should be removed.
/// path 参数是应被删除的具体暂存目录。
///
/// Return true when a directory was removed, false when it was already absent, or an explicit cleanup error.
/// 已删除目录时返回 true，目录原本不存在时返回 false；清理失败时返回显式错误。
fn remove_staging_dir_if_present(path: &Path) -> Result<bool, String> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!(
            "Failed to remove staging directory {}: {}",
            render_skill_manager_path(path),
            error
        )),
    }
}

/// Lifecycle operations that the LuaSkills manager layer exposes for one skill.
/// LuaSkills 管理层为单个技能公开的生命周期操作类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SkillLifecycleAction {
    Install,
    Update,
    Reload,
    Uninstall,
    Enable,
    Disable,
}

/// Logical operation plane used to distinguish host system controls from ordinary skill controls.
/// 用于区分宿主系统控制面与普通技能控制面的逻辑操作平面。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SkillOperationPlane {
    Skills,
    System,
}

/// Authority level supplied by the host for system skill-management entrypoints.
/// 宿主为系统级技能管理入口注入的权限等级。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillManagementAuthority {
    /// Full host-system authority that may write the ROOT skill layer.
    /// 可写入 ROOT 技能层的完整宿主系统权限。
    System,
    /// Delegated tool authority that must follow ordinary PROJECT/USER boundaries.
    /// 必须遵守普通 PROJECT/USER 边界的委托工具权限。
    DelegatedTool,
}

/// High-level manager configuration that defines where installed skills and their state are stored.
/// 定义已安装技能及其状态存放位置的高层管理配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillManagerConfig {
    /// Named skill root whose lifecycle state is managed by the current manager instance.
    /// 当前管理器实例所管理的命名技能根。
    pub skill_root: RuntimeSkillRoot,
    /// Root directory where lifecycle sidecar state of the current named skill root is persisted.
    /// 当前命名技能根生命周期旁路状态的持久化根目录。
    pub lifecycle_root: PathBuf,
    /// Root directory used to cache downloaded skill packages and remote manifests.
    /// 用于缓存下载技能包与远程清单的根目录。
    pub download_cache_root: PathBuf,
    /// Whether managed skill install/update flows may access the network.
    /// 受管技能安装/更新流程是否允许访问网络。
    pub allow_network_download: bool,
    /// Optional GitHub site base URL override used by managed GitHub installs.
    /// 受管 GitHub 安装使用的可选 GitHub 站点基址覆盖。
    #[serde(default)]
    pub github_base_url: Option<String>,
    /// Optional GitHub API base URL override used by managed GitHub installs.
    /// 受管 GitHub 安装使用的可选 GitHub API 基址覆盖。
    #[serde(default)]
    pub github_api_base_url: Option<String>,
    /// Optional official LuaSkills Hub base URL used by managed Hub installs.
    /// 受管 Hub 安装使用的可选官方 LuaSkills Hub 基址。
    #[serde(default)]
    pub official_skill_hub_base_url: Option<String>,
    /// Whether trusted system operations may install from private URL manifests.
    /// 可信 system 操作是否允许从私有 URL manifest 安装。
    #[serde(default)]
    pub enable_private_url_skill_install: bool,
    /// Host-controlled URL prefixes allowed for private skill manifests.
    /// 宿主管控的私有技能 manifest 允许 URL 前缀。
    #[serde(default)]
    pub private_skill_source_allowlist: Vec<String>,
}

/// One install request accepted by the future LuaSkills manager entrypoints.
/// 未来 LuaSkills 管理入口接受的单次安装请求定义。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SkillInstallRequest {
    /// Optional skill id used for install-by-name flows.
    /// 供按名称安装流程使用的可选 skill id。
    pub skill_id: Option<String>,
    /// Optional raw source string such as URL or local directory.
    /// 例如 URL 或本地目录一类的可选原始来源字符串。
    pub source: Option<String>,
    /// Source type used to interpret the source locator. Defaults to GitHub.
    /// 用于解释来源定位值的来源类型，默认使用 GitHub。
    #[serde(default)]
    pub source_type: SkillInstallSourceType,
}

/// One install or update result returned by the skill manager.
/// 由技能管理器返回的单次安装或更新结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillApplyResult {
    /// Stable skill identifier targeted by the current operation.
    /// 当前操作目标的稳定技能标识符。
    pub skill_id: String,
    /// High-level result status such as blocked, already_installed, or not_implemented.
    /// 高层结果状态，例如 blocked、already_installed 或 not_implemented。
    pub status: String,
    /// Human-readable explanation of the current result.
    /// 当前结果的人类可读解释文本。
    pub message: String,
    /// Optional semantic version involved in the current install/update result.
    /// 当前安装/更新结果涉及的可选语义化版本。
    #[serde(default)]
    pub version: Option<String>,
    /// Optional managed install source type involved in the current result.
    /// 当前结果涉及的可选受管安装来源类型。
    #[serde(default)]
    pub source_type: Option<SkillInstallSourceType>,
    /// Optional stable source locator involved in the current result.
    /// 当前结果涉及的可选稳定来源定位值。
    #[serde(default)]
    pub source_locator: Option<String>,
}

/// One staged install/update mutation that is not committed until runtime reload succeeds.
/// 单个尚未提交的安装/更新变更，只有运行时重载成功后才会最终提交。
#[derive(Debug, Clone)]
pub enum PreparedSkillApply {
    /// One immediate result that does not mutate disk state.
    /// 一个不会修改磁盘状态的即时结果。
    Immediate(SkillApplyResult),
    /// One staged install mutation waiting for commit or rollback.
    /// 一个等待提交或回滚的已暂存安装变更。
    Install(PreparedSkillInstall),
    /// One staged update mutation waiting for commit or rollback.
    /// 一个等待提交或回滚的已暂存更新变更。
    Update(PreparedSkillUpdate),
}

/// One staged install mutation prepared before the runtime reload is attempted.
/// 在尝试运行时重载之前准备好的单次安装暂存变更。
#[derive(Debug, Clone)]
pub struct PreparedSkillInstall {
    /// Structured install result returned after the staged install succeeds.
    /// 暂存安装成功后返回的结构化安装结果。
    pub result: SkillApplyResult,
    /// Final target directory where the installed skill has been staged.
    /// 已暂存安装技能的最终目标目录。
    pub target_dir: PathBuf,
    /// Install record that should be persisted only after runtime reload succeeds.
    /// 只有运行时重载成功后才应持久化的安装记录。
    pub install_record: InstalledSkillRecord,
}

/// One staged update mutation prepared before the runtime reload is attempted.
/// 在尝试运行时重载之前准备好的单次更新暂存变更。
#[derive(Debug, Clone)]
pub struct PreparedSkillUpdate {
    /// Structured update result returned after the staged update succeeds.
    /// 暂存更新成功后返回的结构化更新结果。
    pub result: SkillApplyResult,
    /// Final target directory currently holding the staged new skill package.
    /// 当前持有已暂存新技能包的最终目标目录。
    pub target_dir: PathBuf,
    /// Backup directory that still contains the previous skill package until commit completes.
    /// 在提交完成前仍保存旧技能包的备份目录。
    pub backup_dir: PathBuf,
    /// Updated install record that should be persisted only after runtime reload succeeds.
    /// 只有运行时重载成功后才应持久化的更新后安装记录。
    pub install_record: InstalledSkillRecord,
    /// Previous install record that should be restored if the update commit partially fails.
    /// 如果更新提交发生部分失败则需要恢复的旧安装记录。
    pub previous_install_record: InstalledSkillRecord,
}

/// One staged uninstall mutation prepared before the runtime reload is attempted.
/// 在尝试运行时重载之前准备好的单次卸载暂存变更。
#[derive(Debug, Clone)]
pub struct PreparedSkillUninstall {
    /// Structured uninstall result returned after the staged uninstall succeeds.
    /// 暂存卸载成功后返回的结构化卸载结果。
    pub result: SkillUninstallResult,
    /// Final target directory currently reserved for the installed skill.
    /// 当前为已安装技能保留的最终目标目录。
    pub target_dir: PathBuf,
    /// Backup directory that still contains the previous skill package until commit completes.
    /// 在提交完成前仍保存旧技能包的备份目录。
    pub backup_dir: Option<PathBuf>,
    /// Previous disabled-state record that should be restored if uninstall rollback is needed.
    /// 如果需要回滚卸载则应恢复的旧停用状态记录。
    pub previous_disabled_record: Option<DisabledSkillRecord>,
    /// Previous managed install record that should be restored if uninstall rollback is needed.
    /// 如果需要回滚卸载则应恢复的旧受管安装记录。
    pub previous_install_record: Option<InstalledSkillRecord>,
}

/// Optional database cleanup switches accepted by skill uninstall operations.
/// 技能卸载操作接受的可选数据库清理开关集合。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillUninstallOptions {
    /// Remove the SQLite database directory owned by the target skill when true.
    /// 为 true 时删除目标技能拥有的 SQLite 数据目录。
    #[serde(default)]
    pub remove_sqlite: bool,
    /// Remove the LanceDB database directory owned by the target skill when true.
    /// 为 true 时删除目标技能拥有的 LanceDB 数据目录。
    #[serde(default)]
    pub remove_lancedb: bool,
}

/// Structured uninstall result that reports whether code and databases were removed or retained.
/// 结构化卸载结果，用于报告代码与数据库是被删除还是被保留。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillUninstallResult {
    /// Stable skill identifier targeted by the current uninstall action.
    /// 当前卸载动作目标的稳定技能标识符。
    pub skill_id: String,
    /// Whether the skill package directory itself was removed.
    /// skill 包目录本身是否已经被删除。
    pub skill_removed: bool,
    /// Whether the SQLite database directory was removed explicitly.
    /// SQLite 数据目录是否已被显式删除。
    pub sqlite_removed: bool,
    /// Whether the LanceDB database directory was removed explicitly.
    /// LanceDB 数据目录是否已被显式删除。
    pub lancedb_removed: bool,
    /// Whether the SQLite database directory was intentionally retained.
    /// SQLite 数据目录是否被有意保留。
    pub sqlite_retained: bool,
    /// Whether the LanceDB database directory was intentionally retained.
    /// LanceDB 数据目录是否被有意保留。
    pub lancedb_retained: bool,
    /// Human-readable explanation of the uninstall result.
    /// 当前卸载结果的人类可读说明文本。
    pub message: String,
}

/// One resolved effective skill instance after applying root precedence rules.
/// 应用根目录优先级规则后得到的单个生效技能实例。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedSkillInstance {
    /// Stable skill identifier resolved from the directory name.
    /// 从目录名称解析出的稳定技能标识符。
    pub skill_id: String,
    /// Named skill root that currently owns the effective skill instance.
    /// 当前生效技能实例所属的命名技能根。
    pub root_name: String,
    /// Physical skills root directory that currently owns the effective skill instance.
    /// 当前生效技能实例所属的物理 skills 根目录。
    pub skills_root: PathBuf,
    /// Physical skill directory that is currently effective for the resolved skill id.
    /// 当前针对该技能标识符实际生效的物理技能目录。
    pub actual_dir: PathBuf,
}

/// Persistent record written when one skill is explicitly disabled.
/// 显式停用某个技能时写入的持久化记录。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DisabledSkillRecord {
    /// Stable skill identifier bound to this state record.
    /// 与当前状态记录绑定的稳定 skill 标识符。
    pub skill_id: String,
    /// Optional human-readable disable reason.
    /// 可选的人类可读停用原因。
    pub reason: Option<String>,
    /// Unix timestamp in milliseconds when the skill was disabled.
    /// 当前技能被停用时的 Unix 毫秒时间戳。
    pub disabled_at_unix_ms: u128,
}

/// Skill manager that owns persisted skill enabled/disabled state.
/// 持有技能启用/停用持久状态的技能管理器。
pub struct SkillManager {
    config: SkillManagerConfig,
    progress: Option<RuntimeSkillOperationProgressEmitter>,
}

/// Drop guard that removes one staging directory unless the caller explicitly disarms it.
/// 除非调用方显式解除，否则在析构时删除单个暂存目录的清理守卫。
struct TempDirGuard {
    /// Physical staging directory that should be removed on drop.
    /// 析构时应被移除的物理暂存目录。
    path: PathBuf,
    /// Whether automatic cleanup has been disabled explicitly.
    /// 是否已经被显式关闭自动清理。
    disarmed: bool,
}

impl TempDirGuard {
    /// Create one cleanup guard bound to one staging directory.
    /// 创建一个绑定到指定暂存目录的清理守卫。
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            disarmed: false,
        }
    }

    /// Disable automatic cleanup for the current staging directory.
    /// 为当前暂存目录关闭自动清理。
    fn disarm(&mut self) {
        self.disarmed = true;
    }
}

impl Drop for TempDirGuard {
    /// Remove the staging directory best-effort when the guard is still armed.
    /// 当守卫仍处于激活状态时，尽力移除暂存目录。
    fn drop(&mut self) {
        if !self.disarmed {
            let _ = remove_staging_dir_if_present(&self.path);
        }
    }
}

impl SkillManager {
    /// Create one skill manager from a shared configuration object.
    /// 基于共享配置对象创建一个技能管理器实例。
    pub fn new(config: SkillManagerConfig) -> Self {
        Self {
            config,
            progress: None,
        }
    }

    /// Create one skill manager with an operation-scoped progress emitter.
    /// 基于操作级进度发射器创建一个技能管理器实例。
    pub(crate) fn new_with_progress(
        config: SkillManagerConfig,
        progress: Option<RuntimeSkillOperationProgressEmitter>,
    ) -> Self {
        Self { config, progress }
    }

    /// Ensure the skill-state root and its child directories exist.
    /// 确保技能状态根目录及其子目录已经存在。
    pub fn ensure_state_layout(&self) -> Result<(), String> {
        fs::create_dir_all(self.disabled_root()).map_err(|error| {
            format!(
                "Failed to create disabled root {}: {}",
                render_skill_manager_path(&self.disabled_root()),
                error
            )
        })?;
        fs::create_dir_all(self.install_record_root()).map_err(|error| {
            format!(
                "Failed to create install-record root {}: {}",
                render_skill_manager_path(&self.install_record_root()),
                error
            )
        })
    }

    /// Validate one skill id and enforce the root-plane protection boundary.
    /// 校验单个 skill id 并执行根层级平面保护边界。
    pub fn guard_operation(
        &self,
        plane: SkillOperationPlane,
        action: SkillLifecycleAction,
        skill_id: &str,
    ) -> Result<(), String> {
        validate_luaskills_identifier(skill_id, "skill_id")?;
        if plane == SkillOperationPlane::Skills && is_root_skill_layer(&self.config.skill_root) {
            return Err(format!(
                "ROOT skill root is system-controlled and cannot be processed through the skills plane for action {:?}",
                action
            ));
        }
        Ok(())
    }

    /// Return whether one skill is currently enabled.
    /// 返回单个技能当前是否处于启用状态。
    pub fn is_skill_enabled(&self, skill_id: &str) -> Result<bool, String> {
        self.ensure_state_layout()?;
        Ok(!disabled_record_path_is_file(
            &self.disabled_record_path(skill_id),
        )?)
    }

    /// Persist one disabled-state marker for the specified skill.
    /// 为指定技能持久化一份停用状态标记。
    pub fn disable_skill(&self, skill_id: &str, reason: Option<&str>) -> Result<(), String> {
        self.disable_skill_in_plane(SkillOperationPlane::Skills, skill_id, reason)
    }

    /// Persist one disabled-state marker for the specified skill in the requested operation plane.
    /// 在指定操作平面为目标技能持久化一份停用状态标记。
    pub fn disable_skill_in_plane(
        &self,
        plane: SkillOperationPlane,
        skill_id: &str,
        reason: Option<&str>,
    ) -> Result<(), String> {
        self.guard_operation(plane, SkillLifecycleAction::Disable, skill_id)?;
        self.ensure_state_layout()?;
        let record = DisabledSkillRecord {
            skill_id: skill_id.to_string(),
            reason: reason
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
            disabled_at_unix_ms: current_unix_millis("disabled skill record timestamp")?,
        };
        let path = self.disabled_record_path(skill_id);
        let content = serde_json::to_string_pretty(&record)
            .map_err(|error| format!("Failed to serialize disabled record: {}", error))?;
        fs::write(&path, content).map_err(|error| {
            format!(
                "Failed to write {}: {}",
                render_skill_manager_path(&path),
                error
            )
        })
    }

    /// Remove the disabled-state marker for one skill.
    /// 删除单个技能的停用状态标记。
    pub fn enable_skill(&self, skill_id: &str) -> Result<(), String> {
        self.enable_skill_in_plane(SkillOperationPlane::Skills, skill_id)
    }

    /// Remove the disabled-state marker for one skill in the requested operation plane.
    /// 在指定操作平面移除单个技能的停用状态标记。
    pub fn enable_skill_in_plane(
        &self,
        plane: SkillOperationPlane,
        skill_id: &str,
    ) -> Result<(), String> {
        self.guard_operation(plane, SkillLifecycleAction::Enable, skill_id)?;
        self.ensure_state_layout()?;
        let path = self.disabled_record_path(skill_id);
        if disabled_record_path_is_file(&path)? {
            fs::remove_file(&path).map_err(|error| {
                format!(
                    "Failed to remove {}: {}",
                    render_skill_manager_path(&path),
                    error
                )
            })?;
        }
        Ok(())
    }

    /// Read the disabled-state record for one skill when it exists.
    /// 在停用状态记录存在时读取单个技能的停用状态记录。
    pub fn disabled_record(&self, skill_id: &str) -> Result<Option<DisabledSkillRecord>, String> {
        let path = self.disabled_record_path(skill_id);
        if !disabled_record_path_is_file(&path)? {
            return Ok(None);
        }
        let content = fs::read_to_string(&path).map_err(|error| {
            format!(
                "Failed to read {}: {}",
                render_skill_manager_path(&path),
                error
            )
        })?;
        let record = serde_json::from_str::<DisabledSkillRecord>(&content).map_err(|error| {
            format!(
                "Failed to parse {}: {}",
                render_skill_manager_path(&path),
                error
            )
        })?;
        Ok(Some(record))
    }

    /// Remove one installed skill directory and clear its disabled marker.
    /// 删除单个已安装 skill 目录，并清理其停用标记。
    pub fn uninstall_skill(&self, skill_id: &str) -> Result<SkillUninstallResult, String> {
        self.uninstall_skill_in_plane(SkillOperationPlane::Skills, skill_id)
    }

    /// Remove one installed skill directory and clear its disabled marker in the requested operation plane.
    /// 在指定操作平面删除单个已安装技能目录，并清理其停用标记。
    pub fn uninstall_skill_in_plane(
        &self,
        plane: SkillOperationPlane,
        skill_id: &str,
    ) -> Result<SkillUninstallResult, String> {
        let skill_dir = self.config.skill_root.skills_dir.join(skill_id);
        let prepared =
            self.prepare_uninstall_skill_at_path_in_plane(plane, skill_id, &skill_dir)?;
        self.commit_prepared_skill_uninstall(&prepared)
            .map_err(|error| {
                // Rollback result from restoring the staged uninstall after commit finalization fails.
                // 提交收尾失败后恢复已暂存卸载变更得到的回滚结果。
                let rollback_error = self.rollback_prepared_skill_uninstall(&prepared);
                format_uninstall_finalization_error(
                    format!("Failed to finalize uninstall: {}", error),
                    rollback_error,
                )
            })
    }

    /// Remove one installed skill directory at an explicitly resolved path and clear its disabled marker.
    /// 删除单个已解析物理路径上的技能目录，并清理其停用标记。
    pub fn uninstall_skill_at_path_in_plane(
        &self,
        plane: SkillOperationPlane,
        skill_id: &str,
        skill_dir: &Path,
    ) -> Result<SkillUninstallResult, String> {
        let prepared = self.prepare_uninstall_skill_at_path_in_plane(plane, skill_id, skill_dir)?;
        self.commit_prepared_skill_uninstall(&prepared)
            .map_err(|error| {
                // Rollback result from restoring the staged uninstall after commit finalization fails.
                // 提交收尾失败后恢复已暂存卸载变更得到的回滚结果。
                let rollback_error = self.rollback_prepared_skill_uninstall(&prepared);
                format_uninstall_finalization_error(
                    format!("Failed to finalize uninstall: {}", error),
                    rollback_error,
                )
            })
    }

    /// Prepare one uninstall request and stage filesystem changes without committing state deletions yet.
    /// 预处理单个卸载请求并暂存文件系统变更，但暂不提交状态删除。
    pub fn prepare_uninstall_skill_at_path_in_plane(
        &self,
        plane: SkillOperationPlane,
        skill_id: &str,
        skill_dir: &Path,
    ) -> Result<PreparedSkillUninstall, String> {
        self.guard_operation(plane, SkillLifecycleAction::Uninstall, skill_id)?;
        self.ensure_state_layout()?;
        let previous_disabled_record = self.disabled_record(skill_id)?;
        let previous_install_record = self.install_record(skill_id)?;
        let (skill_removed, backup_dir) = if skill_package_dir_is_directory(skill_dir)? {
            let timestamp = current_unix_millis("uninstall backup directory timestamp")?;
            let backup_dir = self
                .config
                .lifecycle_root
                .join("uninstall_backup")
                .join(format!("{}-{}", skill_id, timestamp));
            if let Some(parent) = backup_dir.parent() {
                fs::create_dir_all(parent).map_err(|error| {
                    format!(
                        "Failed to create {}: {}",
                        render_skill_manager_path(parent),
                        error
                    )
                })?;
            }
            fs::rename(skill_dir, &backup_dir).map_err(|error| {
                format!(
                    "Failed to move current skill {} into uninstall backup {}: {}",
                    render_skill_manager_path(skill_dir),
                    render_skill_manager_path(&backup_dir),
                    error
                )
            })?;
            (true, Some(backup_dir))
        } else {
            (false, None)
        };
        Ok(PreparedSkillUninstall {
            result: SkillUninstallResult {
                skill_id: skill_id.to_string(),
                skill_removed,
                sqlite_removed: false,
                lancedb_removed: false,
                sqlite_retained: false,
                lancedb_retained: false,
                message: if skill_removed {
                    "skill package removed".to_string()
                } else {
                    "skill package directory not found".to_string()
                },
            },
            target_dir: skill_dir.to_path_buf(),
            backup_dir,
            previous_disabled_record,
            previous_install_record,
        })
    }

    /// Prepare one install request and stage filesystem changes without committing the install record yet.
    /// 预处理单个安装请求并暂存文件系统变更，但暂不提交安装记录。
    pub fn prepare_install_skill(
        &self,
        plane: SkillOperationPlane,
        skill_roots: &[RuntimeSkillRoot],
        request: &SkillInstallRequest,
    ) -> Result<PreparedSkillApply, String> {
        let skill_id = resolve_requested_skill_id(request)?;
        self.guard_operation(plane, SkillLifecycleAction::Install, &skill_id)?;
        if resolve_declared_skill_instance_from_roots(skill_roots, &skill_id)?.is_some() {
            return Ok(PreparedSkillApply::Immediate(SkillApplyResult {
                skill_id,
                status: "already_installed".to_string(),
                message: "skill already exists; use update to evaluate upgrade behavior"
                    .to_string(),
                version: None,
                source_type: None,
                source_locator: None,
            }));
        }
        self.emit_progress_detail(RuntimeSkillOperationProgressDetail {
            phase: "resolving_source",
            status: "started",
            skill_id: Some(skill_id.as_str()),
            source_type: Some(request.source_type),
            source_locator: request.source.as_deref(),
            bytes_done: None,
            bytes_total: None,
            message: Some("resolving skill install source".to_string()),
        });
        match request.source_type {
            SkillInstallSourceType::Github => self.prepare_install_skill_from_github(&skill_id, request),
            SkillInstallSourceType::OfficialHub => self.prepare_install_skill_from_official_hub(&skill_id, request),
            SkillInstallSourceType::Url => Err(
                "public URL skill install is disabled by source policy; use github, official_hub, or a host-private system manifest"
                    .to_string(),
            ),
            SkillInstallSourceType::PrivateUrlManifest => {
                self.prepare_install_skill_from_private_url_manifest(plane, &skill_id, request)
            }
        }
    }

    /// Prepare one update request and stage filesystem changes without committing the new install record yet.
    /// 预处理单个更新请求并暂存文件系统变更，但暂不提交新的安装记录。
    pub fn prepare_update_skill(
        &self,
        plane: SkillOperationPlane,
        skill_roots: &[RuntimeSkillRoot],
        request: &SkillInstallRequest,
    ) -> Result<PreparedSkillApply, String> {
        let skill_id = resolve_requested_skill_id(request)?;
        self.guard_operation(plane, SkillLifecycleAction::Update, &skill_id)?;
        if resolve_declared_skill_instance_from_roots(skill_roots, &skill_id)?.is_none() {
            return Ok(PreparedSkillApply::Immediate(SkillApplyResult {
                skill_id,
                status: "missing_skill".to_string(),
                message: "skill is not installed; use install first".to_string(),
                version: None,
                source_type: None,
                source_locator: None,
            }));
        }
        self.prepare_managed_skill_update(plane, &skill_id)
    }

    /// Dispatch one managed update according to the persisted install source.
    /// 根据持久化安装来源分发单个受管更新。
    fn prepare_managed_skill_update(
        &self,
        plane: SkillOperationPlane,
        skill_id: &str,
    ) -> Result<PreparedSkillApply, String> {
        let record = self.install_record(skill_id)?.ok_or_else(|| {
            format!(
                "skill '{}' is not managed by the install workflow; automatic update is unavailable",
                skill_id
            )
        })?;
        if !record.managed {
            return Err(format!(
                "skill '{}' is not managed by the install workflow; automatic update is unavailable",
                skill_id
            ));
        }
        self.emit_progress_detail(RuntimeSkillOperationProgressDetail {
            phase: "resolving_source",
            status: "started",
            skill_id: Some(skill_id),
            source_type: Some(record.source.source_type),
            source_locator: Some(record.source.locator.as_str()),
            bytes_done: None,
            bytes_total: None,
            message: Some("resolving managed skill update source".to_string()),
        });
        match record.source.source_type {
            SkillInstallSourceType::Github => {
                self.prepare_github_managed_skill_update(skill_id, record)
            }
            SkillInstallSourceType::OfficialHub => {
                self.prepare_official_hub_managed_skill_update(skill_id, record)
            }
            SkillInstallSourceType::Url => Err(format!(
                "skill '{}' uses legacy public url source; automatic update is disabled by source policy",
                skill_id
            )),
            SkillInstallSourceType::PrivateUrlManifest => {
                self.prepare_private_url_manifest_managed_skill_update(plane, skill_id, record)
            }
        }
    }

    /// Stage one skill package install from the latest GitHub release of the declared repository.
    /// 从声明仓库的最新 GitHub release 暂存单个技能包安装。
    fn prepare_install_skill_from_github(
        &self,
        skill_id: &str,
        request: &SkillInstallRequest,
    ) -> Result<PreparedSkillApply, String> {
        let repo = normalize_github_repo_locator(
            request
                .source
                .as_deref()
                .ok_or_else(|| "github install requires source repository".to_string())?,
        )?;
        let repo_skill_id = github_repo_skill_id(&repo)?;
        if repo_skill_id != skill_id {
            return Err(format!(
                "github repository '{}' resolves to skill_id '{}' but the request targets '{}'",
                repo, repo_skill_id, skill_id
            ));
        }

        let downloader = self.downloader();
        let asset = downloader.resolve_github_managed_skill_release_asset(
            &crate::skill::dependencies::GithubReleaseSourceSpec {
                repo: repo.clone(),
                tag_api: None,
            },
            skill_id,
            None,
        )?;
        let archive_downloader =
            self.downloader_for_skill_progress(skill_id, SkillInstallSourceType::Github);
        let archive_path = archive_downloader.download_with_sha256(
            &crate::download::manager::DownloadRequest {
                source_type: crate::dependency::types::DependencySourceType::GithubRelease,
                source_locator: asset.download_url.clone(),
                cache_key: managed_skill_cache_key(skill_id, asset.version.as_str()),
            },
            asset.sha256.as_deref().ok_or_else(|| {
                format!(
                    "GitHub release '{}' does not expose one SHA-256 checksum for '{}'",
                    asset.tag_name, asset.asset_name
                )
            })?,
        )?;
        self.stage_skill_install_from_archive(
            skill_id,
            &archive_path,
            asset.version.as_str(),
            InstalledSkillSourceRecord {
                source_type: SkillInstallSourceType::Github,
                locator: repo.clone(),
                tag: Some(asset.tag_name.clone()),
            },
            format!(
                "skill '{}' version {} was installed from GitHub repository '{}'",
                skill_id, asset.version, repo
            ),
        )
    }

    /// Stage one managed GitHub-installed skill update by comparing the latest release tag with the current installed version.
    /// 通过比较最新 release 标签与当前已安装版本来暂存单个 GitHub 受管技能更新。
    fn prepare_github_managed_skill_update(
        &self,
        skill_id: &str,
        record: InstalledSkillRecord,
    ) -> Result<PreparedSkillApply, String> {
        let current_version = Version::parse(record.version.as_str()).map_err(|error| {
            format!(
                "installed version '{}' of skill '{}' is invalid: {}",
                record.version, skill_id, error
            )
        })?;
        let downloader = self.downloader();
        let asset = downloader.resolve_github_managed_skill_release_asset(
            &crate::skill::dependencies::GithubReleaseSourceSpec {
                repo: record.source.locator.clone(),
                tag_api: None,
            },
            skill_id,
            None,
        )?;
        let latest_version = Version::parse(asset.version.as_str()).map_err(|error| {
            format!(
                "latest GitHub release version '{}' of skill '{}' is invalid: {}",
                asset.version, skill_id, error
            )
        })?;
        if latest_version <= current_version {
            return Ok(PreparedSkillApply::Immediate(SkillApplyResult {
                skill_id: skill_id.to_string(),
                status: "up_to_date".to_string(),
                message: format!(
                    "skill '{}' is already on version {}",
                    skill_id, record.version
                ),
                version: Some(record.version),
                source_type: Some(SkillInstallSourceType::Github),
                source_locator: Some(record.source.locator),
            }));
        }

        let archive_downloader =
            self.downloader_for_skill_progress(skill_id, SkillInstallSourceType::Github);
        let archive_path = archive_downloader.download_with_sha256(
            &crate::download::manager::DownloadRequest {
                source_type: crate::dependency::types::DependencySourceType::GithubRelease,
                source_locator: asset.download_url.clone(),
                cache_key: managed_skill_cache_key(skill_id, asset.version.as_str()),
            },
            asset.sha256.as_deref().ok_or_else(|| {
                format!(
                    "GitHub release '{}' does not expose one SHA-256 checksum for '{}'",
                    asset.tag_name, asset.asset_name
                )
            })?,
        )?;
        let source_locator = record.source.locator.clone();
        self.stage_skill_update_from_archive(
            skill_id,
            &archive_path,
            asset.version.as_str(),
            record,
            InstalledSkillSourceRecord {
                source_type: SkillInstallSourceType::Github,
                locator: source_locator,
                tag: Some(asset.tag_name.clone()),
            },
        )
    }

    /// Stage one skill package install from the configured official LuaSkills Hub.
    /// 从已配置的官方 LuaSkills Hub 暂存单个技能包安装。
    fn prepare_install_skill_from_official_hub(
        &self,
        skill_id: &str,
        request: &SkillInstallRequest,
    ) -> Result<PreparedSkillApply, String> {
        let hub_locator = request
            .source
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(skill_id);
        validate_luaskills_identifier(hub_locator, "official hub source skill_id")?;
        if hub_locator != skill_id {
            return Err(format!(
                "official_hub source '{}' does not match requested skill_id '{}'",
                hub_locator, skill_id
            ));
        }
        let manifest = self.fetch_official_hub_manifest(hub_locator)?;
        self.prepare_install_skill_from_manifest(
            skill_id,
            manifest,
            SkillInstallSourceType::OfficialHub,
            hub_locator,
        )
    }

    /// Stage one skill package install from a host-private URL manifest.
    /// 从宿主私有 URL manifest 暂存单个技能包安装。
    fn prepare_install_skill_from_private_url_manifest(
        &self,
        plane: SkillOperationPlane,
        skill_id: &str,
        request: &SkillInstallRequest,
    ) -> Result<PreparedSkillApply, String> {
        let manifest_url = request
            .source
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| "private_url_manifest install requires one manifest URL".to_string())?;
        self.ensure_private_url_manifest_allowed(plane, manifest_url)?;
        let manifest = self.fetch_private_url_manifest(manifest_url)?;
        self.ensure_private_archive_url_allowed(manifest.archive.url.as_str())?;
        self.prepare_install_skill_from_manifest(
            skill_id,
            manifest,
            SkillInstallSourceType::PrivateUrlManifest,
            manifest_url,
        )
    }

    /// Stage one managed official-Hub update by comparing the resolved version with the installed version.
    /// 通过比较 Hub 解析版本与已安装版本暂存单个官方 Hub 受管更新。
    fn prepare_official_hub_managed_skill_update(
        &self,
        skill_id: &str,
        record: InstalledSkillRecord,
    ) -> Result<PreparedSkillApply, String> {
        let manifest = self.fetch_official_hub_manifest(record.source.locator.as_str())?;
        self.prepare_update_skill_from_manifest(
            skill_id,
            record,
            manifest,
            SkillInstallSourceType::OfficialHub,
        )
    }

    /// Stage one managed private-URL-manifest update for trusted system operations.
    /// 为可信 system 操作暂存单个私有 URL manifest 受管更新。
    fn prepare_private_url_manifest_managed_skill_update(
        &self,
        plane: SkillOperationPlane,
        skill_id: &str,
        record: InstalledSkillRecord,
    ) -> Result<PreparedSkillApply, String> {
        self.ensure_private_url_manifest_allowed(plane, record.source.locator.as_str())?;
        let manifest = self.fetch_private_url_manifest(record.source.locator.as_str())?;
        self.ensure_private_archive_url_allowed(manifest.archive.url.as_str())?;
        self.prepare_update_skill_from_manifest(
            skill_id,
            record,
            manifest,
            SkillInstallSourceType::PrivateUrlManifest,
        )
    }

    /// Resolve, download, and stage one install from a validated source manifest.
    /// 从已校验的来源 manifest 解析、下载并暂存单个安装。
    fn prepare_install_skill_from_manifest(
        &self,
        skill_id: &str,
        manifest: SkillSourceManifest,
        source_type: SkillInstallSourceType,
        source_locator: &str,
    ) -> Result<PreparedSkillApply, String> {
        let expected_sha256 = manifest.validate_for_skill(skill_id)?;
        self.emit_progress_detail(RuntimeSkillOperationProgressDetail {
            phase: "source_resolved",
            status: "completed",
            skill_id: Some(skill_id),
            source_type: Some(source_type),
            source_locator: Some(source_locator),
            bytes_done: None,
            bytes_total: None,
            message: Some(format!("resolved skill version {}", manifest.version)),
        });
        let archive_downloader = self.downloader_for_skill_progress(skill_id, source_type);
        let archive_path = archive_downloader.download_with_sha256(
            &crate::download::manager::DownloadRequest {
                source_type: crate::dependency::types::DependencySourceType::Url,
                source_locator: manifest.archive.url.clone(),
                cache_key: managed_skill_cache_key(skill_id, manifest.version.as_str()),
            },
            expected_sha256.as_str(),
        )?;
        self.stage_skill_install_from_archive(
            skill_id,
            &archive_path,
            manifest.version.as_str(),
            source_record_from_manifest(&manifest, source_type, source_locator),
            format!(
                "skill '{}' version {} was installed from {:?} '{}'",
                skill_id, manifest.version, source_type, source_locator
            ),
        )
    }

    /// Resolve, download, and stage one update from a validated source manifest.
    /// 从已校验的来源 manifest 解析、下载并暂存单个更新。
    fn prepare_update_skill_from_manifest(
        &self,
        skill_id: &str,
        record: InstalledSkillRecord,
        manifest: SkillSourceManifest,
        source_type: SkillInstallSourceType,
    ) -> Result<PreparedSkillApply, String> {
        let expected_sha256 = manifest.validate_for_skill(skill_id)?;
        let current_version = Version::parse(record.version.as_str()).map_err(|error| {
            format!(
                "installed version '{}' of skill '{}' is invalid: {}",
                record.version, skill_id, error
            )
        })?;
        let latest_version = Version::parse(manifest.version.as_str()).map_err(|error| {
            format!(
                "resolved version '{}' of skill '{}' is invalid: {}",
                manifest.version, skill_id, error
            )
        })?;
        if latest_version <= current_version {
            return Ok(PreparedSkillApply::Immediate(SkillApplyResult {
                skill_id: skill_id.to_string(),
                status: "up_to_date".to_string(),
                message: format!(
                    "skill '{}' is already on version {}",
                    skill_id, record.version
                ),
                version: Some(record.version),
                source_type: Some(source_type),
                source_locator: Some(record.source.locator),
            }));
        }
        self.emit_progress_detail(RuntimeSkillOperationProgressDetail {
            phase: "source_resolved",
            status: "completed",
            skill_id: Some(skill_id),
            source_type: Some(source_type),
            source_locator: Some(record.source.locator.as_str()),
            bytes_done: None,
            bytes_total: None,
            message: Some(format!("resolved skill version {}", manifest.version)),
        });
        let archive_downloader = self.downloader_for_skill_progress(skill_id, source_type);
        let archive_path = archive_downloader.download_with_sha256(
            &crate::download::manager::DownloadRequest {
                source_type: crate::dependency::types::DependencySourceType::Url,
                source_locator: manifest.archive.url.clone(),
                cache_key: managed_skill_cache_key(skill_id, manifest.version.as_str()),
            },
            expected_sha256.as_str(),
        )?;
        let source_locator = record.source.locator.clone();
        self.stage_skill_update_from_archive(
            skill_id,
            &archive_path,
            manifest.version.as_str(),
            record,
            source_record_from_manifest(&manifest, source_type, source_locator.as_str()),
        )
    }

    /// Fetch one official Hub resolve manifest for a skill id.
    /// 获取单个技能标识符对应的官方 Hub resolve manifest。
    fn fetch_official_hub_manifest(&self, skill_id: &str) -> Result<SkillSourceManifest, String> {
        validate_luaskills_identifier(skill_id, "official hub skill_id")?;
        let base_url = self
            .config
            .official_skill_hub_base_url
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                "official_hub install requires host option official_skill_hub_base_url".to_string()
            })?;
        let resolve_url = format!(
            "{}/api/v1/skills/{}/resolve?version=latest",
            base_url.trim_end_matches('/'),
            skill_id
        );
        self.emit_progress_detail(RuntimeSkillOperationProgressDetail {
            phase: "fetching_manifest",
            status: "started",
            skill_id: Some(skill_id),
            source_type: Some(SkillInstallSourceType::OfficialHub),
            source_locator: Some(resolve_url.as_str()),
            bytes_done: None,
            bytes_total: None,
            message: Some("fetching official Hub resolve manifest".to_string()),
        });
        let text = self.downloader().fetch_text_fresh(
            resolve_url.as_str(),
            &source_manifest_cache_key("official-hub", skill_id, resolve_url.as_str()),
        )?;
        parse_skill_source_manifest(text.as_str(), resolve_url.as_str())
    }

    /// Fetch one private URL manifest after policy checks have accepted its URL.
    /// 在策略检查接受 URL 后获取单个私有 URL manifest。
    fn fetch_private_url_manifest(
        &self,
        manifest_url: &str,
    ) -> Result<SkillSourceManifest, String> {
        self.emit_progress_detail(RuntimeSkillOperationProgressDetail {
            phase: "fetching_manifest",
            status: "started",
            skill_id: None,
            source_type: Some(SkillInstallSourceType::PrivateUrlManifest),
            source_locator: Some(manifest_url),
            bytes_done: None,
            bytes_total: None,
            message: Some("fetching private URL skill manifest".to_string()),
        });
        let text = self.downloader().fetch_text_fresh(
            manifest_url,
            &source_manifest_cache_key("private-url", "manifest", manifest_url),
        )?;
        parse_skill_source_manifest(text.as_str(), manifest_url)
    }

    /// Ensure one private URL manifest request satisfies the host source policy.
    /// 确保单个私有 URL manifest 请求满足宿主来源策略。
    fn ensure_private_url_manifest_allowed(
        &self,
        plane: SkillOperationPlane,
        manifest_url: &str,
    ) -> Result<(), String> {
        if plane != SkillOperationPlane::System {
            return Err(
                "private_url_manifest install is restricted to host system authority".to_string(),
            );
        }
        if !self.config.enable_private_url_skill_install {
            return Err(
                "private_url_manifest install is disabled by host source policy".to_string(),
            );
        }
        if !is_allowed_private_source_url(manifest_url, &self.config.private_skill_source_allowlist)
        {
            return Err(format!(
                "private_url_manifest source '{}' is not allowed by host source policy",
                manifest_url
            ));
        }
        Ok(())
    }

    /// Ensure one private manifest archive URL also satisfies the host source policy.
    /// 确保单个私有 manifest 归档 URL 同样满足宿主来源策略。
    fn ensure_private_archive_url_allowed(&self, archive_url: &str) -> Result<(), String> {
        if !is_allowed_private_source_url(archive_url, &self.config.private_skill_source_allowlist)
        {
            return Err(format!(
                "private_url_manifest archive '{}' is not allowed by host source policy",
                archive_url
            ));
        }
        Ok(())
    }

    /// Stage one validated archive as a new skill installation.
    /// 将单个已校验归档暂存为新的技能安装。
    fn stage_skill_install_from_archive(
        &self,
        skill_id: &str,
        archive_path: &Path,
        version: &str,
        source: InstalledSkillSourceRecord,
        message: String,
    ) -> Result<PreparedSkillApply, String> {
        self.emit_progress(
            "extracting_archive",
            "started",
            Some("extracting skill archive"),
        );
        let timestamp = current_unix_millis("install temp directory timestamp")?;
        let install_temp_root = self
            .config
            .lifecycle_root
            .join("install_tmp")
            .join(format!("{}-{}", skill_id, timestamp));
        if staging_temp_root_is_directory(&install_temp_root)? {
            fs::remove_dir_all(&install_temp_root).map_err(|error| {
                format!(
                    "Failed to remove stale temp install root {}: {}",
                    render_skill_manager_path(&install_temp_root),
                    error
                )
            })?;
        }
        fs::create_dir_all(&install_temp_root).map_err(|error| {
            format!(
                "Failed to create temp install root {}: {}",
                render_skill_manager_path(&install_temp_root),
                error
            )
        })?;
        let mut install_temp_guard = TempDirGuard::new(install_temp_root.clone());

        let extracted_skill_dir =
            extract_skill_package_zip(archive_path, &install_temp_root, skill_id)?;
        self.emit_progress(
            "validating_skill_manifest",
            "started",
            Some("validating extracted skill manifest"),
        );
        let installed_meta = read_skill_manifest_from_directory(&extracted_skill_dir)?;
        if installed_meta.effective_skill_id() != skill_id {
            return Err(format!(
                "downloaded skill package resolves to skill_id '{}' instead of '{}'",
                installed_meta.effective_skill_id(),
                skill_id
            ));
        }
        if installed_meta.version() != version {
            return Err(format!(
                "downloaded skill package version '{}' does not match resolved version '{}'",
                installed_meta.version(),
                version
            ));
        }

        self.emit_progress(
            "staging_install",
            "started",
            Some("moving skill into target root"),
        );
        let target_dir = self.skill_root().join(skill_id);
        if skill_package_dir_is_directory(&target_dir)? {
            return Err(format!(
                "target skill directory {} already exists",
                render_skill_manager_path(&target_dir)
            ));
        }
        fs::rename(&extracted_skill_dir, &target_dir).map_err(|error| {
            format!(
                "Failed to move extracted skill {} into {}: {}",
                render_skill_manager_path(&extracted_skill_dir),
                render_skill_manager_path(&target_dir),
                error
            )
        })?;
        install_temp_guard.disarm();
        let _ = fs::remove_dir_all(&install_temp_root);

        let source_type = source.source_type;
        let source_locator = source.locator.clone();
        let record = InstalledSkillRecord {
            skill_id: skill_id.to_string(),
            version: version.to_string(),
            managed: true,
            source,
            installed_at_unix_ms: current_unix_millis("installed skill record timestamp")?,
        };
        Ok(PreparedSkillApply::Install(PreparedSkillInstall {
            result: SkillApplyResult {
                skill_id: skill_id.to_string(),
                status: "installed".to_string(),
                message,
                version: Some(version.to_string()),
                source_type: Some(source_type),
                source_locator: Some(source_locator),
            },
            target_dir,
            install_record: record,
        }))
    }

    /// Stage one validated archive as an update for an installed skill.
    /// 将单个已校验归档暂存为已安装技能的更新。
    fn stage_skill_update_from_archive(
        &self,
        skill_id: &str,
        archive_path: &Path,
        version: &str,
        previous_record: InstalledSkillRecord,
        source: InstalledSkillSourceRecord,
    ) -> Result<PreparedSkillApply, String> {
        self.emit_progress(
            "extracting_archive",
            "started",
            Some("extracting skill archive"),
        );
        let timestamp = current_unix_millis("update temp directory timestamp")?;
        let temp_root = self
            .config
            .lifecycle_root
            .join("update_tmp")
            .join(format!("{}-{}", skill_id, timestamp));
        if staging_temp_root_is_directory(&temp_root)? {
            fs::remove_dir_all(&temp_root).map_err(|error| {
                format!(
                    "Failed to remove stale temp update root {}: {}",
                    render_skill_manager_path(&temp_root),
                    error
                )
            })?;
        }
        fs::create_dir_all(&temp_root).map_err(|error| {
            format!(
                "Failed to create temp update root {}: {}",
                render_skill_manager_path(&temp_root),
                error
            )
        })?;
        let mut update_temp_guard = TempDirGuard::new(temp_root.clone());
        let extracted_skill_dir = extract_skill_package_zip(archive_path, &temp_root, skill_id)?;
        self.emit_progress(
            "validating_skill_manifest",
            "started",
            Some("validating extracted skill manifest"),
        );
        let updated_meta = read_skill_manifest_from_directory(&extracted_skill_dir)?;
        if updated_meta.effective_skill_id() != skill_id {
            return Err(format!(
                "downloaded update package resolves to skill_id '{}' instead of '{}'",
                updated_meta.effective_skill_id(),
                skill_id
            ));
        }
        if updated_meta.version() != version {
            return Err(format!(
                "downloaded update package version '{}' does not match resolved version '{}'",
                updated_meta.version(),
                version
            ));
        }

        self.emit_progress(
            "backing_up_current",
            "started",
            Some("backing up current skill package"),
        );
        let target_dir = self.skill_root().join(skill_id);
        if !skill_package_dir_is_directory(&target_dir)? {
            return Err(format!(
                "installed skill directory {} does not exist",
                render_skill_manager_path(&target_dir)
            ));
        }
        let backup_timestamp = current_unix_millis("update backup directory timestamp")?;
        let backup_dir = self
            .config
            .lifecycle_root
            .join("update_backup")
            .join(format!("{}-{}", skill_id, backup_timestamp));
        if let Some(parent) = backup_dir.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                format!(
                    "Failed to create {}: {}",
                    render_skill_manager_path(parent),
                    error
                )
            })?;
        }
        fs::rename(&target_dir, &backup_dir).map_err(|error| {
            format!(
                "Failed to move current skill {} into backup {}: {}",
                render_skill_manager_path(&target_dir),
                render_skill_manager_path(&backup_dir),
                error
            )
        })?;
        self.emit_progress(
            "staging_update",
            "started",
            Some("moving updated skill into place"),
        );
        if let Err(error) = fs::rename(&extracted_skill_dir, &target_dir) {
            let _ = fs::rename(&backup_dir, &target_dir);
            return Err(format!(
                "Failed to move updated skill {} into {}: {}",
                render_skill_manager_path(&extracted_skill_dir),
                render_skill_manager_path(&target_dir),
                error
            ));
        }
        update_temp_guard.disarm();
        let _ = fs::remove_dir_all(&temp_root);

        let source_type = source.source_type;
        let source_locator = source.locator.clone();
        let updated_record = InstalledSkillRecord {
            skill_id: skill_id.to_string(),
            version: version.to_string(),
            managed: true,
            source,
            installed_at_unix_ms: current_unix_millis("updated skill record timestamp")?,
        };
        Ok(PreparedSkillApply::Update(PreparedSkillUpdate {
            result: SkillApplyResult {
                skill_id: skill_id.to_string(),
                status: "updated".to_string(),
                message: format!(
                    "skill '{}' was updated from version {} to {}",
                    skill_id, previous_record.version, version
                ),
                version: Some(version.to_string()),
                source_type: Some(source_type),
                source_locator: Some(source_locator),
            },
            target_dir,
            backup_dir,
            install_record: updated_record,
            previous_install_record: previous_record,
        }))
    }

    /// Return the configured installed skill root.
    /// 返回当前配置中的已安装技能根目录。
    pub fn skill_root(&self) -> &Path {
        &self.config.skill_root.skills_dir
    }

    /// Return the configured skill-state root.
    /// 返回当前配置中的技能状态根目录。
    pub fn state_root(&self) -> &Path {
        self.config.lifecycle_root.as_path()
    }

    /// Return the root directory used to store managed install records.
    /// 返回用于存放受管安装记录的根目录。
    fn install_record_root(&self) -> PathBuf {
        self.config.lifecycle_root.join("installs")
    }

    /// Return the root directory used to store disabled-state markers.
    /// 返回用于存放停用状态标记的根目录。
    fn disabled_root(&self) -> PathBuf {
        self.config.lifecycle_root.join("skills").join("disabled")
    }

    /// Return the JSON state file path used by one disabled skill.
    /// 返回单个已停用技能对应的 JSON 状态文件路径。
    fn disabled_record_path(&self, skill_id: &str) -> PathBuf {
        self.disabled_root().join(format!("{}.json", skill_id))
    }

    /// Return the YAML install-record path used by one managed skill.
    /// 返回单个受管技能使用的 YAML 安装记录路径。
    fn install_record_path(&self, skill_id: &str) -> PathBuf {
        self.install_record_root()
            .join(format!("{}.yaml", skill_id))
    }

    /// Read one managed install record from disk when it exists.
    /// 在受管安装记录存在时从磁盘读取该记录。
    pub fn install_record(&self, skill_id: &str) -> Result<Option<InstalledSkillRecord>, String> {
        validate_luaskills_identifier(skill_id, "skill_id")?;
        let path = self.install_record_path(skill_id);
        if !install_record_path_is_file(&path)? {
            return Ok(None);
        }
        let yaml = fs::read_to_string(&path).map_err(|error| {
            format!(
                "Failed to read {}: {}",
                render_skill_manager_path(&path),
                error
            )
        })?;
        let record: InstalledSkillRecord = serde_yaml::from_str(&yaml).map_err(|error| {
            format!(
                "Failed to parse {}: {}",
                render_skill_manager_path(&path),
                error
            )
        })?;
        Ok(Some(record))
    }

    /// Persist one managed install record to disk.
    /// 将单个受管安装记录持久化到磁盘。
    fn persist_install_record(&self, record: &InstalledSkillRecord) -> Result<(), String> {
        self.ensure_state_layout()?;
        let path = self.install_record_path(&record.skill_id);
        let yaml = serde_yaml::to_string(record)
            .map_err(|error| format!("Failed to serialize install record: {}", error))?;
        fs::write(&path, yaml).map_err(|error| {
            format!(
                "Failed to write {}: {}",
                render_skill_manager_path(&path),
                error
            )
        })
    }

    /// Remove one managed install record from disk and report whether it existed.
    /// 从磁盘删除单个受管安装记录，并返回它是否存在。
    fn remove_install_record(&self, skill_id: &str) -> Result<bool, String> {
        validate_luaskills_identifier(skill_id, "skill_id")?;
        let path = self.install_record_path(skill_id);
        if !install_record_path_is_file(&path)? {
            return Ok(false);
        }
        fs::remove_file(&path).map_err(|error| {
            format!(
                "Failed to remove {}: {}",
                render_skill_manager_path(&path),
                error
            )
        })?;
        Ok(true)
    }

    /// Persist one disabled-state record exactly as captured before a staged mutation.
    /// 按暂存变更前捕获的原样持久化单个停用状态记录。
    fn persist_disabled_record(&self, record: &DisabledSkillRecord) -> Result<(), String> {
        self.ensure_state_layout()?;
        let path = self.disabled_record_path(&record.skill_id);
        let content = serde_json::to_string_pretty(record)
            .map_err(|error| format!("Failed to serialize disabled record: {}", error))?;
        fs::write(&path, content).map_err(|error| {
            format!(
                "Failed to write {}: {}",
                render_skill_manager_path(&path),
                error
            )
        })
    }

    /// Remove one disabled-state record from disk and report whether it existed.
    /// 从磁盘删除单个停用状态记录，并返回它是否存在。
    fn remove_disabled_record(&self, skill_id: &str) -> Result<bool, String> {
        validate_luaskills_identifier(skill_id, "skill_id")?;
        self.ensure_state_layout()?;
        let path = self.disabled_record_path(skill_id);
        if !disabled_record_path_is_file(&path)? {
            return Ok(false);
        }
        fs::remove_file(&path).map_err(|error| {
            format!(
                "Failed to remove {}: {}",
                render_skill_manager_path(&path),
                error
            )
        })?;
        Ok(true)
    }

    /// Restore one previous disabled-state snapshot or remove the current record when no snapshot existed.
    /// 恢复单个旧停用状态快照，若原先不存在快照则删除当前记录。
    fn restore_disabled_record(
        &self,
        skill_id: &str,
        record: Option<&DisabledSkillRecord>,
    ) -> Result<(), String> {
        match record {
            Some(record) => self.persist_disabled_record(record),
            None => {
                self.remove_disabled_record(skill_id)?;
                Ok(())
            }
        }
    }

    /// Restore one previous install-record snapshot or remove the current record when no snapshot existed.
    /// 恢复单个旧安装记录快照，若原先不存在快照则删除当前记录。
    fn restore_install_record(
        &self,
        skill_id: &str,
        record: Option<&InstalledSkillRecord>,
    ) -> Result<(), String> {
        match record {
            Some(record) => self.persist_install_record(record),
            None => {
                self.remove_install_record(skill_id)?;
                Ok(())
            }
        }
    }

    /// Persist the final install record and remove transitional backup data after runtime reload succeeds.
    /// 在运行时重载成功后持久化最终安装记录，并移除过渡备份数据。
    pub fn commit_prepared_skill_apply(
        &self,
        prepared: &PreparedSkillApply,
    ) -> Result<SkillApplyResult, String> {
        match prepared {
            PreparedSkillApply::Immediate(result) => Ok(result.clone()),
            PreparedSkillApply::Install(prepared_install) => {
                self.persist_install_record(&prepared_install.install_record)?;
                Ok(prepared_install.result.clone())
            }
            PreparedSkillApply::Update(prepared_update) => {
                self.persist_install_record(&prepared_update.install_record)?;
                match skill_package_dir_is_directory(&prepared_update.backup_dir) {
                    Ok(true) => {
                        fs::remove_dir_all(&prepared_update.backup_dir).map_err(|error| {
                            let restore_error =
                                self.persist_install_record(&prepared_update.previous_install_record);
                            match restore_error {
                                Ok(()) => format!(
                                    "Failed to remove update backup {}: previous install record was restored: {}",
                                    render_skill_manager_path(&prepared_update.backup_dir),
                                    error
                                ),
                                Err(restore_error) => format!(
                                    "Failed to remove update backup {}: {}. Failed to restore previous install record: {}",
                                    render_skill_manager_path(&prepared_update.backup_dir),
                                    error,
                                    restore_error
                                ),
                            }
                        })?;
                    }
                    Ok(false) => {}
                    Err(error) => {
                        return match self
                            .persist_install_record(&prepared_update.previous_install_record)
                        {
                            Ok(()) => {
                                Err(format!("{}: previous install record was restored", error))
                            }
                            Err(restore_error) => Err(format!(
                                "{}. Failed to restore previous install record: {}",
                                error, restore_error
                            )),
                        };
                    }
                }
                Ok(prepared_update.result.clone())
            }
        }
    }

    /// Roll back one staged install/update mutation after reload or commit fails.
    /// 在重载或提交失败后回滚一次已暂存的安装或更新变更。
    pub fn rollback_prepared_skill_apply(
        &self,
        prepared: &PreparedSkillApply,
    ) -> Result<(), String> {
        match prepared {
            PreparedSkillApply::Immediate(_) => Ok(()),
            PreparedSkillApply::Install(prepared_install) => {
                if skill_package_dir_is_directory(&prepared_install.target_dir)? {
                    fs::remove_dir_all(&prepared_install.target_dir).map_err(|error| {
                        format!(
                            "Failed to roll back installed skill directory {}: {}",
                            render_skill_manager_path(&prepared_install.target_dir),
                            error
                        )
                    })?;
                }
                Ok(())
            }
            PreparedSkillApply::Update(prepared_update) => {
                // Probe both rollback directories before deleting anything so a broken backup path cannot strand the staged target.
                // 在删除任何内容前先探测两个回滚目录，避免备份路径异常时让暂存目标处于不可恢复状态。
                let target_dir_exists =
                    skill_package_dir_is_directory(&prepared_update.target_dir)?;
                // Whether the previous package backup is available before rollback starts.
                // 回滚开始前旧包备份目录是否可用。
                let backup_dir_exists =
                    skill_package_dir_is_directory(&prepared_update.backup_dir)?;
                if target_dir_exists {
                    fs::remove_dir_all(&prepared_update.target_dir).map_err(|error| {
                        format!(
                            "Failed to remove staged updated skill directory {}: {}",
                            render_skill_manager_path(&prepared_update.target_dir),
                            error
                        )
                    })?;
                }
                if backup_dir_exists {
                    fs::rename(&prepared_update.backup_dir, &prepared_update.target_dir).map_err(
                        |error| {
                            format!(
                                "Failed to restore backup {} into {}: {}",
                                render_skill_manager_path(&prepared_update.backup_dir),
                                render_skill_manager_path(&prepared_update.target_dir),
                                error
                            )
                        },
                    )?;
                }
                Ok(())
            }
        }
    }

    /// Persist the final uninstall state and remove transitional backup data after runtime reload succeeds.
    /// 在运行时重载成功后持久化最终卸载状态，并移除过渡备份数据。
    pub fn commit_prepared_skill_uninstall(
        &self,
        prepared: &PreparedSkillUninstall,
    ) -> Result<SkillUninstallResult, String> {
        if prepared.previous_disabled_record.is_some() {
            self.remove_disabled_record(&prepared.result.skill_id)?;
        }
        if prepared.previous_install_record.is_some() {
            self.remove_install_record(&prepared.result.skill_id)?;
        }
        if let Some(backup_dir) = &prepared.backup_dir {
            fs::remove_dir_all(backup_dir).map_err(|error| {
                let disabled_restore_error = self.restore_disabled_record(
                    &prepared.result.skill_id,
                    prepared.previous_disabled_record.as_ref(),
                );
                let install_restore_error = self.restore_install_record(
                    &prepared.result.skill_id,
                    prepared.previous_install_record.as_ref(),
                );
                let mut message = format!(
                    "Failed to remove uninstall backup {}: {}",
                    render_skill_manager_path(backup_dir),
                    error
                );
                if let Err(restore_error) = disabled_restore_error {
                    message.push_str(&format!(
                        ". Failed to restore previous disabled record: {}",
                        restore_error
                    ));
                }
                if let Err(restore_error) = install_restore_error {
                    message.push_str(&format!(
                        ". Failed to restore previous install record: {}",
                        restore_error
                    ));
                }
                message
            })?;
        }
        Ok(prepared.result.clone())
    }

    /// Roll back one staged uninstall mutation after reload or commit fails.
    /// 在重载或提交失败后回滚一次已暂存的卸载变更。
    pub fn rollback_prepared_skill_uninstall(
        &self,
        prepared: &PreparedSkillUninstall,
    ) -> Result<(), String> {
        if let Some(backup_dir) = &prepared.backup_dir {
            // Probe both uninstall rollback directories before deleting anything so a broken backup path cannot strand the staged target.
            // 在删除任何内容前先探测两个卸载回滚目录，避免备份路径异常时让暂存目标处于不可恢复状态。
            let target_dir_exists = skill_package_dir_is_directory(&prepared.target_dir)?;
            // Whether the previous package backup is available before uninstall rollback starts.
            // 卸载回滚开始前旧包备份目录是否可用。
            let backup_dir_exists = skill_package_dir_is_directory(backup_dir)?;
            if target_dir_exists {
                fs::remove_dir_all(&prepared.target_dir).map_err(|error| {
                    format!(
                        "Failed to remove staged uninstall target directory {}: {}",
                        render_skill_manager_path(&prepared.target_dir),
                        error
                    )
                })?;
            }
            if backup_dir_exists {
                fs::rename(backup_dir, &prepared.target_dir).map_err(|error| {
                    format!(
                        "Failed to restore uninstall backup {} into {}: {}",
                        render_skill_manager_path(backup_dir),
                        render_skill_manager_path(&prepared.target_dir),
                        error
                    )
                })?;
            }
        }
        self.restore_disabled_record(
            &prepared.result.skill_id,
            prepared.previous_disabled_record.as_ref(),
        )?;
        self.restore_install_record(
            &prepared.result.skill_id,
            prepared.previous_install_record.as_ref(),
        )?;
        Ok(())
    }

    /// Build one downloader configured for managed install and update flows.
    /// 为受管安装与更新流程构造单个下载器。
    fn downloader(&self) -> DownloadManager {
        DownloadManager::new(DownloadManagerConfig {
            cache_root: self.config.download_cache_root.clone(),
            allow_network_download: self.config.allow_network_download,
            github_base_url: self.config.github_base_url.clone(),
            github_api_base_url: self.config.github_api_base_url.clone(),
        })
    }

    /// Build one downloader that emits archive-download progress for the current skill operation.
    /// 构造一个会为当前技能操作发出归档下载进度的下载器。
    fn downloader_for_skill_progress(
        &self,
        skill_id: &str,
        source_type: SkillInstallSourceType,
    ) -> DownloadManager {
        let progress_callback = self
            .progress
            .as_ref()
            .map(|progress| progress.download_callback(source_type, skill_id.to_string()));
        DownloadManager::new_with_progress(
            DownloadManagerConfig {
                cache_root: self.config.download_cache_root.clone(),
                allow_network_download: self.config.allow_network_download,
                github_base_url: self.config.github_base_url.clone(),
                github_api_base_url: self.config.github_api_base_url.clone(),
            },
            progress_callback,
        )
    }

    /// Emit one simple progress phase when an operation-scoped progress emitter exists.
    /// 当存在操作级进度发射器时发出一条简单阶段进度。
    fn emit_progress(&self, phase: &str, status: &str, message: Option<&str>) {
        if let Some(progress) = self.progress.as_ref() {
            progress.emit(phase, status, message.map(ToOwned::to_owned));
        }
    }

    /// Emit one detailed progress phase when an operation-scoped progress emitter exists.
    /// 当存在操作级进度发射器时发出一条详细阶段进度。
    fn emit_progress_detail(&self, detail: RuntimeSkillOperationProgressDetail<'_>) {
        if let Some(progress) = self.progress.as_ref() {
            progress.emit_detail(detail);
        }
    }
}

/// Return whether one runtime skill root represents the system-controlled ROOT layer.
/// 返回单个运行时技能根是否代表系统控制的 ROOT 层。
fn is_root_skill_layer(root: &RuntimeSkillRoot) -> bool {
    root.name.trim().eq_ignore_ascii_case("ROOT")
}

/// Build one persisted source record from a manifest and trusted caller-provided locator.
/// 根据 manifest 与可信调用方提供的定位值构造单个持久化来源记录。
fn source_record_from_manifest(
    manifest: &SkillSourceManifest,
    source_type: SkillInstallSourceType,
    source_locator: &str,
) -> InstalledSkillSourceRecord {
    InstalledSkillSourceRecord {
        source_type,
        locator: source_locator.trim().to_string(),
        tag: manifest
            .update
            .as_ref()
            .filter(|update| update.source_type == source_type)
            .and_then(|update| update.tag.clone()),
    }
}

/// Return whether a private source URL is accepted by the host-controlled allowlist.
/// 返回单个私有来源 URL 是否被宿主管控 allowlist 接受。
fn is_allowed_private_source_url(url: &str, allowlist: &[String]) -> bool {
    let candidate = url.trim().trim_end_matches('/').to_ascii_lowercase();
    if candidate.is_empty() {
        return false;
    }
    allowlist.iter().any(|entry| {
        let prefix = entry.trim().trim_end_matches('/').to_ascii_lowercase();
        !prefix.is_empty()
            && (candidate == prefix || candidate.starts_with(format!("{}/", prefix).as_str()))
    })
}

/// Build one stable cache key for a remote source manifest.
/// 为远程来源 manifest 构造单个稳定缓存键。
fn source_manifest_cache_key(kind: &str, skill_id: &str, locator: &str) -> String {
    format!(
        "skill-source-{}-{}-{}",
        sanitize_cache_key_fragment(kind),
        sanitize_cache_key_fragment(skill_id),
        sanitize_cache_key_fragment(locator)
    )
}

/// Sanitize one string fragment for local cache file names.
/// 将单个字符串片段规范化为本地缓存文件名可用格式。
fn sanitize_cache_key_fragment(value: &str) -> String {
    value
        .chars()
        .map(|ch| match ch {
            'a'..='z' | 'A'..='Z' | '0'..='9' => ch,
            _ => '-',
        })
        .collect()
}

/// Resolve the effective request skill id, deriving it from the source locator when needed.
/// 解析当前请求的生效技能标识符，并在需要时从来源定位值派生。
pub(crate) fn resolve_requested_skill_id(request: &SkillInstallRequest) -> Result<String, String> {
    let explicit_skill_id = request
        .skill_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let derived_skill_id = match request.source_type {
        SkillInstallSourceType::Github => request
            .source
            .as_deref()
            .map(normalize_github_repo_locator)
            .transpose()?
            .map(|repo| github_repo_skill_id(&repo))
            .transpose()?,
        SkillInstallSourceType::OfficialHub => request
            .source
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned),
        SkillInstallSourceType::Url | SkillInstallSourceType::PrivateUrlManifest => None,
    };
    let skill_id = explicit_skill_id.or(derived_skill_id).ok_or_else(|| {
        "install/update request requires skill_id or one source that can derive it".to_string()
    })?;
    validate_luaskills_identifier(&skill_id, "skill_id")?;
    Ok(skill_id)
}

/// Normalize one GitHub repository locator into `owner/repo` form.
/// 将单个 GitHub 仓库定位值规范化为 `owner/repo` 形式。
///
/// The source parameter is the caller-provided GitHub repository locator.
/// source 参数是调用方提供的 GitHub 仓库定位值。
///
/// Returns the normalized `owner/repo` locator.
/// 返回规范化后的 `owner/repo` 定位值。
///
/// Returns an error when the locator is not exactly one owner and one repository segment.
/// 当定位值不是准确的一个 owner 段和一个 repo 段时返回错误。
fn normalize_github_repo_locator(source: &str) -> Result<String, String> {
    // Repository locator after trimming supported GitHub URL prefixes and surrounding separators.
    // 去除受支持 GitHub URL 前缀与外围分隔符后的仓库定位值。
    let normalized = source
        .trim()
        .trim_start_matches("https://github.com/")
        .trim_start_matches("http://github.com/")
        .trim_matches('/')
        .to_string();
    // Slash-separated repository locator segments used to enforce exact owner/repo structure.
    // 用于强制准确 owner/repo 结构的斜杠分隔仓库定位值片段。
    let mut segments = normalized.split('/');
    // Owner segment from the normalized repository locator.
    // 规范化仓库定位值中的 owner 段。
    let owner = segments.next().map(str::trim);
    // Repository segment from the normalized repository locator.
    // 规范化仓库定位值中的 repo 段。
    let repo = segments.next().map(str::trim);
    // Third segment, if present, proves the locator contains an unsupported nested path.
    // 如果存在第三段，则证明定位值包含不受支持的嵌套路径。
    let extra_segment = segments.next();
    let (Some(owner), Some(repo), None) = (owner, repo, extra_segment) else {
        return Err(format!(
            "github source '{}' must be one repository locator in owner/repo form",
            source
        ));
    };
    if owner.is_empty() || repo.is_empty() {
        return Err(format!(
            "github source '{}' must be one repository locator in owner/repo form",
            source
        ));
    }
    Ok(format!("{}/{}", owner, repo))
}

/// Derive one skill id from the repository segment of a GitHub locator.
/// 从 GitHub 定位值的仓库段派生单个技能标识符。
///
/// The repo parameter is the normalized GitHub repository locator.
/// repo 参数是规范化后的 GitHub 仓库定位值。
///
/// Returns the skill id derived from the repository segment.
/// 返回从仓库段派生出的技能标识符。
///
/// Returns an error when the locator does not contain an owner/repo separator.
/// 当定位值不包含 owner/repo 分隔符时返回错误。
fn github_repo_skill_id(repo: &str) -> Result<String, String> {
    // Repository segment extracted from the normalized owner/repo locator.
    // 从规范化 owner/repo 定位值中提取出的仓库段。
    let (_, repo_segment) = repo.rsplit_once('/').ok_or_else(|| {
        format!(
            "github repository '{}' must be one repository locator in owner/repo form",
            repo
        )
    })?;
    // Skill id candidate derived from the repository segment before identifier validation.
    // 在标识符校验前从仓库段派生出的技能标识符候选值。
    let skill_id = repo_segment.trim().to_string();
    validate_luaskills_identifier(&skill_id, "derived github skill_id")?;
    Ok(skill_id)
}

/// Format one uninstall finalization failure with rollback diagnostics.
/// 将单个卸载收尾失败与回滚诊断一起格式化。
///
/// The base_message parameter is the primary uninstall finalization failure text.
/// base_message 参数是主要的卸载收尾失败文本。
///
/// The rollback_result parameter is the result of restoring the staged uninstall mutation.
/// rollback_result 参数是恢复已暂存卸载变更的结果。
///
/// Returns one complete human-readable uninstall finalization failure message.
/// 返回一条完整的人类可读卸载收尾失败信息。
fn format_uninstall_finalization_error<R>(
    base_message: String,
    rollback_result: Result<(), R>,
) -> String
where
    R: Display,
{
    // Mutable failure message that receives rollback diagnostics only when rollback fails.
    // 可变失败消息，仅在回滚失败时追加回滚诊断。
    let mut message = base_message;
    // Rollback failure is appended only when the rollback operation returned a concrete error.
    // 只有当回滚操作返回具体错误时，才追加回滚失败信息。
    if let Err(error) = rollback_result {
        message.push_str(&format!(". rollback failed: {}", error));
    }
    message
}

/// Build one stable download-cache key for a managed skill package.
/// 为受管技能包构造单个稳定的下载缓存键。
fn managed_skill_cache_key(skill_id: &str, version: &str) -> String {
    format!("skill-{}-{}", skill_id, version)
}

/// Return the current Unix timestamp in milliseconds for one named lifecycle operation.
/// 返回单个具名生命周期操作使用的当前 Unix 毫秒时间戳。
fn current_unix_millis(context: &str) -> Result<u128, String> {
    unix_millis_from_system_time(SystemTime::now(), context)
}

/// Convert one system time into Unix milliseconds without silently accepting pre-epoch clocks.
/// 将单个系统时间转换为 Unix 毫秒，且不会静默接受早于 epoch 的时钟。
fn unix_millis_from_system_time(time: SystemTime, context: &str) -> Result<u128, String> {
    time.duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .map_err(|error| {
            format!(
                "System clock is before Unix epoch while computing {}: {}",
                context, error
            )
        })
}

/// Read one extracted skill manifest from disk and bind the directory-derived skill id.
/// 从磁盘读取单个已解包技能清单，并绑定从目录派生的技能标识符。
fn read_skill_manifest_from_directory(skill_dir: &Path) -> Result<SkillMeta, String> {
    let skill_id = skill_dir
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| {
            format!(
                "Failed to resolve skill id from directory {}",
                render_skill_manager_path(skill_dir)
            )
        })?
        .trim()
        .to_string();
    validate_luaskills_identifier(&skill_id, "skill_id")?;
    let skill_yaml_path = skill_dir.join("skill.yaml");
    let yaml_text = fs::read_to_string(&skill_yaml_path).map_err(|error| {
        format!(
            "Failed to read {}: {}",
            render_skill_manager_path(&skill_yaml_path),
            error
        )
    })?;
    let yaml_value: serde_yaml::Value = serde_yaml::from_str(&yaml_text).map_err(|error| {
        format!(
            "Failed to parse {}: {}",
            render_skill_manager_path(&skill_yaml_path),
            error
        )
    })?;
    if yaml_value
        .as_mapping()
        .and_then(|mapping| mapping.get(serde_yaml::Value::String("skill_id".to_string())))
        .is_some()
    {
        return Err(format!(
            "skill {} must not declare skill_id in skill.yaml; directory name is the only skill_id",
            render_skill_manager_path(skill_dir)
        ));
    }
    let mut meta: SkillMeta = serde_yaml::from_value(yaml_value).map_err(|error| {
        format!(
            "Failed to decode {}: {}",
            render_skill_manager_path(&skill_yaml_path),
            error
        )
    })?;
    meta.bind_directory_skill_id(skill_id.clone());
    meta.resolve_entry_input_schemas(skill_dir)?;
    validate_luaskills_version(meta.version(), "skill.yaml version")?;
    if meta.effective_skill_id() != skill_id {
        return Err(format!(
            "skill manifest in {} resolved to skill_id '{}' instead of '{}'",
            render_skill_manager_path(&skill_yaml_path),
            meta.effective_skill_id(),
            skill_id
        ));
    }
    Ok(meta)
}

/// Resolve the currently effective skill directories after applying override precedence and empty-directory disable semantics.
/// 在应用 override 优先级与空目录禁用语义后解析当前实际生效的技能目录集合。
pub fn collect_effective_skill_instances(
    base_dir: &Path,
    override_dir: Option<&Path>,
) -> Result<Vec<ResolvedSkillInstance>, String> {
    let mut roots = vec![RuntimeSkillRoot {
        name: "ROOT".to_string(),
        skills_dir: base_dir.to_path_buf(),
    }];
    if let Some(override_dir) = override_dir {
        roots.push(RuntimeSkillRoot {
            name: "PROJECT".to_string(),
            skills_dir: override_dir.to_path_buf(),
        });
    }
    collect_effective_skill_instances_from_roots(&roots)
}

/// Resolve the currently effective skill directories after applying ordered root precedence rules.
/// 在应用有序根目录优先级规则后解析当前实际生效的技能目录集合。
pub fn collect_effective_skill_instances_from_roots(
    roots: &[RuntimeSkillRoot],
) -> Result<Vec<ResolvedSkillInstance>, String> {
    let mut all_skill_ids = BTreeSet::new();
    let mut root_maps = Vec::new();
    for root in roots {
        let root_map = collect_named_skill_dirs(&root.skills_dir)?;
        all_skill_ids.extend(root_map.keys().cloned());
        root_maps.push((root.clone(), root_map));
    }

    let mut resolved = Vec::new();
    for skill_id in all_skill_ids {
        for (root, root_map) in &root_maps {
            let Some(skill_dir) = root_map.get(&skill_id) else {
                continue;
            };
            if is_effective_disable_override(skill_dir)? {
                break;
            }
            if !is_skill_manifest_enabled(skill_dir)? {
                break;
            }
            resolved.push(ResolvedSkillInstance {
                skill_id: skill_id.clone(),
                root_name: root.name.clone(),
                skills_root: root.skills_dir.clone(),
                actual_dir: skill_dir.clone(),
            });
            break;
        }
    }
    Ok(resolved)
}

/// Resolve one effective skill instance by skill id after applying root precedence.
/// 在应用根目录优先级后按技能标识符解析单个生效技能实例。
pub fn resolve_effective_skill_instance(
    base_dir: &Path,
    override_dir: Option<&Path>,
    skill_id: &str,
) -> Result<Option<ResolvedSkillInstance>, String> {
    validate_luaskills_identifier(skill_id, "skill_id")?;
    Ok(collect_effective_skill_instances(base_dir, override_dir)?
        .into_iter()
        .find(|instance| instance.skill_id == skill_id))
}

/// Resolve one effective skill instance by skill id from an ordered root chain.
/// 从有序根目录覆盖链中按技能标识符解析单个生效技能实例。
pub fn resolve_effective_skill_instance_from_roots(
    roots: &[RuntimeSkillRoot],
    skill_id: &str,
) -> Result<Option<ResolvedSkillInstance>, String> {
    validate_luaskills_identifier(skill_id, "skill_id")?;
    Ok(collect_effective_skill_instances_from_roots(roots)?
        .into_iter()
        .find(|instance| instance.skill_id == skill_id))
}

/// Resolve the highest-priority declared skill directory by skill id without applying enable-state filtering.
/// 在不应用启用状态过滤的前提下，按技能标识符解析最高优先级的已声明技能目录。
pub fn resolve_declared_skill_instance_from_roots(
    roots: &[RuntimeSkillRoot],
    skill_id: &str,
) -> Result<Option<ResolvedSkillInstance>, String> {
    validate_luaskills_identifier(skill_id, "skill_id")?;
    for root in roots {
        let root_map = collect_named_skill_dirs(&root.skills_dir)?;
        if let Some(actual_dir) = root_map.get(skill_id) {
            return Ok(Some(ResolvedSkillInstance {
                skill_id: skill_id.to_string(),
                root_name: root.name.clone(),
                skills_root: root.skills_dir.clone(),
                actual_dir: actual_dir.clone(),
            }));
        }
    }
    Ok(None)
}

/// Read one root directory into a validated skill-id -> path map.
/// 把单个根目录读取为经过校验的 skill-id -> 路径映射。
fn collect_named_skill_dirs(
    root: &Path,
) -> Result<std::collections::BTreeMap<String, PathBuf>, String> {
    let mut output = std::collections::BTreeMap::new();
    if !skill_root_path_is_directory(root)? {
        return Ok(output);
    }
    for entry in fs::read_dir(root).map_err(|error| {
        format!(
            "Failed to read {}: {}",
            render_skill_manager_path(root),
            error
        )
    })? {
        let entry = entry.map_err(|error| format!("Failed to read skill entry: {}", error))?;
        let file_type = entry
            .file_type()
            .map_err(|error| format!("Failed to inspect skill entry type: {}", error))?;
        if !file_type.is_dir() {
            continue;
        }
        let skill_id = match entry.file_name().to_str() {
            Some(value) => value.to_string(),
            None => continue,
        };
        if validate_luaskills_identifier(&skill_id, "skill_id").is_err() {
            continue;
        }
        output.insert(skill_id, entry.path());
    }
    Ok(output)
}

/// Return whether one override skill directory should disable lower-priority instances because it is intentionally empty.
/// 返回单个 override 技能目录是否因为有意留空而应禁用更低优先级实例。
fn is_effective_disable_override(skill_dir: &Path) -> Result<bool, String> {
    Ok(fs::read_dir(skill_dir)
        .map_err(|error| {
            format!(
                "Failed to read override dir {}: {}",
                render_skill_manager_path(skill_dir),
                error
            )
        })?
        .next()
        .is_none())
}

/// Return whether one resolved skill directory is enabled by its manifest.
/// 返回单个已解析技能目录是否在其清单中启用。
fn is_skill_manifest_enabled(skill_dir: &Path) -> Result<bool, String> {
    let skill_yaml = skill_dir.join("skill.yaml");
    if !skill_manifest_path_is_file(&skill_yaml)? {
        return Ok(true);
    }
    let yaml_text = fs::read_to_string(&skill_yaml).map_err(|error| {
        format!(
            "Failed to read {}: {}",
            render_skill_manager_path(&skill_yaml),
            error
        )
    })?;
    let yaml_value: serde_yaml::Value = serde_yaml::from_str(&yaml_text).map_err(|error| {
        format!(
            "Failed to parse {}: {}",
            render_skill_manager_path(&skill_yaml),
            error
        )
    })?;
    if yaml_value.as_mapping().is_some_and(|mapping| {
        mapping.contains_key(serde_yaml::Value::String("skill_id".to_string()))
    }) {
        return Err(format!(
            "skill manifest {} must not declare skill_id; directory name is the only skill_id",
            render_skill_manager_path(&skill_yaml)
        ));
    }
    #[derive(Debug, Deserialize)]
    struct SkillEnableProbe {
        /// When omitted the skill is treated as enabled.
        /// 省略时表示技能默认启用。
        #[serde(default = "default_skill_enable")]
        enable: bool,
    }
    /// Return the default enable flag used by lightweight manifest probes.
    /// 返回轻量清单探针使用的默认启用标记。
    fn default_skill_enable() -> bool {
        true
    }
    let probe: SkillEnableProbe = serde_yaml::from_value(yaml_value).map_err(|error| {
        format!(
            "Failed to parse {}: {}",
            render_skill_manager_path(&skill_yaml),
            error
        )
    })?;
    Ok(probe.enable)
}

#[cfg(test)]
mod tests;
