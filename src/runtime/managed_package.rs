use mlua::Lua;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::io::Read;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;
#[cfg(windows)]
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{
    Arc, Mutex, MutexGuard, OnceLock, RwLock, RwLockReadGuard, RwLockWriteGuard, Weak,
};
#[cfg(windows)]
use windows_sys::Win32::Foundation::{GENERIC_READ, INVALID_HANDLE_VALUE};
#[cfg(windows)]
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, CreateFileW, FILE_ATTRIBUTE_REPARSE_POINT,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES,
    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, GetFileInformationByHandle,
    OPEN_EXISTING,
};

/// Process-local monotonic owner-token source for loaded package lifetimes.
/// 为已加载包生命周期提供的进程内单调所有者令牌源。
static NEXT_MANAGED_RUNTIME_OWNER_TOKEN: AtomicU64 = AtomicU64::new(0);

/// Process-local weak registry used to retire an owner before an in-flight VM reaches a worker API.
/// 用于在执行中的 VM 到达 Worker API 前退役所有者的进程内弱注册表。
static MANAGED_RUNTIME_OWNER_STATES: OnceLock<Mutex<HashMap<u64, Weak<ManagedRuntimeOwnerState>>>> =
    OnceLock::new();

/// Platform-native identity of one trusted filesystem object captured at package activation.
/// 在包激活时捕获的单个可信文件系统对象平台原生身份。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ManagedFilesystemObjectIdentity {
    /// Unix filesystem device identifier.
    /// Unix 文件系统设备标识。
    #[cfg(unix)]
    pub(crate) device: u64,
    /// Unix inode identifier unique within the device.
    /// Unix 设备内唯一的 inode 标识。
    #[cfg(unix)]
    pub(crate) inode: u64,
    /// Windows volume serial number containing the object.
    /// 包含该对象的 Windows 卷序列号。
    #[cfg(windows)]
    pub(crate) volume_serial_number: u32,
    /// Windows file identifier unique within the volume.
    /// Windows 卷内唯一的文件标识。
    #[cfg(windows)]
    pub(crate) file_index: u64,
}

/// Atomically correlated dependency-manifest path, native identity, and parsed value.
/// 原子关联的依赖清单路径、平台原生身份与解析值。
struct ManagedDependencyManifestContext {
    /// Canonical manifest path inside the package root.
    /// 包根目录内的规范清单路径。
    path: PathBuf,
    /// Native identity of the fixed object that supplied the parsed bytes.
    /// 提供解析字节的固定对象平台原生身份。
    filesystem_identity: ManagedFilesystemObjectIdentity,
    /// Manifest parsed from that exact fixed object.
    /// 从该精确固定对象解析得到的清单。
    manifest: PackageDependencyManifest,
}

use crate::runtime::path::render_host_visible_path;
use crate::skill::dependencies::PackageDependencyManifest;
use crate::skill::manifest::validate_luaskills_identifier;

/// Package kind that owns one managed Python or Node runtime context.
/// 拥有单个受管 Python 或 Node 运行时上下文的包类型。
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum ManagedRuntimePackageKind {
    /// Ordinary LuaSkills package loaded from one named skill root.
    /// 从命名 Skill 根加载的普通 LuaSkills 包。
    Skill,
    /// Host-owned package bound to one persistent System lease.
    /// 绑定到持久 System lease 的宿主所有包。
    SystemPlugin,
}

impl ManagedRuntimePackageKind {
    /// Return the stable lowercase package-kind identifier.
    /// 返回稳定的小写包类型标识。
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Skill => "skill",
            Self::SystemPlugin => "system_plugin",
        }
    }
}

/// Stable identity that partitions managed workers and package workspaces.
/// 用于隔离受管 Worker 与包工作区的稳定身份。
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct ManagedRuntimePackageIdentity {
    /// Package ownership kind.
    /// 包所有权类型。
    kind: ManagedRuntimePackageKind,
    /// Stable package identifier validated with the LuaSkills identifier grammar.
    /// 使用 LuaSkills 标识符语法校验的稳定包标识。
    package_id: String,
    /// Canonical package root included to distinguish equal ids at different roots.
    /// 用于区分不同根目录下同名包的规范包根目录。
    canonical_package_root: PathBuf,
}

impl ManagedRuntimePackageIdentity {
    /// Return the package ownership kind.
    /// 返回包所有权类型。
    pub(crate) fn kind(&self) -> ManagedRuntimePackageKind {
        self.kind
    }

    /// Return the stable package identifier.
    /// 返回稳定包标识。
    pub(crate) fn package_id(&self) -> &str {
        &self.package_id
    }

    /// Return the canonical package root.
    /// 返回规范包根目录。
    pub(crate) fn package_root(&self) -> &Path {
        &self.canonical_package_root
    }

    /// Return a deterministic SHA-256 identity suitable for private workspace names.
    /// 返回适合私有工作区命名的确定性 SHA-256 身份。
    pub(crate) fn stable_hash(&self) -> String {
        // Stable identity payload with explicit separators between independent fields.
        // 在独立字段之间带显式分隔符的稳定身份载荷。
        let mut hasher = Sha256::new();
        hasher.update(self.kind.as_str().as_bytes());
        hasher.update([0]);
        hasher.update(self.package_id.as_bytes());
        hasher.update([0]);
        hash_path_identity(&mut hasher, &self.canonical_package_root);
        format!("{:x}", hasher.finalize())
    }
}

/// Immutable System lease identity published after manager insertion succeeds.
/// 管理器插入成功后发布的不可变 System lease 身份。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ManagedRuntimeLeaseIdentity {
    /// Opaque lease identifier returned to the host.
    /// 返回给宿主的不透明租约标识。
    lease_id: String,
    /// SID-local lease generation.
    /// SID 内部租约代际。
    generation: u64,
}

impl ManagedRuntimeLeaseIdentity {
    /// Return the opaque lease identifier.
    /// 返回不透明租约标识。
    pub(crate) fn lease_id(&self) -> &str {
        &self.lease_id
    }

    /// Return the SID-local generation.
    /// 返回 SID 内部代际。
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }
}

/// Host-controlled System lease metadata shared with managed child runtimes.
/// 与受管子运行时共享的宿主控制 System lease 元数据。
#[derive(Debug)]
pub(crate) struct ManagedRuntimeLeaseBinding {
    /// Stable System lease SID supplied by the host.
    /// 宿主提供的稳定 System lease SID。
    sid: String,
    /// Optional canonical workspace root authorized by the host.
    /// 宿主授权的可选规范工作区根目录。
    workspace_root: Option<PathBuf>,
    /// Structured host-owned mount metadata.
    /// 宿主所有的结构化挂载元数据。
    mounts: Value,
    /// Lease identity written exactly once after manager insertion.
    /// 管理器插入后仅写入一次的租约身份。
    identity: RwLock<Option<ManagedRuntimeLeaseIdentity>>,
}

impl ManagedRuntimeLeaseBinding {
    /// Create an unbound System lease context before manager insertion.
    /// 在管理器插入前创建未绑定的 System lease 上下文。
    pub(crate) fn new(sid: String, workspace_root: Option<PathBuf>, mounts: Value) -> Self {
        Self {
            sid,
            workspace_root,
            mounts,
            identity: RwLock::new(None),
        }
    }

