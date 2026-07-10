use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use crate::dependency::types::{DependencyScope, DependencySourceType};
use crate::runtime::path::render_host_visible_path;

/// Supported archive formats used by LuaSkills dependency packages.
/// LuaSkills 依赖包支持的归档格式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DependencyArchiveType {
    /// Treat the downloaded payload as one zip archive.
    /// 将下载载荷视为 zip 归档。
    Zip,
    /// Treat the downloaded payload as one tar.gz archive.
    /// 将下载载荷视为 tar.gz 归档。
    TarGz,
    /// Treat the downloaded payload as one raw single file.
    /// 将下载载荷视为单个原始文件。
    #[default]
    Raw,
}

/// One exported file rule used both for installation and existence detection.
/// 同时用于安装与存在性检测的单个导出文件规则。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DependencyExportSpec {
    /// Relative path inside the downloaded archive or raw file payload.
    /// 下载归档或原始文件载荷内部的相对路径。
    pub archive_path: String,
    /// Relative destination path under the dependency root.
    /// 依赖根目录下的相对目标路径。
    pub target_path: String,
    /// Whether the exported file should be marked executable on Unix platforms.
    /// 是否应在 Unix 平台上把导出文件标记为可执行。
    #[serde(default)]
    pub executable: bool,
}

/// One platform-specific package record resolved before actual download.
/// 在实际下载前解析得到的单个平台包记录。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DependencyPackageSpec {
    /// Archive format of the package payload.
    /// 包载荷对应的归档格式。
    #[serde(default)]
    pub archive_type: DependencyArchiveType,
    /// Exact GitHub asset file name used for github_release downloads.
    /// 用于 github_release 下载的精确 GitHub 资产文件名。
    #[serde(default)]
    pub asset_name: Option<String>,
    /// Exact download URL used for direct url or resolved skilllist packages.
    /// 用于直接 url 或已解析 skilllist 包的精确下载地址。
    #[serde(default)]
    pub url: Option<String>,
    /// Exported files that must be installed and later used for existence checks.
    /// 必须被安装、并在之后用于存在性检测的导出文件列表。
    #[serde(default)]
    pub exports: Vec<DependencyExportSpec>,
}

/// GitHub-release source configuration used by one dependency.
/// 单个依赖使用的 GitHub Release 来源配置。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GithubReleaseSourceSpec {
    /// GitHub repository in `owner/repo` format.
    /// `owner
    /// repo` 形式的 GitHub 仓库标识。
    pub repo: String,
    /// Optional explicit release API URL. When omitted the host-configured GitHub API base is used.
    /// 可选的显式 release API 地址；省略时使用宿主配置的 GitHub API 基址。
    #[serde(default)]
    pub tag_api: Option<String>,
}

/// Direct-URL source configuration used by one dependency.
/// 单个依赖使用的直接 URL 来源配置。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct UrlSourceSpec {}

/// Skill-list source configuration used by one dependency.
/// 单个依赖使用的 skilllist 来源配置。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillListSourceSpec {
    /// URL of the remote skill/tool dependency list file.
    /// 远程 skill/tool 依赖列表文件的下载地址。
    pub url: String,
    /// Package key inside the downloaded list file.
    /// 下载后的列表文件内部使用的包键名。
    pub package: String,
}

/// Unified source specification used by all dependency kinds.
/// 所有依赖类型共用的统一来源描述。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DependencySourceSpec {
    /// Upstream source type used for package resolution.
    /// 包解析时使用的上游来源类型。
    #[serde(rename = "type")]
    pub source_type: DependencySourceType,
    /// Optional GitHub-release source payload.
    /// 可选的 GitHub Release 来源载荷。
    #[serde(default)]
    pub github: Option<GithubReleaseSourceSpec>,
    /// Optional direct-URL source payload.
    /// 可选的直接 URL 来源载荷。
    #[serde(default)]
    pub url: Option<UrlSourceSpec>,
    /// Optional skilllist source payload.
    /// 可选的 skilllist 来源载荷。
    #[serde(default)]
    pub skilllist: Option<SkillListSourceSpec>,
}

