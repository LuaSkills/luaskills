# LuaSkills 0.5.1 升级说明

本文面向从 `0.5.0` 升级到 `0.5.1` 的 Rust、C ABI、JSON FFI 与多语言 SDK 宿主。

`0.5.1` 的核心变化是把 LuaSkills 数据根、Python/Node 发行根和受管环境根拆成三个独立边界。Lua API 与包内 `dependencies.yaml` 声明保持不变。

## 版本线

- Rust crate：`luaskills = 0.5.1`
- TypeScript SDK：`@luaskills/sdk@0.5.1`
- Python SDK：`luaskills-sdk==0.5.1`
- Go module tag：`v0.5.1`
- FFI 与 SDK release 资产：`v0.5.1`

## Rust 宿主

继续使用旧目录时无需增加字段：

```text
<runtime_root>/dependencies/runtimes
<runtime_root>/dependencies/envs
```

共享应用级发行包时显式设置：

```rust
let mut host_options = LuaRuntimeHostOptions::with_runtime_root(runtime_data_root);
host_options.managed_runtime_distribution_root =
    Some(application_root.join("dependencies/runtimes"));
host_options.managed_runtime_environment_root =
    Some(user_data_root.join("managed-runtime-envs"));
host_options.managed_runtime_config = LuaRuntimeManagedRuntimeConfig {
    worker_pool_max_size_per_environment: 8,
    worker_idle_ttl_secs: 120,
    persistent_session_limit_per_engine: 128,
    persistent_session_default_buffer_limit_bytes_per_stream: 2 * 1024 * 1024,
    invoke_default_timeout_ms: Some(30_000),
};
```

发行根必须是现有绝对目录，环境根必须是绝对路径。显式字段优先于旧默认路径，不会读取环境变量或 `PATH`。

宿主如果需要定位同一份解释器，调用：

```rust
resolve_managed_runtime_install(
    &distribution_root,
    ManagedRuntimeKind::Python,
    "3.14.6",
    "windows-x64",
)?;
```

不要在宿主中重复拼接目录或只解析清单而忽略规范包含关系。

## C ABI 与 JSON FFI

V1/V2 固定结构体未扩大：

- `luaskills_ffi_engine_new`：V1，使用旧字段。
- `luaskills_ffi_engine_new_v2`：V2，增加 `runtime_root`，受管根仍从它派生。
- `luaskills_ffi_engine_new_v3`：V3，增加独立受管发行根、环境根与可选 `FfiLuaRuntimeManagedRuntimeConfig *`。

JSON 引擎创建直接在 `host_options` 中传：

```json
{
  "runtime_root": "D:/VulcanCodeData/luaskills",
  "managed_runtime_distribution_root": "D:/VulcanCode/dependencies/runtimes",
  "managed_runtime_environment_root": "D:/VulcanCodeData/managed-runtime-envs",
  "managed_runtime_config": {
    "worker_pool_max_size_per_environment": 8,
    "worker_idle_ttl_secs": 120,
    "persistent_session_limit_per_engine": 128,
    "persistent_session_default_buffer_limit_bytes_per_stream": 2097152,
    "invoke_default_timeout_ms": 30000
  }
}
```

`managed_runtime_config` 的稳定默认值依次为 `4`、`60 秒`、`256`、`1 MiB/流` 与 invoke 无限制。全部已配置数值必须大于零；单次正数 `invoke.timeout_ms` 和 `session.open.buffer_limit_bytes` 分别覆盖对应的引擎默认值。标准 C ABI V3 传空策略指针即可保留完整默认值，`has_invoke_default_timeout_ms` 只能为 `0` 或 `1`。

只读解析接口：

```text
luaskills_ffi_managed_runtime_resolve_json
```

返回描述符包含规范安装根、规范可执行文件、运行时、版本、平台、清单哈希与可执行文件哈希。

## 环境缓存变化

环境 marker schema 从 1 升级为 2。环境哈希新增以下输入：

- 运行时安装清单 SHA-256
- 运行时可执行文件 SHA-256
- 包管理器安装清单 SHA-256
- 包管理器可执行文件 SHA-256

因此 0.5.0 环境不会被 0.5.1 错误复用。相同版本下替换发行资产也会生成新环境，并使已解析旧计划在使用前失败。

## 拉取脚本

默认调用保持不变。需要写入精确发行根时使用：

```powershell
scripts/deps/fetch_managed_runtimes.ps1 -RuntimeRoot <build_cache_root> -DistributionRoot <distribution_root> -Target all
```

```bash
RUNTIME_ROOT=<build_cache_root> MANAGED_RUNTIME_DISTRIBUTION_ROOT=<distribution_root> scripts/deps/fetch_managed_runtimes.sh all
```

## 平台

支持 Windows x86_64、Linux x86_64/aarch64、macOS x86_64/aarch64。Windows ARM/aarch64 与 ARM64EC 仍以 `windows_arm_is_not_supported` 显式拒绝，且不会创建环境根或回退系统解释器。

## 升级验收

1. 确认主仓、FFI 资产和三套 SDK 都是 `0.5.1`。
2. 分别验证默认根与显式根的 Python/Node `status`、`invoke`、`session.open`。
3. 验证状态中的 `distribution_source` / `environment_source`。
4. 修改一份测试可执行文件，确认新 `env_hash` 与旧值不同。
5. C ABI 宿主确认 V1/V2 结构体尺寸未变化，并新增包含资源策略指针的 V3 绑定。
6. 以非默认 B3-B7 创建引擎，验证 Worker 容量/空闲回收、Session 上限/默认缓冲、invoke 默认超时及单次覆盖优先级。
7. 发布前运行完整跨平台 CI；验证完成前不得推送 crates.io、npm 或 PyPI 包。
