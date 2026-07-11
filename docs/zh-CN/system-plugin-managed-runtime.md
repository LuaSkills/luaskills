# System Plugin 受管运行时使用指南

[English](../system-plugin-managed-runtime.md) | [中文文档首页](index.md) | [FFI 对接文档](ffi/integration-guide.md)

本文是持久 System Plugin 租约中包隔离 Python/Node 子运行时的任务式帮助入口，覆盖从包准备到确定性清理的完整链路。需要逐字段 Lua API 说明时，另见 [Lua Skill 开发手册](skill-development.md#59-受管-python-与-node-子运行时)。

## 1. 选择正确的执行模型

| 需求 | API | 生命周期 | 可用范围 |
| --- | --- | --- | --- |
| 单次结构化 handler 调用 | `vulcan.runtime.python.invoke(...)` / `node.invoke(...)` | 池化 Worker，每次处理一个请求 | 普通 Skill 与 System Plugin；Linux、macOS、Windows |
| 查看已声明运行时状态 | `vulcan.runtime.python.status()` / `node.status()` | 不创建子会话 | 普通 Skill 与 System Plugin；Linux、macOS、Windows |
| 让一个 stdio 进程跨多次 Lua eval 存活 | `vulcan.runtime.python.session.open(...)` / `node.session.open(...)` | 绑定一个 System 租约 VM | 仅 System Plugin；Windows x86_64、Linux x86_64/aarch64、macOS x86_64/aarch64 |
| 启动任意可执行程序 | `vulcan.process.session.open(...)` | 绑定当前 Lua userdata | 通用进程 API，不受包管理 |

只有当子进程拥有需要保留的内存状态或长期协议时才使用受管持久会话；互相独立的 JSON 兼容调用应优先使用池化 `invoke`。普通 Skill 被明确禁止打开持久受管会话。Windows ARM/aarch64 是唯一明确不支持的官方目标；它会在创建环境、创建快照或预留会话前失败，且绝不回退使用系统解释器。

`python.status()` 与 `node.status()` 的已配置、未配置、就绪和错误响应都会包含相同的稳定机器可读目标能力：

```json
{
  "persistent_session": {
    "supported": true,
    "target_os": "macos",
    "target_arch": "aarch64"
  }
}
```

Windows ARM 返回 `supported: false` 与 `reason: "windows_arm_is_not_supported"`。宿主必须依据 `supported` 和 `reason` 分支，不能把自由文本错误当作唯一判断依据。

## 2. 准备 System Plugin 包

System Plugin 可以使用以下最小布局：

```text
<runtime_root>/system_lua_lib/host-indexer/
  dependencies.yaml
  runtime/plugin.lua
  python/sidecar.py
  python/requirements.lock
  node/sidecar.mjs
  node/package.json
  node/pnpm-lock.yaml
```

包根必须是引擎规范 `system_lua_lib` 的绝对严格子目录，也是专用 VM 唯一新增的包 Lua 模块根。兄弟包不能共享它的 `require(...)` 搜索域、依赖身份、Worker 池或持久会话所有权。

只声明实际使用的运行时：

```yaml
python_runtime:
  version: "3.14.4"
  package_manager: uv
  package_manager_version: "0.11.28"
  lockfile: python/requirements.lock
node_runtime:
  version: "24.18.0"
  package_manager: pnpm
  package_manager_version: "11.11.0"
  package_json: node/package.json
  lockfile: node/pnpm-lock.yaml
```

`node_runtime.version` 必须是大于等于 `22.0.0` 的精确 SemVer；版本范围、标签、不完整版本与更旧版本都会在创建环境前被拒绝。lockfile 与包元数据会复制到私有构建输入，并在包管理器消费前重新计算哈希。

使用创建引擎时的同一个 `runtime_root` 准备可携带运行时：

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/deps/fetch_managed_runtimes.ps1 -RuntimeRoot <runtime_root> -Target all
```

```bash
RUNTIME_ROOT=<runtime_root> scripts/deps/fetch_managed_runtimes.sh all
```

LuaSkills 不会回退使用系统 Python、系统 Node.js、外部 `node_modules` 或未声明依赖。

## 3. 创建严格 System 租约

公共 `_json` FFI 请求包含 `engine_id` 与宿主注入的 authority：

```json
{
  "engine_id": 1,
  "sid": "host-indexer/workspace-42",
  "ttl_sec": 0,
  "replace": false,
  "cwd": "D:/runtime/system_lua_lib/host-indexer",
  "workspace_root": "D:/projects/current",
  "mounts": {
    "project": {
      "root": "D:/projects/current"
    }
  },
  "system_package": {
    "id": "host-indexer",
    "root": "D:/runtime/system_lua_lib/host-indexer",
    "dependencies_file": "dependencies.yaml"
  },
  "authority": "delegated_tool"
}
```

把该请求传给 `luaskills_ffi_system_runtime_lease_create_json`。authority 只接受 `system` 与 `delegated_tool`；它必须由宿主注入，不能直接转发不可信调用方字段。

System create 正文是严格结构：

- 必须包含 `system_package`。
- `system_package.root` 必须是 `system_lua_lib` 下的规范绝对严格子目录，且不能包含 Lua 搜索路径元字符。
- `dependencies_file` 是包相对路径，必须解析为包内普通文件，不能通过符号链接逃逸。
- 可选 `workspace_root` 必须是既有绝对目录。
- `cwd` 必须解析到包根或已授权工作区内；省略时默认为包根。
- 公共 `lua_roots`、`c_roots` 与未知字段都会被拒绝。

把返回的 `lease_id`、`sid` 与 `generation` 作为一个整体保存。后续 eval、status 与 close 请求都回传这三项，使陈旧句柄或串线调用显式失败。

标准 C ABI 的 `luaskills_ffi_system_runtime_lease_create(engine_id, request_json, ...)` 接收同一严格 create 正文，但 `request_json` 内不能再包含 `engine_id` 或 `authority`。

## 4. 打开并复用子会话

第一次 eval 打开子进程，并把 userdata 保存到专用租约 VM：

```lua
-- Open once and retain the userdata in this lease VM.
-- 仅打开一次，并在当前租约 VM 中保存 userdata。
sidecar = vulcan.runtime.python.session.open({
    file = "python/sidecar.py",
    args = { "--stdio" },
    cwd = "python",
    stdout_encoding = "utf-8",
    stderr_encoding = "utf-8",
    stdin_encoding = "utf-8",
    buffer_limit_bytes = 1024 * 1024,
})

return sidecar:status()
```

使用公共 `_json` System eval 入口时，宿主需要把上述 Lua 源码与租约身份、authority 组成请求。以下紧凑等价写法可直接序列化：

```json
{
  "engine_id": 1,
  "lease_id": "rt_...",
  "sid": "host-indexer/workspace-42",
  "generation": 1,
  "code": "sidecar = vulcan.runtime.python.session.open({ file = \"python/sidecar.py\", cwd = \"python\", buffer_limit_bytes = 1048576 }); return sidecar:status()",
  "args": {},
  "timeout_ms": 60000,
  "authority": "delegated_tool"
}
```

把请求传给 `luaskills_ffi_system_runtime_lease_eval_json`。与 create 相同，标准 C ABI 的 eval 正文不包含 `engine_id` 与 `authority`。

Node.js 只需替换 API 与入口路径：

```lua
sidecar = vulcan.runtime.node.session.open({
    file = "node/sidecar.mjs",
    args = { "--stdio" },
    cwd = "node",
    stdout_encoding = "utf-8",
    stderr_encoding = "utf-8",
    stdin_encoding = "utf-8",
    buffer_limit_bytes = 1024 * 1024,
})

return sidecar:status()
```

`session.open(...)` 只接受 `file`、`args`、`cwd`、三种流编码与正数 `buffer_limit_bytes`。`file` 必须是包相对的既有源文件；`args` 必须是稠密字符串数组，并会直接传递而不做 shell 展开。

子进程从每会话独立的不可变包快照执行，并通过 `LUASKILLS_MANAGED_CONTEXT_JSON` 接收受控包/租约元数据。Python 使用声明对应的受管虚拟环境，继承的 `PYTHONHOME`、`PYTHONPATH` 与用户 site-packages 会被移除；Node 从精确受管 `node_modules` 解析裸导入。

同一 `lease_id` 的后续 eval 直接使用已保存的 userdata：

```lua
sidecar:write(vulcan.json.encode(args.request) .. "\n")

local output = sidecar:read({
    timeout_ms = 2000,
    max_bytes = 65536,
    until_text = "\n",
})

return {
    managed_session_id = sidecar:status().managed_session_id,
    stdout = output.stdout,
    stderr = output.stderr,
    timed_out = output.timed_out,
    stdout_total_bytes = output.stdout_total_bytes,
    stdout_dropped_bytes = output.stdout_dropped_bytes,
}
```

userdata 方法如下：

| 方法 | 行为 |
| --- | --- |
| `write(...)` | 使用 `stdin_encoding` 编码标量，写入 stdin 并立即 flush |
| `read({ timeout_ms?, max_bytes?, until_text? })` | 按请求等待，并破坏性排空已捕获输出 |
| `status()` | 返回进程状态、缓冲计数与受管会话专属 `managed_session_id` |
| `close({ timeout_ms? })` | 关闭 stdin 并等待；超时后终止完整进程树 |
| `kill()` | 立即终止完整进程树 |

`read` 与 `status` 会暴露 `stdout_buffered_bytes`、`stdout_total_bytes`、`stdout_dropped_bytes` 及对应 `stderr_*` 字段。dropped 计数非零表示子进程输出超过有界缓冲保留能力；应提高上限或更及时排空。

## 5. 消费宿主事件

每个 System 受管会话可以发出四类事件：

- `stdout_readable`
- `stderr_readable`
- `exited`
- `failed`

无等待轮询：

```json
{
  "engine_id": 1,
  "max_events": 64,
  "authority": "delegated_tool"
}
```

有限等待：

```json
{
  "engine_id": 1,
  "max_events": 64,
  "timeout_ms": 1000,
  "authority": "delegated_tool"
}
```

分别调用 `luaskills_ffi_managed_session_events_poll_json` 或 `luaskills_ffi_managed_session_events_wait_json`。成功 `_json` 响应会把以下批次放在 `{"ok":true,"result":...}` 的 `result` 内：

```json
{
  "events": [
    {
      "system_lease_id": "rt_...",
      "sid": "host-indexer/workspace-42",
      "generation": 1,
      "managed_session_id": 7,
      "kind": "stdout_readable",
      "sequence": 12
    }
  ],
  "remaining": 0,
  "timed_out": false
}
```

poll 与 wait 都是破坏性有界排空，`max_events` 必须为正数。事件按全局单调递增 `sequence` 排序；`remaining` 表示排空后仍在队列中的逻辑事件数。只有 wait 在截止时间前没有事件时才返回 `timed_out=true`。同一会话的同类 readiness 会合并，因此事件只是读取有界 session 缓冲的信号，不是消息正文。

使用完整事件身份进行关联：

1. 找到同时匹配 `system_lease_id`、`sid` 与 `generation` 的宿主句柄。
2. 调度一次正常 System lease eval。
3. 在 eval 内确认/使用 `status().managed_session_id` 与事件相同的 userdata。
4. 调用 `read(...)`，直到所需输出被排空。

仓库 JSON FFI 辅助封装提供同语义的 `ManagedSessionEventsClient.poll(...)` 与 `.wait(...)`，见 [Python](../../examples/ffi/python/json_runtime.py) 和 [TypeScript](../../examples/ffi/typescript/json_runtime.ts)。

## 6. 安全使用标准 ABI wake callback

`luaskills_ffi_set_managed_session_wake_callback` 只存在于标准 C ABI。它只通知事件队列从空变为非空，不携带事件正文，也不能替代 poll/wait。

callback 在每个 engine 的单个串行后台调度器上运行，可以发信号、投递事件循环或把任务加入宿主队列。禁止在其中调用 Lua、同步执行租约、重入 callback 注册、跨 ABI 抛异常，或释放自己的 `user_data`。

当 callback 返回非零时，只要同一队列边沿仍待处理，运行时会按封顶指数退避异步重试。事件发布线程既不会执行 callback，也不会被重试阻塞。

释放 callback 状态前必须：

1. 用空 callback 调用注册接口以清除注册。
2. 等待清除调用成功返回；该调用会取消待处理重试并等待退役调用收敛。
3. 释放 `user_data`。

标准 `poll`/`wait` 通过 `result_json_out` 直接返回批次 JSON；`_json` 版本则返回统一响应包络。

## 7. 按正确顺序关闭

正常关闭顺序：

1. 停止为该租约调度新的应用工作。
2. 如果子协议提供优雅退出命令，先通过 `sidecar:write(...)` 发送该命令，不要关闭 userdata。
3. 在会话仍注册时等待并排空最终 `exited` 或 `failed` 事件，以及剩余 stdout/stderr。
4. 在一次租约 eval 中调用 `sidecar:close({ timeout_ms = ... })`，完成进程树与快照清理。
5. 使用匹配的 `lease_id`、`sid` 与 `generation` 关闭 System 租约。
6. 若注册过 wake callback，先清除它。
7. 释放 engine。

`close()` 与 `kill()` 会在进程清理后注销会话，因此该会话尚未消费的事件也会被移除。若子进程没有协作式退出命令，应把 `close()` 或 `kill()` 视为权威清理操作，并且不要再要求后续最终事件。宿主若必须取得最终 `exited` 边沿，就必须在调用任一清理方法前观察它。

公共 `_json` close 请求为：

```json
{
  "engine_id": 1,
  "lease_id": "rt_...",
  "sid": "host-indexer/workspace-42",
  "generation": 1,
  "authority": "delegated_tool"
}
```

把它传给 `luaskills_ffi_system_runtime_lease_close_json`。

失败链路同样强制清理：eval 失败会回滚该次 eval 新开的全部受管会话；租约 close、同 SID 替换、过期、VM 销毁、userdata 回收与 engine 销毁都会终止完整后代进程树。会话快照只在进程清理后删除，环境生命周期租约会一直持有到快照清理完成。

无关的普通 Skill 根重载不会替换专用 System 租约管理器；存活的 System 会话会继续运行，直到自身租约生命周期结束。

## 8. 安全与隔离规则

- `vulcan.runtime.system_plugin` 与 `vulcan.runtime.mounts` 是递归只读 userdata 视图；`vulcan.runtime.workspace_root` 是规范授权路径或 `nil`。
- 专用 System VM 会移除全局 `rawset` 与 Lua `debug` 库，阻止绕过元表边界。
- 包源码、依赖清单、lockfile、入口文件与授权 cwd 对象都按固定文件系统身份校验；路径穿越、符号链接逃逸与不支持的包对象会被拒绝。
- 存活 Worker 或会话快照持有跨进程环境共享租约。环境发布/替换只尝试非阻塞独占租约，并返回稳定 busy 错误，而不会与存活消费者竞态。
- 后台输出、退出与失败观察器只发布有界引擎事件，绝不执行 Lua。
- 不要转发调用方提供的 authority、任意包根或任意 workspace 根；它们属于宿主策略输入。

## 9. 常见问题排查

| 现象 | 检查项 | 处理方式 |
| --- | --- | --- |
| System create 拒绝 `system_package.root` | root 不是 `system_lua_lib` 的规范严格子目录，或含 Lua 路径元字符 | 把包放入引擎派生的 `system_lua_lib`，并传规范绝对路径 |
| `dependencies_file` 被拒绝 | 路径是绝对路径、用 `..` 逃逸、是符号链接或不是普通文件 | 使用 `dependencies.yaml` 这类包内相对普通文件 |
| 运行时已配置但不可用 | 没有为精确声明版本拉取可携带运行时/包管理器 | 对引擎 `runtime_root` 执行受管运行时拉取脚本 |
| Node 版本被拒绝 | 使用范围、标签、不完整版本或低于 `22.0.0` | 固定一个受支持的精确 SemVer |
| `session.open(...)` 返回 `windows_arm_is_not_supported` | 宿主目标是 Windows ARM/aarch64 | 不创建持久会话，也不回退系统解释器；改用受支持原生目标 |
| 普通 Skill 中 `session.open(...)` 被拒绝 | 持久受管会话要求专用 System 包上下文 | Skill 内使用 `invoke`，或把长期 sidecar 移到授权 System Plugin |
| 环境发布返回 busy | Worker 或会话快照仍持有生命周期租约 | 关闭所有者租约/会话后重试；不要手动删除或替换环境 |
| 收到事件但输出为空 | 其他 read 已排空、readiness 被合并，或输出超过缓冲上限 | 串行化同会话 read，并检查 `*_buffered_bytes` / `*_dropped_bytes` |
| 句柄报告 SID/generation 不匹配 | 宿主混用身份，或替换后仍使用旧句柄 | 从最新 create/list 结果重建句柄，并携带全部身份护栏 |
| wake callback 重复触发 | 队列边沿仍待处理，或 callback 返回非零 | 在宿主任务内排空事件，成功投递后返回零 |

## 10. 发布前检查清单

- 引擎使用预期 `runtime_root` 创建，可携带运行时布局已经存在。
- 每个 System 包都有唯一 id、严格包根、包内依赖清单与锁定依赖。
- 宿主注入 authority，并把 `lease_id + sid + generation` 作为一个句柄保存。
- 在 Windows x86_64、Linux x86_64/aarch64、macOS x86_64/aarch64 上原生运行持久会话验收；Windows ARM 由结构化能力拒绝。
- 宿主使用正数上限排空事件，且不把 readiness 当成输出正文。
- wake callback 只调度工作，并在释放其状态前完成清除。
- 需要最终事件时先观察事件再显式 close；关闭测试覆盖 close、kill、替换、过期、eval 失败、engine 销毁、后代进程、快照与输出缓冲丢弃。

## 11. 相关资料

- [FFI 对接文档](ffi/integration-guide.md)
- [FFI 宿主接入检查清单](ffi/host-checklist.md)
- [Lua Skill 开发手册](skill-development.md#59-受管-python-与-node-子运行时)
- [受管运行时 invoke 示例](../../examples/managed_runtime/README.md)
- [标准 C ABI 头文件](../../include/luaskills_ffi.h)
- [公共 JSON FFI 头文件](../../include/luaskills_json_ffi.h)