/// Tool dependency declaration loaded from one skill package.
/// 从单个 skill 包加载的工具依赖声明。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolDependencySpec {
    /// Stable dependency name.
    /// 稳定依赖名称。
    pub name: String,
    /// Optional expected version string used for display and asset interpolation.
    /// 用于展示和资产名插值的可选预期版本字符串。
    #[serde(default)]
    pub version: Option<String>,
    /// Whether the dependency is required for the skill to load.
    /// 当前依赖是否为技能加载所必需。
    #[serde(default = "default_required_dependency")]
    pub required: bool,
    /// Install scope of the current dependency. Tool dependencies default to skill-private.
    /// 当前依赖的安装作用域。工具依赖默认使用 skill 私有作用域。
    #[serde(default = "default_tool_dependency_scope")]
    pub scope: DependencyScope,
    /// Dependency source specification.
    /// 依赖来源描述。
    pub source: DependencySourceSpec,
    /// Platform-specific package descriptors.
    /// 平台对应的包描述表。
    #[serde(default)]
    pub packages: BTreeMap<String, DependencyPackageSpec>,
}

/// Lua package dependency declaration loaded from one skill package.
/// 从单个 skill 包加载的 Lua 库依赖声明。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LuaDependencySpec {
    /// Stable dependency name.
    /// 稳定依赖名称。
    pub name: String,
    /// Optional expected version string used for display and asset interpolation.
    /// 用于展示和资产名插值的可选预期版本字符串。
    #[serde(default)]
    pub version: Option<String>,
    /// Whether the dependency is required for the skill to load.
    /// 当前依赖是否为技能加载所必需。
    #[serde(default = "default_required_dependency")]
    pub required: bool,
    /// Install scope of the current dependency. Lua dependencies default to skill-private.
    /// 当前依赖的安装作用域。Lua 依赖默认使用 skill 私有作用域。
    #[serde(default = "default_runtime_library_scope")]
    pub scope: DependencyScope,
    /// Dependency source specification.
    /// 依赖来源描述。
    pub source: DependencySourceSpec,
    /// Platform-specific package descriptors.
    /// 平台对应的包描述表。
    #[serde(default)]
    pub packages: BTreeMap<String, DependencyPackageSpec>,
}

/// FFI/native library dependency declaration loaded from one skill package.
/// 从单个 skill 包加载的 FFI/原生库依赖声明。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FfiDependencySpec {
    /// Stable dependency name.
    /// 稳定依赖名称。
    pub name: String,
    /// Optional expected version string used for display and asset interpolation.
    /// 用于展示和资产名插值的可选预期版本字符串。
    #[serde(default)]
    pub version: Option<String>,
    /// Whether the dependency is required for the skill to load.
    /// 当前依赖是否为技能加载所必需。
    #[serde(default = "default_required_dependency")]
    pub required: bool,
    /// Install scope of the current dependency. FFI dependencies default to skill-private.
    /// 当前依赖的安装作用域。FFI 依赖默认使用 skill 私有作用域。
    #[serde(default = "default_runtime_library_scope")]
    pub scope: DependencyScope,
    /// Dependency source specification.
    /// 依赖来源描述。
    pub source: DependencySourceSpec,
    /// Platform-specific package descriptors.
    /// 平台对应的包描述表。
    #[serde(default)]
    pub packages: BTreeMap<String, DependencyPackageSpec>,
}

/// Package manager used by one managed Python runtime declaration.
/// 单个受管 Python 运行时声明使用的包管理器。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PythonRuntimePackageManager {
    /// Use the uv package manager and environment synchronizer.
    /// 使用 uv 包管理器与环境同步器。
    Uv,
}

/// Package manager used by one managed Node.js runtime declaration.
/// 单个受管 Node.js 运行时声明使用的包管理器。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeRuntimePackageManager {
    /// Use pnpm with one host-managed content-addressed package store.
    /// 使用 pnpm 与宿主管理的内容寻址包存储。
    Pnpm,
}

/// Managed Python runtime dependency declared by one skill package.
/// 单个 skill 包声明的受管 Python 运行时依赖。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PythonRuntimeDependencySpec {
    /// Exact Python runtime version requested by the skill.
    /// 当前 skill 请求的精确 Python 运行时版本。
    pub version: String,
    /// Python package manager selected for environment creation.
    /// 用于创建环境的 Python 包管理器。
    pub package_manager: PythonRuntimePackageManager,
    /// Exact package-manager version requested by the skill.
    /// 当前 skill 请求的精确包管理器版本。
    pub package_manager_version: String,
    /// Lockfile path under the current skill directory.
    /// 当前 skill 目录下的锁文件路径。
    #[serde(default)]
    pub lockfile: String,
    /// Whether this managed runtime is required for the skill to load.
    /// 当前受管运行时是否为 skill 加载所必需。
    #[serde(default = "default_required_dependency")]
    pub required: bool,
}

