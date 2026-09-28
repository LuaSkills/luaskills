//! Resolve the host's exact module package without borrowing the System trust-root policy.
//! 解析宿主精确模块包，不借用 System 信任根策略。

use super::*;

impl LuaEngine {
    /// Resolve immutable host `definition` paths and bind the supplied module `instance_id`.
    /// 解析不可变宿主 `definition` 路径并绑定传入的模块 `instance_id`。
    /// Return validated package, workspace and logical-cwd identities before any Lua allocation.
    /// 在分配 Lua 前返回已校验的包、工作区及逻辑工作目录身份。
    pub(super) fn resolve_embedded_module_paths(
        &self,
        definition: &ModuleDefinition,
        instance_id: &str,
    ) -> Result<RuntimeLeasePathContext, String> {
        // Engine-selected interpreter and environment roots remain authoritative.
        // 引擎选择的解释器及环境根仍为权威。
        let runtime_root = self
            .host_options
            .runtime_root
            .as_ref()
            .ok_or_else(|| "embedded modules require runtime_root".to_owned())?;
        // The workspace is optional only because the public module declaration explicitly permits it.
        // 工作区可选仅因为公开模块声明明确允许省略。
        let workspace_root = normalize_optional_runtime_lease_path(
            definition.workspace_root.as_deref(),
            "workspace_root",
        )?
        .map(|path| canonicalize_system_runtime_directory(&path, "workspace_root"))
        .transpose()?;
        // Retain the native object identity, not just its current path spelling.
        // 保留原生对象身份，不仅保留当前路径写法。
        let workspace_root_identity = workspace_root
            .as_deref()
            .map(|path| capture_managed_directory_identity(path, "workspace_root"))
            .transpose()?;
        // Legacy context projection is retained, but the package owner kind is explicitly embedded.
        // 保留旧上下文投影，但包所有者类型明确为嵌入式。
        let binding = Arc::new(ManagedRuntimeLeaseBinding::new(
            instance_id.to_owned(),
            workspace_root.clone(),
            definition.mounts.clone(),
        ));
        binding.bind(instance_id.to_owned(), 1)?;
        // Only the exact host-declared package is accepted; Lua arguments cannot replace it.
        // 仅接受宿主声明的精确包；Lua 参数不能替换它。
        let package = ManagedRuntimePackageContext::for_embedded_plugin_with_roots(
            &definition.plugin_id,
            Path::new(&definition.package_root),
            self.managed_runtime_roots_for(runtime_root)?,
            &definition.dependencies_file,
            binding,
        )?;
        // Reuse the strict package-or-authorized-workspace cwd rule without broadening either root.
        // 复用严格的包内或已授权工作区内 cwd 规则，不扩大任一根。
        let cwd = resolve_system_runtime_cwd(
            definition.cwd.as_deref(),
            package.package_root(),
            workspace_root.as_deref(),
        )?;
        // Same-name directory replacement remains detectable before every actual execution.
        // 每次实际执行前仍可检测同名目录替换。
        let cwd_identity = capture_managed_directory_identity(&cwd, "cwd")?;
        Ok(RuntimeLeasePathContext {
            cwd: Some(cwd),
            cwd_identity: Some(cwd_identity),
            workspace_root,
            workspace_root_identity,
            lua_roots: vec![package.package_root().to_owned()],
            c_roots: Vec::new(),
            mounts: definition.mounts.clone(),
            system_lua_lib_dir: None,
            managed_package: Some(package),
        })
    }
}
