# luaskills FFI 宿主接入检查清单

## 1. 这份清单的用途

这份清单不是完整设计说明，也不是 API 逐项参考。  
它的目标只有一个：

- 让宿主在第一次接入 `luaskills` FFI 时，能按最短路径完成自检

如果您需要完整背景说明，请继续阅读：

- [FFI 对接文档](integration-guide.md)
- [System Plugin 受管运行时使用指南](../system-plugin-managed-runtime.md)
- [宿主数据库 Provider 对接说明](../providers/host-database-provider-guide.md)
- [宿主工具结果桥接、宿主 LuaRuntime（`system_lua_lib`）与执行平面设计稿](../architecture/host-tooling-result-bridge-design.md)

## 2. 先选接入面

在真正写宿主代码之前，先确定这一步：

- 如果宿主本身是 Rust：
  - 优先直接接 Rust API
- 如果宿主是 C / C++ / C# / 其他能稳定处理结构体和 out 指针的语言：
  - 优先接标准 C ABI
- 如果宿主是 Python / Node.js / TypeScript / 动态脚本环境：
  - 优先接公共 `_json` FFI
  - TypeScript / Node.js 优先使用 [`luaskills-sdk-typescript`](https://github.com/LuaSkills/luaskills-sdk-typescript) 的 `@luaskills/sdk`，其中已经封装 JSON provider callback 注册与清理
- 如果宿主是 Python：
  - 优先使用 [`luaskills-sdk-python`](https://github.com/LuaSkills/luaskills-sdk-python) 的 `luaskills-sdk`，其中已经封装 JSON provider callback 注册与清理
- 如果宿主是 Go：
  - 可直接接标准 C ABI
  - 也可使用 [`luaskills-sdk-go`](https://github.com/LuaSkills/luaskills-sdk-go) 的 cgo JSON FFI SDK；该路径需要 `CGO_ENABLED=1`、C 编译器、链接库搜索路径与运行时动态库路径
  - 若需要 provider callback，需要宿主工程自行实现受控 cgo callback bridge，SDK 会通过显式错误提示这条边界
- 如果宿主需要“稳定主链 + 快速调试链”：
  - 可以混合使用
  - 标准 C ABI 负责主链
  - 公共 `_json` FFI 负责快速桥接和动态调试

## 3. 启动前检查

在 `engine_new` 之前，先确认这些条件：

- 已明确当前稳定运行时资产来自两个仓库：
  - `LuaSkills/luaskills` 提供 `luaskills-ffi-sdk-*`
  - `LuaSkills/luaskills-packages` 提供 `lua-runtime-packages-*` 与 `lua-deps-*`
- 已经准备好独立的 LuaSkills `runtime_root`，不要复用宿主程序安装目录
- JSON FFI 宿主直接在 host options 里传 `runtime_root`
- 标准 C ABI 宿主如果只传 `runtime_root`，使用 `FfiLuaRuntimeHostOptionsV2` 与 `luaskills_ffi_engine_new_v2`
- 标准 C ABI 宿主如果独立传受管发行根/环境根或 B3-B7 策略，使用 `FfiLuaRuntimeHostOptionsV3` 与 `luaskills_ffi_engine_new_v3`，不得扩大 V1/V2 结构体
- JSON FFI 宿主可直接设置 `managed_runtime_distribution_root`、`managed_runtime_environment_root` 与 `managed_runtime_config`
- 已确认 B3-B7 引擎默认值：每精确环境/包所有者池 `4` 个 Worker、空闲 `60` 秒、每引擎 `256` 个持久会话、每输出流 `1 MiB`、invoke 无默认超时；自定义数值全部大于零
- 标准 C ABI V3 的空 `managed_runtime_config` 指针表示完整默认策略；存在标记严格为 `0/1`；单次 `invoke.timeout_ms` 与 `session.open.buffer_limit_bytes` 分别优先于对应引擎默认值
- 显式发行根必须是现有绝对目录并直接包含 `python/`、`node/`；显式环境根必须是绝对路径且可安全创建
- 未显式设置时才使用 `<runtime_root>/dependencies/runtimes` 与 `<runtime_root>/dependencies/envs`
- 若宿主自身需要定位同一解释器，调用 `luaskills_ffi_managed_runtime_resolve_json`，不要重复实现目录拼接或回退 `PATH`
- `runtime_root` 固定包含或允许运行时创建这些目录：
  - `bin`
  - `libs`
  - `lua_packages`
  - `resources`
  - `skills`
  - `temp`
  - `temp/downloads`
  - `dependencies`
  - `state`
  - `databases`
  - `config`
  - `system_lua_lib`
- 宿主工具直接放在 `runtime_root/bin`，不要再放到 `runtime_root/bin/tools`
- FFI / 原生库和上级 DLL 依赖放在 `runtime_root/libs`
- 如果使用 packaged runtime：
  - `resources/lua-runtime-manifest.json` 必须存在
  - `resources/luaskills-packages-manifest.json` 必须存在
  - `resources/luaskills-packages/install-manifest.json` 必须存在
  - `resources/luaskills-packages/lua_packages.txt` 必须存在
  - `resources/luaskills-packages/platform-support.json` 必须存在
  - `resources/luaskills-packages/THIRD_PARTY_LICENSES.json` 必须存在
  - `resources/luaskills-packages/THIRD_PARTY_NOTICES.md` 必须存在
  - `resources/luaskills-packages/help/index.json` 必须存在
  - `resources/luaskills-packages/help/packages` 必须存在
  - `resources/luaskills-packages/help/modules` 必须存在
  - `licenses/luaskills-packages/index.json` 必须存在
- 已经决定数据库 provider 模式：
  - `dynamic_library`
  - `host_callback`
  - `space_controller`
- 如果要用 callback：
  - 全局 provider/service callback 必须先注册，再创建 engine
  - 按 engine 绑定的受管会话 wake callback 必须在 engine 创建后注册
  - TypeScript / Python 宿主优先使用 SDK 的 `set_*_provider_json_callback`，不要在业务代码里手写 buffer clone
  - 若要让 Lua 调用宿主工具，需注册 `luaskills_ffi_set_host_tool_json_callback`
- 如果要用 `space_controller`：
  - 已确认 `endpoint / auto_spawn / executable_path / process_mode`
- 如果连接远端 controller：
  - 必须关闭 `auto_spawn`
- 如果宿主准备接 `system_runtime_lease`：
  - 已接受固定的 `runtime_root/system_lua_lib` 作为默认 system Lua 库目录
  - 每次 `create` 都会传入严格 `system_package = { id, root, dependencies_file }`
  - `system_package.root` 是 `system_lua_lib` 下的绝对严格子目录，`dependencies_file` 是不能逃逸包根的包相对普通文件
  - 不再向 System `create` 传 `lua_roots / c_roots`；这两个字段只属于公共租约请求，System 请求遇到未知字段会直接拒绝
  - 已确认包根是专用 System VM 唯一新增的 Lua 模块根，兄弟插件与其 Python/Node 依赖环境互相隔离
  - 只有维护旧版引擎配置路径时才继续显式传入 `system_lua_lib_dir`
- 如果宿主准备消费结构化结果：
  - 已决定 `request_context.client_capabilities.host_result` 的注入策略
  - 已明确默认关闭，只有显式开启时才允许 skill 第四返回值进入宿主结果
- 如果宿主会高频使用 `vulcan.runtime.lua.exec`：
  - 已决定是否覆盖 `runlua_pool_config`
  - 未配置时默认是 `min=1 / max=4 / idle_ttl_secs=60`
- 如果宿主需要屏蔽默认包或冲突包：
  - 在 `FfiLuaRuntimeHostOptions.ignored_skill_ids` 填入对应目录派生的 `skill_id`
  - 被忽略 skill 不会准备依赖、不会绑定数据库，也不会注册 entry
- 如果宿主复用了旧版 demo 或安装脚本：
  - 不要再假设主仓库 release 自带完整 `lua-runtime-*`
  - 应确认 `scripts/deps/fetch_deps` 已经切到 `luaskills-packages` 的 packages 与 deps 资产，FFI SDK 拉取由 `scripts/ffi/fetch_ffi` 单独负责

## 4. 标准创建顺序

第一次接入最推荐按这个顺序实现：

1. `version`
2. `engine_new`
3. `load_from_roots`
4. `list_entries`
5. `call_skill`
6. `run_lua`
7. `engine_free`

如果这条链还没跑通，不建议先去接：

- `install / update / uninstall`
- 数据库 provider callback
- `vulcan.host.*` 宿主工具桥接 callback
- `space_controller`

正式宿主构造 skill roots 时，建议先固定三层语义：

```text
ROOT -> PROJECT -> USER
```

- `ROOT` 是系统控制级，只通过 system tools 或受控 system updater 调整。
- `PROJECT` / `USER` 是普通用户管理面可操作层。
- `ROOT` root 必须出现在启动或加载 root 链中；缺失时应直接报错。
- 普通 `vulcan.runtime.skills.*` 不应暴露 `ROOT` 目标选项。
- 若开放普通技能管理桥接，应同时提供层级列表能力，例如 `vulcan.runtime.skills.layers()`，让调用方获取当前实际存在的 `PROJECT` / `USER` 标签；bridge 关闭时不要把层级标记为可写。
- `ROOT` 中存在同名 `skill_id` 时，任何 authority 都不能向 `PROJECT` / `USER` install 或 update 同名 skill；普通层显式 uninstall 可用于清理残留。
- 若将 system tools 暴露给普通 tools，宿主 wrapper 必须固定注入 `DelegatedTool` authority；只有管理员、修复或受控更新流程才应注入 `System`。
- 查询与 prompt completion 类 FFI 入口也必须注入 authority；`DelegatedTool` 下不得返回 `ROOT` entries、help detail、`is_skill=true` 或 ROOT tool name 归属。`call_skill` / `run_lua` 是运行时执行面，不作为 ROOT 可见性边界；如果不希望普通用户执行任意 Lua，应由宿主单独封装或不暴露 `run_lua`。
- skill config 接口按 `skill_id` 管理配置，不按 root 可见性过滤；配置只有被 Lua 通过 `vulcan.config.*` 读取时才会影响行为。若不希望客户修改配置，不应暴露对应 `set/delete` 能力，核心行为应通过宿主硬逻辑或内置核心 skill 固化。
- `protected_skill_ids` 已取消，不应再作为宿主接入参数或普通管理保护机制。

## 5. 生命周期与查询辅助的第二阶段顺序

基础调用链打通后，再按这个顺序往下补：

1. `disable_skill / enable_skill`
2. `is_skill`
3. `skill_name_for_tool`
4. `prompt_argument_completions`
5. `list_skill_help`
6. `render_skill_help_detail`
7. `runtime_lease create / eval / status / list / close`
8. `system_runtime_lease create / eval / status / list / close`
9. `managed_session_events poll / wait` 与标准 ABI wake callback
10. `host_result` 关闭与开启两条链路

这样更容易定位问题，不会把“运行时主链问题”和“辅助接口问题”混在一起。

## 6. 内存释放检查

这是最容易误用的部分，建议逐项对照：

- 标准 C ABI 接口失败信息：
  - 通过 `FfiOwnedBuffer error_out` 返回
  - 读取后必须 `luaskills_ffi_buffer_free`
- 标准 C ABI 接口的单值文本输出：
  - 例如 `version_out` / `skill_id_out` / `result_json_out`
  - 也应按 `FfiOwnedBuffer` 读取与释放
- 结构化结果：
  - 不能手动释放内部字段
  - 必须调用结构体专用 free 函数
- 字符串数组：
  - 必须调用 `luaskills_ffi_string_array_free`
- 裸字符串辅助函数：
  - `luaskills_ffi_string_free` 只能释放 **luaskills 自己分配** 的字符串

一句话规则：

- 单值文本看 `FfiOwnedBuffer`
- 结构体结果看专用 free
- 不要自己猜该释放什么

## 7. 指针与缓冲规则

宿主在传参时要特别确认：

- `FfiBorrowedBuffer.ptr` 在调用期间必须有效
- `len > 0` 时，`ptr` 不能为 null
- 不能把宿主自己的内存伪装成 `FfiOwnedBuffer`
- 不能把宿主自己的字符串交给 `luaskills_ffi_string_free`

## 8. 回调与线程规则

如果宿主要接 callback，请对照下面几条：

- 全局 provider/service callback 必须在 `engine_new` 前注册；按 engine 绑定的受管会话 wake callback 必须在 `engine_new` 后注册
- callback 不能跨 C ABI 抛异常
- 同一线程内，不支持在一个 engine 调用尚未返回时再次重入同一个 engine
- 受管会话 wake callback 在每个 engine 的单个串行后台调度线程执行，只能唤醒/投递宿主任务；不得调用 Lua 或同步执行租约 eval
- wake callback 是事件队列由空转为非空时的边沿通知，不携带事件正文；宿主随后使用 poll/wait 破坏性读取事件
- wake callback 返回非零时，同一待处理边沿会通过封顶指数退避自动重试，不会阻塞 stdout/stderr 事件发布线程
- 替换或清除 wake callback 返回后，旧 callback 与 `user_data` 才保证不再在途，此后才能释放宿主状态
- `vulcan.host.*` 的 host-tool callback 只接收 JSON 请求并返回完整 JSON 结果，不支持 stream
- host-tool callback 内部必须自行处理工具 allowlist、权限、超时、审计与 secret 管理
- 如果一个进程里需要多套数据库 provider callback 逻辑：
  - 应分别创建不同 engine，让 engine 捕获各自的 provider callback 快照
  - 不要指望在 engine 创建后再切换全局 provider callback 来 retroactive 影响已存在 engine
- 如果一个进程里需要多套 `vulcan.host.*` host-tool callback 逻辑：
  - 当前 host-tool callback 是进程级能力面，Lua 调用时读取当前全局 callback
  - 多套 host-tool 逻辑需要宿主在 callback 内自行路由，或避免在同一进程内混用
- Go 宿主的 provider callback 不应直接挂临时闭包给进程级 C 回调；应先在宿主层设计明确的 cgo bridge、线程模型和生命周期。

## 9. 标准 C ABI 与公共 `_json` FFI 的最短判断

如果还在犹豫该走哪条路，直接按下面判断：

- 想要更稳定的底层契约：
  - 走标准 C ABI
- 想更快接进 Python / Node / TypeScript：
  - 走公共 `_json` FFI
- 想以后接更多语言绑定：
  - 先把标准 C ABI 跑通
- 想快速验证功能闭环：
  - 先跑公共 `_json` FFI 或 Python 示例

## 10. 示例入口速查

按目标直接选示例：

- 最短标准 ABI 闭环：
  - [examples/ffi/c/demo.c](../../../examples/ffi/c/demo.c)
  - [examples/ffi/python/demo.py](../../../examples/ffi/python/demo.py)
  - [examples/ffi/go/demo.go](../../../examples/ffi/go/demo.go)
  - [examples/ffi/typescript/demo.ts](../../../examples/ffi/typescript/demo.ts)
- 生命周期切换：
  - [examples/ffi/python/lifecycle_demo.py](../../../examples/ffi/python/lifecycle_demo.py)
  - [examples/ffi/go/lifecycle_demo/main.go](../../../examples/ffi/go/lifecycle_demo/main.go)
  - [examples/ffi/typescript/lifecycle_demo.ts](../../../examples/ffi/typescript/lifecycle_demo.ts)
- 查询辅助接口：
  - [examples/ffi/python/query_demo.py](../../../examples/ffi/python/query_demo.py)
  - [examples/ffi/go/query_demo/main.go](../../../examples/ffi/go/query_demo/main.go)
  - [examples/ffi/typescript/query_demo.ts](../../../examples/ffi/typescript/query_demo.ts)
- 标准 ABI 共用夹具：
  - [examples/ffi/standard_runtime/README.md](../../../examples/ffi/standard_runtime/README.md)
- 动态安装烟测：
  - [examples/ffi/demo_runtime/README.md](../../../examples/ffi/demo_runtime/README.md)
- 宿主 provider 接管：
  - [TypeScript SDK provider callback example](https://github.com/LuaSkills/luaskills-sdk-typescript/blob/main/examples/provider-callback.mjs)
  - [Python SDK provider callback example](https://github.com/LuaSkills/luaskills-sdk-python/blob/main/examples/provider_callback.py)
  - pip 安装后可运行 `python -m luaskills.examples.provider_callback`
  - [Go SDK provider callback example](https://github.com/LuaSkills/luaskills-sdk-go/blob/main/examples/provider_callback/main.go)
  - [examples/ffi/host_provider_demo/README.md](../../../examples/ffi/host_provider_demo/README.md)

## 11. 发布前最小自测

如果宿主准备进入正式接入联调，至少确认下面这些项目都通过：

- `engine_new -> load_from_roots -> list_entries -> call_skill -> run_lua -> engine_free`
- `disable_skill / enable_skill` 能反映到运行时视图
- `is_skill / skill_name_for_tool / prompt_argument_completions` 返回符合预期
- 所有 `error_out` 都能被正确读取和释放
- 所有结构化结果都通过专用 free 回收
- callback 场景下没有跨 ABI 异常
- callback 场景下没有同线程重入
- `vulcan.host.list / has / call` 在 callback 缺失、工具缺失和 callback 失败时都有可诊断结果
- 普通技能管理工具不会把 `ROOT` 暴露给用户安装、更新或卸载
- 若存在 ROOT 级系统 skill，已确认 PROJECT / USER 同名 skill 不会被加载
- `runtime_lease` 的同一 `lease_id` 多次 `eval` 会保留 Lua 全局状态
- `system_runtime_lease` 缺少 `system_package`，或携带 `lua_roots / c_roots` 等未知字段时会被严格拒绝
- `system_runtime_lease` 在宿主显式传入 `cwd` 时只接受包根或已授权 `workspace_root` 内的规范目录
- `system_runtime_lease` 未传 `cwd` 时默认使用 `system_package.root`，不会回落到整个 `system_lua_lib` 或 `skills`
- `vulcan.runtime.system_plugin` 与 `vulcan.runtime.mounts` 是可索引、可迭代、可 JSON 返回的递归只读 userdata 视图；三个宿主所有根字段不能替换，专用 System VM 会移除全局 `rawset` 与 Lua `debug` 库
- Python/Node `session.open(...)` 保存到 Lua 全局后可跨同一租约多次 eval 复用，`session:status().managed_session_id` 与宿主事件精确对应
- eval 失败、租约 close/replace/过期、VM/引擎销毁都会终止新开或仍存活的完整子进程树，Python/Node 不可变快照在进程清理后删除
- poll/wait 返回 `events / remaining / timed_out`，`max_events` 必须为正数；只有 wait 无事件到期时 `timed_out=true`
- wake callback 只调度宿主工作、不进入 Lua，宿主在后续任务中读取事件并按 `system_lease_id + sid + generation` 执行受控 eval/read
- 宿主未开启 `host_result` 时，skill 第四返回值会被忽略
- 宿主开启 `host_result` 时，支持的 skill 可以返回 `change_set` 等结构化结果，且宿主能独立读取
- `change_set` 的 `modify` 结果会稳定返回 `before + delete[] + insert[] + after` 形式的 hunk，而不是只给模糊 summary 或可选 patch
- `change_set` 的 `create` / `delete` / `rename` 结果已分别带上完整文件内容或 `old_path/new_path`

只要这组检查全部通过，宿主接入通常就已经具备稳定联调基础。
