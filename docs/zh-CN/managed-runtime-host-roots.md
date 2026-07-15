# 宿主指定受管运行时根目录

LuaSkills 0.5.1 将原先统一从 `runtime_root` 派生的三类文件系统授权拆分开：

- `runtime_root`：LuaSkills 数据、包存储、快照、配置和 System Plugin 状态。
- `managed_runtime_distribution_root`：只读 Python、Node.js、uv 与 pnpm 安装。
- `managed_runtime_environment_root`：可写 Python 虚拟环境与 Node 依赖环境。

Lua 调用方不能覆盖这些根，也不能传入解释器可执行文件。宿主在创建引擎时一次性选定它们。

## 创建引擎

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

let engine = LuaEngine::new(LuaEngineOptions::new(pool_config, host_options))?;
```

配置任意显式受管根时仍必须提供 `runtime_root`。两个显式根都必须是绝对路径。发行根必须已经存在；在受支持平台上，LuaSkills 会安全创建环境根，并固定每个活动根的平台原生文件系统对象身份。

省略新字段时继续使用 0.5.0 布局：

```text
distribution_root = <runtime_root>/dependencies/runtimes
environment_root  = <runtime_root>/dependencies/envs
```

优先级始终是“显式 Host Option > `runtime_root` 派生默认值”。环境变量和 `PATH` 永远不是候选来源。

## 引擎级资源策略

`LuaRuntimeHostOptions.managed_runtime_config` 在创建引擎时固定 Worker 与 Session 策略。Lua 代码不能修改该策略；任何非法零值都会在分配运行时根、环境、Worker 或 Session 前被拒绝。

| 字段 | 稳定默认值 | 作用域 |
| --- | ---: | --- |
| `worker_pool_max_size_per_environment` | `4` | 单个精确环境与包所有者池键允许的最大活动 Worker 数量 |
| `worker_idle_ttl_secs` | `60` | 空闲 Worker 回收秒数 |
| `persistent_session_limit_per_engine` | `256` | 单个引擎拥有的启动中与活动 Python/Node 持久会话总数 |
| `persistent_session_default_buffer_limit_bytes_per_stream` | `1048576`（1 MiB） | 每个 Session stdout 或 stderr 流默认保留字节数 |
| `invoke_default_timeout_ms` | `None` / `null` | `python.invoke`/`node.invoke` 默认超时；缺失表示无限制 |

所有已配置数值都必须大于零。单次 `session.open({ buffer_limit_bytes = ... })` 的正数值优先于该 Session 的引擎缓冲默认值；单次 `invoke({ timeout_ms = ... })` 的正数值优先于该调用的引擎超时默认值；其他调用继续使用不可变引擎策略。

## 发行目录契约

配置的发行根直接指向包含 `python/` 与 `node/` 的目录：

```text
<distribution_root>/
  python/
    cpython-3.14.6-windows-x64/
      runtime-manifest.json
      python.exe
    uv-0.11.28-windows-x64/
      runtime-manifest.json
      uv.exe
  node/
    node-24.18.0-windows-x64/
      runtime-manifest.json
      node.exe
    pnpm-11.11.0/
      runtime-manifest.json
      bin/pnpm.cjs
```

每份清单必须精确匹配请求的运行时、版本和平台。`executable` 必须是安全相对路径，其规范普通文件目标必须同时位于规范安装目录和配置发行根内。清单符号链接或逃逸安装目录的可执行文件符号链接都会被拒绝。

环境身份会纳入运行时与包管理器两份安装清单和两个可执行文件的 SHA-256。相同版本下替换任一资产都会生成不同环境哈希；已经解析的陈旧计划会在使用前被拒绝。

## 环境目录布局

环境根会被直接使用：

```text
<environment_root>/
  python/py-3.14.6/<env_hash>/
  node/node-24.18.0/<env_hash>/