    /// Bind the manager-issued lease id and generation exactly once.
    /// 仅一次绑定管理器签发的租约 id 与代际。
    pub(crate) fn bind(&self, lease_id: String, generation: u64) -> Result<(), String> {
        // Write guard recovered after poisoning so a failed caller cannot permanently wedge cleanup.
        // 在锁中毒后恢复的写保护，避免失败调用永久阻塞清理。
        let mut identity = write_lease_identity(&self.identity);
        if identity.is_some() {
            return Err(format!(
                "managed runtime lease binding for SID `{}` is already initialized",
                self.sid
            ));
        }
        *identity = Some(ManagedRuntimeLeaseIdentity {
            lease_id,
            generation,
        });
        Ok(())
    }

    /// Return the stable SID supplied by the host.
    /// 返回宿主提供的稳定 SID。
    pub(crate) fn sid(&self) -> &str {
        &self.sid
    }

    /// Return the canonical authorized workspace root when present.
    /// 存在时返回规范化的授权工作区根目录。
    pub(crate) fn workspace_root(&self) -> Option<&Path> {
        self.workspace_root.as_deref()
    }

    /// Return the host-owned mount metadata.
    /// 返回宿主所有的挂载元数据。
    pub(crate) fn mounts(&self) -> &Value {
        &self.mounts
    }

    /// Return the bound lease identity when manager insertion has completed.
    /// 管理器插入完成后返回已绑定的租约身份。
    pub(crate) fn identity(&self) -> Option<ManagedRuntimeLeaseIdentity> {
        read_lease_identity(&self.identity).clone()
    }
}

/// Trusted package context used by every managed runtime API.
/// 每个受管运行时 API 使用的可信包上下文。
#[derive(Debug)]
pub(crate) struct ManagedRuntimePackageContext {
    /// Stable package identity.
    /// 稳定包身份。
    identity: ManagedRuntimePackageIdentity,
    /// Activation-time native identity of the canonical package root.
    /// 规范包根目录在激活时的平台原生身份。
    package_root_filesystem_identity: ManagedFilesystemObjectIdentity,
    /// Canonical runtime root that owns runtimes, package managers, and environments.
    /// 拥有运行时、包管理器和环境的规范运行时根目录。
    canonical_runtime_root: PathBuf,
    /// Activation-time native identity of the canonical runtime root.
    /// 规范运行时根目录在激活时的平台原生身份。
    runtime_root_filesystem_identity: ManagedFilesystemObjectIdentity,
    /// Canonical dependency manifest path when the package declares one.
    /// 包声明依赖清单时对应的规范清单路径。
    dependency_manifest_path: Option<PathBuf>,
    /// Activation-time native identity of the dependency manifest when present.
    /// 依赖清单存在时在激活阶段捕获的平台原生身份。
    dependency_manifest_filesystem_identity: Option<ManagedFilesystemObjectIdentity>,
    /// Parsed dependency manifest when present.
    /// 存在时的已解析依赖清单。
    dependency_manifest: Option<Arc<PackageDependencyManifest>>,
    /// System lease metadata; absent for ordinary Skill packages.
    /// System lease 元数据；普通 Skill 包中不存在。
    lease_binding: Option<Arc<ManagedRuntimeLeaseBinding>>,
    /// Unique token preventing workers and sessions from crossing package lifetimes.
    /// 防止 Worker 与会话跨越包生命周期的唯一令牌。
    owner_token: u64,
    /// Shared retirement state observed by every in-flight worker request for this exact owner.
    /// 当前精确所有者的每个执行中 Worker 请求共同观察的退役状态。
    owner_state: Arc<ManagedRuntimeOwnerState>,
}

/// Shared lifecycle state for one exact managed package owner token.
/// 单个精确受管包所有者令牌的共享生命周期状态。
#[derive(Debug)]
pub(crate) struct ManagedRuntimeOwnerState {
    /// Whether the owning package lifetime has crossed its irreversible retirement boundary.
    /// 所属包生命周期是否已经跨越不可逆的退役边界。
    retired: AtomicBool,
}

impl ManagedRuntimeOwnerState {
    /// Return whether this exact package owner has been retired.
    /// 返回当前精确包所有者是否已经退役。
    ///
    /// The function has no parameters and returns the acquire-ordered retirement flag.
    /// 此函数不接收参数，并返回采用 acquire 顺序读取的退役标记。
    pub(crate) fn is_retired(&self) -> bool {
        self.retired.load(Ordering::Acquire)
    }

    /// Publish the irreversible retirement boundary for this exact package owner.
    /// 发布当前精确包所有者的不可逆退役边界。
    ///
    /// The function has no parameters and returns no value after release-order publication.
    /// 此函数不接收参数，并在采用 release 顺序发布后不返回值。
    fn retire(&self) {
        self.retired.store(true, Ordering::Release);
    }
}

impl ManagedRuntimePackageContext {
    /// Build one ordinary Skill package context from authoritative loader inputs.
    /// 根据加载器权威输入构造普通 Skill 包上下文。
    pub(crate) fn for_skill(
        package_id: &str,
        package_root: &Path,
        runtime_root: &Path,
        dependency_manifest: Option<PackageDependencyManifest>,
    ) -> Result<Arc<Self>, String> {
        // Canonical package root verified as a real directory.
        // 已验证为真实目录的规范包根。
        let canonical_package_root = canonicalize_existing_directory(package_root, "package root")?;
        // Canonical runtime root verified as a real directory.
        // 已验证为真实目录的规范运行时根。
        let canonical_runtime_root = canonicalize_existing_directory(runtime_root, "runtime root")?;
        // Optional canonical manifest path derived only from the declared fixed location.
        // 仅从声明的固定位置派生的可选规范清单路径。
        let dependency_manifest_path = dependency_manifest
            .as_ref()
            .map(|_| {
                resolve_existing_package_file(
                    &canonical_package_root,
                    "dependencies.yaml",
                    "dependency manifest",
                )
            })
            .transpose()?;
        let dependency_manifest_context =
            match (dependency_manifest, dependency_manifest_path.as_deref()) {
                (Some(prepared_manifest), Some(path)) => {
                    let (current_manifest, identity) =
                        load_managed_dependency_manifest_from_fixed_object(path)?;
                    if current_manifest != prepared_manifest {
                        return Err(
                            "dependency manifest changed after dependency preparation".to_string()
                        );
                    }
                    Some(ManagedDependencyManifestContext {
                        path: path.to_path_buf(),
                        filesystem_identity: identity,
                        manifest: current_manifest,
                    })
                }
                (None, None) => None,
                _ => {
                    return Err(
                        "dependency manifest path and parsed value must be present together"
                            .to_string(),
                    );
                }
            };
        Self::build(
            ManagedRuntimePackageKind::Skill,
            package_id,
            canonical_package_root,
            canonical_runtime_root,
            dependency_manifest_context,
            None,
        )
    }