/// Managed Node.js runtime dependency declared by one skill package.
/// 单个 skill 包声明的受管 Node.js 运行时依赖。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeRuntimeDependencySpec {
    /// Exact Node.js runtime version requested by the skill.
    /// 当前 skill 请求的精确 Node.js 运行时版本。
    pub version: String,
    /// Node.js package manager selected for environment creation.
    /// 用于创建环境的 Node.js 包管理器。
    pub package_manager: NodeRuntimePackageManager,
    /// Exact package-manager version requested by the skill.
    /// 当前 skill 请求的精确包管理器版本。
    pub package_manager_version: String,
    /// Optional package.json path under the current skill directory.
    /// 当前 skill 目录下的可选 package.json 路径。
    #[serde(default)]
    pub package_json: String,
    /// Lockfile path under the current skill directory.
    /// 当前 skill 目录下的锁文件路径。
    #[serde(default)]
    pub lockfile: String,
    /// Whether this managed runtime is required for the skill to load.
    /// 当前受管运行时是否为 skill 加载所必需。
    #[serde(default = "default_required_dependency")]
    pub required: bool,
}

/// Full dependencies.yaml payload loaded from one skill package.
/// 从单个 skill 包加载的完整 dependencies.yaml 载荷。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SkillDependencyManifest {
    /// Tool dependencies such as rg or ast-grep.
    /// 例如 rg 或 ast-grep 一类的工具依赖。
    #[serde(default)]
    pub tool_dependencies: Vec<ToolDependencySpec>,
    /// Lua module dependencies installed under one host-managed lua root.
    /// 安装到宿主管理 Lua 根目录下的 Lua 模块依赖。
    #[serde(default)]
    pub lua_dependencies: Vec<LuaDependencySpec>,
    /// FFI/native library dependencies installed under one host-managed ffi root.
    /// 安装到宿主管理 FFI 根目录下的原生库依赖。
    #[serde(default)]
    pub ffi_dependencies: Vec<FfiDependencySpec>,
    /// Optional managed Python runtime declaration used by Lua orchestration.
    /// 由 Lua 编排调用的可选受管 Python 运行时声明。
    #[serde(default)]
    pub python_runtime: Option<PythonRuntimeDependencySpec>,
    /// Optional managed Node.js runtime declaration used by Lua orchestration.
    /// 由 Lua 编排调用的可选受管 Node.js 运行时声明。
    #[serde(default)]
    pub node_runtime: Option<NodeRuntimeDependencySpec>,
}

/// One skilllist package manifest downloaded from one remote list file.
/// 从远程列表文件下载得到的单个 skilllist 包清单。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillListPackageManifest {
    /// Optional resolved version declared by the list provider.
    /// 列表提供方声明的可选已解析版本号。
    #[serde(default)]
    pub version: Option<String>,
    /// Platform-specific package descriptors resolved from the list provider.
    /// 从列表提供方解析到的平台包描述表。
    #[serde(default)]
    pub packages: BTreeMap<String, DependencyPackageSpec>,
}

/// Remote skilllist file payload that maps package keys to package manifests.
/// 把包键映射到包清单的远程 skilllist 文件载荷。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SkillListIndexFile {
    /// Package map resolved from the remote list file.
    /// 从远程列表文件解析得到的包映射。
    #[serde(default)]
    pub packages: BTreeMap<String, SkillListPackageManifest>,
}

/// Return the default dependency required flag.
/// 返回依赖 required 标志的默认值。
fn default_required_dependency() -> bool {
    true
}

/// Return the default install scope used by tool dependencies.
/// 返回工具依赖默认使用的安装作用域。
fn default_tool_dependency_scope() -> DependencyScope {
    DependencyScope::Skill
}

/// Return the default install scope used by Lua and FFI runtime-library dependencies.
/// 返回 Lua 与 FFI 运行时库依赖默认使用的安装作用域。
fn default_runtime_library_scope() -> DependencyScope {
    DependencyScope::Skill
}