```

环境 marker schema 已升级为 2，除 lock/package 元数据外还记录四个发行资产哈希。Worker 与持久 Session 使用同一份解析计划、生命周期租约、环境 marker 和包快照规则。

## 宿主只读解析接口

Rust 宿主可以直接复用 LuaSkills 的目录和清单校验：

```rust
let descriptor = resolve_managed_runtime_install(
    &distribution_root,
    ManagedRuntimeKind::Node,
    "24.18.0",
    "windows-x64",
)?;
```

`ManagedRuntimeInstallDescriptor` 返回规范安装根、规范可执行文件、精确版本/平台、清单哈希与可执行文件哈希。

Rust 描述符会有意保留原生规范 `PathBuf`，供依赖文件对象身份的宿主逻辑使用；Windows 下这些内部值可能采用 `\\?\` 形式。

对应 JSON FFI 是 `luaskills_ffi_managed_runtime_resolve_json`：

```json
{
  "distribution_root": "D:/VulcanCode/dependencies/runtimes",
  "runtime": "node",
  "version": "24.18.0",
  "platform": "windows-x64"
}
```

它返回标准 `{ "ok": true, "result": ... }` 包络，不创建引擎，也不修改环境状态。JSON 输入可接受带 `\\?\` / `\\?\UNC\` 前缀的 Windows 规范绝对盘符路径与 UNC 路径，但 `result.install_root` 与 `result.executable` 始终返回去除该前缀后的等价宿主可见形式；其他 verbatim 命名空间会在文件系统寻址前被拒绝。

## C ABI 版本

- V1 与 V2 布局完全不变，继续使用 `runtime_root` 派生受管根。
- `FfiLuaRuntimeHostOptionsV3` 嵌入完整 V2，再新增两个受管根与可选 `FfiLuaRuntimeManagedRuntimeConfig *`。
- `managed_runtime_config` 为空指针时保留全部稳定默认值。`has_invoke_default_timeout_ms` 必须严格为 `0` 或 `1`；为 `0` 时忽略数值超时成员。
- 使用 `luaskills_ffi_engine_new_v3` 创建 V3 引擎。
- JSON 引擎创建可直接在 `host_options` 中传两个新字段。

现有绑定不得扩大 V1 或 V2 结构体。

## Python、TypeScript 与 Go SDK

三个 0.5.1 SDK 都暴露相同的两个 JSON Host Options 与只读解析器。Python：

```python
client = LuaSkillsClient(
    runtime_root="D:/VulcanCodeData/luaskills",
    host_options={
        "managed_runtime_distribution_root": "D:/VulcanCode/dependencies/runtimes",
        "managed_runtime_environment_root": "D:/VulcanCodeData/managed-runtime-envs",
        "managed_runtime_config": {
            "worker_pool_max_size_per_environment": 8,
            "worker_idle_ttl_secs": 120,
            "persistent_session_limit_per_engine": 128,
            "persistent_session_default_buffer_limit_bytes_per_stream": 2 * 1024 * 1024,
            "invoke_default_timeout_ms": 30_000,
        },
    },
)
descriptor = LuaSkillsClient.resolve_managed_runtime_install(
    "D:/VulcanCode/dependencies/runtimes",
    "python",
    "3.14.6",
    "windows-x64",
    runtime_root="D:/VulcanCodeData/luaskills",
)
```

TypeScript：

```ts
const client = LuaSkillsClient.create({
  runtimeRoot: "D:/VulcanCodeData/luaskills",
  hostOptions: {
    managed_runtime_distribution_root: "D:/VulcanCode/dependencies/runtimes",
    managed_runtime_environment_root: "D:/VulcanCodeData/managed-runtime-envs",
    managed_runtime_config: {
      worker_pool_max_size_per_environment: 8,
      worker_idle_ttl_secs: 120,
      persistent_session_limit_per_engine: 128,
      persistent_session_default_buffer_limit_bytes_per_stream: 2 * 1024 * 1024,
      invoke_default_timeout_ms: 30_000,
    },
  },
});
const descriptor = LuaSkillsClient.resolveManagedRuntimeInstall({
  runtimeRoot: "D:/VulcanCodeData/luaskills",
  distributionRoot: "D:/VulcanCode/dependencies/runtimes",
  runtime: "node",
  version: "24.18.0",
  platform: "windows-x64",
});
```

Go：

```go
invokeTimeoutMS := uint64(30_000)
hostOptions := map[string]any{
    "managed_runtime_distribution_root": "D:/VulcanCode/dependencies/runtimes",
    "managed_runtime_environment_root":  "D:/VulcanCodeData/managed-runtime-envs",
    "managed_runtime_config": luaskills.ManagedRuntimeConfig{
        WorkerPoolMaxSizePerEnvironment:                   8,
        WorkerIdleTTLSecs:                                 120,
        PersistentSessionLimitPerEngine:                   128,
        PersistentSessionDefaultBufferLimitBytesPerStream: 2 * 1024 * 1024,
        InvokeDefaultTimeoutMS:                            &invokeTimeoutMS,
    },
}
descriptor, err := luaskills.ResolveManagedRuntimeInstall(luaskills.ManagedRuntimeResolveOptions{
    DistributionRoot: "D:/VulcanCode/dependencies/runtimes",
    Runtime:          luaskills.ManagedRuntimeKindNode,
    Version:          "24.18.0",
    Platform:         "windows-x64",
})
```

## 拉取与调试工具

受管运行时拉取器可以把解释器资产直接放进应用自有发行根，同时保留独立构建缓存根：

```powershell
scripts/deps/fetch_managed_runtimes.ps1 -RuntimeRoot D:\build-cache -DistributionRoot D:\VulcanCode\dependencies\runtimes -Target all
```

```bash
RUNTIME_ROOT=/tmp/luaskills-build MANAGED_RUNTIME_DISTRIBUTION_ROOT=/opt/vulcancode/dependencies/runtimes scripts/deps/fetch_managed_runtimes.sh all
```

使用 `managed_runtime_layout_check.py --distribution-root ... --environment-root ...` 校验拆分根。`luaskills-debug` 命令接受 `--managed-runtime-distribution-root`、`--managed-runtime-environment-root` 与 `--help` 列出的五个 `--managed-runtime-*` 资源参数；受管运行时冒烟脚本默认验证拆分根。

## Lua 与状态语义

Lua API 保持不变：

```lua
vulcan.runtime.python.status()
vulcan.runtime.python.invoke(...)
vulcan.runtime.python.session.open(...)
vulcan.runtime.node.status()
vulcan.runtime.node.invoke(...)
vulcan.runtime.node.session.open(...)
```

运行时状态新增 `distribution_root`、`distribution_source`、`environment_root` 与 `environment_source`。来源值稳定为 `host_configured` 或 `runtime_root_default`。Windows 状态路径采用宿主可见形式，绝不包含 `\\?\` / `\\?\UNC\`。

## 平台与失败处理

原生支持 Windows x86_64、Linux x86_64/aarch64、macOS x86_64/aarch64。Windows ARM/aarch64 与 ARM64EC 返回 `windows_arm_is_not_supported`；这些目标上不会创建环境根，也不会回退系统解释器。

常见配置错误都会显式返回：相对根、缺失发行根、文件冒充目录、平台原生对象被替换、清单非法、可执行路径不安全或发行资产哈希变化。应修复宿主所有的目录或资产，不应加入 `PATH` 或系统运行时降级。