    /// Build one System Plugin context inside the host-owned system Lua trust root.
    /// 在宿主所有的 System Lua 信任根内构造 System Plugin 上下文。
    pub(crate) fn for_system_plugin(
        package_id: &str,
        package_root: &Path,
        runtime_root: &Path,
        system_lua_root: &Path,
        dependency_file: &str,
        lease_binding: Arc<ManagedRuntimeLeaseBinding>,
    ) -> Result<Arc<Self>, String> {
        if !package_root.is_absolute() {
            return Err("system_package.root must be an absolute path".to_string());
        }
        // Canonical host trust root used as the package containment anchor.
        // 用作包包含关系锚点的规范宿主信任根。
        let canonical_system_lua_root =
            canonicalize_existing_directory(system_lua_root, "system_lua_lib root")?;
        // Canonical System Plugin root verified before any child path is accepted.
        // 在接受任何子路径前验证的规范 System Plugin 根。
        let canonical_package_root =
            canonicalize_existing_directory(package_root, "system_package.root")?;
        if canonical_package_root == canonical_system_lua_root
            || !canonical_package_root.starts_with(&canonical_system_lua_root)
        {
            return Err(format!(
                "system_package.root {} must be a strict descendant of system_lua_lib root {}",
                render_host_visible_path(&canonical_package_root),
                render_host_visible_path(&canonical_system_lua_root)
            ));
        }
        validate_lua_search_root_path(&canonical_package_root, "system_package.root")?;
        // Canonical runtime root supplied by the engine host options.
        // 由引擎宿主选项提供的规范运行时根。
        let canonical_runtime_root = canonicalize_existing_directory(runtime_root, "runtime root")?;
        // Canonical dependency manifest constrained to the System Plugin root.
        // 限制在 System Plugin 根内的规范依赖清单。
        let dependency_manifest_path = resolve_existing_package_file(
            &canonical_package_root,
            dependency_file,
            "system_package.dependencies_file",
        )?;
        // Parsed dependency manifest loaded from the single validated file.
        // 从唯一已验证文件加载的已解析依赖清单。
        let (dependency_manifest, dependency_manifest_filesystem_identity) =
            load_managed_dependency_manifest_from_fixed_object(&dependency_manifest_path)?;
        Self::build(
            ManagedRuntimePackageKind::SystemPlugin,
            package_id,
            canonical_package_root,
            canonical_runtime_root,
            Some(ManagedDependencyManifestContext {
                path: dependency_manifest_path,
                filesystem_identity: dependency_manifest_filesystem_identity,
                manifest: dependency_manifest,
            }),
            Some(lease_binding),
        )
    }

    /// Build one validated package context from normalized inputs.
    /// 根据规范化输入构造已验证包上下文。
    fn build(
        kind: ManagedRuntimePackageKind,
        package_id: &str,
        canonical_package_root: PathBuf,
        canonical_runtime_root: PathBuf,
        dependency_manifest_context: Option<ManagedDependencyManifestContext>,
        lease_binding: Option<Arc<ManagedRuntimeLeaseBinding>>,
    ) -> Result<Arc<Self>, String> {
        // Normalized package identifier shared by identity, diagnostics, and worker context.
        // 由身份、诊断和 Worker 上下文共享的规范包标识。
        let normalized_package_id = package_id.trim();
        validate_luaskills_identifier(normalized_package_id, "package_id")?;
        let package_root_filesystem_identity = managed_filesystem_object_identity(
            &canonical_package_root,
            "package root",
            ManagedFilesystemObjectKind::Directory,
        )?;
        let runtime_root_filesystem_identity = managed_filesystem_object_identity(
            &canonical_runtime_root,
            "runtime root",
            ManagedFilesystemObjectKind::Directory,
        )?;
        let (
            dependency_manifest_path,
            dependency_manifest_filesystem_identity,
            dependency_manifest,
        ) = match dependency_manifest_context {
            Some(context) => (
                Some(context.path),
                Some(context.filesystem_identity),
                Some(context.manifest),
            ),
            None => (None, None, None),
        };
        // Unique lifetime token allocated once for this exact loaded package instance.
        // 为当前精确已加载包实例仅分配一次的唯一生命周期令牌。
        let owner_token = allocate_managed_runtime_owner_token()?;
        // Shared retirement state registered weakly so the registry cannot extend package lifetime.
        // 以弱引用注册的共享退役状态，使注册表无法延长包生命周期。
        let owner_state = Arc::new(ManagedRuntimeOwnerState {
            retired: AtomicBool::new(false),
        });
        register_managed_runtime_owner_state(owner_token, &owner_state);
        Ok(Arc::new(Self {
            identity: ManagedRuntimePackageIdentity {
                kind,
                package_id: normalized_package_id.to_string(),
                canonical_package_root,
            },
            package_root_filesystem_identity,
            canonical_runtime_root,
            runtime_root_filesystem_identity,
            dependency_manifest_path,
            dependency_manifest_filesystem_identity,
            dependency_manifest: dependency_manifest.map(Arc::new),
            lease_binding,
            owner_token,
            owner_state,
        }))
    }

    /// Return the stable package identity.
    /// 返回稳定包身份。
    pub(crate) fn identity(&self) -> &ManagedRuntimePackageIdentity {
        &self.identity
    }

    /// Return the canonical package root.
    /// 返回规范包根目录。
    pub(crate) fn package_root(&self) -> &Path {
        self.identity.package_root()
    }

    /// Return the activation-time native identity of the canonical package root.
    /// 返回规范包根目录在激活时的平台原生身份。
    pub(crate) fn package_root_filesystem_identity(&self) -> &ManagedFilesystemObjectIdentity {
        &self.package_root_filesystem_identity
    }

    /// Return the canonical runtime root.
    /// 返回规范运行时根目录。
    pub(crate) fn runtime_root(&self) -> &Path {
        &self.canonical_runtime_root
    }

    /// Return the canonical dependency manifest path when present.
    /// 存在时返回规范依赖清单路径。
    pub(crate) fn dependency_manifest_path(&self) -> Option<&Path> {
        self.dependency_manifest_path.as_deref()
    }

    /// Return the parsed dependency manifest when present.
    /// 存在时返回已解析依赖清单。
    pub(crate) fn dependency_manifest(&self) -> Option<&PackageDependencyManifest> {
        self.dependency_manifest.as_deref()
    }

    /// Return the System lease binding when this is a System Plugin package.
    /// 当前为 System Plugin 包时返回 System lease 绑定。
    pub(crate) fn lease_binding(&self) -> Option<&Arc<ManagedRuntimeLeaseBinding>> {
        self.lease_binding.as_ref()
    }

    /// Return the process-local token identifying this exact package lifetime.
    /// 返回标识当前精确包生命周期的进程内令牌。
    pub(crate) fn owner_token(&self) -> u64 {
        self.owner_token
    }

    /// Return a weak lifecycle handle for worker-pool race detection without extending package life.
    /// 返回用于 Worker 池竞态检测且不延长包生命周期的弱生命周期句柄。
    ///
    /// The function has no parameters and returns a weak handle tied to this exact owner token.
    /// 此函数不接收参数，并返回绑定当前精确所有者令牌的弱句柄。
    pub(crate) fn owner_state(&self) -> Weak<ManagedRuntimeOwnerState> {
        Arc::downgrade(&self.owner_state)
    }

    /// Revalidate canonical package/runtime roots and the trusted manifest before each lease eval.
    /// 在每次租约执行前重新校验规范包根、运行时根与可信清单。
    ///
    /// The function has no parameters and returns an error if a live path was replaced by a link,
    /// removed, or changed to a different canonical object.
    /// 此函数不接收参数；当活动路径被替换为链接、被删除或变成不同规范对象时返回错误。
    pub(crate) fn validate_live_filesystem_identity(&self) -> Result<(), String> {
        validate_unchanged_filesystem_identity(
            self.package_root(),
            &self.package_root_filesystem_identity,
            "package root",
            ManagedFilesystemObjectKind::Directory,
        )?;
        validate_unchanged_filesystem_identity(
            self.runtime_root(),
            &self.runtime_root_filesystem_identity,
            "runtime root",
            ManagedFilesystemObjectKind::Directory,
        )?;
        if let Some(manifest_path) = self.dependency_manifest_path() {
            let expected_identity = self
                .dependency_manifest_filesystem_identity
                .as_ref()
                .ok_or_else(|| "dependency manifest native identity is unavailable".to_string())?;
            validate_unchanged_filesystem_identity(
                manifest_path,
                expected_identity,
                "dependency manifest",
                ManagedFilesystemObjectKind::File,
            )?;
        }
        Ok(())
    }

