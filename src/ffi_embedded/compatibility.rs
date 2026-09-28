//! Read-only compatibility evidence available before creating any native ownership.
//! 在创建任何原生所有权前可用的只读兼容证据。

use super::{EMBEDDED_FFI_PROTOCOL_VERSION, EmbeddedFfiStatus};
use crate::runtime::embedded::ExecutionBackend;
use serde::Serialize;
use std::sync::OnceLock;

/// Version of the independent, borrowed compatibility descriptor.
/// 独立借用型兼容描述的版本。
pub const EMBEDDED_DESCRIPTION_VERSION: u32 = 1;
/// Maximum descriptor bytes SDKs may copy before parsing untrusted native metadata.
/// SDK 在解析不可信原生元数据前允许复制的描述字节上限。
pub const EMBEDDED_DESCRIPTION_MAX_BYTES: usize = 16_384;
/// Implemented semantic capabilities; process-restart execution recovery and future backends remain absent.
/// 已实现语义能力；进程重启执行恢复及未来后端仍不在其中。
pub const EMBEDDED_CAPABILITIES: &[&str] = &[
    "bounded_transports_v1",
    "plugin_budgets_v1",
    "shared_pools_v1",
    "dedicated_pools_v1",
    "fixed_sessions_v1",
    "host_request_queue_v1",
    "in_memory_effect_evidence_v1",
    "durable_operation_history_v1",
    "historical_effect_reconciliation_v1",
    "live_storage_recovery_v1",
    "journal_worker_recovery_v1",
    "strict_json_v1",
];

/// Selected package and compiler input identities; binary authentication remains the release artifact's job.
/// 选定包及编译器输入身份；二进制认证仍由发布产物负责。
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "contract-generation", derive(schemars::JsonSchema))]
pub struct EmbeddedBuildIdentity {
    /// SHA-256 of the exact machine-readable selected-input report emitted by build.rs.
    /// build.rs 输出的精确机器可读选定输入报告的 SHA-256。
    pub inputs_sha256: &'static str,
    /// SHA-256 of sorted package-relative input paths and their exact content hashes.
    /// 排序后包相对输入路径及其精确内容摘要的 SHA-256。
    pub source_sha256: &'static str,
    /// Exact bundled embedded contract identity, independently checked by SDKs.
    /// 精确包内嵌入式契约身份，由 SDK 独立检查。
    pub contract_sha256: &'static str,
    /// Bundled package lockfile identity; a consuming Rust workspace may resolve a different dependency graph.
    /// 包内锁文件身份；消费它的 Rust 工作区可能解析出不同依赖图。
    pub package_lock_sha256: &'static str,
    /// Cargo's target triple for this build.
    /// 此构建的 Cargo 目标三元组。
    pub target: &'static str,
    /// Cargo's target operating-system identity.
    /// Cargo 目标操作系统身份。
    pub target_os: &'static str,
    /// Cargo's target architecture identity.
    /// Cargo 目标架构身份。
    pub target_arch: &'static str,
    /// Cargo's target pointer width, preserved as its exact textual value.
    /// Cargo 目标指针位宽，保留其精确文本值。
    pub pointer_width: &'static str,
    /// Actual Cargo optimization setting, not an inferred profile label.
    /// 实际 Cargo 优化设置，不推断配置名称。
    pub opt_level: &'static str,
    /// Cargo's debug-information setting, independent of optimization.
    /// Cargo 调试信息设置，独立于优化。
    pub debug_info: &'static str,
    /// SHA-256 of Cargo's exact encoded additional compiler flags.
    /// Cargo 精确编码额外编译参数的 SHA-256。
    pub rustflags_sha256: &'static str,
    /// The selected rustc executable's verbose version output.
    /// 所选 rustc 可执行文件的详细版本输出。
    pub rustc: &'static str,
    /// Sorted Cargo feature environment suffixes; they are not reverse-mapped into guessed feature names.
    /// 排序后 Cargo 功能环境后缀；不反向映射为猜测功能名。
    pub cargo_features: &'static [&'static str],
}