impl SkillDependencyManifest {
    /// Load one dependency manifest from `dependencies.yaml`.
    /// 从 `dependencies.yaml` 加载一份依赖清单。
    pub fn load_from_path(path: &Path) -> Result<Self, String> {
        let yaml_text = fs::read_to_string(path).map_err(|error| {
            format!(
                "Failed to read {}: {}",
                render_host_visible_path(path),
                error
            )
        })?;
        serde_yaml::from_str(&yaml_text).map_err(|error| {
            format!(
                "Failed to parse {}: {}",
                render_host_visible_path(path),
                error
            )
        })
    }

    /// Return whether the manifest contains any declared dependency.
    /// 返回当前清单是否声明了任何依赖。
    pub fn is_empty(&self) -> bool {
        self.tool_dependencies.is_empty()
            && self.lua_dependencies.is_empty()
            && self.ffi_dependencies.is_empty()
            && self.python_runtime.is_none()
            && self.node_runtime.is_none()
    }
}

impl ToolDependencySpec {
    /// Return the platform package descriptor selected for one normalized platform key.
    /// 返回按标准平台键选中的平台包描述。
    pub fn package_for_platform(&self, platform_key: &str) -> Option<&DependencyPackageSpec> {
        self.packages.get(platform_key)
    }
}

impl LuaDependencySpec {
    /// Return the platform package descriptor selected for one normalized platform key.
    /// 返回按标准平台键选中的平台包描述。
    pub fn package_for_platform(&self, platform_key: &str) -> Option<&DependencyPackageSpec> {
        self.packages.get(platform_key)
    }
}

impl FfiDependencySpec {
    /// Return the platform package descriptor selected for one normalized platform key.
    /// 返回按标准平台键选中的平台包描述。
    pub fn package_for_platform(&self, platform_key: &str) -> Option<&DependencyPackageSpec> {
        self.packages.get(platform_key)
    }
}