    /// Resolve one real package-relative file without lexical or symbolic-link escape.
    /// 解析一个真实包相对文件，并拒绝词法或符号链接逃逸。
    pub(crate) fn resolve_existing_file(
        &self,
        relative_path: &str,
        field_name: &str,
    ) -> Result<PathBuf, String> {
        resolve_existing_package_file(self.package_root(), relative_path, field_name)
    }

    /// Build the controlled JSON context passed to Python and Node workers.
    /// 构造传给 Python 与 Node Worker 的受控 JSON 上下文。
    pub(crate) fn worker_context_json(&self) -> Value {
        // Base package object intentionally excludes environment variables and secrets.
        // 基础包对象有意排除环境变量与密钥。
        let mut context = json!({
            "package": {
                "kind": self.identity.kind().as_str(),
                "id": self.identity.package_id(),
                "root": render_host_visible_path(self.package_root()),
            }
        });
        if let Some(binding) = self.lease_binding() {
            // Bound identity remains null only during the internal create transaction.
            // 已绑定身份仅在内部创建事务期间可能为空。
            let identity = binding.identity();
            context["system_plugin"] = json!({
                "lease_id": identity.as_ref().map(ManagedRuntimeLeaseIdentity::lease_id),
                "sid": binding.sid(),
                "generation": identity.as_ref().map(ManagedRuntimeLeaseIdentity::generation),
            });
            context["workspace_root"] = binding
                .workspace_root()
                .map(render_host_visible_path)
                .map(Value::String)
                .unwrap_or(Value::Null);
            context["mounts"] = binding.mounts().clone();
        }
        context
    }
}

/// Allocate one nonzero process-local owner token without wrapping on exhaustion.
/// 分配一个非零进程内所有者令牌，并在耗尽时拒绝回绕。
fn allocate_managed_runtime_owner_token() -> Result<u64, String> {
    NEXT_MANAGED_RUNTIME_OWNER_TOKEN
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            current.checked_add(1)
        })
        .map(|previous| previous + 1)
        .map_err(|_| "managed runtime owner token space is exhausted".to_string())
}

/// Reject path bytes that Lua interprets as package-search delimiters or placeholders.
/// 拒绝会被 Lua 解释为包搜索分隔符或占位符的路径字节。
///
/// `path` is one canonical search root and `field_name` labels validation diagnostics.
/// `path` 是单个规范搜索根，`field_name` 用于标记校验诊断。
///
/// Returns unit only when the path can be embedded into `package.path` without grammar injection.
/// 仅当路径可安全嵌入 `package.path` 且不会注入语法时返回空值。
pub(crate) fn validate_lua_search_root_path(path: &Path, field_name: &str) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;

        let bytes = path.as_os_str().as_bytes();
        if bytes.iter().any(|byte| matches!(*byte, b';' | b'?' | 0)) {
            return Err(format!(
                "{field_name} contains a character reserved by Lua package search paths: {}",
                render_host_visible_path(path)
            ));
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;

        // Canonical Windows paths commonly begin with the trusted `\\?\` namespace prefix;
        // only characters in the actual path body participate in Lua search-path grammar.
        // 规范 Windows 路径通常以可信 `\\?\` 命名空间前缀开头；只有实际路径主体中的字符
        // 才参与 Lua 搜索路径语法。
        let units = path.as_os_str().encode_wide().collect::<Vec<_>>();
        let path_body = if units.starts_with(&[0x005C, 0x005C, 0x003F, 0x005C]) {
            &units[4..]
        } else {
            units.as_slice()
        };
        if path_body
            .iter()
            .any(|unit| matches!(*unit, 0x003B | 0x003F | 0))
        {
            return Err(format!(
                "{field_name} contains a character reserved by Lua package search paths: {}",
                render_host_visible_path(path)
            ));
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let text = path.as_os_str().to_string_lossy();
        if text.contains([';', '?', '\0']) {
            return Err(format!(
                "{field_name} contains a character reserved by Lua package search paths: {}",
                render_host_visible_path(path)
            ));
        }
    }
    Ok(())
}

/// Expected filesystem object kind at one managed package trust boundary.
/// 单个受管包信任边界预期的文件系统对象类型。
#[derive(Clone, Copy)]
enum ManagedFilesystemObjectKind {
    /// Real directory object.
    /// 真实目录对象。
    Directory,
    /// Real regular file object.
    /// 真实普通文件对象。
    File,
}

/// Recanonicalize one stored path and require native object identity equality.
/// 重新规范化一个已存储路径，并要求平台原生对象身份相等。
///
/// `expected_path` is the activation-time canonical path, `expected_identity` is its captured
/// device/file identity, `label` identifies the boundary, and `kind` constrains the object type.
/// `expected_path` 是激活时的规范路径，`expected_identity` 是捕获的设备/文件身份，`label`
/// 标识边界，`kind` 约束对象类型。
///
/// Returns unit only when both the canonical name and underlying filesystem object are unchanged.
/// 仅当规范名称与底层文件系统对象均未改变时返回空值。
fn validate_unchanged_filesystem_identity(
    expected_path: &Path,
    expected_identity: &ManagedFilesystemObjectIdentity,
    label: &str,
    kind: ManagedFilesystemObjectKind,
) -> Result<(), String> {
    let current = fs::canonicalize(expected_path).map_err(|error| {
        format!(
            "failed to revalidate {label} {}: {error}",
            render_host_visible_path(expected_path)
        )
    })?;
    if current != expected_path {
        return Err(format!(
            "{label} identity changed after package activation: expected {}, got {}",
            render_host_visible_path(expected_path),
            render_host_visible_path(&current)
        ));
    }
    let actual_identity = managed_filesystem_object_identity(&current, label, kind)?;
    if &actual_identity != expected_identity {
        return Err(format!(
            "{label} filesystem object changed after package activation: {}",
            render_host_visible_path(&current)
        ));
    }
    Ok(())
}

/// Capture the platform-native identity of one canonical trusted directory.
/// 捕获单个规范可信目录的平台原生身份。
///
/// `path` must already be canonical and `label` identifies the host boundary in diagnostics.
/// `path` 必须已经规范化，`label` 用于在诊断中标识宿主边界。
///
/// Returns a stable identity suitable for later same-name replacement detection.
/// 返回适合后续检测同名替换的稳定身份。
pub(crate) fn capture_managed_directory_identity(
    path: &Path,
    label: &str,
) -> Result<ManagedFilesystemObjectIdentity, String> {
    managed_filesystem_object_identity(path, label, ManagedFilesystemObjectKind::Directory)
}