include!(concat!(env!("OUT_DIR"), "/embedded_build_identity.rs"));

/// Immutable description of the exact linked core, usable without a transport or runtime.
/// 精确链接核心的不可变描述，无需传输或运行时即可使用。
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "contract-generation", derive(schemars::JsonSchema))]
pub struct EmbeddedCoreDescription {
    /// Version of this independent descriptor format.
    /// 此独立描述格式的版本。
    pub description_version: u32,
    /// Cargo package version of this exact core.
    /// 此精确核心的 Cargo 包版本。
    pub core_version: &'static str,
    /// Exact embedded JSON protocol version.
    /// 精确嵌入式 JSON 协议版本。
    pub protocol_version: u32,
    /// Exact independent embedded ABI structure version.
    /// 精确独立嵌入式 ABI 结构版本。
    pub abi_structure_version: u32,
    /// Root command names shared with the exhaustive dispatcher.
    /// 与穷尽分发器共享的根命令名称。
    pub commands: &'static [&'static str],
    /// Nested command names shared with the exhaustive dispatcher.
    /// 与穷尽分发器共享的嵌套命令名称。
    pub runtime_commands: &'static [&'static str],
    /// Implemented semantic features; a name does not grant host permissions.
    /// 已实现语义功能；名称不授予宿主权限。
    pub capabilities: &'static [&'static str],
    /// Actually implemented execution backends, excluding reserved unsupported variants.
    /// 实际已实现执行后端，不包含预留且不支持的取值。
    pub execution_backends: &'static [ExecutionBackend],
    /// Build input evidence; release manifests bind it to commits and signed artifact checksums separately.
    /// 构建输入证据；发布清单另将其关联到提交及签名产物摘要。
    pub build: &'static EmbeddedBuildIdentity,
}

/// Single immutable authority shared by the borrowed ABI and transport describe response.
/// 借用型 ABI 与传输描述响应共享的唯一不可变权威。
static DESCRIPTION: EmbeddedCoreDescription = EmbeddedCoreDescription {
    description_version: EMBEDDED_DESCRIPTION_VERSION,
    core_version: env!("CARGO_PKG_VERSION"),
    protocol_version: EMBEDDED_FFI_PROTOCOL_VERSION,
    abi_structure_version: EMBEDDED_FFI_PROTOCOL_VERSION,
    commands: super::protocol::ROOT_COMMAND_NAMES,
    runtime_commands: super::commands::RUNTIME_COMMAND_NAMES,
    capabilities: EMBEDDED_CAPABILITIES,
    execution_backends: &[ExecutionBackend::InProcess],
    build: &BUILD_IDENTITY,
};

/// Return the compiled core description without constructing threads, runtimes or native handles.
/// 返回编译核心描述，不构造线程、运行时或原生句柄。
pub fn embedded_core_description() -> &'static EmbeddedCoreDescription {
    &DESCRIPTION
}

/// Return immutable JSON bytes valid until library unload; no caller-owned allocation is published.
/// 返回直到动态库卸载前有效的不可变 JSON 字节；不发布调用方拥有的分配。
pub(super) fn description_bytes() -> Result<&'static [u8], EmbeddedFfiStatus> {
    // A library-local cache owns these bytes; SDKs copy while retaining the loaded library and never free them.
    // 动态库局部缓存拥有这些字节；SDK 保持库已加载时复制，绝不释放它们。
    static BYTES: OnceLock<Result<Vec<u8>, EmbeddedFfiStatus>> = OnceLock::new();
    BYTES
        .get_or_init(|| {
            let bytes =
                serde_json::to_vec(&DESCRIPTION).map_err(|_| EmbeddedFfiStatus::Internal)?;
            if bytes.len() > EMBEDDED_DESCRIPTION_MAX_BYTES {
                return Err(EmbeddedFfiStatus::CapacityExceeded);
            }
            Ok(bytes)
        })
        .as_deref()
        .map_err(|status| *status)
}
