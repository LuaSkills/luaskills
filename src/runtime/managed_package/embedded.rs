//! Exact host package authorization for formal modules, independent of legacy System roots.
//! 正式模块的宿主精确包授权，独立于旧 System 根。

use super::*;

impl ManagedRuntimePackageContext {
    /// Bind the host-selected package, dependency file, runtime roots and lease metadata.
    /// 绑定宿主选择的包、依赖文件、运行时根及租约元数据。
    /// The caller must authorize this exact package before supplying these arguments.
    /// 调用方必须在传入这些参数前授权此精确包。
    /// Return a distinct embedded identity, or reject invalid paths and object replacement.
    /// 返回独立嵌入式身份，或拒绝无效路径及对象替换。
    pub(crate) fn for_embedded_plugin_with_roots(
        package_id: &str,
        package_root: &Path,
        managed_runtime_roots: Arc<ManagedRuntimeRoots>,
        dependency_file: &str,
        lease_binding: Arc<ManagedRuntimeLeaseBinding>,
    ) -> Result<Arc<Self>, String> {
        if !package_root.is_absolute() {
            return Err("module package_root must be an absolute path".to_owned());
        }
        // A module declaration authorizes only this canonical package, not its parent or siblings.
        // 模块声明仅授权此规范包，不授权其父目录或同级目录。
        let canonical_package_root =
            canonicalize_existing_directory(package_root, "module package_root")?;
        validate_lua_search_root_path(&canonical_package_root, "module package_root")?;
        // Capture before reading the manifest so replacement during preparation cannot rebind the package.
        // 在读取清单前捕获身份，避免准备期间的替换重新绑定包。
        let package_identity =
            capture_managed_directory_identity(&canonical_package_root, "module package_root")?;
        managed_runtime_roots.validate_live_filesystem_identity()?;
        // The dependency manifest must remain inside the exact authorized package after canonicalization.
        // 依赖清单规范化后必须保持在精确授权包内。
        let dependency_manifest_path = resolve_existing_package_file(
            &canonical_package_root,
            dependency_file,
            "module dependencies_file",
        )?;
        // Parse through the existing fixed-object reader, preserving identity and content checks.
        // 通过既有固定对象读取器解析，保留身份与内容校验。
        let (manifest, filesystem_identity) =
            load_managed_dependency_manifest_from_fixed_object(&dependency_manifest_path)?;
        // Embedded owners use their own managed-worker namespace instead of impersonating System leases.
        // 嵌入式所有者使用独立受管 Worker 命名空间，不冒充 System 租约。
        let package = Self::build(
            ManagedRuntimePackageKind::EmbeddedPlugin,
            package_id,
            canonical_package_root,
            managed_runtime_roots,
            Some(ManagedDependencyManifestContext {
                path: dependency_manifest_path,
                filesystem_identity,
                manifest,
            }),
            Some(lease_binding),
        )?;
        validate_managed_directory_identity(
            package.package_root(),
            &package_identity,
            "module package_root",
        )?;
        package.validate_live_filesystem_identity()?;
        Ok(package)
    }
}