/// Revalidate one canonical trusted directory against its activation-time native identity.
/// 根据激活时的平台原生身份重新校验单个规范可信目录。
///
/// `path` is the stored canonical name, `identity` is the captured object identity, and `label`
/// identifies the boundary in diagnostics.
/// `path` 是已存储规范名称，`identity` 是捕获的对象身份，`label` 用于在诊断中标识边界。
///
/// Returns unit only when both the resolved name and underlying directory object are unchanged.
/// 仅当解析后的名称与底层目录对象均未改变时返回空值。
pub(crate) fn validate_managed_directory_identity(
    path: &Path,
    identity: &ManagedFilesystemObjectIdentity,
    label: &str,
) -> Result<(), String> {
    validate_unchanged_filesystem_identity(
        path,
        identity,
        label,
        ManagedFilesystemObjectKind::Directory,
    )
}

/// Open, identify, read, and parse one manifest through the same fixed filesystem object.
/// 通过同一个固定文件系统对象打开、识别、读取并解析清单。
///
/// `path` is the canonical dependency manifest selected by the trusted package loader.
/// `path` 是可信包加载器选定的规范依赖清单。
///
/// Returns the parsed manifest paired with the exact native identity of the object that supplied
/// its bytes, eliminating the read-then-reopen identity gap.
/// 返回解析清单及提供其字节的精确平台原生对象身份，消除读取后重新打开的身份空窗。
fn load_managed_dependency_manifest_from_fixed_object(
    path: &Path,
) -> Result<(PackageDependencyManifest, ManagedFilesystemObjectIdentity), String> {
    #[cfg(unix)]
    let (mut file, identity) = {
        let file = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)
            .map_err(|error| {
                format!(
                    "failed to open dependency manifest {}: {error}",
                    render_host_visible_path(path)
                )
            })?;
        let metadata = file.metadata().map_err(|error| {
            format!(
                "failed to inspect dependency manifest handle {}: {error}",
                render_host_visible_path(path)
            )
        })?;
        if !metadata.is_file() {
            return Err(format!(
                "dependency manifest is not a regular file: {}",
                render_host_visible_path(path)
            ));
        }
        let identity = ManagedFilesystemObjectIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
        };
        (file, identity)
    };
    #[cfg(windows)]
    let (mut file, identity) = {
        let mut wide_path = path.as_os_str().encode_wide().collect::<Vec<_>>();
        wide_path.push(0);
        let raw_handle = unsafe {
            CreateFileW(
                wide_path.as_ptr(),
                GENERIC_READ,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_OPEN_REPARSE_POINT,
                std::ptr::null_mut(),
            )
        };
        if raw_handle == INVALID_HANDLE_VALUE {
            return Err(format!(
                "failed to open dependency manifest {}: {}",
                render_host_visible_path(path),
                std::io::Error::last_os_error()
            ));
        }
        let file = unsafe { fs::File::from_raw_handle(raw_handle as _) };
        let mut information: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        if unsafe { GetFileInformationByHandle(file.as_raw_handle() as _, &mut information) } == 0 {
            return Err(format!(
                "failed to read dependency manifest identity {}: {}",
                render_host_visible_path(path),
                std::io::Error::last_os_error()
            ));
        }
        if information.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(format!(
                "dependency manifest must not be a reparse point: {}",
                render_host_visible_path(path)
            ));
        }
        let identity = ManagedFilesystemObjectIdentity {
            volume_serial_number: information.dwVolumeSerialNumber,
            file_index: (u64::from(information.nFileIndexHigh) << 32)
                | u64::from(information.nFileIndexLow),
        };
        (file, identity)
    };
    #[cfg(not(any(unix, windows)))]
    let (mut file, identity) = {
        let _ = path;
        return Err("fixed-object dependency manifest reads are unsupported".to_string());
    };
    let mut yaml_text = String::new();
    file.read_to_string(&mut yaml_text).map_err(|error| {
        format!(
            "failed to read dependency manifest {}: {error}",
            render_host_visible_path(path)
        )
    })?;
    let manifest = serde_yaml::from_str(&yaml_text).map_err(|error| {
        format!(
            "failed to parse dependency manifest {}: {error}",
            render_host_visible_path(path)
        )
    })?;
    Ok((manifest, identity))
}

/// Capture one platform-native identity after rejecting links and unexpected object kinds.
/// 在拒绝链接及非预期对象类型后捕获平台原生身份。
///
/// `path` is a canonical trust-boundary path, `label` names diagnostics, and `kind` is required.
/// `path` 是规范信任边界路径，`label` 用于诊断，`kind` 是所需类型。
///
/// Returns a stable device/file identity suitable for later same-path replacement detection.
/// 返回适合后续检测同路径替换的稳定设备/文件身份。
fn managed_filesystem_object_identity(
    path: &Path,
    label: &str,
    kind: ManagedFilesystemObjectKind,
) -> Result<ManagedFilesystemObjectIdentity, String> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        format!(
            "failed to inspect {label} {}: {error}",
            render_host_visible_path(path)
        )
    })?;
    if metadata.file_type().is_symlink() {
        return Err(format!(
            "{label} must not be a symbolic link: {}",
            render_host_visible_path(path)
        ));
    }
    let kind_matches = match kind {
        ManagedFilesystemObjectKind::Directory => metadata.is_dir(),
        ManagedFilesystemObjectKind::File => metadata.is_file(),
    };
    if !kind_matches {
        return Err(format!(
            "{label} has an unexpected filesystem object type: {}",
            render_host_visible_path(path)
        ));
    }
    #[cfg(unix)]
    {
        Ok(ManagedFilesystemObjectIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }
    #[cfg(windows)]
    {
        let mut wide_path = path.as_os_str().encode_wide().collect::<Vec<_>>();
        wide_path.push(0);
        let raw_handle = unsafe {
            CreateFileW(
                wide_path.as_ptr(),
                FILE_READ_ATTRIBUTES,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
                std::ptr::null_mut(),
            )
        };
        if raw_handle == INVALID_HANDLE_VALUE {
            return Err(format!(
                "failed to open {label} identity handle {}: {}",
                render_host_visible_path(path),
                std::io::Error::last_os_error()
            ));
        }
        let handle = unsafe { OwnedHandle::from_raw_handle(raw_handle as _) };
        let mut information: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        if unsafe { GetFileInformationByHandle(raw_handle, &mut information) } == 0 {
            return Err(format!(
                "failed to read {label} filesystem identity {}: {}",
                render_host_visible_path(path),
                std::io::Error::last_os_error()
            ));
        }
        let _handle_owner = handle;
        Ok(ManagedFilesystemObjectIdentity {
            volume_serial_number: information.dwVolumeSerialNumber,
            file_index: (u64::from(information.nFileIndexHigh) << 32)
                | u64::from(information.nFileIndexLow),
        })
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = metadata;
        Err(format!(
            "platform-native {label} filesystem identity is unsupported: {}",
            render_host_visible_path(path)
        ))
    }
}

/// Register one newly allocated owner state and opportunistically prune expired weak entries.
/// 注册一个新分配的所有者状态，并顺便清理已经失效的弱条目。
///
/// `owner_token` is globally unique in this process and `owner_state` is owned by one package context.
/// `owner_token` 在当前进程内全局唯一，`owner_state` 由单个包上下文拥有。
///
/// The function returns no value after publishing the weak lookup entry.
/// 此函数在发布弱查找条目后不返回值。
fn register_managed_runtime_owner_state(
    owner_token: u64,
    owner_state: &Arc<ManagedRuntimeOwnerState>,
) {
    let mut states = lock_managed_runtime_owner_states();
    states.retain(|_, state| state.strong_count() > 0);
    states.insert(owner_token, Arc::downgrade(owner_state));
}