#[cfg(test)]
mod tests {
    use super::SkillDependencyManifest;
    use crate::runtime::path::render_host_visible_path;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    /// Build one unique temporary dependency-manifest test root.
    /// 构造一个唯一的依赖清单测试临时根目录。
    fn make_dependency_manifest_test_dir(label: &str) -> std::path::PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!("luaskills_dependency_manifest_{label}_{nonce}"))
    }

    /// Verify dependency-manifest parse errors render paths through the host-visible formatter.
    /// 验证依赖清单解析错误会通过宿主可见路径渲染器输出路径。
    #[test]
    fn dependency_manifest_parse_error_uses_host_visible_path() {
        // Temporary root that isolates the invalid dependency manifest fixture.
        // 隔离非法依赖清单夹具的临时根目录。
        let temp_root = make_dependency_manifest_test_dir("parse_error_path");
        fs::create_dir_all(&temp_root).expect("temp root should be created");
        // Dependency manifest path used by the real load_from_path entrypoint.
        // 真实 load_from_path 入口使用的依赖清单路径。
        let manifest_path = temp_root.join("dependencies.yaml");
        fs::write(&manifest_path, "tool_dependencies: [")
            .expect("invalid dependency manifest should be written");
        // Error returned by the real dependency manifest loader.
        // 真实依赖清单加载器返回的错误。
        let error = SkillDependencyManifest::load_from_path(&manifest_path)
            .expect_err("invalid dependency manifest should fail");
        // Expected diagnostic prefix rendered with the shared host-visible path formatter.
        // 使用共享宿主可见路径渲染器生成的期望诊断前缀。
        let expected_prefix = format!(
            "Failed to parse {}:",
            render_host_visible_path(&manifest_path)
        );

        assert!(
            error.starts_with(&expected_prefix),
            "unexpected error: {}",
            error
        );
        // Cleanup result is intentionally ignored for best-effort temporary test artifacts.
        // 对临时测试产物的清理结果按最佳努力原则有意忽略。
        let _ = fs::remove_dir_all(&temp_root);
    }

    /// Verify that the new dependency manifest format parses tool/lua/ffi groups correctly.
    /// 验证新的依赖清单格式能正确解析 tool/lua/ffi 三个分组。
    #[test]
    fn parse_dependency_manifest_groups() {
        let yaml_text = r#"
tool_dependencies:
  - name: ast-grep
    scope: skill
    source:
      type: github_release
      github:
        repo: ast-grep/ast-grep
    packages:
      windows-x64:
        archive_type: zip
        asset_name: app-x86_64-pc-windows-msvc.zip
        exports:
          - archive_path: ast-grep.exe
            target_path: bin/ast-grep.exe
lua_dependencies:
  - name: lua-cjson
    required: false
    scope: skill
    source:
      type: url
      url: {}
    packages:
      windows-x64:
        archive_type: raw
        url: https://example.com/cjson.lua
        exports:
          - archive_path: cjson.lua
            target_path: share/lua/cjson.lua
ffi_dependencies:
  - name: example-lib
    source:
      type: skilllist
      skilllist:
        url: https://example.com/index.yaml
        package: example-lib
"#;
        let manifest: SkillDependencyManifest =
            serde_yaml::from_str(yaml_text).expect("manifest should parse");
        assert_eq!(manifest.tool_dependencies.len(), 1);
        assert_eq!(manifest.lua_dependencies.len(), 1);
        assert_eq!(manifest.ffi_dependencies.len(), 1);
        assert_eq!(
            manifest.tool_dependencies[0].scope,
            crate::dependency::types::DependencyScope::Skill
        );
        assert_eq!(
            manifest.lua_dependencies[0].scope,
            crate::dependency::types::DependencyScope::Skill
        );
        assert_eq!(
            manifest.tool_dependencies[0]
                .package_for_platform("windows-x64")
                .expect("windows package should exist")
                .exports[0]
                .target_path,
            "bin/ast-grep.exe"
        );
    }

    /// Verify that missing scope fields still fall back to the expected per-kind defaults.
    /// 验证省略 scope 字段时仍会回落到按依赖类型定义的默认作用域。
    #[test]
    fn dependency_scope_defaults_match_kind_policy() {
        let yaml_text = r#"
tool_dependencies:
  - name: rg
    source:
      type: url
      url: {}
    packages: {}
lua_dependencies:
  - name: demo-lua
    source:
      type: url
      url: {}
    packages: {}
ffi_dependencies:
  - name: demo-ffi
    source:
      type: url
      url: {}
    packages: {}
"#;
        let manifest: SkillDependencyManifest =
            serde_yaml::from_str(yaml_text).expect("manifest should parse");
        assert_eq!(
            manifest.tool_dependencies[0].scope,
            crate::dependency::types::DependencyScope::Skill
        );
        assert_eq!(
            manifest.lua_dependencies[0].scope,
            crate::dependency::types::DependencyScope::Skill
        );
        assert_eq!(
            manifest.ffi_dependencies[0].scope,
            crate::dependency::types::DependencyScope::Skill
        );
    }

    /// Verify that managed Python and Node runtime declarations parse from dependencies.yaml.
    /// 验证受管 Python 与 Node 运行时声明可以从 dependencies.yaml 中解析。
    #[test]
    fn parse_managed_runtime_dependency_declarations() {
        let yaml_text = r#"
python_runtime:
  version: "3.12.8"
  package_manager: uv
  package_manager_version: "0.11.17"
  lockfile: python/requirements.lock
node_runtime:
  version: "22.11.0"
  package_manager: pnpm
  package_manager_version: "9.15.0"
  package_json: node/package.json
  lockfile: node/pnpm-lock.yaml
"#;
        let manifest: SkillDependencyManifest =
            serde_yaml::from_str(yaml_text).expect("manifest should parse");
        let python_runtime = manifest
            .python_runtime
            .expect("python runtime should parse");
        let node_runtime = manifest.node_runtime.expect("node runtime should parse");

        assert_eq!(python_runtime.version, "3.12.8");
        assert_eq!(
            python_runtime.package_manager,
            super::PythonRuntimePackageManager::Uv
        );
        assert_eq!(python_runtime.lockfile, "python/requirements.lock");
        assert!(python_runtime.required);
        assert_eq!(node_runtime.version, "22.11.0");
        assert_eq!(
            node_runtime.package_manager,
            super::NodeRuntimePackageManager::Pnpm
        );
        assert_eq!(node_runtime.package_json, "node/package.json");
        assert_eq!(node_runtime.lockfile, "node/pnpm-lock.yaml");
        assert!(node_runtime.required);
        assert!(
            !SkillDependencyManifest {
                python_runtime: Some(python_runtime),
                node_runtime: Some(node_runtime),
                ..SkillDependencyManifest::default()
            }
            .is_empty()
        );
    }
}