/// Mark one exact package owner retired before worker-pool resources are detached.
/// 在分离 Worker 池资源前，将单个精确包所有者标记为已退役。
///
/// `owner_token` identifies the immutable package lifetime selected by the engine manager.
/// `owner_token` 标识引擎管理器选定的不可变包生命周期。
///
/// Returns whether a live owner state was found and marked; missing expired owners are already inert.
/// 返回是否找到并标记了活动所有者状态；缺失的已失效所有者本就不可再活动。
pub(crate) fn retire_managed_runtime_owner_state(owner_token: u64) -> bool {
    // State is cloned while the registry lock is held, then the atomic flag is published unlocked.
    // 在持有注册表锁时克隆状态，随后在解锁后发布原子标记。
    let owner_state = {
        let mut states = lock_managed_runtime_owner_states();
        states.retain(|_, state| state.strong_count() > 0);
        states.get(&owner_token).and_then(Weak::upgrade)
    };
    if let Some(owner_state) = owner_state {
        owner_state.retire();
        true
    } else {
        false
    }
}

/// Acquire the process-local owner-state registry while recovering after lock poisoning.
/// 获取进程内所有者状态注册表，并在锁中毒后恢复。
///
/// The function has no parameters and returns the mutable registry guard.
/// 此函数不接收参数，并返回可变注册表保护对象。
fn lock_managed_runtime_owner_states()
-> MutexGuard<'static, HashMap<u64, Weak<ManagedRuntimeOwnerState>>> {
    MANAGED_RUNTIME_OWNER_STATES
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Feed one canonical path into an identity hash without lossy text conversion.
/// 在不进行有损文本转换的情况下将规范路径写入身份哈希。
fn hash_path_identity(hasher: &mut Sha256, path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;

        hasher.update(path.as_os_str().as_bytes());
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;

        // UTF-16 code units encoded explicitly as little-endian bytes for stable Windows identity.
        // 为稳定 Windows 身份显式编码为小端字节的 UTF-16 代码单元。
        for unit in path.as_os_str().encode_wide() {
            hasher.update(unit.to_le_bytes());
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        hasher.update(path.as_os_str().to_string_lossy().as_bytes());
    }
}

/// Host-private app-data slot containing the currently active managed package.
/// 包含当前活动受管包的宿主私有应用数据槽。
#[derive(Clone, Default)]
pub(crate) struct ManagedRuntimePackageContextSlot {
    /// Active package context; absent for ordinary anonymous Lua execution.
    /// 活动包上下文；普通匿名 Lua 执行时不存在。
    context: Option<Arc<ManagedRuntimePackageContext>>,
}

/// Replace the active Lua package context and return the previous context.
/// 替换活动 Lua 包上下文并返回此前上下文。
pub(crate) fn replace_lua_managed_package_context(
    lua: &Lua,
    context: Option<Arc<ManagedRuntimePackageContext>>,
) -> Option<Arc<ManagedRuntimePackageContext>> {
    lua.set_app_data(ManagedRuntimePackageContextSlot { context })
        .and_then(|slot| slot.context)
}

/// Return the active trusted package context required by one managed runtime API.
/// 返回单个受管运行时 API 所需的活动可信包上下文。
pub(crate) fn current_lua_managed_package_context(
    lua: &Lua,
    api_name: &str,
) -> Result<Arc<ManagedRuntimePackageContext>, mlua::Error> {
    // Cloned context released from the app-data borrow before any filesystem or Lua operation.
    // 在任何文件系统或 Lua 操作前解除应用数据借用的克隆上下文。
    let context = lua
        .app_data_ref::<ManagedRuntimePackageContextSlot>()
        .and_then(|slot| slot.context.clone());
    context.ok_or_else(|| mlua::Error::runtime(format!("{api_name}: no active package context")))
}

/// Return the active trusted package context when one execution scope installed it.
/// 当执行作用域已安装可信包上下文时返回该上下文。
pub(crate) fn optional_lua_managed_package_context(
    lua: &Lua,
) -> Option<Arc<ManagedRuntimePackageContext>> {
    // Cloned context released immediately from the host-private app-data borrow.
    // 立即解除宿主私有应用数据借用的克隆上下文。
    lua.app_data_ref::<ManagedRuntimePackageContextSlot>()
        .and_then(|slot| slot.context.clone())
}

/// Resolve one package-relative real file and enforce canonical containment.
/// 解析包相对真实文件并强制规范包含关系。
fn resolve_existing_package_file(
    canonical_package_root: &Path,
    relative_path: &str,
    field_name: &str,
) -> Result<PathBuf, String> {
    // Relative path parsed once before any filesystem lookup.
    // 在任何文件系统查找前仅解析一次的相对路径。
    let path = Path::new(relative_path);
    if relative_path.trim().is_empty()
        || path.is_absolute()
        || !path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
    {
        return Err(format!(
            "{field_name} must be a non-empty safe path under the package root"
        ));
    }
    // Canonical target follows links so containment rejects links that escape the package.
    // 规范目标会跟随链接，从而通过包含校验拒绝逃逸包外的链接。
    let candidate = canonical_package_root.join(path);
    let canonical_candidate = fs::canonicalize(&candidate).map_err(|error| {
        format!(
            "failed to canonicalize {field_name} {}: {error}",
            render_host_visible_path(&candidate)
        )
    })?;
    if !canonical_candidate.starts_with(canonical_package_root) {
        return Err(format!(
            "{field_name} {} escapes package root {}",
            render_host_visible_path(&canonical_candidate),
            render_host_visible_path(canonical_package_root)
        ));
    }
    // Target metadata used to distinguish files from directories and other filesystem objects.
    // 用于区分文件、目录及其他文件系统对象的目标元数据。
    let metadata = fs::metadata(&canonical_candidate).map_err(|error| {
        format!(
            "failed to inspect {field_name} {}: {error}",
            render_host_visible_path(&canonical_candidate)
        )
    })?;
    if !metadata.is_file() {
        return Err(format!(
            "{field_name} is not a file: {}",
            render_host_visible_path(&canonical_candidate)
        ));
    }
    Ok(canonical_candidate)
}

/// Canonicalize one existing directory without hiding metadata failures.
/// 规范化一个现有目录，同时不隐藏元数据失败。
fn canonicalize_existing_directory(path: &Path, field_name: &str) -> Result<PathBuf, String> {
    // Canonical directory path used as a trusted containment anchor.
    // 用作可信包含关系锚点的规范目录路径。
    let canonical = fs::canonicalize(path).map_err(|error| {
        format!(
            "failed to canonicalize {field_name} {}: {error}",
            render_host_visible_path(path)
        )
    })?;
    // Directory metadata inspected after canonicalization.
    // 规范化后检查的目录元数据。
    let metadata = fs::metadata(&canonical).map_err(|error| {
        format!(
            "failed to inspect {field_name} {}: {error}",
            render_host_visible_path(&canonical)
        )
    })?;
    if !metadata.is_dir() {
        return Err(format!(
            "{field_name} is not a directory: {}",
            render_host_visible_path(&canonical)
        ));
    }
    Ok(canonical)
}

/// Acquire one lease-identity read guard while recovering from poisoning.
/// 获取租约身份读保护，并在锁中毒后恢复。
fn read_lease_identity(
    identity: &RwLock<Option<ManagedRuntimeLeaseIdentity>>,
) -> RwLockReadGuard<'_, Option<ManagedRuntimeLeaseIdentity>> {
    identity
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Acquire one lease-identity write guard while recovering from poisoning.
/// 获取租约身份写保护，并在锁中毒后恢复。
fn write_lease_identity(
    identity: &RwLock<Option<ManagedRuntimeLeaseIdentity>>,
) -> RwLockWriteGuard<'_, Option<ManagedRuntimeLeaseIdentity>> {
    identity
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    /// Build one unique test directory path without external test dependencies.
    /// 在不依赖外部测试库的情况下构造唯一测试目录路径。
    fn test_root(label: &str) -> PathBuf {
        // Nanosecond timestamp used only to avoid collisions between parallel tests.
        // 仅用于避免并行测试冲突的纳秒时间戳。
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("test clock should be after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("luaskills-managed-package-{label}-{suffix}"))
    }

    /// Verify System Plugin construction canonicalizes and binds one trusted manifest.
    /// 验证 System Plugin 构造会规范化并绑定唯一可信清单。
    #[test]
    fn system_plugin_context_uses_canonical_trust_root() {
        // Test runtime root containing the fixed System Lua trust root.
        // 包含固定 System Lua 信任根的测试运行时根。
        let runtime_root = test_root("canonical");
        // Host-owned System Lua root used as the containment anchor.
        // 用作包含关系锚点的宿主所有 System Lua 根。
        let system_root = runtime_root.join("system_lua_lib");
        // Concrete plugin root accepted by the context constructor.
        // 上下文构造器接受的具体插件根。
        let package_root = system_root.join("vulcan-debug");
        fs::create_dir_all(&package_root).expect("create package root");
        fs::write(package_root.join("dependencies.yaml"), "{}").expect("write manifest");
        // Unbound lease metadata shared with the context.
        // 与上下文共享的未绑定租约元数据。
        let binding = Arc::new(ManagedRuntimeLeaseBinding::new(
            "system.debug.workspace".to_string(),
            None,
            json!({}),
        ));
        // Validated System Plugin package context under test.
        // 被测的已验证 System Plugin 包上下文。
        let context = ManagedRuntimePackageContext::for_system_plugin(
            "vulcan-debug",
            &package_root,
            &runtime_root,
            &system_root,
            "dependencies.yaml",
            binding,
        )
        .expect("build System Plugin context");
        assert_eq!(
            context.package_root(),
            fs::canonicalize(&package_root).expect("canonical package root")
        );
        assert!(context.dependency_manifest().is_some());
        let _ = fs::remove_dir_all(&runtime_root);
    }

    /// Verify a System Plugin root outside the host trust root is rejected.
    /// 验证宿主信任根外的 System Plugin 根会被拒绝。
    #[test]
    fn system_plugin_context_rejects_root_outside_trust_anchor() {
        // Test runtime root containing separate trusted and untrusted directories.
        // 包含独立可信与非可信目录的测试运行时根。
        let runtime_root = test_root("outside");
        // Trusted System Lua root that must contain every plugin.
        // 必须包含所有插件的可信 System Lua 根。
        let system_root = runtime_root.join("system_lua_lib");
        // Untrusted package root intentionally placed beside the anchor.
        // 有意放置在锚点旁边的非可信包根。
        let package_root = runtime_root.join("outside-plugin");
        fs::create_dir_all(&system_root).expect("create system root");
        fs::create_dir_all(&package_root).expect("create outside package root");
        fs::write(package_root.join("dependencies.yaml"), "{}").expect("write manifest");
        // Unbound lease metadata required by the System Plugin constructor.
        // System Plugin 构造器所需的未绑定租约元数据。
        let binding = Arc::new(ManagedRuntimeLeaseBinding::new(
            "system.debug.workspace".to_string(),
            None,
            json!({}),
        ));
        // Rejected constructor result proving root containment is enforced.
        // 证明根包含关系已强制执行的拒绝结果。
        let error = ManagedRuntimePackageContext::for_system_plugin(
            "vulcan-debug",
            &package_root,
            &runtime_root,
            &system_root,
            "dependencies.yaml",
            binding,
        )
        .expect_err("outside package root should fail");
        assert!(error.contains("must be a strict descendant of system_lua_lib root"));
        let _ = fs::remove_dir_all(&runtime_root);
    }

    /// Verify parent traversal is rejected before the filesystem is queried.
    /// 验证父目录穿越会在查询文件系统前被拒绝。
    #[test]
    fn package_file_resolver_rejects_parent_traversal() {
        // Test package root used by the direct resolver call.
        // 直接解析器调用使用的测试包根。
        let package_root = test_root("parent-traversal");
        fs::create_dir_all(&package_root).expect("create package root");
        // Canonical package root required by the low-level resolver.
        // 底层解析器所需的规范包根。
        let canonical_root = fs::canonicalize(&package_root).expect("canonical package root");
        // Stable validation error returned for lexical traversal.
        // 针对词法穿越返回的稳定校验错误。
        let error = resolve_existing_package_file(&canonical_root, "../outside.py", "session.file")
            .expect_err("parent traversal should fail");
        assert!(error.contains("must be a non-empty safe path"));
        let _ = fs::remove_dir_all(&package_root);
    }

    /// Verify System package roots cannot inject extra Lua search templates.
    /// 验证 System 包根不能注入额外的 Lua 搜索模板。
    #[test]
    fn system_plugin_context_rejects_lua_search_path_metacharacters() {
        // Runtime root containing the fixed host-owned System library directory.
        // 包含固定宿主所有 System 库目录的运行时根。
        let runtime_root = test_root("search-metacharacters");
        // Canonical System trust root used by the constructor.
        // 构造器使用的规范 System 信任根。
        let system_root = runtime_root.join("system_lua_lib");
        // Semicolon is valid in host paths but would split Lua package.path entries.
        // 分号在宿主路径中有效，但会拆分 Lua package.path 条目。
        let package_root = system_root.join("plugin;injected-template");
        fs::create_dir_all(&package_root).expect("create metacharacter package root");
        fs::write(package_root.join("dependencies.yaml"), "{}")
            .expect("write metacharacter package manifest");
        let binding = Arc::new(ManagedRuntimeLeaseBinding::new(
            "system.search.metacharacters".to_string(),
            None,
            json!({}),
        ));

        let error = ManagedRuntimePackageContext::for_system_plugin(
            "search-metacharacters",
            &package_root,
            &runtime_root,
            &system_root,
            "dependencies.yaml",
            binding,
        )
        .expect_err("Lua package search metacharacters must be rejected");

        assert!(error.contains("reserved by Lua package search paths"));
        let _ = fs::remove_dir_all(&runtime_root);
    }

    /// Verify activation-time canonical roots must still exist before a later evaluation.
    /// 验证激活时的规范根在后续执行前必须仍然存在。
    #[test]
    fn package_context_revalidation_rejects_removed_trust_root() {
        // Runtime and package directories used to create one valid ordinary context.
        // 用于创建一个有效普通上下文的运行时与包目录。
        let runtime_root = test_root("live-root-revalidation");
        let package_root = runtime_root.join("package");
        fs::create_dir_all(&package_root).expect("create revalidation package root");
        let context = ManagedRuntimePackageContext::for_skill(
            "live-root-revalidation",
            &package_root,
            &runtime_root,
            None,
        )
        .expect("create revalidation package context");
        fs::remove_dir_all(&package_root).expect("remove activated package root");

        let error = context
            .validate_live_filesystem_identity()
            .expect_err("removed activated root must fail revalidation");

        assert!(error.contains("failed to revalidate package root"));
        let _ = fs::remove_dir_all(&runtime_root);
    }

    /// Verify recreating a package directory at the same canonical path changes native identity.
    /// 验证在同一规范路径重建包目录会改变平台原生身份。
    #[test]
    fn package_context_revalidation_rejects_same_path_directory_replacement() {
        let runtime_root = test_root("same-path-package-replacement");
        let package_root = runtime_root.join("package");
        let original_root = runtime_root.join("original-package");
        fs::create_dir_all(&package_root).expect("create original package root");
        let context = ManagedRuntimePackageContext::for_skill(
            "same-path-package-replacement",
            &package_root,
            &runtime_root,
            None,
        )
        .expect("create package context");
        fs::rename(&package_root, &original_root).expect("move original package root");
        fs::create_dir(&package_root).expect("create replacement package root");

        let error = context
            .validate_live_filesystem_identity()
            .expect_err("same-path replacement must fail native identity validation");

        assert!(error.contains("package root filesystem object changed"));
        let _ = fs::remove_dir_all(&runtime_root);
    }

    /// Verify replacing a manifest at the same path cannot reuse the activated parsed context.
    /// 验证替换同一路径清单后不能继续复用已激活的解析上下文。
    #[test]
    fn package_context_revalidation_rejects_same_path_manifest_replacement() {
        let runtime_root = test_root("same-path-manifest-replacement");
        let system_root = runtime_root.join("system_lua_lib");
        let package_root = system_root.join("identity-plugin");
        let manifest_path = package_root.join("dependencies.yaml");
        let original_manifest = package_root.join("original-dependencies.yaml");
        fs::create_dir_all(&package_root).expect("create manifest package root");
        fs::write(&manifest_path, "{}").expect("write original manifest");
        let binding = Arc::new(ManagedRuntimeLeaseBinding::new(
            "system.identity.replacement".to_string(),
            None,
            json!({}),
        ));
        let context = ManagedRuntimePackageContext::for_system_plugin(
            "identity-plugin",
            &package_root,
            &runtime_root,
            &system_root,
            "dependencies.yaml",
            binding,
        )
        .expect("create System package context");
        fs::rename(&manifest_path, &original_manifest).expect("move original manifest");
        fs::write(&manifest_path, "{}").expect("write replacement manifest");

        let error = context
            .validate_live_filesystem_identity()
            .expect_err("same-path manifest replacement must fail native identity validation");

        assert!(error.contains("dependency manifest filesystem object changed"));
        let _ = fs::remove_dir_all(&runtime_root);
    }

    /// Verify Skill activation rejects a manifest changed after dependency preparation.
    /// 验证 Skill 激活会拒绝依赖准备后发生变化的清单。
    #[test]
    fn skill_context_rejects_manifest_changed_after_preparation() {
        let runtime_root = test_root("prepared-manifest-replacement");
        let package_root = runtime_root.join("package");
        let manifest_path = package_root.join("dependencies.yaml");
        fs::create_dir_all(&package_root).expect("create prepared-manifest package root");
        fs::write(
            &manifest_path,
            "python_runtime:\n  version: '3.12.8'\n  package_manager: uv\n  package_manager_version: '0.6.0'\n  lockfile: requirements.lock\n",
        )
        .expect("write prepared manifest");
        let prepared_manifest = PackageDependencyManifest::load_from_path(&manifest_path)
            .expect("parse prepared manifest");
        fs::write(
            &manifest_path,
            "node_runtime:\n  version: '24.18.0'\n  package_manager: pnpm\n  package_manager_version: '11.11.0'\n  package_json: package.json\n  lockfile: pnpm-lock.yaml\n",
        )
        .expect("replace prepared manifest contents");

        let error = ManagedRuntimePackageContext::for_skill(
            "prepared-manifest-replacement",
            &package_root,
            &runtime_root,
            Some(prepared_manifest),
        )
        .expect_err("changed prepared manifest must fail activation");

        assert!(error.contains("changed after dependency preparation"));
        let _ = fs::remove_dir_all(&runtime_root);
    }

    /// Verify weak owner-state registry entries are pruned instead of growing across reloads.
    /// 验证弱所有者状态注册表条目会被清理，而不会随重载持续增长。
    #[test]
    fn managed_runtime_owner_state_registry_prunes_expired_entries() {
        // Shared real roots isolate the lifecycle stress loop from other tests.
        // 共享真实根用于将生命周期压力循环与其他测试隔离。
        let runtime_root = test_root("owner-state-pruning");
        let package_root = runtime_root.join("package");
        fs::create_dir_all(&package_root).expect("create owner-state package root");
        // Tokens whose weak registry entries must disappear after the next allocation.
        // 其弱注册表条目必须在下一次分配后消失的令牌。
        let mut retired_tokens = Vec::new();
        for _iteration in 0..128 {
            // Exact package lifetime created and retired once per simulated reload.
            // 每次模拟重载创建并退役一次的精确包生命周期。
            let context = ManagedRuntimePackageContext::for_skill(
                "owner-state-pruning",
                &package_root,
                &runtime_root,
                None,
            )
            .expect("create owner-state package context");
            assert!(retire_managed_runtime_owner_state(context.owner_token()));
            retired_tokens.push(context.owner_token());
            drop(context);
        }
        // Final live context triggers pruning of the preceding expired weak entry.
        // 最终活动上下文会触发对前一个失效弱条目的清理。
        let final_context = ManagedRuntimePackageContext::for_skill(
            "owner-state-pruning",
            &package_root,
            &runtime_root,
            None,
        )
        .expect("create final owner-state package context");
        let states = lock_managed_runtime_owner_states();

        assert!(
            retired_tokens
                .iter()
                .all(|owner_token| !states.contains_key(owner_token))
        );
        assert!(states.contains_key(&final_context.owner_token()));
        drop(states);
        drop(final_context);
        let _ = fs::remove_dir_all(&runtime_root);
    }

    /// Verify links escaping the package root are rejected after canonicalization.
    /// 验证逃逸包根的链接会在规范化后被拒绝。
    #[cfg(unix)]
    #[test]
    fn package_file_resolver_rejects_symlink_escape() {
        use std::os::unix::fs::symlink;

        // Test root holding both the package and the external target.
        // 同时容纳包与外部目标的测试根。
        let root = test_root("symlink");
        // Package directory whose link must not escape.
        // 链接不得逃逸的包目录。
        let package_root = root.join("package");
        // External file intentionally located outside the package.
        // 有意位于包外的外部文件。
        let outside_file = root.join("outside.py");
        fs::create_dir_all(&package_root).expect("create package root");
        fs::write(&outside_file, "print('outside')").expect("write outside file");
        symlink(&outside_file, package_root.join("escape.py")).expect("create escape symlink");
        // Canonical package root used as the security boundary.
        // 用作安全边界的规范包根。
        let canonical_root = fs::canonicalize(&package_root).expect("canonical package root");
        // Stable containment error returned after following the symlink.
        // 跟随符号链接后返回的稳定包含关系错误。
        let error = resolve_existing_package_file(&canonical_root, "escape.py", "session.file")
            .expect_err("symlink escape should fail");
        assert!(error.contains("escapes package root"));
        let _ = fs::remove_dir_all(&root);
    }
}
