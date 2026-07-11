# 任务循环记录

## 2026-07-05 第 1 轮：修复测试级全局环境污染与 Windows 符号链接权限识别

### 问题探索

- 全量 `cargo test` 并发执行时出现 `process.session.open: program not found` 与 `failed to spawn managed runtime worker: program not found`。
- 单独运行相关 process session 与 managed runtime worker 测试均通过，顺序运行 `cargo test -- --test-threads=1` 后仅剩 Windows symlink 权限相关失败，确认 `program not found` 来自并发测试修改进程级 `PATH` / `PATHEXT` 后影响其他依赖 PATH 的测试。
- `src/runtime/engine/tests.rs` 已有局部 `process_env_test_guard`，但该锁只在 engine tests 模块内部可见，无法保护 `src/runtime/process_session/tests.rs`；同时 `managed_runtime_worker_pool_reuses_warm_worker` 自身依赖 `powershell` 却未持锁。
- Windows 符号链接失败返回 `os error 1314`，现有 `should_skip_windows_symlink_test` 只判断 `ErrorKind::PermissionDenied`，导致已有跳过机制没有生效。

### 执行调整

- 新增 `src/runtime/test_support.rs`，在测试编译范围内提供共享的进程级环境锁与环境变量恢复守卫。
- 将 engine tests 中原本私有的环境锁和恢复守卫迁移到共享测试支持模块。
- 让 `process_session` 中依赖 PATH 启动 `powershell`、`python`、`cmd` 或 `sh` 的测试持有共享环境锁。
- 让 managed runtime worker pool 测试持有共享环境锁，避免与 PATH 修改测试并发冲突。
- 将 Windows symlink 权限错误码 1314 纳入已有 skip 判断，保留对非权限类错误的 panic。

### 验证记录

- 修改前：`cargo test` 失败 9 项。
- 修改前：`cargo test -- --test-threads=1` 失败 4 项，均为 Windows symlink 权限相关。
- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test runtime::engine::tests::managed_runtime_worker_pool_reuses_warm_worker -- --nocapture` 通过。
- 修改后：`cargo test runtime::process_session::tests::dropping_process_session_kills_child_process -- --nocapture` 通过。
- 修改后：`cargo test runtime::engine::tests::execute_runlua_request_inline_supports_vulcan_fs_remove_for_dangling_symlink_entries -- --nocapture` 通过。
- 修改后：`cargo test` 通过，222 个测试全部通过。
- 审核后：已补充新增 `_env_guard` 变量的双语意图注释，并再次执行 `cargo fmt` 与 `cargo test`，结果均通过。

### 代码审核与遗留事项

- 本轮修改仅影响测试代码与测试编译模块，不改变生产运行时接口和行为。
- 共享环境锁覆盖了已确认依赖 PATH 的 process session 测试，以及 managed runtime worker pool 测试；原本修改 PATH / PATHEXT 的 engine 测试继续使用同一把锁。
- `cargo clippy --all-targets -- -D warnings` 当前仍失败，失败点为仓库既有问题，包含 FFI unsafe 函数缺少 `# Safety` 文档、部分默认初始化写法、可折叠 if 等；本轮未展开修复，作为后续循环任务候选。
- 未发现本轮新增代码需要继续自动修复的问题。

## 2026-07-05 第 2 轮：补全公开 FFI unsafe 出口安全契约

### 问题探索

- 基线验证中 `cargo test` 继续通过，222 个测试全部通过。
- `cargo clippy --all-targets -- -D warnings` 失败 155 项，其中最大单类问题是 98 个 `clippy::missing_safety_doc`。
- 完整枚举 `src/ffi.rs` 与 `src/ffi_standard.rs` 后确认，98 个问题全部来自公开 `pub unsafe extern "C" fn` C ABI 出口。
- 执行链路为：外部宿主从 `include/luaskills_json_ffi.h` 或 `include/luaskills_ffi.h` 调用 JSON FFI / 标准 ABI；Rust 侧在 `src/ffi.rs` 解析 `FfiBorrowedBuffer` JSON 请求，在 `src/ffi_standard.rs` 解析裸指针、输出槽与 callback；返回的 LuaSkills 拥有内存必须由匹配 free 函数释放。
- 该问题不是运行时崩溃，而是 FFI 边界安全契约缺失：外部调用方无法从 Rust 文档直接确认裸指针、借用缓冲、输出槽、callback 生命周期和返回分配的安全前置条件。

### 执行调整

- 为 `src/ffi.rs` 中 39 个公开 unsafe JSON FFI 出口补充双语 `# Safety` 文档。
- 为 `src/ffi_standard.rs` 中 59 个公开 unsafe 标准 ABI 出口补充双语 `# Safety` 文档。
- 安全文档统一覆盖以下契约：调用期间指针与借用缓冲必须有效，输出槽必须可写，LuaSkills 拥有的返回分配必须用匹配释放函数处理，已注册 callback 必须保持可调用，callback 不得跨 FFI 边界展开异常。
- 审核后自动修复文档块位置：将 `# Safety` 块整理到 `#[unsafe(no_mangle)]` 属性之前，使函数摘要、安全契约和属性顺序更符合 Rust 文档习惯。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test` 通过，222 个测试全部通过。
- 修改后：`cargo clippy --all-targets -- -D warnings` 仍失败，但失败数从 155 降至 57，`clippy::missing_safety_doc` 已不再出现。
- 修改后：脚本检查 `src/ffi.rs` 与 `src/ffi_standard.rs`，确认 `pub unsafe extern "C" fn` 数量为 98，`# Safety` 数量为 98，且每个 unsafe 出口附近均存在安全文档。

### 代码审核与遗留事项

- 本轮未修改 FFI 运行逻辑、符号名、结构体布局或 C 头文件，属于安全契约文档补全。
- 本轮新增注释均按英文说明与中文说明成对出现，符合双语注释要求。
- 仍遗留 57 个 clippy 问题，主要包括 `ffi_standard.rs` 可折叠 if、`runtime/engine/tests.rs` 默认初始化后字段赋值、若干 needless borrow、too many arguments 等；这些问题已具备下一轮继续清理的明确候选。
- 未发现本轮新增 FFI 安全文档需要继续自动修复的问题。

## 2026-07-05 第 3 轮：修复标准 FFI entry descriptor 头文件布局缺字段并清理回调桥接分支

### 问题探索

- 基线验证中 `cargo test` 继续通过，222 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 仍失败 57 项。
- 追查 `src/ffi_standard.rs` 的 clippy 信号时发现更高优先级问题：Rust `#[repr(C)]` 类型 `FfiRuntimeEntryDescriptor`、分配函数 `alloc_entry_descriptor`、释放函数 `free_entry_descriptor`、Rust 测试、Python 示例和 TypeScript 示例都包含 `input_schema_json` 字段。
- 公共 C 头文件 `include/luaskills_ffi.h` 中的 `FfiRuntimeEntryDescriptor` 缺少 `input_schema_json`，导致 C/Go cgo 等直接依赖头文件的宿主会按错误结构布局读取 `parameters` 和 `parameters_len`。
- 执行链路为：宿主调用 `luaskills_ffi_list_entries`，Rust 侧分配包含 `input_schema_json` 的 entry descriptor list，调用方按 `include/luaskills_ffi.h` 结构体声明解读返回内存；头文件缺字段会让声明与实际内存布局错位。
- 同一文件中 JSON/SQLite/LanceDB/model callback 桥接存在多处嵌套 `if`，语义上都用于拒绝“成功状态下仍返回非空 error_out”的异常 callback 响应，适合收敛为更直接的 let-chain。

### 执行调整

- 在 `include/luaskills_ffi.h` 的 `FfiRuntimeEntryDescriptor` 中补充 `input_schema_json` 字段，并添加中英文说明，使 C ABI 头文件与 Rust `#[repr(C)]` 类型、释放逻辑和跨语言示例保持一致。
- 在 `examples/ffi/c/demo.c` 中打印 `input_schema_json`，让 C 示例覆盖该字段，避免后续头文件再次漂移而无人注意。
- 清理 `src/ffi_standard.rs` 中 `alloc_entry_descriptor` 的无意义借用。
- 将 JSON provider、SQLite provider、LanceDB provider 和模型 callback 错误响应归一化中的嵌套 `if` 收敛为等价 let-chain，保留原有错误语义和返回文本。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test` 通过，222 个测试全部通过。
- 修改后：`cargo test ffi_standard -- --nocapture` 通过，11 个相关测试全部通过。
- 修改后：`cargo clippy --all-targets -- -D warnings` 仍失败，但失败数从 57 降至 51，且新 clippy 输出中不再包含 `src\\ffi_standard.rs`。
- 已确认 `include/luaskills_ffi.h`、`src/ffi_standard.rs`、`examples/ffi/c/demo.c` 均出现 `input_schema_json`，C 头文件、Rust 实现与 C 示例已对齐。
- 本机缺少 `gcc`、`clang` 与 `cl`，因此无法对 C 示例执行编译级验证。

### 代码审核与遗留事项

- 本轮修复了标准 FFI C 头文件与 Rust 实际 ABI 布局不一致的问题，属于外部宿主集成风险修复。
- `src/ffi_standard.rs` 的分支清理为等价结构化调整，没有改变 callback 成功/失败判定规则。
- 仍遗留 51 个 clippy 问题，主要集中在 `runtime/engine/tests.rs` 默认初始化后字段赋值、`runtime/config.rs` 小型可读性问题、`runtime/engine/host_result.rs` 可折叠 if，以及若干函数参数过多设计问题。
- 未发现本轮新增代码需要继续自动修复的问题；C 示例编译验证待具备 C 编译器环境后补跑。

## 2026-07-05 第 4 轮：收敛技能配置持久化路径的小型控制流与借用坏味道

### 问题探索

- 基线验证中 `cargo test` 继续通过，222 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 失败 51 项。
- 本轮选定 `src/runtime/config.rs` 的 `SkillConfigStore` 作为优化目标，因为剩余 clippy 信号集中在同一条生产配置持久化路径上，而不是孤立测试写法。
- 已追清执行流程：JSON FFI 与标准 FFI 的 skill config 接口进入 `RuntimeEngine::{list,get,set,delete}_skill_config_*`，Lua 侧 `vulcan.config.get/has/set/delete/list` 先通过 `current_vulcan_config_skill_id` 取得当前技能上下文，再调用同一个 `SkillConfigStore`。
- `SkillConfigStore` 的读路径经 `with_document_read` 获取有效配置文件路径与进程级共享锁，再由 `read_document_from` 读取 JSON；写路径经 `with_document_mut` 在同一把路径锁下执行读改写，再由 `write_document_to` 写入临时文件并调用 `replace_file_atomically` 提交。
- 删除路径已由 `skill_config_store_delete_prunes_empty_skill_namespace` 覆盖：删除最后一个 key 后必须清理空技能命名空间，并将持久化文件保持为 `{ "skills": {} }` 语义。
- clippy 指出的四处问题均为等价表达式层面的坏味道：删除后的空 namespace 清理存在可折叠嵌套 `if`，读取文件与替换文件存在重复借用，Windows 锁路径归一化分支存在不必要 `return`。

### 执行调整

- 将 `delete_value` 中的嵌套 `if let Some(items)` 与 `items.is_empty()` 收敛为 let-chain，保留“只有确认为空才移除 namespace”的行为。
- 将 `fs::read_to_string(&file_path)` 调整为 `fs::read_to_string(file_path)`，避免对已经是 `&Path` 的参数再次借用。
- 将 `replace_file_atomically(&temp_path, &file_path)` 调整为 `replace_file_atomically(&temp_path, file_path)`，让调用点与函数签名直接对齐。
- 移除 `normalize_skill_config_lock_identity_path` Windows cfg 分支中的不必要 `return`，保持同一个 `normalize_windows_skill_config_lock_identity_path(path)` 返回值。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test runtime::config -- --nocapture` 通过，10 个相关测试全部通过。
- 修改后：`cargo test` 通过，222 个测试全部通过。
- 修改后：`cargo clippy --all-targets -- -D warnings` 仍失败，但失败数从 51 降至 47，且新输出中不再包含 `src\runtime\config.rs`。
- 修改范围审核：`git diff -- src\runtime\config.rs` 显示 1 个文件 7 行新增、7 行删除，未出现存储格式、API 签名或错误消息变更。

### 代码审核与遗留事项

- 本轮改动仅收敛控制流和重复借用表达式，未改变技能配置文件路径解析、进程级锁粒度、JSON 文档格式、删除返回值或 Lua/FFI 可见行为。
- 删除 namespace 的逻辑仍在 `with_document_mut` 的路径锁保护下执行，未引入新的并发窗口。
- 仍遗留 47 个 clippy 问题，下一轮候选包括 `runtime/engine/host_result.rs` 的 host_result payload 校验分支、`runtime/engine/lease.rs` 的租约匹配分支、以及若干参数过多的结构性问题。
- 未发现本轮新增代码需要继续自动修复的问题。

## 2026-07-05 第 5 轮：收敛 host_result 与 change_set 协议校验路径的条件表达式

### 问题探索

- 基线验证中 `cargo test` 继续通过，222 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 失败 47 项。
- 本轮选定 `src/runtime/engine/host_result.rs`，因为 clippy 告警集中在结构化 host_result 与 canonical `change_set` 校验路径上，属于生产运行时协议边界，而不是单纯测试代码风格。
- 已追清执行流程：`LuaEngine::call_skill` 调用 Lua 后进入 `parse_tool_call_output`；该函数从 `LuaInvocationContext` 解析 host_result 能力，只有宿主显式开启后才解析 Lua 第四返回值。
- host_result 第四返回值先由 `parse_host_result_value` 转为 JSON object，校验 `kind`、`allowed_kinds` 与 `payload`，再由 `normalize_host_result_payload` 对 `change_set` payload 进行归一化，最后进入 `validate_host_result_payload`。
- `change_set` 路径中，delete 记录先由 `normalize_change_set_delete_file_record` 统一补齐 `content_mode` 与 `total_line_count`，必要时拆分 `content_head` 和 `content_tail`；随后 `validate_change_set_payload` 与 `validate_change_set_file_payload` 校验 mode、summary、files、patch、modify hunk、create/delete/rename 等字段。
- 相关测试已覆盖合法 lifecycle payload、delete 全量模式、delete 超大内容截断、显式截断缺少 total_line_count、modify 缺 hunks、空 hunk、rename 缺路径、hunk 行号顺序等协议边界。
- clippy 指出的四处问题均为等价表达式坏味道：payload 字节上限判断、summary 类型判断、patch 类型判断存在可折叠嵌套 `if`，`split_change_set_lines` 存在可省略显式生命周期。

### 执行调整

- 将 `validate_host_result_payload` 中 `max_payload_bytes` 与 `payload_json.len() > limit` 的双层判断收敛为 let-chain，保留相同错误文本与字节数计算。
- 将 `validate_change_set_payload` 中 `summary` 存在且非 string/null 的双层判断收敛为 let-chain，保持 summary 允许缺失、字符串或 null 的原有规则。
- 将 `validate_change_set_file_payload` 中 `patch` 存在且非 string/null 的双层判断收敛为 let-chain，保持 patch 允许缺失、字符串或 null 的原有规则。
- 将 `split_change_set_lines<'a>(text: &'a str) -> Vec<&'a str>` 调整为 `split_change_set_lines(text: &str) -> Vec<&str>`，交由 Rust 生命周期省略规则表达同一借用关系。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test runtime::engine::tests::validate_change_set -- --nocapture` 通过，7 个相关测试全部通过。
- 修改后：`cargo test runtime::engine::tests::normalize_change_set -- --nocapture` 通过，2 个相关测试全部通过。
- 修改后：`cargo test` 通过，222 个测试全部通过。
- 修改后：`cargo clippy --all-targets -- -D warnings` 仍失败，但失败数从 47 降至 43，且新输出中不再包含 `src\runtime\engine\host_result.rs`。
- 修改范围审核：`git diff -- src\runtime\engine\host_result.rs` 显示 1 个文件 26 行新增、24 行删除，未出现协议字段、错误消息、返回类型或调用入口变更。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮改动仅收敛协议校验表达式，没有改变 host_result 能力开关、allowed kind 过滤、payload 大小限制、change_set 归一化与校验语义。
- delete 内容拆行仍使用 `text.lines()`，保留“不把结尾换行算作额外行”的既有设计。
- 仍遗留 43 个 clippy 问题，下一轮候选包括 `runtime/engine/lease.rs` 的租约匹配分支、`runtime/engine/runlua.rs` 的超时分支与参数过多结构问题、以及 `runtime/engine/tests.rs` 的 Default 初始化后字段赋值问题。
- 未发现本轮新增代码需要继续自动修复的问题。

## 2026-07-05 第 6 轮：收敛运行时租约身份校验分支

### 问题探索

- 基线验证中 `cargo test` 继续通过，222 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 失败 43 项。
- 本轮选定 `src/runtime/engine/lease.rs`，因为 clippy 告警集中在运行时租约身份校验路径上，涉及 public/system 租约面、SID 与 generation 这些宿主恢复逻辑依赖的稳定字段。
- 已追清执行流程：JSON FFI 与标准 FFI 的 runtime lease create/eval/status/list/close 接口最终调用 `LuaEngine` 上的租约 JSON API；这些 API 按 public 或 system profile 进入同一套 `RuntimeSessionManager`。
- `RuntimeSessionManager::insert` 为每个 SID 签发递增 generation，创建 `RuntimeSession` 并写入 active lease 表；replace、close、expire 会把活跃租约转入 tombstone，以便后续请求继续返回稳定终态错误。
- `RuntimeSessionManager::get`、`status` 与 `close` 在返回活跃 session 或 tombstone 终态错误前，都会调用 `validate_session_identity` / `validate_tombstone_identity` 与 `validate_session_profile` / `validate_tombstone_profile`，用于拒绝 SID、generation 或 profile 不匹配的宿主回传请求。
- 相关测试已覆盖 stateful eval、system runtime lease cwd/profile、closed、replaced、stale handle、busy replace、SID mismatch、generation mismatch、active list 等租约关键行为。
- clippy 指出的四处问题均为等价表达式坏味道：profile mismatch、tombstone profile mismatch、SID mismatch、generation mismatch 的“可选期望值存在且实际值不匹配”判断使用了嵌套 `if`。

### 执行调整

- 将 `validate_session_profile` 中 `expected_profile` 存在且 `session.profile` 不匹配的双层判断收敛为 let-chain，保持 `lease_profile_mismatch` 错误码和 message 不变。
- 将 `validate_tombstone_profile` 中 `expected_profile` 存在且 `tombstone.profile` 不匹配的双层判断收敛为 let-chain，保持终态租约 profile 校验语义不变。
- 将 `validate_identity_parts` 中 `expected_sid` 存在且 `actual_sid` 不匹配的双层判断收敛为 let-chain，保持 `lease_sid_mismatch` 错误码和 message 不变。
- 将 `validate_identity_parts` 中 `expected_generation` 存在且 `actual_generation` 不匹配的双层判断收敛为 let-chain，保持 `lease_generation_mismatch` 错误码和 message 不变。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test runtime_session -- --nocapture` 通过，17 个相关测试全部通过。
- 修改后：`cargo test system_runtime_lease -- --nocapture` 通过，1 个相关测试通过。
- 修改后：`cargo test` 通过，222 个测试全部通过。
- 修改后：`cargo clippy --all-targets -- -D warnings` 仍失败，但失败数从 43 降至 39，且新输出中不再包含 `src\runtime\engine\lease.rs`。
- 修改范围审核：`git diff -- src\runtime\engine\lease.rs` 显示 1 个文件 42 行新增、42 行删除，仅为四处条件结构收敛。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮改动没有改变租约创建、替换、关闭、过期、tombstone 保留、profile 过滤、SID/generation 校验或 JSON 错误载荷语义。
- active session 与 terminal tombstone 仍使用同一组稳定错误码供宿主恢复逻辑判断。
- 仍遗留 39 个 clippy 问题，下一轮候选包括 `runtime/engine/runlua.rs` 的超时分支与参数过多结构问题、`process_session.rs` 的 return/if 收敛、以及 `runtime/engine/tests.rs` 的 Default 初始化后字段赋值问题。
- 未发现本轮新增代码需要继续自动修复的问题。

## 2026-07-05 第 7 轮：抽取 runlua 隔离执行依赖快照并收敛超时轮询分支

### 问题探索

- 基线验证中 `cargo test` 继续通过，222 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 失败 39 项。
- 本轮选定 `src/runtime/engine/runlua.rs`，因为剩余告警不仅有超时轮询的嵌套 `if`，还集中暴露了 `populate_vulcan_luaexec_bridge`、`acquire_runlua_vm`、`execute_runlua_request_inline_with_runtime` 三个函数长期成组传递同一批运行时依赖。
- 已追清执行流程：普通 skill VM 在 `create_vm_with_runtime_state` 中注册 `vulcan.runtime.lua.exec`；该桥接函数解析 Lua 表入参为 `RunLuaExecRequest`，读取当前 tool_name 作为重入保护上下文，再进入 `execute_runlua_request_inline_with_runtime`。
- 宿主直接调用 `execute_runlua_request_json_inline` 时，先解析 JSON 为 `RunLuaExecRequest`，再通过 `execute_runlua_request_inline` 从当前 engine 快照构造同一批依赖并执行隔离 runlua。
- 隔离执行路径中，`execute_runlua_request_inline_with_runtime` 负责解析 inline code 或 file、从专用 `runlua_pool` 获取 VM、注入模拟 request context、内部执行上下文、文件上下文、LanceDB/SQLite 空上下文、托管 IO 与超时 guard，最后渲染 Markdown 结果。
- 成组传递的参数归属已确认：`runlua_pool`、`skills`、`entry_registry`、`host_options`、`skill_config_store`、`runtime_skill_roots`、`lancedb_host`、`sqlite_host` 都是隔离 runlua VM 创建与嵌套调用所需的运行时依赖快照，不是多个互不相关的临时参数。
- 相关测试已覆盖 dedicated pool 复用、托管 `io.open`、默认输入输出、Unicode 文件操作、符号链接处理、nested luaexec、上下文恢复等 inline runlua 行为。

### 执行调整

- 新增内部结构 `RunLuaRuntimeContext`，用双语文档描述并封装隔离 runlua 执行所需的运行时依赖快照。
- 新增 `RunLuaRuntimeContext::from_engine`，从当前 engine 与显式 `skills` / `entry_registry` 快照构造依赖上下文，保持普通 skill VM 快照入口与宿主 inline 入口的原有数据来源。
- 将 `populate_vulcan_luaexec_bridge` 的 9 个参数收敛为 `lua` 与 `RunLuaRuntimeContext`，闭包内按调用次数克隆上下文，避免继续扩散参数列表。
- 将 `acquire_runlua_vm` 改为接收 `&RunLuaRuntimeContext`，由上下文提供 VM pool、技能快照、入口注册表、host options、配置存储、runtime roots 与可选数据库桥接。
- 将 `execute_runlua_request_inline_with_runtime` 改为接收 `RunLuaRuntimeContext`，统一驱动 VM 获取、request scope guard 与 runlua 执行环境配置。
- 将进程 exec 超时轮询中的 `timeout` 与 `started_at.elapsed() >= limit` 双层判断收敛为 let-chain，保留 kill/wait 与 `timed_out` 置位逻辑。
- 审核中发现桥接注入前有一次不必要的上下文克隆，已去除，只保留闭包调用时按需克隆。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test execute_runlua_request_inline -- --nocapture` 通过，32 个相关测试全部通过。
- 修改后：`cargo test luaexec -- --nocapture` 未匹配到测试，0 个执行；不作为覆盖依据。
- 修改后：`cargo test` 通过，222 个测试全部通过。
- 修改后：`cargo clippy --all-targets -- -D warnings` 仍失败，但失败数从 39 降至 35，且新输出中不再包含 `src\runtime\engine\runlua.rs`。
- 修改范围审核：`git diff -- src\runtime\engine\runlua.rs src\runtime\engine.rs` 显示 2 个文件 81 行新增、78 行删除，核心变化为依赖上下文抽取、调用点收敛与超时条件收敛。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变 runlua 请求 JSON 结构、专用 VM 池、技能与入口快照来源、host options 使用、托管 IO、超时 guard、Markdown 渲染或错误文本语义。
- `RunLuaRuntimeContext` 明确表达了隔离 runlua 执行依赖归属，避免后续新增数据库桥接、配置或运行时根时继续扩大多个函数签名。
- 仍遗留 35 个 clippy 问题，下一轮候选包括 `process_session.rs` 的 return/if 收敛、`runtime/engine.rs` 的小型性能/可读性问题，以及 `runtime/engine/tests.rs` 的 Default 初始化后字段赋值问题。
- 未发现本轮新增代码需要继续自动修复的问题。

## 2026-07-05 第 8 轮：收敛进程会话平台分支与 Windows 进程树错误记录逻辑

### 问题探索

- 基线验证中 `cargo test` 继续通过，222 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 失败 35 项。
- 本轮选定 `src/runtime/process_session.rs`，因为剩余告警集中在真实进程会话生命周期路径上，覆盖非 reap 状态探测、进程树隔离配置、Windows Job Object 归属、ToolHelp snapshot fallback 与整棵进程树终止。
- 已追清执行流程：`ManagedProcessSession::open` 创建子进程与后台读管道；`non_reaping_status` 用平台专用方式观察进程状态；`kill_process_tree_and_wait` 与 `ProcessTreeController::terminate` 负责 drop/close/kill 时回收直接子进程与后代进程。
- Windows 路径中，`prepare_command` 根据宿主是否处于 Job Object 决定是否请求 `CREATE_BREAKAWAY_FROM_JOB`；`attach` 优先使用 Job Object，若归属被拒绝则降级为 ToolHelp snapshot 策略；`terminate_windows_process_tree_snapshot` 先终止后代，再终止根进程，并保留第一个终止错误。
- 相关测试覆盖 drop 清理、后代进程清理、kill 幂等、reader handle 保留、子进程已退出后仍清理后代等进程会话行为。
- clippy 指出的生产代码问题均为等价表达式坏味道：Windows cfg 块末尾使用不必要 `return`，Windows Job Object match 分支使用不必要 `return`，以及 snapshot 终止逻辑用嵌套 `if` 记录首个错误。
- 另有一个测试告警位于 `wait_for_descendant_pid` 的 Windows ToolHelp 快照优先分支，语义上同样是“成功收集 descendants 且存在首个 pid 时提前返回”。

### 执行调整

- 将 `non_reaping_status` Windows cfg 块的 `return peek_windows_process_status(...)` 收敛为块尾表达式，保持不提前 reap `Child` 的状态探测方式。
- 将 `ProcessTreeController::prepare_command` Windows cfg 块的 `return Ok(in_job)` 收敛为块尾表达式，保持 breakaway 请求判断不变。
- 将 `ProcessTreeController::attach` Windows Job Object 归属 match 中的 `return Ok(...)` 与 `return Err(...)` 收敛为 match arm 表达式，保持 Job 优先与 ToolHelp fallback 语义不变。
- 将 `ProcessTreeController::terminate` Windows cfg 块的 `return self.strategy.terminate(_child)` 收敛为块尾表达式，保持按策略终止进程树。
- 将 `terminate_windows_process_tree_snapshot` 中“终止后代失败且尚未记录错误”与“终止根进程失败且尚未记录错误”的嵌套判断收敛为 let-chain，继续只保留第一个错误。
- 将 `wait_for_descendant_pid` Windows 测试分支的 descendants 收集与首个 pid 提取收敛为 let-chain，保持优先使用 ToolHelp 快照、失败后再读 stdout 的原行为。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test process_session -- --nocapture` 通过，9 个相关测试全部通过。
- 修改后：`cargo test` 通过，222 个测试全部通过。
- 修改后：`cargo clippy --all-targets -- -D warnings` 仍失败，但失败数从 35 降至 26，且新输出中不再包含 `src\runtime\process_session.rs` 或 `src\runtime\process_session\tests.rs`。
- 修改范围审核：`git diff -- src\runtime\process_session.rs src\runtime\process_session\tests.rs` 显示生产代码仅为平台 cfg 块、match arm 与首个错误记录条件的等价收敛；测试文件 diff 中包含早前循环已加入但尚未提交的 PATH guard，本轮新增部分为 Windows descendant 探测 let-chain。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变进程会话请求结构、子进程启动参数、Job Object/ToolHelp 策略选择、进程树终止顺序、错误文本或 reader 回收语义。
- Windows snapshot 终止仍按后代反向顺序先处理子进程，再处理根进程，并继续只返回第一个观察到的终止错误。
- 仍遗留 26 个 clippy 问题，下一轮候选包括 `runtime/engine.rs` 的小型性能/可读性问题、`ffi/tests.rs` 与 `runtime/engine/tests.rs` 的大小写比较问题、以及 `runtime/engine/tests.rs` 的 Default 初始化后字段赋值问题。
- 未发现本轮新增代码需要继续自动修复的问题。

## 2026-07-05 第 9 轮：收敛 runtime engine 中已追清链路的小型生产坏味道

### 问题探索

- 基线验证中，`cargo test` 继续通过，222 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 失败 26 项，另有 1 个 warning。
- 本轮优先选定 `src/runtime/engine.rs` 中能完整追清执行流程且不需要改变架构边界的问题，暂不处理 `enter_nested_call` 与 `emit_skill_lifecycle_event` 的参数过多问题，因为这两项需要单独抽取上下文结构并验证嵌套调用或生命周期事件边界。
- Windows DLL 目录路径链路已追清：宿主提供的 FFI/native library 目录会被转换为 Windows 宽字符串并传给系统 DLL 目录 API；此处仅需在追加末尾 NUL 之前拒绝路径内部嵌入 NUL。
- Vulcan 文件上下文链路已追清：`capture_vulcan_file_context` 从 `vulcan.context.skill_dir`、`entry_dir`、`entry_file` 捕获快照；`LuaNestedCallScopeGuard` 进入嵌套 `vulcan.call` 前保存该快照，恢复时用 `skill_dir` 与 `entry_file` 调回 `populate_vulcan_file_context`，由该函数重新派生 `entry_dir`。
- ROOT 声明解析链路已追清：`resolve_root_declared_skill_instance` 先在运行时根链中找到标签为 `ROOT` 的单个 root，再调用 `resolve_declared_skill_instance_from_roots`；该解析函数签名明确接收 `&[RuntimeSkillRoot]`，因此无需克隆单个 root 构造临时数组。
- Lua package 搜索链链路已追清：`setup_package_paths` 只在宿主传入且实际存在 `lua_packages_dir` 时，把项目内 `share/lua` 与 `lib/lua` 模式前置到 Lua 原有 `package.path` 与 `package.cpath`，不覆盖原有搜索链。
- Lua 平台信息链路已追清：`vulcan.os.info()` 创建 Lua 表并暴露当前宿主 `os` 与 `arch`；`os` 原 match 分支对 windows/linux/macos 返回的就是 `std::env::consts::OS` 本身，属于无意义重复映射。

### 执行调整

- 将 `windows_wide_null_path` 中的 `wide_path.iter().any(|value| *value == 0)` 调整为 `wide_path.contains(&0)`，保持相同的嵌入 NUL 拒绝语义。
- 新增内部类型别名 `VulcanFileContextSnapshot`，用双语文档明确该三元组代表 `(skill_dir, entry_dir, entry_file)`，并让捕获函数与嵌套调用守卫字段使用该类型表达真实归属。
- 将 ROOT 单根声明解析从 `&[root.clone()]` 调整为 `std::slice::from_ref(root)`，避免无意义克隆，同时保持只解析 ROOT 单根的行为。
- 移除 `package.cpath` 与 `package.path` 拼接中的多余 `.to_string()`，直接把 `mlua::String::to_str()` 的显示值放入 `format!`。
- 将 `vulcan.os.info()` 的 `current_os` 从无意义 match 改为直接使用 `std::env::consts::OS`，保留 `arch` 的规范化映射。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test runtime::engine::tests::vulcan_call_restores_outer_context_after_nested_failure -- --nocapture` 通过，1 个相关测试通过。
- 修改后：`cargo test execute_runlua_request_inline -- --nocapture` 通过，32 个相关测试通过。
- 修改后：`cargo test` 通过，222 个测试全部通过。
- 修改后：`cargo clippy --all-targets -- -D warnings` 仍失败，但失败数从 26 降至 20；本轮处理的 `manual_contains`、`type_complexity`、`cloned_ref_to_slice_refs`、`to_string_in_format_args` 与 `needless_match` 均不再出现。
- 修改范围审核：`git diff -- src/runtime/engine.rs` 显示该文件累计 diff 中仍包含上一轮未提交的 `RunLuaRuntimeContext` 调整；本轮实际新增修改集中在 Windows 宽路径校验、Vulcan 文件上下文快照类型别名、ROOT 单根解析借用、package 搜索链拼接与 `vulcan.os.info()`。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变 Lua 可见 API、错误文本、宿主配置字段、ROOT 优先级规则、package 搜索链前置策略或 `vulcan.context` 字段语义。
- 本轮新增类型别名带有中英文双语文档；其余改动均为既有表达式的等价收敛，未新增函数、公开类型或协议字段。
- 仍遗留 20 个 clippy 问题：`src/host/database.rs` 与 `src/runtime/engine.rs` 的参数过多问题需要后续结构化重构；`src/host/options.rs` 存在测试模块位置问题；`src/ffi/tests.rs` 与 `src/runtime/engine/tests.rs` 仍有大小写比较、Default 初始化后字段赋值和少量 needless borrow 问题。
- 未发现本轮新增代码需要继续自动修复的问题。

## 2026-07-05 第 10 轮：抽取嵌套 vulcan.call 目标上下文以消除参数团

### 问题探索

- 基线验证中，`cargo test` 继续通过，222 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 失败 20 项，另有 1 个 warning。
- 本轮选定 `src/runtime/engine.rs` 中 `LuaNestedCallScopeGuard::enter_nested_call` 的 10 参数问题，因为它位于真实的 `vulcan.call` 嵌套调用生产路径，而不是单纯测试写法。
- 已追清执行流程：`populate_vulcan_call_for_lua` 注册 Lua 侧 `vulcan.call`；每次调用先在 `dispatch_entries` 中按 canonical 工具名找到目标入口，再从全局 Lua 函数表取出目标模块函数。
- `dispatch_entries` 的字段来源已确认：它由当前 `entry_registry` 与 `skills_map` 构造，包含目标 canonical 名称、Lua 模块名、所属 skill id、局部入口名、运行时根名、所属 skill 目录和入口文件绝对路径。
- 嵌套调用继承上下文链路已确认：dispatcher 在进入目标前从 `LuaNestedCallScopeGuard` 保存的外层 `vulcan.context.request`、`client_budget` 与 `tool_config` 转回 JSON，并构造新的 `LuaInvocationContext` 传给嵌套 skill。
- 数据库绑定链路已确认：dispatcher 根据目标所属 skill id 分别从 `LanceDbSkillHost` 与 `SqliteSkillHost` 查询 binding，进入嵌套调用时写入 `vulcan` 上下文；恢复时则根据进入前保存的外层 skill 名称重新查回原 binding。
- `enter_nested_call` 的原参数并非彼此独立：入口元数据、继承调用上下文和两个数据库 binding 都描述同一个已解析嵌套目标，因此长期上应作为一个明确的上下文对象传递。

### 执行调整

- 新增内部结构 `LuaNestedCallTarget<'a>`，用双语文档描述一次 `vulcan.call` 进入前所需的已解析目标上下文。
- 将目标 canonical 名称、所属 skill id、局部入口名、运行时根名、skill 目录、入口文件路径、继承调用上下文、LanceDB binding 与 SQLite binding 收敛为 `LuaNestedCallTarget` 字段。
- 将 `LuaNestedCallScopeGuard::enter_nested_call` 的签名从 10 个参数调整为 `target: LuaNestedCallTarget<'_>`，并补齐参数与返回值双语文档。
- 在 `enter_nested_call` 内部统一通过 `target` 填充 `vulcan.context`、`vulcan.runtime.internal`、`vulcan.deps`、LanceDB 上下文与 SQLite 上下文。
- 将 dispatcher 调用点改为构造 `LuaNestedCallTarget` 结构体字面量，保留原本的目标解析、luaexec 重入保护、binding 查询、函数调用与恢复逻辑。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test runtime::engine::tests::vulcan_call_restores_outer_context_after_nested_failure -- --nocapture` 通过，1 个相关测试通过。
- 修改后：`cargo test vulcan_call -- --nocapture` 通过，1 个相关测试通过。
- 修改后：`cargo test execute_runlua_request_inline -- --nocapture` 通过，32 个相关测试通过。
- 修改后：`cargo test` 通过，222 个测试全部通过。
- 修改后：`cargo clippy --all-targets -- -D warnings` 仍失败，但失败数从 20 降至 19；`LuaNestedCallScopeGuard::enter_nested_call` 的 `too_many_arguments` 告警已消失。
- 修改范围审核：`git diff -- src/runtime/engine.rs` 是累计 diff，包含前几轮未提交的 engine 调整；本轮新增部分集中在 `LuaNestedCallTarget` 定义、`enter_nested_call` 签名与字段访问，以及 dispatcher 的结构体字面量调用。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变 `vulcan.call` 的 Lua API、目标选择规则、luaexec 重入保护、请求上下文继承、数据库 binding 查询、错误文本或嵌套调用后的恢复策略。
- `LuaNestedCallTarget` 的存在让“目标入口元数据 + 继承调用上下文 + 目标数据库绑定”的归属关系更明确，避免后续继续向 `enter_nested_call` 追加分散参数。
- 仍遗留 19 个 clippy 问题：`src/host/database.rs` 与 `src/runtime/engine.rs` 仍各有一个参数过多问题；`src/host/options.rs` 存在测试模块位置问题；测试代码中仍有大小写比较、Default 初始化后字段赋值和少量 needless borrow 问题。
- 未发现本轮新增代码需要继续自动修复的问题。

## 2026-07-05 第 11 轮：抽取技能生命周期事件草稿以消除宿主回调参数团

### 问题探索

- 基线验证中，`cargo test` 继续通过，222 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 失败 19 项，另有 1 个 warning。
- 本轮选定 `src/runtime/engine.rs` 中 `LuaEngine::emit_skill_lifecycle_event` 的 8 参数问题，因为它位于技能安装、更新、卸载、启用、停用后的宿主生命周期事件桥接路径。
- 已追清宿主事件类型：`RuntimeSkillLifecycleEvent` 定义在 `src/host/callbacks.rs`，字段包含 `plane`、`action`、`skill_id`、`root_name`、`skill_dir`、`status` 与 `message`，并由 `set_skill_lifecycle_callback` 注册的进程级回调消费。
- 已追清发射链路：`LuaEngine::emit_skill_lifecycle_event` 只负责把运行时内部字段转换为 `RuntimeSkillLifecycleEvent` 并调用 `crate::host::callbacks::emit_skill_lifecycle_event`，后者在存在宿主回调时同步转发事件。
- 已追清状态变更路径：`mutate_skill_state_and_reload` 在 guard 阻塞时发出 `blocked`，在 disable/enable/uninstall 直接动作失败时发出 `failed`，在动作成功并 reload 后发出 `completed`。
- 已追清显式卸载路径：`uninstall_skill_and_reload_in_root` 在 guard 阻塞、prepare 失败、reload 失败、commit 失败时发出对应 `blocked` 或 `failed`，在依赖与数据库清理汇总后发出 `completed` 并携带结果 message。
- 已追清 install/update 路径：`apply_skill_request_in_root` 先通过 progress emitter 发出细粒度进度，最终解析变更后的目标实例并发出一条生命周期完成事件；该事件的 root 和目录来自 `resolve_declared_skill_instance_from_roots` 的结果。
- 原函数的参数并非独立临时值，而是同一条宿主可见生命周期事件载荷，因此长期上应以事件草稿对象传递，避免后续继续扩散参数列表。

### 执行调整

- 新增内部结构 `SkillLifecycleEventDraft<'a>`，用双语文档描述运行时内部组装的宿主可见生命周期事件草稿。
- 将 `plane`、`action`、`skill_id`、`root_name`、`skill_dir`、`status` 与 `message` 收敛为 `SkillLifecycleEventDraft` 字段。
- 将 `LuaEngine::emit_skill_lifecycle_event` 签名从 8 个参数调整为 `event: SkillLifecycleEventDraft<'_>`，并补齐参数与返回值双语文档。
- 将状态变更、显式卸载、install/update 三条路径中的所有 `emit_skill_lifecycle_event` 调用点改为构造 `SkillLifecycleEventDraft` 结构体字面量。
- 保留原有 `RuntimeSkillLifecycleEvent` 对外结构、状态字符串、错误 message、root_name 与 skill_dir 来源、以及宿主回调调用方式不变。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test lifecycle -- --nocapture` 通过，1 个相关测试通过。
- 修改后：`cargo test uninstall -- --nocapture` 通过，3 个相关测试通过。
- 修改后：`cargo test install_skill -- --nocapture` 通过，4 个相关测试通过。
- 修改后：`cargo test skill_manager -- --nocapture` 通过，1 个相关测试通过。
- 修改后：`cargo test` 通过，222 个测试全部通过。
- 修改后：`cargo clippy --all-targets -- -D warnings` 仍失败，但失败数从 19 降至 18；`LuaEngine::emit_skill_lifecycle_event` 的 `too_many_arguments` 告警已消失。
- 修改范围审核：`git diff -- src/runtime/engine.rs` 是累计 diff，包含前几轮未提交的 engine 调整；本轮新增部分集中在 `SkillLifecycleEventDraft` 定义、生命周期事件调用点结构体字面量，以及 `emit_skill_lifecycle_event` 签名和字段转换。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变技能生命周期对外 API、宿主回调类型、事件字段、状态值、错误文本、progress emitter 行为、技能管理权限规则或 reload/rollback 流程。
- `SkillLifecycleEventDraft` 让宿主可见事件载荷在调用点显式成型，后续新增 operation id、source 信息或审计字段时可以扩展同一事件对象，而不是继续扩大函数参数列表。
- 仍遗留 18 个 clippy 问题：`src/host/database.rs` 仍有一个生产构造函数参数过多问题；`src/host/options.rs` 存在测试模块位置问题；测试代码中仍有大小写比较、Default 初始化后字段赋值和少量 needless borrow 问题。
- 未发现本轮新增代码需要继续自动修复的问题。
## 2026-07-05 第 12 轮：抽取数据库绑定上下文规格以消除生产构造参数团

### 问题探索

- 基线验证中，`cargo test` 持续通过，222 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 失败 18 项，并额外报告 1 个 warning。
- 本轮优先选定 `src/host/database.rs` 中 `RuntimeDatabaseBindingContext::new` 的 `too_many_arguments`，因为它是剩余 Clippy 中最后一个生产代码结构问题，位于 SQLite/LanceDB provider 与宿主/controller 数据库绑定协议之间。
- 已追清真实构造链路：`src/providers/lancedb.rs` 与 `src/providers/sqlite.rs` 分别在 `LanceDbSkillHost` / `SqliteSkillHost` 中解析 `skill_dir_name`、`skills_root`、`sidecar_root` 与默认数据库路径，再构造 `RuntimeDatabaseBindingContext`。
- 已确认 `space_root` 的实际来源不是物理 skill root，而是由 `skills_root.parent().unwrap_or(skills_root).join(database_dir_name)` 得到的运行时数据库 sidecar 根目录；该字段随后被 controller 用作宿主空间根。
- 已追清消费链路：binding context 写入 provider binding，暴露到 status JSON，克隆进 provider request，经 `ffi_standard` 转为 C ABI，并在 `host/controller.rs` 中参与 `attach_binding`、`controller_space_id_for_binding` 与 `controller_binding_id_for_binding`。
- 已确认 `RuntimeDatabaseBindingContext::new` 的真实调用点只有 LanceDB provider、SQLite provider 和 `src/host/database.rs` 内部测试 helper，不存在隐藏的第三条生产构造路径。

### 执行调整

- 新增 `RuntimeDatabaseBindingContextSpec`，用双语文档集中表达一次 skill 级数据库绑定上下文构造所需的完整规格字段。
- 将 `RuntimeDatabaseBindingContext::new` 从 8 个分散参数调整为接收单一 `RuntimeDatabaseBindingContextSpec`，并在函数内部解构规格对象后继续生成相同的 `binding_tag`。
- 修正 `RuntimeDatabaseBindingContext::space_root` 的双语文档，把原先误导性的“物理技能根目录”说明改为“运行时数据库 sidecar 根目录”。
- 更新 LanceDB 与 SQLite provider 的构造点，显式构造 `RuntimeDatabaseBindingContextSpec`，保留原有字段来源、默认数据库路径和 provider kind。
- 更新数据库测试 helper 的构造方式，并在 `src/lib.rs` 中同步 re-export `RuntimeDatabaseBindingContextSpec`，保持公开 Rust API 的上下文类型可达性。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test captured_callback_snapshots_stay_engine_scoped -- --nocapture` 通过，1 个相关测试通过。
- 修改后：`cargo test database -- --nocapture` 通过，2 个相关测试通过。
- 修改后：`cargo test sqlite -- --nocapture` 通过，1 个相关测试通过。
- 修改后：`cargo test` 通过，222 个测试全部通过。
- 修改后：`cargo clippy --all-targets -- -D warnings` 仍失败，但失败数从 18 降至 17，warning 从 1 降至 0；生产代码中的 `RuntimeDatabaseBindingContext::new` 参数过多告警已消除。
- 修改范围审核：`git diff -- src/host/database.rs src/providers/lancedb.rs src/providers/sqlite.rs src/lib.rs` 显示 4 个文件累计 91 行新增、60 行删除，变更集中在规格结构、构造调用点、文档修正和 re-export。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变数据库绑定的字段值来源、`binding_tag` 格式、SQLite/LanceDB 默认路径解析、provider request/status JSON 载荷、FFI 转换字段或 controller 绑定语义。
- `RuntimeDatabaseBindingContextSpec` 让“space、skill、物理目录、数据库类型、默认路径”这组强关联构造输入在类型层面成组，避免后续继续向构造函数追加分散参数。
- 剩余 17 个 Clippy 问题均为既有测试写法或测试模块布局问题：`src/ffi/tests.rs` 与 `src/runtime/engine/tests.rs` 的大小写比较，`src/host/options.rs` 的测试模块后置项，`src/runtime/engine/tests.rs` 的 Default 后字段赋值和少量 needless borrow。
- 本轮新增代码已按中英文双语注释要求补充类型、字段和构造函数文档，未发现新增代码需要继续自动修复的问题。

## 2026-07-05 第 13 轮：收敛测试宿主选项构造并清空剩余 Clippy 告警

### 问题探索

- 基线验证中，`cargo test` 持续通过，222 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 失败 17 项，0 个 warning。
- 本轮优先选定 `src/runtime/engine/tests.rs` 中的 `LuaRuntimeHostOptions::default()` 后字段赋值问题，因为它覆盖多条运行时测试链路，且重复写法会让测试依赖的宿主配置散落在多行可变状态中。
- 已追清执行链路：这些测试都在调用 `LuaEngine::new` 或 `make_runtime_test_engine_with_host_options` 前完成 host options 赋值；`LuaEngine::new` 会先对 host options 执行 `normalized()`，再解析默认文本编码、runlua 池配置、skill config 路径，并把结果封装进 engine。
- 已确认字段归属：`resources_dir`、`lua_packages_dir`、`host_provided_lua_root` 属于打包运行时资源与 Lua 包搜索路径输入；`capabilities` 控制技能管理桥与 managed IO 兼容；`runlua_pool_config` 控制隔离 runlua VM 池；`skill_config_file_path` 控制统一技能配置路径；`ignored_skill_ids` 控制资源准备前跳过指定技能；`default_text_encoding` 和 `system_lua_lib_dir` 分别服务文本编码与系统 Lua 库租约测试。
- 在修改后代码审核中，Clippy 暴露同一测试文件内仍有 ROOT 过滤比较与路径读取多余借用；同时剩余全局告警还包含 FFI 委托查询测试的 ROOT 比较，以及 `src/host/options.rs` 中测试模块位于生产 impl 之前的问题。
- 已追清这些附加项：ROOT 比较均用于验证委托权限视角不可见 ROOT 元数据；`fs::read_to_string(target_dir.join(...))` 的临时路径只用于立即读取；`host/options.rs` 的测试模块移动不会改变 `LuaInvocationContext` 和 `normalize_context_object` 的生产调用链。

### 执行调整

- 将多处 `let mut host_options = LuaRuntimeHostOptions::default(); host_options.xxx = ...` 改为 `LuaRuntimeHostOptions { ... , ..Default::default() }` 结构体字面量，并在 `LuaEngineOptions` 或测试 helper 调用中直接传入。
- 为测试中的能力开关显式构造 `LuaRuntimeCapabilityOptions`，让 `enable_skill_management_bridge` 与 `enable_managed_io_compat` 的测试意图直接体现在创建 engine 的参数中。
- 将 runtime 与 FFI 委托查询测试中的 `trim().to_ascii_uppercase() != "ROOT"` 改为 `!trim().eq_ignore_ascii_case("ROOT")`，避免额外分配并贴合“大小写不敏感比较”的真实意图。
- 移除 `fs::read_to_string(&target_dir.join(...))` 中对临时 `PathBuf` 的多余借用，保持路径读取断言语义不变。
- 将 `src/host/options.rs` 的 `#[cfg(test)] mod tests` 移动到文件末尾，保持测试内容原样，消除测试模块之后仍有生产项的布局问题。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test packaged_runtime -- --nocapture` 通过，3 个相关测试通过。
- 修改后：`cargo test runtime_skills -- --nocapture` 通过，3 个相关测试通过。
- 修改后：`cargo test runlua_pool -- --nocapture` 通过，2 个相关测试通过。
- 修改后：`cargo test skill_config -- --nocapture` 通过，21 个相关测试通过。
- 修改后：`cargo test load_from_roots_skips_host_ignored_skill_before_resource_setup -- --nocapture` 通过，1 个相关测试通过。
- 修改后：`cargo test host_default_text_encoding -- --nocapture` 通过，1 个相关测试通过。
- 修改后：`cargo test managed_io_compat -- --nocapture` 通过，6 个相关测试通过。
- 修改后：`cargo test system_runtime_lease_preserves_explicit_cwd_override -- --nocapture` 通过，1 个相关测试通过。
- 自动修复后：`cargo test ffi_query_json_filters_root_for_delegated_authority -- --nocapture` 通过，1 个相关测试通过。
- 自动修复后：`cargo test delegated_authority_query_helpers_hide_root_skills -- --nocapture` 通过，1 个相关测试通过。
- 自动修复后：`cargo test execute_runlua_request_inline_supports_vulcan_fs_copy_directory_tree_with_overwrite_control -- --nocapture` 通过，1 个相关测试通过。
- 自动修复后：`cargo test capability_options_require_explicit_managed_io_compat_flag -- --nocapture` 通过，1 个相关测试通过。
- 自动修复后：`cargo test runtime_root_expands_fixed_layout -- --nocapture` 通过，1 个相关测试通过。
- 全量验证：`cargo test` 通过，222 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变任何测试覆盖的运行时行为，仅把测试输入从“先默认构造再可变赋值”收敛为一次性构造，使每个测试依赖的宿主配置在创建 engine 的调用点显式可见。
- ROOT 过滤比较仍然执行 trim 后的 ASCII 大小写不敏感判断，继续验证委托权限不可见 ROOT 元数据，同时保留 system 权限可见 ROOT 的断言。
- `host/options.rs` 只调整测试模块位置，`LuaInvocationContext::new`、`LuaInvocationContext::empty` 与 `normalize_context_object` 的生产实现和文档保持不变。
- 当前 `cargo clippy --all-targets -- -D warnings` 已无告警；后续循环可以转向更深层的结构性坏味道，而不再受现有 Clippy 基线噪声干扰。

## 2026-07-05 第 14 轮：抽取运行时测试引擎单 VM 构造夹具

### 问题探索

- 基线验证中，`cargo test` 通过，222 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- Clippy 清零后，本轮从结构重复入手，发现 `src/runtime/engine/tests.rs` 中多处测试直接调用 `LuaEngine::new(LuaEngineOptions { ... })`，并反复写入相同的主 VM 池配置 `min_size: 1`、`max_size: 1`、`idle_ttl_secs: 60`。
- 已追清执行链路：这些构造点均进入 `LuaEngine::new`，后者会对 host options 执行 `normalized()`，解析默认文本编码、runlua 池配置、skill config 路径，随后用传入的主 VM 池配置创建 `pool`。
- 已确认字段归属：这些重复构造点的差异主要是 host options，例如技能管理桥开关、runlua 独立池覆盖、显式 skill config 路径、忽略技能列表、默认文本编码和系统 Lua 库目录；主 VM 池本身在普通测试中只是固定测试夹具。
- 已确认例外：schema export 测试使用 `LuaEngineOptions::new(LuaVmPoolConfig { idle_ttl_secs: 30, ... }, ...)`，这是有意自定义主 VM 池 TTL 的用例，不应被普通测试池 helper 吞掉。
- 本轮目标因此不是隐藏 host options，而是把“普通测试主 VM 池”集中表达，让每个测试调用点只保留真正不同的宿主选项和自己的失败上下文。

### 执行调整

- 新增 `runtime_test_single_vm_pool_config()`，集中返回普通运行时引擎测试共享的单 VM 池配置。
- 新增 `runtime_test_engine_options(host_options)`，将显式 host options 与普通单 VM 测试池组合为 `LuaEngineOptions`。
- 新增 `try_make_runtime_test_engine_with_host_options(host_options)`，统一执行普通测试引擎创建，同时允许调用点继续保留自定义 `.expect(...)` 文本。
- 调整 `make_runtime_test_engine_with_host_options`，改为复用新的 fallible helper。
- 将技能管理桥、runlua 池覆盖、skill config、reload、ambiguous runtime root、忽略技能、nested context 恢复等固定池测试构造点统一改为 `try_make_runtime_test_engine_with_host_options(...)`。
- 保留 schema export 测试中的 `LuaEngineOptions::new` 和自定义 TTL=30 主 VM 池配置，避免把真实测试意图误收敛为普通夹具。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test runtime_skills -- --nocapture` 通过，3 个相关测试通过。
- 修改后：`cargo test runlua_pool -- --nocapture` 通过，2 个相关测试通过。
- 修改后：`cargo test skill_config -- --nocapture` 通过，21 个相关测试通过。
- 修改后：`cargo test load_from_roots_skips_host_ignored_skill_before_resource_setup -- --nocapture` 通过，1 个相关测试通过。
- 修改后：`cargo test vulcan_call_restores_outer_context_after_nested_failure -- --nocapture` 通过，1 个相关测试通过。
- 修改后：`cargo test input_schema -- --nocapture` 通过，6 个相关测试通过，确认自定义 TTL=30 的 schema export 路径仍可运行。
- 全量验证：`cargo test` 通过，222 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 范围审核：`rg` 确认普通测试构造均经由 `try_make_runtime_test_engine_with_host_options`，仅 schema export 用例保留 `LuaEngine::new(LuaEngineOptions::new(...))`。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变 `LuaEngine::new`、`LuaEngineOptions`、host options 归一化、主 VM 池语义或任何测试断言，只收敛测试夹具构造方式。
- 调用点仍显式写出各自差异化的 host options，避免把技能管理桥、skill config、ignored skill 等不同测试意图塞进模糊的万能 builder。
- 新增 helper 均带有中英文双语说明，且只位于测试模块中，不影响生产 API。
- 当前 `cargo test` 与 `cargo clippy --all-targets -- -D warnings` 均通过；后续循环可继续从非 Clippy 型重复、过长测试夹具或生产路径中的结构耦合继续挖。

## 2026-07-05 第 15 轮：收敛宿主回调注册表锁定与克隆逻辑

### 问题探索

- 基线验证中，`cargo test` 通过，222 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮选定 `src/host/callbacks.rs`，因为生产代码中多个进程级 callback registry 都重复手写了“获取 registry、加锁、替换 callback”以及“加锁、克隆 Option、释放锁后调用 callback”的流程。
- 已追清 setter 链路：`set_skill_lifecycle_callback`、`set_skill_operation_progress_callback`、`set_entry_registry_callback`、`set_skill_management_callback`、`set_host_tool_callback`、`set_model_embed_callback`、`set_model_llm_callback` 都写入对应的 `OnceLock<Mutex<Option<...>>>` 进程级注册表。
- 已追清发射链路：`RuntimeEngine` 在技能生命周期、entry registry 变化、进度事件中调用 `emit_*`；这些函数必须先从 registry 中 clone 出 `Arc` 回调，再释放锁并调用宿主代码，否则宿主回调重入时会有死锁风险。
- 已追清分发链路：Lua 侧 `vulcan.runtime.skills.*`、`vulcan.host.*` 和 `vulcan.models.*` 最终分别进入 skill-management、host-tool、model embed/llm dispatch 函数；这些函数也都需要先 clone 回调，再在锁外执行宿主逻辑。
- 已确认 `try_has_*` 链路：Lua 的能力探测只需要判断当前 registry 是否存在回调，不需要执行回调，但仍复用了同一份“锁定并读取 Option”的注册表访问模式。

### 执行调整

- 新增 `set_callback_registry_value`，集中处理进程级 callback registry 的写入，保留 setter 在 registry 锁中毒时 panic 的既有行为，并补充参数与行为说明。
- 新增 `clone_callback_registry_value`，集中处理进程级 callback registry 的读取与 clone，返回 `Result<Option<T>, String>`，让调用方可以在执行宿主代码前释放锁。
- 将 7 个 `set_*_callback` 函数统一改为调用 `set_callback_registry_value`，保留原有公开函数签名和回调类型。
- 将 lifecycle、progress、entry registry 三个 emit 函数改为调用 `clone_callback_registry_value`，仍然在锁外执行 `callback(event)`。
- 将 skill-management、host-tool、model embed、model llm dispatch 函数改为复用 `clone_callback_registry_value`，保留缺失回调时的既有错误语义。
- 将 `try_has_skill_management_callback`、`try_has_host_tool_callback`、`try_has_model_embed_callback`、`try_has_model_llm_callback` 改为复用同一读取 helper，消除重复 lock/is_some 代码。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test progress_emitter_reports_sequence_and_percent -- --nocapture` 通过，1 个相关测试通过。
- 修改后：`cargo test vulcan_host_bridge -- --nocapture` 通过，2 个相关测试通过。
- 修改后：`cargo test runtime_skills_bridge -- --nocapture` 通过，1 个相关测试通过。
- 修改后：`cargo test vulcan_models -- --nocapture` 通过，5 个相关测试通过。
- 修改后：`cargo test model_json_callback_setters_round_trip_response_and_provider_error -- --nocapture` 通过，1 个相关测试通过。
- 全量验证：`cargo test` 通过，222 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 范围审核：`rg` 确认 `src/host/callbacks.rs` 内 setter、emit、dispatch、has 判断均复用新的 registry helper；回调调用仍发生在 helper 返回之后。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变任何公开 callback 类型、setter 函数签名、Lua 可见 API、错误文本主干或测试断言。
- 关键不变量保持不变：所有 host callback 仍然先从 registry 中 clone 出来，再在 mutex 锁外执行，避免宿主回调重入时阻塞注册表。
- setter 锁中毒仍保持 panic 行为，只是由重复的 `lock().unwrap()` 变为带 registry 名称的集中 panic 信息；dispatch/has 路径仍返回结构化错误。
- 当前 `cargo test` 与 `cargo clippy --all-targets -- -D warnings` 均通过；后续循环可继续处理 `src/host/database.rs` 中类似的 provider callback registry 重复，或转向 runtime pool 锁语义优化。

## 2026-07-05 第 16 轮：收敛数据库 provider 回调注册表访问逻辑

### 问题探索

- 基线验证中，`cargo test` 通过，222 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮选定 `src/host/database.rs`，因为数据库 provider 回调注册表存在与宿主回调类似的重复代码：四个 setter 分别手写获取 registry、加锁、替换 callback；引擎快照捕获路径又通过 `take_optional_callback` 读取四个 registry。
- 已追清注册链路：`src/ffi_standard.rs` 中 SQLite/LanceDB 的 standard 与 JSON FFI setter 会分别调用 `set_sqlite_provider_callback`、`set_lancedb_provider_callback`、`set_sqlite_provider_json_callback`、`set_lancedb_provider_json_callback`，最终写入四个进程级 `OnceLock<Mutex<Option<...>>>` 注册表。
- 已追清捕获链路：`LuaEngine::new` 调用 `RuntimeDatabaseProviderCallbacks::capture_process_defaults()`，将当前进程级 provider callback 克隆为引擎私有快照，并通过 `Arc<RuntimeDatabaseProviderCallbacks>` 传入 SQLite 与 LanceDB provider。
- 已追清分发链路：SQLite provider 调用 `dispatch_sqlite_provider_request`，LanceDB provider 调用 `dispatch_lancedb_provider_request`；standard 模式直接执行结构化回调，JSON 模式先序列化请求再解析响应。
- 已确认测试恢复链路：`ProcessCallbackRestoreGuard` 在测试前捕获当前快照，测试结束时通过四个公开 setter 恢复进程级默认值，因此 setter helper 会被现有测试覆盖。

### 执行调整

- 新增 `set_database_provider_callback_registry_value`，集中处理数据库 provider callback registry 写入，保留 setter 在 registry 锁中毒时 panic 的行为，并让 panic 信息带上具体 provider 名称。
- 新增 `clone_database_provider_callback_registry_value`，集中处理数据库 provider callback registry 的锁定与克隆，返回 `Result<Option<T>, String>` 供引擎创建时捕获快照。
- 将四个公开 setter 统一改为调用写入 helper，保留函数签名、回调类型和注册表归属不变。
- 将 `RuntimeDatabaseProviderCallbacks::capture_process_defaults()` 中四个快照字段统一改为调用克隆 helper，删除旧的 `take_optional_callback`。
- 保留 provider 分发语义：回调依然从引擎私有快照中 clone 后执行，不在全局 registry 锁内调用宿主代码。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test captured_callback_snapshots_stay_engine_scoped -- --nocapture` 通过，1 个关键快照隔离测试通过。
- 修改后：`cargo test database -- --nocapture` 通过，2 个数据库相关测试通过。
- 修改后：`cargo test provider_callback -- --nocapture` 通过，1 个 provider callback 过滤测试通过。
- 全量验证：`cargo test` 通过，222 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 范围审核：`rg` 确认旧的 `take_optional_callback` 已无残留，四个 setter 与四个快照字段均复用新的 registry helper。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变任何公开 FFI setter、Rust setter、provider callback 类型、Lua provider 行为或测试断言。
- 本轮没有引入多来源字段兼容、候选路径轮询或可选结构兜底；四类 provider callback 的 registry 归属均由现有源码链路确认。
- 锁作用域保持清晰：写入 helper 只在替换全局默认值时持锁，克隆 helper 只在捕获 `Arc` 回调时持锁，provider 请求分发仍在锁外执行宿主逻辑。
- 修改部分代码审核未发现需要继续自动修复的问题；当前 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续检查 provider 构建路径中的模式选择与错误信息重复，或转向 `src/providers/sqlite.rs` / `src/providers/lancedb.rs` 中的请求构造重复逻辑。

## 2026-07-05 第 17 轮：收敛数据库 provider 宿主初始化模式决策

### 问题探索

- 基线验证中，`cargo test` 通过，222 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮沿第 16 轮遗留方向继续检查 provider 构建路径，发现 `SqliteSkillHost::new` 与 `LanceDbSkillHost::new` 都在同一个函数里混写三类职责：动态库路径校验与加载、host callback 是否注册校验、space-controller 桥接创建。
- 已追清入口链路：`LuaEngine::load_skill` 根据 skill metadata 的 `effective_sqlite()` / `effective_lancedb()` 判断是否需要数据库能力；首次启用时分别调用 `SqliteSkillHost::new` 或 `LanceDbSkillHost::new` 创建宿主级 provider host。
- 已追清初始化链路：动态库模式需要读取对应 library path 并加载 provider API；host-callback 模式只要求引擎创建时捕获到的 `RuntimeDatabaseProviderCallbacks` 存在指定传输模式的回调；space-controller 模式只创建 `LuaRuntimeSpaceControllerBridge`，具体 binding 在 `register_skill` 时 attach。
- 已追清绑定链路：`register_skill` 才负责按 skill 计算 sidecar 路径、创建 `RuntimeDatabaseBindingContext`、打开动态库句柄或向 controller 启用 binding；本轮不跨进该路径，避免把全局资源初始化和单 skill binding 生命周期混在一起。
- 已确认重复点：SQLite 与 LanceDB 文件中各有一份 `callback_mode_name`，并分别拼接相同结构的缺失回调错误文本，后续新增 callback transport 时容易漏改。

### 执行调整

- 在 `src/host/database.rs` 新增 `database_callback_mode_name`，集中返回数据库 callback transport 的稳定显示标签。
- 在 `src/host/database.rs` 新增 `require_database_provider_callback_registration`，集中生成 host-callback 模式缺失回调时的启动错误，并补充一条单元测试锁住 standard/json 两类错误文本。
- 在 `src/providers/sqlite.rs` 新增 `resolve_sqlite_skill_host_api`，专门解析 SQLite 动态库模式或校验 host-callback 模式；新增 `resolve_sqlite_skill_host_controller`，专门解析 SQLite space-controller 桥接。
- 在 `src/providers/lancedb.rs` 新增 `resolve_lancedb_skill_host_api`，专门解析 LanceDB 动态库模式或校验 host-callback 模式；新增 `resolve_lancedb_skill_host_controller`，专门解析 LanceDB space-controller 桥接。
- 将 `SqliteSkillHost::new` 与 `LanceDbSkillHost::new` 收敛为调用资源解析 helper 后组装 host 状态，删除两个 provider 文件中重复的 `callback_mode_name`。
- 顺手修正两个 `new` 的注释：它们不再被描述为总是“立即加载动态库”，而是根据 provider 模式解析所需资源。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test callback_registration_requirement_reports_transport_mode -- --nocapture` 通过，1 个新增错误语义测试通过。
- 修改后：`cargo test provider_callback -- --nocapture` 通过，1 个 provider callback 过滤测试通过。
- 修改后：`cargo test database -- --nocapture` 通过，3 个数据库相关测试通过。
- 修改后：`cargo test runtime_skills -- --nocapture` 通过，3 个运行时技能加载相关测试通过。
- 修改后：`cargo test skill_config -- --nocapture` 通过，21 个配置相关测试通过。
- 修改后：`cargo test captured_callback_snapshots_stay_engine_scoped -- --nocapture` 通过，1 个快照隔离测试通过。
- 全量验证：`cargo test` 通过，223 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 范围审核：`rg` 确认两个 provider 文件中的旧 `callback_mode_name` 已删除，SQLite/LanceDB 初始化均改为调用各自资源解析 helper，并共享 `require_database_provider_callback_registration`。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变公开 FFI/Rust API、host options 字段、provider 模式枚举、动态库加载条件、space-controller 创建条件或请求分发行为。
- 本轮没有引入跨 provider 的模糊泛型资源容器；SQLite 与 LanceDB 的动态库类型仍保持各自明确解析，公共逻辑只集中到 callback transport 命名与缺失回调校验。
- `register_skill` 中的单 skill 路径绑定、动态库句柄创建、controller attach 仍保持原语义，本轮只处理宿主级初始化模式决策。
- 修改部分代码审核未发现需要继续自动修复的问题；当前 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续检查 `register_skill` 内 sidecar 路径、binding context、controller enable 请求构造的重复结构，或深入 SQLite/LanceDB 请求分发方法中的 controller/host/dynamic 三分支重复。

## 2026-07-05 第 18 轮：收敛单 skill 数据库绑定路径计划

### 问题探索

- 基线验证中，`cargo test` 通过，223 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮沿第 17 轮遗留方向继续检查 `register_skill`，发现 `SqliteSkillHost::register_skill` 与 `LanceDbSkillHost::register_skill` 都重复执行 skill 目录名解析、skills 根目录解析、运行时数据库 sidecar 根目录计算、provider 存储路径拼接、`RuntimeDatabaseBindingContext` 构造。
- 已追清入口链路：`LuaEngine::load_skill` 在 skill metadata 中发现 SQLite/LanceDB 启用后，会调用对应 host 的 `register_skill(root_name, skill_id, dir, meta)`；其中 `dir` 是当前生效 skill 实例的物理目录。
- 已追清路径归属：两个 provider 都通过 `skill_dir.parent()` 得到 skills 根，再通过 `skills_root.parent().unwrap_or(skills_root)` 得到运行时根，最后拼接 `host_options.database_dir_name` 得到共享数据库 sidecar 根。
- 已追清 provider 差异：SQLite 的 provider 存储目录是 `<database_root>/sqlite/<skill>`，默认数据库文件是 `<skill>.sqlite3`；LanceDB 的 provider 存储目录是 `<database_root>/lancedb/<skill>`，默认数据库路径就是该目录本身。
- 已确认动态库/controller 生命周期：动态库句柄创建、controller attach、controller enable 请求仍由各 provider 的 `register_skill` 分支负责，本轮只收敛进入这些分支之前的绑定路径计划。
- 额外发现：两个动态库分支的目录创建错误信息都把同一个 `error` 打印了两次，例如 `...: error: error`，属于诊断噪声。

### 执行调整

- 在 `RuntimeDatabaseKind` 上新增 provider sidecar 目录名与默认数据库路径规则，集中表达 SQLite 与 LanceDB 的真实差异。
- 新增 `RuntimeDatabaseBindingPlan`，统一携带 `skill_dir_name`、provider 存储目录、默认数据库路径和 `RuntimeDatabaseBindingContext`。
- 新增 `build_runtime_database_binding_plan`，集中解析单 skill 数据库绑定所需的 sidecar 路径与上下文，保留原有无效 skill 目录和 skill 根错误语义。
- 将 SQLite `register_skill` 改为消费 `build_runtime_database_binding_plan` 的结果，删除本地重复的 sidecar 根与 `RuntimeDatabaseBindingContextSpec` 构造。
- 将 LanceDB `register_skill` 改为消费同一个绑定计划结果，删除本地重复路径解析代码。
- 修正 SQLite/LanceDB 动态库目录创建错误信息，避免重复打印同一个底层错误。
- 新增 `database_binding_plan_resolves_provider_paths` 单元测试，明确覆盖 SQLite 文件型默认路径和 LanceDB 目录型默认路径。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test database_binding_plan_resolves_provider_paths -- --nocapture` 通过，1 个新增路径规划测试通过。
- 修改后：`cargo test database -- --nocapture` 通过，4 个数据库相关测试通过。
- 修改后：`cargo test runtime_skills -- --nocapture` 通过，3 个运行时技能加载相关测试通过。
- 修改后：`cargo test skill_config -- --nocapture` 通过，21 个配置相关测试通过。
- 修改后：`cargo test provider_callback -- --nocapture` 通过，1 个 provider callback 过滤测试通过。
- 修改后：`cargo test captured_callback_snapshots_stay_engine_scoped -- --nocapture` 通过，1 个快照隔离测试通过。
- 全量验证：`cargo test` 通过，224 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 范围审核：`rg` 确认 `sidecar_root` 只保留在 `build_runtime_database_binding_plan` 内，两个 provider 文件改为调用共享绑定计划函数。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变公开 API、host options 字段、provider 模式枚举、动态库句柄生命周期、controller attach/enable 行为或请求分发行为。
- 本轮没有引入候选路径轮询或兼容式 fallback；SQLite 与 LanceDB 的默认路径差异由 `RuntimeDatabaseKind` 的明确分支表达，并由新增测试覆盖。
- 修改部分代码审核发现并修复了测试中重复书写运行时根路径字面量的问题，避免测试自身形成新的路径规则漂移点。
- 当前 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续检查 `register_skill` 中 controller enable 请求构造重复，或深入 SQLite/LanceDB 各操作方法中的 controller/host/dynamic 三分支重复。

## 2026-07-05 第 19 轮：收敛 space-controller 绑定启用前置步骤

### 问题探索

- 基线验证中，`cargo test` 通过，224 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮沿第 18 轮遗留方向继续检查 `register_skill` 中的 controller enable 分支，发现 SQLite 与 LanceDB 都重复执行相同的前置步骤：克隆 controller、计算 controller space id、计算 client-scoped binding id、复制数据库路径、`attach_binding`，然后才构造各自的 enable 请求。
- 已追清 controller bridge 职责：`LuaRuntimeSpaceControllerBridge::new` 负责创建 controller client、连接控制器并解析本客户端 session scope；`attach_binding` 负责把 `RuntimeDatabaseBindingContext` 注册成 controller space；`controller_binding_id_for_binding` 负责把稳定 binding tag 与当前 session scope 合成为 controller binding id。
- 已追清 provider 分支职责：SQLite 的 enable 请求需要 `space_id`、`binding_id`、`db_path` 和 `enforce_db_file_lock=false`；LanceDB 的 enable 请求需要 `space_id`、`binding_id` 和 `default_db_path`。请求体字段仍应保留在各 provider 中，不适合抽成模糊的跨 provider 请求构造器。
- 已确认执行顺序：两个 provider 都应先 attach space，再执行 enable 请求；如果 attach 失败，应在进入 enable 请求前返回错误。
- 本轮目标因此不是合并 SQLite/LanceDB enable 请求，而是把“attach 并解析 controller 标识”这个共同前置动作放回 controller bridge 内部。

### 执行调整

- 在 `src/host/controller.rs` 新增 `LuaRuntimeSpaceControllerBindingIds`，明确承载 controller `space_id` 与 client-scoped `binding_id`。
- 在 `LuaRuntimeSpaceControllerBridge` 上新增 `attach_binding_with_ids`，集中完成 controller space id 解析、binding id 解析和 `attach_binding`，并返回 enable 请求所需的两个标识。
- 将 SQLite `register_skill` 的 space-controller 分支改为调用 `attach_binding_with_ids`，保留 SQLite 自己构造 `ControllerSqliteEnableRequest` 的职责。
- 将 LanceDB `register_skill` 的 space-controller 分支改为调用 `attach_binding_with_ids`，保留 LanceDB 自己构造 `ControllerLanceDbEnableRequest` 的职责。
- 保留现有 `controller_space_id_for_binding` 与 `controller_binding_id_for_binding`，供运行期绑定状态和诊断路径继续使用。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test database -- --nocapture` 通过，4 个数据库相关测试通过。
- 修改后：`cargo test runtime_skills -- --nocapture` 通过，3 个运行时技能加载相关测试通过。
- 修改后：`cargo test skill_config -- --nocapture` 通过，21 个配置相关测试通过。
- 修改后：`cargo test provider_callback -- --nocapture` 通过，1 个 provider callback 过滤测试通过。
- 全量验证：`cargo test` 通过，224 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 范围审核：`rg` 确认两个 provider 文件中不再直接对 `binding_context` 手写 `attach_binding`、`controller_space_id_for_binding` 与 `controller_binding_id_for_binding` 组合，而是统一调用 `attach_binding_with_ids`。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变公开 FFI/Rust API、host options 字段、controller client 连接行为、controller enable 请求字段或 provider 请求分发行为。
- 本轮没有把 SQLite 与 LanceDB 的 enable 请求合并成泛化结构；二者仍保留各自清晰的请求类型与 provider 专属字段。
- `attach_binding_with_ids` 保持 attach 失败即返回错误的原有语义，并把 enable 所需 id 的解析集中到 controller bridge 里，减少后续 session scope 或 binding id 规则变化时的漏改点。
- 修改部分代码审核未发现需要继续自动修复的问题；当前 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续深入 SQLite/LanceDB 运行期操作方法中的 controller/host/dynamic 三分支重复，尤其是 SQLite 多个方法中反复获取 bridge、space id、binding id、序列化请求、执行 controller call 的结构。

## 2026-07-05 第 20 轮：收敛运行期 controller 调用上下文

### 问题探索

- 基线验证中，`cargo test` 通过，224 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮沿第 19 轮遗留方向继续检查 SQLite/LanceDB 运行期操作方法，发现大量 space-controller 分支重复执行 `controller_bridge()`、`controller_space_id()`、`controller_binding_id()` 三件套，再把这两个 id 传入 controller SDK 调用。
- 已追清 id 来源：运行期 controller 调用所需的 `space_id` 与 `binding_id` 都只来自当前 binding 的 `provider_binding` 和当前 controller bridge 的 session scope，没有其它字段来源。
- 已追清注册期与运行期差异：注册期需要先 `attach_binding` 再 enable；运行期操作只需要复用同一个 binding context 派生 controller ids，不应再次 attach。
- 已追清 SQLite 例外：`query_stream_wait_metrics`、`query_stream_chunk`、`query_stream_close` 三个 controller 调用只依赖 stream id，不需要数据库 binding id，因此不应强行套用 controller binding ids helper。
- 本轮目标因此是把“运行期 controller bridge + ids”解析收敛为绑定对象内部 helper，而不合并具体 SQL、FTS、LanceDB 操作请求。

### 执行调整

- 在 `LuaRuntimeSpaceControllerBridge` 上新增 `binding_ids_for_binding`，集中基于 `RuntimeDatabaseBindingContext` 和当前 bridge session scope 解析 `space_id` 与 `binding_id`。
- 将既有 `attach_binding_with_ids` 改为复用 `binding_ids_for_binding`，让注册期 enable 与运行期操作共用同一套 id 规则。
- 在 `LanceDbSkillBinding` 中新增 `controller_call_context`，一次性返回 controller bridge 与 `LuaRuntimeSpaceControllerBindingIds`。
- 将 LanceDB 的 `create_table`、`vector_upsert`、`vector_search`、`delete`、`drop_table` 五个 space-controller 分支改为使用 `controller_call_context`，删除本地 `controller_space_id` 与 `controller_binding_id` helper。
- 在 `SqliteSkillBinding` 中新增同名 `controller_call_context`，一次性返回 controller bridge 与 `LuaRuntimeSpaceControllerBindingIds`。
- 将 SQLite 中需要数据库 binding id 的 controller 分支改为使用 `controller_call_context`：脚本执行、批量执行、JSON 查询、query stream 打开、分词、自定义词、FTS index、FTS 文档与 FTS 搜索。
- 保留 SQLite 的 QueryStream metrics/chunk/close 三个纯 stream-id controller 调用直接使用 `controller_bridge()`，避免为不需要 binding id 的调用制造伪依赖。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test database -- --nocapture` 通过，4 个数据库相关测试通过。
- 修改后：`cargo test runtime_skills -- --nocapture` 通过，3 个运行时技能加载相关测试通过。
- 修改后：`cargo test skill_config -- --nocapture` 通过，21 个配置相关测试通过。
- 修改后：`cargo test provider_callback -- --nocapture` 通过，1 个 provider callback 过滤测试通过。
- 全量验证：`cargo test` 通过，224 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 范围审核：`rg` 确认 provider 文件中的旧 `controller_space_id()` 与 `controller_binding_id()` helper 已删除，运行期需要 binding id 的 controller 调用均改为 `controller_call_context`。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变公开 FFI/Rust API、host options 字段、controller SDK 调用方法、请求字段、日志字段或 provider 模式分支顺序。
- 本轮没有合并 SQLite 与 LanceDB 的具体操作请求；各 provider 仍保留自己的 controller SDK 方法、输入解析和响应组装，只共享 controller ids 解析规则。
- 对不需要 binding id 的 SQLite QueryStream 辅助操作没有做强行抽象，保持其仅依赖 stream id 的真实语义。
- 修改部分代码审核未发现需要继续自动修复的问题；当前 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续检查 SQLite space-controller 分支中“日志开始、started_at、controller run、log_if_slow、JSON 响应组装”的重复，或转向动态库分支中的 FFI 错误处理重复。

## 2026-07-05 第 21 轮：收敛 LanceDB controller 操作日志与计时外壳

### 问题探索

- 基线验证中，`cargo test` 通过，224 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮沿第 20 轮遗留方向继续检查运行期 controller 分支，发现 LanceDB 的多个 space-controller 操作在获取 controller 调用上下文后，仍重复写普通日志、`Instant::now()`、执行 SDK 调用、慢日志输出这一整套外壳。
- 已追清重复块的真实链路：`create_table`、`delete`、`drop_table` 都是无额外慢日志字段的 controller 调用；`vector_upsert` 需要在普通日志和慢日志中输出相同的 `payload_bytes`；这些操作都可以共享同一个“日志 + 计时 + controller ids + invoke”外壳。
- 已确认不纳入本轮的例外：`vector_search` 的慢日志字段依赖 controller 返回的 `result.data.len()`，不是调用前就能确定的静态 extra，因此保留原状更符合真实语义。
- 已确认响应结构边界：各操作返回的 JSON 字段仍由各方法自己组装，不能因为外壳重复就合并成模糊的跨操作响应构造器。

### 执行调整

- 在 `LanceDbSkillBinding` 中新增 `run_controller_binding_operation`，统一处理普通日志、开始计时、`controller_call_context` 获取、provider 专属 controller SDK 调用和慢日志输出。
- 将 `create_table_json` 的 space-controller 分支改为使用该 helper，保留 `create_lancedb_table` 调用和 `{ "message": ... }` 响应组装。
- 将 `vector_upsert_json` 的 space-controller 分支改为使用该 helper，并通过 `slow_extra` 保持 `payload_bytes` 普通日志与慢日志一致。
- 将 `delete_json` 的 space-controller 分支改为使用该 helper，保留删除响应中的 `message`、`version`、`deleted_rows` 字段。
- 将 `drop_table_json` 的 space-controller 分支改为使用该 helper，保留 `table_name` 解析和响应组装。
- 保留 `vector_search_json` 的原有显式日志流程，因为它的慢日志需要 controller 返回后的结果字节数。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test database -- --nocapture` 通过，4 个数据库相关测试通过。
- 修改后：`cargo test runtime_skills -- --nocapture` 通过，3 个运行时技能加载相关测试通过。
- 修改后：`cargo test skill_config -- --nocapture` 通过，21 个配置相关测试通过。
- 修改后：`cargo test provider_callback -- --nocapture` 通过，1 个 provider callback 过滤测试通过。
- 全量验证：`cargo test` 通过，224 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 范围审核：确认 LanceDB 的 `create_table`、`vector_upsert`、`delete`、`drop_table` space-controller 分支已通过 `run_controller_binding_operation` 收敛；`vector_search` 因结果相关慢日志保留显式流程。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变公开 API、controller SDK 方法、LanceDB 请求字段、响应 JSON 字段、日志字段含义或 provider 模式分支顺序。
- 本轮没有合并不同 LanceDB 操作的响应结构；helper 只负责外壳流程，具体请求与返回仍由各操作方法明确表达。
- `run_controller_binding_operation` 的 `slow_extra` 同时用于普通日志和慢日志，保持 `vector_upsert` 既有日志语义不变。
- 修改部分代码审核未发现需要继续自动修复的问题；当前 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续检查 SQLite space-controller 分支中的同类日志/计时外壳，或转向动态库分支中的 FFI 错误处理重复。

## 2026-07-05 第 22 轮：收敛 SQLite controller 固定日志计时外壳

### 问题探索

- 基线验证中，`cargo test` 通过，224 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮沿第 21 轮遗留方向继续检查 SQLite space-controller 分支，发现多个操作重复编写普通日志、`Instant::now()`、`controller_call_context()`、controller SDK 调用、慢日志输出这一套固定外壳。
- 已追清 controller id 来源：这些操作的 `space_id` 与 `binding_id` 都来自 `SqliteSkillBinding::controller_call_context()`，该 helper 又通过当前 bridge 与 `provider_binding` 派生，不存在其它候选字段。
- 已按慢日志信息来源区分三类分支：固定 extra 或无 extra 的操作可以统一外壳；`query_json`、`query_stream`、`list_custom_words`、`search_fts` 的慢日志依赖返回值，必须保留显式流程；QueryStream metrics/chunk/close 只依赖 stream id，不需要 binding ids。
- 本轮目标因此限定为收敛 SQLite controller 分支中“调用前即可确定慢日志 extra”的外壳重复，不合并响应 JSON 组装，也不碰 host-managed 与动态库 FFI 分支。

### 执行调整

- 在 `SqliteSkillBinding` 中新增 `run_controller_binding_operation`，统一处理普通日志、开始计时、controller bridge 与 binding ids 获取、provider 专属 controller SDK 调用和慢日志输出。
- 将 `execute_script`、`execute_batch` 的 space-controller 分支改为使用该 helper，保留 SQL 参数解析、controller typed 调用和响应 JSON 字段。
- 将 `tokenize_text_json` 的 space-controller 分支改为使用该 helper，并保留 `tokenizer_mode` 与 `search_mode` 的普通日志 extra。
- 将 `upsert_custom_word_json`、`remove_custom_word_json` 的 space-controller 分支改为使用该 helper，并保留 `word` 日志 extra。
- 将 `ensure_fts_index_json`、`rebuild_fts_index_json` 的 space-controller 分支改为使用该 helper，并保留 `index_name` 日志 extra。
- 将 `upsert_fts_document_json`、`delete_fts_document_json` 的 space-controller 分支改为使用该 helper，并保留 `index_name` 与 `id` 日志 extra。
- 保留 `query_json`、`query_stream`、`list_custom_words_json`、`search_fts_json` 的显式日志流程，因为它们的慢日志字段分别依赖返回后的行数、stream 状态、词数或命中数。
- 保留 QueryStream wait metrics、chunk、close 三个 controller 分支直接使用 `controller_bridge()`，避免为纯 stream-id 操作制造虚假的数据库 binding id 依赖。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test database -- --nocapture` 通过，4 个数据库相关测试通过。
- 修改后：`cargo test runtime_skills -- --nocapture` 通过，3 个运行时技能加载相关测试通过。
- 修改后：`cargo test skill_config -- --nocapture` 通过，21 个配置相关测试通过。
- 修改后：`cargo test provider_callback -- --nocapture` 通过，1 个 provider callback 过滤测试通过。
- 全量验证：`cargo test` 通过，224 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 范围审核：`rg` 确认 SQLite 的固定 extra controller 分支已调用 `run_controller_binding_operation`，返回值相关慢日志分支仍保留显式 `log_info`、`controller_call_context` 与 `log_if_slow`。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变公开 API、controller SDK 方法、SQLite 请求字段、响应 JSON 字段、日志字段含义或 provider 模式分支顺序。
- helper 只承接外壳流程，不负责解释 SQL、FTS、分词、自定义词等业务响应，避免把不同操作揉成模糊抽象。
- 本轮没有引入候选式 binding id 或多路径兜底，controller 调用仍只使用已确认的 `controller_call_context()` 来源。
- 修改部分代码审核未发现需要继续自动修复的问题；当前 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续检查 SQLite 返回值相关 controller 分支是否存在可安全收敛的局部结构，或转向动态库 FFI 分支中的重复错误处理。

## 2026-07-05 第 23 轮：收敛 SQLite 动态库空 handle 错误处理并补齐结果句柄清理

### 问题探索

- 基线验证中，`cargo test` 通过，224 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮沿第 22 轮遗留方向检查 SQLite 动态库 FFI 分支，发现多个 handle 创建失败路径重复执行 `drop(guard)`、`api.take_last_error_message()`、`self.log_warning(...)`、`return Err(error)`。
- 已追清重复路径只属于动态库模式：`execute_script`、`execute_batch`、`query_json`、`query_stream`、`tokenize_text_json`、`list_custom_words_json`、`search_fts_json` 都先持有 `SkillHandleState` 锁，再调用 native 函数返回 handle，空 handle 代表需要读取 native last-error。
- 已追清不纳入本轮的边界：`SqliteSkillHost::register_skill` 的 runtime/database 打开失败没有持有同一个 handle 锁，也没有既有 warning 语义；`ensure_status` 处理的是状态码错误而非空结果 handle。
- 修改部分代码审核时进一步发现，`query_json` 和 `tokenize_text_json` 在非空结果 handle 后仍会通过 `take_owned_string(...) -> Result` 或 JSON 解析错误提前返回，原生结果句柄销毁可能被跳过。
- 该清理问题有源码依据：`LoadedSqliteApi::take_owned_string` 明确在空指针时返回 `Err`，`query_json` 又显式处理 invalid JSON，因此不能假设这些路径永远不会失败。

### 执行调整

- 在 `SqliteSkillBinding` 中新增 `take_ffi_null_handle_error`，统一消费已持有的 `SkillHandleState` 锁、释放锁、读取 native last-error、写 warning，并把错误字符串返回给调用方。
- 将 `execute_script`、`execute_batch`、`query_json`、`query_stream`、`tokenize_text_json`、`list_custom_words_json`、`search_fts_json` 的动态库空 handle 分支改为调用该 helper。
- 调整 `query_json` 的动态库成功分支：先保存 `query_json_result_json_data` 的提取结果，再销毁 `query_json_result_handle` 并释放锁，随后才处理字符串错误和 JSON 解析。
- 调整 `tokenize_text_json` 的动态库成功分支：用一个可失败载荷闭包集中提取 `normalized_text`、`fts_query` 与 tokens，随后无论提取是否成功都先销毁 `tokenize_result_handle` 并释放锁，再继续返回结果或错误。
- 保留每个操作的 native 调用、请求参数、响应 JSON 字段、慢日志字段与原有错误返回内容，不把不同 handle 类型合并成泛化资源管理器。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test database -- --nocapture` 通过，4 个数据库相关测试通过。
- 修改后：`cargo test provider_callback -- --nocapture` 通过，1 个 provider callback 过滤测试通过。
- 全量验证：`cargo test` 通过，224 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 范围审核：`rg` 确认 7 个动态库空 handle 分支均调用 `take_ffi_null_handle_error`，直接操作名级别的 `log_warning("...")` 重复点已消失。
- 清理审核：`rg` 与局部读取确认 `query_json_result_destroy` 和 `tokenize_result_destroy` 都移动到错误传播前执行，避免字符串提取或 JSON 解析提前返回时泄漏 native 结果句柄。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变公开 API、FFI 符号、SQLite 请求字段、响应 JSON 字段、provider 模式分支顺序或 native last-error 内容。
- `take_ffi_null_handle_error` 明确消费锁，保持“读取 native last-error 前先释放 handle 锁”的既有顺序，不引入额外 fallback 或候选路径。
- `query_json` 与 `tokenize_text_json` 的修复只调整 native 结果句柄销毁时机；成功路径返回值与慢日志字段保持一致，失败路径继续返回原始提取错误或 JSON 解析错误。
- 修改部分代码审核未发现需要继续自动修复的问题；当前 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续检查 LanceDB 动态库分支是否存在类似 native 响应释放一致性问题，或继续拆解 `src/runtime/engine.rs` 中的大型执行流程。

## 2026-07-05 第 24 轮：收紧 LanceDB 动态库响应释放与锁持有范围

### 问题探索

- 基线验证中，`cargo test` 通过，224 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮沿第 23 轮遗留方向检查 LanceDB 动态库分支，重点追踪 native 字符串响应、out byte buffer、`SkillHandleState` 锁与 JSON 解析之间的执行顺序。
- 已确认 `LoadedLanceDbApi::take_owned_string` 在非空响应时会复制字符串并调用 `string_free`，因此 `vector_upsert_json` 与 `call_json_string` 的字符串响应本身不会因 JSON 解析失败泄漏。
- 已确认 `LoadedLanceDbApi::take_owned_bytes` 负责复制并调用 `bytes_free` 释放 `VldbLancedbByteBuffer`，而 `vector_search_json` 的动态库分支原先会先解析 meta JSON，再调用 `take_owned_bytes(buffer)`。
- 已追清 `vector_search_json` 的返回 bytes 会被 Lua 层作为 `data_json` 或 `data` 继续消费，因此该 buffer 是真实结果载荷，不是可忽略的临时字段。
- 问题边界由此确定：`vector_search_json` 在 response 文本有效但 meta JSON 解析失败时，可能跳过 out byte buffer 释放；并且多个动态库成功路径在 native 响应已经复制后仍持有 engine 锁执行 Rust JSON 解析。

### 执行调整

- 调整 `vector_upsert_json` 动态库成功路径：`take_owned_string` 复制并释放 native 字符串后立即释放 `SkillHandleState` 锁，再执行 Rust 侧 JSON 解析。
- 调整 `call_json_string`：通用 JSON 字符串响应路径在 native 字符串复制完成后立即释放 engine 锁，再解析响应 JSON，使 `create_table`、`delete`、`drop_table` 共享更短的锁持有范围。
- 调整 `vector_search_json` 成功路径：在解析 meta JSON 前先通过 `take_owned_bytes(buffer)` 复制并释放 native out byte buffer，然后释放 engine 锁，再执行 JSON 解析。
- 调整 `vector_search_json` response 为空的错误路径：在写 warning 与返回 native last-error 前，先尝试释放可能已经由 native 写入的 out byte buffer。
- 为锁提前释放和 out-buffer 提前释放补充双语设计注释，明确这些顺序是为了避免 Rust 解析失败扩大 native 资源占用或造成泄漏。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test database -- --nocapture` 通过，4 个数据库相关测试通过。
- 修改后：`cargo test provider_callback -- --nocapture` 通过，1 个 provider callback 过滤测试通过。
- 全量验证：`cargo test` 通过，224 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 范围审核：`rg` 确认 `vector_upsert_json`、`vector_search_json` 与 `call_json_string` 的 `drop(guard)` 已移动到 Rust JSON 解析前，`vector_search_json` 成功与错误路径均有 `take_owned_bytes(buffer)` 释放点。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变公开 API、FFI 符号、LanceDB 请求字段、响应 JSON 字段、controller/host-provider 分支或慢日志字段。
- `vector_search_json` 的成功路径仍返回同一个 meta JSON 与 bytes，只是先把 native buffer 复制释放，再解析 meta；解析失败时 bytes 的 Rust `Vec` 会随错误路径自动释放。
- response 为空的错误路径继续返回 `take_owned_string` 读取到的 native last-error，额外的 `take_owned_bytes(buffer)` 只负责释放可能存在的 out-buffer，不改变错误内容。
- 修改部分代码审核未发现需要继续自动修复的问题；当前 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续检查 `src/runtime/engine.rs` 中 Lua context 注入的大型函数，或继续审视 provider 动态库分支里其它锁范围与资源释放顺序。

## 2026-07-05 第 25 轮：收敛 runtime provider Lua 输入与结果转换壳

### 问题探索

- 基线验证中，`cargo test` 通过，224 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮沿第 24 轮遗留方向进入 `src/runtime/engine.rs` 的 Lua context 注入层，重点检查 `populate_vulcan_lancedb_context` 与 `populate_vulcan_sqlite_context`。
- 已追清 provider Lua 方法的共同执行链路：Lua 代理函数接收 `input`，通过 `require_table_arg(..., "input")` 校验 table，再用 `lua_value_to_json` 转为 `serde_json::Value`，调用对应 binding 方法，最后将 `Result<Value, String>` 转成 Lua 值或 runtime error。
- 已确认该共同链路在 LanceDB 的 `create_table`、`delete`、`drop_table` 与 SQLite 的大多数 JSON 方法中重复出现，差异只在 API 名称和具体 binding 方法。
- 已确认不应抽取的边界：LanceDB `vector_upsert` 还需要拆分 `rows/data` 并设置 `input_format`，`vector_search` 还需要设置 `output_format` 并处理 bytes；因此本轮只复用输入 table 转 JSON，保留它们的业务逻辑。
- 已确认禁用分支、`info/status` 分支和无输入的 `list_custom_words` 分支语义不同，不强行合并成一个过宽注册器。

### 执行调整

- 在 runtime JSON/Lua 转换工具区新增 `provider_input_table_to_json`，统一处理 provider Lua `input` table 参数校验与 JSON 转换，并保留原有 API 名称进入错误消息。
- 新增 `provider_json_result_to_lua`，统一将 provider binding 的 `Result<Value, String>` 映射为 Lua 返回值，保留 provider 错误作为 `mlua::Error::runtime`，保留 JSON 到 Lua 转换错误作为 external error。
- 将 LanceDB 的 `create_table`、`delete`、`drop_table` Lua 代理改为使用两个 helper。
- 将 LanceDB 的 `vector_upsert`、`vector_search` Lua 代理改为使用 `provider_input_table_to_json`，但保留 payload、format、bytes 处理逻辑。
- 将 SQLite 的 `tokenize_text`、`execute_script`、`execute_batch`、`query_json`、`query_stream`、QueryStream 辅助方法、自定义词方法、FTS index、FTS document 与 `search_fts` Lua 代理改为复用 helper。
- 将 `list_custom_words` 的无输入结果转换改为复用 `provider_json_result_to_lua`，不伪造 input table。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test runtime_skills -- --nocapture` 通过，3 个运行时技能加载相关测试通过。
- 修改后：`cargo test database -- --nocapture` 通过，4 个数据库相关测试通过。
- 修改后：`cargo test provider_callback -- --nocapture` 通过，1 个 provider callback 过滤测试通过。
- 全量验证：`cargo test` 通过，224 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 范围审核：`rg` 确认 provider 注入层不再直接出现 `require_table_arg(input, "sqlite...")` 或 `require_table_arg(input, "lancedb...")`，统一入口已迁移到 `provider_input_table_to_json`。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变公开 API、Lua 暴露函数名、provider binding 方法、provider 请求字段、返回 JSON 字段或禁用分支错误文本。
- `provider_input_table_to_json` 只收敛输入 table 校验与 JSON 转换，不负责解释任何 provider 业务字段，因此没有引入模糊字段兼容或候选路径。
- `provider_json_result_to_lua` 保留原有错误分层：binding 返回错误仍是 runtime error，JSON 到 Lua 转换失败仍是 external error。
- 修改部分代码审核未发现需要继续自动修复的问题；当前 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续抽取 provider Lua 方法注册的 `create_function`/`table.set` 重复壳，或继续拆分 `populate_vulcan_sqlite_context` 中大量方法注册逻辑。

## 2026-07-05 第 26 轮：收敛 provider Lua JSON 方法注册壳

### 问题探索

- 基线验证中，`cargo test` 通过，224 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮沿第 25 轮遗留方向继续检查 `src/runtime/engine.rs` 中 `populate_vulcan_lancedb_context` 与 `populate_vulcan_sqlite_context` 的方法注册段。
- 已追清简单 provider JSON 方法的共同注册链路：先 `lua.create_function` 创建代理函数，代理内部执行 `provider_input_table_to_json` 与 provider binding 调用，再通过 `provider_json_result_to_lua` 返回 Lua 值，最后 `provider_table.set(method_name, fn)` 安装到 `vulcan.<provider>` 表。
- 已确认错误文本模式同构：创建失败统一为 `Failed to create vulcan.<provider>.<method>: ...`，安装失败统一为 `Failed to set vulcan.<provider>.<method>: ...`。
- 已确认不纳入本轮的边界：LanceDB `vector_upsert` 需要拆分 `rows/data` 与编码 payload，`vector_search` 需要处理 `output_format` 与 bytes；SQLite `list_custom_words` 无输入参数；`info/status` 与禁用代理分支也不是 input table -> provider JSON result 的同一形状。

### 执行调整

- 新增 `register_provider_json_method`，统一创建有 `input` table 参数的 provider JSON 代理函数，并统一安装到 provider table。
- helper 内部继续调用 `provider_input_table_to_json` 与 `provider_json_result_to_lua`，不重新实现输入转换或结果转换逻辑。
- 将 LanceDB 的 `create_table`、`delete`、`drop_table` 改为通过 `register_provider_json_method` 注册。
- 将 SQLite 的 `tokenize_text`、`execute_script`、`execute_batch`、`query_json`、`query_stream`、QueryStream 辅助方法、自定义词方法、FTS index、FTS document 与 `search_fts` 改为通过 `register_provider_json_method` 注册。
- 保留 LanceDB `vector_upsert`、`vector_search`、SQLite `list_custom_words`、`info/status`、禁用代理分支的显式注册，避免为不同调用形状制造错误抽象。
- 根据 `mlua::Lua::create_function` 的实际编译约束，为 helper 的 `invoke` 泛型补充 `Send` 约束，满足 `MaybeSend + 'static` 要求。

### 验证记录

- 修改后首次定向验证暴露编译错误：`mlua::Lua::create_function` 要求闭包实现 `MaybeSend`，helper 泛型 `F` 缺少 `Send` 约束。
- 自动修复后：`cargo fmt` 通过。
- 自动修复后：`cargo test runtime_skills -- --nocapture` 通过，3 个运行时技能加载相关测试通过。
- 自动修复后：`cargo test database -- --nocapture` 通过，4 个数据库相关测试通过。
- 自动修复后：`cargo test provider_callback -- --nocapture` 通过，1 个 provider callback 过滤测试通过。
- 全量验证：`cargo test` 通过，224 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 范围审核：`rg` 确认简单 SQLite 方法与 LanceDB 简单方法已集中调用 `register_provider_json_method`；剩余显式 `Failed to create/set vulcan.<provider>.<method>` 位于 `info/status`、禁用分支和 LanceDB 特殊 payload/bytes 方法。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变 Lua 暴露函数名、provider binding 方法、请求 JSON 字段、返回 JSON 字段、禁用分支错误文本或特殊 LanceDB payload/bytes 处理。
- `register_provider_json_method` 只抽取 create/set 注册外壳与统一错误文本，实际 provider 调用仍由调用点传入闭包明确表达。
- `Send` 约束来自 `mlua::create_function` 的当前 API 要求，不是为了兼容未知线程模型添加的投机兜底。
- 修改部分代码审核未发现需要继续自动修复的问题；当前 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续收敛无输入 JSON 方法注册、`info/status` 注册、禁用代理注册，或进一步拆分 provider context 注入函数。

## 2026-07-05 第 27 轮：收敛 provider 禁用上下文安装流程

### 问题探索

- 基线验证中，`cargo test` 通过，224 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮沿第 26 轮遗留方向继续检查 `populate_vulcan_lancedb_context` 与 `populate_vulcan_sqlite_context` 的禁用分支。
- 已追清禁用上下文的执行来源：`reset_pooled_vm_request_scope`、runlua/session 执行、嵌套调用恢复等路径会在没有当前 skill binding 时调用 `populate_vulcan_lancedb_context(lua, None, None)` 与 `populate_vulcan_sqlite_context(lua, None, None)`。
- 已追清禁用分支行为：provider table 会设置 `enabled=false`，`status/info` 返回 provider 专属 disabled JSON，所有操作方法安装为 runtime error 代理。
- 已确认 LanceDB 与 SQLite 的禁用分支结构完全同构，差异只在 provider 名称、disabled status JSON、disabled error 文案和方法列表。
- 已确认错误文本需要保留：`Failed to create disabled vulcan.<provider>.status/info`、`Failed to set vulcan.<provider>.status`、`Failed to set disabled vulcan.<provider>.info`、`Failed to create disabled vulcan.<provider> proxy` 和 `Failed to set disabled method <method>` 都是现有错误表面。

### 执行调整

- 新增 `install_disabled_provider_context`，统一安装 provider 禁用状态、`status`、`info` 和禁用代理方法。
- 将 LanceDB 禁用分支改为调用该 helper，并保留原有 `create_table`、`vector_upsert`、`vector_search`、`delete`、`drop_table` 禁用代理方法列表。
- 将 SQLite 禁用分支改为调用该 helper，并保留原有 16 个 SQLite 禁用代理方法列表。
- helper 继续使用 provider 专属的 `disabled_skill_status_json` 结果，不合并 LanceDB 与 SQLite 的 disabled JSON 构造逻辑。
- helper 保留原有 create/set 错误文本形态，避免禁用状态下的诊断信息漂移。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test runtime_skills -- --nocapture` 通过，3 个运行时技能加载相关测试通过。
- 修改后：`cargo test database -- --nocapture` 通过，4 个数据库相关测试通过。
- 全量验证：`cargo test` 通过，224 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 范围审核：`rg` 确认禁用错误文本集中在 `install_disabled_provider_context` 内，LanceDB 与 SQLite 禁用分支只保留 provider 名称、disabled status、disabled error 和方法列表。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变 Lua 暴露函数名、禁用状态 JSON 字段、禁用代理方法列表、禁用 runtime error 文案或 provider binding 解析逻辑。
- `install_disabled_provider_context` 只抽取无 binding 时的 table 安装流程，不参与启用状态下的真实 provider 调用。
- LanceDB 与 SQLite 的 disabled status JSON 仍由各 provider 自己生成，避免把 provider 专属状态字段揉成跨 provider 模糊结构。
- 修改部分代码审核未发现需要继续自动修复的问题；当前 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续收敛启用状态下的 `info/status` 注册，或把 provider context 注入拆成更小的注册单元。

## 2026-07-05 第 28 轮：收敛 provider 无输入 JSON 方法注册壳

### 问题探索

- 基线验证中，`cargo test` 通过，224 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮沿第 27 轮遗留方向继续检查 `populate_vulcan_lancedb_context`、`populate_vulcan_sqlite_context` 与 `install_disabled_provider_context` 中的 `info/status` 注册逻辑。
- 已追清启用分支行为：LanceDB 与 SQLite 都会在 binding 存在时安装 `info()` 与 `status()` 两个无输入 Lua 方法，它们分别调用 binding 的 `info_json()` 与 `status_json()` 并转为 Lua 值。
- 已追清禁用分支行为：`install_disabled_provider_context` 会安装无输入的 disabled `status()` 与 `info()`，二者都返回 provider 专属 disabled JSON。
- 已确认错误文本边界：启用分支使用 `Failed to create/set vulcan.<provider>.<method>`；禁用 `status` 的 set 错误仍是 `Failed to set vulcan.<provider>.status`，禁用 `info` 的 set 错误是 `Failed to set disabled vulcan.<provider>.info`，需要分别保留。
- 本轮目标因此限定为抽取“无输入 JSON 值 -> Lua 方法 -> table.set”的注册外壳，不合并 provider 状态 JSON 的生成来源。

### 执行调整

- 新增 `register_provider_json_noarg_method`，统一创建无输入 Lua 函数、调用 JSON 值生产闭包、将 JSON 转为 Lua 值，并安装到 provider table。
- helper 支持独立传入 create/set 错误主体文本，用于保留启用与禁用分支现有错误文本差异。
- 将 LanceDB 启用分支的 `info`、`status` 注册改为调用该 helper。
- 将 SQLite 启用分支的 `info`、`status` 注册改为调用该 helper。
- 将 `install_disabled_provider_context` 内部的 disabled `status`、`info` 注册改为调用该 helper，并保留原先不完全一致的 set 错误文本。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test runtime_skills -- --nocapture` 通过，3 个运行时技能加载相关测试通过。
- 修改后：`cargo test database -- --nocapture` 通过，4 个数据库相关测试通过。
- 全量验证：`cargo test` 通过，224 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 范围审核：`rg` 确认启用态 `info/status` 的显式 create/set 错误文本已收敛到 `register_provider_json_noarg_method` 调用，禁用态只剩 disabled proxy 错误文本保持显式。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变 Lua 暴露函数名、`info/status` 返回 JSON、禁用状态 JSON、禁用代理方法列表或 provider binding 解析逻辑。
- `register_provider_json_noarg_method` 只抽取无输入 JSON 方法注册外壳，真实 JSON 内容仍由各调用点闭包明确提供。
- 禁用分支的 `status/info` set 错误文本差异通过显式参数保留，没有为了统一外观改动现有诊断表面。
- 修改部分代码审核未发现需要继续自动修复的问题；当前 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续把 provider context 注入拆成更小的“启用方法注册组”，或转向其它大型 runtime 函数。

## 2026-07-05 第 29 轮：收敛 provider context 外层表安装流程

### 问题探索

- 基线验证中，`cargo test` 通过，224 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮沿第 28 轮遗留方向继续检查 `src/runtime/engine.rs` 中 `populate_vulcan_lancedb_context` 与 `populate_vulcan_sqlite_context` 的外层安装流程。
- 已追清两个 provider context 的共同入口：普通 skill 调用、help 渲染、runlua、runtime session lease、嵌套调用进入与恢复都会调用这两个 populate 函数。
- 已追清外层重复壳：两个函数都先获取根级 `vulcan` 表，创建 `vulcan.<provider>` 表，写入 `__lancedb_skill_name` 或 `__sqlite_skill_name`，完成启用/禁用方法填充后再挂回 `vulcan.<provider>`。
- 已确认嵌套调用保护器会读取并恢复 `__lancedb_skill_name` 与 `__sqlite_skill_name`，因此 marker key、空字符串默认值和写入时机不能改变。
- 已确认错误文本边界需要保留：获取根表失败仍是 `Failed to get vulcan module`，创建 provider 表失败仍是 `Failed to create vulcan.<provider> table`，marker 写入失败仍是 `Failed to set vulcan.__*_skill_name`，最终挂表失败仍是 `Failed to set vulcan.<provider>`。
- 已发现项目内已有 `get_vulcan_table` helper，因此本轮新增 helper 复用该入口，不再重复实现根级 `vulcan` 查找。

### 执行调整

- 新增 `create_provider_context_table`，统一获取根级 `vulcan` 表、创建新的 provider table，并写入当前 skill marker。
- 新增 `install_provider_context_table`，统一在 provider table 填充完成后将其安装回根级 `vulcan` 表。
- 将 LanceDB context 注入函数改为调用 `create_provider_context_table(lua, "lancedb", "__lancedb_skill_name", current_skill_name)`，并在方法注册完成后调用 `install_provider_context_table(&vulcan, "lancedb", lancedb_table)`。
- 将 SQLite context 注入函数改为调用 `create_provider_context_table(lua, "sqlite", "__sqlite_skill_name", current_skill_name)`，并在方法注册完成后调用 `install_provider_context_table(&vulcan, "sqlite", sqlite_table)`。
- 保留启用分支、禁用分支、具体 provider 方法注册、LanceDB payload/bytes 处理和 SQLite 方法列表的现有逻辑，不扩大本轮改动范围。

### 验证记录

- 修改前基线：`cargo test` 通过，224 个测试全部通过。
- 修改前基线：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test runtime_skills -- --nocapture` 通过，3 个运行时技能加载相关测试通过。
- 修改后：`cargo test database -- --nocapture` 通过，4 个数据库相关测试通过。
- 全量验证：`cargo test` 通过，224 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 范围审核：`rg` 确认 `create_provider_context_table`、`install_provider_context_table`、`__lancedb_skill_name`、`__sqlite_skill_name` 和两个 provider 的最终安装调用均在预期位置。
- 错误文本审核：字面量查找确认 provider 表创建与 marker/table 安装错误文本仍由原模板生成，没有改动 Lua 可见诊断表面。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变 Lua 暴露函数名、provider binding 方法、provider 请求字段、返回 JSON 字段、禁用代理方法列表、marker key 或嵌套调用恢复语义。
- `create_provider_context_table` 只收敛 provider 表创建与当前 skill marker 写入，不解释 provider 业务字段，也没有引入候选字段兜底。
- `install_provider_context_table` 只在方法填充完成后发布 provider table，保持原有“先填充后挂表”的观察顺序。
- 修改部分代码审核未发现需要继续自动修复的问题；当前 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续拆分 provider 启用方法注册组，或转向 `call_skill`、`render_help_payload` 等仍然较大的 runtime 执行函数。

## 2026-07-05 第 30 轮：收敛已加载 skill Lua 上下文装配流程

### 问题探索

- 基线验证中，`cargo test` 通过，224 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮沿第 29 轮遗留方向继续检查 `src/runtime/engine.rs` 中 `call_skill` 与 `render_help_payload` 两个 runtime 执行入口。
- 已追清普通 skill 调用执行流：解析 entry 后获取 `LoadedSkill` 与 tool，必要时 debug 重编译 Lua，然后依次填充 request、internal、file、dependency、LanceDB 与 SQLite 上下文，最后从全局表取 handler 并执行。
- 已追清 Lua help 渲染执行流：读取 `.lua` help 文件，创建空参数的 help invocation context，然后依次填充同一组 request、internal、file、dependency、LanceDB 与 SQLite 上下文，最后编译并执行 help chunk。
- 已确认两个入口的重复块是同一个“已加载 skill 上下文装配”流程，差异只在展示工具名、入口名、入口文件路径和 invocation context。
- 已确认不能合并的边界：普通 skill 调用的 debug 编译、handler 查找、参数解析与 help 渲染的文件读取、chunk 编译、返回字符串校验属于不同业务流程，本轮不抽取。

### 执行调整

- 新增 `LoadedSkillLuaContext`，显式承载进入一次已加载 skill 执行所需的入口级元数据：展示工具名、入口名、入口路径和 invocation context。
- 新增 `populate_loaded_skill_lua_context` 私有方法，统一填充 request、internal、file、dependency、LanceDB 与 SQLite 上下文。
- 将 `call_skill` 中的重复上下文装配块替换为 `populate_loaded_skill_lua_context` 调用，保留原来的 `display_tool_name`、`resolved_target.local_name` 和 `tool_entry_path` 映射。
- 将 `render_help_payload` 中的重复上下文装配块替换为同一 helper 调用，保留原来的 `vulcan-help`、`relative_path` 和 `helper_path` 映射。
- 保留 provider binding clone、effective skill id、root name、skill dir 与 dependency context 的原有来源，避免引入任何候选字段或兼容式兜底。

### 验证记录

- 修改前基线：`cargo test` 通过，224 个测试全部通过。
- 修改前基线：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test runtime_skills -- --nocapture` 通过，3 个运行时技能加载相关测试通过。
- 修改后：`cargo test help -- --nocapture` 通过，3 个 help 相关测试通过。
- 修改后：`cargo test database -- --nocapture` 通过，4 个数据库相关测试通过。
- 全量验证：`cargo test` 通过，224 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 范围审核：`rg` 确认 `populate_loaded_skill_lua_context` 只有普通 skill 调用和 Lua help 渲染两个业务调用点。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变 Lua 暴露函数名、handler 查找方式、help chunk 编译方式、返回值解析规则、provider binding 来源、dependency 路径来源或 cleanup 错误合并逻辑。
- `populate_loaded_skill_lua_context` 只抽取已确认同构的上下文装配顺序，仍然按原顺序先填 request/internal/file/deps，再填 provider context。
- `LoadedSkillLuaContext` 只承载入口级事实字段，不承担推断 skill 身份、root、目录或 provider binding 的职责，这些仍来自唯一的 `LoadedSkill`。
- 修改部分代码审核未发现需要继续自动修复的问题；当前 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续检查 runlua/session 的匿名 Lua 上下文装配流程，或进一步拆分 `populate_vulcan_call_for_lua` 的 dispatcher 注册逻辑。

## 2026-07-05 第 31 轮：收敛匿名 Lua 执行上下文装配流程

### 问题探索

- 基线验证中，`cargo test` 通过，224 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮沿第 30 轮遗留方向继续检查 runlua 与 runtime session 的匿名 Lua 执行路径。
- 已追清 `run_lua_with_lease` 执行流：创建 pooled VM request scope 后，填充 request、默认 internal、空 file、清空 dependency、禁用 provider context，然后设置 `__runlua_args` 并执行包装代码。
- 已追清 inline luaexec 执行流：scope reset 先清空 request 级状态，随后填充模拟 request、`luaexec_active=true` 的 internal、可选 entry_file、禁用 provider context，再安装输出捕获与 timeout 后执行 wrapper；该路径原本不重新填 dependency context。
- 已追清 runtime session eval 执行流：显式 reset session VM，填充 request、`luaexec_active=true` 的 internal、空 file、清空 dependency、禁用 provider context，然后设置 session args 并按 session cwd 求值。
- 已确认这些路径共享“匿名 Lua 上下文装配”骨架，差异只在 invocation context、internal context、entry_file 以及 dependency context 处理策略。
- 已确认 inline luaexec 的 dependency 行为必须显式表达为“保留 scope reset 后的当前状态”，不能误改成普通 run_lua/session 的再次清空步骤。

### 执行调整

- 新增 `AnonymousLuaDependencyContext`，用 `ClearWithHostOptions` 与 `PreserveCurrent` 显式表达匿名执行时的 dependency context 策略。
- 新增 `AnonymousLuaExecutionContext`，承载匿名 Lua 执行所需的 invocation context、internal context、entry_file 与 dependency 策略。
- 新增 `populate_anonymous_lua_context` 私有方法，统一填充 request、internal、file、dependency 处理与禁用态 LanceDB/SQLite provider context。
- 将 `reset_pooled_vm_request_scope` 改为调用该 helper 后继续清理 `__runlua_args`。
- 将 `run_lua_with_lease` 改为调用该 helper，并保留默认 internal、无 entry file、清空 dependency 的原有语义。
- 将 inline luaexec 改为调用该 helper，并通过 `PreserveCurrent` 保留 scope reset 后的清空依赖状态，同时保留 `luaexec_caller_tool_name` 与 entry file 映射。
- 将 runtime session eval 改为调用该 helper，并保留 `luaexec_active=true`、无 caller、无 entry file、清空 dependency 的原有语义。

### 验证记录

- 修改前基线：`cargo test` 通过，224 个测试全部通过。
- 修改前基线：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 修改后首次聚焦验证：`cargo test runlua -- --nocapture` 通过，34 个 runlua 相关测试通过。
- 修改后首次聚焦验证：`cargo test session -- --nocapture` 通过，26 个 session 相关测试通过。
- 注释边界修正后：`cargo fmt` 通过。
- 注释边界修正后：`cargo test runlua -- --nocapture` 通过，34 个 runlua 相关测试通过。
- 注释边界修正后：`cargo test session -- --nocapture` 通过，26 个 session 相关测试通过。
- 全量验证：`cargo test` 通过，224 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 范围审核：`rg` 确认匿名上下文装配入口集中到 `populate_anonymous_lua_context`，`runlua.rs` 与 `lease.rs` 不再散落手写的 request/internal/file/deps/provider 连续装配链。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变 runlua wrapper、timeout guard、输出捕获、runtime session cwd 求值、`__runlua_args` 生命周期、provider 禁用状态或 cleanup 错误合并逻辑。
- `AnonymousLuaDependencyContext::PreserveCurrent` 专门用于 inline luaexec，明确保留 `LuaVmRequestScopeGuard` reset 后的清空依赖状态，避免把缺省行为写成隐式遗漏。
- `populate_anonymous_lua_context` 只面向没有 active skill 的 Lua 执行，因此 provider context 仍统一以禁用状态安装，不参与任何 provider binding 解析。
- 修改部分代码审核未发现需要继续自动修复的问题；当前 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续拆分 `populate_vulcan_call_for_lua` 的 dispatcher 注册逻辑，或检查 runtime session/runlua 中 wrapper 构造与 cleanup 结果合并是否还能进一步收敛。

## 2026-07-05 第 32 轮：收敛 pooled VM cleanup 结果合并流程

### 问题探索

- 基线验证中，`cargo test` 通过，224 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮沿第 31 轮遗留方向检查 runtime 执行尾部的 pooled VM cleanup 结果合并逻辑。
- 已追清 `LuaVmRequestScopeGuard::finish` 的真实行为：显式调用 `reset_pooled_vm_request_scope`，清理失败时丢弃 VM lease 并返回 cleanup 错误，成功时关闭 active 标记。
- 已确认四个执行入口拥有同构合并规则：`call_skill`、`render_help_payload`、`run_lua_with_lease` 与 inline luaexec 都先得到主流程结果，再调用 `scope_guard.finish()`，最后按主流程结果和 cleanup 结果组合返回。
- 已确认合并语义完全一致：主流程成功且 cleanup 成功时返回主流程成功值；主流程成功但 cleanup 失败时返回 cleanup 错误；主流程失败但 cleanup 成功时返回主流程错误；二者都失败时返回 `主流程错误; cleanup 标签: cleanup 错误`。
- 已确认差异边界只在 cleanup 标签文本：普通 skill、Lua help 与普通 run_lua 使用 `pooled Lua VM cleanup failed`，inline luaexec 使用 `pooled runlua VM cleanup failed`。

### 执行调整

- 新增 `finish_pooled_vm_request_scope<T>`，统一结束 `LuaVmRequestScopeGuard` 并合并主流程结果与 cleanup 结果。
- helper 通过 `cleanup_error_label` 保留调用点原有错误标签，不把普通 pooled Lua VM 与 dedicated runlua VM 的诊断文本混在一起。
- 将 `call_skill` 的重复 match 替换为 `finish_pooled_vm_request_scope(call_result, scope_guard, "pooled Lua VM cleanup failed")`。
- 将 `render_help_payload` 的重复 match 替换为同一 helper，并保留 `pooled Lua VM cleanup failed` 标签。
- 将 `run_lua_with_lease` 的重复 match 替换为同一 helper，并保留 `pooled Lua VM cleanup failed` 标签。
- 将 inline luaexec 的重复 match 替换为同一 helper，并保留 `pooled runlua VM cleanup failed` 标签。

### 验证记录

- 修改前基线：`cargo test` 通过，224 个测试全部通过。
- 修改前基线：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test runlua -- --nocapture` 通过，34 个 runlua 相关测试通过。
- 修改后：`cargo test help -- --nocapture` 通过，3 个 help 相关测试通过。
- 修改后：`cargo test runtime_skills -- --nocapture` 通过，3 个 runtime skill 相关测试通过。
- 全量验证：`cargo test` 通过，224 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 范围审核：`rg` 确认旧的 `match ((call|rendered|run|render)_result, cleanup_result)` 形态已消失，cleanup 标签集中保留在 helper 调用点。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变 `LuaVmRequestScopeGuard::finish` 的 reset、discard、active 标记行为，也没有改变 Drop 中兜底 reset 的日志与 discard 行为。
- `finish_pooled_vm_request_scope` 只抽取已确认同构的结果组合规则，不参与 runlua wrapper、handler 调用、help 渲染、输出捕获或 provider 上下文装配。
- helper 的泛型只承载主流程成功值类型，错误仍统一为现有 `String`，没有引入新的错误类型或兼容分支。
- 修改部分代码审核未发现需要继续自动修复的问题；当前 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续拆分 `populate_vulcan_call_for_lua` 的 dispatcher 注册逻辑，或继续检查 runlua/session 中 wrapper 构造的重复表达。

## 2026-07-05 第 33 轮：收敛 vulcan.call 外层 invocation context 还原流程

### 问题探索

- 基线验证中，`cargo test` 通过，224 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮沿第 32 轮遗留方向继续检查 `populate_vulcan_call_for_lua` 的 dispatcher 注册与嵌套调用执行流。
- 已追清 dispatcher 执行流程：先从 `dispatch_entries` 找到目标入口，解析 Lua 全局 handler，创建 `LuaNestedCallScopeGuard` 捕获外层状态，再执行 luaexec 禁止规则、provider binding 解析、进入 nested context、调用目标函数并恢复外层状态。
- 已确认 dispatcher 闭包中原先混入一段纯数据转换：从 `nested_scope_guard.previous_context`、`previous_client_budget`、`previous_tool_config` 还原 `LuaInvocationContext`。
- 已确认该转换只依赖 `LuaNestedCallScopeGuard` 捕获的 previous 字段，不依赖目标 entry、provider binding、handler 或 luaexec 阻止规则。
- 已确认需要保留的语义：`vulcan.context.request` 转 JSON 后如果是空对象则 request context 为 `None`；非空对象解析为 `RuntimeRequestContext`，解析失败时继续按现有 `.ok()` 语义变为 `None`。

### 执行调整

- 在 `LuaNestedCallScopeGuard` 上新增 `previous_invocation_context` 方法，集中把外层 `vulcan.context` 快照还原为嵌套调用要继承的 `LuaInvocationContext`。
- 方法内部保留原有 request 空对象判断、`RuntimeRequestContext` 解析 `.ok()` 行为，以及 client budget/tool config 的 Lua 值到 JSON 转换错误映射。
- 将 `populate_vulcan_call_for_lua` dispatcher 闭包中的 `current_request_context_json`、`current_client_budget`、`current_tool_config` 临时转换块替换为 `nested_scope_guard.previous_invocation_context()?`。
- 保留 dispatcher 的目标解析、luaexec 禁止规则、provider binding 解析、nested context 进入、目标函数调用与 restore 结果合并逻辑不变。

### 验证记录

- 修改前基线：`cargo test` 通过，224 个测试全部通过。
- 修改前基线：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test vulcan_call -- --nocapture` 通过，1 个 vulcan.call 相关测试通过。
- 修改后：`cargo test runlua -- --nocapture` 通过，34 个 runlua 相关测试通过。
- 修改后：`cargo test runtime_skills -- --nocapture` 通过，3 个 runtime skill 相关测试通过。
- 全量验证：`cargo test` 通过，224 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 范围审核：`rg` 确认 dispatcher 闭包中的 `current_request_context_json`、`current_client_budget`、`current_tool_config` 临时变量已移除，嵌套 invocation context 构造集中到 `previous_invocation_context`。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变 vulcan.call 的目标查找、handler 查找、luaexec 自调用禁止、luaexec 中禁止再次调用 lua-exec/lua-file、provider binding 解析、nested context 填充或 restore 错误合并逻辑。
- `previous_invocation_context` 只抽取外层 request/budget/tool config 快照到 `LuaInvocationContext` 的纯转换流程，不参与业务决策。
- request context 的空对象与解析失败行为保持原样，没有引入新的兜底路径或候选字段兼容。
- 修改部分代码审核未发现需要继续自动修复的问题；当前 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续拆分 `populate_vulcan_call_for_lua` 中 dispatch entry 预构建或 nested restore 结果合并逻辑。

## 2026-07-05 第 34 轮：收敛 vulcan.call dispatch entry 预构建流程

### 问题探索

- 基线验证中，`cargo test` 通过，224 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮沿第 33 轮遗留方向继续检查 `populate_vulcan_call_for_lua` 中 dispatcher 注册前的 dispatch entry 预构建逻辑。
- 已追清 dispatch entry 的字段来源：`display_name`、`owner_skill_id`、`local_name` 来自 `ResolvedEntryTarget`；`module_name` 来自目标 tool；`root_name` 和 `owner_skill_dir` 来自 `LoadedSkill`；`entry_path` 来自 `tool_entry_path(&skill.dir, tool)`。
- 已确认现有跳过规则：如果 `skills_map` 中找不到 `target.skill_storage_key`，或 skill metadata 中找不到 `target.local_name` 对应 tool，则该 entry 通过 `filter_map` 被跳过。
- 已确认 dispatch entry 构建只负责把运行时 registry 和已加载 skill 快照转成闭包可移动的分发元数据，不参与目标查找、luaexec 禁止规则、provider binding 解析或 nested context 切换。

### 执行调整

- 新增顶层私有结构 `LuaCallDispatchEntry`，承载 `vulcan.call` dispatcher 闭包所需的已解析入口元数据。
- 新增 `build_lua_call_dispatch_entries`，集中从 `skills_map` 与 `entry_registry` 构建 `Vec<LuaCallDispatchEntry>`。
- 将原本定义在 `populate_vulcan_call_for_lua` 内部的本地 `DispatchEntry` 结构移出，避免 dispatcher 注册函数同时承担数据结构定义、数据预构建和运行时分发三层职责。
- 将 `populate_vulcan_call_for_lua` 内部的 `entry_registry.values().filter_map(...)` 构建块替换为 `build_lua_call_dispatch_entries(skills_map, entry_registry)`。
- 保留原有字段来源、`filter_map` 跳过行为、路径字符串化方式和闭包捕获方式不变。

### 验证记录

- 修改前基线：`cargo test` 通过，224 个测试全部通过。
- 修改前基线：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test vulcan_call -- --nocapture` 通过，1 个 vulcan.call 相关测试通过。
- 修改后：`cargo test runtime_skills -- --nocapture` 通过，3 个 runtime skill 相关测试通过。
- 修改后：`cargo test runlua -- --nocapture` 通过，34 个 runlua 相关测试通过。
- 全量验证：`cargo test` 通过，224 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 范围审核：`rg` 确认本地 `struct DispatchEntry` 已移除，`LuaCallDispatchEntry` 与 `build_lua_call_dispatch_entries` 是唯一 dispatch entry 构建入口。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变 `vulcan.call` 的 Lua 可见函数名、目标查找错误、handler 查找错误、luaexec 禁止规则、provider binding 解析、nested context 填充或 restore 逻辑。
- `build_lua_call_dispatch_entries` 只抽取已确认同构的预构建流程，仍然使用原有唯一字段来源，不引入候选字段或兼容兜底。
- `LuaCallDispatchEntry` 只保存 dispatcher 闭包运行所需的不可变元数据，不持有 Lua VM、host 或 provider binding 状态。
- 修改部分代码审核未发现需要继续自动修复的问题；当前 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续检查 `populate_vulcan_call_for_lua` 中 nested restore 结果合并逻辑，或转向 runlua/session wrapper 构造重复表达。

## 2026-07-05 第 35 轮：收敛 vulcan.call nested restore 结果合并流程

### 问题探索

- 基线验证中，`cargo test` 通过，224 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮沿第 34 轮遗留方向继续检查 `populate_vulcan_call_for_lua` 中目标调用结束后的 nested context restore 结果合并逻辑。
- 已追清 `LuaNestedCallScopeGuard::finish` 的真实行为：调用 `restore_previous_state` 恢复外层 `vulcan` 状态，然后把 guard 标记为 inactive，并把 restore 结果返回给调用方。
- 已确认 dispatcher 尾部的合并规则与 nested guard 生命周期强绑定：目标函数调用成功且 restore 成功时返回目标结果；目标成功但 restore 失败时返回 restore 错误；目标失败但 restore 成功时返回目标错误；二者都失败时返回 `目标错误; nested vulcan.call restore failed: restore 错误`。
- 已确认该合并逻辑只依赖目标调用结果与 `LuaNestedCallScopeGuard::finish`，不依赖目标 entry、provider binding 或 luaexec 阻止规则。

### 执行调整

- 在 `LuaNestedCallScopeGuard` 上新增 `finish_nested_call<T>`，统一结束 nested call scope，并合并目标 Lua 调用结果与 restore 结果。
- helper 保留原有四种组合语义和 `nested vulcan.call restore failed` 诊断文本。
- 将 `populate_vulcan_call_for_lua` dispatcher 闭包尾部的 `restore_result` 与 `match (call_result, restore_result)` 替换为 `nested_scope_guard.finish_nested_call(call_result)`。
- 保留 `LuaNestedCallScopeGuard::finish` 与 Drop 中兜底 restore 日志行为不变。

### 验证记录

- 修改前基线：`cargo test` 通过，224 个测试全部通过。
- 修改前基线：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test vulcan_call -- --nocapture` 通过，1 个 vulcan.call 相关测试通过。
- 修改后：`cargo test runlua -- --nocapture` 通过，34 个 runlua 相关测试通过。
- 修改后：`cargo test runtime_skills -- --nocapture` 通过，3 个 runtime skill 相关测试通过。
- 全量验证：`cargo test` 通过，224 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 范围审核：`rg` 确认 dispatcher 中旧的 `restore_result = nested_scope_guard.finish()` 与 `match (call_result, restore_result)` 已移除，restore 合并集中到 `finish_nested_call`。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变 nested call 的目标查找、handler 调用、provider binding 解析、context 进入、外层 context restore 步骤或 Drop 兜底 restore 行为。
- `finish_nested_call` 只抽取已确认同构的目标调用结果与 restore 结果合并流程，不参与任何业务阻止规则。
- 双失败时的错误文本保持原样，没有引入新的错误类型或多分支兜底。
- 修改部分代码审核未发现需要继续自动修复的问题；当前 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续检查 runlua/session wrapper 构造重复表达，或继续拆分 `populate_vulcan_call_for_lua` 中 provider binding 解析逻辑。

## 2026-07-05 第 36 轮：收敛 luaexec 下 vulcan.call 阻止规则

### 问题探索

- 基线验证中，`cargo test` 通过，224 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮沿第 35 轮遗留方向继续检查 `populate_vulcan_call_for_lua` 中 luaexec 激活时的嵌套调用阻止规则。
- 已追清规则触发条件：只有外层 `VulcanInternalExecutionContext.luaexec_active` 为 true 时才进入检查。
- 已追清第一条阻止规则：如果目标 `dispatch_entry.display_name` 等于外层 `luaexec_caller_tool_name`，则拒绝调用并返回 `vulcan.call cannot call the current luaexec caller tool '<name>'`。
- 已追清第二条阻止规则：如果目标来自 `vulcan-runtime` 且 local name 是 `lua-exec` 或 `lua-file`，则拒绝调用并返回 `vulcan.call cannot invoke '<name>' inside luaexec`。
- 已确认这两条规则只依赖目标 `LuaCallDispatchEntry` 和外层 internal context，不依赖 Lua handler、provider binding、nested invocation context 或 restore 流程。

### 执行调整

- 在 `LuaCallDispatchEntry` 上新增 `reject_forbidden_luaexec_call`，集中表达 luaexec 激活期间的两条 `vulcan.call` 阻止规则。
- 方法参数显式接收外层 `VulcanInternalExecutionContext`，避免从闭包里散落读取 `previous_internal_context` 字段。
- 将 dispatcher 闭包中的内联 `if nested_scope_guard.previous_internal_context.luaexec_active { ... }` 规则块替换为 `dispatch_entry.reject_forbidden_luaexec_call(&nested_scope_guard.previous_internal_context)?`。
- 保留两条错误文本、触发条件、检查顺序和非 luaexec 状态下直接允许的行为不变。

### 验证记录

- 修改前基线：`cargo test` 通过，224 个测试全部通过。
- 修改前基线：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test vulcan_call -- --nocapture` 通过，1 个 vulcan.call 相关测试通过。
- 修改后：`cargo test runlua -- --nocapture` 通过，34 个 runlua 相关测试通过。
- 修改后：`cargo test runtime_skills -- --nocapture` 通过，3 个 runtime skill 相关测试通过。
- 全量验证：`cargo test` 通过，224 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 范围审核：`rg` 确认两条 luaexec 阻止错误文本集中在 `reject_forbidden_luaexec_call`，dispatcher 闭包只保留 helper 调用。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变 `vulcan.call` 的目标查找、handler 查找、provider binding 解析、nested context 进入、目标调用或 restore 行为。
- `reject_forbidden_luaexec_call` 只抽取已确认的业务阻止规则，不引入新的工具名候选、owner 候选或兼容分支。
- 非 luaexec 状态仍直接允许调用；luaexec 状态下的两条阻止规则顺序与错误文本保持原样。
- 修改部分代码审核未发现需要继续自动修复的问题；当前 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续检查 `populate_vulcan_call_for_lua` 中 provider binding 解析逻辑，或转向 runlua/session wrapper 构造重复表达。

## 2026-07-05 第 37 轮：收敛 vulcan.call provider binding 解析流程

### 问题探索

- 基线验证中，`cargo test` 通过，224 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮沿第 36 轮遗留方向继续检查 `populate_vulcan_call_for_lua` 中进入嵌套目标前的 provider binding 解析流程。
- 已追清 `LuaNestedCallTarget` 的 provider 字段归属：进入 nested call 前需要为目标所属 skill 安装 `lancedb_binding` 与 `sqlite_binding`。
- 已确认目标 provider binding 的唯一来源是当前 dispatch entry 的 `owner_skill_id`，即 dispatcher 中的 `owner_skill_name`；没有发现其他候选 skill 名、模块名或 fallback 来源。
- 已确认 LanceDB 与 SQLite 的解析规则同构：对应 host 存在时调用 `binding_for_skill(owner_skill_name)`，host 不存在时保持 `None`，binding 解析错误由调用点转换为 `mlua::Error::runtime`。
- 已单独核对 `restore_previous_state`：恢复外层 provider context 使用进入 nested call 前保存的 `__lancedb_skill_name` 与 `__sqlite_skill_name` marker，不属于目标 provider binding 解析链路，本轮不修改该流程。

### 执行调整

- 新增 `LuaCallProviderBindings`，集中承载一次嵌套 `vulcan.call` 目标解析出的 LanceDB 与 SQLite binding。
- 新增 `resolve_lua_call_provider_bindings`，以目标 `owner_skill_name` 和两个可选共享 host 为唯一输入，统一解析目标 provider binding。
- 将 dispatcher 闭包中两段内联 `match lancedb_host/sqlite_host` 解析逻辑替换为 helper 调用。
- `LuaNestedCallTarget` 继续接收明确的 `lancedb_binding` 与 `sqlite_binding` 字段，进入 nested context 的后续流程保持不变。

### 验证记录

- 修改前基线：`cargo test` 通过，224 个测试全部通过。
- 修改前基线：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test vulcan_call -- --nocapture` 通过，1 个 vulcan.call 相关测试通过。
- 修改后：`cargo test runlua -- --nocapture` 通过，34 个 runlua 相关测试通过。
- 修改后：`cargo test runtime_skills -- --nocapture` 通过，3 个 runtime skill 相关测试通过。
- 修改后：`cargo test database -- --nocapture` 通过，4 个数据库相关测试通过。
- 全量验证：`cargo test` 通过，224 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 范围审核：`rg` 确认旧的 `target_binding` 与 `target_sqlite_binding` 内联变量已移除，`binding_for_skill(owner_skill_name)` 只保留在 `resolve_lua_call_provider_bindings` 内。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变 `vulcan.call` 的目标查找、Lua handler 查找、luaexec 阻止规则、nested invocation context 构造、context 进入或外层 restore 行为。
- `resolve_lua_call_provider_bindings` 只使用已确认的目标 owner skill id 与两个可选 host，不引入候选字段、多来源 fallback 或模糊兼容逻辑。
- host 缺失时继续返回 `None`，host 存在但 binding 解析失败时仍在 dispatcher 调用点转为 Lua runtime error，错误边界保持不变。
- 修改部分代码审核未发现需要继续自动修复的问题；当前 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续检查 runlua/session wrapper 构造重复表达，或继续拆分 `populate_vulcan_call_for_lua` 中目标函数查找与 nested scope 构造的边界。

## 2026-07-05 第 38 轮：收敛 skill handler 全局键命名规则

### 问题探索

- 基线验证中，`cargo test` 通过，224 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮继续拆解 Lua skill 调用链中 `__skill_{lua_module}` 私有全局键的生产与消费流程。
- 已追清生产者：`compile_skill_into_lua` 在编译并初始化 tool 入口后，把 handler 写入 Lua globals 中的 `__skill_{tool.lua_module}`。
- 已追清普通消费路径：`call_skill` 根据 resolved target 找到 tool，再用 `tool.lua_module` 读取同一个 Lua global handler。
- 已追清 nested 消费路径：`populate_vulcan_call_for_lua` 的 dispatcher 根据 `LuaCallDispatchEntry.module_name` 读取同一个 Lua global handler，再进入 nested scope。
- CodeKit 范围搜索确认 `format!("__skill_{}", ...)` 只散落在上述三处；没有发现第四条生产或消费路径。

### 执行调整

- 新增 `lua_skill_handler_global_name`，集中表达 Lua skill handler 私有全局键命名规则。
- `compile_skill_into_lua` 注册 handler 时改用该 helper 生成 key。
- `call_skill` 读取普通 skill handler 时改用该 helper 生成 key。
- `populate_vulcan_call_for_lua` 读取 nested skill handler 时改用该 helper 生成 key。
- 保留三条路径原有的错误处理边界：注册错误仍由注册路径报告，普通调用缺失 handler 仍携带底层 Lua 错误，nested 调用缺失 handler 仍返回 Lua runtime error。

### 验证记录

- 修改前基线：`cargo test` 通过，224 个测试全部通过。
- 修改前基线：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test call_skill -- --nocapture` 通过，2 个 call_skill 相关测试通过。
- 修改后：`cargo test vulcan_call -- --nocapture` 通过，1 个 vulcan.call 相关测试通过。
- 修改后：`cargo test runtime_skills -- --nocapture` 通过，3 个 runtime skill 相关测试通过。
- 全量验证：`cargo test` 通过，224 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 范围审核：CodeKit 搜索确认 `format!("__skill_{}", module_name)` 只保留在 `lua_skill_handler_global_name` 内，三条调用链都改为 helper 调用。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变 skill target 解析、debug hot reload、Lua chunk 编译、handler 初始化、参数转换、输出解析或 nested context 进入与恢复流程。
- `lua_skill_handler_global_name` 只接收已确认的 manifest `lua_module`，没有引入候选模块名、兼容键名或多路查找逻辑。
- 修改部分代码审核未发现需要继续自动修复的问题；当前 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续检查 `populate_vulcan_call_for_lua` 中目标函数读取与 nested scope 构造之间的闭包职责，或转向普通 `call_skill` 中 request scope 与调用结果合并边界。

## 2026-07-05 第 39 轮：收敛 skill handler 读取入口

### 问题探索

- 基线验证中，`cargo test` 通过，224 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮沿第 38 轮继续检查 Lua skill handler 的读取链路，重点确认哪些路径会按 `lua_module` 从 Lua globals 中取 handler。
- CodeKit 结构搜索确认 handler 读取只有两条消费路径：普通 `call_skill` 和 nested `populate_vulcan_call_for_lua` dispatcher。
- 已追清普通消费路径：`call_skill` 解析目标 tool 后，用 `module_name` 读取 handler，缺失时返回包含底层 Lua 错误的 `String`。
- 已追清 nested 消费路径：`populate_vulcan_call_for_lua` 通过 `LuaCallDispatchEntry.module_name` 读取 handler，缺失时转换为 `mlua::Error::runtime`。
- 已确认注册路径 `compile_skill_into_lua` 只负责写入 handler，不承担读取职责，因此本轮不改变注册错误处理。

### 执行调整

- 新增 `resolve_lua_skill_handler`，集中执行“按 manifest Lua module name 从 Lua globals 中读取已编译 skill handler”的操作。
- `call_skill` 改为通过 `resolve_lua_skill_handler` 读取 handler，并保留原本包含底层 Lua 错误的错误消息。
- `populate_vulcan_call_for_lua` dispatcher 改为通过 `resolve_lua_skill_handler` 读取 nested handler，并保留原本的 Lua runtime error 边界。
- `lua_skill_handler_global_name` 继续作为唯一的私有全局键命名规则来源，`resolve_lua_skill_handler` 只组合使用该规则与 Lua globals 查找。

### 验证记录

- 修改前基线：`cargo test` 通过，224 个测试全部通过。
- 修改前基线：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test call_skill -- --nocapture` 通过，2 个 call_skill 相关测试通过。
- 修改后：`cargo test vulcan_call -- --nocapture` 通过，1 个 vulcan.call 相关测试通过。
- 修改后：`cargo test runtime_skills -- --nocapture` 通过，3 个 runtime skill 相关测试通过。
- 全量验证：`cargo test` 通过，224 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 范围审核：CodeKit 搜索确认 `resolve_lua_skill_handler` 是 handler 读取入口，普通调用与 nested 调用均通过该 helper；直接 handler key 读取只在 helper 内部出现。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变 skill target 解析、handler 注册、debug hot reload、参数表转换、Lua handler 调用、输出解析或 nested context 进入与恢复流程。
- `resolve_lua_skill_handler` 只使用已确认的 `module_name` 与单一私有全局键规则，不引入候选键、多路查找或兼容 fallback。
- 两条消费路径的错误包装仍由原调用点负责，避免把普通调用的 `String` 错误边界与 nested Lua runtime error 边界混淆。
- 修改部分代码审核未发现需要继续自动修复的问题；当前 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续检查普通 `call_skill` 中 handler 调用与输出解析的结果边界，或继续拆分 `populate_vulcan_call_for_lua` 中 dispatcher 闭包的职责。

## 2026-07-05 第 40 轮：收敛普通 skill handler 调用结果边界

### 问题探索

- 基线验证中，`cargo test` 通过，224 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮沿第 39 轮继续检查普通 `call_skill` 在解析 handler 后的调用与输出解析流程。
- CodeKit 结构搜索确认 `handler.call(args_table)` 与 `parse_tool_call_output` 的组合只属于普通 `call_skill` 路径。
- 已追清 nested `vulcan.call` 的目标函数调用流程：nested dispatcher 直接调用目标 Lua function 并返回 `MultiValue`，再由 `finish_nested_call` 合并 restore 结果，不走 `parse_tool_call_output`。
- 已追清普通 `call_skill` 的错误边界：handler 调用失败时记录 `[LuaSkill:error] Lua skill '<tool>' error: ...` 并返回该消息；输出解析失败时记录 `[LuaSkill:error] <parse error>` 并返回解析错误。
- 已确认 request scope cleanup 合并由 `finish_pooled_vm_request_scope` 负责，不属于 handler 调用与输出解析本身。

### 执行调整

- 新增 `invoke_loaded_lua_skill_handler`，集中表达普通 skill handler 调用、Lua 错误日志记录、输出解析与解析错误日志记录。
- `call_skill` 在完成目标解析、VM 获取、上下文填充、handler 解析与 JSON 参数转换后，改为调用 `invoke_loaded_lua_skill_handler`。
- 保留 `finish_pooled_vm_request_scope(call_result, scope_guard, "pooled Lua VM cleanup failed")` 的 cleanup 合并边界不变。
- 保留 nested `vulcan.call` 直接调用目标 Lua function 并返回 `MultiValue` 的语义，不复用普通输出解析 helper。

### 验证记录

- 修改前基线：`cargo test` 通过，224 个测试全部通过。
- 修改前基线：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test call_skill -- --nocapture` 通过，2 个 call_skill 相关测试通过。
- 修改后：`cargo test runtime_skills -- --nocapture` 通过，3 个 runtime skill 相关测试通过。
- 修改后：`cargo test runlua -- --nocapture` 通过，34 个 runlua 相关测试通过。
- 全量验证：`cargo test` 通过，224 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 范围审核：CodeKit 搜索确认 `handler.call(args_table)` 与 `parse_tool_call_output` 集中在 `invoke_loaded_lua_skill_handler` 内，`call_skill` 只保留 helper 调用。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变 skill target 解析、debug hot reload、Lua handler 解析、JSON 参数转换、pooled VM request scope cleanup 或 nested `vulcan.call` 的返回值与 restore 语义。
- `invoke_loaded_lua_skill_handler` 只处理普通 skill 调用的已确认流程，不引入可选 handler、候选输出格式或兼容 fallback。
- 两类失败日志文本保持原样，普通调用失败与输出解析失败仍返回原有 `String` 错误边界。
- 修改部分代码审核未发现需要继续自动修复的问题；当前 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续检查普通 `call_skill` 中目标解析元数据的收束，或继续削薄 `populate_vulcan_call_for_lua` dispatcher 闭包。

## 2026-07-05 第 41 轮：收敛普通 call_skill 目标解析边界

### 问题探索

- 基线验证中，`cargo test` 通过，224 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮沿第 40 轮继续检查普通 `call_skill` 从宿主可见工具名到实际已加载 skill/tool 元数据的解析流程。
- CodeKit 结构搜索确认 `call_skill` 的真实解析链路是：`entry_registry.get(tool_name)` 取得 `ResolvedEntryTarget`，再通过 `skill_storage_key` 读取 `self.skills`，最后通过 `ResolvedEntryTarget.local_name` 调用 `SkillMeta::find_tool_by_local_name`。
- 已确认 `ResolvedEntryTarget.canonical_name` 是调用期间用于诊断与 `vulcan.runtime.internal.tool_name` 的展示工具名。
- 已确认 `ResolvedEntryTarget.local_name` 是填充 `LoadedSkillLuaContext.entry_name` 的唯一来源，不应从 `tool.name` 或其他字段猜测。
- 已确认三段解析失败原本都返回同一错误文本 `Lua skill '<tool_name>' not found`，本轮需要保持该错误边界。

### 执行调整

- 新增 `CallSkillInvocationTarget`，集中承载普通 `call_skill` 已解析出的 loaded skill、tool metadata、canonical display tool name 与 local entry name。
- 新增 `LuaEngine::resolve_call_skill_invocation_target`，统一封装 `entry_registry -> skills -> local tool` 三段解析流程。
- `call_skill` 主流程改为先取得 `invocation_target`，后续 debug hot reload、entry path、Lua context 填充、handler 解析与调用均使用该解析结果。
- 保留 `module_name` 从已解析 tool 的 `lua_module` 克隆得到，用于 handler lookup 错误消息与后续调用。

### 验证记录

- 修改前基线：`cargo test` 通过，224 个测试全部通过。
- 修改前基线：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test call_skill -- --nocapture` 通过，2 个 call_skill 相关测试通过。
- 修改后：`cargo test runtime_skills -- --nocapture` 通过，3 个 runtime skill 相关测试通过。
- 修改后：`cargo test standard_ffi_call_skill -- --nocapture` 通过，1 个 FFI 标准调用相关测试通过。
- 全量验证：`cargo test` 通过，224 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 范围审核：CodeKit 搜索确认 `call_skill` 主流程只保留 `resolve_call_skill_invocation_target(tool_name)`，三段目标解析集中在 helper 内。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变 entry registry 构建、skill metadata 结构、debug hot reload、Lua context 填充、handler lookup、参数转换、handler 调用或 pooled VM cleanup 合并语义。
- `resolve_call_skill_invocation_target` 只使用已确认的 `tool_name`、`skill_storage_key` 与 `local_name` 链路，不引入候选 tool 名、fallback skill 查找或兼容字段。
- 三段解析失败仍返回原来的 `Lua skill '<tool_name>' not found`，没有改变对外错误文本。
- 修改部分代码审核未发现需要继续自动修复的问题；当前 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续检查普通 `call_skill` 中 VM scope/context 准备边界，或继续削薄 `populate_vulcan_call_for_lua` dispatcher 闭包。

## 2026-07-05 第 42 轮：收敛普通 call_skill Lua 上下文准备边界

### 问题探索

- 基线验证中，`cargo test` 通过，224 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮沿第 41 轮继续检查普通 `call_skill` 中 VM scope、debug reload、loaded-skill context 填充、handler lookup 与 request cleanup 的执行顺序。
- CodeKit 结构搜索确认普通 `call_skill` 的 request scope 创建由 `LuaVmRequestScopeGuard::new` 完成，cleanup 合并由 `finish_pooled_vm_request_scope` 完成。
- 已追清 debug hot reload 只在 `invocation_target.skill.meta.debug` 为 true 时调用 `compile_skill_into_lua(lua, skill, tool, true)`。
- 已追清 entry path 与 loaded skill context 填充只依赖已解析的 skill、tool、canonical display tool name、local entry name 与 invocation context。
- 已确认 handler lookup、JSON 参数转换、handler 调用和 output parse 不属于该上下文准备边界；request scope 生命周期也不应被抽入该 helper。

### 执行调整

- 新增 `LuaEngine::prepare_call_skill_lua_context`，集中表达普通 `call_skill` 调用前的 debug hot reload 与 loaded-skill Lua context 填充。
- `call_skill` 在创建 `LuaVmRequestScopeGuard` 并取得 `lua` 后，改为调用 `prepare_call_skill_lua_context(lua, &invocation_target, invocation_context)`。
- 保留 `LuaVmRequestScopeGuard::new` 与 `finish_pooled_vm_request_scope(call_result, scope_guard, "pooled Lua VM cleanup failed")` 在 `call_skill` 主流程中，避免隐藏 request scope 生命周期。
- 保留 handler lookup、JSON 参数转换、handler 调用与 output parse 的现有边界不变。

### 验证记录

- 修改前基线：`cargo test` 通过，224 个测试全部通过。
- 修改前基线：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test call_skill -- --nocapture` 通过，2 个 call_skill 相关测试通过。
- 修改后：`cargo test runtime_skills -- --nocapture` 通过，3 个 runtime skill 相关测试通过。
- 修改后：`cargo test standard_ffi_call_skill -- --nocapture` 通过，1 个 FFI 标准调用相关测试通过。
- 全量验证：`cargo test` 通过，224 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 范围审核：CodeKit 搜索确认 `call_skill` 主流程只保留 `prepare_call_skill_lua_context` 调用，debug reload 与 loaded-skill context 填充集中在 helper 内。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变目标解析、VM lease 获取、request scope 创建与清理、handler lookup、参数转换、handler 调用、输出解析或 nested `vulcan.call` 行为。
- `prepare_call_skill_lua_context` 只使用已确认的 `CallSkillInvocationTarget` 字段，不引入候选 entry path、候选 skill context 或兼容 fallback。
- request scope 生命周期仍显式保留在 `call_skill` 主流程中，便于继续审查 cleanup 合并边界。
- 修改部分代码审核未发现需要继续自动修复的问题；当前 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续检查普通 `call_skill` 的 handler lookup/JSON 参数转换边界，或转向 `render_help_payload` 中与 `call_skill` 相似的 pooled VM/context 准备流程。

## 2026-07-05 第 43 轮：收敛普通 call_skill Lua 调用输入准备边界

### 问题探索

- 基线验证中，`cargo test` 通过，224 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮沿第 42 轮继续检查普通 `call_skill` 在上下文准备完成后、handler 调用前的 Lua 调用输入准备流程。
- CodeKit 结构搜索确认普通 `call_skill` 中 handler lookup 使用 `resolve_lua_skill_handler(lua, &module_name)`，并将缺失 handler 包装为 `Skill function '<module>' not found: <lua error>`。
- 已追清普通 `call_skill` 的 JSON 参数转换使用 `json_to_lua_table(lua, args)`，转换结果只用于当前 handler 调用。
- 已确认 nested `vulcan.call` 也使用 `resolve_lua_skill_handler`，但它需要保留自己的 `mlua::Error::runtime` 错误边界，不应复用普通 `call_skill` 的 `String` 错误包装。
- 已确认 runlua/session 也使用 `json_to_lua_table`，但它们没有 skill handler lookup，不属于普通 `call_skill` 的 invocation input 准备流程。

### 执行调整

- 新增 `CallSkillLuaInvocationInput`，集中承载普通 `call_skill` 调用前准备好的 Lua handler 与 Lua 参数表。
- 新增 `prepare_call_skill_lua_invocation_input`，统一封装普通 `call_skill` 的 handler lookup 错误包装与 JSON 参数转 Lua 表。
- `call_skill` 主流程改为调用 `prepare_call_skill_lua_invocation_input(lua, &module_name, args)`，再把返回的 handler 与 args table 交给 `invoke_loaded_lua_skill_handler`。
- 保留 nested `vulcan.call` 的 handler lookup 与错误包装边界不变，保留 runlua/session 的参数转换调用不变。

### 验证记录

- 修改前基线：`cargo test` 通过，224 个测试全部通过。
- 修改前基线：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test call_skill -- --nocapture` 通过，2 个 call_skill 相关测试通过。
- 修改后：`cargo test runtime_skills -- --nocapture` 通过，3 个 runtime skill 相关测试通过。
- 修改后：`cargo test standard_ffi_call_skill -- --nocapture` 通过，1 个 FFI 标准调用相关测试通过。
- 全量验证：`cargo test` 通过，224 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 范围审核：CodeKit 搜索确认普通 `call_skill` 主流程只保留 `prepare_call_skill_lua_invocation_input`，handler lookup 与 JSON 参数转换集中在 helper 内。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变目标解析、VM lease 获取、request scope 创建与清理、debug hot reload、Lua context 填充、handler 调用、输出解析、nested `vulcan.call` 或 runlua/session 行为。
- `prepare_call_skill_lua_invocation_input` 只使用已确认的 `module_name` 与调用方 JSON 参数，不引入候选 handler、候选参数来源或多路 fallback。
- 普通 handler 缺失错误文本仍保持原来的 `Skill function '<module>' not found: <lua error>`，参数转换错误仍由 `json_to_lua_table` 原样返回。
- 修改部分代码审核未发现需要继续自动修复的问题；当前 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可转向 `render_help_payload` 中与 `call_skill` 相似的 pooled VM/context 准备流程，或继续削薄 `populate_vulcan_call_for_lua` dispatcher 闭包。

## 2026-07-05 第 44 轮：收敛 Lua help payload 执行与文本校验边界

### 问题探索

- 基线验证中，`cargo test` 通过，224 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮沿第 43 轮遗留方向检查 `render_help_payload` 中 Lua help 文件的执行流程。
- CodeKit 结构搜索确认 Lua help 文件的 compile、init、可选 runtime function 调用、UTF-8 字符串转换和非字符串错误诊断全部内联在 `render_help_payload` 的 pooled VM 流程中。
- 已追清 plain text help 路径：非 `.lua` help 文件直接通过 `read_skill_text_file(&skill.dir, relative_path, "help")` 读取，不进入 Lua VM。
- 已追清 Lua help 路径的外层职责：读取 helper source、创建 pooled VM request scope、安装 `vulcan-help` 上下文、构造 chunk name，并在末尾通过 `finish_pooled_vm_request_scope` 合并 cleanup 结果。
- 已确认 Lua help 执行与文本校验只依赖 `lua`、`helper_path`、`helper_source` 与 `chunk_name`，不需要持有 scope guard 或修改外层 context 准备流程。

### 执行调整

- 新增 `render_lua_help_payload_text`，集中执行 Lua help chunk 编译、初始化、可选 function runtime 调用、返回值 UTF-8 文本转换与类型校验。
- `render_help_payload` 在完成 helper source 读取、VM scope 创建和 `vulcan-help` context 填充后，改为调用 `render_lua_help_payload_text` 得到 `rendered_result`。
- 保留 `finish_pooled_vm_request_scope(rendered_result, scope_guard, "pooled Lua VM cleanup failed")` 在 `render_help_payload` 外层，request scope cleanup 边界不变。
- 保留所有 Lua help compile/init/runtime/UTF-8/type 错误文本不变。

### 验证记录

- 修改前基线：`cargo test` 通过，224 个测试全部通过。
- 修改前基线：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test help -- --nocapture` 通过，3 个 help 相关测试通过。
- 修改后：`cargo test runtime_skills -- --nocapture` 通过，3 个 runtime skill 相关测试通过。
- 全量验证：`cargo test` 通过，224 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 范围审核：CodeKit 搜索确认所有 Lua help compile/init/runtime/UTF-8/type 错误文本集中在 `render_lua_help_payload_text`，`render_help_payload` 只保留 helper 调用与 cleanup 合并。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变 plain text help 读取、Lua help source 读取、VM lease 获取、request scope 创建与清理、`vulcan-help` context 填充或 chunk name 构造语义。
- `render_lua_help_payload_text` 只使用已确认的 `lua`、`helper_path`、`helper_source` 与 `chunk_name`，不引入候选文件路径、候选返回格式或兼容 fallback。
- pooled VM cleanup 合并仍显式保留在 `render_help_payload` 中，便于继续审查 help 渲染的 scope 生命周期。
- 修改部分代码审核未发现需要继续自动修复的问题；当前 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续检查 `render_help_payload` 的 `vulcan-help` context 准备边界，或转向 `populate_vulcan_call_for_lua` dispatcher 闭包。

## 2026-07-05 第 45 轮：收敛 Lua help 的 vulcan-help 上下文准备边界

### 问题探索

- 基线验证中，`cargo test` 通过，224 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮沿第 44 轮继续检查 `render_help_payload` 中 Lua help 的 `vulcan-help` context 准备流程。
- CodeKit 结构搜索确认固定工具名 `vulcan-help` 只在 `render_help_payload` 这条 Lua help 路径中使用。
- 已追清 help invocation context 的构造规则：复制可选 `RuntimeRequestContext`，并使用两个空 JSON object 作为 client budget 与 tool config。
- 已追清 `LoadedSkillLuaContext` 的 help 字段来源：`display_tool_name` 固定为 `vulcan-help`，`entry_name` 来自 manifest 中的 `relative_path`，`entry_path` 来自已解析的 `helper_path`。
- 已确认文件读取、VM scope 创建、Lua help payload 执行和 pooled VM cleanup 不属于 help context 准备本身。

### 执行调整

- 新增 `LuaEngine::prepare_lua_help_context`，集中构造 help invocation context，并安装 `vulcan-help` 的 loaded-skill Lua context。
- `render_help_payload` 在创建 pooled VM scope 并取得 `lua` 后，改为调用 `prepare_lua_help_context(lua, skill, relative_path, &helper_path, request_context)`。
- 保留 helper source 读取、chunk name 构造、`render_lua_help_payload_text` 调用与 `finish_pooled_vm_request_scope` cleanup 合并在 `render_help_payload` 外层。
- 保留 `vulcan-help` 的工具名、entry name、entry path 和 request context 暴露语义不变。

### 验证记录

- 修改前基线：`cargo test` 通过，224 个测试全部通过。
- 修改前基线：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test help -- --nocapture` 通过，3 个 help 相关测试通过。
- 修改后：`cargo test runtime_skills -- --nocapture` 通过，3 个 runtime skill 相关测试通过。
- 全量验证：`cargo test` 通过，224 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 范围审核：CodeKit 搜索确认 `request_context.cloned()`、固定 `vulcan-help` 工具名和 help context 填充集中在 `prepare_lua_help_context` 内。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变 plain text help 读取、Lua help source 读取、VM lease 获取、request scope 创建与清理、Lua help payload 执行、返回文本校验或 chunk name 构造语义。
- `prepare_lua_help_context` 只使用已确认的 skill、relative path、helper path 与 request context，不引入候选工具名、候选 entry path 或兼容 fallback。
- pooled VM cleanup 合并仍显式保留在 `render_help_payload` 中，Lua help 执行仍由 `render_lua_help_payload_text` 负责。
- 修改部分代码审核未发现需要继续自动修复的问题；当前 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续检查 `render_help_payload` 的 helper source 读取与 pooled VM scope 边界，或转向 `populate_vulcan_call_for_lua` dispatcher 闭包。

## 2026-07-05 第 46 轮：收敛 Lua help source 路径解析与读取边界

### 问题探索

- 基线验证中，`cargo test` 通过，224 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮沿第 45 轮继续检查 `render_help_payload` 中 Lua help source 的路径解析与读取流程。
- CodeKit 结构搜索确认 plain text help 路径仍通过 `read_skill_text_file(&skill.dir, relative_path, "help")` 直接返回，不进入 Lua VM。
- 已追清 Lua help source 路径：只由 `skill.dir.join(relative_path)` 解析得到，后续同时用于 `vulcan-help` 文件上下文和 Lua help 执行诊断。
- 已追清 Lua help source 读取错误文本：读取失败返回 `Failed to read help file <path>: <error>`，与 plain text help 的 help label 错误格式保持一致。
- 已确认 VM lease、request scope、`vulcan-help` context 准备、chunk name 构造、Lua help 执行和 cleanup 合并不属于 source 读取边界。

### 执行调整

- 新增 `LuaHelpPayloadSource`，集中承载 Lua help 的已解析 `helper_path` 与 UTF-8 `source`。
- 新增 `read_lua_help_payload_source`，统一封装 `skill.dir.join(relative_path)` 与 Lua help source 文件读取。
- `render_help_payload` 中 Lua help 分支改为先读取 `help_payload_source`，再把其中的 `helper_path` 与 `source` 分别传给 help context 准备和 Lua help 执行 helper。
- 保留 plain text help 的 `read_skill_text_file` 入口、Lua help 读取错误文本、VM scope 生命周期和 cleanup 合并边界不变。

### 验证记录

- 修改前基线：`cargo test` 通过，224 个测试全部通过。
- 修改前基线：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test help -- --nocapture` 通过，3 个 help 相关测试通过。
- 修改后：`cargo test runtime_skills -- --nocapture` 通过，3 个 runtime skill 相关测试通过。
- 全量验证：`cargo test` 通过，224 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 范围审核：CodeKit 搜索确认 `Failed to read help file` 错误文本集中在 `read_lua_help_payload_source`，`render_help_payload` 只保留 `help_payload_source` 调用与后续流程传参。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变 `.lua` 判定、plain text help 读取、VM lease 获取、request scope 创建与清理、`vulcan-help` context 填充、Lua help payload 执行、返回文本校验或 chunk name 构造语义。
- `read_lua_help_payload_source` 只使用已确认的 skill 目录与 manifest relative path，不引入候选路径、fallback 文件或兼容读取策略。
- `LuaHelpPayloadSource` 只是把必须同行传递的 helper path 与 source 绑定成明确数据对象，避免后续流程使用错配的路径与源码。
- 修改部分代码审核未发现需要继续自动修复的问题；当前 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续检查 `render_help_payload` 的 chunk name 构造边界，或转向 `populate_vulcan_call_for_lua` dispatcher 闭包。

## 2026-07-05 第 47 轮：收敛 vulcan.call dispatch entry 查找边界

### 问题探索

- 基线验证中，`cargo test` 通过，224 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮转向 `populate_vulcan_call_for_lua` dispatcher 闭包，检查 Lua 调用名到 `LuaCallDispatchEntry` 的查找流程。
- CodeKit 结构搜索确认 `dispatch_entries` 只由 `build_lua_call_dispatch_entries(skills_map, entry_registry)` 预构建，并移动进 `vulcan.call` dispatcher 闭包。
- 已追清 Lua 调用名来源：`require_string_arg(name, "call", "name", false)` 解析出调用方传入的 skill 名。
- 已追清目标匹配规则：只按 `entry.display_name == name` 线性查找，不存在其他候选名、owner 名或 local entry 名 fallback。
- 已确认查找失败原本返回 `mlua::Error::runtime(format!("Skill '{}' not found", name))`，后续 handler lookup、luaexec guard、provider binding 与 nested context 进入都依赖成功解析出的 dispatch entry。

### 执行调整

- 新增 `resolve_lua_call_dispatch_entry`，集中封装 `vulcan.call` 的 dispatch entry 查找与 not-found runtime error。
- `populate_vulcan_call_for_lua` dispatcher 闭包改为调用 `resolve_lua_call_dispatch_entry(&dispatch_entries, &name)`。
- 保留 `dispatch_entries` 的构建方式、匹配字段 `display_name` 和 not-found 错误文本不变。
- 保留后续 handler lookup、luaexec 阻止规则、provider binding 解析、nested context 进入和目标调用流程不变。

### 验证记录

- 修改前基线：`cargo test` 通过，224 个测试全部通过。
- 修改前基线：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test vulcan_call -- --nocapture` 通过，1 个 vulcan.call 相关测试通过。
- 修改后：`cargo test runtime_skills -- --nocapture` 通过，3 个 runtime skill 相关测试通过。
- 修改后：`cargo test runlua -- --nocapture` 通过，34 个 runlua 相关测试通过。
- 全量验证：`cargo test` 通过，224 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 范围审核：CodeKit 搜索确认 `display_name == name` 与 `Skill '<name>' not found` 现在集中在 `resolve_lua_call_dispatch_entry` 内，dispatcher 闭包只保留 helper 调用。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变 `vulcan.call` 参数解析、dispatch entries 构建、Lua handler lookup、luaexec 阻止规则、provider binding 解析、nested context 进入、目标调用或 restore 合并语义。
- `resolve_lua_call_dispatch_entry` 只使用已确认的 `display_name` 匹配规则，不引入候选名称、模糊匹配或兼容 fallback。
- not-found 错误仍是原来的 Lua runtime error 文本 `Skill '<name>' not found`。
- 修改部分代码审核未发现需要继续自动修复的问题；当前 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续检查 `populate_vulcan_call_for_lua` 中 handler lookup 与 nested scope guard 创建边界，或回到 `render_help_payload` 的 chunk name 构造边界。

## 2026-07-05 第 48 轮：收敛 vulcan.call handler lookup 边界

### 问题探索

- 基线验证中，`cargo test` 通过，224 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮沿第 47 轮继续检查 `populate_vulcan_call_for_lua` dispatcher 中 dispatch entry 解析成功后的 Lua handler lookup 流程。
- CodeKit 结构搜索确认普通 `call_skill` 和 nested `vulcan.call` 都复用 `resolve_lua_skill_handler` 的底层 Lua globals lookup。
- 已追清普通 `call_skill` 的错误边界：handler 缺失时保留底层 Lua lookup 错误，返回 `String` 错误文本 `Skill function '<module>' not found: <lua error>`。
- 已追清 nested `vulcan.call` 的错误边界：handler 缺失时丢弃底层 Lua lookup 错误，并转换为 `mlua::Error::runtime(format!("Skill function '{}' not found", module))`。
- 已确认 nested handler lookup 只依赖已解析 `LuaCallDispatchEntry.module_name`，不依赖 owner skill、provider binding、nested scope guard 或 args table。

### 执行调整

- 新增 `resolve_lua_call_dispatch_handler`，集中封装 nested `vulcan.call` 通过 dispatch entry module name 解析 Lua handler 的流程。
- `populate_vulcan_call_for_lua` dispatcher 闭包改为调用 `resolve_lua_call_dispatch_handler(lua, dispatch_entry)`。
- 保留普通 `call_skill` 的 `prepare_call_skill_lua_invocation_input` 错误包装边界不变。
- 保留 nested `vulcan.call` 的 handler 缺失 runtime error 文本不变。

### 验证记录

- 修改前基线：`cargo test` 通过，224 个测试全部通过。
- 修改前基线：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test vulcan_call -- --nocapture` 通过，1 个 vulcan.call 相关测试通过。
- 修改后：`cargo test runtime_skills -- --nocapture` 通过，3 个 runtime skill 相关测试通过。
- 修改后：`cargo test runlua -- --nocapture` 通过，34 个 runlua 相关测试通过。
- 全量验证：`cargo test` 通过，224 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 范围审核：CodeKit 搜索确认普通 `call_skill` 与 nested `vulcan.call` 各自保留不同 helper 和错误包装边界。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变 `vulcan.call` 参数解析、dispatch entry 查找、luaexec 阻止规则、provider binding 解析、nested context 进入、目标调用或 restore 合并语义。
- `resolve_lua_call_dispatch_handler` 只使用已确认的 `LuaCallDispatchEntry.module_name`，不引入候选 module、fallback handler key 或兼容查找。
- 普通 `call_skill` 的 handler lookup 仍保留底层 Lua 错误；nested `vulcan.call` 仍返回原有 Lua runtime error 文本。
- 修改部分代码审核未发现需要继续自动修复的问题；当前 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续检查 `populate_vulcan_call_for_lua` 中 nested scope guard 创建与 invocation context 准备边界，或回到 `render_help_payload` 的 chunk name 构造边界。

## 2026-07-05 第 49 轮：收敛 vulcan.call nested scope 准备边界

### 问题探索

- 基线验证中，`cargo test` 通过，224 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮沿第 48 轮继续检查 `populate_vulcan_call_for_lua` dispatcher 中 handler 解析后的 nested scope guard 创建与继承调用上下文准备流程。
- CodeKit 结构搜索确认 `LuaNestedCallScopeGuard::new` 原本只在 `populate_vulcan_call_for_lua` dispatcher 内调用一次。
- 已追清 `LuaNestedCallScopeGuard::new` 的事实边界：它捕获外层 `vulcan` 状态，包括 `vulcan.context`、provider skill name、内部执行上下文和文件上下文，并保持 RAII restore 能力。
- 已追清 `previous_invocation_context` 的事实边界：它只从 guard 已捕获的外层 `vulcan.context.request`、`client_budget` 与 `tool_config` 推导嵌套调用应继承的 `LuaInvocationContext`。
- 已确认 luaexec 禁止规则仍必须读取 guard 捕获的 `previous_internal_context`，因此不应被混入 provider binding、handler lookup 或目标调用阶段。
- 已确认 `enter_nested_call` 与 `finish_nested_call` 仍是切换嵌套上下文和合并恢复错误的显式生命周期边界，本轮不改变其职责。

### 执行调整

- 新增 `PreparedLuaNestedCallScope`，用一个结构表达“已捕获的嵌套调用守卫 + 已派生的继承调用上下文”。
- 新增 `prepare_lua_nested_call_scope`，集中封装 `LuaNestedCallScopeGuard::new` 与 `previous_invocation_context` 的连续准备步骤。
- `populate_vulcan_call_for_lua` dispatcher 改为解构 `PreparedLuaNestedCallScope`，后续仍显式执行 luaexec 禁止检查、provider binding 解析、`enter_nested_call`、Lua handler 调用与 `finish_nested_call`。
- 保留 `LuaNestedCallScopeGuard::new` 的 `String` 错误到 `mlua::Error::runtime` 的转换边界，保留 `previous_invocation_context` 的原始 `mlua::Result` 传播方式。

### 验证记录

- 修改前基线：`cargo test` 通过，224 个测试全部通过。
- 修改前基线：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 修改后：`cargo fmt` 通过。
- 修改后：CodeKit 搜索确认 `LuaNestedCallScopeGuard::new` 现在只在 `prepare_lua_nested_call_scope` 内调用，dispatcher 只调用准备 helper。
- 修改后：`cargo test vulcan_call -- --nocapture` 通过，1 个 vulcan.call 相关测试通过。
- 修改后：`cargo test runlua -- --nocapture` 通过，34 个 runlua 相关测试通过。
- 修改后：`cargo test runtime_skills -- --nocapture` 通过，3 个 runtime skill 相关测试通过。
- 全量验证：`cargo test` 通过，224 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变 `vulcan.call` 参数解析、dispatch entry 查找、handler lookup、luaexec 阻止规则、provider binding 解析、nested context 进入、目标调用或 restore 合并语义。
- `prepare_lua_nested_call_scope` 只收敛已确认的“捕获外层状态并派生继承调用上下文”顺序，不引入候选路径、多来源字段、可选 fallback 或兼容分支。
- `finish_nested_call` 仍在 dispatcher 调用点显式执行，helper 没有隐藏嵌套调用的恢复生命周期。
- 修改部分代码审核未发现需要继续自动修复的问题；当前 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续检查 `populate_vulcan_call_for_lua` 中 provider binding 与 `enter_nested_call` 的边界，或回到 `render_help_payload` 的 chunk name 构造边界。

## 2026-07-05 第 50 轮：收敛 vulcan.call nested target 组装边界

### 问题探索

- 基线验证中，`cargo test` 通过，224 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮沿第 49 轮继续检查 `populate_vulcan_call_for_lua` dispatcher 中 provider binding 解析后的 `LuaNestedCallTarget` 组装流程。
- CodeKit 结构搜索确认 `LuaNestedCallTarget` 原本只在 dispatcher 内通过结构体字面量构造一次。
- 已追清 provider binding 来源：`resolve_lua_call_provider_bindings(owner_skill_name, lancedb_host.as_ref(), sqlite_host.as_ref())` 只按嵌套目标所属 skill id 解析 LanceDB 与 SQLite binding。
- 已追清 target 字段来源：展示名、owner skill、local entry、root、skill dir 与 entry path 均来自已解析的 `LuaCallDispatchEntry`，调用上下文来自第 49 轮已收敛的继承 context，数据库 binding 来自唯一 provider binding 解析结果。
- 已确认 `enter_nested_call` 只消费完整 target，并负责填充 request、internal、file、dependency、LanceDB 与 SQLite 子上下文；本轮不改变这些填充顺序和错误传播方式。

### 执行调整

- 新增 `build_lua_nested_call_target`，集中封装 `LuaCallDispatchEntry`、继承 `LuaInvocationContext` 与 `LuaCallProviderBindings` 到 `LuaNestedCallTarget` 的字段映射。
- `populate_vulcan_call_for_lua` dispatcher 不再手写 `LuaNestedCallTarget { ... }`，改为调用 `build_lua_nested_call_target(dispatch_entry, &nested_invocation_context, provider_bindings)`。
- 保留 provider binding 解析位置不变，仍在 luaexec 禁止检查之后、`enter_nested_call` 之前完成。
- 保留 `enter_nested_call`、Lua handler 调用与 `finish_nested_call` 的顺序和错误转换边界不变。

### 验证记录

- 修改前基线：`cargo test` 通过，224 个测试全部通过。
- 修改前基线：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 修改后：`cargo fmt` 通过。
- 修改后：CodeKit 搜索确认 `LuaNestedCallTarget` 现在只在 `build_lua_nested_call_target` 内构造，dispatcher 只调用该 helper。
- 修改后：`cargo test vulcan_call -- --nocapture` 通过，1 个 vulcan.call 相关测试通过。
- 修改后：`cargo test runlua -- --nocapture` 通过，34 个 runlua 相关测试通过。
- 修改后：`cargo test runtime_skills -- --nocapture` 通过，3 个 runtime skill 相关测试通过。
- 全量验证：`cargo test` 通过，224 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变 `vulcan.call` 参数解析、dispatch entry 查找、handler lookup、nested scope 准备、luaexec 阻止规则、provider binding 解析、nested context 进入、目标调用或 restore 合并语义。
- `build_lua_nested_call_target` 只使用已确认的三类输入：`LuaCallDispatchEntry`、继承 `LuaInvocationContext` 与 `LuaCallProviderBindings`，不引入候选字段、路径 fallback 或兼容分支。
- provider binding 仍在进入嵌套调用前解析并移动进 target，避免 `enter_nested_call` 内部重新查找或猜测所属 skill。
- 修改部分代码审核未发现需要继续自动修复的问题；当前 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续检查 `enter_nested_call` 内部多个 `populate_vulcan_*` 调用的上下文载荷边界，或回到 `render_help_payload` 的 chunk name 构造边界。

## 2026-07-05 第 51 轮：收敛 vulcan.call nested internal context 组装边界

### 问题探索

- 基线验证中，`cargo test` 通过，224 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮沿第 50 轮继续检查 `enter_nested_call` 内部 `VulcanInternalExecutionContext` 的组装流程。
- CodeKit 结构搜索确认 internal context 有多类构造点：普通 skill 进入时清空 luaexec，匿名 luaexec 进入时设置 `luaexec_active`，nested `vulcan.call` 进入时从外层 captured context 继承 luaexec 标记。
- 已追清 nested internal context 的字段来源：`tool_name`、`skill_name`、`entry_name` 与 `root_name` 来自 `LuaNestedCallTarget`，`luaexec_active` 与 `luaexec_caller_tool_name` 来自 `LuaNestedCallScopeGuard.previous_internal_context`。
- 已确认 nested call 的 luaexec 标记继承规则不能与普通 skill 或匿名 luaexec 的 context 构造合并，否则会模糊不同执行模式的身份来源。
- 已确认 `enter_nested_call` 后续仍按 request、internal、file、dependency、LanceDB、SQLite 的顺序填充上下文，本轮只处理 internal context 的载荷构造。

### 执行调整

- 新增 `build_lua_nested_internal_execution_context`，集中表达 nested target 身份字段与外层 luaexec 标记的合成规则。
- `enter_nested_call` 改为先构造 `nested_internal_context`，再调用 `populate_vulcan_internal_execution_context(&self.lua, &nested_internal_context)`。
- 保留 `populate_vulcan_request_context` 在 internal context 之前执行，保留 file、dependency、LanceDB 与 SQLite context 填充顺序不变。
- 保留普通 skill、匿名 luaexec 与测试中的 `VulcanInternalExecutionContext` 构造点不变，避免跨执行模式抽象。

### 验证记录

- 修改前基线：`cargo test` 通过，224 个测试全部通过。
- 修改前基线：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 修改后：`cargo fmt` 通过。
- 修改后：CodeKit 搜索确认 `build_lua_nested_internal_execution_context` 只影响 nested `enter_nested_call`，普通 skill 与匿名 luaexec 的 internal context 构造仍保持独立。
- 修改后：`cargo test vulcan_call -- --nocapture` 通过，1 个 vulcan.call 相关测试通过。
- 修改后：`cargo test runlua -- --nocapture` 通过，34 个 runlua 相关测试通过。
- 修改后：`cargo test runtime_skills -- --nocapture` 通过，3 个 runtime skill 相关测试通过。
- 全量验证：`cargo test` 通过，224 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变 `vulcan.call` 参数解析、dispatch entry 查找、handler lookup、nested scope 准备、luaexec 阻止规则、provider binding 解析、file/dependency/provider context 填充、目标调用或 restore 合并语义。
- `build_lua_nested_internal_execution_context` 只使用已确认的 `LuaNestedCallTarget` 与外层 `VulcanInternalExecutionContext`，不引入多来源候选或 fallback。
- nested call 继承 luaexec 标记的规则现在有独立函数表达，后续审阅不需要在 `enter_nested_call` 的多段 context 填充中反复解析该混合规则。
- 修改部分代码审核未发现需要继续自动修复的问题；当前 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续检查 `enter_nested_call` 的 file/dependency/provider context 填充边界，或回到 `render_help_payload` 的 chunk name 构造边界。

## 2026-07-05 第 52 轮：消除 vulcan.call dispatch entry 路径字符串往返

### 问题探索

- 第一次基线 `cargo test` 出现 `process_session` 相关失败，其中一个测试未能在时限内确认子进程退出，后续两个失败来自测试环境锁被 panic 污染。
- 未进入代码修改前先复跑 `cargo test process_session -- --nocapture`，9 个 process_session 相关测试全部通过；随后复跑全量 `cargo test`，224 个测试全部通过，确认该红灯是基线阶段的瞬态进程时序问题。
- 基线 `cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮检查 `LuaCallDispatchEntry` 的路径字段，CodeKit 搜索确认 `owner_skill_dir` 与 `entry_path` 只在 `build_lua_call_dispatch_entries`、`build_lua_nested_call_target` 与 `enter_nested_call` 之间流转。
- 已追清原流程：`skill.dir` 与 `tool_entry_path(&skill.dir, tool)` 原本都是 `PathBuf`，构建 dispatch entry 时被 `to_string_lossy().to_string()` 转为 `String`，进入 nested context 时再通过 `Path::new(target.owner_skill_dir)` 与 `Path::new(target.entry_path)` 转回路径引用。
- 已确认该字符串往返没有面向用户展示或序列化需求，只服务内部 file/dependency context 填充，因此会引入不必要的 lossy 路径风险和类型噪音。

### 执行调整

- 将 `LuaCallDispatchEntry.owner_skill_dir` 与 `LuaCallDispatchEntry.entry_path` 从 `String` 改为 `PathBuf`。
- `build_lua_call_dispatch_entries` 改为保存 `skill.dir.clone()` 与 `tool_entry_path(&skill.dir, tool)` 的原始 `PathBuf`，不再调用 `to_string_lossy()`。
- 将 `LuaNestedCallTarget.owner_skill_dir` 与 `LuaNestedCallTarget.entry_path` 从 `&str` 改为 `&Path`。
- `enter_nested_call` 改为直接向 `populate_vulcan_file_context` 与 `populate_vulcan_dependency_context` 传入 `target.owner_skill_dir` / `target.entry_path`，不再通过 `Path::new` 从字符串重建路径。

### 验证记录

- 修改前基线：第一次 `cargo test` 出现 process_session 瞬态失败；复跑 `cargo test process_session -- --nocapture` 通过，9 个相关测试全部通过。
- 修改前基线：复跑 `cargo test` 通过，224 个测试全部通过。
- 修改前基线：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 修改后：`cargo fmt` 通过。
- 修改后：CodeKit 搜索确认 `LuaCallDispatchEntry` 持有 `PathBuf`，`LuaNestedCallTarget` 持有 `&Path`，目标链路中不再存在 `Path::new(target...)`。
- 修改后：`cargo test vulcan_call -- --nocapture` 通过，1 个 vulcan.call 相关测试通过。
- 修改后：`cargo test runlua -- --nocapture` 通过，34 个 runlua 相关测试通过。
- 修改后：`cargo test runtime_skills -- --nocapture` 通过，3 个 runtime skill 相关测试通过。
- 全量验证：`cargo test` 通过，224 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变 `vulcan.call` 参数解析、dispatch entry 查找、handler lookup、nested scope 准备、luaexec 阻止规则、provider binding 解析、context 填充顺序、目标调用或 restore 合并语义。
- 路径字段现在沿内部链路保持 `PathBuf` / `&Path`，不再提前转成 lossy 文本；这符合长期优化方向，不考虑历史字符串兼容。
- 其它模块中的 `to_string_lossy()` 属于对外文本展示、FFI 字符串、测试断言或不同业务路径，本轮未触碰。
- 修改部分代码审核未发现需要继续自动修复的问题；当前 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续检查 `enter_nested_call` 的 file/dependency/provider context 填充能否用更明确的资源上下文载荷表达，或转向其它模块的路径文本化边界。

## 2026-07-05 第 53 轮：收敛 vulcan.call nested resource context 填充边界

### 问题探索

- 基线验证中，`cargo test` 通过，224 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮沿第 52 轮继续检查 `enter_nested_call` 内部 file、dependency、LanceDB 与 SQLite context 的连续填充流程。
- CodeKit 结构搜索确认 `populate_vulcan_file_context`、`populate_vulcan_dependency_context`、`populate_vulcan_lancedb_context` 与 `populate_vulcan_sqlite_context` 同时出现在 nested call、restore previous state、普通 skill 进入、匿名执行和测试场景中。
- 已追清 nested call 的资源上下文字段来源：file/dependency 路径来自 `LuaNestedCallTarget.owner_skill_dir` 与 `entry_path`，dependency/provider 当前 skill 来自 `target.owner_skill_name`，provider binding 来自 target 内已移动的 LanceDB/SQLite binding。
- 已确认 restore、普通 skill、匿名执行三类模式各有不同来源，不应和 nested call 资源上下文混合抽象。
- 已确认 `enter_nested_call` 的执行顺序仍应保持为 request context、internal context、resource context；本轮只收敛最后一段资源上下文填充。

### 执行调整

- 新增 `populate_lua_nested_resource_contexts`，集中填充 nested call 的 file、dependency、LanceDB 与 SQLite context。
- `populate_lua_nested_resource_contexts` 接收 `LuaNestedCallTarget` 并消费其中 provider bindings，避免额外 clone 或重新查询 provider host。
- `enter_nested_call` 改为保留 request context 与 internal context 的显式填充，然后调用 `populate_lua_nested_resource_contexts(&self.lua, self.host_options.as_ref(), target)`。
- 保留 restore previous state、普通 skill 进入、匿名执行和测试中的资源上下文填充逻辑不变。

### 验证记录

- 修改前基线：`cargo test` 通过，224 个测试全部通过。
- 修改前基线：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 修改后：`cargo fmt` 通过。
- 修改后：CodeKit 搜索确认 nested call 资源上下文填充集中到 `populate_lua_nested_resource_contexts`，其它执行模式保持独立。
- 修改后：`cargo test vulcan_call -- --nocapture` 通过，1 个 vulcan.call 相关测试通过。
- 修改后：`cargo test runlua -- --nocapture` 通过，34 个 runlua 相关测试通过。
- 修改后：`cargo test runtime_skills -- --nocapture` 通过，3 个 runtime skill 相关测试通过。
- 全量验证：`cargo test` 通过，224 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变 `vulcan.call` 参数解析、dispatch entry 查找、handler lookup、nested scope 准备、luaexec 阻止规则、provider binding 解析位置、context 填充顺序、目标调用或 restore 合并语义。
- `populate_lua_nested_resource_contexts` 只使用已确认的 `LuaNestedCallTarget` 与 `LuaRuntimeHostOptions`，不引入候选字段、多来源 fallback 或重新查找 provider binding。
- `enter_nested_call` 现在呈现为 request、internal、resource 三段，后续审阅时不需要在同一个函数里反复解析 provider binding 所有权转移。
- 修改部分代码审核未发现需要继续自动修复的问题；当前 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续检查 restore previous state 的 provider binding 恢复与文件上下文恢复边界，或转向其它模块的路径文本化边界。

## 2026-07-05 第 54 轮：修复 nested restore 丢失 entry_dir 快照的问题

### 问题探索

- 基线验证中，`cargo test` 通过，224 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮检查 `VulcanFileContextSnapshot`，发现其原定义为 `(Option<String>, Option<String>, Option<String>)`，恢复时通过 `.0` 和 `.2` 访问，字段语义不可读。
- CodeKit 结构搜索确认 `capture_vulcan_file_context` 捕获了 `skill_dir`、`entry_dir`、`entry_file` 三个字段，但 `restore_previous_state` 原本只使用 `skill_dir` 和 `entry_file` 调用 `populate_vulcan_file_context`。
- 已追清旧恢复行为：`populate_vulcan_file_context` 会从 `entry_file.parent()` 重新推导 `entry_dir`，因此如果外层上下文中的 `entry_dir` 被 Lua 代码显式设置为不同值，nested call 结束后会丢失捕获到的原值。
- 已确认 `entry_dir` 是 `vulcan.context` 中真实暴露的字段，既然 guard 捕获了它，nested restore 应该原样恢复快照，而不是重新推导。

### 执行调整

- 将 `VulcanFileContextSnapshot` 从三元组类型别名改为命名结构，字段为 `skill_dir`、`entry_dir`、`entry_file`。
- 新增 `restore_vulcan_file_context_field`，按字段把捕获到的可选字符串恢复到 `vulcan.context`，缺失值恢复为 `nil`。
- 新增 `restore_vulcan_file_context_snapshot`，原样恢复 `skill_dir`、`entry_dir` 与 `entry_file` 三个快照字段。
- `restore_previous_state` 改为调用 `restore_vulcan_file_context_snapshot`；dependency context 仍只使用捕获到的 `skill_dir` 和内部上下文中的 skill name 恢复。
- 扩展 `vulcan_call_restores_outer_context_after_nested_failure`：outer skill 在 nested call 前设置自定义 `vulcan.context.entry_dir`，nested failure 后断言该值被原样恢复。

### 验证记录

- 修改前基线：`cargo test` 通过，224 个测试全部通过。
- 修改前基线：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 修改后：`cargo fmt` 通过。
- 修改后：CodeKit 搜索确认 `previous_file_context.0/.2` 元组索引已消失，`entry_dir` 由 `restore_vulcan_file_context_snapshot` 显式恢复。
- 修改后：`cargo test vulcan_call_restores_outer_context_after_nested_failure -- --nocapture` 通过，扩展后的单测通过。
- 修改后：`cargo test vulcan_call -- --nocapture` 通过，1 个 vulcan.call 相关测试通过。
- 修改后：`cargo test runlua -- --nocapture` 通过，34 个 runlua 相关测试通过。
- 修改后：`cargo test runtime_skills -- --nocapture` 通过，3 个 runtime skill 相关测试通过。
- 全量验证：`cargo test` 通过，224 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变 `vulcan.call` 参数解析、dispatch entry 查找、handler lookup、nested scope 准备、luaexec 阻止规则、provider binding 解析位置、进入嵌套上下文的填充顺序、目标调用或 restore 错误合并语义。
- `restore_vulcan_file_context_snapshot` 只使用已捕获的 `VulcanFileContextSnapshot`，不从 `entry_file` 猜测 `entry_dir`，也不引入多来源 fallback。
- 新增测试覆盖了旧逻辑会失败的路径：outer context 的 `entry_dir` 与 `entry_file.parent()` 不一致时，nested failure 后仍应恢复自定义 `entry_dir`。
- 修改部分代码审核未发现需要继续自动修复的问题；当前 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续检查 restore previous state 中 provider binding 的恢复边界，或转向其它模块的路径文本化边界。

## 2026-07-05 第 55 轮：显式化 provider skill marker 捕获错误

### 问题探索

- 基线验证中，`cargo test` 通过，224 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮沿 nested restore 检查 LanceDB / SQLite provider skill marker 的捕获与恢复流程。
- CodeKit 结构搜索确认 `__lancedb_skill_name` 与 `__sqlite_skill_name` 只由 provider context populate 流程写入，并由 `LuaNestedCallScopeGuard::new` 捕获、`restore_previous_state` 恢复。
- 已追清写入协议：`create_provider_context_table` 将缺失的 active skill name 归一化为空字符串 marker，而不是 `nil`。
- 已发现旧捕获逻辑使用 `vulcan.get(...).unwrap_or_default()`，会把缺字段、类型错误或读取失败都吞成空字符串，混淆“无 active skill”与“内部 marker 损坏”。
- 已确认正常初始化 VM 中这两个 marker 应始终是字符串，因此读取失败应作为上下文捕获异常显式返回。

### 执行调整

- 新增 `capture_provider_skill_marker`，按精确 marker key 从 `vulcan` 表读取 provider skill marker。
- `capture_provider_skill_marker` 保留空字符串 marker 作为正常协议值，但读取失败或类型不匹配时返回带字段名的错误。
- `LuaNestedCallScopeGuard::new` 改为通过 `capture_provider_skill_marker(&vulcan, "__lancedb_skill_name")?` 和 `capture_provider_skill_marker(&vulcan, "__sqlite_skill_name")?` 捕获 provider marker。
- 保留 `non_empty_skill_name` 的恢复判断不变，仍由空字符串表示无 active skill。

### 验证记录

- 修改前基线：`cargo test` 通过，224 个测试全部通过。
- 修改前基线：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 修改后：`cargo fmt` 通过。
- 修改后：CodeKit 搜索确认 provider marker 链路中不再使用 `unwrap_or_default()`，剩余 `unwrap_or_default()` 命中均属于其它模块或其它语义。
- 修改后：`cargo test vulcan_call -- --nocapture` 通过，1 个 vulcan.call 相关测试通过。
- 修改后：`cargo test runlua -- --nocapture` 通过，34 个 runlua 相关测试通过。
- 修改后：`cargo test runtime_skills -- --nocapture` 通过，3 个 runtime skill 相关测试通过。
- 全量验证：`cargo test` 通过，224 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变 provider marker 写入协议、provider binding 恢复顺序、`non_empty_skill_name` 的空字符串处理、nested context 进入流程、目标调用或 restore 错误合并语义。
- `capture_provider_skill_marker` 只区分已确认的两类状态：空字符串是合法“无 active skill”标记，读取失败是内部状态异常；不再用默认值掩盖异常。
- 修改部分代码审核未发现需要继续自动修复的问题；当前 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续检查 restore previous state 中 provider binding lookup 的重复 skill name 解析，或转向其它模块的路径文本化边界。

## 2026-07-05 第 56 轮：收敛 nested restore provider 上下文解析

### 问题探索

- 基线验证中，`cargo test` 通过，224 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮沿第 55 轮继续检查 `restore_previous_state` 中 provider binding 恢复边界。
- CodeKit 结构搜索确认重复解析只发生在 `restore_previous_state`：`previous_lancedb_skill_name` 与 `previous_sqlite_skill_name` 先经 `non_empty_skill_name` 用于 `binding_for_skill`，随后又再次经 `non_empty_skill_name` 传给 provider context populate。
- 已追清语义：同一个捕获 marker 应同时决定“是否查 binding”和“写回哪一个 active skill marker”，两者不应在调用点各自重新解析。
- 已确认 `create_provider_context_table` 仍使用空字符串 marker 表示无 active skill，本轮不改变该协议，也不改变 provider context 写回顺序。

### 执行调整

- 新增 `RestoredLuaProviderContexts`，集中表达恢复 provider context 时成对使用的 LanceDB/SQLite binding 与 skill name。
- 新增 `resolve_restored_lua_provider_contexts`，只对捕获到的 LanceDB/SQLite marker 各执行一次 `non_empty_skill_name` 归一化。
- `resolve_restored_lua_provider_contexts` 在 host 与 skill marker 同时存在时解析对应 binding；缺 host 或空 marker 时保留 `None`。
- `restore_previous_state` 改为解构 `RestoredLuaProviderContexts`，并把同一份 skill name 同时用于 binding lookup 结果和 provider marker 写回。

### 验证记录

- 修改前基线：`cargo test` 通过，224 个测试全部通过。
- 修改前基线：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 修改后：`cargo fmt` 通过。
- 修改后：CodeKit 搜索确认 `restore_previous_state` 不再直接对 `previous_lancedb_skill_name` / `previous_sqlite_skill_name` 重复调用 `non_empty_skill_name`，binding lookup 集中在 `resolve_restored_lua_provider_contexts` 内。
- 修改后：`cargo test vulcan_call -- --nocapture` 通过，1 个 vulcan.call 相关测试通过。
- 修改后：`cargo test runlua -- --nocapture` 通过，34 个 runlua 相关测试通过。
- 修改后：`cargo test runtime_skills -- --nocapture` 通过，3 个 runtime skill 相关测试通过。
- 全量验证：`cargo test` 通过，224 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变 provider marker 写入协议、provider binding 查询条件、provider context 写回顺序、nested context 进入流程、目标调用或 restore 错误合并语义。
- `resolve_restored_lua_provider_contexts` 只使用已捕获的 provider marker 与 guard 持有的 provider host，不引入候选 skill、fallback binding 或重新读取 Lua 状态。
- 修改部分代码审核未发现需要继续自动修复的问题；当前 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续检查 nested restore 中 request/client context 的手写恢复段，或转向其它模块的路径文本化边界。

## 2026-07-05 第 57 轮：收敛 nested restore 的 vulcan.context 快照边界

### 问题探索

- 基线验证中，`cargo test` 通过，224 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮沿第 56 轮继续检查 `restore_previous_state` 中 request、client、tool 与 host result 相关 `vulcan.context` 字段的手写恢复段。
- CodeKit 结构搜索确认 `previous_context`、`previous_client_info`、`previous_client_capabilities`、`previous_client_budget`、`previous_tool_config` 与 `previous_host_result` 只属于 `LuaNestedCallScopeGuard` 的捕获、继承 invocation context 和 restore 流程。
- 已追清捕获来源：六个字段均来自同一个 `vulcan.context` 表，分别读取 `request`、`client_info`、`client_capabilities`、`client_budget`、`tool_config` 与 `host_result`。
- 已追清消费边界：nested invocation context 只需要 `request`、`client_budget` 与 `tool_config`；restore 需要原样写回全部六个字段。
- 已确认本轮不改变字段集合、读取错误文本、恢复字段顺序或空 request 对象表示无 request context 的既有约定。

### 执行调整

- 新增 `VulcanContextSnapshot`，集中表达 nested call 需要捕获并恢复的六个 `vulcan.context` 字段。
- 新增 `capture_vulcan_context_snapshot_field` 与 `capture_vulcan_context_snapshot`，统一捕获六个上下文字段并保留精确字段名错误。
- 新增 `restore_vulcan_context_snapshot_field` 与 `restore_vulcan_context_snapshot`，统一恢复六个上下文字段并保留精确字段名错误。
- `LuaNestedCallScopeGuard` 改为持有一个 `previous_context: VulcanContextSnapshot`，替代六个散落的 `previous_*` LuaValue 字段。
- `previous_invocation_context` 改为从 `previous_context.request`、`previous_context.client_budget` 与 `previous_context.tool_config` 读取继承上下文。
- `restore_previous_state` 改为调用 `restore_vulcan_context_snapshot(&self.lua, &self.previous_context)`，去掉手写逐字段恢复段。

### 验证记录

- 修改前基线：`cargo test` 通过，224 个测试全部通过。
- 修改前基线：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 修改后：`cargo fmt` 通过。
- 修改后：CodeKit 搜索确认旧的 `previous_client_*` / `previous_tool_config` / `previous_host_result` 散字段已消失，恢复入口集中到 `restore_vulcan_context_snapshot`。
- 修改后：`cargo test vulcan_call -- --nocapture` 通过，1 个 vulcan.call 相关测试通过。
- 修改后：`cargo test runlua -- --nocapture` 通过，34 个 runlua 相关测试通过。
- 修改后：`cargo test runtime_skills -- --nocapture` 通过，3 个 runtime skill 相关测试通过。
- 全量验证：`cargo test` 通过，224 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变 provider context 恢复、internal context 恢复、file context 恢复、dependency context 恢复、nested context 进入流程、目标调用或 restore 错误合并语义。
- `VulcanContextSnapshot` 只封装已确认的六个 `vulcan.context` 字段，不引入候选字段、fallback 字段或重新推导逻辑。
- `previous_invocation_context` 仍只使用 request、client_budget 与 tool_config，保留空 request 对象表示无 request context 的原约定。
- 修改部分代码审核未发现需要继续自动修复的问题；当前 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续检查 restore previous state 中各类 snapshot restore 的执行顺序，或转向其它模块的路径文本化边界。

## 2026-07-05 第 58 轮：收敛 nested restore provider 写回阶段

### 问题探索

- 基线验证中，`cargo test` 通过，224 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮沿第 57 轮继续检查 `restore_previous_state` 中多段上下文恢复顺序。
- CodeKit 结构搜索确认 provider 恢复已经有 `resolve_restored_lua_provider_contexts` 负责 marker 归一化与 binding lookup，但 `restore_previous_state` 仍直接解构 provider 结果并分别调用 LanceDB / SQLite provider context populate。
- 已追清阶段顺序：`core_state.restore` 必须先恢复根表拓扑，随后恢复 provider 表，再恢复 `vulcan.context` 快照、internal context、file context 和 dependency context。
- 已确认普通 skill 与匿名执行也会填充 provider context，但它们使用不同来源，不应与 nested restore 的捕获 marker 恢复逻辑合并。

### 执行调整

- 新增 `restore_lua_nested_provider_contexts`，集中负责从捕获 provider marker 恢复 LanceDB / SQLite provider 表与 skill marker。
- `restore_lua_nested_provider_contexts` 复用 `resolve_restored_lua_provider_contexts`，不重新读取 Lua 状态，也不引入额外 binding lookup。
- `restore_previous_state` 改为调用 `restore_lua_nested_provider_contexts`，随后继续按原顺序恢复 `vulcan.context`、internal、file 与 dependency context。
- 保留 `resolve_restored_lua_provider_contexts` 的 marker 归一化和 binding 查询条件不变。

### 验证记录

- 修改前基线：`cargo test` 通过，224 个测试全部通过。
- 修改前基线：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 修改后：`cargo fmt` 通过。
- 修改后：CodeKit 搜索确认 nested restore provider 写回集中到 `restore_lua_nested_provider_contexts`，普通 skill 与匿名执行的 provider 填充路径保持独立。
- 修改后：`cargo test vulcan_call -- --nocapture` 通过，1 个 vulcan.call 相关测试通过。
- 修改后：`cargo test runlua -- --nocapture` 通过，34 个 runlua 相关测试通过。
- 修改后：`cargo test runtime_skills -- --nocapture` 通过，3 个 runtime skill 相关测试通过。
- 全量验证：`cargo test` 通过，224 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变 provider marker 写入协议、provider binding 查询条件、provider context 写回顺序、`vulcan.context` 快照恢复、internal/file/dependency 恢复、目标调用或 restore 错误合并语义。
- `restore_lua_nested_provider_contexts` 只使用 guard 捕获的 provider marker 与 provider host，不引入候选 skill、fallback binding 或重新读取 Lua 状态。
- `restore_previous_state` 现在更清楚地呈现为 core、provider、context、internal、file、dependency 六个恢复阶段。
- 修改部分代码审核未发现需要继续自动修复的问题；当前 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续检查 dependency context 恢复中 `skill_dir` 文本转路径的边界，或转向其它模块的路径文本化边界。

## 2026-07-05 第 59 轮：收敛 nested restore dependency 上下文恢复边界

### 问题探索

- 基线验证中，`cargo test` 通过，224 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮沿第 58 轮继续检查 `restore_previous_state` 中 dependency context 恢复边界。
- CodeKit 结构搜索确认 nested restore 的 dependency 恢复只使用两个来源：`previous_file_context.skill_dir` 与 `previous_internal_context.skill_name`。
- 已追清 `previous_file_context.skill_dir` 来源：它来自 `vulcan.context.skill_dir` 快照，当前仍以字符串形式捕获并在依赖恢复时通过 `Path::new` 转回路径引用。
- 已追清 `previous_internal_context.skill_name` 来源：它来自 `vulcan.runtime.internal.skill_name` 快照，用作依赖目录下的 active skill id。
- 已确认本轮不改变依赖路径算法，不改变 `populate_vulcan_dependency_context` 在缺失 skill dir 或 skill id 时清空 deps 的既有行为。

### 执行调整

- 新增 `restore_lua_nested_dependency_context`，集中封装从 file snapshot 与 internal snapshot 恢复 `vulcan.deps` 的流程。
- `restore_lua_nested_dependency_context` 内部仍调用 `populate_vulcan_dependency_context`，并保留 `file_context.skill_dir.as_deref().map(Path::new)` 与 `internal_context.skill_name.as_deref()` 的原有输入规则。
- `restore_previous_state` 改为调用 `restore_lua_nested_dependency_context`，不再手写 skill_dir 字符串转 Path 与 skill name 取值。
- 保留普通 skill、匿名执行、nested enter 和测试中的 dependency context 填充路径不变。

### 验证记录

- 修改前基线：`cargo test` 通过，224 个测试全部通过。
- 修改前基线：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 修改后：`cargo fmt` 通过。
- 修改后：CodeKit 搜索确认 `restore_previous_state` 不再手写 `previous_file_context.skill_dir` 到 `Path::new` 的转换，该逻辑集中在 `restore_lua_nested_dependency_context` 内。
- 修改后：`cargo test vulcan_call -- --nocapture` 通过，1 个 vulcan.call 相关测试通过。
- 修改后：`cargo test runlua -- --nocapture` 通过，34 个 runlua 相关测试通过。
- 修改后：`cargo test runtime_skills -- --nocapture` 通过，3 个 runtime skill 相关测试通过。
- 全量验证：`cargo test` 通过，224 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变 dependency root 推导算法、deps 清空条件、provider/context/internal/file 恢复顺序、目标调用或 restore 错误合并语义。
- `restore_lua_nested_dependency_context` 只使用已确认的 `VulcanFileContextSnapshot` 与 `VulcanInternalExecutionContext`，不引入候选 skill id、fallback path 或重新读取 Lua 状态。
- `restore_previous_state` 现在保持 core、provider、context、internal、file、dependency 六个恢复阶段，并且每个阶段都有单独边界函数。
- 修改部分代码审核未发现需要继续自动修复的问题；当前 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续检查 file snapshot 的字符串路径是否应进一步改为结构化路径快照，或转向其它模块的路径文本化边界。

## 2026-07-05 第 60 轮：收敛 nested guard 外层状态快照边界

### 问题探索

- 基线验证中，`cargo test` 通过，224 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮沿第 59 轮继续检查 `LuaNestedCallScopeGuard` 捕获外层状态的边界。
- CodeKit 结构搜索确认 guard 中的 `core_state`、`previous_context`、provider marker、`previous_internal_context` 与 `previous_file_context` 都只属于同一次 nested call 的外层状态快照。
- 已追清消费流程：`previous_invocation_context` 从外层 `vulcan.context` 快照派生继承调用上下文；`enter_nested_call` 从外层 internal context 保留 luaexec 标记；`restore_previous_state` 依次恢复 core、provider、context、internal、file 与 dependency；dispatcher 只需要读取外层 internal context 做 luaexec 禁止规则校验。
- 已确认本轮不改变捕获字段集合、捕获顺序、恢复阶段顺序、luaexec 校验规则或目标调用流程，只收敛状态所有权表达。

### 执行调整

- 新增 `LuaNestedOuterStateSnapshot`，集中表达单次 nested call guard 持有的完整外层状态快照。
- 新增 `capture_lua_nested_outer_state`，把 core state、context snapshot、provider marker、internal context 与 file context 的捕获流程收束到同一个入口。
- `LuaNestedCallScopeGuard` 改为持有 `previous_state: LuaNestedOuterStateSnapshot`，删除分散的 `previous_*` 状态字段。
- 新增 `previous_internal_context()` 只读访问器，供 luaexec 校验与 nested internal context 派生使用，避免 dispatcher 直接触达 guard 内部字段。
- `previous_invocation_context`、`enter_nested_call` 与 `restore_previous_state` 均改为从 `previous_state` 读取已捕获外层状态。

### 验证记录

- 修改前基线：`cargo test` 通过，224 个测试全部通过。
- 修改前基线：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 修改后：`cargo fmt` 通过。
- 修改后：搜索确认旧的 `previous_context`、`previous_lancedb_skill_name`、`previous_sqlite_skill_name`、`previous_internal_context` 与 `previous_file_context` 分散字段已经消失。
- 修改后：`cargo test vulcan_call -- --nocapture` 通过，1 个 vulcan.call 相关测试通过。
- 修改后：`cargo test runlua -- --nocapture` 通过，34 个 runlua 相关测试通过。
- 修改后：`cargo test runtime_skills -- --nocapture` 通过，3 个 runtime skill 相关测试通过。
- 全量验证：`cargo test` 通过，224 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变 nested call 的参数解析、dispatch entry 查找、handler lookup、provider binding 解析、context 填充、目标调用或 restore 错误合并语义。
- `capture_lua_nested_outer_state` 使用的字段来源均为已追清的 Lua VM 当前外层状态，不引入候选字段、fallback 字段、多来源猜测或重新派生逻辑。
- `restore_previous_state` 仍保持 core、provider、context、internal、file、dependency 六个恢复阶段，并且每个阶段继续使用前几轮收敛出的独立边界函数。
- dispatcher 现在只能通过 `previous_internal_context()` 读取 luaexec 校验所需的外层 internal context，guard 的快照所有权更集中。
- 修改部分代码审核未发现需要继续自动修复的问题；当前 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续检查 file snapshot 的字符串路径建模，或转向其它模块中路径、配置与运行时状态边界不清晰的问题。

## 2026-07-05 第 61 轮：拆分 file snapshot 的 Lua 文本与宿主路径语义

### 问题探索

- 基线验证中，`cargo test` 通过，224 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮沿第 60 轮遗留线索检查 `VulcanFileContextSnapshot` 中路径字段仍以 `Option<String>` 保存的问题。
- CodeKit 结构搜索确认 file snapshot 只在 nested guard 捕获与恢复链路中流转：`capture_vulcan_file_context` 从 `vulcan.context.skill_dir`、`entry_dir`、`entry_file` 读取；`restore_vulcan_file_context_snapshot` 原样写回 Lua 字段；`restore_lua_nested_dependency_context` 只消费 `skill_dir` 来恢复 `vulcan.deps`。
- 已追清 Lua 暴露层约束：`populate_vulcan_file_context` 写入的是 `render_host_visible_path` 生成的 Lua 可见字符串；第 54 轮测试要求自定义 `entry_dir` 必须按捕获文本原样恢复，不能从 `entry_file` 或 `PathBuf` 重新渲染。
- 已追清内部消费约束：dependency 恢复需要的是宿主路径引用，旧逻辑在消费点通过 `file_context.skill_dir.as_deref().map(Path::new)` 临时从字符串解释路径，导致 Lua 原始文本语义与宿主路径语义混在同一个字段上。

### 执行调整

- 新增 `VulcanFileContextPath`，同时保存从 Lua 捕获的精确 `lua_text` 与由该文本派生出的 `PathBuf`。
- `VulcanFileContextSnapshot` 的 `skill_dir`、`entry_dir`、`entry_file` 从 `Option<String>` 改为 `Option<VulcanFileContextPath>`。
- 新增 `capture_vulcan_file_context_path`，集中读取单个 `vulcan.context` 文件字段，并保留“字段只能是字符串或 nil”的现有约定。
- `restore_vulcan_file_context_field` 改为接收 `Option<&VulcanFileContextPath>`，恢复 Lua 字段时只使用 `lua_text()`，确保自定义文本不被路径渲染改写。
- `restore_lua_nested_dependency_context` 改为使用 `VulcanFileContextPath::path`，不再在恢复点通过 `Path::new` 重新解释 `skill_dir` 字符串。

### 验证记录

- 修改前基线：`cargo test` 通过，224 个测试全部通过。
- 修改前基线：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 修改后：`cargo fmt` 通过。
- 修改后：搜索确认旧的 `file_context.skill_dir.as_deref().map(Path::new)` 已消失。
- 修改后：`cargo test vulcan_call_restores_outer_context_after_nested_failure -- --nocapture` 通过，覆盖自定义 `entry_dir` 精确恢复路径。
- 修改后：`cargo test vulcan_call -- --nocapture` 通过，1 个 vulcan.call 相关测试通过。
- 修改后：`cargo test runlua -- --nocapture` 通过，34 个 runlua 相关测试通过。
- 修改后：`cargo test runtime_skills -- --nocapture` 通过，3 个 runtime skill 相关测试通过。
- 全量验证：`cargo test` 通过，224 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变 `vulcan.context` 三个文件字段的 Lua 可见值、nil 清理语义、dependency root 推导算法、nested restore 阶段顺序或目标调用流程。
- `VulcanFileContextPath` 只由已确认的单个 Lua 字符串构造，不引入候选路径、fallback 字段、多来源兼容或额外 Lua 状态读取。
- Lua 字段恢复路径只使用 `lua_text()`，dependency 恢复路径只使用 `path()`，两种语义在类型层面分开。
- 修改部分代码审核未发现需要继续自动修复的问题；当前 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续检查同一文件中其他 `display().to_string()` 写入运行时结构的路径文本化边界，或转向运行时配置路径的规范化与展示语义。

## 2026-07-05 第 62 轮：统一 host-visible skill_dir 路径渲染边界

### 问题探索

- 基线验证中，`cargo test` 通过，224 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮沿第 61 轮遗留线索检查 `engine.rs` 中直接使用 `.display().to_string()` 写入运行时结构的路径输出。
- CodeKit 结构搜索确认候选集中在三类对外结构：技能生命周期事件 `RuntimeSkillLifecycleEvent.skill_dir`、入口描述 `RuntimeEntryDescriptor.skill_dir`、帮助摘要与详情 `RuntimeSkillHelpDescriptor.skill_dir` / `RuntimeHelpDetail.skill_dir`。
- 已追清字段归属：这些 `skill_dir` 字段都是宿主可见的物理 skill 目录文本，不参与内部路径运算；同文件已有 `render_host_visible_path` 专门用于宿主可见运行时表面，并负责去除 Windows verbatim 前缀。
- 已确认测试中已有 `normalize_host_visible_path_text_*` 覆盖 host-visible 路径归一化函数本身，因此本轮重点是把对外结构字段接入既有渲染边界。

### 执行调整

- 将 `mutate_skill_state_and_reload` 中 blocked、failed、completed 生命周期事件的 `skill_dir` 改为 `render_host_visible_path(&resolved_instance.actual_dir)`。
- 将 `uninstall_skill_and_reload_in_root` 中 blocked、failed、completed 生命周期事件的 `skill_dir` 改为 `render_host_visible_path(&resolved_instance.actual_dir)`。
- 将 `apply_skill_request_in_root` 完成事件中的 `skill_dir` 改为 `render_host_visible_path(&instance.actual_dir)`。
- 将 `list_entries` 生成的 `RuntimeEntryDescriptor.skill_dir` 改为 `render_host_visible_path(&skill.dir)`。
- 将 `list_skill_help` 与 `render_skill_help_detail` 生成的帮助结构 `skill_dir` 改为 `render_host_visible_path(&skill.dir)`。

### 验证记录

- 修改前基线：`cargo test` 通过，224 个测试全部通过。
- 修改前基线：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 修改后：`cargo fmt` 通过。
- 修改后：搜索确认 `engine.rs` 中不再存在 `.display().to_string()`；唯一残留的 `display()` 是 ROOT 层拒绝操作错误消息中的诊断文本，不属于本轮对外结构字段范围。
- 修改后：`cargo test list_entries -- --nocapture` 通过，3 个入口列表相关测试通过。
- 修改后：`cargo test runtime_skills -- --nocapture` 通过，3 个 runtime skill 相关测试通过。
- 全量验证：`cargo test` 通过，224 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变技能生命周期状态、权限过滤、reload 流程、entry registry delta 计算、帮助内容渲染或 descriptor 字段集合。
- 所有修改点只把已确认的物理 skill 目录路径从直接 `display().to_string()` 改为既有 `render_host_visible_path`，不引入新路径来源、候选字段或 fallback 逻辑。
- 诊断错误消息中的 `root_instance.actual_dir.display()` 本轮保留不动，因为它属于错误文本边界，而非结构化 `skill_dir` 字段。
- 修改部分代码审核未发现需要继续自动修复的问题；当前 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续检查其它文件中 host-visible 路径字段是否仍使用 `to_string_lossy()` 或 `display()` 绕过统一渲染函数。

## 2026-07-05 第 63 轮：抽取共享 host-visible path 渲染并接入数据库绑定上下文

### 问题探索

- 基线验证中，`cargo test` 通过，224 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮沿第 62 轮遗留线索检查其它文件中直接用 `to_string_lossy().to_string()` 输出宿主可见路径字段的问题。
- CodeKit 结构搜索确认 `src/host/database.rs` 的 `RuntimeDatabaseBindingContext` 会进入 `RuntimeSqliteProviderRequest` / `RuntimeLanceDbProviderRequest`，并通过标准 FFI provider callback 暴露给宿主。
- 已追清字段归属：`space_root`、`skill_dir`、`default_database_path` 都是宿主可见数据库绑定上下文路径文本；旧逻辑在 `build_runtime_database_binding_plan` 中直接用 `to_string_lossy().to_string()` 生成。
- 已确认 `engine.rs` 中原有 `render_host_visible_path` 是私有函数，不能被 host/database 复用；复制函数会继续制造多份路径渲染规则，因此需要抽成 runtime 级 crate 内共享工具。

### 执行调整

- 新增 `src/runtime/path.rs`，迁入 `normalize_host_visible_path_text` 与 `render_host_visible_path`，并保持 `pub(crate)` 可见性，不扩大公开 API。
- `src/runtime/mod.rs` 新增 `pub(crate) mod path`，供 runtime 与 host 内部模块复用。
- `engine.rs` 改为从 `crate::runtime::path::render_host_visible_path` 引用共享函数，并保留本地 `render_log_friendly_path` 作为日志语义包装。
- `host/database.rs` 的 `build_runtime_database_binding_plan` 改为使用 `render_host_visible_path` 生成 `space_root`、`skill_dir` 与 `default_database_path`。
- 更新 `database_binding_plan_resolves_provider_paths` 的期望值，让测试也校验同一 host-visible 渲染边界。
- 自动修复 clippy 发现的抽取后未使用导入：`normalize_host_visible_path_text` 测试改为直接引用 `crate::runtime::path::normalize_host_visible_path_text`。

### 验证记录

- 修改前基线：`cargo test` 通过，224 个测试全部通过。
- 修改前基线：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test database_binding_plan -- --nocapture` 通过，1 个数据库绑定计划测试通过。
- 修改后：`cargo test database -- --nocapture` 通过，4 个 database 相关测试通过。
- 修改后：`cargo test provider -- --nocapture` 通过，5 个 provider 相关测试通过。
- 修改后：`cargo test normalize_host_visible_path_text -- --nocapture` 通过，2 个路径归一化测试通过。
- 自动修复后：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 全量验证：`cargo test` 通过，224 个测试全部通过。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变数据库 sidecar 目录推导、provider storage 目录结构、SQLite/LanceDB 默认路径规则、provider request 字段集合或 FFI 数据结构。
- `runtime::path` 是 crate 内共享模块，避免 host/database 复制 engine 私有路径渲染逻辑；当前没有新增公开 API。
- `build_runtime_database_binding_plan` 仍只使用已确认的 `skill_dir`、`database_dir_name` 与 `database_kind` 推导路径，不引入候选根目录、fallback 路径或额外状态读取。
- 修改部分代码审核发现并修复了抽取后的未使用导入问题；当前 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续检查 `host/controller.rs`、provider status JSON 或 FFI 测试中的路径文本化边界。

## 2026-07-05 第 64 轮：统一 space controller 启动路径渲染

### 问题探索

- 基线验证中，`cargo test` 通过，224 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮沿第 63 轮遗留线索检查 `src/host/controller.rs` 中空间控制器相关路径文本化边界。
- CodeKit 结构搜索确认 `LuaRuntimeSpaceControllerBridge::new` 只有一处直接 `to_string_lossy().to_string()`，用于把 `host_options.space_controller.executable_path` 写入 `ControllerClientConfig.spawn_executable`。
- 已追清字段来源：`executable_path` 来自宿主配置的可选外部 controller 可执行文件路径。
- 已追清字段消费：`spawn_executable` 传给 `vldb_controller_client::ControllerClientConfig`，用于自动启动外部控制器进程，是宿主侧进程启动路径文本，不参与 Lua 内部路径运算。
- 已确认第 63 轮新增的 `runtime::path::render_host_visible_path` 正是当前项目统一的宿主可见路径渲染边界，本轮应复用而不是新增 controller 私有规则。

### 执行调整

- `host/controller.rs` 引入 `crate::runtime::path::render_host_visible_path`。
- `ControllerClientConfig.spawn_executable` 从 `path.to_string_lossy().to_string()` 改为 `render_host_visible_path(path)`。
- 保留 endpoint、auto-spawn、process mode、超时、连接、attach binding 与 controller ID 生成逻辑不变。

### 验证记录

- 修改前基线：`cargo test` 通过，224 个测试全部通过。
- 修改前基线：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 修改后：`cargo fmt` 通过。
- 修改后：搜索确认 `src/host/controller.rs` 中不再存在直接 `to_string_lossy().to_string()`。
- 修改后：`cargo test controller -- --nocapture` 通过，3 个 controller 相关测试通过。
- 修改后：`cargo test bridge_runtime -- --nocapture` 通过，2 个 bridge runtime 相关测试通过。
- 修改后：`cargo test normalize_host_visible_path_text -- --nocapture` 通过，2 个共享路径归一化测试通过。
- 全量验证：`cargo test` 通过，224 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变 controller 客户端注册、连接、自动启动开关、进程模式映射、binding attach 或 controller binding id 生成语义。
- `spawn_executable` 仍只来自已确认的 `host_options.space_controller.executable_path`，不引入候选可执行文件、fallback 路径或环境搜索。
- 修改部分代码审核未发现需要继续自动修复的问题；当前 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续检查 provider status JSON 中的 `library_path`、`space_root`、`default_database_path` 等宿主可见路径字段是否仍绕过统一渲染函数。

## 2026-07-05 第 65 轮：统一 provider status library_path 渲染边界

### 问题探索

- 基线验证中，`cargo test` 通过，224 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮沿第 64 轮遗留线索检查 provider status JSON 中的动态库路径输出。
- 源码搜索确认 `src/providers/lancedb.rs` 与 `src/providers/sqlite.rs` 的 `status_json` 均直接从已加载 API 表的 `library_path: PathBuf` 生成 `"library_path"` 字段。
- 已追清字段来源：`LoadedLanceDbApi.library_path` 与 `LoadedSqliteApi.library_path` 均由动态库加载流程保存，属于 provider 状态 JSON 暴露给宿主诊断面的真实文件路径。
- 已区分同文件内其它 `to_string_lossy().to_string()`：它们均位于 `CStr::from_ptr(ptr)` 后，用于 FFI 错误消息或返回字符串解码，并非路径渲染边界，本轮不应改动。
- 已确认 `space_root` 与 `default_database_path` 来自上一轮已统一处理过的数据库绑定上下文，本轮不新增候选路径或重复渲染逻辑。

### 执行调整

- `src/providers/lancedb.rs` 引入 `crate::runtime::path::render_host_visible_path`。
- `src/providers/sqlite.rs` 引入 `crate::runtime::path::render_host_visible_path`。
- LanceDB provider 的 `status_json.library_path` 从 `api.library_path.to_string_lossy().to_string()` 改为 `render_host_visible_path(&api.library_path)`。
- SQLite provider 的 `status_json.library_path` 从 `api.library_path.to_string_lossy().to_string()` 改为 `render_host_visible_path(&api.library_path)`。
- 保留 FFI 字符串解码、动态库加载、provider mode、status JSON 字段集合和数据库绑定上下文行为不变。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：搜索确认 `library_path.*to_string_lossy` 已无命中，两个 provider 的 `library_path` 均改为调用 `render_host_visible_path`。
- 修改后：搜索确认剩余 `to_string_lossy().to_string()` 均与 `CStr::from_ptr(ptr)` 绑定，属于 FFI 字符串解码路径。
- 修改后：`cargo test provider -- --nocapture` 通过，5 个 provider 相关测试通过。
- 修改后：`cargo test sqlite -- --nocapture` 通过，1 个 SQLite 相关测试通过。
- 修改后：`cargo test lancedb -- --nocapture` 通过，当前过滤条件无 LanceDB 命名测试命中。
- 修改后：`cargo test normalize_host_visible_path_text -- --nocapture` 通过，2 个路径归一化测试通过。
- 全量验证：`cargo test` 通过，224 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变动态库加载流程、FFI API 表结构、错误消息解码、provider request 执行流程、status JSON 字段集合或 provider 绑定上下文。
- `library_path` 只来自已确认的 `LoadedLanceDbApi.library_path` / `LoadedSqliteApi.library_path`，没有引入候选字段、fallback 路径或多来源兼容逻辑。
- 修改部分代码审核未发现需要继续自动修复的问题；当前 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续检查 runtime/FFI 测试与示例代码中是否还存在宿主可见路径绕过 `runtime::path` 统一渲染的输出边界。

## 2026-07-05 第 66 轮：拆分 luaskills-debug 同步结果与输出路径渲染

### 问题探索

- 基线验证中，`cargo test` 通过，224 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮沿第 65 轮遗留线索检查 runtime/FFI 测试与示例代码中的路径文本化边界。
- 搜索确认 `src/bin/luaskills-debug.rs` 在 `sync`、`inspect` 与 `call` 输出中多处直接使用 `display().to_string()` 生成 `runtime_root`、`source_skill_path` 与 `synced_skill_path`。
- 已追清字段来源：这些字段都来自 `DebugCliCommand` 解析出的路径，经 `absolutize_path`、同步流程和 `PreparedDebugRuntime` 保存为 `PathBuf`。
- 已追清字段消费：这些路径最终进入 `DebugSyncOutput`、`DebugInspectOutput` 与 `DebugCallOutput`，由 pretty 或 JSON 输出暴露给调试 CLI 用户，属于宿主可见输出表面。
- 进一步发现 `resolve_debug_target` 会把 `sync_debug_skill` 返回的 `DebugSyncOutput.source_skill_path` 字符串再转回 `PathBuf`，导致公开输出 DTO 反向参与内部执行路径。
- 已确认 `runtime::path` 当前为 crate 内部模块，`src/bin/luaskills-debug.rs` 作为独立二进制 target 不能直接访问该模块；因此需要提供最小公开入口，而不是在 debug CLI 中复制路径渲染规则。

### 执行调整

- `src/runtime/mod.rs` 通过 `#[doc(hidden)] pub use path::render_host_visible_path` 暴露最小路径渲染入口，保留 `path` 模块本身为 `pub(crate)`。
- `src/runtime/path.rs` 将 `render_host_visible_path` 可见性从 `pub(crate)` 放宽为 `pub`，供 runtime re-export 使用；`normalize_host_visible_path_text` 仍保持 `pub(crate)`。
- `src/bin/luaskills-debug.rs` 引入 `luaskills::runtime::render_host_visible_path`，统一渲染 debug CLI 的结构化路径输出。
- 新增 `DebugSkillSyncResult`，以 `PathBuf` 保存同步后的内部事实：`runtime_root`、`source_skill_path` 与 `synced_skill_path`。
- `sync_debug_skill` 改为返回 `DebugSkillSyncResult`，不再直接返回可序列化输出 DTO。
- 新增 `build_sync_output`，专门将 `DebugSkillSyncResult` 渲染为 `DebugSyncOutput`。
- `resolve_debug_target` 改为直接消费 `DebugSkillSyncResult.source_skill_path`，删除 `PathBuf::from(sync_output.source_skill_path)` 的输出字符串反灌内部路径逻辑。
- `inspect` 与 `call` 输出中的路径字段统一改为 `render_host_visible_path`。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：搜索确认 `src/bin/luaskills-debug.rs` 中不再存在 `display().to_string()`、`to_string_lossy().to_string()` 或 `PathBuf::from(sync...)`。
- 修改后：搜索确认 `runtime` 只通过 `#[doc(hidden)] pub use path::render_host_visible_path` 暴露最小入口，`path` 模块仍为 crate 内部模块。
- 修改后：`cargo test luaskills_debug -- --nocapture` 通过，但过滤条件未命中具体测试用例。
- 修改后：`cargo test --bin luaskills-debug -- --nocapture` 通过，7 个 debug 二进制测试全部通过。
- 全量验证：`cargo test` 通过，224 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变 CLI 参数解析、skill 同步目录结构、manifest 绑定、runtime root 布局、engine 加载、tool 解析、pretty/JSON/content 输出字段集合或错误消息语义。
- `DebugSkillSyncResult` 只保存已确认的同步事实，不引入候选路径、fallback 路径或额外环境搜索。
- 公开输出 DTO 现在只在输出构建边界生成，内部执行流继续使用原始 `PathBuf`，展示渲染规则不再反向影响 `prepare_debug_runtime`。
- `render_host_visible_path` 的公开入口通过 `runtime` 模块文档隐藏 re-export 提供，避免把整个 `runtime::path` 模块变成公开 API。
- 修改部分代码审核未发现需要继续自动修复的问题；当前 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续检查 `src/ffi_standard/tests.rs` 中大量测试路径到 C 字符串的转换是否存在可抽取的测试辅助函数，或转向 `src/dependency/manager.rs` 的版本目录名字符串化边界。

## 2026-07-05 第 67 轮：严格解析本地依赖版本目录名

### 问题探索

- 基线验证中，`cargo test` 通过，224 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮沿第 66 轮遗留线索检查 `src/dependency/manager.rs` 的 `entry.file_name().to_string_lossy().to_string()`。
- 已追清字段来源：`version_component` 来自 `local_unversioned_dependency_probe_requests` 扫描已安装依赖目录时获取的版本目录名。
- 已追清字段消费：该目录名会作为 `version` 传入 `local_dependency_probe_request_variants_for_root`，再进入 `ResolvedDependencyRequest.version` 与 export 模板解析。
- 已确认该字段不是宿主可见路径输出，因此不应套用 `render_host_visible_path`；它是依赖版本语义字段。
- 问题边界是有损 UTF-8 转换会把非法 OS 目录名伪造成带替换字符的版本号，继续参与本地依赖探测和 export 模板解析。

### 执行调整

- `src/dependency/manager.rs` 引入 `std::ffi::OsString`，用于显式处理 OS 目录名。
- 新增 `local_dependency_version_component_from_file_name`，将本地依赖版本目录名严格解析为 UTF-8 字符串。
- `local_unversioned_dependency_probe_requests` 改为调用该辅助函数；解析失败的目录通过 `?` 在 `filter_map` 中跳过。
- 保留依赖根目录扫描、平台目录检测、版本候选、tag 候选、export 模板解析、下载解析和安装流程不变。
- `src/dependency/manager/tests.rs` 新增 UTF-8 目录名被接受的测试。
- `src/dependency/manager/tests.rs` 新增 Unix 平台非法 UTF-8 目录名被拒绝的条件测试。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：搜索确认 `src/dependency/manager.rs` 中不再存在 `file_name().to_string_lossy().to_string()`。
- 修改后：`cargo test local_dependency_version_component -- --nocapture` 通过，当前 Windows 环境命中 1 个辅助函数测试。
- 修改后：`cargo test dependency -- --nocapture` 通过，13 个 dependency 相关测试通过。
- 全量验证：`cargo test` 通过，225 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变依赖安装目录结构、依赖作用域、网络下载开关、GitHub release 解析、URL 下载、export 模板字段集合或检测状态语义。
- `version_component` 仍只来自已确认的本地版本目录名，不引入候选字段、fallback 版本或额外 manifest 读取。
- 非 UTF-8 版本目录名现在会被显式跳过，避免有损字符串继续污染版本号和 export 模板。
- Unix 非 UTF-8 用例在当前 Windows 环境不会执行，但已通过条件编译覆盖跨平台风险；当前 Windows 环境执行了 UTF-8 正常路径测试。
- 修改部分代码审核未发现需要继续自动修复的问题；当前 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续检查 `src/ffi_standard/tests.rs` 中重复的测试路径到 C 字符串转换是否可以抽取为严格辅助函数，或继续搜索其它 `to_string_lossy` 残留的语义边界。

## 2026-07-05 第 68 轮：抽取标准 FFI 测试 host options 与路径 CString 夹具

### 问题探索

- 基线验证中，`cargo test` 通过，225 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮沿第 67 轮遗留线索检查 `src/ffi_standard/tests.rs` 中重复的路径到 `CString` 转换。
- 搜索确认多个标准 FFI 测试重复构造 `temp_dir`、`resources_dir`、`lua_packages_dir`、`host_provided_tool_root` 与 `host_provided_ffi_root` 的 `CString`。
- 已追清字段来源：这些路径均来自每个测试创建的 `temp_root` 及其固定子目录。
- 已追清字段消费：这些 `CString` 的裸指针进入 `FfiLuaRuntimeHostOptions` 或 `FfiLuaRuntimeHostOptionsV2`，随后传给标准 FFI engine 创建函数。
- 已确认这些 `CString` 必须由测试侧结构持有并至少存活到 FFI 调用结束，不能简单返回裸指针 options。
- 已确认测试专属差异包括 root 名称、skills root、tool name、runtime_root V2 字段与 skill_config_file_path；这些差异应保留在各自测试中。

### 执行调整

- `src/ffi_standard/tests.rs` 引入 `crate::runtime::path::render_host_visible_path` 和 `std::path::Path`。
- 新增 `ffi_test_path_cstring`，统一用 `render_host_visible_path` 将测试路径转为 FFI `CString`。
- 新增 `empty_ffi_runtime_host_options`，集中维护标准 FFI host options 的空基准值。
- 新增 `FfiStandardHostOptionsFixture`，持有标准 FFI host options 所需的目录 `CString`，并生成借用型 `FfiLuaRuntimeHostOptions`。
- 为 `FfiStandardHostOptionsFixture` 提供 `new` 与 `with_skill_config_file_path`，覆盖普通测试与 skill config 测试的配置路径差异。
- 将 load/list、call_skill、run_lua、skill_config、disable/enable 等标准 FFI 测试改为复用 `FfiStandardHostOptionsFixture`。
- 将 V2 runtime_root 测试改为复用 `ffi_test_path_cstring` 与 `empty_ffi_runtime_host_options`。
- 保留测试目录创建、skill.yaml 内容、Lua 脚本内容、FFI 调用顺序、断言和清理逻辑不变。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：搜索确认 `src/ffi_standard/tests.rs` 中不再存在 `display().to_string()` 或 `CString::new(...display())`。
- 修改后：`cargo test ffi_standard -- --nocapture` 通过，11 个标准 FFI 相关测试通过。
- 全量验证：`cargo test` 通过，225 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变标准 FFI 类型定义、C ABI 函数、engine 创建参数语义、测试 skill 内容、借用 buffer 流程、skill config API 或 lifecycle API。
- `FfiStandardHostOptionsFixture` 只持有已确认的测试目录字符串，不引入候选目录、fallback 路径或跨测试共享状态。
- `ffi_test_path_cstring` 统一使用已有宿主可见路径渲染边界，避免测试路径继续绕过 `runtime::path`。
- 修改部分代码审核未发现需要继续自动修复的问题；当前 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续搜索其它 `display().to_string()` 或 `to_string_lossy().to_string()` 残留，优先区分诊断文本、路径输出、目录组件与 FFI 字符串解码四类语义。

## 2026-07-05 第 69 轮：避免 Windows process.which 候选路径有损字符串化

### 问题探索

- 基线验证中，`cargo test` 通过，225 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮重新全局搜索 `display().to_string()` 与 `to_string_lossy().to_string()` 残留。
- 已分类 provider 中的 `CStr::from_ptr(ptr).to_string_lossy().to_string()` 为 FFI C 字符串解码，不属于路径渲染或内部路径运算问题。
- 已分类 `path.basename` 与 `path.stem` 中的 `file_name` / `file_stem` 转换为 Lua path API 的路径组件文本输出，本轮不混入进程查找修复。
- 已追清 `vulcan_process_candidate_paths` 的执行流：`vulcan.process.which` 根据显式路径或 PATH 目录构造基础路径，再在 Windows 下按 PATHEXT 扩展候选，最终用 `is_vulcan_process_executable` 检测并将命中路径渲染给 Lua。
- 问题边界是 Windows 候选生成用 `base.as_os_str().to_string_lossy().to_string()` 把内部 `Path` 有损转字符串，再通过 `PathBuf::from(format!(...))` 转回路径，属于内部路径运算中的不必要字符串往返。

### 执行调整

- `src/runtime/engine.rs` 中的 Windows `vulcan_process_candidate_paths` 删除 `base_text` 字符串中间态。
- PATHEXT 候选改为从 `base.as_os_str().to_os_string()` 派生，并通过 `OsString::push` 追加扩展名，最后构造 `PathBuf`。
- 保留基础路径本身作为第一个候选、已有扩展名时直接返回、PATHEXT 解析顺序、可执行检测和 Lua 输出渲染逻辑不变。
- `src/runtime/engine/tests.rs` 新增 Windows 条件测试 `vulcan_process_candidate_paths_appends_windows_pathexts`，覆盖 `.CMD;.EXE` 顺序和追加候选路径。
- Windows 专用测试导入使用 `#[cfg(windows)]` 收窄，避免非 Windows 下出现未使用导入。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：搜索确认 `vulcan_process_candidate_paths` 中不再存在 `base_text`、`as_os_str().to_string_lossy().to_string()` 或 `PathBuf::from(format!(...))` 的候选生成链路。
- 修改后：`cargo test vulcan_process_candidate_paths_appends_windows_pathexts -- --nocapture` 通过，1 个 Windows PATHEXT 候选测试通过。
- 修改后：`cargo test process_which -- --nocapture` 通过，2 个 process.which 相关测试通过。
- 全量验证：`cargo test` 通过，226 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变 `vulcan.process.which` 的 PATH 搜索顺序、显式路径判断、PATHEXT 归一化规则、可执行文件检测、Lua 返回值类型或错误传播语义。
- 候选路径仍只由已确认的 `base` 路径和 PATHEXT 项派生，不引入候选目录、fallback 扩展名或额外环境变量读取。
- 内部路径候选生成现在保留 OS 字符串语义，避免非 UTF 路径在进入 `is_vulcan_process_executable` 前被有损替换。
- 修改部分代码审核未发现需要继续自动修复的问题；当前 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续处理 `src/runtime/engine/tests.rs` 中四处测试 JSON 参数路径的 `to_string_lossy().to_string()`，或评估 Lua `path.basename` / `path.stem` 的组件文本是否需要显式 UTF-8 策略。

## 2026-07-05 第 70 轮：统一 engine 测试 JSON 路径参数渲染

### 问题探索

- 基线验证中，`cargo test` 通过，226 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮沿第 69 轮遗留线索检查 `src/runtime/engine/tests.rs` 中四处 `path.to_string_lossy().to_string()`。
- 已追清字段来源：这些 `path` 均由测试通过 `std::env::temp_dir()` 加唯一文件名构造。
- 已追清字段消费：这些字符串只作为 `execute_runlua_request_json_inline` 的 JSON `args.path` 传给 Lua 测试代码，用于 managed IO 和默认编码测试。
- 已确认这些路径不是生产路径计算，也不是 FFI 字符串解码；它们是测试侧宿主可见路径输入。
- 已确认测试模块已经使用 `render_host_visible_path`，因此应复用现有边界，而不是继续手写有损转换。

### 执行调整

- 将 `execute_runlua_request_inline_uses_managed_io_open` 的 `args.path` 改为 `render_host_visible_path(&path)`。
- 将 `execute_runlua_request_inline_uses_managed_io_default_input` 的 `args.path` 改为 `render_host_visible_path(&path)`。
- 将 `execute_runlua_request_inline_uses_managed_io_default_output` 的 `args.path` 改为 `render_host_visible_path(&path)`。
- 将 `execute_runlua_request_inline_uses_host_default_text_encoding` 的 `args.path` 改为 `render_host_visible_path(&path)`。
- 保留测试文件创建、Lua 代码、managed IO 调用、默认编码配置、断言与清理逻辑不变。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：搜索确认 `src/runtime/engine/tests.rs` 中不再存在 `display().to_string()` 或 `to_string_lossy().to_string()`。
- 修改后：`cargo test managed_io -- --nocapture` 通过，13 个 managed IO 相关测试通过。
- 修改后：`cargo test host_default_encoding -- --nocapture` 过滤条件未命中测试用例，随后改用实际测试名验证。
- 修改后：`cargo test execute_runlua_request_inline_uses_host_default_text_encoding -- --nocapture` 通过，1 个默认编码测试通过。
- 全量验证：`cargo test` 通过，226 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变 managed IO 实现、runlua 请求解析、Lua 测试代码、默认编码处理、文件读写行为或测试清理策略。
- `args.path` 仍只来自已确认的测试临时文件路径，不引入候选字段、fallback 路径或额外状态读取。
- 测试 JSON 路径现在统一使用宿主可见路径渲染边界，避免测试构造绕过 `runtime::path`。
- 修改部分代码审核未发现需要继续自动修复的问题；当前 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可评估 Lua `path.basename` / `path.stem` 中的组件文本是否需要显式 UTF-8 策略；provider 中的 `CStr::from_ptr(...).to_string_lossy()` 仍属于 FFI 字符串解码，不应与路径渲染混改。

## 2026-07-05 第 71 轮：显式化 Lua path 组件 UTF-8 渲染策略

### 问题探索

- 基线验证中，`cargo test` 通过，226 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮沿第 70 轮遗留线索检查 `src/runtime/engine.rs` 中 `path.basename` 与 `path.stem` 的 `to_string_lossy().to_string()`。
- 已追清字段来源：`path.basename`、`path.stem` 与 `path.extname` 均先通过 `require_path_arg` 获取 Lua 输入路径文本。
- 已追清 `require_path_arg`：它调用 `require_string_arg` 返回 Rust `String`，再通过 `validate_path_text` 做统一路径文本校验，因此这些 Lua path helper 的输入已经是 UTF-8 文本。
- 已追清字段消费：这些结果只作为 Lua `vulcan.path.*` API 的组件字符串返回给调用方。
- 已确认该问题不是宿主可见完整路径渲染，也不是内部路径运算；它是路径组件文本输出策略不显式的问题。
- 旧实现用 `to_string_lossy()` 隐式表达“可以有损替换”，与已确认的 UTF-8 输入约束不一致，也会掩盖未来如果出现非 UTF 组件时的错误边界。

### 执行调整

- `src/runtime/engine.rs` 引入 `std::ffi::OsStr`，用于显式接收 `std::path` 返回的 OS 组件。
- 新增 `render_vulcan_path_component`，统一渲染 Lua-facing path helper 的可选路径组件。
- `render_vulcan_path_component` 对缺失组件返回空字符串；对非 UTF-8 组件返回显式 Lua runtime error；对有效组件返回原始 UTF-8 文本。
- `path.basename` 改为通过 `render_vulcan_path_component(Path::new(&path).file_name(), "path.basename")` 输出组件。
- `path.stem` 改为通过 `render_vulcan_path_component(Path::new(&path).file_stem(), "path.stem")` 输出组件。
- `path.extname` 改为先通过 `render_vulcan_path_component(Path::new(&path).extension(), "path.extname")` 获取扩展名，再在非空时添加 `.` 前缀。
- 保留 `dirname`、`join`、`normalize`、`is_abs`、路径校验、Lua API 名称和既有返回字段语义不变。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：搜索确认 `src/runtime/engine.rs` 中不再存在 `to_string_lossy().to_string()` 或 `display().to_string()`。
- 修改后：`cargo test execute_runlua_request_inline_supports_vulcan_path_helpers -- --nocapture` 通过，1 个 `vulcan.path.*` helper 测试通过。
- 全量验证：`cargo test` 通过，226 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 空白审核：`git diff --check` 通过。
- 全局残留搜索确认：当前 `to_string_lossy().to_string()` 只剩 provider 中的 `CStr::from_ptr(...)` FFI 字符串解码路径。

### 代码审核与遗留事项

- 本轮没有改变 Lua path helper 的 API 名称、参数校验入口、成功返回格式、`extname` 的点号前缀规则、路径归一化规则或现有测试期望。
- `render_vulcan_path_component` 只消费已确认的 `std::path` 组件，不引入候选字段、fallback 组件或额外路径解析。
- 非 UTF-8 组件现在会显式报错，而不是被 `to_string_lossy` 静默替换，边界更清楚。
- 修改部分代码审核未发现需要继续自动修复的问题；当前 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续追 provider 的 FFI 字符串解码是否需要抽取为通用 C 字符串读取辅助函数；该方向需先确认两个 provider 的 ownership/free 规则是否完全一致。

## 2026-07-05 第 72 轮：抽取 provider 非空 FFI C 字符串解码边界

### 问题探索

- 基线验证中，`cargo test` 通过，226 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮沿第 71 轮遗留线索检查 provider 中剩余的 `CStr::from_ptr(...).to_string_lossy().to_string()`。
- 已追清 LanceDB 的错误字符串流程：`take_last_error_message` 从 `last_error_message` 读取指针，空指针返回固定兜底错误文本，非空指针解码为 Rust `String`，随后调用 `clear_last_error`。
- 已追清 LanceDB 的返回字符串流程：`take_owned_string` 对空指针转入 `take_last_error_message`，非空指针解码后调用 `string_free` 释放动态库分配的字符串。
- 已追清 SQLite 的错误字符串流程：`take_last_error_message` 与 LanceDB 一致，保留 SQLite 专属的空错误文本与 `clear_last_error` 调用。
- 已追清 SQLite 的返回字符串流程：`take_owned_string` 负责必填字符串的空指针错误转换和释放，`take_optional_string` 负责可选字符串的空指针 `None` 语义和非空释放。
- 已确认两个 provider 真正重复的是“非空 C 字符串指针解码为 Rust String”的动作；空指针语义、last-error 清理和动态库字符串释放仍属于各 provider 方法的所有权边界。

### 执行调整

- `src/providers/mod.rs` 新增 `decode_non_null_ffi_c_string`，集中表达 provider FFI 的非空 C 字符串解码策略。
- `decode_non_null_ffi_c_string` 使用双语安全文档明确调用方必须保证指针非空、有效，并且解码完成前不会被释放。
- `src/providers/lancedb.rs` 移除局部 `CStr` 导入，改为通过 `decode_non_null_ffi_c_string` 解码 `take_last_error_message` 与 `take_owned_string` 中的非空指针。
- `src/providers/sqlite.rs` 移除局部 `CStr` 导入，改为通过 `decode_non_null_ffi_c_string` 解码 `take_last_error_message`、`take_owned_string` 与 `take_optional_string` 中的非空指针。
- 保留 `clear_last_error`、`string_free`、空指针错误转换、可选字符串 `None`、provider 请求执行和动态库 API 表结构不变。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：搜索确认 `src/providers/lancedb.rs` 与 `src/providers/sqlite.rs` 中不再直接调用 `CStr::from_ptr`。
- 修改后：搜索确认 `CStr::from_ptr` 只剩 `src/providers/mod.rs` 的 `decode_non_null_ffi_c_string` 单一入口。
- 修改后：搜索确认 `src` 与 `examples` 中已无 `display().to_string()` 或 `to_string_lossy().to_string()` 残留。
- 修改后：`cargo test provider -- --nocapture` 通过，5 个 provider 相关测试通过。
- 修改后：`cargo test sqlite -- --nocapture` 通过，1 个 SQLite 相关测试通过。
- 全量验证：`cargo test` 通过，226 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变 provider 的空指针处理、last-error 生命周期、动态库字符串释放、状态 JSON、请求分发或 provider mode 选择语义。
- `decode_non_null_ffi_c_string` 只处理已由调用方确认非空的 C 字符串指针，不引入候选字段、fallback 文本、多来源兼容或额外释放策略。
- 修改部分代码审核未发现需要继续自动修复的问题；当前 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可从更宽的坏味道搜索开始，例如检查剩余 `to_string_lossy().into_owned()` 是否还有非 FFI 场景，或转向 provider/engine 中重复的控制器调用样板。

## 2026-07-05 第 73 轮：收紧 managed io.write stdout 文本解码边界

### 问题探索

- 基线延续第 72 轮闭环状态：`cargo test` 通过，226 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮从剩余 `to_string_lossy` 搜索中定位到 `src/runtime/managed_io.rs` 的 `lua_value_to_display_text`。
- 已追清调用入口：`install_managed_io_compat` 注册 Lua `io.write`，`write_to_compat_output` 在没有 `io.output` 当前输出文件时调用 `lua_value_to_display_text`。
- 已追清重定向路径：如果存在当前输出文件，`io.write` 走 `ManagedIoFile::write_values`，再进入 `lua_value_to_output_bytes`；文本模式已经要求 Lua 字符串必须是有效 UTF-8，二进制模式保留原始字节。
- 已追清 stdout 路径：没有当前输出文件时，`write_to_compat_output` 将所有 Lua 值转换为文本片段并通过 `runtime_logging::info("[LuaSkill:stdout] ...")` 写日志。
- 问题边界是 stdout 日志属于文本语义，但旧实现对 Lua 字符串调用 `to_string_lossy()`，会把非法 UTF-8 静默替换成替代字符。
- 已确认该问题不是文件二进制写入、不是路径渲染、不是编码转换选项；它只影响未重定向的 `io.write` stdout 日志文本。

### 执行调整

- `src/runtime/managed_io.rs` 将 `lua_value_to_display_text` 的说明改为严格 UTF-8 stdout 文本转换。
- `lua_value_to_display_text` 的 Lua 字符串分支改为调用 `to_str()`，非法 UTF-8 返回显式 Lua runtime error。
- 新错误消息为 `io.write string must be valid UTF-8 when no output file is selected`，明确该限制只属于 stdout 输出路径。
- 保留整数、浮点数、布尔值、`nil` 与其他 Lua 值的展示文本转换不变。
- `src/runtime/managed_io/tests.rs` 新增 `managed_io_display_text_rejects_invalid_utf8_string`，直接覆盖非法 Lua 字节字符串进入 stdout 文本转换时的错误边界。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test managed_io_display_text_rejects_invalid_utf8_string -- --nocapture` 通过，新增非法 UTF-8 用例通过。
- 修改后：`cargo test managed_io -- --nocapture` 通过，14 个 managed_io 相关测试通过。
- 首次静态验证：`cargo clippy --all-targets -- -D warnings` 发现新增测试中 `create_string(&[0xff])` 存在 needless borrow。
- 自动修复：将 `create_string(&[0xff])` 改为 `create_string([0xff])`，并重新执行 `cargo fmt`。
- 修复后：`cargo test managed_io_display_text_rejects_invalid_utf8_string -- --nocapture` 通过。
- 全量验证：`cargo test` 通过，227 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 空白审核：`git diff --check` 通过。
- 目标残留搜索确认：`lua_value_to_display_text` 中不再存在 `to_string_lossy`，仅保留严格 `to_str()` 转换。

### 代码审核与遗留事项

- 本轮没有改变 `vulcan.io.open`、`vulcan.io.write_text`、文件句柄 `file:write`、二进制模式、默认编码、`io.output` 重定向或 `popen` 行为。
- stdout 日志边界现在与文件写入文本模式保持一致：文本必须是有效 UTF-8，二进制字节应走文件二进制写入路径而不是 stdout 文本日志。
- 修改部分代码审核发现的 clippy 问题已自动修复；当前 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续处理 `src/runtime/managed_io/tests.rs` 中测试路径传给 Lua 字符串时的 `path.to_string_lossy()`，或回到 `src/runtime/config.rs` 的 Windows 锁身份归一化逻辑继续深挖。

## 2026-07-05 第 74 轮：统一 managed_io 测试路径 Lua 字面量渲染

### 问题探索

- 基线延续第 73 轮闭环状态：`cargo test` 通过，227 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮沿第 73 轮遗留线索检查 `src/runtime/managed_io/tests.rs` 中的 `path.to_string_lossy()`。
- 已追清字段来源：这些 `path` 均由测试通过 `std::env::temp_dir()` 加固定测试文件名构造，用作 managed IO 测试文件。
- 已追清字段消费：这些路径被 `lua_quote` 写入 Lua 脚本文本，再传给 `vio.read_text`、`io.open`、`io.input`、`io.output` 等 managed IO API。
- 已追清运行时入口：managed IO API 接收路径后进入 `require_path_arg`，该函数要求 Lua 字符串是严格 UTF-8，并执行统一路径文本校验。
- 已确认测试里的 `path.to_string_lossy()` 不是内部路径运算，也不是 FFI 解码；它是宿主路径进入 Lua runtime 表面的测试侧渲染边界。
- 问题边界是测试绕过了已有 `render_host_visible_path`，继续手写有损路径文本化，导致测试构造与生产/其他测试中的宿主可见路径渲染规则不一致。

### 执行调整

- `src/runtime/managed_io/tests.rs` 引入 `crate::runtime::render_host_visible_path`，复用统一宿主可见路径渲染入口。
- 新增 `lua_path_literal`，将文件系统路径先渲染为宿主可见路径文本，再通过 `lua_quote` 生成 Lua 字符串字面量。
- `lua_path_literal` 提供双语函数说明、参数说明和返回值说明，明确它只服务 managed IO 测试的 Lua 路径字面量。
- 将 6 处 `lua_quote(&path.to_string_lossy())` 改为 `lua_path_literal(&path)`。
- 保留测试文件创建、Lua 脚本文本、断言、清理逻辑和 managed IO 运行时行为不变。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test managed_io -- --nocapture` 通过，14 个 managed_io 相关测试通过。
- 修改后：搜索确认 `src/runtime/managed_io/tests.rs` 中不再存在 `path.to_string_lossy()`。
- 全量验证：`cargo test` 通过，227 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变 managed IO 的路径校验、文件读写、编码处理、Lua API 名称、兼容 `io` 表行为或错误传播语义。
- 测试路径仍只来自已确认的临时文件路径，不引入候选路径、fallback 路径或额外状态读取。
- 修改部分代码审核未发现需要继续自动修复的问题；当前 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续检查 `src/runtime/engine/tests.rs` 中剩余的测试路径有损渲染，或回到 `src/runtime/config.rs` 的 Windows 锁身份归一化逻辑继续深挖。

## 2026-07-05 第 75 轮：统一 engine 测试中的宿主路径文本期望

### 问题探索

- 基线延续第 74 轮闭环状态：`cargo test` 通过，227 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮检查 `src/runtime/engine/tests.rs` 中剩余的 `to_string_lossy()`，定位到模型回调 caller 断言和 system runtime lease 的 `cwd` JSON 输入。
- 已追清模型 caller 字段来源：生产代码在构造 `RuntimeModelCaller.skill_dir` 时使用 `render_host_visible_path(&resolved_instance.actual_dir)`。
- 已追清模型 caller 字段消费：两个测试分别捕获 embed/LLM 回调请求，并断言 `captured.caller.skill_dir` 等于当前测试 skill 目录。
- 已确认旧测试期望使用 `skill_dir.to_string_lossy().as_ref()`，与生产字段的宿主可见渲染策略不一致。
- 已追清 system lease `cwd` 来源：测试构造 `explicit_cwd` 后把它放入 `create_system_runtime_lease_json` 的 JSON 请求。
- 已追清 system lease `cwd` 消费：运行时创建租约后返回 `created["cwd"]`，该测试已经用 `render_host_visible_path(&explicit_cwd)` 作为返回值期望。
- 问题边界是测试输入/期望仍绕过统一宿主可见路径渲染，不涉及运行时路径解析或租约逻辑本身。

### 执行调整

- 将 embed 模型回调测试中的 `captured.caller.skill_dir` 期望改为 `render_host_visible_path(&skill_dir)`。
- 将 LLM 模型回调测试中的 `captured.caller.skill_dir` 期望改为 `render_host_visible_path(&skill_dir)`。
- 将 system runtime lease 测试 JSON 请求中的 `"cwd"` 从 `explicit_cwd.to_string_lossy()` 改为 `render_host_visible_path(&explicit_cwd)`。
- 保留模型回调注册、捕获请求、client context、lease 创建、返回值断言和清理逻辑不变。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test dispatches_registered_callback_with_context -- --nocapture` 通过，2 个模型回调上下文测试通过。
- 修改后：`cargo test system_runtime_lease_preserves_explicit_cwd_override -- --nocapture` 通过，1 个 system lease cwd 测试通过。
- 修改后：搜索确认 `src/runtime/engine/tests.rs` 中不再存在 `to_string_lossy()` 或 `display().to_string()`。
- 全量验证：`cargo test` 通过，227 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变模型回调请求结构、caller 字段生产逻辑、system runtime lease 创建流程、cwd 校验、system_lua_lib 处理或返回 JSON 结构。
- 三处路径仍只来自已确认的测试 skill 目录或测试临时 cwd，不引入候选路径、fallback 路径或多来源兼容。
- 修改部分代码审核未发现需要继续自动修复的问题；当前 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可回到 `src/runtime/config.rs` 的 Windows lock identity 归一化，或继续全局搜索其它非展示场景的 `to_string_lossy()`。

## 2026-07-05 第 76 轮：收紧 skill config Windows 锁身份归一化

### 问题探索

- 基线延续第 75 轮闭环状态：`cargo test` 通过，227 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮回到 `src/runtime/config.rs`，检查生产代码中的 `normalize_windows_skill_config_lock_identity_path`。
- 已追清调用链：`SkillConfigStore::with_document_read` 与 `with_document_mut` 先解析有效配置文件路径，再调用 `shared_skill_config_path_lock` 获取进程级共享锁。
- 已追清锁键来源：`shared_skill_config_path_lock` 调用 `skill_config_lock_key`，该函数先把相对路径固定到当前工作目录，再做词法路径折叠。
- 已追清 Windows 身份归一化：词法折叠后的路径进入 `normalize_windows_skill_config_lock_identity_path`，旧实现通过 `path.to_string_lossy()` 去除 verbatim 前缀并整体小写。
- 已确认 `skill_config_lock_key` 本身已经返回 `Result<PathBuf, String>`，因此 Windows 路径文本无法严格表示时可以显式失败，不需要用有损替换继续生成锁键。
- 已追清测试残留：Windows alias 测试只用 `canonical_path.to_string_lossy().into_owned()` 构造盘符小写与 verbatim 前缀别名，属于测试侧宿主路径渲染边界。

### 执行调整

- `skill_config_lock_key` 将词法路径折叠结果保存为 `normalized_path`，再传入平台身份归一化函数，明确两个边界。
- `normalize_skill_config_lock_identity_path` 改为返回 `Result<PathBuf, String>`；非 Windows 分支返回 `Ok(path.to_path_buf())`。
- `normalize_windows_skill_config_lock_identity_path` 改为返回 `Result<PathBuf, String>`，并用 `path.to_str()` 替代 `to_string_lossy()`。
- Windows 分支在路径文本不是有效 UTF-8 时返回 `skill config lock path must be valid UTF-8 on Windows`，避免生成被替代字符污染的锁键。
- Windows verbatim drive 前缀、UNC verbatim 前缀去除和大小写归一化规则保持不变。
- Windows alias 测试中的 `canonical_text` 改为通过 `crate::runtime::path::render_host_visible_path(&canonical_path)` 构造。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test shared_lock -- --nocapture` 通过，2 个 shared lock 相关测试通过。
- 修改后：`cargo test skill_config_store_normalizes_windows_aliases_for_shared_lock -- --nocapture` 通过，1 个 Windows alias 测试通过。
- 修改后：`cargo test config -- --nocapture` 通过，25 个 config 相关测试通过。
- 修改后：搜索确认 `src/runtime/config.rs` 中不再存在 `to_string_lossy()` 或 `display().to_string()`。
- 全量验证：`cargo test` 通过，227 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变 skill config 文件读写、默认路径解析、显式路径固定、词法 `.`/`..` 折叠、Windows verbatim 前缀处理或共享锁注册表结构。
- 锁键仍只来自已确认的有效配置文件路径，不引入候选路径、fallback 路径或多来源兼容。
- Windows 非 UTF-8 路径现在会显式报错，而不是通过 `to_string_lossy` 静默生成可能冲突的锁身份。
- 修改部分代码审核未发现需要继续自动修复的问题；当前 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可重新全局搜索 `to_string_lossy`，区分 FFI 解码、宿主展示路径、测试构造和内部身份键四类场景。

## 2026-07-05 第 77 轮：收紧 tar.gz 归档条目路径匹配文本边界

### 问题探索

- 基线延续第 76 轮闭环状态：`cargo test` 通过，227 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮全局搜索 `to_string_lossy` 后，定位到 `src/download/archive.rs` 的 tar.gz 导出安装流程。
- 已追清调用入口：依赖安装流程调用 `install_downloaded_payload`，当 `DependencyArchiveType::TarGz` 时进入 `install_from_tar_gz_archive`。
- 已追清字段来源：`archive_entry.path()` 从 tar 条目头读取归档内路径。
- 已追清字段消费：tar 条目路径只用于 `archive_entry_matches_export`，与 manifest 中声明的 `export.archive_path` 做直接匹配或剥离一层顶层目录后匹配。
- 已追清目标写入路径：实际写入位置来自 `join_relative_target(install_root, &export.target_path)`，不是来自 tar 条目路径。
- 旧实现使用 `archive_entry.path()?.to_string_lossy().replace('\\', "/")`，会把非 UTF-8 归档条目路径静默替换后继续参与 export 匹配。
- 已确认 zip 路径匹配入口本身使用 `&str` entry name；tar 分支需要单独把 `Path` 边界显式转为严格 UTF-8 文本。

### 执行调整

- `install_from_tar_gz_archive` 将 `archive_entry.path()` 结果先转为拥有的 `PathBuf`，再调用新的 `normalize_tar_entry_match_path`。
- 新增 `normalize_tar_entry_match_path`，用 `Path::to_str()` 严格读取 tar entry path 文本，非法 UTF-8 返回 `tar.gz entry path must be valid UTF-8`。
- `normalize_tar_entry_match_path` 在 UTF-8 成功后复用 `normalize_archive_entry_match_path`，保留原有反斜杠转正斜杠和首尾斜杠裁剪规则。
- 新增 `normalize_tar_entry_match_path_uses_utf8_forward_slash_text` 单元测试，覆盖 tar entry path 的正斜杠匹配表示。
- 保留 export 匹配规则、剥离一层顶层目录规则、目标路径生成、文件写入、可执行位设置和缺失 export 报错逻辑不变。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test normalize_tar_entry_match_path_uses_utf8_forward_slash_text -- --nocapture` 通过，新增 tar entry path helper 测试通过。
- 修改后：`cargo test dependency -- --nocapture` 通过，13 个 dependency 相关测试通过。
- 修改后：`cargo test archive -- --nocapture` 通过，1 个 archive 相关测试通过。
- 修改后：搜索确认 `src/download/archive.rs` 中不再存在 `to_string_lossy()`。
- 全量验证：`cargo test` 通过，228 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变下载缓存、归档类型选择、zip 安装流程、raw 安装流程、dependency manifest 解析或 export 模板替换逻辑。
- tar entry path 仍只用于匹配已确认的 `export.archive_path`，不会派生目标写入路径，也不引入 fallback 匹配策略。
- 非 UTF-8 tar entry path 现在会显式报错，而不是通过替代字符参与导出匹配。
- 修改部分代码审核未发现需要继续自动修复的问题；当前 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续处理 `src/ffi_standard.rs` 中剩余的 `to_string_lossy`，或切换到锁、缓存、线程同步等非字符串类坏味道。

## 2026-07-05 第 78 轮：收紧标准 FFI string_clone 的 UTF-8 契约

### 问题探索

- 基线延续第 77 轮闭环状态：`cargo test` 通过，228 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮检查剩余 `src/ffi_standard.rs` 中的 `luaskills_ffi_string_clone`。
- 已追清文档契约：标准 FFI 与 JSON FFI 文档均把宿主字符串和返回缓冲描述为 UTF-8 文本。
- 已追清内部解析函数：`parse_required_string`、`parse_required_string_allow_empty`、`parse_optional_string` 与字符串数组解析均使用 `CStr::to_str()` 严格校验 UTF-8。
- 已追清函数用途：`luaskills_ffi_string_clone` 将宿主拥有的 C 字符串复制为 LuaSkills 拥有的堆字符串，供 callback/helper 返回值使用。
- 已确认该函数没有 `error_out` 输出槽；因此非法 UTF-8 无法返回错误缓冲，只能通过返回空指针显式表示克隆失败。
- 旧实现使用 `CStr::from_ptr(value).to_string_lossy().to_string()`，会把非法 UTF-8 静默替换后交给宿主继续释放和消费。

### 执行调整

- `luaskills_ffi_string_clone` 改为对非空输入调用 `CStr::to_str()`，有效 UTF-8 直接克隆，非法 UTF-8 返回 null。
- 保留 null 输入返回空字符串的既有语义。
- 为 `luaskills_ffi_string_clone` 补充双语参数、返回值和安全说明，明确输入必须是 null 或 NUL 结尾 UTF-8 字符串。
- `include/luaskills_json_ffi.h` 中 `luaskills_ffi_string_clone` 注释补充“输入必须为空指针或有效 UTF-8；非法 UTF-8 返回空指针”。
- `src/ffi_standard/tests.rs` 新增 `ffi_string_clone_copies_valid_utf8_text`，覆盖有效 UTF-8 克隆和释放。
- `src/ffi_standard/tests.rs` 新增 `ffi_string_clone_rejects_invalid_utf8_text`，覆盖非法 UTF-8 返回 null。
- 测试中显式引用 `crate::ffi::luaskills_ffi_string_free`，保持当前 string clone 与 string free 的 ABI 归属事实不变。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test ffi_string_clone -- --nocapture` 通过，2 个 string clone 测试通过。
- 修改后：`cargo test ffi_standard -- --nocapture` 通过，13 个标准 FFI 相关测试通过。
- 修改后：搜索确认 `src/ffi_standard.rs` 中不再存在 `to_string_lossy()`。
- 全量验证：`cargo test` 通过，230 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变标准 FFI 的结构体布局、函数签名、释放函数归属、buffer clone、bytes clone、callback 注册或 JSON FFI 请求解析逻辑。
- `luaskills_ffi_string_clone` 仍只消费已确认的单个 C 字符串指针，不引入 fallback 文本、多来源兼容或错误缓冲伪造。
- 非法 UTF-8 现在通过 null 返回值显式暴露，不再生成包含替代字符的 LuaSkills-owned 字符串。
- 修改部分代码审核未发现需要继续自动修复的问题；当前 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可重新全局搜索 `to_string_lossy`；预期只剩 `runtime::path` 统一宿主路径渲染入口和 provider FFI 解码入口。

## 2026-07-05 第 79 轮：恢复共享工具缓存的锁 poison 后可用性

### 问题探索

- 基线延续第 78 轮闭环状态：`cargo test` 通过，230 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮转向非字符串类坏味道，检查 `src/runtime/cache.rs` 中共享工具缓存的同步边界。
- 已追清生产入口：Lua 侧 `vulcan.cache.put`、`vulcan.cache.get`、`vulcan.cache.delete` 在 `src/runtime/engine.rs` 中调用 `global_tool_cache()` 后进入 `SharedToolCache::create/get/delete`。
- 已追清数据性质：共享工具缓存只保存短生命周期 JSON 值，用于分页和工具状态交接；缓存条目由工具名、JSON value、创建序号和过期时间组成。
- 已追清锁使用点：`create` 获取写锁后清理过期条目、插入新条目并执行容量淘汰；`get` 先读锁快路径命中，再写锁清理过期条目并二次确认；`delete` 写锁清理后按工具命名空间删除。
- 旧实现对 `RwLock::read/write` 直接调用 `expect("tool cache poisoned")`，一旦某个缓存操作持锁期间 panic，后续任意缓存读写都会再次 panic。
- 长期优化判断：缓存是可再生的进程内临时状态，锁 poison 只代表曾经有线程持锁 panic，不应让后续 Lua cache API 持续崩溃；应显式恢复内部 guard 并继续执行既有 TTL、容量和命名空间规则。

### 执行调整

- `src/runtime/cache.rs` 引入 `RwLockReadGuard` 与 `RwLockWriteGuard`，用于表达缓存内部锁 helper 的返回类型。
- 新增 `SharedToolCache::read_store`，读取缓存存储时通过 `PoisonError::into_inner` 恢复被标记为 poisoned 的读锁 guard。
- 新增 `SharedToolCache::write_store`，写入缓存存储时通过 `PoisonError::into_inner` 恢复被标记为 poisoned 的写锁 guard。
- `create`、`get` 读快路径、`get` 写清理路径和 `delete` 全部改用 `read_store` 或 `write_store`，移除生产路径中的 `expect("tool cache poisoned")`。
- 新增 `cache_recovers_after_poisoned_write_lock` 单元测试，先在持有内部写锁时制造并捕获 panic，再验证 `create/get/delete` 均可继续正常工作。
- 保持缓存 ID 生成、TTL 解析、过期清理、容量淘汰、工具命名空间隔离和全局缓存初始化行为不变。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test cache_recovers_after_poisoned_write_lock` 通过，新增 poison 恢复测试通过。
- 修改后：`cargo test cache` 通过，5 个缓存相关测试全部通过。
- 全量验证：`cargo test` 通过，231 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 空白审核：`git diff --check` 通过。
- 残留搜索：`src/runtime/cache.rs` 中生产路径不再存在 `expect("tool cache poisoned")`，只保留测试中用于新建缓存初始写锁断言的 `expect("initial tool cache write lock")`。

### 代码审核与遗留事项

- 本轮没有改变 Lua cache API 形状、返回值结构、错误消息、JSON value 克隆、TTL 上限、容量上限或工具命名空间隔离规则。
- poison 恢复只发生在已确认的单一缓存存储锁上，不引入多来源数据兜底、候选字段兼容或模糊路径判断。
- 修改部分代码审核未发现需要继续自动修复的问题；当前 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续检查其它生产锁、回调注册表、数据库句柄和主机控制器中是否存在 panic 型同步边界。

## 2026-07-05 第 80 轮：恢复 Lua VM 池锁 poison 后的借还能力

### 问题探索

- 基线延续第 79 轮闭环状态：`cargo test` 通过，231 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮沿同步边界继续扫描生产代码中的 `expect/unwrap/panic`，定位到 `src/runtime/engine.rs` 的 `LuaVmPool`。
- 已追清池结构：`LuaVmPool` 由 `Mutex<LuaVmPoolState>`、`Condvar` 和池配置组成；状态内保存 `available: Vec<LuaVm>` 与 `total_count`。
- 已追清主池入口：`LuaEngine::acquire_vm` 调用 `self.pool.acquire(|| self.create_vm())`，供普通 `run_lua` 和 skill 调用复用 Lua VM。
- 已追清 runlua 池入口：`acquire_runlua_vm` 克隆 `runlua_pool` 后调用 `runlua_pool.acquire(...)`，供 `vulcan.runtime.lua.exec` 的隔离执行复用独立 VM。
- 已追清预热入口：`load_skills_from_roots` 在重建入口注册表后分别对主池和 runlua 池调用 `prewarm`。
- 已追清借还状态机：`acquire` 先清理空闲 VM，再优先弹出可用 VM；未达上限时预占 `total_count` 并创建新 VM；达到上限时通过 `Condvar::wait` 等待归还；`LuaVmLease::drop` 调用 `release` 归还，`discard` 则减少 `total_count`。
- 旧实现对池状态 `Mutex::lock()` 与 `Condvar::wait()` 直接 `unwrap()`，一旦某个持锁操作 panic，后续预热、借出、归还、废弃或计数读取都会继续 panic。
- 长期优化判断：Lua VM 池状态机的共享状态本身仍是唯一事实来源；锁 poison 只是 Rust 对“曾经持锁 panic”的标记，不应让所有后续 VM 池操作永久失效。

### 执行调整

- `src/runtime/engine.rs` 引入 `MutexGuard`，用于表达 Lua VM 池状态锁 helper 的返回类型。
- 新增 `LuaVmPool::lock_state`，通过 `PoisonError::into_inner` 恢复被标记为 poisoned 的池状态锁。
- 新增 `LuaVmPool::wait_state`，在 `Condvar::wait` 唤醒重新拿锁时同样恢复 poisoned 状态锁。
- `prewarm`、`acquire`、`release`、`discard` 和 `total_count` 全部改用 `lock_state`。
- `acquire` 的等待路径改用 `wait_state`，避免 `Condvar::wait(...).unwrap()` 在 poisoned 状态下 panic。
- `src/runtime/engine/tests.rs` 新增 `lua_vm_pool_recovers_after_poisoned_state_lock_and_wait`，先制造 `total_count` 已占满且池状态锁被 poison 的场景，再通过通知线程归还一个 VM，验证 `acquire` 能穿过 poisoned wait 路径拿到租约。
- 保持 VM 创建工厂、预占计数回滚、租约归还、损坏 VM 废弃、空闲回收、池容量限制和 runlua 隔离池语义不变。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test lua_vm_pool_recovers_after_poisoned_state_lock_and_wait` 通过，新增 VM 池 poison 恢复测试通过。
- 修改后：`cargo test runlua` 通过，34 个 runlua 相关测试全部通过。
- 全量验证：`cargo test` 通过，232 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 空白审核：`git diff --check` 通过。
- 残留搜索：`src/runtime/engine.rs` 与 `src/runtime/engine/tests.rs` 中不再存在 `self.state.lock().unwrap()` 或 `condvar.wait(state).unwrap()`。

### 代码审核与遗留事项

- 本轮没有改变 Lua VM 池配置归一化、主池和 runlua 池拆分、Lua VM 初始化、请求作用域清理、`LuaVmLease` 自动归还或错误返回类型。
- poison 恢复只发生在已确认的单一池状态锁与其条件变量等待上，不引入候选状态、fallback 池或多来源兼容。
- 修改部分代码审核未发现需要继续自动修复的问题；当前 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续检查 `host::callbacks` 与 `host::database` 的全局回调注册表锁是否仍存在 panic 型 poison 边界。

## 2026-07-05 第 81 轮：恢复宿主回调注册表锁 poison 后的读写能力

### 问题探索

- 基线延续第 80 轮闭环状态：`cargo test` 通过，232 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮检查 `src/host/callbacks.rs` 的进程级宿主回调注册表。
- 已追清写入入口：`set_skill_lifecycle_callback`、`set_skill_operation_progress_callback`、`set_entry_registry_callback`、`set_skill_management_callback`、`set_host_tool_callback`、`set_model_embed_callback`、`set_model_llm_callback` 都通过统一 helper 写入 `OnceLock<Mutex<Option<Callback>>>`。
- 已追清读取入口：生命周期、进度和入口注册表事件通过 `emit_*` clone 当前回调；skill management、host tool、model embed、model llm 分发通过 `dispatch_*` clone 当前回调后调用宿主代码。
- 已追清可用性入口：Lua bridge 中的 `try_has_skill_management_callback`、`try_has_host_tool_callback`、`try_has_model_embed_callback`、`try_has_model_llm_callback` 只需要判断回调是否存在。
- 已确认调用宿主回调前会 clone `Arc` 并释放注册表锁，宿主代码不会在持有全局注册表锁时执行。
- 旧实现中 setter 在注册表锁 poison 后直接 panic；clone helper 将 poison 映射为错误，部分 emit 路径再 `expect` panic，部分 Lua model status/has 路径用 `unwrap_or(false)` 把锁 poison 伪装成能力不存在。
- 长期优化判断：全局回调注册表中的 `Option<Arc<...>>` 是单一事实来源；锁 poison 不代表回调值不可读取，也不应被伪装成 capability missing。

### 执行调整

- `src/host/callbacks.rs` 引入 `MutexGuard` 并新增 `lock_callback_registry`，通过 `PoisonError::into_inner` 恢复被标记为 poisoned 的回调注册表锁。
- `set_callback_registry_value` 改为使用 `lock_callback_registry`，setter 不再因注册表 poison panic。
- `clone_callback_registry_value` 改为直接返回 `Option<T>`，读取路径不再构造锁 poison 错误。
- `emit_skill_lifecycle_event`、`emit_skill_operation_progress_event`、`emit_entry_registry_delta` 改为直接使用 clone 结果，不再对注册表 poison 使用 `expect`。
- `dispatch_skill_management_request`、`dispatch_host_tool_request`、`dispatch_model_embed_request`、`dispatch_model_llm_request` 保留缺失回调的原有错误语义，但移除锁 poison 错误分支。
- `try_has_*_callback` 改为返回 `bool`，真实表达“当前是否注册回调”，不再暴露一个只为锁 poison 存在的 `Result`。
- `src/runtime/engine.rs` 与 `src/runtime/engine/bridge.rs` 同步布尔返回值调用点，移除 `.map_err(...)` 与 `.unwrap_or(false)`。
- 删除已无调用方的 `model_internal_error`。
- 新增 `progress_callback_registry_recovers_after_poisoned_lock` 单元测试，制造 progress callback 注册表 poison 后，验证 setter 可重新安装回调，emit 路径可 clone 并调用回调。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test progress_callback_registry_recovers_after_poisoned_lock` 通过，新增回调注册表恢复测试通过。
- 修改后：`cargo test callbacks` 通过，3 个 callbacks 相关测试全部通过。
- 修改后：`cargo test model` 通过，6 个 model 相关测试全部通过。
- 修改后：`cargo test host_tool` 执行成功，但当前无匹配用例，0 个测试运行。
- 自动修复：`cargo clippy --all-targets -- -D warnings` 首次发现 `model_internal_error` 已成为死代码，已删除后复跑通过。
- 全量验证：`cargo test` 通过，233 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 空白审核：`git diff --check` 通过。
- 残留搜索：`src/host/callbacks.rs`、`src/runtime/engine.rs`、`src/runtime/engine/bridge.rs` 中不再存在 `callback registry lock poisoned`、`model_internal_error`、`try_has_*().unwrap_or(false)` 或 `try_has_*().map_err(...)`。

### 代码审核与遗留事项

- 本轮没有改变宿主回调类型、全局注册表归属、FFI setter 入口、回调 clone 后再调用宿主代码的执行顺序、缺失回调的业务错误语义或模型 unavailable 错误结构。
- poison 恢复只发生在已确认的单一回调注册表锁上，不引入备用注册表、候选回调、fallback 能力判断或多来源兼容。
- 修改部分代码审核发现并自动修复了一个死代码问题；最终 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可用同样方式检查 `host::database` 的数据库 provider 回调注册表锁。

## 2026-07-05 第 82 轮：恢复数据库 provider 回调注册表锁 poison 后的快照捕获

### 问题探索

- 基线延续第 81 轮闭环状态：`cargo test` 通过，233 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮检查 `src/host/database.rs` 的数据库 provider 回调注册表。
- 已追清写入入口：`set_sqlite_provider_callback`、`set_lancedb_provider_callback`、`set_sqlite_provider_json_callback`、`set_lancedb_provider_json_callback` 写入四个进程级 `OnceLock<Mutex<Option<Callback>>>`。
- 已追清捕获入口：`LuaEngine::new` 调用 `RuntimeDatabaseProviderCallbacks::capture_process_defaults`，把当前进程级回调默认值捕获到单个 engine 私有 snapshot。
- 已追清消费入口：SQLite 与 LanceDB provider 在 host-callback 模式下只从 engine snapshot 分发请求，标准回调直接调用结构化 callback，JSON 回调先序列化 request 再解析 response。
- 已追清能力校验：`require_database_provider_callback_registration` 只根据 snapshot 中是否存在指定 provider 和传输模式的 callback 报告启动错误。
- 旧实现中 setter 在注册表锁 poison 后 panic；clone helper 把锁 poison 映射为 `String` 错误，导致 engine 创建期间的 snapshot 捕获失败。
- 长期优化判断：数据库 provider 回调注册表只保存 `Option<Arc<...>>`，回调值 clone 后才会调用宿主代码；锁 poison 不代表 snapshot 中的回调值不可用，不应阻止后续 engine 创建或 provider 分发。

### 执行调整

- `src/host/database.rs` 引入 `MutexGuard` 并新增 `lock_database_provider_callback_registry`，通过 `PoisonError::into_inner` 恢复被标记为 poisoned 的 provider 回调注册表锁。
- `set_database_provider_callback_registry_value` 改为使用恢复 helper，setter 不再因 provider 回调注册表 poison panic。
- `clone_database_provider_callback_registry_value` 改为直接返回 `Option<T>`，捕获 snapshot 时不再构造锁 poison 错误。
- `RuntimeDatabaseProviderCallbacks::capture_process_defaults` 改为直接返回 `Self`，因为唯一旧错误来源已被恢复 helper 消除。
- `LuaEngine::new` 同步移除对 `capture_process_defaults` 的错误映射，直接保存 engine-scoped provider callback snapshot。
- 测试辅助 `ProcessCallbackRestoreGuard::capture` 与 snapshot 隔离测试同步新的直接返回值。
- 新增 `database_provider_callback_registry_recovers_after_poisoned_lock`，制造 SQLite provider callback 注册表 poison 后，验证 setter 可重新安装回调，snapshot capture 可恢复 clone，标准 SQLite provider 分发能命中恢复后的回调。
- 保持标准/JSON callback 类型、provider callback mode、engine-scoped snapshot 隔离、缺失回调错误文案、SQLite/LanceDB request/response 编解码逻辑不变。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test database_provider_callback_registry_recovers_after_poisoned_lock` 通过，新增数据库 provider 注册表恢复测试通过。
- 修改后：`cargo test database` 通过，5 个 database 相关测试全部通过。
- 修改后：`cargo test provider` 通过，6 个 provider 相关测试全部通过。
- 全量验证：`cargo test` 通过，234 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 空白审核：`git diff --check` 通过。
- 残留搜索：`src/host/database.rs` 与 `src/runtime/engine.rs` 中不再存在 `database provider callback registry lock poisoned`，`capture_process_defaults` 已无 `Result` 式锁 poison 错误分支。

### 代码审核与遗留事项

- 本轮没有改变数据库 provider 对外回调 ABI、FFI setter 名称、host-callback 标准/JSON 分发协议、engine snapshot 隔离语义或缺失 callback 的业务错误。
- poison 恢复只发生在已确认的四个进程级 provider callback 注册表锁上，不引入备用回调、候选 provider、fallback transport 或跨 engine 动态重绑定。
- 修改部分代码审核未发现需要继续自动修复的问题；当前 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续检查 FFI engine registry、provider skill binding registry 和其它 `lock poisoned` 文案。

## 2026-07-05 第 83 轮：恢复 FFI engine registry 锁 poison 后的句柄创建与释放

### 问题探索

- 基线延续第 82 轮闭环状态：`cargo test` 通过，234 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮检查 `src/ffi.rs` 与 `src/ffi_standard.rs` 的全局 FFI engine registry。
- 已追清注册表结构：`FFI_ENGINE_REGISTRY` 是 `OnceLock<Mutex<HashMap<u64, FfiEngineSlot>>>`，每个 slot 保存 `Arc<Mutex<LuaEngine>>`。
- 已追清 JSON FFI 创建路径：`luaskills_ffi_engine_new_json` 创建 `LuaEngine`，分配 `engine_id`，再向全局 registry 插入 `FfiEngineSlot`。
- 已追清 JSON FFI 释放路径：`luaskills_ffi_engine_free_json` 解析 `engine_id` 后从全局 registry 删除对应 slot。
- 已追清标准 C ABI 创建/释放路径：`luaskills_ffi_engine_new`、`luaskills_ffi_engine_new_v2` 和 `luaskills_ffi_engine_free` 使用同一个全局 registry。
- 已追清执行路径：`with_engine` / `with_engine_mut` 先从全局 registry clone 出 `Arc<Mutex<LuaEngine>>`，随后释放 registry 锁，再进入单个 engine 实例锁和实际运行时操作。
- 旧实现中 registry 锁 poison 会导致 engine handle clone、新建 engine 插入、释放 engine 删除都返回 `FFI engine registry lock poisoned`。
- 长期优化判断：全局 FFI registry 只保存 engine id 到 engine handle 的映射，锁内不执行 Lua、不调用宿主回调；锁 poison 不代表映射内容不可继续读取或更新。

### 执行调整

- `src/ffi.rs` 引入 `MutexGuard` 并新增 `lock_ffi_engine_registry`，通过 `PoisonError::into_inner` 恢复被标记为 poisoned 的全局 FFI engine registry 锁。
- `clone_engine_handle` 改用 `lock_ffi_engine_registry`，engine handle 查找不再因 registry poison 失败。
- `luaskills_ffi_engine_new_json` 与 `luaskills_ffi_engine_free_json` 改用 `lock_ffi_engine_registry`，JSON FFI 创建和释放句柄不再返回 registry poison 错误。
- `src/ffi_standard.rs` 的 `luaskills_ffi_engine_new`、`luaskills_ffi_engine_new_v2`、`luaskills_ffi_engine_free` 改用同一个恢复 helper。
- `src/ffi/tests.rs` 的测试注册与清理辅助改用 `lock_ffi_engine_registry`，避免 poison 后测试清理失败。
- 新增 `ffi_engine_registry_recovers_after_poisoned_lock_for_json_handles`，制造全局 registry poison 后验证 JSON FFI 仍能创建并释放 engine handle。
- 自动修正新增测试隔离问题：去掉 poison 测试中的 registry `clear()`，避免并发标准 FFI 测试的 engine id 被误删；同时把 `with_engine_releases_registry_lock_before_operation` 调整为区分 `TryLockError::WouldBlock` 与 `TryLockError::Poisoned`，只把仍被持有的 `WouldBlock` 判为失败。
- 保持单个 `LuaEngine` 实例锁的 poison 错误语义不变，因为该锁保护实际运行时状态，一致性风险需要后续单独追踪。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test ffi_engine_registry_recovers_after_poisoned_lock_for_json_handles` 通过，新增 FFI registry 恢复测试通过。
- 修改后：`cargo test with_engine_releases_registry_lock_before_operation` 通过，registry 锁释放测试适配 poisoned 状态后通过。
- 修改后：`cargo test ffi` 通过，31 个 FFI 相关测试全部通过。
- 修改后：`cargo test ffi_standard` 通过，13 个标准 FFI 相关测试全部通过。
- 首次全量验证发现新增测试清空全局 registry 会干扰并发标准 FFI 测试，已移除 `clear()` 并修正 `try_lock` 断言。
- 全量验证：`cargo test` 通过，235 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 空白审核：`git diff --check` 通过。
- 残留搜索：生产路径中不再存在 `FFI engine registry lock poisoned`，全局 FFI registry 创建、释放和查找路径均通过 `lock_ffi_engine_registry`。

### 代码审核与遗留事项

- 本轮没有改变 FFI handle id 分配、registry 存储结构、JSON FFI 响应包络、标准 C ABI 函数签名、engine not found 错误或同线程重入保护。
- poison 恢复只发生在已确认的全局 FFI engine registry 锁上，不引入备用 registry、候选 engine id、跨实例 fallback 或对单个 engine 状态锁的混合恢复。
- 修改部分代码审核发现并自动修复了测试隔离问题；最终 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可单独检查 `FFI engine {id} lock poisoned` 是否可恢复，或转向 provider skill binding registry。

## 2026-07-05 第 84 轮：恢复 provider skill binding registry 锁 poison 后的注册与读取

### 问题探索

- 基线延续第 83 轮闭环状态：`cargo test` 通过，235 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮检查 SQLite 与 LanceDB provider host 的 skill binding registry。
- 已追清 SQLite 结构：`SqliteSkillHost` 持有 `Mutex<HashMap<String, Arc<SqliteSkillBinding>>>`，按 skill id 缓存已创建的 SQLite binding。
- 已追清 LanceDB 结构：`LanceDbSkillHost` 持有 `Mutex<HashMap<String, Arc<LanceDbSkillBinding>>>`，按 skill id 缓存已创建的 LanceDB binding。
- 已追清写入入口：`register_skill` 在加载 skill 时创建或复用 binding，并将新 binding 插入对应 provider host 的 registry。
- 已追清读取入口：`binding_for_skill` 在 Lua provider 上下文注入、跨 skill 调用恢复、嵌套调用恢复时按 skill name 取回已注册 binding。
- 已确认 registry 锁内只维护 skill name 到 `Arc<...Binding>` 的映射；真正的动态库调用、host callback 分发、space controller 调用和 binding 内部句柄锁不在该 registry 锁内执行。
- 旧实现中 `register_skill` 和 `binding_for_skill` 都把 registry lock poison 转成错误，导致后续 provider 上下文恢复或重复注册失败。
- 长期优化判断：provider skill binding registry 是单一映射事实来源；锁 poison 不代表映射不可继续读取或更新，应恢复 guard 并维持原有注册/复用规则。

### 执行调整

- `src/providers/sqlite.rs` 引入 `MutexGuard`，新增 `SqliteSkillHost::lock_skills`，通过 `PoisonError::into_inner` 恢复 SQLite skill binding registry 锁。
- `SqliteSkillHost::register_skill` 与 `SqliteSkillHost::binding_for_skill` 改用 `lock_skills`，不再返回 SQLite registry poison 错误。
- `src/providers/lancedb.rs` 引入 `MutexGuard`，新增 `LanceDbSkillHost::lock_skills`，通过 `PoisonError::into_inner` 恢复 LanceDB skill binding registry 锁。
- `LanceDbSkillHost::register_skill` 与 `LanceDbSkillHost::binding_for_skill` 改用 `lock_skills`，不再返回 LanceDB registry poison 错误。
- `src/providers/sqlite/tests.rs` 新增 `sqlite_skill_binding_registry_recovers_after_poisoned_lock`，制造 SQLite registry poison 后，在 host-callback 模式下调用真实 `register_skill` 和 `binding_for_skill` 验证写后读恢复。
- `src/providers/lancedb.rs` 新增 `lancedb_skill_binding_registry_recovers_after_poisoned_lock`，同样覆盖 LanceDB registry poison 后的写入与读取恢复。
- 自动修复：测试编译时确认 `SkillSqliteMeta` 不在 crate root 导出，已改为从 sqlite provider 测试的上级模块导入，避免猜测类型归属。
- 保持 binding plan、provider mode 分支、动态库句柄创建、host callback snapshot、space controller 启用、缺失 binding 返回 `None` 语义不变。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test sqlite_skill_binding_registry_recovers_after_poisoned_lock` 通过，新增 SQLite binding registry 恢复测试通过。
- 修改后：`cargo test lancedb_skill_binding_registry_recovers_after_poisoned_lock` 通过，新增 LanceDB binding registry 恢复测试通过。
- 修改后：`cargo test provider` 通过，8 个 provider 相关测试全部通过。
- 修改后：`cargo test database` 通过，5 个 database 相关测试全部通过。
- 全量验证：`cargo test` 通过，237 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 空白审核：`git diff --check` 通过。
- 残留搜索：`src/providers/sqlite.rs`、`src/providers/sqlite/tests.rs`、`src/providers/lancedb.rs` 中不再存在 `skill binding registry lock poisoned` 或 `failed to acquire * skill registry lock`，读写路径均通过 `lock_skills`。

### 代码审核与遗留事项

- 本轮没有改变 SQLite/LanceDB binding 的外部行为、provider mode 选择、callback mode、controller mode、binding context 生成、动态库资源生命周期或 host callback 分发。
- poison 恢复只发生在已确认的 provider host skill binding 映射锁上，不引入备用 binding、候选 skill name、跨 provider fallback 或对 binding 内部动态库句柄锁的混合恢复。
- 修改部分代码审核发现并自动修复了测试 import 归属问题；最终 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续检查 `runtime::config`、`managed_io`、`process_session` 或单个 FFI engine 实例锁中的剩余 `lock poisoned` 错误。

## 2026-07-05 第 85 轮：恢复 skill config 锁 poison 后的路径解析与文件读写

### 问题探索

- 基线延续第 84 轮闭环状态：`cargo test` 通过，237 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮检查 `src/runtime/config.rs` 的 skill config 存储锁。
- 已追清默认路径锁：`SkillConfigStore::default_runtime_root` 保存未显式传入配置文件时使用的 runtime root，`set_default_runtime_root` 写入，`file_path` 读取并派生 `config/skill_config.json`。
- 已追清路径锁注册表：`skill_config_lock_registry` 按规范化后的配置文件路径保存进程级 `Arc<Mutex<()>>`，确保指向同一配置文件的 store 共享同一把 IO 锁。
- 已追清单文件 IO 锁：`with_document_read` 和 `with_document_mut` 先通过 `shared_skill_config_path_lock` 取得单文件锁，再读取、修改、写回配置文档。
- 已追清写入安全性：`write_document_to` 先写临时文件、flush、sync，再通过平台相关的原子替换策略提升到目标文件；panic 期间最多留下临时文件，不会要求通过 fallback 读取多份目标文件。
- 旧实现把默认 runtime root 锁、共享 IO 锁和锁注册表 poison 都转成错误，导致后续路径解析或配置读写持续失败。
- 长期优化判断：这些锁分别保护单一内存路径值、锁注册表映射和单文件读写临界区；poison 标记不代表其事实来源不可继续使用，应恢复 guard 并保持原有路径和原子写入规则。

### 执行调整

- `src/runtime/config.rs` 引入 `MutexGuard`。
- 新增 `SkillConfigStore::lock_default_runtime_root`，通过 `PoisonError::into_inner` 恢复默认 runtime-root 锁。
- `set_default_runtime_root` 与 `file_path` 改用 `lock_default_runtime_root`，不再返回 `skill config runtime-root lock poisoned`。
- 新增 `lock_skill_config_lock_registry`，通过 `PoisonError::into_inner` 恢复进程级路径锁注册表。
- `shared_skill_config_path_lock` 改用 `lock_skill_config_lock_registry`，不再返回 `skill config lock registry poisoned`。
- 新增 `lock_shared_skill_config_path`，通过 `PoisonError::into_inner` 恢复单文件 skill-config IO 锁。
- `with_document_read` 与 `with_document_mut` 改用 `lock_shared_skill_config_path`，不再返回 `skill config shared io lock poisoned`。
- 新增 `skill_config_default_runtime_root_recovers_after_poisoned_lock`，覆盖默认 runtime root 锁 poison 后仍可 set 和 resolve。
- 新增 `skill_config_lock_registry_recovers_after_poisoned_lock`，覆盖全局路径锁注册表 poison 后仍能解析并复用同一把共享锁。
- 新增 `skill_config_shared_io_lock_recovers_after_poisoned_lock`，覆盖单文件 IO 锁 poison 后仍能写入并读取配置值。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test skill_config_default_runtime_root_recovers_after_poisoned_lock` 通过。
- 修改后：`cargo test skill_config_lock_registry_recovers_after_poisoned_lock` 通过。
- 修改后：`cargo test skill_config_shared_io_lock_recovers_after_poisoned_lock` 通过。
- 修改后：`cargo test config` 通过，28 个 config 相关测试全部通过。
- 全量验证：`cargo test` 通过，240 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 空白审核：`git diff --check` 通过。
- 残留搜索：`src/runtime/config.rs` 中不再存在旧的 `skill config * lock poisoned` 错误分支，默认路径、锁注册表和单文件 IO 锁均通过恢复 helper。

### 代码审核与遗留事项

- 本轮没有改变 skill config 文件格式、显式路径固定、默认路径派生、路径身份归一化、原子写入策略、键/skill id 校验或配置读写 API。
- poison 恢复只发生在已确认的 runtime-root 内存槽、路径锁注册表和单文件 IO 锁上，不引入备用配置文件、候选路径、fallback 文档或多来源兼容。
- 修改部分代码审核未发现需要继续自动修复的问题；当前 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续检查 `managed_io`、`process_session`、managed runtime worker pool 或单个 FFI engine 实例锁的剩余 poison 错误。

## 2026-07-05 第 86 轮：恢复 managed IO 状态锁 poison 后的文件与兼容 IO 可用性

### 问题探索

- 基线延续第 85 轮闭环状态：`cargo test` 通过，240 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮检查 `src/runtime/managed_io.rs` 的托管 IO 状态锁。
- 已追清托管文件状态：`ManagedIoFile` 内部持有 `Arc<Mutex<ManagedIoFileState>>`，状态包含路径、打开模式、编码、内存 buffer、cursor、flush 位置、closed 标记和关闭状态。
- 已追清文件操作入口：`read`、`write`、`flush`、`close`、`seek`、`io.type` 都通过 `ManagedIoFile::lock_state` 访问同一份文件状态。
- 已追清兼容 IO 状态：`ManagedIoCompatState` 只保存当前默认 input/output 托管文件句柄。
- 已追清兼容 IO 入口：`io.input`、`io.output`、`io.read`、`io.write`、`io.flush`、`io.close` 只在短时间内读取或替换当前默认句柄，然后把实际文件操作交给 `ManagedIoFile`。
- 旧实现中托管文件状态锁 poison 会让所有后续文件方法返回 `managed file lock poisoned`；兼容 IO 状态锁 poison 会让 `io.input/output/read/write/flush/close` 返回 `compat state lock poisoned`。
- 长期优化判断：这些锁保护的都是单一内存状态容器；poison 标记不代表 buffer、cursor、closed 标记或当前 input/output 句柄不可继续使用，应恢复 guard 并保留原有 IO 行为。

### 执行调整

- `src/runtime/managed_io.rs` 引入 `MutexGuard`。
- `ManagedIoFile::lock_state` 改为直接返回 `MutexGuard`，通过 `PoisonError::into_inner` 恢复文件状态锁。
- `ManagedIoFile` 的 `is_closed`、读取、写入、flush、close、seek 路径全部继续通过 `lock_state` 访问状态，只移除 poison 错误分支。
- 新增 `lock_compat_state`，通过 `PoisonError::into_inner` 恢复兼容 IO 状态锁。
- `io.input`、`io.output`、`io.read`、`io.write`、`io.flush`、`io.close` 的兼容状态访问全部改用 `lock_compat_state`。
- `src/runtime/managed_io/tests.rs` 新增 `managed_io_file_state_recovers_after_poisoned_lock`，制造托管文件状态锁 poison 后验证 `is_closed` 仍能读取状态。
- `src/runtime/managed_io/tests.rs` 新增 `managed_io_compat_state_recovers_after_poisoned_lock`，制造兼容状态锁 poison 后验证 `io.flush` 等价路径仍可执行。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test managed_io_file_state_recovers_after_poisoned_lock` 通过。
- 修改后：`cargo test managed_io_compat_state_recovers_after_poisoned_lock` 通过。
- 修改后：`cargo test managed_io` 通过，16 个 managed_io 相关测试全部通过。
- 全量验证：`cargo test` 通过，242 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 空白审核：`git diff --check` 通过。
- 残留搜索：`src/runtime/managed_io.rs` 与 `src/runtime/managed_io/tests.rs` 中不再存在 `managed file lock poisoned` 或 `compat state lock poisoned`。

### 代码审核与遗留事项

- 本轮没有改变 Lua `io` 兼容 API、`vulcan.io` API、文件读写格式、编码处理、flush/close 语义、popen 关闭状态或无默认 output 时写入运行时日志的行为。
- poison 恢复只发生在已确认的托管文件状态锁和兼容 IO 状态锁上，不引入备用文件句柄、候选 output、fallback buffer 或多来源兼容。
- 修改部分代码审核未发现需要继续自动修复的问题；当前 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续检查 `process_session`、managed runtime worker pool 或单个 FFI engine 实例锁的剩余 poison 错误。

## 2026-07-05 第 87 轮：恢复 managed runtime worker pool 全局锁 poison 后的受管运行时调用能力

### 问题探索

- 基线延续第 86 轮闭环状态：`cargo test` 通过，242 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮检查 `src/runtime/engine.rs` 的进程级受管运行时 worker 池。
- 已追清全局池来源：`managed_runtime_worker_pool` 通过 `OnceLock<Mutex<ManagedRuntimeWorkerPool>>` 保存进程级池实例。
- 已追清池内状态：`ManagedRuntimeWorkerPool` 只保存 `ManagedRuntimeWorkerKey -> Vec<ManagedRuntimeWorker>` 的复用桶，并通过 `max_idle_per_key` 限制每个 key 的空闲 worker 数量。
- 已追清调用入口：`invoke_pooled_managed_runtime` 先短暂锁池执行 `acquire`，随后释放锁并调用 `invoke_managed_runtime_worker`，调用完成后再次短暂锁池执行 `release` 或 `discard`。
- 已追清执行边界：实际受管 runtime 子进程交互、超时处理、stdout/stderr 解析和 discard 判定都在池锁外完成，池锁不保护 worker 协议读写过程。
- 旧实现把池锁 poison 转成 `managed runtime worker pool lock poisoned`，一次 panic 会让后续所有 Python/Node 等受管运行时池化调用持续失败。
- 长期优化判断：该锁只保护进程内 worker 复用桶；poison 标记不代表桶结构不可继续使用，应恢复 guard 并保持既有 key、复用、discard 和容量规则。

### 执行调整

- `src/runtime/engine.rs` 复用已有 `MutexGuard` 引入。
- 新增 `lock_managed_runtime_worker_pool`，通过 `PoisonError::into_inner` 恢复进程级受管运行时 worker 池锁。
- `invoke_pooled_managed_runtime` 的 acquire 阶段改用 `lock_managed_runtime_worker_pool`，不再因为锁 poison 直接中断调用。
- `invoke_pooled_managed_runtime` 的 release/discard 阶段改用 `lock_managed_runtime_worker_pool`，保持 worker 调用后仍能归还或丢弃 worker。
- `src/runtime/engine/tests.rs` 引入 `lock_managed_runtime_worker_pool` 与 `managed_runtime_worker_pool`，用于直接覆盖全局池恢复路径。
- 新增 `managed_runtime_worker_pool_recovers_after_poisoned_global_lock`，制造全局池锁 poison 后验证恢复 helper 可以继续取得池并执行 `discard`。
- 修改部分代码审核时发现新增测试的文档说明复用了 warm-worker 测试描述，已自动修正为 poison 恢复语义。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test managed_runtime_worker_pool_recovers_after_poisoned_global_lock` 通过，1 个目标测试通过。
- 修改后：`cargo test managed_runtime_worker_pool` 通过，2 个 worker pool 相关测试全部通过。
- 全量验证：`cargo test` 通过，243 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 空白审核：`git diff --check` 通过。
- 残留搜索：`src/runtime/engine.rs` 与 `src/runtime/engine/tests.rs` 中不再存在旧的 `managed runtime worker pool lock poisoned` 错误分支。

### 代码审核与遗留事项

- 本轮没有改变受管运行时 manifest 解析、环境计划、worker key 组成、子进程协议、调用超时、stdout/stderr 兼容、worker 复用上限或 discard 判定。
- poison 恢复只发生在已确认的进程级 worker 复用桶锁上，不引入备用 worker 池、候选 key、fallback runtime 或多来源兼容。
- 修改部分代码审核已发现并修正测试文档描述不匹配问题；修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续检查 `process_session` 或单个 FFI engine 实例锁的剩余 poison 错误。

## 2026-07-05 第 88 轮：恢复单个 FFI engine 实例锁 poison 后的句柄可用性

### 问题探索

- 基线延续第 87 轮闭环状态：`cargo test` 通过，243 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮检查 `src/ffi.rs` 中单个 FFI engine slot 的实例锁。
- 已追清注册表职责：`FFI_ENGINE_REGISTRY` 只负责通过 `engine_id` 找到 `FfiEngineSlot`，并克隆其中的 `Arc<Mutex<LuaEngine>>`。
- 已追清执行入口：JSON FFI 与标准 C ABI 最终都通过 `with_engine` 或 `with_engine_mut` 执行只读或可变的 engine 操作。
- 已追清锁顺序：`clone_engine_handle` 克隆 `Arc<Mutex<LuaEngine>>` 后释放注册表锁，随后 `ActiveFfiEngineGuard::enter` 做同线程重入保护，最后才锁单个 engine 实例。
- 已追清并发边界：`with_engine_releases_registry_lock_before_operation` 已覆盖操作期间不持有全局注册表锁；`with_engine_rejects_same_thread_reentry` 已覆盖同线程重入不会死锁。
- 旧实现把单个 engine 锁 poison 转成 `FFI engine {id} lock poisoned`，一次 panic 会让该 FFI handle 后续所有 JSON FFI 与标准 C ABI 操作持续失败。
- 长期优化判断：该锁只保护唯一的 `LuaEngine` 实例；poison 标记不代表该实例需要通过备用 handle、备用 registry 或多路兼容恢复，应恢复同一个 guard 并保留现有重入保护和锁顺序。

### 执行调整

- `src/ffi.rs` 新增 `lock_engine_handle`，通过 `PoisonError::into_inner` 恢复单个 FFI engine 实例锁。
- `with_engine` 改用 `lock_engine_handle` 获取只读操作 guard，不再因为 engine 实例锁 poison 直接返回错误。
- `with_engine_mut` 改用 `lock_engine_handle` 获取可变操作 guard，不再因为 engine 实例锁 poison 直接返回错误。
- `src/ffi/tests.rs` 引入 `with_engine_mut`，用于同时验证只读和可变 FFI 操作路径。
- 新增 `with_engine_recovers_after_engine_handle_lock_poisoned`，克隆同一个 engine handle 后制造实例锁 poison，并验证 `with_engine` 与 `with_engine_mut` 都能继续执行。
- 修改部分代码审核时补齐新增测试中测试级注册表 guard 的双语意图说明。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test with_engine_recovers_after_engine_handle_lock_poisoned` 通过，1 个目标测试通过。
- 修改后：`cargo test with_engine` 通过，3 个 with_engine 相关测试全部通过。
- 修改后：`cargo test ffi` 通过，32 个 FFI 相关测试全部通过。
- 全量验证：`cargo test` 通过，244 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 空白审核：`git diff --check` 通过。
- 残留搜索：`src/ffi.rs` 与 `src/ffi/tests.rs` 中不再存在旧的 `FFI engine .* lock poisoned` 错误分支。

### 代码审核与遗留事项

- 本轮没有改变 FFI engine 注册表结构、engine id 分配、engine free 行为、JSON FFI 响应包络、标准 C ABI 状态码、同线程重入拒绝或注册表锁释放时机。
- poison 恢复只发生在已确认的单个 `Arc<Mutex<LuaEngine>>` 实例锁上，不引入备用 engine、候选 handle、fallback registry 或多来源兼容。
- 修改部分代码审核已补齐新增测试注释；修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续检查 `process_session` 的 stdin/stdout/stderr/final_status/child 等剩余 poison 错误。

## 2026-07-05 第 89 轮：恢复 process session 输出缓冲与 reader 槽位锁 poison 后的读取清理能力

### 问题探索

- 基线延续第 88 轮闭环状态：`cargo test` 通过，244 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮检查 `src/runtime/process_session.rs` 中输出缓冲区与 reader 槽位相关锁。
- 已追清输出缓冲来源：`ManagedProcessSessionState` 分别持有 `stdout_buffer` 与 `stderr_buffer`，类型都是 `Arc<Mutex<Vec<u8>>>`。
- 已追清输出写入路径：`spawn_session_pipe_reader` 的后台线程从 stdout/stderr pipe 读取字节，并通过 `append_bounded` 写入对应缓冲区。
- 已追清输出读取路径：`has_readable_output` 读取缓冲区判断是否已有数据或目标 marker；`drain_buffer` 从缓冲区取出最多 `max_bytes` 字节。
- 已追清 reader 槽位来源：`stdout_reader` 与 `stderr_reader` 是 `Mutex<Option<SessionPipeReader>>`，只保存后台 reader 线程 handle、完成通知 channel 和完成标记。
- 已追清 reader 槽位调用路径：`output_readers_drained` 通过 `reader_completed` 检查完成标记；`join_one_reader` 在进程关闭后等待 reader 完成并取出 handle join。
- 旧实现中输出缓冲锁 poison 会让 read 路径返回 `stdout/stderr/output lock poisoned` 或让后台 reader 直接退出；reader 槽位锁 poison 会让 drain/close 清理路径返回 `reader lock poisoned`。
- 长期优化判断：这些锁只保护同一会话内的输出字节窗口和 reader handle 槽位；poison 标记不代表输出事实来源变成多来源，也不需要 fallback buffer，应恢复同一 guard 并保持原有 drain、marker、timeout 和 join 规则。

### 执行调整

- `src/runtime/process_session.rs` 引入 `MutexGuard`。
- 新增 `lock_session_output_buffer`，通过 `PoisonError::into_inner` 恢复单个 process session 输出缓冲区锁。
- 新增 `lock_session_reader_slot`，通过 `PoisonError::into_inner` 恢复单个 reader 槽位锁。
- `has_readable_output` 改用 `lock_session_output_buffer` 读取 stdout/stderr 缓冲区，不再因为输出缓冲锁 poison 中断 read 等待。
- `spawn_session_pipe_reader` 改用 `lock_session_output_buffer` 追加输出，避免一次 poison 导致后台 reader 永久停止写入。
- `drain_buffer` 改用 `lock_session_output_buffer` 取出输出字节，不再返回 `process.session.read: output lock poisoned`。
- `reader_completed` 改用 `lock_session_reader_slot`，并在恢复后直接返回完成状态布尔值。
- `join_one_reader` 两次访问 reader 槽位都改用 `lock_session_reader_slot`，保留原有超时、成功 take 和 join 语义。
- `src/runtime/process_session/tests.rs` 新增 `process_session_output_buffer_recovers_after_poisoned_lock`，覆盖输出 buffer poison 后 `drain_buffer` 仍可读取。
- `src/runtime/process_session/tests.rs` 新增 `process_session_reader_slot_recovers_after_poisoned_lock`，覆盖 reader 槽位 poison 后 `reader_completed` 仍可读取完成状态。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test process_session_output_buffer_recovers_after_poisoned_lock` 通过，1 个目标测试通过。
- 修改后：`cargo test process_session_reader_slot_recovers_after_poisoned_lock` 通过，1 个目标测试通过。
- 修改后：`cargo test process_session` 通过，11 个 process_session 相关测试全部通过。
- 全量验证：`cargo test` 通过，246 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 空白审核：`git diff --check` 通过。
- 残留搜索：`src/runtime/process_session.rs` 与 `src/runtime/process_session/tests.rs` 中不再存在 `stdout lock poisoned`、`stderr lock poisoned`、`output lock poisoned` 或 `reader lock poisoned` 旧错误分支。

### 代码审核与遗留事项

- 本轮没有改变 process session 打开参数、编码解码、输出缓冲窗口大小、marker 等待、read drain 顺序、reader shutdown timeout、reader join 或进程树清理策略。
- poison 恢复只发生在已确认的输出 byte buffer 与 reader slot 上，不引入备用缓冲区、候选 reader、fallback 输出流或多来源兼容。
- 修改部分代码审核确认 `child`、`stdin`、`closed`、`final_status` 等生命周期锁未在本轮改变，留待后续按执行流单独处理。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续检查 `process_session` 的 stdin、closed、final_status 与 child 生命周期锁剩余 poison 错误。

## 2026-07-05 第 90 轮：恢复 process session stdin/closed/final_status 轻量生命周期锁 poison 后的清理能力

### 问题探索

- 基线延续第 89 轮闭环状态：`cargo test` 通过，246 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮继续检查 `src/runtime/process_session.rs`，范围限定为 `stdin`、`closed` 与 `final_status` 三类轻量生命周期状态锁。
- 已追清 stdin 来源：`ManagedProcessSessionState::stdin` 是 `Mutex<Option<ChildStdin>>`，`write_values` 通过它写入和 flush，`close_stdin_pipe` 通过 `take` 丢弃管道。
- 已追清 closed 来源：`ManagedProcessSessionState::closed` 是 `Mutex<bool>`，当前由 close/kill/drop 清理路径写入，用于记录会话生命周期关闭意图。
- 已追清 final_status 来源：`ManagedProcessSessionState::final_status` 是 `Mutex<Option<ProcessStatusSnapshot>>`，`cached_final_status` 读取显式 teardown 后的终态缓存，`store_final_status` 在直接子进程完成回收后写入缓存。
- 已追清调用边界：`kill_process_tree_and_wait` 会先读取 `final_status`，未命中时再通过 `child` 执行进程树终止与 wait，最后写回 `final_status`。
- 旧实现中 stdin 锁 poison 会让 `write` 或 `close_stdin_pipe` 失败；closed 锁 poison 会让 `mark_closed` 失败；final_status 锁 poison 会让状态缓存读取或写入失败，从而影响 kill/close/drop 的幂等清理。
- 长期优化判断：这三把锁只保护同一会话内的单一状态槽；poison 标记不代表 stdin、关闭标记或终态缓存有多来源，也不需要备用状态，应恢复同一 guard 并保持原有写入、关闭和幂等缓存规则。

### 执行调整

- `src/runtime/process_session.rs` 新增 `lock_session_stdin_pipe`，通过 `PoisonError::into_inner` 恢复 stdin 管道槽位锁。
- `write_values` 改用 `lock_session_stdin_pipe`，保留 stdin 已关闭时返回 `process.session.write: stdin is closed` 的原有语义。
- `close_stdin_pipe` 改用 `lock_session_stdin_pipe`，继续通过 `take` 丢弃同一个 stdin 管道槽位。
- 新增 `lock_session_closed_flag`，通过 `PoisonError::into_inner` 恢复关闭标记锁。
- `mark_closed` 改用 `lock_session_closed_flag`，保留原有单标记写入行为。
- 新增 `lock_session_final_status`，通过 `PoisonError::into_inner` 恢复最终状态缓存锁。
- `cached_final_status` 与 `store_final_status` 改用 `lock_session_final_status`，保留先查缓存、kill 后写缓存的幂等清理流程。
- `src/runtime/process_session/tests.rs` 新增 `process_session_lifecycle_state_locks_recover_after_poisoned_lock`，在真实长运行会话中分别制造 stdin、closed、final_status 锁 poison，并验证 write/close、mark_closed、kill 后 final_status 缓存回读全部可用。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test process_session_lifecycle_state_locks_recover_after_poisoned_lock` 通过，1 个目标测试通过。
- 修改后：`cargo test process_session` 通过，12 个 process_session 相关测试全部通过。
- 全量验证：`cargo test` 通过，247 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 空白审核：`git diff --check` 通过。
- 残留搜索：`src/runtime/process_session.rs` 与 `src/runtime/process_session/tests.rs` 中不再存在 `stdin lock poisoned`、`closed lock poisoned` 或 `final_status lock poisoned` 旧错误分支；剩余 poison 分支只集中在 `child lock poisoned`。

### 代码审核与遗留事项

- 本轮没有改变 process session 的进程启动、进程树控制、child wait/kill、输出读取、reader join、编码、Lua API 或错误上下文前缀。
- poison 恢复只发生在已确认的 stdin 管道槽位、关闭标记和终态缓存上，不引入备用 stdin、候选关闭状态、fallback 终态缓存或多来源兼容。
- 修改部分代码审核确认 `child` 锁仍保留原有错误路径，下一轮需要单独围绕平台状态探测与进程树 teardown 追清后再处理。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续检查 `process_session` 的 `child` 生命周期锁剩余 poison 错误。

## 2026-07-05 第 91 轮：恢复 process session child 锁 poison 后的状态探测与进程树清理能力

### 问题探索

- 基线延续第 90 轮闭环状态：`cargo test` 通过，247 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮继续检查 `src/runtime/process_session.rs` 中最后剩余的 `child lock poisoned` 分支。
- 已追清 child 来源：`ManagedProcessSessionState::child` 是单个 `Mutex<Child>`，保存该会话的直接子进程句柄。
- 已追清状态探测路径：`peek_status_snapshot` 先读取 `final_status` 缓存，未命中时按平台通过同一个 child 句柄探测状态；Unix 使用 `waitid(..., WNOWAIT)` 避免提前 reap，Windows 使用进程 handle 查询状态，其他平台使用 `try_wait`。
- 已追清清理路径：`kill_process_tree_and_wait` 先读取 `final_status` 缓存，未命中时锁 child，调用 `process_tree.terminate(&child)` 处理进程树，再对直接 child 执行 `try_wait` 或 `wait`，最后写入 `final_status` 缓存。
- 已追清上层入口：`status`、`close`、`kill`、drop cleanup 最终都依赖上述状态探测或进程树清理路径。
- 旧实现中 child 锁 poison 会让状态探测、close/kill/drop 清理路径返回 `child lock poisoned`，一次 panic 会让同一会话的后续生命周期操作持续失败。
- 长期优化判断：child 锁只保护该会话唯一的直接子进程句柄；poison 标记不代表需要备用 child、候选 pid 或 fallback 进程树，应恢复同一 guard 并保持平台探测、terminate 与 wait 顺序不变。

### 执行调整

- `src/runtime/process_session.rs` 新增 `lock_session_child`，通过 `PoisonError::into_inner` 恢复单个 child 进程锁。
- `peek_status_snapshot` 的 Unix 分支改用 `lock_session_child`，保留 `waitid`、`WNOHANG` 与 `WNOWAIT` 语义。
- `peek_status_snapshot` 的 Windows 分支改用 `lock_session_child`，保留 `peek_windows_process_status` 的 handle 查询逻辑。
- `peek_status_snapshot` 的其他平台分支改用 `lock_session_child`，保留 `try_wait` 状态探测逻辑。
- `kill_process_tree_and_wait` 改用 `lock_session_child`，保留先 terminate 进程树、再 `try_wait`/`wait` 直接子进程、最后写入 `final_status` 的原有顺序。
- `src/runtime/process_session/tests.rs` 新增 `process_session_child_lock_recovers_after_poisoned_lock`，在真实长运行会话中制造 child 锁 poison，并验证 `peek_status_snapshot` 与 `kill_child` 都能继续执行。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test process_session_child_lock_recovers_after_poisoned_lock` 通过，1 个目标测试通过。
- 修改后：`cargo test process_session` 通过，13 个 process_session 相关测试全部通过。
- 全量验证：`cargo test` 通过，248 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 空白审核：`git diff --check` 通过。
- 残留搜索：`src/runtime/process_session.rs` 中不再存在旧的 `lock poisoned` 错误分支；`process_session` 范围内剩余 `poisoned` 命中均为新测试说明或恢复 helper。

### 代码审核与遗留事项

- 本轮没有改变 process session 的平台状态探测、Unix 非 reap 探测、Windows handle 查询、进程树 terminate 策略、直接子进程 wait、最终状态缓存、reader join 或 Lua API。
- poison 恢复只发生在已确认的单个 `Mutex<Child>` 上，不引入备用 child、候选 pid、fallback child handle 或多来源兼容。
- 修改部分代码审核确认 `process_session` 的 poison 错误分支已按输出/reader、轻量状态、child 生命周期三轮拆分清理完成。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可回到全仓搜索，继续寻找其他剩余 `lock poisoned`、路径编码或进程/FFI 边界问题。

## 2026-07-05 第 92 轮：恢复 space controller bridge runtime 锁 poison 后的控制器请求能力

### 问题探索

- 基线延续第 91 轮闭环状态：`cargo test` 通过，248 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮检查 `src/host/controller.rs` 中 `LuaRuntimeSpaceControllerBridge` 的 runtime 锁。
- 已追清 bridge 状态：`LuaRuntimeSpaceControllerBridge` 持有一个 `ControllerClient`、一个 `Mutex<Runtime>` 和一个 `binding_scope_id`。
- 已追清构造流程：`new` 创建 bridge-owned Tokio runtime，创建 controller client，先连接 controller，再解析当前 client session 作为绑定作用域。
- 已追清请求入口：`run` 锁住同一个 bridge-owned runtime，然后通过 `run_controller_operation_with_client` 克隆 controller client 并执行异步 SDK 操作。
- 已追清同步/异步兼容边界：`run_future_on_bridge_runtime` 在普通同步线程直接 `block_on`，在已有 Tokio runtime 内则通过 runtime handle spawn 并同步等待结果。
- 旧实现把 runtime 锁 poison 转成 `controller runtime lock poisoned`，一次 panic 会让同一个 bridge 的后续 attach 或 controller 请求持续失败。
- 长期优化判断：该锁只保护 bridge 唯一拥有的 Tokio runtime；poison 标记不代表需要新建 controller client、替换 session scope 或备用 runtime，应恢复同一个 guard 并保持原有 SDK 调度路径。

### 执行调整

- `src/host/controller.rs` 引入 `MutexGuard`。
- 新增 `lock_controller_runtime`，通过 `PoisonError::into_inner` 恢复 bridge-owned controller runtime 锁。
- `LuaRuntimeSpaceControllerBridge::run` 改用 `lock_controller_runtime`，不再因为 runtime 锁 poison 直接返回错误。
- 测试模块引入 `lock_controller_runtime`、`std::panic` 与 `Mutex`。
- 新增 `controller_runtime_lock_recovers_after_poisoned_lock`，用独立 `Mutex<Runtime>` 制造 runtime 锁 poison，并验证恢复后的 runtime 仍可通过 `run_future_on_bridge_runtime` 执行 future。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test controller_runtime_lock_recovers_after_poisoned_lock` 通过，1 个目标测试通过。
- 修改后：`cargo test controller` 通过，4 个 controller 相关测试全部通过。
- 全量验证：`cargo test` 通过，249 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 空白审核：`git diff --check` 通过。
- 残留搜索：`src/host/controller.rs` 中不再存在旧的 `controller runtime lock poisoned` 错误分支。

### 代码审核与遗留事项

- 本轮没有改变 controller client 注册、endpoint 配置、auto-spawn 配置、binding scope 解析、binding id 生成、future 调度策略或 drop shutdown 行为。
- poison 恢复只发生在已确认的 bridge-owned `Mutex<Runtime>` 上，不引入备用 controller runtime、候选 client、fallback session scope 或多来源兼容。
- 修改部分代码审核未发现需要继续自动修复的问题；当前 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续检查 `runtime/engine/runlua.rs` 与 `runtime/engine/lease.rs` 中剩余的 poison 错误分支。

## 2026-07-05 第 93 轮：恢复 runlua cwd guard 与 print capture 锁 poison 后的执行能力

### 问题探索

- 基线延续第 92 轮闭环状态：`cargo test` 通过，249 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮检查 `src/runtime/engine/runlua.rs` 中 luaexec 文件执行 cwd guard 与 print capture 锁。
- 已追清 cwd guard 来源：`runlua_cwd_guard` 是进程级 `OnceLock<Mutex<()>>`，用于串行化临时切换进程当前目录的文件型 luaexec 执行。
- 已追清 cwd guard 调用路径：`execute_runlua_wrapper` 在存在 `entry_file.parent()` 时锁 cwd guard，保存当前目录，切换到脚本目录，执行 Lua wrapper，然后恢复原目录。
- 已追清共享调用点：全量测试暴露 `runtime/engine/lease.rs` 的 runtime lease cwd override 也复用同一把 `runlua_cwd_guard`，因此必须同步恢复同一个锁路径。
- 已追清 print capture 来源：单次 runlua 执行创建 `Arc<Mutex<Vec<String>>>`，覆写 Lua `print` 后把每次打印追加到该唯一捕获缓冲区，执行结束后克隆用于渲染成功或错误 Markdown。
- 旧实现中 cwd guard poison 会让文件型 luaexec 或 runtime lease cwd override 失败；print capture poison 会让 `print` 调用或执行后输出收集失败。
- 长期优化判断：cwd guard 只保护进程级当前目录切换临界区，print capture 只保护单次 runlua 的唯一输出容器；poison 标记不代表需要备用 cwd guard、备用输出容器或多路径兼容，应恢复同一 guard 并保持原有执行/恢复顺序。

### 执行调整

- `src/runtime/engine/runlua.rs` 新增 `lock_runlua_cwd_guard`，通过 `PoisonError::into_inner` 恢复进程级 runlua cwd guard。
- `execute_runlua_wrapper` 改用 `lock_runlua_cwd_guard`，保留原有保存 cwd、切换目录、执行 wrapper、恢复 cwd 的顺序。
- `src/runtime/engine/lease.rs` 的 runtime lease cwd override 改用同一个 `lock_runlua_cwd_guard`，修复共享全局 cwd guard 被 poison 后 lease 路径仍失败的问题。
- `src/runtime/engine/runlua.rs` 新增 `lock_runlua_print_capture`，通过 `PoisonError::into_inner` 恢复单次 runlua print 捕获缓冲区锁。
- runlua 执行后的 `printed_output` 克隆改用 `lock_runlua_print_capture`。
- 覆写的 Lua `print` 函数改用 `lock_runlua_print_capture` 追加捕获文本。
- `src/runtime/engine/tests.rs` 将显式配置重载测试中对 `runlua_cwd_guard` 的直接锁获取改为 `lock_runlua_cwd_guard`，避免新增 poison 恢复测试影响后续测试。
- 新增 `execute_runlua_request_inline_recovers_after_poisoned_cwd_guard`，制造进程级 cwd guard poison 后验证文件型 luaexec 仍可执行并捕获输出。
- 新增 `runlua_print_capture_recovers_after_poisoned_lock`，制造 print capture 锁 poison 后验证恢复 helper 仍可写入与读取。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test execute_runlua_request_inline_recovers_after_poisoned_cwd_guard` 通过，1 个目标测试通过。
- 修改后：`cargo test runlua_print_capture_recovers_after_poisoned_lock` 通过，1 个目标测试通过。
- 修改后：`cargo test runlua` 通过，36 个 runlua 相关测试全部通过。
- 中途全量验证发现 `system_runtime_lease_preserves_explicit_cwd_override` 与 `ffi_system_runtime_session_json_supports_delegated_wrapper_flow` 失败，原因是 runtime lease 仍直接锁同一把已 poison 的 cwd guard；已将 lease 路径同步改用 `lock_runlua_cwd_guard`。
- 回归验证：`cargo test system_runtime_lease_preserves_explicit_cwd_override` 通过。
- 回归验证：`cargo test ffi_system_runtime_session_json_supports_delegated_wrapper_flow` 通过。
- 全量验证：`cargo test` 通过，251 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 空白审核：`git diff --check` 通过。
- 残留搜索：`src/runtime/engine/runlua.rs`、`src/runtime/engine/lease.rs` 与 `src/runtime/engine/tests.rs` 中不再存在 `luaexec cwd guard lock poisoned`、`runtime lease cwd guard lock poisoned`、`runlua print capture lock poisoned` 或 `Failed to lock runlua output capture` 旧分支。

### 代码审核与遗留事项

- 本轮没有改变 runlua 请求解析、Lua wrapper 构造、timeout guard、cwd 保存/恢复顺序、print 文本格式、Markdown 渲染、runtime lease cwd override 语义或 FFI delegated wrapper 行为。
- poison 恢复只发生在已确认的进程级 cwd guard 和单次 runlua print capture buffer 上，不引入备用 cwd、候选执行目录、fallback 输出容器或多来源兼容。
- 修改部分代码审核发现并修复同一 cwd guard 在 runtime lease 路径的遗漏；修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续检查 `runtime/engine/lease.rs` 中 runtime session manager 与 lease session lock 的剩余 poison/try_lock 错误路径。

## 2026-07-05 第 94 轮：恢复 runtime session manager 与单 lease 锁 poison 后的租约可用性

### 问题探索

- 基线延续第 93 轮闭环状态：`cargo test` 通过，251 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮检查 `src/runtime/engine/lease.rs` 中 runtime session manager 与单个 runtime session 的锁处理。
- 已追清 manager state 来源：`RuntimeSessionManager` 持有一个 `Mutex<RuntimeSessionManagerState>`，内部保存 active lease 表、SID 索引、tombstone、generation 和本地递增序列。
- 已追清单 session 来源：每个 active lease 的 `RuntimeSessionEntry` 持有一个 `Arc<Mutex<RuntimeSession>>`，保存持久 Lua VM、SID、lease id、generation、TTL、路径上下文、终态标记和关闭状态。
- 已追清 manager 调用路径：create/replace、get、status、list、close、active snapshot 更新都会先锁 manager state，再按需锁单个 session。
- 已追清 session 调用路径：status、close、eval、replace 旧 lease 检查、expired prune 都会尝试锁单个 session；真正的并发占用表现为 `TryLockError::WouldBlock`。
- 旧实现把 manager state poison 转成 `lease_manager_poisoned`；同时把单 session 的 `TryLockError::Poisoned` 与真正忙碌混在一起，导致 poisoned 但未被占用的 lease 被误判为 busy 或 unavailable。
- 长期优化判断：manager state 与单 session 都是唯一事实容器；`WouldBlock` 才表示正在被其他调用持有，`Poisoned` 携带可恢复 guard，应恢复同一 guard 并保留真正 busy 场景的拒绝语义。

### 执行调整

- `RuntimeSessionManager::lock_state` 改为通过 `PoisonError::into_inner` 恢复 manager state 锁，不再返回 `lease_manager_poisoned`。
- 新增 `RuntimeSessionManager::try_lock_session`，统一区分 `TryLockError::WouldBlock` 与 `TryLockError::Poisoned`：前者继续返回 `lease_busy`，后者恢复 guard。
- `get`、`status`、`close` 和 eval 入口改用 `try_lock_session`，保持忙碌 lease 的稳定 JSON 错误，同时恢复 poisoned lease。
- `insert` 中检查同 SID 旧 lease 时恢复 `TryLockError::Poisoned`，仍然在 `WouldBlock` 时保持 `replace=true` 的 busy 拒绝语义。
- `prune_inactive_locked` 在检查过期 lease 时恢复 `TryLockError::Poisoned`，仍在 `WouldBlock` 时跳过 busy lease。
- `src/runtime/engine/lease.rs` 新增 `runtime_session_manager_state_recovers_after_poisoned_lock`，覆盖 manager state 锁 poison 后仍可访问索引。
- `src/runtime/engine/tests.rs` 新增 `runtime_session_operations_recover_poisoned_session_lock`，覆盖单 session 锁 poison 后 status、eval、close 均可执行。
- `src/runtime/engine/tests.rs` 新增 `runtime_session_replace_recovers_poisoned_existing_session_lock`，覆盖 `replace=true` 恢复 poisoned 旧 session 并退役原 lease。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test runtime_session_manager_state_recovers_after_poisoned_lock` 通过，1 个目标测试通过。
- 修改后：`cargo test runtime_session_operations_recover_poisoned_session_lock` 通过，1 个目标测试通过。
- 修改后：`cargo test runtime_session_replace_recovers_poisoned_existing_session_lock` 通过，1 个目标测试通过。
- 修改后：`cargo test runtime_session` 通过，20 个 runtime_session 相关测试全部通过。
- 全量验证：`cargo test` 通过，254 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 空白审核：`git diff --check` 通过。
- 残留搜索：`src/runtime/engine/lease.rs` 中不再存在旧的 `lock poisoned` 或 `lease_manager_poisoned` 错误分支；剩余 `Poisoned` 命中均为显式恢复分支。

### 代码审核与遗留事项

- 本轮没有改变 runtime session JSON API、SID/generation/profile 校验、busy lease 拒绝、lease 替换规则、TTL 刷新、tombstone 保留、snapshot 排序或 eval 执行语义。
- poison 恢复只发生在已确认的 manager state 与单个 session guard 上，不引入备用 manager、候选 lease、fallback session handle 或多来源兼容。
- 修改部分代码审核确认真正并发占用仍由 `TryLockError::WouldBlock` 返回 `lease_busy`，不会被 poison 恢复逻辑误放行。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可重新进行全仓 poison 搜索，并转向其他非 poison 类坏味道。

## 2026-07-05 第 95 轮：移除 provider FFI C 字符串的有损 UTF-8 解码

### 问题探索

- 基线延续第 94 轮闭环状态：`cargo test` 通过，254 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮转向非 poison 类问题，检查 provider FFI 字符串边界中的 `to_string_lossy`。
- 已追清底层入口：`src/providers/mod.rs::decode_non_null_ffi_c_string` 负责把非空 C 字符串转成 Rust `String`，被 SQLite 与 LanceDB 动态库 provider 调用。
- 已追清 LanceDB 调用点：`take_last_error_message` 读取动态库最后错误文本；`take_owned_string` 读取动态库分配的成功响应字符串并释放原始分配。
- 已追清 SQLite 调用点：`take_last_error_message`、`take_owned_string`、`take_optional_string` 分别处理错误文本、必填 owned string、可选 owned string。
- 已追清 SQLite 可选字符串流向：execute 结果 message、tokenize token、custom word list、search_fts source/query_mode 和各 hit 字段都会经由 `take_optional_string`。
- 旧实现使用 `CStr::to_string_lossy()`，非 UTF-8 字节会被替换字符静默吞掉，导致 provider 返回的数据、路径、title、snippet 或错误文本被篡改后继续流入 JSON。
- 长期优化判断：provider FFI 文本边界应要求精确 UTF-8；错误消息可降级成明确诊断字符串，但业务数据字符串必须返回错误，不能用有损替换或默认值掩盖解码失败。

### 执行调整

- `src/providers/mod.rs::decode_non_null_ffi_c_string` 改为返回 `Result<String, String>`，使用 `CStr::to_str` 严格校验 UTF-8。
- `src/providers/mod.rs` 新增 `provider_ffi_c_string_decodes_valid_utf8`，覆盖普通 UTF-8 C 字符串仍可解码。
- `src/providers/mod.rs` 新增 `provider_ffi_c_string_rejects_invalid_utf8`，覆盖非法 UTF-8 不再有损替换。
- LanceDB `take_last_error_message` 在错误文本非法 UTF-8 时返回明确诊断字符串。
- LanceDB `take_owned_string` 在释放动态库分配后返回严格解码结果，非法 UTF-8 作为错误返回。
- SQLite `take_last_error_message` 在错误文本非法 UTF-8 时返回明确诊断字符串。
- SQLite `take_owned_string` 在释放动态库分配后返回严格解码结果，非法 UTF-8 作为错误返回。
- SQLite `take_optional_string` 改为 `Result<Option<String>, String>`，空指针仍表示 `None`，非空非法 UTF-8 返回错误。
- SQLite `execute_script` 与 `execute_batch` 的结果 message 解码改为可失败闭包，确保 `execute_result_destroy` 在成功和失败路径都会执行。
- SQLite `tokenize_text` token 提取改为显式传播可选字符串解码错误，保留原有 token result handle 销毁逻辑。
- SQLite `list_custom_words` 改为先在可失败闭包中提取 words，再销毁 list handle，最后传播解码错误。
- SQLite `search_fts` 改为在可失败闭包中提取 source、query_mode 与 hit 字段，再销毁 search result handle，最后传播解码错误。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test provider_ffi_c_string` 通过，2 个 provider FFI 字符串测试全部通过。
- 修改后：`cargo test provider` 通过，10 个 provider 相关测试全部通过。
- 修改后：`cargo test sqlite` 通过，2 个 SQLite 相关测试全部通过。
- 修改后：`cargo test lancedb` 通过，1 个 LanceDB 相关测试通过。
- 全量验证：`cargo test` 通过，256 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 空白审核：`git diff --check` 通过。
- 残留搜索：`src/providers`、`src/host`、`src/ffi_standard.rs` 与 `src/ffi.rs` 中不再存在 provider FFI 解码相关 `to_string_lossy`；`decode_non_null_ffi_c_string` 调用点均已处理 `Result`。

### 代码审核与遗留事项

- 本轮没有改变 provider callback 模式、space controller 模式、动态库符号加载、FFI 分配释放函数、SQLite/LanceDB JSON 响应结构或慢日志策略。
- 非 UTF-8 错误消息返回明确诊断字符串；非 UTF-8 业务数据返回错误，不再引入替换字符、默认值掩盖或多来源兼容。
- 修改部分代码审核确认 SQLite execute/search/list 相关原生 result handle 在解码失败时仍会销毁，未引入资源泄漏。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续检查普通 `display()` 暴露到运行时/JSON 表面的路径文本，以及 managed runtime/download/skill manager 的错误边界。

## 2026-07-05 第 96 轮：统一 managed runtime 错误路径渲染

### 问题探索

- 基线延续第 95 轮闭环状态：`cargo test` 通过，256 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮先全仓搜索 `to_string_lossy()`，确认除 `src/runtime/path.rs::render_host_visible_path` 这个集中封装外，业务代码不再直接调用有损路径渲染。
- 随后转向 `src/runtime/managed_runtime.rs` 中的 `display()` 暴露点，确认这些路径进入的是用户可见错误消息，而不是底层文件系统调用。
- 已追清执行流程：`resolve_python_env_plan` 与 `resolve_node_env_plan` 先读取运行时安装 manifest，再解析 skill 内 package/lock 文件，最后构造环境 plan。
- 已追清环境创建流程：`ensure_managed_env` 根据 runtime 类型进入 Python/Node 环境创建，涉及临时 build dir、目标 env dir、marker 写入、目录复制回退与 package manager 命令执行。
- 已追清错误边界：`sha256_file`、`read_managed_env_marker`、`create_node_env`、`prepare_build_dir`、`finish_build_dir`、`write_expected_marker`、`copy_dir_recursive`、`read_install_manifest`、`resolve_install_executable`、`resolve_required_skill_file`、`resolve_optional_skill_file` 都会把路径拼入 `String` 错误文本。
- 旧实现直接使用 `Path::display()`，在 Windows verbatim 路径或宿主可见路径规范上绕过了项目已经建立的 `render_host_visible_path` 统一出口。
- 长期优化判断：managed runtime 计划解析与环境创建属于宿主/用户可见错误面，应该统一使用项目路径渲染规则；实际文件操作仍必须使用原始 `Path`，不能把展示文本反灌回执行路径。

### 执行调整

- `src/runtime/managed_runtime.rs` 引入 `render_host_visible_path`，新增 `render_managed_runtime_path`，作为 managed runtime 用户可见错误消息的本地路径渲染出口。
- `sha256_file` 的读取失败错误改为通过 `render_managed_runtime_path` 输出路径。
- `read_managed_env_marker` 的读取与 JSON 解析失败错误改为通过统一路径渲染输出 marker 路径。
- `create_node_env` 的 env dir 删除、创建、package.json 复制、lockfile 复制错误改为统一路径渲染。
- `prepare_build_dir` 的无父目录、父目录创建、旧 build dir 删除、新 build dir 创建错误改为统一路径渲染。
- `finish_build_dir` 的目标 env dir 删除、copy fallback 清理失败、rename 与 copy fallback 双失败错误改为统一路径渲染。
- `write_expected_marker` 的 marker 写入失败错误改为统一路径渲染。
- `copy_dir_recursive` 的目标目录创建、源目录读取、目录项读取、文件复制失败错误改为统一路径渲染。
- `read_install_manifest` 的 manifest 读取与 JSON 解析失败错误改为统一路径渲染。
- `resolve_install_executable` 的 manifest schema/runtime/version/platform 校验失败，以及 executable 缺失错误改为统一路径渲染。
- `resolve_required_skill_file` 与 `resolve_optional_skill_file` 的文件缺失错误改为统一路径渲染。
- 新增 `python_env_plan_missing_lockfile_error_uses_host_visible_path`，通过完整 Python env plan 解析流程覆盖缺失 lockfile 错误路径与 `render_host_visible_path` 保持一致。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test python_env_plan_missing_lockfile_error_uses_host_visible_path` 通过，1 个目标测试通过。
- 修改后：`cargo test managed_runtime` 通过，11 个 managed runtime 相关测试全部通过。
- 全量验证：`cargo test` 通过，257 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 空白审核：`git diff --check` 通过。
- 残留搜索：`src/runtime/managed_runtime.rs` 中不再存在 `display()` 调用，路径错误文本已集中走 `render_managed_runtime_path`。

### 代码审核与遗留事项

- 本轮没有改变 managed runtime 的环境 hash、安装 manifest 协议、package manager 调用、marker schema、目录布局、copy fallback 策略或 skill 相对路径安全校验。
- 统一渲染只作用于错误文本，文件读取、写入、复制、重命名、命令工作目录仍使用原始 `Path`/`PathBuf`，没有引入展示路径与执行路径混用。
- 修改部分代码审核确认新测试走完整 `resolve_python_env_plan` 流程，不是单独测试私有格式化函数，能覆盖真实调用链中的缺失 lockfile 错误。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续检查 `src/download/manager.rs`、`src/skill/manager.rs` 与 `src/runtime/engine/lease.rs` 中的用户可见 `display()` 错误边界。

## 2026-07-05 第 97 轮：统一 download manager 错误路径渲染

### 问题探索

- 基线延续第 96 轮闭环状态：`cargo test` 通过，257 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮比较 `src/download/manager.rs`、`src/skill/manager.rs`、`src/runtime/engine/lease.rs` 的 `display()` 使用面，确认下载层问题更集中，适合作为 skill install/update 前置基础能力先收敛。
- 已追清下载流程：`download` 先检查网络策略，再创建 cache root，命中缓存则返回缓存路径，否则执行 HTTP 下载并写入缓存文件。
- 已追清文本下载流程：`fetch_text_fresh` 会删除旧缓存、重新下载再读取 UTF-8 文本；`fetch_text` 通过 `download` 获取缓存路径后读取文本。
- 已追清校验流程：`download_with_sha256` 调用 `verify_file_sha256`，首轮校验失败会删除缓存并自动重下，重下后仍失败才合并两次校验错误返回。
- 已追清校验工具边界：`verify_file_sha256` 会校验期望 SHA-256 格式、读取文件、计算实际 SHA-256，并在格式错误、读取失败、摘要不匹配时把路径拼进错误文本。
- 旧实现直接使用 `Path::display()`，绕过了项目中已经建立的 `render_host_visible_path` 统一路径展示规则。
- 长期优化判断：下载缓存路径和校验文件路径属于用户/宿主可见错误面，应统一展示规则；缓存路径构造、HTTP 请求、文件读写和删除仍必须继续使用原始 `Path`。

### 执行调整

- `src/download/manager.rs` 引入 `render_host_visible_path`，新增 `render_download_path`，作为下载层用户可见错误消息的本地路径渲染出口。
- `download` 创建 cache root 失败错误改为通过 `render_download_path` 输出路径。
- `download` 写入缓存文件失败错误改为通过 `render_download_path` 输出目标路径。
- `fetch_text_fresh` 删除旧缓存失败错误改为通过 `render_download_path` 输出缓存路径。
- `fetch_text_fresh` 读取重新下载后的文本失败错误改为通过 `render_download_path` 输出下载路径。
- `fetch_text` 读取缓存文本失败错误改为通过 `render_download_path` 输出缓存路径。
- `verify_file_sha256` 的期望 checksum 格式错误、文件读取失败、checksum mismatch 错误均改为通过 `render_download_path` 输出文件路径。
- 新增 `file_sha256_verification_mismatch_error_uses_host_visible_path`，通过真实文件读取与 SHA-256 计算覆盖 checksum mismatch 错误路径与 `render_host_visible_path` 保持一致。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test file_sha256_verification_mismatch_error_uses_host_visible_path` 通过，1 个目标测试通过。
- 修改后：`cargo test download` 通过，4 个 download 相关测试全部通过。
- 全量验证：`cargo test` 通过，258 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 空白审核：`git diff --check` 通过。
- 残留搜索：`src/download/manager.rs` 中不再存在 `display()` 调用，下载层路径错误文本已集中走 `render_download_path`。

### 代码审核与遗留事项

- 本轮没有改变下载缓存 key、扩展名推断、网络门禁、HTTP 客户端构造、GitHub release 解析、checksum manifest 解析、自动重下载策略或文件删除策略。
- 统一渲染只作用于错误文本，缓存目录创建、文件写入、读取、删除和 SHA-256 计算仍使用原始 `Path`/`PathBuf`，没有引入展示路径与执行路径混用。
- 修改部分代码审核确认新增测试使用真实 payload 内容触发 mismatch 分支，不是单独测试格式化函数，能覆盖 `download_with_sha256` 复用的底层错误来源。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续检查 `src/skill/manager.rs` 的 install/update/uninstall 流程错误路径，以及 `src/runtime/engine/lease.rs` 中 runtime session JSON 错误面。

## 2026-07-05 第 98 轮：统一 skill manager 生命周期错误路径渲染

### 问题探索

- 基线延续第 97 轮闭环状态：`cargo test` 通过，258 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮完整搜索 `src/skill/manager.rs` 的 `display()` 使用点，确认路径展示分布在生命周期状态文件、安装暂存、更新暂存、卸载暂存、提交/回滚、manifest 解析和根目录扫描多个真实链路。
- 已追清状态文件流程：`ensure_state_layout` 创建 disabled 与 install-record 根目录；`disable_skill`、`enable_skill`、`disabled_record`、`install_record`、`persist_*_record`、`remove_*_record` 负责 JSON/YAML 状态读写删除。
- 已追清卸载流程：`prepare_uninstall_skill_at_path_in_plane` 将当前 skill 目录移动到 uninstall backup；commit 阶段删除 backup，失败时恢复 disabled/install record；rollback 阶段恢复 backup 到原 target。
- 已追清安装流程：`stage_skill_install_from_archive` 创建 install temp root、解压、读取 manifest、校验目录派生 skill_id 与版本，再移动到目标 skill 根。
- 已追清更新流程：`stage_skill_update_from_archive` 创建 update temp root、解压、校验 manifest、备份当前 skill、移动新版本到目标目录，并在移动失败时恢复 backup。
- 已追清 manifest 与根目录发现流程：`read_skill_manifest_from_directory` 读取/解析/解码 `skill.yaml`，`collect_named_skill_dirs` 扫描 skill 根目录，`is_effective_disable_override` 与 `is_skill_manifest_enabled` 决定 override 与 manifest enable 状态。
- 旧实现直接把 `Path::display()` 放入错误文本，绕过了 `render_host_visible_path` 统一规则；这些错误文本会向 host/runtime 管理 API 暴露。
- 长期优化判断：skill manager 是用户可见生命周期控制面，应统一路径展示规则；所有实际 `fs` 读写、rename、remove、manifest 解析仍必须继续使用原始 `Path`/`PathBuf`。

### 执行调整

- `src/skill/manager.rs` 引入 `render_host_visible_path`，新增 `render_skill_manager_path`，作为 skill manager 用户可见错误消息的本地路径渲染出口。
- `ensure_state_layout` 创建 disabled root 与 install-record root 的错误路径改为统一渲染。
- disabled 状态记录的写入、删除、读取、解析错误路径改为统一渲染。
- uninstall 准备阶段的 backup parent 创建失败、当前 skill 移入 uninstall backup 失败错误路径改为统一渲染。
- install 暂存阶段的 temp root 删除/创建、目标目录已存在、解压目录移入目标目录失败错误路径改为统一渲染。
- update 暂存阶段的 temp root 删除/创建、目标目录缺失、backup parent 创建、当前 skill 移入 backup、更新目录移入目标目录失败错误路径改为统一渲染。
- install record 的读取、解析、写入、删除错误路径改为统一渲染。
- disabled record 恢复路径中的写入、删除错误路径改为统一渲染。
- commit/rollback 阶段的 update backup 删除、安装目录回滚删除、更新目录回滚删除、backup 恢复、uninstall backup 删除、uninstall target 删除、uninstall backup 恢复错误路径改为统一渲染。
- `read_skill_manifest_from_directory` 的目录名解析、`skill.yaml` 读取/解析/解码、禁止显式 `skill_id`、最终 skill_id 不一致错误路径改为统一渲染。
- `collect_named_skill_dirs`、`is_effective_disable_override`、`is_skill_manifest_enabled` 的根目录读取、override dir 读取、`skill.yaml` 读取/解析/禁止显式 `skill_id`/enable probe 解析错误路径改为统一渲染。
- 新增 `disabled_record_parse_error_uses_host_visible_path`，通过真实 disabled record 文件解析失败覆盖状态读取错误路径与 `render_host_visible_path` 保持一致。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test disabled_record_parse_error_uses_host_visible_path` 通过，1 个目标测试通过。
- 修改后：`cargo test skill_manager` 通过，1 个名称过滤测试通过。
- 修改后：`cargo test skill::manager` 通过，11 个 skill manager 模块测试全部通过。
- 全量验证：`cargo test` 通过，259 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 空白审核：`git diff --check` 通过。
- 残留搜索：`src/skill/manager.rs` 中不再存在 `display()` 调用，skill manager 路径错误文本已集中走 `render_skill_manager_path`。

### 代码审核与遗留事项

- 本轮没有改变 skill 生命周期状态机、ROOT 平面保护、managed install/update 来源策略、下载器配置、归档解压、manifest 解析结构、提交/回滚顺序、backup 目录布局或 enable override 语义。
- 统一渲染只作用于错误文本，所有 `fs::create_dir_all`、`fs::write`、`fs::read_to_string`、`fs::remove_file`、`fs::remove_dir_all`、`fs::rename` 和 manifest 解码仍使用原始 `Path`/`PathBuf`。
- 修改部分代码审核确认新增测试走真实 `SkillManager::disabled_record` 读取流程，不是单独测试私有格式化函数，能覆盖生命周期状态错误面。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续检查 `src/runtime/engine/lease.rs` 的 runtime session JSON 错误路径，以及全仓剩余 `display()` 是否仍暴露到用户可见面。

## 2026-07-05 第 99 轮：统一 runtime lease 错误路径渲染

### 问题探索

- 基线延续第 98 轮闭环状态：`cargo test` 通过，259 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮检查 `src/runtime/engine/lease.rs` 中剩余 `display()`，并结合 `src/runtime/engine.rs` 中既有 `render_log_friendly_path` 与 `render_host_visible_path` 使用方式确认上下文。
- 已追清 JSON payload 路径状态：`session_status_cwd_text`、`session_status_workspace_root_text`、`session_status_system_lua_lib_text` 已经通过 `render_host_visible_path` 输出宿主可见路径。
- 已追清 runtime lease 创建流程：system runtime profile 会调用 `resolve_system_lua_lib_dir`，随后 `create_dir_all` 确保固定系统 Lua 库目录存在，失败错误会进入 create JSON 调用链。
- 已追清 eval cwd 流程：`eval_lua_value_with_optional_cwd` 在执行 Lua wrapper 前锁住 cwd guard、读取原 cwd、切换到 lease cwd、执行后恢复原 cwd；`set_current_dir` 失败会返回 `mlua::Error::runtime`。
- 已区分剩余 `display()`：`configure_runtime_lease_vm` 中的 `root.display()` 用于构造 Lua `package.cpath`/`package.path` 搜索模式，是实际模块加载语义，不是错误文本。
- 旧错误文本中的 `system_lua_lib_dir` 与 `runtime lease set cwd` 仍直接使用 `Path::display()`，绕过了统一宿主可见路径渲染。
- 长期优化判断：runtime lease JSON/错误面应统一路径展示；Lua package 搜索模式属于执行语义，不能在没有专门验证 Lua loader 行为前混入用户展示规则。

### 执行调整

- `src/runtime/engine/lease.rs` 中 `resolve_runtime_lease_path_context` 创建 `system_lua_lib_dir` 失败错误改为通过 `render_host_visible_path` 输出路径。
- `src/runtime/engine/lease.rs` 中 `eval_lua_value_with_optional_cwd` 的 `runtime lease set cwd` 失败错误改为通过 `render_host_visible_path` 输出 cwd。
- 保留 `configure_runtime_lease_vm` 中用于 Lua `package.path`/`package.cpath` 搜索模式拼接的 `display()`，不把错误文本治理误扩散到模块加载执行语义。
- 新增 `runtime_lease_cwd_error_uses_host_visible_path`，通过把普通文件作为 cwd 触发真实 `set_current_dir` 失败，验证错误文本包含 `render_host_visible_path` 输出。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test runtime_lease_cwd_error_uses_host_visible_path` 通过，1 个目标测试通过。
- 修改后：`cargo test runtime_session` 通过，20 个 runtime_session 相关测试全部通过。
- 修改后：`cargo test system_runtime` 通过，4 个 system runtime 相关测试全部通过。
- 全量验证：`cargo test` 通过，260 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 空白审核：`git diff --check` 通过。
- 残留搜索：`src/runtime/engine/lease.rs` 剩余 `display()` 仅位于 Lua package 搜索路径模式构造，不在错误文本路径中。

### 代码审核与遗留事项

- 本轮没有改变 runtime session lease 状态机、SID/generation/profile 校验、TTL 语义、system runtime 固定目录选择、Lua VM 创建、cwd guard 锁恢复、执行后 cwd 恢复或 package path/cpath 搜索模式。
- 统一渲染只作用于错误文本，`create_dir_all` 与 `set_current_dir` 仍使用原始 `Path`/`PathBuf`。
- 修改部分代码审核确认新增测试走真实 cwd 切换失败路径，不是单独测试私有格式化函数，能覆盖 runtime lease eval 错误面。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可进行全仓 `display()` 残留复查，区分用户可见错误文本与执行语义路径字符串。

## 2026-07-05 第 100 轮：统一 download archive 错误路径渲染

### 问题探索

- 基线延续第 99 轮闭环状态：`cargo test` 通过，260 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮全仓复查 `display()` 后，确认 `src/download/archive.rs` 是剩余路径错误文本最集中的下载前置模块之一。
- 已追清 raw 安装流程：`install_downloaded_payload` 先创建 install root，再按 archive type 分派；raw payload 通过 `copy_file_with_parent_dir` 复制到目标导出路径，并按需 chmod。
- 已追清 skill zip 解包流程：`extract_skill_package_zip` 创建 temp root、打开 zip、逐条校验顶层目录、创建目标目录/文件、复制 entry 内容，最后检查 `skill.yaml`。
- 已追清普通 zip 导出流程：`install_from_zip_archive` 打开 zip、按声明导出路径解析 entry、创建父目录与目标文件、复制 entry 内容、按需 chmod。
- 已追清 tar.gz 导出流程：`install_from_tar_gz_archive` 读取归档、枚举 tar entry、严格规范化 UTF-8 entry 路径、匹配导出声明、创建目标文件并写入、检查所有导出是否存在。
- 已追清工具函数错误边界：`copy_file_with_parent_dir` 创建父目录与复制失败会输出路径；`mark_executable_if_needed` 在 Unix 上 stat/chmod 失败会输出路径。
- 旧实现直接使用 `Path::display()` 拼接归档、安装根、临时根、目标文件、父目录、复制源与复制目标路径，绕过统一宿主可见路径规则。
- 长期优化判断：归档安装和 skill zip 解包错误最终会反馈给依赖安装或 skill lifecycle 调用方，应统一展示路径；归档 entry 匹配、文件创建、复制、chmod 仍必须使用原始路径和既有安全校验。

### 执行调整

- `src/download/archive.rs` 引入 `render_host_visible_path`，新增 `render_archive_path`，作为归档层用户可见错误消息的本地路径渲染出口。
- `install_downloaded_payload` 创建 install root 失败错误改为通过 `render_archive_path` 输出路径。
- `extract_skill_package_zip` 的 temp root 创建、archive 打开、顶层目录不匹配、entry 目录创建、父目录创建、目标文件创建、entry 复制、缺失 `skill.yaml` 错误路径改为统一渲染。
- `install_from_zip_archive` 的 archive 打开、entry 缺失、entry 读取、父目录创建、目标文件创建、entry 复制错误路径改为统一渲染。
- `install_from_tar_gz_archive` 的 archive 读取、entry 枚举、父目录创建、目标文件创建、entry 读取、目标写入、缺失导出错误路径改为统一渲染。
- `copy_file_with_parent_dir` 的父目录创建与文件复制错误路径改为统一渲染。
- `mark_executable_if_needed` 的 Unix stat/chmod 错误路径改为统一渲染。
- 新增 `install_root_create_error_uses_host_visible_path`，通过把普通文件作为 install root 触发真实 `create_dir_all` 失败，验证错误文本与 `render_host_visible_path` 保持一致。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test install_root_create_error_uses_host_visible_path` 通过，1 个目标测试通过。
- 修改后：`cargo test download` 通过，5 个 download 相关测试全部通过。
- 全量验证：`cargo test` 通过，261 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 空白审核：`git diff --check` 通过。
- 残留搜索：`src/download/archive.rs` 中不再存在 `display()` 调用，归档层路径错误文本已集中走 `render_archive_path`。

### 代码审核与遗留事项

- 本轮没有改变 raw/zip/tar.gz 分派、zip entry traversal 防护、tar entry UTF-8 要求、导出路径匹配、顶层 skill 目录校验、父目录创建顺序、文件复制、chmod 语义或缺失导出判定。
- 统一渲染只作用于错误文本，所有归档读取、entry 匹配、文件创建、复制、写入、metadata 与 chmod 操作仍使用原始 `Path`/`PathBuf`。
- 修改部分代码审核确认新增测试走真实 `install_downloaded_payload` 入口，不是单独测试私有格式化函数，能覆盖依赖安装入口的错误面。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续检查 `src/dependency/manager.rs`、`src/runtime/config.rs` 与 provider 动态库加载错误中的路径展示。

## 2026-07-05 第 101 轮：统一 dependency manager 错误路径渲染

### 问题探索

- 基线延续第 100 轮闭环状态：`cargo test` 通过，261 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮检查 `src/dependency/manager.rs` 的 `display()` 残留，确认剩余路径展示集中在依赖根清理与目录创建错误文本。
- 已追清更新清理流程：`cleanup_updated_skill_dependencies` 收集旧 manifest 与新 manifest 对应的 skill-local dependency roots，删除旧集合中不再被新集合使用的 stale root。
- 已追清卸载清理流程：`cleanup_uninstalled_skill_dependencies_from_roots` 调用 `remove_skill_private_dependency_roots`，删除目标 skill 在 tool/lua/ffi 三类依赖根下的私有目录。
- 已追清公共目录保障入口：`ensure_directory` 是依赖管理器外部复用的根目录创建辅助函数，创建失败会把目标路径放入错误文本。
- 旧实现直接使用 `Path::display()` 输出 stale root、私有依赖 root 与公共目录 root，绕过统一宿主可见路径规则。
- 长期优化判断：依赖清理与目录创建错误会反馈给 dependency install/cleanup 调用方，应统一路径展示；依赖根计算、集合差异、目录存在性检查和删除操作仍必须使用原始 `Path`/`PathBuf`。

### 执行调整

- `src/dependency/manager.rs` 引入 `render_host_visible_path`，新增 `render_dependency_manager_path`，作为依赖管理器用户可见错误消息的本地路径渲染出口。
- `cleanup_updated_skill_dependencies` 删除 stale dependency root 失败错误改为通过 `render_dependency_manager_path` 输出路径。
- `remove_skill_private_dependency_roots` 删除 tool/lua/ffi 私有依赖根失败错误改为通过 `render_dependency_manager_path` 输出路径。
- `ensure_directory` 创建目录失败错误改为通过 `render_dependency_manager_path` 输出路径。
- 新增 `ensure_directory_create_error_uses_host_visible_path`，通过把普通文件作为目录根触发真实 `create_dir_all` 失败，验证错误文本与 `render_host_visible_path` 保持一致。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test ensure_directory_create_error_uses_host_visible_path` 通过，1 个目标测试通过。
- 修改后：`cargo test dependency::manager` 通过，11 个 dependency manager 模块测试全部通过。
- 全量验证：`cargo test` 通过，262 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 空白审核：`git diff --check` 通过。
- 残留搜索：`src/dependency/manager.rs` 中不再存在 `display()` 调用，依赖管理器路径错误文本已集中走 `render_dependency_manager_path`。

### 代码审核与遗留事项

- 本轮没有改变依赖安装根布局、scope 规则、manifest 差异计算、stale root 判定、卸载时 tool/lua/ffi 私有根删除顺序、下载器配置或目录存在性判断。
- 统一渲染只作用于错误文本，`fs::remove_dir_all` 与 `fs::create_dir_all` 仍使用原始 `Path`/`PathBuf`。
- 修改部分代码审核确认新增测试走真实 `ensure_directory` 辅助函数，不是单独测试私有格式化函数，能覆盖依赖目录创建错误面。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续检查 `src/runtime/config.rs` 的 skill config IO 错误路径，以及 provider 动态库加载错误中的路径展示。

## 2026-07-05 第 102 轮：统一 skill config IO 错误路径渲染

### 问题探索

- 基线延续第 101 轮闭环状态：`cargo test` 通过，262 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮检查 `src/runtime/config.rs` 的 `display()` 残留，确认路径展示全部集中在统一 skill config 文件的读取、解析、父目录创建、临时文件写入与替换错误文本。
- 已追清读取流程：`list_entries`、`list_skill_values`、`get_value` 等入口经 `with_document_read` 获取有效配置文件路径、共享文件锁，再由 `read_document_from` 读取 JSON 文档；缺失文件视为空文档。
- 已追清写入流程：`set_value`、`delete_value` 经 `with_document_mut` 获取共享文件锁、读取文档、修改内存结构，再由 `write_document_to` 先写临时文件，最后原子替换目标文件。
- 已追清写入错误边界：`write_document_to` 会校验目标文件父目录、创建父目录、创建 temp file、write、flush、sync，再调用 `replace_file_atomically` 推进临时文件。
- 旧实现直接使用 `Path::display()` 输出配置文件路径、父目录、临时文件路径与目标文件路径，绕过统一宿主可见路径规则。
- 长期优化判断：skill config 错误会直接反馈给 Lua `vulcan.config.*`、宿主和 FFI 配置管理面，应统一展示路径；锁键归一化、实际文件读写、临时文件路径和原子替换仍必须使用原始 `Path`/`PathBuf`。

### 执行调整

- `src/runtime/config.rs` 引入 `render_host_visible_path`，新增 `render_skill_config_path`，作为 skill config 用户可见错误消息的本地路径渲染出口。
- `read_document_from` 的配置文件读取失败与 JSON 解析失败错误路径改为通过 `render_skill_config_path` 输出。
- `write_document_to` 的无父目录错误、父目录创建失败错误改为通过 `render_skill_config_path` 输出。
- `write_document_to` 的 temp file 创建、写入、flush、sync 失败错误路径改为通过 `render_skill_config_path` 输出。
- `write_document_to` 的 temp file promote 到目标文件失败错误改为同时通过 `render_skill_config_path` 输出临时路径和目标路径。
- 新增 `skill_config_parse_error_uses_host_visible_path`，通过写入非法 JSON 并调用真实 `SkillConfigStore::list_entries` 读取路径，验证解析错误与 `render_host_visible_path` 保持一致。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test skill_config_parse_error_uses_host_visible_path` 通过，1 个目标测试通过。
- 修改后：`cargo test skill_config` 通过，25 个 skill_config 相关测试全部通过。
- 全量验证：`cargo test` 通过，263 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 空白审核：`git diff --check` 通过。
- 残留搜索：`src/runtime/config.rs` 中不再存在 `display()` 调用，skill config IO 错误文本已集中走 `render_skill_config_path`。

### 代码审核与遗留事项

- 本轮没有改变配置文件路径解析、默认 runtime root 语义、共享锁键归一化、poison 恢复、JSON 文档结构、临时文件扩展名、写入/flush/sync 顺序或原子替换策略。
- 统一渲染只作用于错误文本，`fs::read_to_string`、`fs::create_dir_all`、`fs::File::create`、`write_all`、`flush`、`sync_all` 与 `replace_file_atomically` 仍使用原始 `Path`/`PathBuf`。
- 修改部分代码审核确认新增测试走真实 `SkillConfigStore::list_entries` 读取流程，不是单独测试私有格式化函数，能覆盖宿主可见配置读取错误面。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续检查 provider 动态库加载错误、host database 错误与 `luaskills-debug` CLI 错误路径。

## 2026-07-05 第 103 轮：统一 provider 与 database binding 错误路径渲染

### 问题探索

- 基线延续第 102 轮闭环状态：`cargo test` 通过，263 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮检查 `src/providers/lancedb.rs`、`src/providers/sqlite.rs` 与 `src/host/database.rs` 的 `display()` 残留，确认路径展示集中在动态库加载、符号加载、provider 存储目录创建和数据库绑定计划错误文本。
- 已追清 LanceDB 动态库加载流程：`LoadedLanceDbApi::load` 先检查 library path 是否存在，再用 `libloading::Library::new` 加载动态库，随后 `from_library` 逐个解析必需符号。
- 已追清 SQLite 动态库加载流程：`LoadedSqliteApi::load` 与 LanceDB 同样执行路径存在性检查、动态库加载和符号解析。
- 已追清 provider 注册流程：`register_skill` 通过 `build_runtime_database_binding_plan` 获取 provider storage dir 与默认数据库路径，动态库模式下会先创建 provider 存储目录再创建 runtime/database handle。
- 已追清共享绑定计划流程：`build_runtime_database_binding_plan` 从物理 `skill_dir` 提取目录名与 skills root，派生 sidecar database root、provider storage dir、默认数据库路径和宿主绑定上下文。
- 旧实现直接使用 `Path::display()` 输出动态库路径、符号加载来源库路径、SQLite/LanceDB provider 存储目录和非法 skill 目录路径，绕过统一宿主可见路径规则。
- 长期优化判断：provider 初始化错误与 database binding plan 错误会直接进入宿主启动、Lua 数据库状态或管理 API，应统一展示路径；动态库加载、符号解析、目录创建和绑定上下文路径本体仍必须使用原始 `Path`/`PathBuf`。

### 执行调整

- `LoadedLanceDbApi::load` 的动态库路径不存在错误与动态库加载失败错误改为通过 `render_host_visible_path` 输出路径。
- `LoadedLanceDbApi::from_library` 的必需符号加载失败错误改为通过 `render_host_visible_path` 输出库路径。
- `LanceDbSkillHost::register_skill` 的 provider 存储目录创建失败错误改为通过 `render_host_visible_path` 输出路径。
- `LoadedSqliteApi::load` 的动态库路径不存在错误与动态库加载失败错误改为通过 `render_host_visible_path` 输出路径。
- `LoadedSqliteApi::from_library` 的必需符号加载失败错误改为通过 `render_host_visible_path` 输出库路径。
- `SqliteSkillHost::register_skill` 的 provider 存储目录创建失败错误改为通过 `render_host_visible_path` 输出路径。
- `build_runtime_database_binding_plan` 的非法 skill 目录名错误与非法 skill root 错误改为通过 `render_host_visible_path` 输出 skill 目录路径。
- 新增 `lancedb_missing_library_error_uses_host_visible_path`，覆盖 LanceDB 动态库缺失预检错误路径。
- 新增 `sqlite_missing_library_error_uses_host_visible_path`，覆盖 SQLite 动态库缺失预检错误路径。
- 新增 `database_binding_plan_invalid_skill_dir_error_uses_host_visible_path`，覆盖共享 database binding plan 的非法 skill 目录错误路径。

### 验证记录

- 修改后：首次并行定点测试暴露测试写法问题：`expect_err` 要求成功类型实现 `Debug`，而 provider API 表不应为了测试派生 Debug。
- 自动修复：将 LanceDB/SQLite 动态库缺失测试改为显式 `match` 提取错误，保持生产类型定义不变。
- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test lancedb_missing_library_error_uses_host_visible_path` 通过，1 个目标测试通过。
- 修改后：`cargo test sqlite_missing_library_error_uses_host_visible_path` 通过，1 个目标测试通过。
- 修改后：`cargo test database_binding_plan_invalid_skill_dir_error_uses_host_visible_path` 通过，1 个目标测试通过。
- 修改后：`cargo test provider` 通过，12 个 provider 相关测试全部通过。
- 修改后：`cargo test database_binding_plan` 通过，2 个 database binding plan 测试全部通过。
- 修改后：`cargo test sqlite` 通过，3 个 SQLite 相关测试全部通过。
- 修改后：`cargo test lancedb` 通过，2 个 LanceDB 相关测试全部通过。
- 全量验证：`cargo test` 通过，266 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 空白审核：`git diff --check` 通过。
- 残留搜索：`src/providers/lancedb.rs`、`src/providers/sqlite.rs` 与 `src/host/database.rs` 中不再存在 `display()` 调用。

### 代码审核与遗留事项

- 本轮没有改变动态库选择策略、`libloading` 调用、符号集合、FFI API 表生命周期、provider binding registry、callback/space-controller 模式、database sidecar 目录布局或默认数据库路径规则。
- 统一渲染只作用于错误文本，动态库加载、符号解析、provider 目录创建和 binding plan 路径派生仍使用原始 `Path`/`PathBuf`。
- 修改部分代码审核确认新增测试走真实 provider loader 与 database binding planner，不是单独测试格式化函数，能覆盖宿主可见 provider 初始化错误面。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续检查 `luaskills-debug` CLI 错误路径、`runtime/engine.rs` 剩余错误文本，以及 `skill/dependencies.rs`/`skill/manifest.rs` 的路径展示。
## 2026-07-05 第 104 轮：统一 skill dependency 与 manifest schema 错误路径渲染

### 问题探索

- 基线延续第 103 轮闭环状态：`cargo test` 通过，266 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮复查 `src/skill/dependencies.rs` 与 `src/skill/manifest.rs` 的 `display()` 残留，确认路径展示集中在依赖清单读取/解析错误，以及 entry 外部 `input_schema_file` 读取/解析错误。
- 已追清依赖清单读取流程：`SkillDependencyManifest::load_from_path` 直接读取调用方传入的 `dependencies.yaml` 路径，再用 `serde_yaml::from_str` 解析，错误会返回给依赖安装与技能生命周期调用方。
- 已追清外部 schema 解析流程：`SkillMeta::resolve_entry_input_schemas` 遍历 entries，`resolve_entry_input_schema` 对 `input_schema_file` 分支调用 `load_entry_input_schema_file`，再以 `skill_dir.join(relative_path)` 定位 schema 文件并解析 JSON。
- 旧实现直接使用 `Path::display()` 输出清单路径和 schema 路径，绕过统一宿主可见路径渲染规则；该错误文本属于用户可见诊断面。
- 长期优化判断：依赖清单与外部 schema 文件解析错误应统一使用 `render_host_visible_path` 输出路径；真实文件读取、YAML/JSON 解析和 relative path join 语义仍必须保持原始 `Path`/`PathBuf`。

### 执行调整

- `src/skill/dependencies.rs` 引入 `render_host_visible_path`，将 `SkillDependencyManifest::load_from_path` 中读取失败与解析失败的路径渲染改为统一宿主可见路径输出。
- `src/skill/manifest.rs` 引入 `render_host_visible_path`，将 `load_entry_input_schema_file` 中 schema 读取失败与 JSON 解析失败的路径渲染改为统一宿主可见路径输出。
- 新增 `dependency_manifest_parse_error_uses_host_visible_path`，通过真实 `SkillDependencyManifest::load_from_path` 读取非法 `dependencies.yaml` 覆盖依赖清单解析错误路径。
- 新增 `skill_meta_schema_parse_error_uses_host_visible_path`，通过真实 `SkillMeta::resolve_entry_input_schemas` 解析非法外部 schema 覆盖 entry schema 错误路径。

### 验证记录

- 修改后：`cargo test dependency_manifest_parse_error_uses_host_visible_path` 通过，1 个目标测试通过。
- 修改后：`cargo test skill_meta_schema_parse_error_uses_host_visible_path` 首次暴露测试夹具缺失 `lua_module` 字段，已补齐以对齐真实 `SkillEntryMeta` 结构。
- 修改后：`cargo test skill_meta_schema_parse_error_uses_host_visible_path` 再次暴露测试期望路径与生产 `skill_dir.join("schemas/broken.schema.json")` 语义不一致，已修正测试期望路径以匹配真实执行流程。
- 修改后：`cargo test skill_meta_schema_parse_error_uses_host_visible_path` 通过，1 个目标测试通过。
- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test skill::dependencies` 通过，4 个 dependency manifest 相关测试全部通过。
- 修改后：`cargo test skill::manifest` 通过，8 个 skill manifest 相关测试全部通过。
- 全量验证：`cargo test` 通过，268 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 空白审核：`git diff --check` 通过。
- 残留搜索：`src/skill/dependencies.rs` 与 `src/skill/manifest.rs` 中不再存在 `display()` 调用。

### 代码审核与遗留事项

- 本轮没有改变依赖清单格式、依赖分组结构、YAML/JSON 解析规则、entry schema 内联/外部分支选择、`skill_dir.join(relative_path)` 解析语义或 `SkillEntryMeta` 字段要求。
- 统一渲染只作用于错误文本，所有 `fs::read_to_string`、`serde_yaml::from_str`、`serde_json::from_str` 和路径 join 仍使用原始 `Path`/`PathBuf` 与原始文本内容。
- 修改部分代码审核确认新增测试走真实 loader/resolver 入口，不是单独测试私有格式化函数；两次测试失败均源于测试夹具未准确复刻真实执行结构，已按源码事实自动修正。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续检查 `src/bin/luaskills-debug.rs` 和 `src/runtime/engine.rs` 中剩余用户可见错误文本的路径展示与执行语义路径是否混杂。

## 2026-07-05 第 105 轮：统一 luaskills-debug 错误路径渲染

### 问题探索

- 基线延续第 104 轮闭环状态：`cargo test` 通过，268 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮复查 `src/bin/luaskills-debug.rs` 的 `display()` 残留，确认它们分布在预同步 skill 缺失、skill manifest 读取/解析、运行时目录创建、同步目录删除、canonicalize、递归复制、runtime skills 目录扫描和 args 文件读取/解析错误文本中。
- 已追清 sync 流程：`sync_debug_skill` 先绝对化 runtime root 与 source skill path，加载并绑定 source manifest，确保 debug runtime 布局，再把源 skill 同步进 `runtime_root/skills/<skill-id>`。
- 已追清 prepare 流程：`prepare_debug_runtime` 在提供 source path 时复用 sync 结果，在只提供 skill id 时检查已同步目录，随后通过正常 `LuaEngine::load_from_roots` 加载 runtime root。
- 已追清目录复制流程：`copy_directory_recursive` 创建目标目录、枚举源目录、拒绝符号链接、递归复制目录和普通文件；所有实际复制仍依赖原始 `Path`/`PathBuf`。
- 已追清 args 加载流程：`load_invocation_args` 在 `--args-file` 分支读取文件文本并解析 JSON，错误直接反馈给 `call` 命令调用方。
- 旧实现部分输出 payload 已使用 `render_host_visible_path`，但错误分支仍直接使用 `Path::display()`，导致 CLI 面向开发者的诊断路径规则不一致。
- 长期优化判断：`luaskills-debug` 是操作者直接使用的调试入口，用户可见错误文本应统一走宿主可见路径渲染；同步、canonicalize、读取、复制和 JSON/YAML 解析仍必须使用原始路径与原始内容。

### 执行调整

- 新增 `render_debug_path`，作为 `luaskills-debug` 用户可见诊断路径的本地统一出口，内部复用 `render_host_visible_path`。
- 将预同步 skill 缺失错误中的 synced skill path 与 runtime root 改为通过 `render_debug_path` 输出。
- 将 `load_bound_skill_manifest` 中非目录、非 UTF-8 目录名、manifest 读取失败和 manifest 解析失败的路径改为通过 `render_debug_path` 输出。
- 将 `ensure_debug_runtime_layout` 创建 runtime 目录失败错误路径改为通过 `render_debug_path` 输出。
- 将 `synchronize_skill_into_runtime_root` 删除旧同步目录失败错误路径改为通过 `render_debug_path` 输出。
- 将 `paths_refer_to_same_directory` 中 source/target canonicalize 失败错误路径改为通过 `render_debug_path` 输出。
- 将 `copy_directory_recursive` 中目标目录创建、源目录枚举、目录项读取、entry 类型检查、符号链接拒绝和文件复制错误路径改为通过 `render_debug_path` 输出。
- 将 `collect_ignored_skill_ids` 中 runtime skills 目录枚举、目录项读取和 entry 类型检查错误路径改为通过 `render_debug_path` 输出。
- 将 `load_invocation_args` 中 args 文件读取失败和解析失败错误路径改为通过 `render_debug_path` 输出。
- 新增 `load_bound_skill_manifest_parse_error_uses_host_visible_path`，通过真实 debug manifest loader 读取非法 `skill.yaml` 覆盖 manifest 解析错误路径。
- 新增 `load_invocation_args_file_parse_error_uses_host_visible_path`，通过真实 args-file 加载入口读取非法 JSON 覆盖参数文件解析错误路径。

### 验证记录

- 修改后：首次并行目标测试暴露测试模块显式导入缺失，`load_bound_skill_manifest` 与 `render_debug_path` 未进入 `mod tests` 作用域。
- 自动修复：按现有测试模块白名单导入风格补充 `load_bound_skill_manifest` 与 `render_debug_path`，未改变生产 API。
- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test load_bound_skill_manifest_parse_error_uses_host_visible_path` 通过，1 个目标测试通过。
- 修改后：`cargo test load_invocation_args_file_parse_error_uses_host_visible_path` 通过，1 个目标测试通过。
- 修改后：`cargo test --bin luaskills-debug` 通过，9 个二进制测试全部通过。
- 残留搜索：`src/bin/luaskills-debug.rs` 中不再存在 `display()` 调用。
- 全量验证：`cargo test` 通过，270 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变 CLI 参数解析规则、sync/inspect/list-tools/call 子命令语义、运行时目录布局、source 与 synced 目录判断、符号链接拒绝策略、目录复制行为、ignored skill id 收集规则或 args JSON 解析规则。
- 统一渲染只作用于错误文本，所有 `fs::create_dir_all`、`fs::read_dir`、`fs::canonicalize`、`fs::copy`、`fs::read_to_string`、`serde_yaml::from_str` 和 `serde_json::from_str` 仍使用真实 `Path`/`PathBuf` 与原始文件内容。
- 修改部分代码审核确认新增测试走真实 `load_bound_skill_manifest` 与 `load_invocation_args` 入口，不是单独测试格式化 helper；测试模块导入失败已按现有结构自动修正。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续检查 `src/runtime/engine.rs` 与 `src/runtime/engine/runlua.rs` 中剩余用户可见错误文本，并区分 Lua package 搜索语义路径与诊断展示路径。

## 2026-07-05 第 106 轮：修复 luaexec 文件读取错误路径与重复错误文本

### 问题探索

- 基线延续第 105 轮闭环状态：`cargo test` 通过，270 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮复查剩余 `display()` 时发现 `src/runtime/engine/runlua.rs` 仅剩一处 `file_path.display()`，位于 `LuaEngine::resolve_runlua_source` 的 file-backed luaexec 分支。
- 已追清 file-backed luaexec 流程：`execute_runlua_request_json_inline` 解析 JSON，`execute_runlua_request_inline` 捕获运行时快照，`execute_runlua_request_inline_with_runtime` 调用 `resolve_runlua_source` 解析 `code` 或 `file`，随后才进入 VM 执行。
- 已追清文件路径解析流程：`resolve_runlua_source` 先校验 `file` 文本，绝对路径原样使用，相对路径通过当前进程目录拼接成 `PathBuf`，再用 `std::fs::read_to_string` 读取 Lua 文件。
- 旧错误文本直接使用 `Path::display()`，并把同一个 IO error 拼接了两次，实际输出形态为 `Failed to read luaexec file <path>: <error>: <error>`。
- 长期优化判断：luaexec file 读取失败属于用户可见诊断面，应统一走宿主可见路径渲染，并去掉重复错误；路径校验、相对路径解析和真实文件读取仍必须使用原始 `PathBuf`。

### 执行调整

- `src/runtime/engine/runlua.rs` 中 `resolve_runlua_source` 的文件读取失败错误改为通过 `render_log_friendly_path(&file_path)` 输出路径。
- 同一错误文本去掉重复的底层 IO error，只保留一次 `error`，避免噪声诊断。
- `src/runtime/engine/tests.rs` 新增 `execute_runlua_request_inline_file_read_error_uses_host_visible_path`，通过真实 `execute_runlua_request_json_inline` 请求缺失 Lua 文件，覆盖 file-backed luaexec 读取错误分支。
- 新测试同时断言错误前缀包含 `render_host_visible_path(&missing_path)` 输出，并确认 `os error` 只出现一次，防止重复拼接回归。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test execute_runlua_request_inline_file_read_error_uses_host_visible_path` 通过，1 个目标测试通过。
- 修改后：`cargo test runlua` 通过，37 个 runlua 相关测试全部通过。
- 残留搜索：`src/runtime/engine/runlua.rs` 中不再存在 `display()` 调用。
- 全量验证：`cargo test` 通过，271 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变 luaexec 请求 JSON 结构、`code`/`file` 互斥规则、路径文本校验、相对路径解析、VM 池获取、cwd guard 语义、文件执行目录切换或 Lua wrapper 执行逻辑。
- 统一渲染只作用于错误文本，`std::fs::read_to_string` 仍使用真实 `file_path`，不会把展示路径反向用于文件系统访问。
- 修改部分代码审核确认新增测试走真实 `execute_runlua_request_json_inline` 入口，不是单独测试私有格式化函数；错误文本重复问题已通过断言覆盖。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续检查 `src/runtime/engine.rs` 中剩余用户可见错误文本，并继续保留 `lease.rs` 中 Lua package 搜索模式相关 `display()`。

## 2026-07-05 第 107 轮：统一 Windows 原生库搜索目录错误路径渲染

### 问题探索

- 基线延续第 106 轮闭环状态：`cargo test` 通过，271 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮从 `src/runtime/engine.rs` 剩余 `display()` 中定位到 Windows 原生库搜索分支：`NativeLibrarySearchGuard::new_windows` 和 `windows_wide_null_path`。
- 已追清构造流程：`LuaEngine::new` 与 `LuaEngine::reload` 通过 `NativeLibrarySearchGuard::new(&host_options)` 注册 `host_provided_ffi_root`，Windows 下进入 `new_windows`。
- 已追清目录注册流程：`new_windows` 仅在 `host_provided_ffi_root` 存在且是目录时启用 Windows DLL 搜索桶，随后调用 `windows_wide_null_path` 转成宽字符串，再调用 `AddDllDirectory` 注册目录。
- 已追清宽字符串转换流程：`windows_wide_null_path` 通过 `OsStrExt::encode_wide` 获取路径宽字符序列，若路径本身包含 NUL，则在追加终止符前返回错误。
- 旧实现直接使用 `Path::display()` 输出 `host_provided_ffi_root` 与嵌入 NUL 路径，绕过 engine 已有 `render_log_friendly_path` 规则。
- 长期优化判断：Windows DLL 搜索目录错误属于引擎初始化/重载的用户可见诊断面，应统一走宿主可见路径渲染；目录存在性检查、宽字符串转换和 `AddDllDirectory` 调用仍必须使用原始 `Path`。

### 执行调整

- `NativeLibrarySearchGuard::new_windows` 中 `AddDllDirectory` 注册失败错误改为通过 `render_log_friendly_path(host_provided_ffi_root)` 输出目录路径。
- `windows_wide_null_path` 中嵌入 NUL 错误改为通过 `render_log_friendly_path(path)` 输出路径。
- `src/runtime/engine/tests.rs` 新增 Windows 定向测试 `windows_wide_null_path_error_uses_host_visible_path`，通过真实宽路径转换 helper 构造嵌入 NUL 路径并验证错误文本。
- `AddDllDirectory` 失败依赖系统 API 状态，未构造脆弱测试；其错误路径渲染与已测 helper 使用同一 engine 级路径渲染出口。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test windows_wide_null_path_error_uses_host_visible_path` 通过，1 个 Windows 目标测试通过。
- 全量验证：`cargo test` 通过，272 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变 `host_provided_ffi_root` 选择规则、目录存在性判断、`SetDefaultDllDirectories` 调用、`AddDllDirectory` 生命周期、cookie drop 行为或 Windows 宽字符串终止符追加逻辑。
- 统一渲染只作用于错误文本，Windows API 调用仍使用原始宽字符串指针，文件系统判断仍使用原始 `Path`。
- 修改部分代码审核确认新增测试覆盖确定性的嵌入 NUL 转换错误分支；系统 API 注册失败分支只做等价渲染替换，不引入脆弱环境依赖。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续处理 `src/runtime/engine.rs` 中 managed runtime、helper 初始化、skill manifest 扫描等剩余用户可见路径错误文本。

## 2026-07-05 第 108 轮：统一 managed runtime helper 错误路径渲染

### 问题探索

- 基线延续第 107 轮闭环状态：`cargo test` 通过，272 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮继续检查 `src/runtime/engine.rs` 剩余 `display()`，定位到 managed runtime 前置 helper 中的 `skill_dir.display()` 与 `resolved.display()`。
- 已追清依赖上下文流程：`populate_vulcan_dependency_context` 在有当前 skill 时从 `skill_dir` 推导 `skills_root` 与 `runtime_root`，再把 tools/lua/ffi 依赖目录写入 `vulcan.deps`。
- 已追清 managed runtime root 推导流程：`runtime_root_from_skill_dir` 从 active skill directory 向上推导 runtime root，被 Python/Node status/invoke 等受管运行时入口复用。
- 已追清 managed runtime 源文件解析流程：`resolve_managed_runtime_skill_file` 校验 Lua 请求中的相对文件路径，拒绝空路径、绝对路径和 `..`，再用 `skill_dir.join(path)` 解析并要求文件存在。
- 旧实现直接把 `Path::display()` 放进错误文本，导致受管运行时状态/调用错误面与已统一的宿主可见路径规则不一致。
- 长期优化判断：这些错误会直接反馈给 Lua `vulcan.runtime.python/node.*` 调用方，应统一使用 engine 的 `render_log_friendly_path`；上下文推导、路径安全校验和文件存在性判断仍必须使用原始 `Path`。

### 执行调整

- `populate_vulcan_dependency_context` 中从 `skill_dir` 推导 `skills_root` 与 `runtime_root` 失败的错误路径改为通过 `render_log_friendly_path(skill_dir)` 输出。
- `runtime_root_from_skill_dir` 中 `skill_dir` 推导失败错误改为通过 `render_log_friendly_path(skill_dir)` 输出。
- `resolve_managed_runtime_skill_file` 中源文件不存在错误改为通过 `render_log_friendly_path(&resolved)` 输出。
- 新增 `managed_runtime_root_error_uses_host_visible_path`，通过真实 `runtime_root_from_skill_dir` helper 构造无法推导 runtime root 的 skill 目录并验证诊断路径。
- 新增 `managed_runtime_skill_file_error_uses_host_visible_path`，通过真实 `resolve_managed_runtime_skill_file` helper 构造缺失源文件并验证诊断路径。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test managed_runtime_root_error_uses_host_visible_path` 通过，1 个目标测试通过。
- 修改后：`cargo test managed_runtime_skill_file_error_uses_host_visible_path` 通过，1 个目标测试通过。
- 修改后：`cargo test managed_runtime` 通过，13 个 managed runtime 相关测试全部通过。
- 残留搜索：`src/runtime/engine.rs` 中目标 `skill_dir.display()` 与 `resolved.display()` 不再存在。
- 全量验证：`cargo test` 通过，274 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变 `vulcan.deps` 路径派生规则、runtime root 祖先层级假设、managed runtime 请求结构、相对路径安全校验、缺失文件判定、Python/Node env plan 解析或 worker 调用流程。
- 统一渲染只作用于错误文本，所有 `parent()`、`join()`、`is_file()` 和依赖路径写入仍使用原始 `Path`/`PathBuf`。
- 修改部分代码审核确认新增测试覆盖真实受管运行时 helper，而不是单独测试格式化函数；测试不需要启动 Python/Node 子进程，避免引入环境依赖。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续处理 `src/runtime/engine.rs` 中 managed node import、helper 初始化、manifest 扫描和 Lua helper 加载等剩余路径错误文本。

## 2026-07-05 第 109 轮：统一 managed Node import-root 错误路径渲染

### 问题探索

- 基线延续第 108 轮闭环状态：`cargo test` 通过，274 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮定位到 `src/runtime/engine.rs` 的 managed Node import staging 执行流：`prepare_managed_node_import_root` 与 `copy_managed_node_skill_import_root`。
- 已追清 Node invoke 流程：`invoke_node_runtime` 校验当前 skill 上下文、加载 `dependencies.yaml`、解析 Node env plan、确保环境存在、校验请求文件存在，再准备 `.luaskills-skill` import root。
- 已追清 import-root 准备流程：`prepare_managed_node_import_root` 使用 `plan.env_dir/.luaskills-skill`，若旧路径存在则读取 metadata；损坏 symlink 文件走 `remove_file`，普通路径走 `remove_dir_all`，最后递归复制 skill 目录。
- 已追清递归复制流程：`copy_managed_node_skill_import_root` 创建目标目录、读取源 skill 目录、跳过 `node_modules`、递归复制目录并复制普通文件。
- 旧实现直接用 `Path::display()` 输出 import root、source、destination、source file 和 destination file，导致 managed Node 错误面与统一宿主可见路径规则不一致。
- 长期优化判断：import-root 清理与复制错误会反馈给 `vulcan.runtime.node.invoke` 调用方，应统一走 `render_log_friendly_path`；清理策略、跳过 `node_modules` 规则和实际复制仍必须使用原始 `Path`。

### 执行调整

- `prepare_managed_node_import_root` 中 inspect import root、remove stale import root file、remove stale import root dir 的错误路径改为通过 `render_log_friendly_path(&import_root)` 输出。
- `copy_managed_node_skill_import_root` 中目标目录创建失败、源目录读取失败、目录项读取失败和文件复制失败的路径改为通过 `render_log_friendly_path` 输出。
- 新增 `make_test_managed_node_env_plan`，为 import-root 测试构造最小 `ManagedRuntimeEnvPlan`，避免启动真实 Node 环境。
- 新增 `managed_node_import_root_cleanup_error_uses_host_visible_path`，通过真实 `prepare_managed_node_import_root` 构造旧 import root 为普通文件的场景，覆盖清理错误路径。
- 新增 `managed_node_import_root_copy_error_uses_host_visible_path`，通过真实 `copy_managed_node_skill_import_root` 构造复制文件到同名目录的场景，覆盖 source/destination 复制错误路径。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test managed_node_import_root_cleanup_error_uses_host_visible_path` 通过，1 个目标测试通过。
- 修改后：`cargo test managed_node_import_root_copy_error_uses_host_visible_path` 通过，1 个目标测试通过。
- 修改后：`cargo test managed_runtime` 通过，13 个 managed runtime 相关测试全部通过。
- 残留搜索：目标 import-root/source/destination `display()` 调用不再存在。
- 全量验证：`cargo test` 通过，276 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变 Node env plan 解析、环境创建、import root 目录位置、旧目录清理策略、损坏 symlink 判断、`node_modules` 跳过规则、递归复制顺序或 `vulcan.runtime.node.invoke` payload 结构。
- 统一渲染只作用于错误文本，所有 `symlink_metadata`、`remove_file`、`remove_dir_all`、`create_dir_all`、`read_dir` 和 `fs::copy` 仍使用真实 `Path`/`PathBuf`。
- 修改部分代码审核确认新增测试覆盖真实 import-root helper，不是单独测试格式化函数；测试用最小 env plan 避免引入 Node 安装依赖。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续处理 `src/runtime/engine.rs` 中 help payload、manifest 扫描、Lua helper 加载和其他剩余用户可见路径错误文本。

## 2026-07-05 第 110 轮：统一 skill 文本与 Lua help 诊断路径渲染

### 问题探索

- 基线延续第 109 轮闭环状态：`cargo test` 通过，276 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮定位到 `src/runtime/engine.rs` 中 help/text 文件加载链路的剩余 `display()`：`read_skill_text_file`、`read_lua_help_payload_source` 和 `render_lua_help_payload_text`。
- 已追清普通文本读取流程：`read_skill_text_file` 用 `skill_dir.join(relative_path)` 定位 manifest 声明的帮助/文档文本文件，再用 `read_to_string` 读取 UTF-8 文本。
- 已追清 Lua help 源码读取流程：`read_lua_help_payload_source` 解析相对 help Lua 文件路径，读取源码并返回 `LuaHelpPayloadSource`，后续用于安装 help 文件上下文并执行。
- 已追清 Lua help payload 执行流程：`render_lua_help_payload_text` 将源码编译为函数、调用初始化结果；若返回函数则再次执行，最后要求结果是普通 UTF-8 字符串。
- 旧实现直接把 `helper_path.display()` 和 `file_path.display()` 放入读取、编译、初始化、运行、UTF-8 转换与返回类型错误文本，绕过 engine 统一路径渲染。
- 长期优化判断：help/text 诊断会反馈给 runtime help API 或加载流程调用方，应统一使用 `render_log_friendly_path`；文件定位、源码读取、Lua 编译执行与返回值校验仍保持原语义。

### 执行调整

- `read_skill_text_file` 的读取失败路径改为通过 `render_log_friendly_path(&file_path)` 输出。
- `read_lua_help_payload_source` 的读取失败路径改为通过 `render_log_friendly_path(&helper_path)` 输出。
- `render_lua_help_payload_text` 中 Lua 编译失败、初始化失败、运行失败、返回 UTF-8 转换失败和返回类型不合法错误路径改为通过 `render_log_friendly_path(helper_path)` 输出。
- 新增 `skill_text_file_read_error_uses_host_visible_path`，通过真实 `read_skill_text_file` 构造缺失文本文件并验证诊断路径。
- 新增 `lua_help_source_read_error_uses_host_visible_path`，通过真实 `read_lua_help_payload_source` 构造缺失 Lua help 文件并验证诊断路径。
- 新增 `lua_help_payload_runtime_error_uses_host_visible_path`，通过真实 `render_lua_help_payload_text` 执行会在返回函数中报错的 Lua help payload 并验证诊断路径。

### 验证记录

- 修改后：首次目标测试暴露 `read_lua_help_payload_source(...).expect_err(...)` 要求成功类型 `LuaHelpPayloadSource` 实现 `Debug`，而生产结构不应为了测试派生。
- 自动修复：将该测试改为显式 `match` 提取错误，保持生产类型定义不变。
- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test skill_text_file_read_error_uses_host_visible_path` 通过，1 个目标测试通过。
- 修改后：`cargo test lua_help_source_read_error_uses_host_visible_path` 通过，1 个目标测试通过。
- 修改后：`cargo test lua_help_payload_runtime_error_uses_host_visible_path` 通过，1 个目标测试通过。
- 修改后：`cargo test help` 通过，5 个 help 相关测试全部通过。
- 残留搜索：目标 `file_path.display()` 与 `helper_path.display()` 不再存在。
- 全量验证：`cargo test` 通过，279 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变 help 文件相对路径解析、普通文本读取、Lua 源码读取、chunk name、help 文件上下文、Lua 编译/初始化/运行顺序、UTF-8 要求或返回类型要求。
- 统一渲染只作用于错误文本，所有 `join()`、`read_to_string`、`lua.load`、`into_function`、`call` 和 `to_str` 仍使用原始路径、源码与 Lua 值。
- 修改部分代码审核确认新增测试覆盖真实文本/helper 入口；测试失败已按既有原则修复测试写法，没有为了测试修改生产类型。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续处理 `src/runtime/engine.rs` 中 skill manifest 扫描、Lua path helper 加载、provider/database 运行时上下文等剩余路径错误文本。

## 2026-07-05 第 111 轮：统一 root 平面保护错误路径渲染

### 问题探索

- 基线延续第 110 轮闭环状态：`cargo test` 通过，279 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮定位到 `src/runtime/engine.rs` root 平面保护链路中的两处路径展示：`runtime_skill_root_index` 与 `ensure_root_skill_id_is_not_system_occupied`。
- 已追清显式 root 校验流程：`runtime_skill_root_index` 用 root name 与 skills_dir 同时匹配当前完整 runtime root chain，若目标 root 不在链内，则在任何安装/更新/卸载探测前拒绝。
- 已追清 ROOT 占用保护流程：`ensure_root_skill_id_is_not_system_occupied` 只在 install/update 且目标是 PROJECT/USER 等普通可写 root 时检查 ROOT 是否已声明同名 skill id。
- 已追清真实入口：`system_update_skill_in_root` 与 `system_uninstall_skill_in_root` 都先校验 target root 是否在链内；`install_skill_in_root` 和 system install/update 都会在普通层写入前触发 ROOT 同名保护。
- 旧实现直接使用 `target_root.skills_dir.display()` 与 `root_instance.actual_dir.display()`，导致管理 API 错误文本与宿主可见路径渲染规则不一致。
- 长期优化判断：root 平面保护错误是管理 API 直接返回给宿主/工具的用户可见诊断，应统一走 `render_log_friendly_path`；root 匹配、权限判断、ROOT 遮蔽规则和生命周期操作顺序保持不变。

### 执行调整

- `runtime_skill_root_index` 的链外 target root 错误路径改为通过 `render_log_friendly_path(&target_root.skills_dir)` 输出。
- `ensure_root_skill_id_is_not_system_occupied` 的 ROOT 实例目录错误路径改为通过 `render_log_friendly_path(&root_instance.actual_dir)` 输出。
- 增强 `root_owned_skill_id_blocks_project_user_install_update_for_all_authorities`，断言 ROOT 占用错误包含 `render_host_visible_path(&root_skill_dir)`。
- 增强 `system_update_skill_in_root_rejects_unlisted_target_before_missing_target`，断言链外 target root 错误包含 `render_host_visible_path(&rogue_root.skills_dir)`。
- 增强 `system_uninstall_skill_in_root_rejects_unlisted_target_root`，断言链外 uninstall target root 错误包含 `render_host_visible_path(&rogue_root.skills_dir)`。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test root_owned_skill_id_blocks_project_user_install_update_for_all_authorities` 通过，1 个目标测试通过。
- 修改后：`cargo test system_update_skill_in_root_rejects_unlisted_target_before_missing_target` 通过，1 个目标测试通过。
- 修改后：`cargo test system_uninstall_skill_in_root_rejects_unlisted_target_root` 通过，1 个目标测试通过。
- 修改后：`cargo test root` 通过，47 个 root 相关测试全部通过。
- 残留搜索：目标 `target_root.skills_dir.display()` 与 `root_instance.actual_dir.display()` 不再存在。
- 全量验证：`cargo test` 通过，279 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变 runtime root chain 匹配规则、ROOT/PROJECT/USER 权限模型、ROOT 同名 skill id 遮蔽策略、显式 root install/update/uninstall 调用顺序或生命周期事件记录。
- 统一渲染只作用于错误文本，root 匹配仍比较原始 `RuntimeSkillRoot` 的 `name` 与 `skills_dir`，ROOT 实例解析仍使用原始 `PathBuf`。
- 修改部分代码审核确认测试均走真实公开管理 API，不是单独测试私有格式化函数；断言只增强路径可见性，不改变行为期望。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续处理 `src/runtime/engine.rs` 中 database 目录移除、skill manifest 扫描、Lua helper 加载等剩余路径错误文本。

## 2026-07-05 第 112 轮：统一技能数据库清理错误路径渲染

### 问题探索

- 基线延续第 111 轮闭环状态：`cargo test` 通过，279 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮定位到 `src/runtime/engine.rs` 的 `remove_skill_database_dir`，这是卸载流程中 SQLite/LanceDB 数据保留或删除语义共用的清理 helper。
- 已追清卸载流程：`uninstall_skill_and_reload_in_root` 完成技能生命周期卸载与依赖清理后，根据 `SkillUninstallOptions` 的 `remove_sqlite` 与 `remove_lancedb` 分别调用 `remove_skill_database_dir`。
- 已追清清理 helper 行为：未请求删除时返回 retained；目标目录不存在时返回未删除未保留；目标路径存在且请求删除时调用 `fs::remove_dir_all`。
- 旧实现直接用 `database_dir.display()` 输出删除失败路径，绕过统一宿主可见路径渲染规则。
- 长期优化判断：数据库清理失败会进入卸载结果 warning/message，属于用户可见诊断面，应统一走 `render_log_friendly_path`；删除请求语义、路径派生和实际删除操作保持不变。

### 执行调整

- `remove_skill_database_dir` 中删除失败错误路径改为通过 `render_log_friendly_path(&database_dir)` 输出。
- 新增 `skill_database_cleanup_error_uses_host_visible_path`，通过真实 `remove_skill_database_dir` helper 构造数据库目标为普通文件的场景，使 `remove_dir_all` 稳定失败并验证诊断路径。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test skill_database_cleanup_error_uses_host_visible_path` 通过，1 个目标测试通过。
- 修改后：`cargo test database` 通过，7 个 database 相关测试全部通过。
- 修改后：`cargo test uninstall` 通过，3 个 uninstall 相关测试全部通过。
- 残留搜索：目标 `database_dir.display()` 不再存在。
- 全量验证：`cargo test` 通过，280 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变 SQLite/LanceDB 数据目录布局、卸载完成顺序、数据保留默认语义、`remove_sqlite`/`remove_lancedb` 选项含义、warning 拼接或生命周期 reload 行为。
- 统一渲染只作用于错误文本，`fs::remove_dir_all` 仍使用真实 `database_dir`。
- 修改部分代码审核确认新增测试覆盖真实清理 helper，而不是单独测试格式化函数；测试通过普通文件冲突稳定触发删除错误。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续处理 `src/runtime/engine.rs` 中 skill manifest 扫描、Lua helper 加载和 Lua package 搜索字符串中真正属于诊断文本的剩余路径展示。

## 2026-07-05 第 113 轮：统一单个 skill 加载校验错误路径渲染

### 问题探索

- 基线延续第 112 轮闭环状态：`cargo test` 通过，280 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮定位到 `src/runtime/engine.rs` 的 `load_single_skill`，剩余路径展示集中在缺失 `skill.yaml`、禁止显式 `skill_id`、非法目录名、目录名标识符校验失败和缺失 Lua entry 文件。
- 已追清加载流程：`load_single_skill` 先检查 `skill.yaml` 存在，再读取 YAML、拒绝 manifest 显式声明 `skill_id`，随后从目录名绑定 skill id 并校验标识符。
- 已追清 entry 校验流程：manifest 基础字段合法后逐个校验 entry name、`lua_entry`、`lua_module`，并检查 `dir.join(tool.lua_entry)` 对应 Lua 文件是否存在。
- 初始测试尝试通过 `load_from_roots` 触发缺失 manifest、非法目录名和缺失 Lua entry，但目标测试暴露真实 root 扫描入口会跳过部分不可加载目录，并没有进入这些错误分支。
- 根据源码事实修正测试入口：这些诊断属于 `load_single_skill` 的生产校验面，因此改用该 helper 直接覆盖错误分支；显式 `skill_id` 仍保留既有 `load_from_roots` 真实入口测试。
- 长期优化判断：单个 skill 加载校验错误会出现在运行时加载失败诊断中，应统一使用 `render_log_friendly_path`；YAML 读取/解析、目录名绑定和 Lua entry 存在性检查保持原语义。

### 执行调整

- `load_single_skill` 中缺失 `skill.yaml` 错误路径改为通过 `render_log_friendly_path(dir)` 输出。
- 禁止显式 `skill_id` 错误路径改为通过 `render_log_friendly_path(dir)` 输出。
- 非 UTF-8/非法目录名错误路径改为通过 `render_log_friendly_path(dir)` 输出。
- 目录名标识符校验失败错误路径改为通过 `render_log_friendly_path(dir)` 输出。
- 缺失 Lua entry 文件错误路径改为通过 `render_log_friendly_path(dir)` 输出。
- 增强 `load_from_roots_rejects_explicit_skill_id_field`，断言显式 `skill_id` 错误包含 `render_host_visible_path(&skill_dir)`。
- 新增 `load_single_skill_missing_skill_yaml_error_uses_host_visible_path`，通过真实 `load_single_skill` helper 覆盖缺失 manifest 错误路径。
- 新增 `load_single_skill_invalid_skill_directory_error_uses_host_visible_path`，通过真实 `load_single_skill` helper 覆盖非法目录名错误路径。
- 新增 `load_single_skill_missing_lua_entry_error_uses_host_visible_path`，通过真实 `load_single_skill` helper 覆盖缺失 Lua entry 错误路径。

### 验证记录

- 修改后：首次 `load_from_roots_missing_skill_yaml_error_uses_host_visible_path` 与 `load_from_roots_invalid_skill_directory_error_uses_host_visible_path` 失败，原因是 root 扫描入口没有进入这些错误分支。
- 修改后：`load_from_roots_missing_lua_entry_error_uses_host_visible_path` 也失败，进一步确认该批错误分支应以 `load_single_skill` 为事实入口覆盖。
- 自动修复：将三个新增测试改名并改为调用 `engine.load_single_skill(&skill_dir, "ROOT")`，保留真实 helper 校验路径。
- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test load_from_roots_rejects_explicit_skill_id_field` 通过，1 个目标测试通过。
- 修改后：`cargo test load_single_skill_missing_skill_yaml_error_uses_host_visible_path` 通过，1 个目标测试通过。
- 修改后：`cargo test load_single_skill_invalid_skill_directory_error_uses_host_visible_path` 通过，1 个目标测试通过。
- 修改后：`cargo test load_single_skill_missing_lua_entry_error_uses_host_visible_path` 通过，1 个目标测试通过。
- 修改后：`cargo test load_from_roots` 通过，13 个 root 加载相关测试全部通过。
- 残留搜索：目标 `dir.display()` 不再存在。
- 全量验证：`cargo test` 通过，283 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变 root 扫描策略、`skill.yaml` 存在性判断、YAML 解析、显式 `skill_id` 禁止规则、目录名派生 skill id 规则、entry 字段校验或 Lua entry 文件存在性判断。
- 统一渲染只作用于错误文本，所有 `join()`、`exists()`、`read_to_string`、`serde_yaml` 解析和 `Path` 派生仍使用原始路径。
- 修改部分代码审核确认测试失败后已按源码事实修正入口，没有把错误假设写进测试或生产逻辑。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续处理 `src/runtime/engine.rs` 中 Lua helper 文件读取和 Lua package 搜索字符串中真正属于诊断文本的剩余路径展示。

## 2026-07-05 第 114 轮：统一 Lua 工具源码读取错误路径渲染

### 问题探索

- 基线延续第 113 轮闭环状态：`cargo test` 通过，283 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 本轮定位到 `src/runtime/engine.rs` 的 `compile_skill_into_lua`，剩余 `lua_path.display()` 位于读取工具 Lua 源码失败错误中。
- 已追清编译流程：`register_skill_functions` 遍历已加载 skill 的 entries，逐个调用 `compile_skill_into_lua`；该 helper 先用 `tool_entry_path(&skill.dir, tool)` 解析 Lua 源文件，再读取源码、编译 chunk、初始化 handler 并注册到 Lua globals。
- 已区分语义路径与诊断路径：`tool_entry_path` 结果作为真实文件读取路径必须保持 `PathBuf`；热重载日志已经使用 `render_log_friendly_path(&lua_path)`。
- 旧实现只在读取失败错误中直接使用 `lua_path.display()`，导致同一 helper 内日志与错误路径规则不一致。
- 长期优化判断：工具源码读取失败会直接导致 runtime 加载或热重载失败，是用户可见诊断面，应统一走 `render_log_friendly_path`；文件定位、源码读取、Lua 编译和全局注册保持不变。

### 执行调整

- `compile_skill_into_lua` 中 `std::fs::read_to_string(&lua_path)` 失败错误路径改为通过 `render_log_friendly_path(&lua_path)` 输出。
- 新增 `compile_skill_into_lua_read_error_uses_host_visible_path`，构造指向临时 skill 目录但缺失 `runtime/test.lua` 的 `LoadedSkill`，通过真实 `compile_skill_into_lua` helper 覆盖读取失败错误路径。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test compile_skill_into_lua_read_error_uses_host_visible_path` 通过，1 个目标测试通过。
- 修改后：`cargo test compile` 通过，1 个 compile 相关测试通过。
- 修改后：`cargo test load_from_roots` 通过，13 个加载相关测试全部通过。
- 残留搜索：目标 `lua_path.display()` 不再存在。
- 全量验证：`cargo test` 通过，284 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 告警。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变 Lua entry path 派生、源码读取时机、hot reload 日志、chunk name、Lua 编译、handler 初始化或全局函数注册逻辑。
- 统一渲染只作用于错误文本，`std::fs::read_to_string` 仍使用真实 `lua_path`。
- 修改部分代码审核确认新增测试覆盖真实编译 helper，不是单独测试格式化函数；测试不需要完整 root 扫描即可稳定触发读取错误。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续复查 `src/runtime/engine.rs` 中剩余 Lua package 搜索字符串，区分执行语义字符串与用户可见诊断文本。

## 2026-07-05 第 115 轮：统一 `vulcan.fs.list` 非 UTF-8 目录项诊断路径渲染

### 问题探索

- 基线延续第 114 轮闭环状态：`cargo test` 通过，284 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 本轮继续复查 `src/runtime/engine.rs` 剩余 `display()`：确认 `lua_packages.display()` 与 `root.display()` 位于 `package.path`/`package.cpath` 构造中，属于 Lua 模块搜索路径执行语义，不属于用户可见错误诊断，本轮不修改。
- 定位到唯一剩余诊断路径：`vulcan.fs.list` 注册闭包在 `entry.file_name().into_string()` 失败时使用 `Path::new(&dir).display()` 输出目录路径。
- 已追清执行流程：Lua 脚本调用 `vulcan.fs.list(args.path)` 后，参数先经过 `require_path_arg` 校验为路径字符串，再进入 `std::fs::read_dir(&dir)`，逐项读取目录项名称；只有目录项文件名无法转换为 UTF-8 时才拼接该目录路径到 runtime error。
- 旧实现会绕过 `render_host_visible_path`/`render_log_friendly_path`，在 Windows verbatim 路径场景下可能把 `\\?\` 前缀泄漏到 Lua 可见错误文本。
- 长期优化判断：该错误文本直接反馈给 Lua 调用方，属于用户可见诊断面，应统一走宿主可见路径渲染；实际目录读取、路径参数校验和目录项枚举行为必须保持原始 `Path`/字符串语义不变。

### 执行调整

- 在 `src/runtime/engine.rs` 新增 `format_vulcan_fs_list_non_utf8_file_name_error(dir, name)`，集中格式化 `vulcan.fs.list` 非 UTF-8 文件名错误。
- 将 `fs.list` 闭包中的 `Path::new(&dir).display()` 替换为 `render_log_friendly_path(dir_path)`，并复用已解析的 `dir_path`，避免错误文本路径渲染规则再次分叉。
- 在 `src/runtime/engine/tests.rs` 新增 Windows 测试 `vulcan_fs_list_non_utf8_file_name_error_uses_host_visible_path`，验证该诊断会去除 `\\?\` verbatim 前缀并保留目录与文件名信息。
- 在 `src/runtime/engine/tests.rs` 新增 Unix 端到端测试 `execute_runlua_request_inline_fs_list_non_utf8_entry_error_uses_host_visible_path`，通过原始字节文件名触发真实 `vulcan.fs.list` 错误分支。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test runtime::engine::tests::vulcan_fs_list_non_utf8_file_name_error_uses_host_visible_path` 通过，1 个目标测试通过。
- 修改后：`cargo test execute_runlua_request_inline_supports_vulcan_fs_rename_with_unicode_paths` 通过，1 个相邻 `vulcan.fs` 回归测试通过。
- 修改后：`cargo test normalize_host_visible_path_text_strips_windows_drive_verbatim_prefix` 通过，1 个路径规范化相关测试通过。
- 搜索复查：`Path::new(&dir).display()` 已不再存在；`fs.list` 非 UTF-8 诊断只通过新增 formatter 生成。
- 全量验证：`cargo test` 通过，285 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变 `require_path_arg` 校验、`std::fs::read_dir` 调用、目录项读取顺序、返回列表结构或 Lua runtime table 注册方式。
- 统一渲染只作用于非 UTF-8 目录项错误文本，文件系统操作仍使用调用方传入的真实路径字符串。
- 修改部分代码审核确认 Windows 测试覆盖 host-visible 路径渲染，Unix 测试覆盖真实文件系统错误分支；Windows 文件系统下未强行构造非 UTF-8 文件名，避免引入不稳定平台假设。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可转向继续搜索生产代码中的其他用户可见路径错误、投机性 fallback 或可维护性较差的运行时 helper。

## 2026-07-05 第 116 轮：消除池化 Lua VM 租约访问的 panic 路径

### 问题探索

- 基线延续第 115 轮闭环状态：`cargo test` 通过，285 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 本轮先复查 `src/runtime/engine/lease.rs` 的剩余 `root.display()`，确认全部位于持久 runtime session 的 `package.path`/`package.cpath` 前缀构造中，属于 Lua 搜索路径执行语义，不属于错误诊断，本轮不修改。
- 随后排查生产代码中的 `expect/unwrap`，定位到 `LuaVmLease::lua()` 使用 `self.vm.as_ref().expect("lua vm lease missing instance")` 直接访问池化 VM。
- 已追清生命周期：`LuaVmPool::acquire` 返回持有 `Option<LuaVm>` 的 `LuaVmLease`；正常 `Drop` 会 `take()` VM 并归还池；`discard()` 会提前 `take()` VM 并减少池计数，表示该 VM 已退役。
- 已追清调用流程：`LuaVmRequestScopeGuard::new/finish/drop`、`call_skill`、Lua help 渲染和 `runlua` inline 执行都会通过 scope guard 借用该 VM；如果内部状态已经被 `discard()` 清空，再调用 `lua()` 会触发 panic，而不是把状态异常作为 `Result` 返回。
- 长期优化判断：池化 VM 租约缺失实例是内部生命周期错误，但不能让公共调用路径 panic；应显式返回错误并让调用方传播或记录，同时保持正常回收、退役和清理语义不变。

### 执行调整

- 将 `LuaVmLease::lua()` 从 `&Lua` 改为 `Result<&Lua, String>`，当租约已经退役时返回 `"pooled Lua VM lease has already been retired"`。
- 将 `LuaVmRequestScopeGuard::lua()` 同步改为 `Result<&Lua, String>`，并让 `new`、`finish`、`Drop` 中的 reset 流程通过 `and_then` 显式处理缺失 VM 错误。
- 更新 `call_skill`、Lua help 渲染和 `runlua` inline 执行路径，对 `scope_guard.lua()?` 显式传播错误，避免内部状态异常被升级为 panic。
- 更新测试中直接借用 `lease.lua()`/`scope_guard.lua()` 的断言，使测试显式声明“租约应持有 VM”的不变量。
- 新增 `lua_vm_lease_lua_returns_error_after_discard`，直接覆盖 `discard()` 后访问 VM 的路径，验证返回错误而不是 panic。

### 验证记录

- 修改后：首次目标编译测试暴露 `runlua.rs` 两处真实执行入口仍把 `scope_guard.lua()` 当作裸 `&Lua` 使用，已改为 `?` 传播。
- 修改后：测试调用点也随返回类型变化补充了测试期 `expect` 或在 `Result` 闭包中使用 `?`。
- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test lua_vm_lease_lua_returns_error_after_discard` 通过，1 个目标测试通过。
- 修改后：`cargo test execute_runlua_request_inline_reuses_dedicated_pool` 通过，1 个 runlua pool 回归测试通过。
- 修改后：`cargo test lua_vm_pool` 通过，1 个 VM pool 相关测试通过。
- 修改后：`cargo test run_lua` 通过，4 个 `run_lua` 相关测试通过。
- 修改后：`cargo test execute_runlua_request_inline` 通过，34 个 inline runlua 相关测试通过。
- 搜索复查：`lua vm lease missing instance` 已不存在；生产代码中未留下未处理的 `let lua = scope_guard.lua();`。
- 全量验证：`cargo test` 通过，286 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变 VM 池容量计算、等待/唤醒、`Drop` 归还 VM、`discard()` 退役 VM、request scope reset 内容或 Lua 执行逻辑。
- 错误显式化只作用于“租约已不再持有 VM 后仍被访问”的异常路径；正常借用路径仍返回同一个底层 Lua VM 引用。
- 修改部分代码审核确认公共调用路径现在通过 `Result` 传播内部生命周期错误，`Drop` 路径会记录清理失败并继续退役 VM，不会 panic。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续排查生产代码中仍会把内部状态异常升级为 panic 的 `expect`，优先区分测试模块、恢复性 poison 测试与真实运行入口。

## 2026-07-05 第 117 轮：显式化入口 input_schema 未解析访问错误

### 问题探索

- 基线延续第 116 轮闭环状态：`cargo test` 通过，286 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 本轮继续排查生产代码中的 `expect/unwrap`，定位到 `src/skill/manifest.rs` 的 `SkillToolMeta::resolved_input_schema()` 使用 `expect("entry input schema must be resolved before use")` 直接读取入口 schema。
- 已追清解析流程：`SkillMeta::resolve_entry_input_schemas` 会遍历 manifest entries，调用 `SkillToolMeta::resolve_input_schema`，按 `input_schema_file`、内联 `input_schema` 或旧版 `parameters` 生成最终 schema，并写回 `self.input_schema`。
- 已追清加载入口：`load_single_skill`、`SkillManager` 加载和 debug CLI 加载都会在技能进入运行态前调用 `resolve_entry_input_schemas`；因此运行期 list_entries 正常情况下应只看到已解析 schema。
- 已追清生产调用点：唯一生产调用在 `LuaSkillRuntime::list_entries()`，用于构造 `RuntimeEntryDescriptor.input_schema`，后续会被 reload 对比、FFI JSON、标准 C FFI 和 debug CLI 读取。
- 长期优化判断：未解析 schema 是入口加载生命周期不变量被破坏，不能用默认空 schema 掩盖，也不应让公共枚举路径 panic；应由 accessor 返回显式错误，并在 list_entries 中记录内部异常后跳过损坏条目。

### 执行调整

- 将 `SkillToolMeta::resolved_input_schema()` 从直接 `expect` 改为返回 `Result<&JsonValue, String>`，未解析时返回 `"skill entry {name} input_schema has not been resolved"`。
- 更新 `LuaSkillRuntime::list_entries()`，先显式读取 resolved schema；若生命周期不变量破坏，则记录 `[LuaSkill:error] Failed to list runtime entry ...` 并跳过该条目，不构造投机性默认 schema。
- 更新外部 schema 解析测试，使其在已执行解析生命周期后通过 `expect("entry schema should resolve")` 显式声明测试不变量。
- 新增 `skill_tool_meta_unresolved_input_schema_returns_error`，覆盖未调用 `resolve_entry_input_schemas` 就访问 schema 的路径，验证返回错误而不是 panic。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test skill_tool_meta_unresolved_input_schema_returns_error` 通过，1 个目标测试通过。
- 修改后：`cargo test skill_meta_resolves_external_entry_input_schema_file` 通过，1 个外部 schema 解析回归测试通过。
- 修改后：`cargo test list_entries_exposes_resolved_entry_input_schema` 通过，1 个运行时 entry schema 暴露回归测试通过。
- 修改后：`cargo test manifest` 通过，17 个 manifest 相关测试全部通过。
- 修改后：`cargo test list_entries` 通过，3 个 list_entries 相关测试全部通过。
- 全量验证：`cargo test` 通过，287 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变 manifest schema 解析优先级、schema 根类型校验、旧版 parameters 反向派生、技能加载顺序、entry registry 结构或 FFI 输出字段含义。
- 错误显式化只作用于“生命周期不变量被破坏但仍访问 schema”的异常路径；正常加载后的 list_entries 仍返回已解析的完整 JSON Schema。
- 修改部分代码审核确认没有引入 `a || b`、多路字段猜测或默认 schema 兜底；异常路径会被日志显式暴露，避免静默伪造协议数据。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续排查生产代码中剩余 `expect/unwrap`，优先处理真实运行入口中会把内部状态异常升级为 panic 的路径。

## 2026-07-05 第 118 轮：显式化数据库 provider 动态 API 缺失错误

### 问题探索

- 基线延续第 117 轮闭环状态：`cargo test` 通过，287 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 本轮继续排查生产代码中的 `expect/unwrap/panic`，定位到 `src/providers/lancedb.rs` 与 `src/providers/sqlite.rs` 的 `api_ref()`，它们通过 `expect("... dynamic-library API missing ...")` 读取动态库 API。
- 已追清构造流程：`LanceDbSkillHost::new` 与 `SqliteSkillHost::new` 会根据 `LuaRuntimeDatabaseProviderMode` 解析动态库 API、host callback 或 space controller；只有 dynamic-library 模式应持有 `api`。
- 已追清注册流程：`register_skill` 根据 host 级资源为单个 skill 写入 `provider_mode`、`handles`、`controller` 与 `provider_binding`；正常动态库绑定应同时拥有 `api` 和原生 handles。
- 已追清执行流程：LanceDB 的 `vector_upsert`、`vector_search`、`call_json_string` 以及 SQLite 的 execute/query/stream/tokenize/FTS 等真实动态分发入口都会在 host callback 和 space controller 分支之后调用 `api_ref()`。
- 长期优化判断：动态库模式缺失 API 是内部绑定状态不一致，不能让真实运行入口 panic；应返回显式错误并沿现有 `Result<Value, String>` 调用链传播。

### 执行调整

- 将 `LanceDbSkillBinding::api_ref()` 改为 `Result<&LoadedLanceDbApi, String>`，缺失 API 时返回 `"LanceDB dynamic-library API is unavailable for {mode} binding"`。
- 将 `SqliteSkillBinding::api_ref()` 改为 `Result<&LoadedSqliteApi, String>`，缺失 API 时返回 `"SQLite dynamic-library API is unavailable for {mode} binding"`。
- 将 LanceDB 3 个动态 API 调用点更新为 `self.api_ref()?`，保持 host callback 与 space controller 分支行为不变。
- 将 SQLite 16 个动态 API 调用点更新为 `self.api_ref()?`，保持 SQL 参数解析、原生 handle 加锁、stream/FTS 逻辑和 host/controller 分支行为不变。
- 新增 `lancedb_dynamic_binding_without_api_returns_error` 与 `sqlite_dynamic_binding_without_api_returns_error`，手工构造动态库模式但 API 缺失的绑定，通过真实操作入口验证返回错误而不是 panic。

### 验证记录

- 修改后：首次新增测试编译暴露 SQLite 真实入口名为 `execute_script`，不是 `execute_script_json`；已按源码事实修正测试调用。
- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test lancedb_dynamic_binding_without_api_returns_error` 通过，1 个目标测试通过。
- 修改后：`cargo test sqlite_dynamic_binding_without_api_returns_error` 通过，1 个目标测试通过。
- 修改后：`cargo test lancedb` 通过，3 个 LanceDB 相关测试全部通过。
- 修改后：`cargo test sqlite` 通过，4 个 SQLite 相关测试全部通过。
- 修改后：`cargo test provider` 通过，14 个 provider 相关测试全部通过。
- 搜索复查：`dynamic-library API missing` 与裸 `self.api_ref();` 已不存在；动态路径均使用 `self.api_ref()?`。
- 全量验证：`cargo test` 通过，289 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变 provider mode 选择、动态库加载、host callback 注册要求、space controller 桥接、数据库路径派生、原生 handle 生命周期或实际数据库操作语义。
- 错误显式化只作用于“绑定声明走 dynamic-library 路径但 API 缺失”的内部异常状态；正常动态库绑定仍按原路径执行。
- 修改部分代码审核确认没有引入默认 provider、备用 API、host/controller 分支兜底或字段猜测；缺失 API 会作为明确错误返回调用方。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续排查 FFI 标准接口和生命周期 apply 逻辑中仍可能把内部异常升级为 panic 或不可达断言的路径。

## 2026-07-05 第 119 轮：收窄 install/update apply 生命周期动作集合

### 问题探索

- 基线延续第 118 轮闭环状态：`cargo test` 通过，289 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 本轮继续排查生产代码中的 panic/不可达断言，定位到 `src/runtime/engine.rs` 的 `apply_skill_request_in_root` 内存在两处 `unreachable!("unsupported apply action should have returned early")`。
- 已追清动作来源：公开生命周期动作定义在 `SkillLifecycleAction`，包含 `Install`、`Update`、`Reload`、`Uninstall`、`Enable`、`Disable`。
- 已追清 apply 执行入口：`install_skill`、`update_skill`、`system_install_skill*`、`system_update_skill*` 只会向 `apply_skill_request_in_root` 传入 `Install` 或 `Update`；`Reload`、`Enable`、`Disable` 由 `mutate_skill_state_and_reload` 处理；`Uninstall` 走独立卸载流程。
- 旧实现先用 `matches!(action, Install | Update)` 做前置拦截，再在后续两个 `match action` 中通过 `_ => unreachable!` 表示理论不可达分支；这种写法把“动作集合已收窄”的事实分散在远离实际使用点的位置。
- 长期优化判断：apply 流程确实只支持 install/update，应把这一约束建模成内部窄类型，让后续分支在类型层面只面对合法动作，而不是依赖运行时前置判断和不可达断言维持不变式。

### 执行调整

- 在 `src/runtime/engine.rs` 新增内部枚举 `SkillApplyLifecycleAction`，只包含 `Install` 与 `Update` 两个 apply 流程合法动作，并补充中英文说明。
- 新增 `SkillApplyLifecycleAction::from_lifecycle_action`，将公开 `SkillLifecycleAction` 显式转换为内部窄动作；遇到 `Reload`、`Uninstall`、`Enable`、`Disable` 时返回 `"unsupported apply action {:?}"`。
- 将 `apply_skill_request_in_root` 的前置 `matches!` 检查改为一次窄类型转换，后续 target root 推导、依赖 manifest 读取、安装请求准备和更新请求准备全部使用 `apply_action`。
- 删除两处 `_ => unreachable!("unsupported apply action should have returned early")`，使 install/update 分支不再依赖不可达断言表达控制流。
- 在 `src/runtime/engine/tests.rs` 新增 `skill_apply_lifecycle_action_only_accepts_install_and_update`，覆盖 install/update 成功转换以及 reload/uninstall/enable/disable 明确拒绝。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test skill_apply_lifecycle_action_only_accepts_install_and_update` 通过，1 个目标测试通过。
- 修改后：`cargo test apply` 通过，1 个 apply 相关测试通过。
- 修改后：`cargo test install` 通过，12 个 install 相关测试全部通过。
- 修改后：`cargo test update` 通过，14 个 update 相关测试全部通过。
- 搜索复查：`unsupported apply action should have returned early` 与 `unreachable!(` 在 `src/runtime/engine.rs` 中已不存在。
- 全量验证：`cargo test` 通过，290 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变 install/update 的公开 API、目标 root 选择、依赖 manifest 读取、插件安装准备、插件更新准备、事件记录或 reload 行为。
- 窄类型只作用于 `apply_skill_request_in_root` 内部控制流，公开生命周期枚举仍保留原有动作集合，其他生命周期路径仍由既有入口处理。
- 修改部分代码审核确认没有引入多来源字段猜测、候选式动作兜底或默认动作；非 install/update 动作会在进入 apply 流程时明确返回不支持错误。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续排查 FFI 标准接口、序列化路径和剩余生产代码中的 `expect`/`unwrap`，优先处理真实运行入口中会把内部异常升级为 panic 的路径。

## 2026-07-05 第 120 轮：显式化 FFI entry schema 序列化错误

### 问题探索

- 基线延续第 119 轮闭环状态：`cargo test` 通过，290 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 本轮继续排查生产代码中的 `expect/unwrap/panic/unreachable`，定位到 `src/ffi_standard.rs` 的 `alloc_entry_descriptor` 使用 `expect("runtime entry input schema should serialize")` 序列化 `RuntimeEntryDescriptor.input_schema`。
- 已追清数据结构来源：`RuntimeEntryDescriptor.input_schema` 的类型是 `serde_json::Value`，由运行时入口描述传入标准 FFI 层。
- 已追清执行流：`luaskills_ffi_list_entries` 先通过 `with_engine` 调用 `engine.list_entries_for_authority(authority)` 获取 entry 列表，再逐个调用 `alloc_entry_descriptor` 转为 `FfiRuntimeEntryDescriptor`，最后返回 `FfiRuntimeEntryDescriptorList` 给 C ABI 调用方。
- 已追清释放流：成功返回后的列表由 `luaskills_ffi_entry_list_free` 释放，单个 entry 会通过 `free_entry_descriptor` 释放 canonical name、skill id、root name、skill dir、description、input schema JSON 和参数数组。
- 旧实现的问题是 FFI 外部边界内的 schema 序列化失败会被 `expect` 升级为 panic；同时如果未来 descriptor 构造中出现可失败步骤，列表级构造也需要明确处理已经分配的嵌套缓冲所有权。
- 长期优化判断：即便 `serde_json::Value` 当前正常情况下几乎不会序列化失败，FFI 边界仍不应该依赖 panic 表达异常；应通过现有 `ffi_error_status` 返回明确错误，并保证中途失败时不会泄漏已构造的 FFI 资源。

### 执行调整

- 新增 `serialize_entry_input_schema_json`，将 `serde_json::to_string(&value.input_schema)` 的错误转换为 `"runtime entry input schema failed to serialize: ..."`，不再使用 `expect`。
- 将 `alloc_entry_descriptor` 改为返回 `Result<FfiRuntimeEntryDescriptor, String>`，并在任何嵌套 FFI 缓冲分配前先完成 schema 序列化。
- 新增 `alloc_entry_descriptor_list`，集中构造 `FfiRuntimeEntryDescriptorList`；如果单个 descriptor 构造失败，会调用 `free_entry_descriptor` 释放此前已构造的 descriptor，再返回错误。
- 将 `luaskills_ffi_list_entries` 改为使用 `alloc_entry_descriptor_list`，成功时写入 `entries_out`，失败时通过 `ffi_error_status` 返回错误。
- 更新 `entry_list_free_handles_nested_owned_buffers`，对新的 `alloc_entry_descriptor` 返回值显式断言成功，保持嵌套释放测试覆盖真实 descriptor 构造路径。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test entry_list_free_handles_nested_owned_buffers` 通过，1 个目标测试通过。
- 修改后：首次运行 `cargo test luaskills_ffi_list_entries` 未匹配到测试名称，确认没有真实测试执行，因此改查测试名。
- 修改后：`cargo test standard_ffi_load_and_list_entries_round_trip` 通过，1 个标准 FFI entry 列表往返测试通过。
- 修改后：`cargo test ffi_standard` 通过，13 个 FFI 标准接口相关测试全部通过。
- 搜索复查：`runtime entry input schema should serialize` 在 `src/ffi_standard.rs` 中已不存在；生产路径不再通过该 `expect` 处理 schema 序列化。
- 全量验证：`cargo test` 通过，290 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变 `list_entries_for_authority` 的筛选语义、entry 描述字段含义、C ABI 结构布局、成功路径的内存释放契约或调用方释放函数。
- 错误显式化只作用于 entry input schema JSON 序列化与 descriptor 列表构造；正常 entry 列表仍返回相同字段与相同 JSON schema 文本。
- 修改部分代码审核确认没有引入默认 schema、空 schema 兜底、多字段猜测或兼容式候选路径；序列化异常会通过 FFI 错误状态明确暴露。
- 已特别复查中途失败所有权：schema 序列化发生在单个 descriptor 分配前，列表级失败会释放已经构造完成的 descriptor，避免裸 `Vec<FfiRuntimeEntryDescriptor>` drop 造成嵌套缓冲泄漏。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续排查 FFI 字符串分配、配置时间戳和运行时配置路径中剩余的真实生产 `expect`/`unwrap`。

## 2026-07-05 第 121 轮：收窄 FFI 字符串克隆的输入模型

### 问题探索

- 基线延续第 120 轮闭环状态：`cargo test` 通过，290 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 本轮继续排查生产 `expect/unwrap`，先确认 `src/runtime/config.rs` 的 `expect("system time before unix epoch")` 位于 `#[cfg(test)]` 测试模块，不属于生产路径，因此本轮不修改。
- 随后定位到 `src/ffi_standard.rs` 的 `alloc_c_string`：旧实现接受任意 Rust 字符串，调用 `CString::new(value.as_ref())`，遇到内部 NUL 时再通过 `CString::new("FFI string contains NUL byte").expect("static text")` 生成兜底字符串。
- 已追清调用面：`alloc_c_string` 只被 `luaskills_ffi_string_clone` 调用；该公开函数的输入要么是 null，要么先经 `CStr::from_ptr(value)` 解析为 NUL 结尾 C 字符串。
- 已追清行为语义：`luaskills_ffi_string_clone` 会拒绝非法 UTF-8 并返回 null；合法输入则应返回 LuaSkills 拥有的 C 字符串克隆，调用方通过 `luaskills_ffi_string_free` 释放。
- 旧实现的问题不是需要兼容内部 NUL，而是 helper 的输入类型过宽，导致用“任意 Rust 字符串转 C 字符串 + fallback expect”掩盖了真实执行流：这里实际只需要克隆已解析的 `CStr`。
- 长期优化判断：把 helper 收窄为 `&CStr` 可以从类型上消除内部 NUL 分支和静态文本 `expect`，同时保持 FFI clone 成功、非法 UTF-8 拒绝和释放契约不变。

### 执行调整

- 将 `alloc_c_string` 从 `impl AsRef<str>` 改为 `&CStr`，实现改为 `value.to_owned().into_raw()`，不再调用 `CString::new` 或任何兜底 `expect`。
- 将 `luaskills_ffi_string_clone` 的 null 分支改为克隆 `c""`，仍返回调用方需要释放的空 C 字符串。
- 将非 null 分支改为只解析一次 `CStr::from_ptr(value)`，通过 `source.to_str()` 验证 UTF-8，成功时克隆同一个 `CStr`，失败时返回 null。
- 新增 `ffi_string_clone_null_input_returns_owned_empty_string`，覆盖 null 输入返回拥有所有权的空 C 字符串，并验证可按原释放函数释放。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test ffi_string_clone` 通过，3 个字符串克隆相关测试全部通过。
- 修改后：`cargo test ffi_standard` 通过，14 个 FFI 标准接口相关测试全部通过。
- 搜索复查：`FFI string contains NUL byte` 与 `expect("static text")` 在 `src/ffi_standard.rs` 中已不存在；`alloc_c_string` 只剩 `&CStr` 克隆调用。
- 全量验证：`cargo test` 通过，291 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变 `luaskills_ffi_string_clone` 的公开 ABI、非法 UTF-8 返回 null 的语义、null 输入返回空字符串的语义或 `luaskills_ffi_string_free` 的释放契约。
- 类型收窄只作用于内部 helper，真实执行流从“先解析 C 字符串、再验证 UTF-8、最后克隆 C 字符串”表达得更直接。
- 修改部分代码审核确认没有引入替换字符、有损转换、内部 NUL 文本兜底或多路径猜测；非法 UTF-8 仍显式返回 null。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续排查配置存储、下载归档、debug CLI 和其他生产入口中剩余的 `expect`/`unwrap`，优先排除测试模块后再处理真实运行路径。

## 2026-07-05 第 122 轮：结构化 JSON FFI 序列化兜底

### 问题探索

- 基线延续第 121 轮闭环状态：`cargo test` 通过，291 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 本轮继续排查剩余 `expect/unwrap` 命中，确认 `src/download/archive.rs`、`src/download/manager.rs`、`src/providers/mod.rs`、`src/runtime/managed_runtime.rs` 的相关命中均位于测试模块，不属于生产路径。
- 随后扩大到生产代码中的错误吞噬/默认兜底模式，定位到 `src/ffi.rs` 的 `encode_json_buffer`。
- 已追清执行流：JSON FFI 的 `ffi_ok` 与 `ffi_error` 都调用 `encode_json_buffer`；所有 `luaskills_ffi_*_json` 出口最终都通过该函数把统一响应包络写入 `FfiOwnedBuffer`。
- 旧实现对原始响应执行 `serde_json::to_string(value)`；失败后通过 `format!("{{\"ok\":false,\"error\":\"...{}\"}}", error)` 手拼 JSON 字符串。
- 已确认风险：序列化错误文本没有通过 JSON encoder 转义，若错误消息中包含引号或其他特殊字符，兜底响应可能变成非法 JSON，进而破坏 FFI 调用方对统一响应包络的解析假设。
- 长期优化判断：FFI JSON 兜底路径仍然必须返回合法 JSON，不能用手拼字符串表达结构化响应；应使用 `serde_json::json!` 构造错误包络，让转义规则由 JSON encoder 统一负责。

### 执行调整

- 将 `encode_json_buffer` 的兜底逻辑改为匹配 `serde_json::to_string(value)` 的结果，失败时调用专用 helper 生成错误 JSON 文本。
- 新增 `encode_json_serialization_error_text`，通过 `json!({ "ok": false, "error": ... }).to_string()` 生成合法 JSON 错误包络。
- 在 `src/ffi/tests.rs` 新增 `FailingJsonSerialize`，通过自定义 `Serialize` 实现稳定触发序列化失败，并返回带引号的错误文本。
- 新增 `encode_json_buffer_serialization_failure_returns_escaped_error_envelope`，验证兜底响应可被 JSON 解析，`ok` 为 false，错误文本完整保留引号，且不包含 `result` 字段。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test encode_json_buffer_serialization_failure_returns_escaped_error_envelope` 通过，1 个目标测试通过。
- 修改后：`cargo test ffi::` 通过，16 个 JSON FFI 相关测试全部通过。
- 搜索复查：旧的 `unwrap_or_else(|error| ...)` 手拼兜底不再存在；生产代码中只保留 `json!` 结构化错误包络生成。
- 全量验证：`cargo test` 通过，292 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变 JSON FFI 成功响应结构、失败响应字段名、`FfiOwnedBuffer` 所有权模型、各个 `luaskills_ffi_*_json` 出口的公开 ABI 或调用方释放方式。
- 兜底改动只作用于“统一响应包络序列化失败”的异常路径；正常 `ffi_ok` 与 `ffi_error` 仍按原包络结构序列化。
- 修改部分代码审核确认没有引入字符串拼接 JSON、默认空对象、吞错返回成功或多层候选字段猜测；序列化失败会稳定暴露为 `ok:false` 错误包络。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续排查生产路径中的 `.ok()` 静默丢错、`unwrap_or_default` 默认值掩盖真实状态，以及 FFI/provider 边界的错误文本构造。

## 2026-07-05 第 123 轮：显式记录本地依赖版本扫描异常

### 问题探索

- 基线延续第 122 轮闭环状态：`cargo test` 通过，292 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 本轮继续排查生产路径中的 `.ok()` 静默丢错，定位到 `src/dependency/manager.rs` 的 `local_unversioned_dependency_probe_requests`。
- 已追清执行流：`ensure_skill_dependencies` 逐类进入 `ensure_dependency`；`ensure_dependency` 先调用 `find_existing_local_dependency_request` 做本地探测；当依赖 manifest 省略版本时，`local_dependency_probe_requests` 会额外调用 `local_unversioned_dependency_probe_requests` 扫描已有版本目录。
- 已追清语义边界：该扫描用于“网络解析前尽量复用本地已安装版本”，扫描失败不应直接让必需依赖失败，因为后续仍可能通过远程解析或明确缺失路径处理。
- 旧实现中 `fs::read_dir(&dependency_root)` 失败直接返回空列表，`version_entries.filter_map(Result::ok)` 会吞掉目录项读取错误，`entry.file_type().ok()?` 会吞掉文件类型读取错误；这些异常会让本地复用失败但没有任何诊断。
- 已确认现有日志通道：依赖管理器已使用 `log_warn` 记录 optional dependency 缺失和缓存安装失败，适合承载本地扫描异常。
- 长期优化判断：保持“扫描异常后继续后续解析”的行为不变，但必须把非 NotFound 的环境异常显式记录为 dependency warning，避免用户只看到后续缺失/下载失败而不知道本地版本扫描已损坏。

### 执行调整

- 将 `local_unversioned_dependency_probe_requests` 中的 `read_dir` 处理改为显式 `match`：`NotFound` 仍安静返回空列表；其他错误通过 `log_warn` 记录后返回空列表。
- 将目录项遍历从 `filter_map(Result::ok)` 改为显式循环；目录项读取失败时记录 warning 并继续扫描后续条目。
- 将 `entry.file_type().ok()?` 改为显式 `match`；文件类型读取失败时记录 warning 并继续扫描后续条目。
- 保留非目录条目、非 UTF-8/非法版本目录名、平台子目录缺失时的跳过语义，因为这些属于正常候选过滤，而不是 I/O 异常。
- 在 `src/dependency/manager/tests.rs` 新增 runtime log callback 测试守卫和自动清理守卫，避免测试污染全局日志回调。
- 新增 `local_unversioned_dependency_probe_requests_warns_when_version_root_is_not_directory`，用普通文件占用版本根目录，稳定触发非 NotFound `read_dir` 错误，并验证 warning 内容包含宿主可见路径。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test local_unversioned_dependency_probe_requests_warns_when_version_root_is_not_directory` 通过，1 个目标测试通过。
- 修改后：`cargo test dependency::manager` 通过，12 个依赖管理器相关测试全部通过。
- 搜索复查：目标 `file_type().ok()` 与 `filter_map(Result::ok)` 在 `src/dependency/manager.rs` 中已不存在。
- 全量验证：`cargo test` 通过，293 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变依赖解析优先级、网络下载开关语义、host/skill scope 安装根规则、版本目录命名规则、导出文件检测规则或最终下载安装流程。
- 错误显式化只作用于本地无版本依赖扫描的 I/O 异常诊断；本地目录不存在仍按“未安装本地版本”处理，不额外产生日志噪音。
- 修改部分代码审核确认没有引入默认版本、备用依赖源、候选字段猜测或扫描失败即成功；异常只会记录为 warning 并允许既有后续解析继续执行。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续排查其他 `.ok()` 静默丢错点，特别是运行时缓存时间戳、Lua bridge 请求上下文解析和 provider 边界的默认值掩盖。

## 2026-07-05 第 124 轮：显式校验 Lua request context 解析

### 问题探索

- 基线延续第 123 轮闭环状态：`cargo test` 通过，293 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 本轮继续排查 `.ok()` 静默丢错，先检查 `metadata_modified_unix_ms`，确认该函数服务 `vulcan.fs.stat` 的可选 `modified_unix_ms` 字段；时间戳不可得时省略字段属于明确可选语义，因此不修改。
- 随后定位到两处 `serde_json::from_value::<RuntimeRequestContext>(...).ok()`：一处在 `src/runtime/engine/bridge.rs` 的模型回调 caller context 捕获路径，另一处在 `src/runtime/engine.rs` 的嵌套 `vulcan.call` 继承 invocation context 路径。
- 已追清执行流：模型 `vulcan.models.embed/llm` 会读取 `vulcan.context.request`，转换为 JSON 后尝试解析 `RuntimeRequestContext`，再从中提取 `client_name` 与 `request_id` 传给宿主模型 callback。
- 已追清嵌套调用流：`vulcan.call` 进入目标 skill 前，会从外层 `vulcan.context.request` 快照恢复 request context，并和 client budget/tool config 一起构造新的 `LuaInvocationContext`。
- 旧实现的问题是：非空但格式错误的 request context 会被 `.ok()` 静默降级为 `None`，导致 caller context 丢失，模型回调仍可能被调用，排查审计/计费上下文问题时没有明确诊断。
- 中途验证暴露额外事实：空 Lua 表通过现有 `lua_value_to_json` 会表示为 `[]` 而不是 `{}`；因此无上下文哨兵必须同时接受空对象和空数组，不能简单把所有非对象都当成错误。
- 长期优化判断：保留“空对象/空数组表示无 request context”的既有约定；对于非空、字段类型不符合 `RuntimeRequestContext` 的值，必须返回显式错误，而不是静默丢弃上下文。

### 执行调整

- 新增共享 helper `parse_runtime_request_context_json`，集中处理 `vulcan.context.request` 的解析规则。
- helper 对空对象 `{}` 与空数组 `[]` 返回 `None`，对其他值执行 `serde_json::from_value::<RuntimeRequestContext>`；解析失败时返回包含来源名称的显式错误。
- 将模型 bridge 的 `current_runtime_model_caller` 改为使用该 helper，malformed request context 会转成模型 `internal_error`，并阻止 host model callback 被调用。
- 将嵌套调用的 `previous_invocation_context` 改为使用同一 helper，malformed 外层 request context 会阻止继承出错误的 invocation context。
- 新增 `parse_runtime_request_context_json_rejects_malformed_non_empty_context`，覆盖空对象、空数组、合法 request context 与格式错误 request context。
- 新增 `vulcan_models_embed_rejects_malformed_request_context`，通过 Lua 主动写入 `{ request_id = 42 }` 触发格式错误，验证返回 `internal_error` 且宿主 embedding callback 未被调用。

### 验证记录

- 修改后：首次 `cargo test vulcan_models` 失败，原因是空 Lua 表在 JSON 转换中表示为 `[]`；已根据源码事实将空数组纳入无上下文哨兵。
- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test parse_runtime_request_context_json_rejects_malformed_non_empty_context` 通过，1 个目标测试通过。
- 修改后：`cargo test vulcan_models_embed_rejects_malformed_request_context` 通过，1 个目标测试通过。
- 修改后：`cargo test vulcan_models` 通过，6 个模型桥接相关测试全部通过。
- 搜索复查：`from_value::<RuntimeRequestContext>(...).ok()` 静默解析模式在 `src/runtime/engine.rs` 与 `src/runtime/engine/bridge.rs` 中已不存在。
- 全量验证：`cargo test` 通过，295 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变 `RuntimeRequestContext` 字段定义、`vulcan.context.request` 的注入格式、模型成功响应结构、模型 callback 注册机制或嵌套调用的 budget/tool config 继承规则。
- 空上下文兼容只保留已有事实：host 未提供 request context 时是空对象，Lua 空表转换后可能是空数组；其他 malformed 非空值不再被吞掉。
- 修改部分代码审核确认没有引入多字段候选猜测、默认 request id、默认 client name 或 malformed 上下文兜底；错误会以模型 `internal_error` 或嵌套调用 runtime error 明确暴露。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续排查 Lua table 到 JSON 转换中的 `unwrap_or_default`、provider 边界默认值，以及其他会静默丢失诊断信息的 `.ok()`。

## 2026-07-05 第 125 轮：拒绝 Lua 字符串非法 UTF-8 的静默空值转换

### 问题探索

- 基线延续第 124 轮闭环状态：`cargo test` 通过，295 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 本轮继续排查 Lua table 到 JSON 转换中的 `unwrap_or_default`，定位到 `src/runtime/engine.rs` 的 `lua_value_to_json`。
- 已追清执行流：`lua_value_to_json` 是 host bridge 参数转换、模型 caller context 转换、host_result 解析、嵌套 invocation context 继承等多个运行时边界共用的 Lua 到 JSON 转换函数。
- 旧实现中 `LuaValue::String(s)` 使用 `s.to_str().map(...).unwrap_or_default()`；当 Lua 字符串不是有效 UTF-8 时，会静默变成空 JSON 字符串。
- 继续复查后发现第二处同类问题：运行时重写的全局 `print` 在渲染 `LuaValue::String` 时也使用同样的 `unwrap_or_default()`，导致非法 UTF-8 日志参数显示为空片段。
- 长期优化判断：JSON 边界不能把非法 UTF-8 伪造成空字符串，因为这会篡改调用参数和上下文；日志边界也不应静默空值，而应输出明确诊断文本。

### 执行调整

- 为 `lua_value_to_json` 补充中英文说明，并将 `LuaValue::String` 分支改为 `to_str()` 失败时返回 `"Cannot convert Lua string to JSON: invalid UTF-8: ..."`。
- 新增 `render_lua_print_argument`，集中渲染运行时 `print` 参数；普通字符串保持原样，非法 UTF-8 字符串渲染为 `<invalid UTF-8 Lua string: ...>`。
- 将全局 `print` 闭包改为使用 `render_lua_print_argument`，移除内联的有损空值转换。
- 新增 `lua_value_to_json_rejects_invalid_utf8_string`，直接构造非法 UTF-8 Lua 字符串并验证 JSON 转换返回显式错误。
- 新增 `render_lua_print_argument_marks_invalid_utf8_string`，验证非法 UTF-8 print 参数会生成非空诊断文本。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test lua_value_to_json_rejects_invalid_utf8_string` 通过，1 个目标测试通过。
- 修改后：`cargo test render_lua_print_argument_marks_invalid_utf8_string` 通过，1 个目标测试通过。
- 修改后：`cargo test vulcan_models` 通过，6 个模型桥接相关测试全部通过。
- 修改后：`cargo test host_bridge` 通过，2 个 host bridge 相关测试全部通过。
- 搜索复查：目标 `to_str().unwrap_or_default` 模式在 `src/runtime/engine.rs` 中已不存在；相关位置只保留显式非法 UTF-8 错误或诊断文本。
- 全量验证：`cargo test` 通过，297 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变 Lua 数值、布尔、nil、table、function/thread/userdata 的 JSON 转换规则，也没有改变 `print` 对数字、布尔、nil 或其他 Lua 值的渲染语义。
- 错误显式化只作用于非法 UTF-8 Lua 字符串；合法 UTF-8 字符串仍按原文本进入 JSON 或日志。
- 修改部分代码审核确认没有引入空字符串兜底、替换字符、有损转换或多路径猜测；无法表示为 UTF-8 的内容会在 JSON 边界报错，在日志边界显式标记。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续排查 provider 边界默认值、Lua table 空数组/空对象歧义和其他 `unwrap_or_default` 掩盖真实输入的问题。

## 2026-07-05 第 126 轮：标记 provider last-error 非法 UTF-8 诊断

### 问题探索

- 基线延续第 125 轮闭环状态：`cargo test` 通过，297 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 本轮先检查 SQLite provider status 中的 library info 默认值，确认其属于状态展示降级，不直接改变数据库操作语义，因此暂不作为本轮目标。
- 随后定位到 SQLite 与 LanceDB 动态库边界的 `take_last_error_message`：两者都会读取动态库 `last_error_message` 指针，并执行 `decode_non_null_ffi_c_string(ptr).unwrap_or_else(|error| error)`。
- 已追清执行流：当动态库调用返回空指针或失败时，`take_last_error_message` 会把动态库 last-error 文本转成 Rust `String`，再沿调用链返回给 SQLite/LanceDB 操作方。
- 旧实现的问题是：如果动态库返回的 last-error C 字符串不是有效 UTF-8，解码错误文本会被直接当成 provider 原始错误内容返回，无法区分“provider 报错”和“provider 错误消息本身不可解码”。
- 长期优化判断：动态库错误消息解码失败本身就是边界诊断，应明确标注 provider 名称和 UTF-8 解码失败，而不是把通用解码错误伪装成 provider 错误正文。

### 执行调整

- 在 `src/providers/mod.rs` 新增 `decode_provider_last_error_message`，集中处理 provider last-error 指针解码。
- helper 对空指针返回调用方提供的 unknown message；对非法 UTF-8 返回 `"{provider} provider error message is not valid UTF-8: ..."`。
- 将 `SqliteSkillBinding` 使用的 `LoadedSqliteApi::take_last_error_message` 改为调用通用 helper，并保留 `"unknown SQLite host error"` 空指针语义。
- 将 `LanceDbSkillBinding` 使用的 `LoadedLanceDbApi::take_last_error_message` 改为调用通用 helper，并保留 `"unknown LanceDB host error"` 空指针语义。
- 新增 `provider_last_error_message_marks_invalid_utf8`，验证非法 UTF-8 provider 错误文本会被标记为诊断失败。
- 新增 `provider_last_error_message_uses_unknown_message_for_null`，验证空指针仍返回 provider 专属 unknown message。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test provider_last_error_message` 通过，2 个 last-error 解码测试全部通过。
- 修改后：`cargo test provider_ffi_c_string` 通过，2 个原有 provider FFI 字符串测试全部通过。
- 修改后：`cargo test sqlite` 通过，4 个 SQLite 相关测试全部通过。
- 修改后：`cargo test lancedb` 通过，3 个 LanceDB 相关测试全部通过。
- 修改后：`cargo test provider` 通过，16 个 provider 相关测试全部通过。
- 搜索复查：`decode_non_null_ffi_c_string(ptr).unwrap_or_else(|error| error)` 在 `src/providers` 中已不存在。
- 全量验证：`cargo test` 通过，299 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变 SQLite/LanceDB 动态库加载、API 符号解析、操作分发、原生 handle 生命周期或空指针 unknown error 语义。
- 错误显式化只作用于 provider last-error 文本无法按 UTF-8 解码的异常路径；合法 provider 错误文本仍原样返回。
- 修改部分代码审核确认没有引入默认 provider 错误、替换字符、有损转换或多来源字段猜测；错误消息不可解码时会明确标注 provider 名称。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续排查 SQLite 搜索结果字段中的 `unwrap_or_default`、provider status 展示默认值和其他会把缺失/异常数据伪装为空字符串的路径。

## 2026-07-05 第 127 轮：拒绝 SQLite custom word 缺失核心字段

### 问题探索

- 基线延续第 126 轮闭环状态：`cargo test` 通过，299 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 本轮继续排查 SQLite provider 中会把缺失数据伪装为空字符串的 `unwrap_or_default`。
- 已追清相关输出：`execute_result_message`、FTS `title_highlight/content_snippet` 等字段更像可选展示字段，缺失时是否应为 null 需要单独评估；本轮先处理语义最明确的 custom word。
- 已追清执行流：`list_custom_words_json` 在 dynamic-library 模式下调用 `database_list_custom_words` 取得列表 handle，再逐项读取 `custom_word_list_get_word` 与 `custom_word_list_get_weight`，最后返回 `words` 数组给 Lua/宿主调用方。
- 旧实现对 `custom_word_list_get_word` 使用 `api.take_optional_string(...)?.unwrap_or_default()`；如果动态库返回 null，结果会变成 `word: ""`，伪造一条空词记录。
- 长期优化判断：custom word 的 `word` 是记录身份字段，不是可选展示文本；缺失说明动态库返回结构损坏，应显式报错，而不是制造空字符串。

### 执行调整

- 新增 `require_sqlite_dynamic_string`，用于动态库返回记录中的必需字符串字段校验。
- helper 接受 `Option<String>` 与字段名；存在时返回字符串，缺失时返回 `"SQLite dynamic result field `{field}` is missing"`。
- 将 `list_custom_words_json` 中的 `custom_word_list_get_word` 读取改为通过 `require_sqlite_dynamic_string(..., "custom_word.word")` 校验。
- 保留 `weight` 原有读取方式，因为它由动态库数值函数直接返回，不涉及可选字符串缺失。
- 新增 `require_sqlite_dynamic_string_rejects_missing_value`，覆盖存在值与缺失值两条路径。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test require_sqlite_dynamic_string_rejects_missing_value` 通过，1 个目标测试通过。
- 修改后：`cargo test sqlite` 通过，5 个 SQLite 相关测试全部通过。
- 修改后：`cargo test provider` 通过，17 个 provider 相关测试全部通过。
- 搜索复查：`custom_word_list_get_word` 的结果不再通过 `unwrap_or_default` 转为空字符串，而是走必需字段校验 helper。
- 全量验证：`cargo test` 通过，300 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变 SQLite custom word 的增删改接口、动态库 handle 生命周期、list_custom_words 的列表长度读取、weight 字段读取或 host/space-controller 模式行为。
- 错误显式化只作用于 dynamic-library 返回 custom word 记录缺失 `word` 的损坏路径；合法 custom word 文本仍原样返回。
- 修改部分代码审核确认没有引入默认词、空字符串兜底、替换字符或多字段猜测；动态库记录缺失核心字段时会明确失败。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续区分 SQLite 执行 message、FTS hit 字段和 provider status 字段中哪些应保留 null，哪些应升级为必需字段错误。

## 2026-07-05 第 128 轮：显式化 `vulcan.json.encode` 序列化失败

### 问题探索

- 基线延续第 127 轮闭环状态：`cargo test` 通过，300 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 本轮继续排查 `unwrap_or_default`，定位到 `src/runtime/engine.rs` 中 `vulcan.json.encode` 的实现。
- 已追清执行流：Lua 调用 `vulcan.json.encode(value)` 后，运行时先用 `lua_value_to_json` 转换 Lua 值，再通过 `serde_json::to_string(&value)` 生成 JSON 文本并返回 Lua 字符串。
- 旧实现对 `serde_json::to_string(&value)` 使用 `unwrap_or_default()`；如果 JSON 文本序列化失败，会返回空字符串。
- 已对照文档事实：文档说明不可 JSON 化的 Lua 值传给 `vulcan.json.encode(...)` 应抛运行时错误；空字符串兜底与该语义不一致，也会让调用方把失败误认为合法 JSON 文本。
- 长期优化判断：即使 `serde_json::Value` 正常情况下几乎不会序列化失败，公开 JSON 编码 API 也不能定义“失败即空字符串”；必须显式返回 runtime error。

### 执行调整

- 将 `vulcan.json.encode` 中的 `serde_json::to_string(&value).unwrap_or_default()` 改为 `map_err`。
- 序列化失败时返回 `json.encode: failed to serialize JSON value: ...` 的 Lua runtime error。
- 保留 `lua_value_to_json` 失败时原有 `json.encode: ...` 错误前缀，确保 function/thread/userdata 等不可表示值仍直接报错。
- 新增 `run_lua_json_encode_rejects_function_value`，通过 `vulcan.json.encode(function() end)` 验证非 JSON Lua 值会失败而不是返回空文本。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test run_lua_json_encode_rejects_function_value` 通过，1 个目标测试通过。
- 修改后：`cargo test run_lua` 通过，5 个 run_lua 相关测试全部通过。
- 搜索复查：`serde_json::to_string(&value).unwrap_or_default` 在 `src/runtime/engine.rs` 中已不存在。
- 全量验证：`cargo test` 通过，301 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变 `vulcan.json.encode` 对合法 JSON 值的输出、`vulcan.json.decode` 行为、Lua 到 JSON 的类型转换规则或 run_lua VM 清理逻辑。
- 错误显式化只作用于 JSON 文本序列化失败和非 JSON Lua 值传入的异常路径；成功路径仍返回 JSON 字符串。
- 修改部分代码审核确认没有引入空字符串兜底、默认 JSON 文本、替换字符或多路径猜测；编码失败会明确暴露给 Lua 调用方。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续排查 runtime session payload 默认值、SQLite FTS hit 字段默认值，以及其他公开 API 中的空值伪装。

## 2026-07-05 第 129 轮：拒绝 runtime lease package 搜索路径非法 UTF-8

### 问题探索

- 基线延续第 128 轮闭环状态：`cargo test` 通过，301 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 本轮继续排查 runtime session payload 与 runtime lease 初始化中的默认值路径，先确认 tombstone 状态比较中的默认值属于内部状态摘要防御，本轮暂不调整。
- 随后定位到 `src/runtime/engine/lease.rs` 的 `configure_runtime_lease_vm`：创建 runtime lease VM 时会读取 Lua `package.cpath` 与 `package.path`，拼接租约提供的 native/Lua 模块根目录前缀，再写回 package 表。
- 已追清执行流：当 `path_context.c_roots` 或 `path_context.lua_roots` 非空时，旧实现会对已有 package 搜索路径执行 `to_str().map(...).unwrap_or_default()`，解码失败时直接把旧搜索路径当成空字符串。
- 旧实现的问题是：Lua VM 暴露的已有 `package.cpath/path` 如果包含非法 UTF-8，runtime lease 初始化会静默丢弃旧搜索路径，后续模块加载失败时难以追溯到真实初始化异常。
- 长期优化判断：runtime lease 的 package 搜索路径是模块解析链路的基础配置，非法 UTF-8 应作为初始化错误显式暴露，不能通过空字符串兜底掩盖。

### 执行调整

- 在 `LuaEngine` 内新增 `runtime_lease_package_search_path_text`，集中解码已有 Lua package 搜索路径。
- helper 对合法 UTF-8 返回原始文本；对非法 UTF-8 返回 `runtime lease package.{field} is not valid UTF-8: ...`。
- 将 `package.cpath` 前缀拼接处改为调用 helper，拒绝非法 UTF-8，而不是把旧 cpath 清空。
- 将 `package.path` 前缀拼接处改为调用 helper，拒绝非法 UTF-8，而不是把旧 path 清空。
- 新增 `runtime_lease_package_search_path_text_preserves_valid_utf8`，验证合法搜索路径原样保留。
- 新增 `runtime_lease_package_search_path_text_rejects_invalid_utf8`，验证非法 UTF-8 搜索路径会携带字段名报错。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test runtime_lease_package_search_path_text` 通过，2 个目标测试全部通过。
- 修改后：`cargo test runtime_lease` 通过，4 个 runtime lease 相关测试全部通过。
- 搜索复查：`configure_runtime_lease_vm` 中 `package.cpath/path` 的旧路径解码不再使用 `unwrap_or_default`。
- 全量验证：`cargo test` 通过，303 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变 runtime lease 的 `lua_roots/c_roots` 前缀格式、package 表读取逻辑、VM 创建流程、cwd 切换逻辑或无额外根目录时的行为。
- 错误显式化只作用于需要前置拼接搜索路径且旧 package 搜索路径不是合法 UTF-8 的异常路径；合法 `package.cpath/path` 仍按原文本保留并拼接。
- 修改部分代码审核确认没有引入替换字符、有损转换、多字段猜测或兼容式 fallback；非法 UTF-8 会在 runtime lease 初始化阶段明确失败。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续排查 SQLite FTS hit 字段默认值、provider status 展示默认值，以及其他公开 API 中把异常输入伪装为空字符串的路径。

## 2026-07-05 第 130 轮：拒绝 SQLite FTS 动态命中字段缺失

### 问题探索

- 基线延续第 129 轮闭环状态：`cargo test` 通过，303 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 本轮继续排查 SQLite provider 中剩余的 `unwrap_or_default`，重点查看 FTS 检索结果的 dynamic-library 分支。
- 已追清执行流：`search_fts_json` 在 dynamic-library 模式下调用 `database_search_fts` 得到结果 handle，再用 `search_result_get_id/file_path/title/title_highlight/content_snippet` 逐条读取命中字段并组装 JSON。
- 旧实现对五个命中字符串 getter 都执行 `api.take_optional_string(...)? .unwrap_or_default()`；如果动态库返回空指针，调用方会收到空字符串命中字段。
- 已查证协议归属：`vldb-controller-client 0.2.1` 的 `ControllerSqliteSearchFtsHit` 中 `id`、`file_path`、`title`、`title_highlight`、`content_snippet` 都是 `String`；对应 `controller.proto` 的 `SqliteSearchFtsHit` 也将这五项定义为非 optional `string` 字段。
- 长期优化判断：合法空文本应由动态库返回空 C 字符串表达；空指针表示字段缺失或结果结构损坏，不能被宿主包装层伪装为空字符串。

### 执行调整

- 复用第 127 轮新增的 `require_sqlite_dynamic_string`，作为 dynamic-library 必需字符串字段校验入口。
- 将 FTS hit 的 `id` 读取改为 `require_sqlite_dynamic_string(..., "fts_hit.id")`。
- 将 FTS hit 的 `file_path` 读取改为 `require_sqlite_dynamic_string(..., "fts_hit.file_path")`。
- 将 FTS hit 的 `title` 读取改为 `require_sqlite_dynamic_string(..., "fts_hit.title")`。
- 将 FTS hit 的 `title_highlight` 读取改为 `require_sqlite_dynamic_string(..., "fts_hit.title_highlight")`。
- 将 FTS hit 的 `content_snippet` 读取改为 `require_sqlite_dynamic_string(..., "fts_hit.content_snippet")`。
- 新增 `require_sqlite_dynamic_string_labels_missing_fts_hit_field`，固定 FTS hit 字段缺失时的诊断文本。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test require_sqlite_dynamic_string` 通过，2 个目标测试全部通过。
- 修改后：`cargo test sqlite` 通过，6 个 SQLite 相关测试全部通过。
- 搜索复查：`search_result_get_id/file_path/title/title_highlight/content_snippet` 的读取路径不再使用 `unwrap_or_default`。
- 修改后：`cargo test provider` 通过，18 个 provider 相关测试全部通过。
- 全量验证：`cargo test` 通过，304 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变 FTS 搜索参数解析、`database_search_fts` 调用、结果 handle 生命周期、score/rank/raw_score 读取、`source/query_mode` 默认值或 space-controller/host-provider 分支输出。
- 错误显式化只作用于 dynamic-library 返回 FTS hit 字符串字段空指针的损坏路径；合法空 C 字符串仍会被解码为空文本并返回。
- 修改部分代码审核确认没有引入多字段猜测、替换字符、有损转换或兼容式 fallback；缺失字段会按所属结果结构精确报错。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续排查 SQLite execute result message 默认值、provider status 展示默认值，以及 skill/download 配置解析中的默认值是否会掩盖真实输入错误。

## 2026-07-05 第 131 轮：拒绝 SQLite 动态执行结果消息缺失

### 问题探索

- 基线延续第 130 轮闭环状态：`cargo test` 通过，304 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 本轮继续排查 SQLite provider 中剩余的 `unwrap_or_default`，定位到 `execute_script_json` 与 `execute_batch_json` 的 dynamic-library 分支。
- 已追清执行流：动态执行脚本和批处理分别调用 `database_execute_script`、`database_execute_batch` 得到共享 execute-result handle，再通过 `execute_result_message` 读取执行消息并组装 JSON。
- 旧实现对 `execute_result_message` 执行 `api.take_optional_string(...)? .unwrap_or_default()`；如果动态库返回空指针，调用方会收到 `message: ""`。
- 已查证协议归属：`vldb-controller-client 0.2.1` 的 `ControllerSqliteExecuteResult` 与 `ControllerSqliteExecuteBatchResult` 都将 `message` 定义为 `String`；对应 proto 的 `ExecuteSqliteScriptResponse` 与 `ExecuteSqliteBatchResponse` 也都使用非 optional `string message`。
- 长期优化判断：执行消息可以是合法空字符串，但这必须由动态库返回空 C 字符串表达；空指针意味着动态结果结构损坏，应显式报错。

### 执行调整

- 将 `execute_script_json` 中 `execute_result_message` 的读取改为 `require_sqlite_dynamic_string(..., "execute_result.message")`。
- 将 `execute_batch_json` 中 `execute_result_message` 的读取改为 `require_sqlite_dynamic_string(..., "execute_batch_result.message")`。
- 保留 `execute_result_success`、`rows_changed`、`last_insert_rowid`、`statements_executed` 的原有读取方式。
- 保留 result handle 销毁顺序：字段读取封装在闭包中，随后先销毁 handle，再传播读取错误。
- 新增 `require_sqlite_dynamic_string_labels_missing_execute_message`，固定执行消息缺失时的诊断文本。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test require_sqlite_dynamic_string` 通过，3 个目标测试全部通过。
- 修改后：`cargo test sqlite` 通过，7 个 SQLite 相关测试全部通过。
- 修改后：`cargo test provider` 通过，19 个 provider 相关测试全部通过。
- 搜索复查：`execute_result_message` 的读取路径不再使用 `unwrap_or_default`。
- 全量验证：`cargo test` 通过，305 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变 SQL 参数解析、动态库执行函数调用、执行结果 handle 生命周期、成功标志读取、行数统计读取或 controller/host-provider 分支输出。
- 错误显式化只作用于 dynamic-library 返回 execute-result message 空指针的损坏路径；合法空 C 字符串仍会被解码为空文本并返回。
- 修改部分代码审核确认没有引入默认消息、替换字符、有损转换、多字段猜测或兼容式 fallback；缺失执行消息会按执行结果类型精确报错。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续排查 FTS 文档写入中的 `title/content` 默认值、provider status 展示默认值，以及 skill/download 配置解析中的默认值是否会掩盖真实输入错误。

## 2026-07-05 第 132 轮：要求 SQLite FTS 文档显式提供标题与正文

### 问题探索

- 基线延续第 131 轮闭环状态：`cargo test` 通过，305 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 本轮继续排查 SQLite FTS 文档写入中的默认值，定位到 `upsert_fts_document_json` 的 `title/content` 输入处理。
- 已追清执行流：space-controller 分支先解析 `index_name/tokenizer_mode/id/file_path/title/content`，再调用 `upsert_sqlite_fts_document`；dynamic-library 分支解析同一组字段后构造 C 字符串并调用 `database_upsert_fts_document`。
- 旧实现对 `title/content` 缺失使用空字符串兜底；调用方如果漏传标题或正文，provider 会收到与显式空文本完全相同的值。
- 已查证协议归属：`vldb-controller-client 0.2.1` 的 `UpsertSqliteFtsDocumentRequest` 与 client 方法都把 `title`、`content` 作为普通字符串实参；动态 FFI 函数签名也要求对应 C 字符串指针。
- 长期优化判断：如果调用方确实要写入空标题或空正文，应显式传入 `""`；字段缺失应作为输入错误暴露，不能由运行时替调用方补空文本。

### 执行调整

- 新增 `require_present_string_field`，用于要求字段存在且类型为字符串，同时允许显式空字符串。
- 将 space-controller 分支的 `title/content` 改为通过 `require_present_string_field` 读取。
- 将 dynamic-library 分支的 `title/content` 改为通过 `require_present_string_field` 读取。
- 保留 `index_name/id/file_path` 使用 `require_string_field` 的非空校验语义。
- 新增 `require_present_string_field_preserves_empty_text`，验证显式空字符串会被保留，缺失字段会报错。
- 新增 `upsert_fts_document_requires_explicit_title_and_content`，验证 FTS 文档写入在访问 controller 前要求显式提供 `title/content`。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test require_present_string_field_preserves_empty_text` 通过，1 个目标测试通过。
- 修改后：`cargo test upsert_fts_document_requires_explicit_title_and_content` 通过，1 个目标测试通过。
- 修改后：`cargo test sqlite` 通过，9 个 SQLite 相关测试全部通过。
- 修改后：`cargo test provider` 通过，21 个 provider 相关测试全部通过。
- 搜索复查：SQLite provider 中已不存在 `title/content` 的 `unwrap_or_default` 或省略即空文本路径。
- 全量验证：`cargo test` 通过，307 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 空白审核：`git diff --check` 通过。

### 代码审核与遗留事项

- 本轮没有改变 FTS 文档写入的 `index_name/id/file_path` 校验、tokenizer mode 默认值、provider 调用参数顺序、动态 FFI handle 生命周期或返回 JSON 结构。
- 输入显式化只作用于缺失或非字符串的 `title/content`；调用方显式传入空字符串仍按空文本写入。
- 修改部分代码审核确认没有引入默认标题、默认正文、替换字符、有损转换或多字段猜测；缺失字段会在 provider 调用前明确失败。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 本轮为用户要求结束目标前的最后一轮，后续不再继续自动循环；若未来重启，可继续排查 provider status 展示默认值与其他剩余默认值路径。

## 2026-07-05 第 133 轮：拒绝 provider status 伪装缺失动态 API 与库元数据

### 问题探索

- 基线延续第 132 轮闭环状态：`cargo test` 通过，307 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 本轮按上一轮遗留候选继续排查 provider status 展示默认值，定位到 SQLite 与 LanceDB 的动态库状态信息路径。
- 已追清 Lua 执行流：`vulcan.sqlite.status()/info()` 与 `vulcan.lancedb.status()/info()` 由 `src/runtime/engine.rs` 的 `register_provider_json_noarg_method` 注册，Lua 调用时直接执行 provider binding 的 `status_json/info_json` 并转成 Lua 表。
- 已追清 SQLite 元数据来源：动态模式下 `SqliteSkillBinding::status_json` 读取必需符号 `vldb_sqlite_library_info_json`，该符号在 `LoadedSqliteApi::from_library` 中加载；因此已进入动态库模式但 API 或 metadata 缺失属于绑定状态或 provider 协议损坏。
- 旧实现问题：SQLite status 会对 `library_info_json` 调用失败、`name/version/ffi_stage/capabilities` 缺失以及 `library_path` 缺失使用 fallback；LanceDB status 会把缺失动态库路径渲染为空字符串。
- 长期优化判断：动态 provider 状态页是诊断入口，不能用 `"unknown"` 或空字符串掩盖损坏状态；动态库 API 缺失或库元数据不符合协议时应明确失败，非动态模式下的合成状态仍可稳定返回。

### 执行调整

- 将 `register_provider_json_noarg_method` 的 resolver 签名从 `Fn() -> Value` 改为 `Fn() -> Result<Value, String>`，让 provider status/info 可以把真实诊断传播为 Lua runtime error。
- 将禁用 provider 的 `status/info` 注册改为显式 `Ok(disabled_status)`，保留未启用 skill 的稳定状态对象。
- 将 `SqliteSkillBinding::status_json/info_json` 改为返回 `Result<Value, String>`；动态库 API 缺失时返回 `SQLite dynamic-library API is unavailable for dynamic_library binding`。
- 新增 SQLite library metadata 校验，要求动态 `library_info_json` 必须是对象，并且必须提供非空 `name/version/ffi_stage` 与数组 `capabilities`。
- 将 SQLite/LanceDB 的动态 `library_path` 展示改为通过 host-visible path helper 输出；缺失时返回 JSON `null`，不再制造空字符串占位。
- 将 `LanceDbSkillBinding::status_json/info_json` 改为返回 `Result<Value, String>`；动态库 API 缺失时返回 `LanceDB dynamic-library API is unavailable for dynamic_library binding`。
- 新增 `sqlite_dynamic_status_without_api_returns_error`、`sqlite_library_info_requires_protocol_fields` 与 `lancedb_dynamic_status_without_api_returns_error`，固定缺失 API 与无效 metadata 的错误语义。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test sqlite_dynamic_status_without_api_returns_error` 通过，1 个目标测试通过。
- 修改后：`cargo test sqlite_library_info_requires_protocol_fields` 通过，1 个目标测试通过。
- 修改后：`cargo test lancedb_dynamic_status_without_api_returns_error` 通过，1 个目标测试通过。
- 全量验证：`cargo test` 通过，310 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 空白审核：`git diff --check` 通过。
- 搜索复查：`library_info_json` 旧 fallback、provider status `library_path` 空字符串兜底、`status_json() -> Value` 与 `Fn() -> Value` 注册签名在本轮相关文件中均已不存在。

### 代码审核与遗留事项

- 本轮没有改变 SQLite/LanceDB 的建表、查询、写入、删除、动态 FFI 操作分发、handle 生命周期或未启用 provider 的状态 JSON 结构。
- 错误显式化只作用于已声明为 dynamic-library 但缺少 API 表，或 SQLite 动态库返回不符合协议的 metadata；host-callback 与 space-controller 的合成状态仍按既有路径返回。
- 修改部分代码审核确认没有引入 `"unknown"` 元数据、空字符串路径占位、多字段猜测或兼容式 fallback；损坏状态会在 status/info 查询处明确暴露。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 按用户补充条件，本轮已处理发现的实际问题；当前验证通过且本轮范围内没有残留问题，可结束本次目标。

## 2026-07-05 第 134 轮：拒绝 input_schema description 非字符串伪装为空描述

### 问题探索

- 基线延续第 133 轮闭环状态：`cargo test` 通过，310 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 本轮继续排查公开 API 边界中会把异常输入伪装为默认值的路径，定位到 `src/skill/manifest.rs` 中的 entry input schema 解析与旧版参数反投影流程。
- 已追清执行流：技能安装或运行时加载 manifest 后会调用 `SkillMeta::resolve_entry_input_schemas`，每个 entry 进入 `SkillToolMeta::resolve_input_schema`；该函数先解析 inline schema、schema 文件或 legacy parameters，再调用 `validate_entry_input_schema_root` 递归校验 JSON Schema，最后在 `parameters` 为空时调用 `derive_legacy_parameters_from_input_schema` 反投影旧版参数。
- 已确认字段归属：`description` 是 JSON Schema 节点上的注解字段；顶层 properties 中的 `description` 会被 `derive_legacy_parameters_from_input_schema` 映射为 `SkillParam.description`，进而影响宿主与模型看到的参数说明。
- 旧实现问题：schema 校验器没有校验 `description` 的类型，而反投影阶段使用 `and_then(JsonValue::as_str).unwrap_or_default()`；如果 manifest 把 `description` 写成数字、对象或数组，运行时会静默把它当成空描述。
- 长期优化判断：`description` 如果存在就必须是字符串；非字符串描述是 manifest/schema 输入错误，不能在 AI-facing 参数描述中被伪装成“没有描述”。

### 执行调整

- 新增 `validate_tool_schema_description_field`，集中校验 JSON Schema 节点上的 `description` 注解。
- 在 `validate_tool_schema_node` 中紧跟 `type` 字段校验后调用 `validate_tool_schema_description_field`，确保递归经过的根节点、properties、items、组合 schema 等节点都执行同一规则。
- 保留合法字符串描述与省略 `description` 的既有行为；本轮只拒绝存在但类型不是字符串的描述。
- 新增 `skill_meta_rejects_non_string_input_schema_description`，验证非字符串 property description 会在旧版参数投影前失败，并返回精确字段路径。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test skill_meta_rejects_non_string_input_schema_description -- --nocapture` 通过，1 个目标测试通过。
- 全量验证：`cargo test` 通过，311 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 空白审核：`git diff --check` 通过。
- 修改范围审核：`git diff -- src\skill\manifest.rs` 确认本轮新增内容仅包含 description 类型校验、对应调用点与测试覆盖；没有改动 schema 解析来源、legacy parameters 构造规则或入口导出结构。

### 代码审核与遗留事项

- 本轮没有改变 `input_schema`、`input_schema_file` 与 legacy `parameters` 的选择顺序，也没有改变 `required`、`type`、`properties`、`items` 等既有 schema 校验语义。
- 合法字符串描述仍会原样投影为旧版参数描述；省略描述仍表示无描述；只有非字符串描述会明确失败。
- 修改部分代码审核确认没有引入空字符串兜底、替换字符、有损转换、多字段猜测或兼容式 fallback；错误会以完整 schema 字段路径暴露给 manifest 作者。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续排查下载 checksum 清单解析中的单字段坏行诊断、GitHub release 错误消息兜底，以及其他公开 manifest/download 边界中的默认值路径。

## 2026-07-05 第 135 轮：拒绝下载 checksum 清单坏行伪装为资产缺失

### 问题探索

- 基线延续第 134 轮闭环状态：`cargo test` 通过，311 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 本轮按第 134 轮遗留候选继续排查下载 checksum 清单解析，定位到 `src/download/manager.rs` 的 `parse_checksum_manifest_for_asset`。
- 已追清执行流：GitHub-managed skill 安装会通过 `resolve_github_managed_skill_release_asset` 解析 `{skill_id}-v{version}-skill.zip`，随后下载 `{skill_id}-v{version}-checksums.txt`，并调用 `parse_checksum_manifest_for_asset` 为目标 zip 提取 SHA-256，再交给 `download_with_sha256` 做下载校验。
- 已查证格式归属：文档要求 GitHub-managed skill release 使用 `{skill_id}-v{version}-checksums.txt` 校验文件；现有测试与解析逻辑均按 sha256sum 风格的 `sha256 file-name` 两列行处理，并保留 `*file-name` 二进制标记。
- 旧实现问题：非空行使用 `parts.next().unwrap_or_default()` 读取 checksum 与文件名；如果某行只有 checksum 没有文件名，该坏行会被当成文件名为空的无关行跳过，最终误报为目标资产缺失。
- 长期优化判断：checksum 清单是下载安全校验链路的一部分；坏行表示清单损坏，应按行号明确失败，不能伪装成“没有目标资产条目”。

### 执行调整

- 将 `parse_checksum_manifest_for_asset` 改为按行号解析非空行，要求每行必须包含一个 SHA-256 值和一个资产文件名。
- 保留 `*asset-name` 的 sha256sum 二进制标记处理。
- 新增对多余字段的拒绝，避免把不符合生成资产名约束的 checksum 行做歧义解释。
- 新增对空资产文件名的显式错误，避免继续把清单损坏降级为未命中。
- 新增 `checksum_manifest_parser_rejects_row_without_asset_name`，固定单字段坏行的诊断文本。
- 新增 `checksum_manifest_parser_rejects_row_with_extra_fields`，固定多余字段坏行的诊断文本。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test checksum_manifest_parser -- --nocapture` 通过，3 个 checksum 清单解析测试全部通过。
- 修改后：`cargo test file_sha256_verification -- --nocapture` 通过，2 个文件校验测试全部通过。
- 全量验证：`cargo test` 通过，313 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 空白审核：`git diff --check` 通过。
- 修改范围审核：`git diff -- src\download\manager.rs` 确认本轮新增内容集中在 checksum 清单行解析和测试覆盖，没有改变 GitHub release 选择、checksum 文件下载、缓存键生成或文件 SHA-256 比对算法。

### 代码审核与遗留事项

- 本轮没有改变 release asset 命名规则、checksum asset 命名规则、下载缓存命中行为、自动重下载逻辑或 SHA-256 值合法性判断。
- 合法 `sha256 file-name` 与 `sha256 *file-name` 行仍按既有方式解析；只有缺失文件名、文件名为空或多余字段的坏行会明确失败。
- 修改部分代码审核确认没有引入空文件名兜底、默认 checksum、替换字符、有损转换、多字段猜测或兼容式 fallback；清单损坏会按具体行号暴露。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续排查 GitHub release 错误消息兜底、下载缓存命中 metadata 读取失败兜底，以及 skill/download 边界中其他默认值路径。

## 2026-07-05 第 136 轮：让 runtime lease tombstone 使用 typed 身份而非展示快照

### 问题探索

- 基线延续第 135 轮闭环状态：`cargo test` 通过，313 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 本轮继续排查剩余 `unwrap_or_default` 默认值路径，定位到 `src/runtime/engine/lease.rs` 的 `RuntimeSessionTombstone::from_snapshot`。
- 已追清执行流：runtime lease 创建时 `RuntimeSessionManager::insert` 构造 `RuntimeSession`，同时缓存一份 `RuntimeSession::status_payload()` 作为 active list 的无锁展示快照；replace、expire 等退役路径进入 `retire_active_lease_locked`，旧实现从这份 JSON snapshot 反解析 `sid/lease_id/generation/profile` 来构造 tombstone。
- 已确认字段归属：`sid`、`lease_id`、`generation` 与 `profile` 是 runtime session 的 typed 身份字段，展示快照只是列表与状态输出载荷，不应该成为终态身份的唯一来源。
- 旧实现问题：`from_snapshot` 对缺失或类型错误的 `sid/lease_id/generation/profile` 使用空字符串、0 或 public profile 兜底；如果 active snapshot 被内部流程污染，终态 tombstone 会带着错误身份继续服务后续 status/eval/close 错误响应。
- 长期优化判断：tombstone 身份应来自创建 lease 时已经确定的 typed 数据，而不是从 host-visible JSON 展示载荷中反推；展示快照损坏不应改变租约恢复语义。

### 执行调整

- 在 `RuntimeSessionEntry` 中新增 typed 身份字段：`sid`、`lease_id`、`generation` 与 `profile`。
- 在 `RuntimeSessionManager::insert` 写入 active entry 时同步保存上述 typed 身份字段。
- 将 `RuntimeSessionTombstone::from_snapshot` 替换为 `RuntimeSessionTombstone::from_entry`，退役时直接从 typed entry 构造 tombstone，不再读取展示 JSON。
- 新增仅测试编译的 `replace_active_snapshot_for_test`，用于模拟 active snapshot 被污染的内部状态。
- 新增 `runtime_session_replaced_tombstone_ignores_corrupted_snapshot_identity`，验证 snapshot 身份被污染后，replace 旧 lease 仍返回真实 `lease_replaced`，并保留原始 SID 与 generation。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test runtime_session_replaced_tombstone_ignores_corrupted_snapshot_identity -- --nocapture` 初次失败，原因为测试误读错误载荷字段；经查证 `runtime_session_error_payload` 使用 `message` 字段后已修正测试断言。
- 修改后：`cargo test runtime_session_replaced_tombstone_ignores_corrupted_snapshot_identity -- --nocapture` 通过，1 个目标测试通过。
- 修改后：`cargo test runtime_session -- --nocapture` 通过，21 个 runtime session 相关测试全部通过。
- 修改后：`cargo test system_runtime_lease -- --nocapture` 通过，1 个 system runtime lease 测试通过。
- 全量验证：`cargo test` 通过，314 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 空白审核：`git diff --check` 通过。
- 搜索复查：`RuntimeSessionTombstone::from_snapshot` 已不存在；tombstone 退役路径改为 `from_entry`，不再对 tombstone 身份字段使用空字符串或 0 兜底。

### 代码审核与遗留事项

- 本轮没有改变 runtime lease 创建、eval、status、list、close 的外部 JSON 结构，也没有改变 SID/generation/profile 校验规则或 tombstone 保留时间。
- active list 仍使用 snapshot 提供无锁展示；只有终态 tombstone 的身份来源改为 typed entry 字段。
- 测试辅助 `replace_active_snapshot_for_test` 仅在 `#[cfg(test)]` 下存在，不进入生产构建。
- 修改部分代码审核确认没有引入空 SID、默认 lease_id、默认 generation、默认 profile、多字段猜测或兼容式 fallback；终态错误身份由 typed lease entry 保证。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续排查 runtime lease 列表排序中对展示 snapshot 的默认值、GitHub release 错误消息兜底，以及缓存 metadata 读取失败兜底。

## 2026-07-05 第 137 轮：拒绝 entry registry 用空目录名参与冲突排序

### 问题探索

- 基线延续第 136 轮闭环状态：`cargo test` 通过，314 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 本轮继续排查剩余默认值路径，定位到 `src/runtime/engine.rs` 的 `rebuild_entry_registry`。
- 已追清加载链路：`load_single_skill` 会从 skill 物理目录 `file_name()` 派生目录 skill id，并要求目录名必须是合法 UTF-8 且匹配 LuaSkills 标识符；`read_skill_manifest_from_directory` 也使用同样的目录名归属规则。
- 已追清 registry 链路：`rebuild_entry_registry` 为全部已加载 entry 收集 `EntrySeed`，再按 `base_name/directory_name/skill_id/local_name/module_name` 排序，用于稳定生成 canonical entry name 与冲突数字后缀。
- 旧实现问题：尽管加载阶段已经校验目录名，registry 重建阶段仍对 `skill.dir.file_name().to_str()` 使用 `unwrap_or_default()`；如果内部 `LoadedSkill.dir` 被构造或后续代码污染为空路径/非法路径，canonical 名称冲突排序会默默使用空目录名。
- 长期优化判断：`directory_name` 是 canonical entry 冲突排序的确定性输入；如果已加载 skill 没有可用目录基名，说明内部状态损坏，应立即失败，而不是让空字符串参与排序。

### 执行调整

- 将 `rebuild_entry_registry` 中的目录名读取改为显式 `ok_or_else`。
- 新增错误信息 `loaded skill '{skill_id}' has invalid directory name: {path}`，并通过已有宿主可见路径渲染器输出路径。
- 保留正常加载路径、冲突编号排序字段与 canonical entry name 生成规则不变。
- 新增 `rebuild_entry_registry_rejects_invalid_loaded_skill_directory_name`，通过测试直接污染 `LoadedSkill.dir`，验证 registry 重建会在派生 canonical 名称前失败。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test rebuild_entry_registry -- --nocapture` 通过，3 个 entry registry 相关测试全部通过。
- 全量验证：`cargo test` 通过，315 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 空白审核：`git diff --check` 通过。
- 搜索复查：`rebuild_entry_registry` 目录名读取路径已不再使用空字符串兜底，并存在目标测试覆盖非法目录名分支。

### 代码审核与遗留事项

- 本轮没有改变 `load_single_skill` 的目录名校验、skill id 绑定、entry duplicate 检查、host reserved name 处理或数字后缀生成规则。
- 正常已加载 skill 的排序行为保持不变；只有内部状态中缺失目录基名时会明确失败。
- 修改部分代码审核确认没有引入默认目录名、替换字符、有损转换、多路径猜测或兼容式 fallback；registry 不变量破坏会在重建阶段暴露。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续排查 GitHub release 错误消息兜底、下载缓存 metadata 读取失败兜底、以及 runtime lease 列表排序对展示 snapshot 的默认值。

## 2026-07-05 第 138 轮：让 runtime lease active list 使用 typed 身份过滤与排序

### 问题探索

- 基线延续第 137 轮闭环状态：`cargo test` 通过，315 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 本轮继续排查 runtime lease 列表排序中对展示 snapshot 的默认值路径，定位到 `src/runtime/engine/lease.rs` 的 `RuntimeSessionManager::list` 与 `compare_runtime_session_payloads`。
- 已追清执行流：`list_runtime_leases_json` 解析可选 SID 后进入 `RuntimeSessionManager::list`；该函数在 manager 锁下遍历 active entries，不锁定每个 VM，而是读取 active entry 上缓存的 `status_payload` snapshot，用于返回宿主可见 lease 列表。
- 已确认字段归属：`sid`、`lease_id`、`generation` 与 `profile` 在第 136 轮已作为 typed identity 存入 `RuntimeSessionEntry`；snapshot 是列表展示载荷，不应驱动过滤、排序或最终身份输出。
- 旧实现问题：list 过滤读取 `entry.snapshot["sid"]`，profile 过滤读取 `entry.snapshot["profile"]`，排序函数对 snapshot 的 `sid/generation/lease_id` 使用空字符串或 0 兜底；如果 snapshot 被内部流程污染，按 SID 查询可能漏掉真实 active lease，排序也会被错误身份带偏。
- 长期优化判断：active list 的身份与排序应由 typed entry 字段保证，展示 snapshot 只应承载非身份展示字段；返回前也应把身份字段恢复为 typed 值，避免把损坏展示快照暴露给宿主。

### 执行调整

- 新增 `RuntimeSessionEntry::list_payload`，复制展示 snapshot 后用 typed `sid/lease_id/generation/profile` 覆盖身份字段。
- `list_payload` 在 snapshot 不是 JSON object 时返回 `lease_snapshot_corrupted`，避免对损坏快照构造模糊列表载荷。
- 将 `RuntimeSessionManager::list` 的 SID 过滤改为读取 `entry.sid`。
- 将 profile 过滤改为读取 `entry.profile`。
- 将列表排序从 `compare_runtime_session_payloads` 改为 `compare_runtime_session_entries`，按 typed `sid/generation/lease_id` 排序。
- 新增 `runtime_session_list_uses_typed_identity_when_snapshot_is_corrupted`，污染 active snapshot 后验证按真实 SID 仍可列出 lease，并且返回身份字段被恢复为真实 typed 值。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test runtime_session_list_uses_typed_identity_when_snapshot_is_corrupted -- --nocapture` 通过，1 个目标测试通过。
- 修改后：`cargo test runtime_session -- --nocapture` 通过，22 个 runtime session 相关测试全部通过。
- 全量验证：`cargo test` 通过，316 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 空白审核：`git diff --check` 通过。
- 搜索复查：旧的 `compare_runtime_session_payloads` 已不存在；runtime lease 文件中不再通过展示 snapshot 默认值驱动 active list 身份排序。

### 代码审核与遗留事项

- 本轮没有改变 runtime lease 创建、eval、status、close 的外部 JSON 结构，也没有改变 active list 的非身份展示字段来源。
- active list 仍使用 snapshot 避免锁定 VM；但身份过滤、排序和返回身份字段由 typed entry 保证。
- 修改部分代码审核确认没有引入空 SID、默认 generation、默认 lease_id、默认 profile、多字段猜测或兼容式 fallback；snapshot 损坏为非对象时会明确返回 `lease_snapshot_corrupted`。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续排查 GitHub release 错误消息兜底、下载缓存 metadata 读取失败兜底、以及 host/skill 边界中其他默认值路径。

## 2026-07-05 第 139 轮：拒绝下载缓存目录伪装为缓存文件

### 问题探索

- 基线延续第 138 轮闭环状态：`cargo test` 通过，316 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 本轮继续排查下载缓存 metadata 读取失败兜底，定位到 `src/download/manager.rs` 的 `DownloadManager::download` 缓存命中分支。
- 已追清执行流：依赖管理器、skill 下载、`fetch_text` 与 `download_with_sha256` 都通过共享 `download` 进入缓存层；`download` 会先确认网络策略与缓存根目录，再用 `cached_path_for_request` 派生确定性缓存路径。
- 已确认字段归属：缓存命中返回给上层的是一个实际载荷文件路径，后续读取文本、解压归档或校验 SHA-256 都依赖该路径是普通文件；目录不是合法下载载荷。
- 旧实现问题：缓存命中只判断 `target_path.exists()`，如果确定性缓存路径被目录占用，会直接返回目录路径；进度回调读取 metadata 时还通过 `unwrap_or_default()` 把 metadata 读取失败伪装成 0 字节缓存。
- 长期优化判断：缓存命中必须显式证明目标是普通文件；metadata 读取失败或路径不是普通文件都表示缓存状态损坏，应直接失败，而不是把无效路径继续交给调用方。

### 执行调整

- 将 `DownloadManager::download` 的缓存命中分支改为先读取 `fs::metadata`，metadata 读取失败时返回带宿主可见路径的错误。
- 增加 `metadata.is_file()` 校验，缓存路径存在但不是普通文件时返回 `Download cache path ... exists but is not a regular file`。
- 进度回调复用同一份已校验 metadata 读取文件长度，移除 0 字节兜底。
- 新增 `download_rejects_cached_directory_instead_of_returning_it`，构造确定性缓存路径被目录占用的损坏状态，验证下载层会在返回载荷路径前明确拒绝。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test download_rejects_cached_directory_instead_of_returning_it -- --nocapture` 通过，1 个目标测试通过。
- 相关验证：`cargo test checksum_manifest_parser -- --nocapture` 通过，3 个 checksum manifest 相关测试全部通过。
- 全量验证：`cargo test` 通过，317 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 空白审核：`git diff --check` 通过。
- 搜索复查：缓存命中分支已存在 metadata 显式读取和普通文件校验，新增测试覆盖目录伪装缓存文件分支。

### 代码审核与遗留事项

- 本轮没有改变缓存 key 派生规则、网络下载流程、写入缓存流程、checksum 校验规则或 GitHub release 解析逻辑。
- 对合法普通文件缓存命中行为保持不变；只有 metadata 无法读取或路径不是普通文件时会明确失败。
- 修改部分代码审核确认没有引入 0 字节兜底、目录载荷兼容、多路径猜测、自动删除重试或无依据的 fallback；损坏缓存状态会直接暴露。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续排查 GitHub release 错误消息兜底、`fetch_text_fresh` 删除缓存文件时对目录路径的行为，以及 host/skill 边界中其他默认值路径。

## 2026-07-05 第 140 轮：拒绝 runlua 子进程输出捕获失败伪装为空输出

### 问题探索

- 基线延续第 139 轮闭环状态：`cargo test` 通过，317 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 本轮继续排查默认值兜底路径，定位到 `src/runtime/engine/runlua.rs` 的 `execute_exec_request` 子进程 stdout/stderr 捕获逻辑。
- 已追清执行流：`vulcan.runtime.lua.exec` 解析 `ExecRequest` 后启动子进程，将 stdout/stderr 设置为 piped，并分别交给 `spawn_pipe_reader` 后台线程读取；子进程结束后 join reader thread，再解码为 `ExecResult` 返回 Lua。
- 已确认字段归属：stdout/stderr 捕获结果是执行包络的一部分；如果管道读取失败或读取线程 panic，代表宿主捕获边界失败，不是业务层“没有输出”。
- 旧实现问题：`spawn_pipe_reader` 对 `read_to_end` 错误直接丢弃，reader thread join 失败又通过 `unwrap_or_default()` 变成空字节；执行结果可能在捕获失败时仍表现为成功且 stdout/stderr 为空。
- 长期优化判断：子进程输出捕获失败必须显式进入 `ExecResult.error`，并保留失败前已经读取到的部分字节；不能用空输出掩盖捕获边界异常。

### 执行调整

- 新增 `PipeCapture`，同时承载已捕获字节与显式捕获错误。
- 将 `spawn_pipe_reader` 改为记录 `read_to_end` 错误，并保留失败前的部分输出。
- 新增 `join_pipe_reader`，将 reader thread panic 转换为 `process stdout/stderr reader thread panicked`，不再使用空输出兜底。
- `execute_exec_request` 解码 stdout/stderr 后合并捕获错误：如果进程退出成功但捕获失败，整体 `success/ok` 为 false，`error` 与 stderr 会包含捕获错误。
- 代码审核中发现并修正本轮引入的错误合并细节：拆分 `process_success` 与整体 `success`，避免捕获失败时误报 `process exited with code 0`。
- 新增 `pipe_reader_reports_read_error_without_dropping_partial_bytes`，验证读取失败会保留部分输出并报告错误。
- 新增 `pipe_reader_join_reports_reader_thread_panic`，验证 reader thread panic 会被转成显式捕获错误。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test pipe_reader_ -- --nocapture` 通过，2 个目标测试通过。
- 相关验证：`cargo test runlua -- --nocapture` 通过，39 个 runlua 相关测试全部通过。
- 全量验证：`cargo test` 通过，319 个测试全部通过。
- 静态验证：首次 `cargo clippy --all-targets -- -D warnings` 失败，原因是新增测试模块位于文件中段；已将测试模块移动到文件末尾。
- 修正后：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 空白审核：`git diff --check` 通过。
- 搜索复查：`join().unwrap_or_default()` 的 stdout/stderr 捕获路径已不存在，reader 失败与 panic 都有目标测试覆盖。

### 代码审核与遗留事项

- 本轮没有改变 exec 请求解析、子进程启动方式、stdin 写入策略、超时轮询、退出码提取或文本编码解码规则。
- 合法 stdout/stderr 捕获行为保持不变；只有管道读取失败或 reader thread panic 会让整体执行包络失败。
- 修改部分代码审核确认没有引入空输出兜底、成功退出码误报、捕获错误吞并、候选字段兼容或多路径猜测；捕获边界异常会显式暴露。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续排查 stdin writer join 被忽略、GitHub release 错误消息兜底，以及 host/skill 边界中其他默认值路径。

## 2026-07-05 第 141 轮：拒绝 runlua 子进程 stdin 写入失败被吞并

### 问题探索

- 基线延续第 140 轮闭环状态：`cargo test` 通过，319 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 本轮顺着第 140 轮遗留继续排查 `src/runtime/engine/runlua.rs` 的 `spawn_stdin_writer` 与 `execute_exec_request`。
- 已追清执行流：`parse_exec_request` 从 Lua table 的 `stdin` 字段解析出可选输入文本；`execute_exec_request` 先按 `stdin_encoding` 编码为字节，子进程启动后把字节交给后台 `spawn_stdin_writer` 写入 child stdin。
- 已确认字段归属：`stdin` 是调用方显式要求交付给子进程的输入；写入或 flush 失败意味着宿主没有完成输入交付，不是可忽略的正常无输入状态。
- 旧实现问题：`spawn_stdin_writer` 对 `write_all` 和 `flush` 的错误全部丢弃，主线程对 writer thread 的 `join` 结果也直接忽略；即使 stdin 没有写入成功，执行结果仍可能只按子进程退出码报告成功。
- 长期优化判断：stdin 写入边界应与 stdout/stderr 捕获边界一样显式化；调用方请求输入时，写入失败或 writer thread panic 都必须进入 `ExecResult.error`。

### 执行调整

- 新增 `StdinWriteResult`，承载 stdin 写入线程的显式错误。
- 将 `spawn_stdin_writer` 改为分别报告 `failed to write process stdin` 与 `failed to flush process stdin`。
- 新增 `join_stdin_writer`，将 writer thread panic 转换为 `process stdin writer thread panicked`。
- `execute_exec_request` 在子进程结束后等待 stdin writer，并把 stdin 写入错误与 stdout/stderr 捕获错误合并为统一的 IO 边界错误集合。
- 更新错误合并注释，明确整体成功要求 stdin/stdout/stderr IO 边界都没有错误。
- 新增 `stdin_writer_reports_write_error`，验证 stdin 写入失败不会被吞并。
- 新增 `stdin_writer_reports_flush_error`，验证 stdin flush 失败不会被吞并。
- 新增 `stdin_writer_join_reports_writer_thread_panic`，验证 stdin writer thread panic 会显式返回。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test stdin_writer_ -- --nocapture` 通过，3 个目标测试通过。
- 回归验证：`cargo test pipe_reader_ -- --nocapture` 通过，2 个 pipe reader 测试全部通过。
- 相关验证：`cargo test runlua -- --nocapture` 通过，42 个 runlua 相关测试全部通过。
- 全量验证：`cargo test` 通过，322 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 空白审核：`git diff --check` 通过。
- 搜索复查：stdin writer 不再忽略 `write_all`、`flush` 或 `join` 错误，新增测试覆盖写入失败、flush 失败和 writer panic。

### 代码审核与遗留事项

- 本轮没有改变 stdin 字段解析、stdin 编码、子进程启动方式、stdout/stderr 解码、超时逻辑或退出码提取规则。
- 合法 stdin 写入行为保持不变；只有调用方请求 stdin 且写入、flush 或 writer thread 失败时，整体执行包络会失败。
- 修改部分代码审核确认没有引入 stdin 写入错误吞并、空错误兜底、成功退出码误报、自动重试或多路径猜测；输入交付失败会显式暴露。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续排查 GitHub release 错误消息兜底、下载/依赖安装中的清理失败吞并，以及 host/skill 边界中其他默认值路径。

## 2026-07-05 第 142 轮：拒绝依赖安装重试前清理失败被吞并

### 问题探索

- 基线延续第 141 轮闭环状态：`cargo test` 通过，322 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 本轮继续排查下载/依赖安装中的清理失败吞并，定位到 `src/dependency/manager.rs` 的依赖安装失败重试分支。
- 已追清执行流：`ensure_dependency` 检测依赖缺失后通过共享下载器获取载荷路径，再调用 `install_downloaded_payload` 安装；若首次安装失败，会尝试删除下载缓存文件与安装根目录，然后重新下载并重新安装。
- 已确认字段归属：`download_path` 是首次安装失败所使用的缓存载荷路径；`resolved_request.install_root` 是本次依赖安装根目录。重试前必须清理这两个位置，否则第二次下载/安装可能继续使用旧坏包或污染安装目录。
- 旧实现问题：重试前的 `fs::remove_file(&download_path)` 与 `fs::remove_dir_all(&resolved_request.install_root)` 都使用 `let _ = ...` 丢弃结果；如果缓存文件删不掉或安装根不是可删除目录，系统仍会进入所谓“redownload and reinstall”。
- 长期优化判断：重试语义必须以清理成功为前提。`NotFound` 可以视为已经清理完成，但其他删除失败必须中断并显式暴露，不能用后续重试掩盖状态不一致。

### 执行调整

- 新增 `cleanup_failed_dependency_install_attempt`，集中执行失败依赖安装的重试前清理。
- 新增 `remove_failed_dependency_download`，删除失败下载载荷；仅将 `NotFound` 视为已清理，其他错误返回 `Failed to remove failed dependency download ... before redownload`。
- 新增 `remove_failed_dependency_install_root`，删除失败安装根目录；仅将 `NotFound` 视为已清理，其他错误返回 `Failed to remove failed dependency install root ... before reinstall`。
- 将依赖安装失败重试分支中的两个 `let _ = ...` 替换为上述 helper 的显式 `?` 传播。
- 新增 `cleanup_failed_dependency_install_attempt_rejects_download_directory`，验证下载缓存路径被目录占用时不会继续进入安装根清理或重试。
- 新增 `cleanup_failed_dependency_install_attempt_rejects_install_root_file`，验证安装根路径被文件占用时会显式失败，并确认已完成的下载载荷清理不会被回滚。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test cleanup_failed_dependency_install_attempt -- --nocapture` 通过，2 个目标测试通过。
- 相关验证：`cargo test dependency -- --nocapture` 通过，18 个依赖相关测试全部通过。
- 全量验证：`cargo test` 通过，324 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 空白审核：`git diff --check` 通过。
- 搜索复查：依赖安装失败重试分支已不再吞掉 `download_path` 与 `install_root` 清理错误，两个目标测试覆盖非文件下载路径和非目录安装根路径。

### 代码审核与遗留事项

- 本轮没有改变依赖声明解析、下载 cache key、下载 URL 解析、归档安装逻辑、导出文件检测或重试次数。
- 合法清理成功路径保持原行为；只有重试前无法删除旧缓存载荷或旧安装根时会提前失败。
- 修改部分代码审核确认没有引入清理失败吞并、自动删除替代路径、权限猜测、重试绕过或多路径 fallback；清理前置条件失败会显式暴露。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续排查 `download_with_sha256` 中校验失败后的缓存删除结果吞并、GitHub release 错误消息兜底，以及 host/skill 边界中其他默认值路径。

## 2026-07-05 第 143 轮：拒绝 checksum 失败后坏缓存删除失败被吞并

### 问题探索

- 基线延续第 142 轮闭环状态：`cargo test` 通过，324 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 本轮继续排查 `download_with_sha256` 中校验失败后的缓存删除结果吞并，定位到 `src/download/manager.rs` 的 SHA-256 自动恢复流程。
- 已追清执行流：`download_with_sha256` 先通过共享 `download` 获取缓存载荷路径，再调用 `verify_file_sha256`；如果 checksum 不匹配，会删除当前缓存文件并再次调用 `download`，期望触发重新下载。
- 已确认字段归属：`target_path` 与 `redownloaded_path` 都是 checksum 校验过的下载载荷路径；当校验失败时，这些路径代表坏缓存，必须在继续恢复流程前被删除。
- 旧实现问题：首次校验失败后的 `fs::remove_file(&target_path)` 和二次校验失败后的 `fs::remove_file(&redownloaded_path)` 都使用 `let _ = ...` 丢弃结果；如果删除失败，下一次 `download` 可能继续命中同一个坏缓存，却被描述为“Automatic redownload”。
- 长期优化判断：checksum 恢复的前置条件是坏缓存已移除。`NotFound` 可以视为已经清理完成，但目录占用、权限失败或其他删除错误必须显式中断恢复流程。

### 执行调整

- 新增 `remove_checksum_mismatched_download`，集中删除 checksum 不匹配的下载载荷。
- `remove_checksum_mismatched_download` 仅将 `NotFound` 视为已清理；其他删除失败返回 `Failed to remove checksum-mismatched download ...`，并包含恢复阶段。
- 首次 checksum 失败后，如果坏缓存删除失败，直接返回原 checksum 错误加清理错误，不再进入自动重新下载。
- 二次 checksum 失败后，如果坏缓存删除失败，返回原 checksum 错误、二次校验错误与清理错误，避免坏缓存残留被静默忽略。
- 新增 `checksum_mismatch_cleanup_rejects_directory_before_redownload`，验证目录占用坏缓存路径时会在自动重新下载前失败。
- 新增 `checksum_mismatch_cleanup_accepts_missing_file`，验证坏缓存已经不存在时恢复清理保持幂等。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test checksum_mismatch_cleanup -- --nocapture` 通过，2 个目标测试通过。
- 相关验证：`cargo test file_sha256_verification -- --nocapture` 通过，2 个文件 SHA-256 校验测试全部通过。
- 全量验证：`cargo test` 通过，326 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 空白审核：`git diff --check` 通过。
- 搜索复查：`download_with_sha256` 中已不存在对 `target_path` 或 `redownloaded_path` 删除结果的吞并，checksum 清理 helper 有目标测试覆盖目录失败与缺失文件幂等分支。

### 代码审核与遗留事项

- 本轮没有改变下载 cache key、网络请求流程、checksum 计算方式、checksum 格式校验或成功路径返回值。
- 合法坏缓存删除成功路径保持原行为；只有坏缓存删除失败时会提前失败。
- 修改部分代码审核确认没有引入坏缓存删除失败吞并、伪自动重新下载、替代路径删除、权限猜测或多路径 fallback；checksum 恢复前置条件失败会显式暴露。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续排查 GitHub release 错误消息兜底、下载文本刷新中的目录删除行为，以及 host/skill 边界中其他默认值路径。

## 2026-07-05 第 144 轮：消除 fresh 文本下载旧缓存清理的 exists/remove 竞态

### 问题探索

- 基线延续第 143 轮闭环状态：`cargo test` 通过，326 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 本轮同时复查 GitHub release 版本错误消息兜底与 `fetch_text_fresh` 旧缓存清理；已确认 GitHub release 的 `last_not_found.unwrap_or_default()` 在当前固定两个候选 tag 循环中不会实际为空，优先级低于文本缓存清理。
- 已追清 `fetch_text_fresh` 执行流：official hub 与 private URL manifest 获取都会调用 `fetch_text_fresh`；该函数构造 `DownloadRequest` 后派生确定性缓存路径，先删除旧缓存，再调用共享 `download` 获取最新文本。
- 已确认字段归属：`cached_path` 是本次 fresh 文本请求的确定性缓存文件路径；旧文本缓存必须在下载前被清理，否则 fresh 语义可能退化成读取旧缓存。
- 旧实现问题：`fetch_text_fresh` 先 `cached_path.exists()` 再 `fs::remove_file`；如果检查后文件已经不存在，会把已经清理完成的状态误报成失败。目录占用缓存路径时也缺少 fresh 文本语义下的明确诊断。
- 长期优化判断：fresh 下载的缓存清理应是幂等的单步删除语义。`NotFound` 应视为已清理，目录、权限或其他删除失败必须显式阻止 fresh 下载。

### 执行调整

- 新增 `remove_stale_text_cache_before_fresh_download`，集中处理 fresh 文本下载前的旧缓存清理。
- `remove_stale_text_cache_before_fresh_download` 删除成功或 `NotFound` 时继续，其他删除错误返回 `Failed to remove stale text cache ... before fresh download`。
- 将 `fetch_text_fresh` 中的 `exists()` + `remove_file()` 替换为无 TOCTOU 的 helper 调用。
- 新增 `fresh_text_cache_cleanup_accepts_missing_file`，验证旧缓存已经不存在时 fresh 清理保持幂等。
- 新增 `fresh_text_cache_cleanup_rejects_directory`，验证目录占用文本缓存路径时会在 fresh 下载前明确失败。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test fresh_text_cache_cleanup -- --nocapture` 通过，2 个目标测试通过。
- 回归验证：`cargo test checksum_mismatch_cleanup -- --nocapture` 通过，2 个 checksum 清理测试全部通过。
- 回归验证：`cargo test download_rejects_cached_directory_instead_of_returning_it -- --nocapture` 通过，1 个缓存目录测试通过。
- 相关验证：`cargo test checksum_manifest_parser -- --nocapture` 通过，3 个 checksum manifest 测试全部通过。
- 全量验证：`cargo test` 通过，328 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 空白审核：`git diff --check` 通过。
- 搜索复查：`fetch_text_fresh` 已不再使用 `exists()` 驱动旧缓存删除，fresh 文本缓存清理 helper 有缺失文件和目录占用目标测试覆盖。

### 代码审核与遗留事项

- 本轮没有改变文本下载 URL、cache key 派生、网络请求流程、读取 UTF-8 文本方式或调用方解析逻辑。
- 合法旧缓存文件删除成功路径保持原行为；旧缓存已经不存在时不再误报失败。
- 修改部分代码审核确认没有引入旧缓存删除失败吞并、目录兼容、自动替代路径、权限猜测或多路径 fallback；fresh 下载前置条件失败会显式暴露。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续排查 GitHub release 错误消息兜底、skill manager 中回滚错误消息拼接兜底，以及 host/skill 边界中其他默认值路径。

## 2026-07-05 第 145 轮：完整报告 GitHub release 版本查询尝试过的标签端点

### 问题探索

- 基线延续第 144 轮闭环状态：`cargo test` 通过，328 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 本轮同时复查 GitHub release 版本错误消息兜底与 skill manager 回滚消息拼接；已确认 skill manager 会执行 rollback，并只在 rollback 失败时追加错误，优先级低于 GitHub release 诊断丢失。
- 已追清 GitHub release 执行流：`resolve_github_release_asset` 调用 `fetch_github_release`；当用户提供显式版本时，会先尝试无 `v` 标签端点，再尝试带 `v` 标签端点；两个端点都 404 后返回版本解析失败。
- 已确认字段归属：每个 `api_url` 都是本次版本解析真实请求过的 GitHub release tag endpoint，错误消息应报告完整尝试集合，供用户定位实际远端路径。
- 旧实现问题：循环中只保存 `last_not_found`，最终错误文案却写成复数 `attempted tag endpoints`；前一个真实尝试端点被丢弃，诊断信息与执行事实不一致。
- 长期优化判断：诊断消息应该完整反映真实执行路径，不应用最后一个路径代表全部尝试，更不应通过空字符串兜底掩盖内部状态。

### 执行调整

- 将 `last_not_found` 替换为 `attempted_tag_urls`，记录所有返回 404 的 tag endpoint。
- 新增 `format_github_release_tag_not_found_error`，集中格式化 GitHub release tag 未找到的错误消息。
- 错误消息从 `attempted tag endpoints ending with ...` 调整为 `attempted tag endpoints: ...`，列出全部真实尝试 URL。
- 移除该路径中的 `unwrap_or_default()` 空字符串兜底。
- 新增 `github_release_tag_not_found_error_reports_all_attempted_endpoints`，验证错误消息同时包含无 `v` 与带 `v` 的两个真实尝试端点。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test github_release_tag_not_found_error_reports_all_attempted_endpoints -- --nocapture` 通过，1 个目标测试通过。
- 回归验证：`cargo test fresh_text_cache_cleanup -- --nocapture` 通过，2 个 fresh 文本缓存清理测试全部通过。
- 回归验证：`cargo test checksum_mismatch_cleanup -- --nocapture` 通过，2 个 checksum 清理测试全部通过。
- 相关验证：`cargo test checksum_manifest_parser -- --nocapture` 通过，3 个 checksum manifest 测试全部通过。
- 全量验证：`cargo test` 通过，329 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 空白审核：`git diff --check` 通过。
- 搜索复查：`last_not_found` 已不存在，GitHub release 显式版本解析失败路径改为报告完整 `attempted_tag_urls`。

### 代码审核与遗留事项

- 本轮没有改变 GitHub release tag 候选顺序、网络请求流程、404 处理规则、非 404 错误传播或 release asset 选择规则。
- 合法 release 命中路径保持不变；只有两个 tag endpoint 都未命中时的错误诊断更完整。
- 修改部分代码审核确认没有引入空 endpoint 兜底、最后路径代替全部路径、隐藏请求路径、多版本兼容猜测或额外网络请求；错误消息来自真实尝试集合。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续排查 skill manager 中回滚错误消息拼接兜底、时间戳默认值兜底，以及 host/skill 边界中其他默认值路径。

## 2026-07-05 第 146 轮：拒绝 skill manager 时间戳异常伪装为 Unix epoch

### 问题探索

- 基线延续第 145 轮闭环状态：`cargo test` 通过，329 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 本轮同时复查 skill manager 回滚错误消息拼接兜底与生命周期时间戳默认值；已确认回滚路径会执行 rollback，只有 rollback 失败时才追加错误，优先级低于时间戳异常被静默写成 0。
- 已追清时间戳执行流：禁用技能写入 `DisabledSkillRecord.disabled_at_unix_ms`；卸载、安装、更新流程分别用时间戳构造生命周期临时目录或备份目录；安装与更新记录写入 `InstalledSkillRecord.installed_at_unix_ms`。
- 已确认字段归属：这些时间戳都来自 skill manager 内部的 `current_unix_millis`，不由外部输入、manifest 或调用方提供。
- 旧实现问题：`SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis()` 会在系统时钟早于 Unix epoch 时返回 0，导致状态记录和目录名出现伪造的 epoch 时间戳。
- 长期优化判断：生命周期状态和目录命名依赖时间戳表达真实执行时间；如果系统时钟异常，应该显式失败并报告具体生命周期上下文，而不是继续写入错误状态或创建 `*-0` 目录。

### 执行调整

- 将 `current_unix_millis` 从无失败返回值改为 `current_unix_millis(context: &str) -> Result<u128, String>`。
- 新增 `unix_millis_from_system_time`，集中转换 `SystemTime` 并在早于 Unix epoch 时返回包含上下文的错误。
- 禁用记录、卸载备份目录、安装临时目录、安装记录、更新临时目录、更新备份目录、更新记录全部改为通过 `?` 显式传播时间戳异常。
- 移除时间戳路径中的 `unwrap_or_default()` 默认 0 行为，避免把异常时钟伪装成合法 epoch。
- 新增 `unix_millis_from_system_time_accepts_post_epoch_time`，验证正常 epoch 之后时间戳仍按毫秒返回。
- 新增 `unix_millis_from_system_time_rejects_pre_epoch_time`，验证早于 epoch 的时间戳会显式失败且错误中包含调用上下文。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test unix_millis_from_system_time -- --nocapture` 通过，2 个目标测试通过。
- 相关验证：`cargo test skill_manager -- --nocapture` 通过，1 个目标测试通过。
- 回归验证：`cargo test skill -- --nocapture` 通过，91 个 skill 相关测试全部通过。
- 全量验证：`cargo test` 通过，331 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 空白审核：`git diff --check` 通过。
- 搜索复查：`duration_since(UNIX_EPOCH)` 只剩在 `unix_millis_from_system_time` 内；skill manager 时间戳路径已不再使用 `unwrap_or_default()`。

### 代码审核与遗留事项

- 本轮没有改变生命周期根目录、状态文件结构、正常系统时钟下的时间戳数值、安装包解压流程、备份目录层级或回滚执行顺序。
- 正常时间戳路径保持原行为；只有系统时钟早于 Unix epoch 这类异常状态会从静默写 0 改为显式失败。
- 修改部分代码审核确认没有引入多来源时间兜底、候选路径轮询、目录名替代方案、错误吞并或兼容式猜测；所有时间戳调用点都带明确上下文。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续排查 skill manager 中回滚错误消息拼接兜底、GitHub/source 解析默认值兜底，以及 host/skill 边界中其他默认值路径。

## 2026-07-05 第 147 轮：让 vulcan.fs.stat 修改时间异常显式失败

### 问题探索

- 基线延续第 146 轮闭环状态：`cargo test` 通过，331 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 本轮先复查 skill manager 回滚消息、GitHub 来源解析默认空段、全仓默认值与静默失败点；已确认回滚消息没有吞掉 rollback 失败，GitHub 解析默认空段后续仍有结构校验，优先级低于 `vulcan.fs.stat` 的时间戳静默丢字段。
- 已追清执行流：Lua 调用 `vulcan.fs.stat(path)` 后，注册函数使用 `fs::symlink_metadata` 获取元数据，再调用 `create_vulcan_fs_stat_table` 构造对外 table；`modified_unix_ms` 由 `metadata_modified_unix_ms` 计算后写入返回值。
- 已确认字段归属：`modified_unix_ms` 不是调用方可选输入，而是 `vulcan.fs.stat` 对文件系统元数据的结构化输出；成功 stat 的结果应反映真实修改时间，异常应明确暴露。
- 旧实现问题：`metadata.modified().ok()?` 和 `duration_since(UNIX_EPOCH).ok()?` 会把读取修改时间失败、早于 epoch 的异常时间都折叠成 `None`，最终表现为成功返回但缺少 `modified_unix_ms` 字段。
- 长期优化判断：`modified_unix_ms` 是可观察 API 字段，静默缺失会让调用方误以为目标没有时间戳；应在底层时间异常时返回 `fs.stat` 错误，而不是继续返回不完整 table。

### 执行调整

- 新增 `system_time_to_unix_millis_i64`，将 `SystemTime` 转为 Lua 可安全表示的 Unix 毫秒值，并拒绝早于 epoch 的时间。
- 将 `metadata_modified_unix_ms` 从 `Option<i64>` 改为 `Result<i64, String>`，读取修改时间失败时返回包含 stat 目标路径的错误。
- `create_vulcan_fs_stat_table` 新增 path 参数，用真实 stat 目标生成时间戳诊断上下文。
- `vulcan.fs.stat` 注册点在成功获取 metadata 后，将 `Path::new(&path)` 传入 table 构造函数。
- `create_vulcan_fs_stat_table` 不再条件性写入 `modified_unix_ms`；成功 stat 时必须转换并设置该字段，转换失败则显式返回 runtime error。
- 新增 `system_time_to_unix_millis_i64_accepts_post_epoch_time`，验证正常 epoch 之后时间戳可转换。
- 新增 `system_time_to_unix_millis_i64_rejects_pre_epoch_time`，验证早于 epoch 的时间戳不会再被静默吞掉。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test system_time_to_unix_millis_i64 -- --nocapture` 通过，2 个目标测试通过。
- 回归验证：`cargo test vulcan_fs_stat -- --nocapture` 通过，2 个 `fs.stat` 相关测试通过。
- 回归验证：`cargo test runtime::engine -- --nocapture` 通过，154 个 engine 范围测试全部通过。
- 全量验证：`cargo test` 通过，333 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 空白审核：`git diff --check` 通过。
- 搜索复查：`metadata.modified().ok()?` 与 `duration_since(UNIX_EPOCH).ok()?` 已不在 `vulcan.fs.stat` 时间戳链路中；`modified_unix_ms` 由显式 `Result` 路径产生。
- 验证过程注意：曾误执行 `cargo test system_time_to_unix_millis_i64 vulcan_fs_stat -- --nocapture`，这是 cargo 参数用法错误，不是代码失败；随后已按两个过滤器分别执行并通过。

### 代码审核与遗留事项

- 本轮没有改变 `vulcan.fs.stat` 的路径解析、缺失目标返回 `nil`、symlink metadata 使用方式、kind 判定、size 字段或 readonly 字段。
- 正常文件和目录的 stat 成功路径保持结构化返回；唯一行为变化是修改时间读取失败或早于 epoch 时不再返回缺字段 table，而是显式返回错误。
- 修改部分代码审核确认没有引入时间多来源兜底、字段缺失兼容、默认 0、候选路径轮询或错误吞并；错误消息带有真实 stat 目标路径。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续排查 GitHub/source 解析默认空段、skill manager 回滚消息格式、runtime cache 时间戳默认值，以及 host/skill 边界中其他静默失败路径。

## 2026-07-05 第 148 轮：阻止 runtime tool cache 生成伪 epoch 缓存编号

### 问题探索

- 基线延续第 147 轮闭环状态：`cargo test` 通过，333 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 本轮沿第 147 轮遗留候选继续排查 `runtime/cache.rs` 的时间戳默认值，并同步追踪 `vulcan.cache.put` 对外入口。
- 已追清执行流：Lua 调用 `vulcan.cache.put(value, ttl_sec)` 后，engine 解析当前 tool 或 skill scope，调用 `global_tool_cache().create(&scope, payload, ttl_secs)`；`SharedToolCache::create` 调用 `next_cache_id` 生成返回给调用方的 cache id，并将条目写入共享缓存。
- 已确认字段归属：cache id 由 runtime cache 内部生成，`tc-{unix_ms}-{seq}` 中的 `unix_ms` 来自 `SystemTime::now()`，不是调用方输入，也不是可缺省协议字段。
- 旧实现问题：`SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis()` 会在系统时钟早于 Unix epoch 时生成 `tc-0-*`，把异常时钟伪装成合法缓存编号。
- 长期优化判断：cache id 是返回给 Lua 技能的公开句柄，应该可诊断、可追溯；系统时间异常应阻止创建并返回 `cache.put` 错误，而不是写入带伪 epoch 的缓存条目。

### 执行调整

- 将 `SharedToolCache::create` 从返回 `String` 改为返回 `Result<String, String>`，让缓存编号生成失败可以阻止写入。
- 将 `next_cache_id` 改为返回 `Result<String, String>`，并移除 `duration_since(UNIX_EPOCH).unwrap_or_default()`。
- 新增 `system_time_to_cache_id_unix_millis`，集中转换 cache id 的 Unix 毫秒组成部分，早于 epoch 时返回包含上下文的错误。
- `vulcan.cache.put` 调用点改为将 `SharedToolCache::create` 错误映射成 `cache.put: ...` runtime error。
- 更新 cache 单元测试中的所有 `create` 调用，显式断言创建成功。
- 新增 `cache_id_unix_millis_accepts_post_epoch_time`，验证正常 epoch 之后时间戳可用于 cache id。
- 新增 `cache_id_unix_millis_rejects_pre_epoch_time`，验证早于 epoch 的时间戳不会再生成 `tc-0-*`。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test runtime::cache -- --nocapture` 通过，7 个 cache 模块测试全部通过。
- 回归验证：`cargo test runtime::engine -- --nocapture` 通过，154 个 engine 范围测试全部通过。
- 全量验证：`cargo test` 通过，335 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 空白审核：`git diff --check` 通过。
- 搜索复查：`src/runtime/cache.rs` 中已不存在 `unwrap_or_default()`；cache id 时间戳只通过 `system_time_to_cache_id_unix_millis` 显式转换。
- 验证过程注意：探索阶段有两条 `rg` 命令因路径通配或复杂正则写法失败，随后已用明确路径和简单固定搜索重跑并完成复核。

### 代码审核与遗留事项

- 本轮没有改变缓存命名空间隔离、TTL 默认值、TTL 上限裁剪、1 秒最小 TTL、过期清理、容量淘汰、读取或删除逻辑。
- 正常系统时钟下的 cache id 格式仍是 `tc-{unix_ms}-{seq}`；唯一行为变化是系统时钟早于 Unix epoch 时不再创建缓存条目。
- 修改部分代码审核确认没有引入默认 0、备用时间来源、候选编号、错误吞并、写入后补救或兼容式兜底；cache id 生成失败会在写入前停止。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续排查 host callbacks/controller 的时间戳默认值、GitHub/source 解析默认空段、skill manager 回滚消息格式，以及 runtime engine 中其他静默默认值路径。

## 2026-07-05 第 149 轮：阻止 space controller 注册名使用伪 epoch 时间戳

### 问题探索

- 基线延续第 148 轮闭环状态：`cargo test` 通过，335 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 本轮同时追踪 `host/callbacks.rs` 的 skill operation id 与 `host/controller.rs` 的 controller client registration name；已确认 callbacks operation id 仍需更大范围事件构造调整，优先选择本轮可闭环的 controller 注册名路径。
- 已追清执行流：当数据库后端需要 space controller 时，宿主通过 `LuaRuntimeSpaceControllerBridge::new` 创建控制器桥接；该函数构造 `ClientRegistration.client_name`，格式为 `luaskills-{process_id}-{backend_suffix}-{started_at_ms}`，随后创建 client 并连接 controller。
- 已确认字段归属：`started_at_ms` 来自 controller bridge 内部的 `SystemTime::now()`，用于宿主可见的 controller client registration name，不由调用方输入。
- 旧实现问题：`SystemTime::now().duration_since(UNIX_EPOCH).map(...).unwrap_or_default()` 会在系统时钟早于 Unix epoch 时将 `started_at_ms` 写成 0，生成伪 epoch 的注册名。
- 长期优化判断：controller client registration name 是跨进程通信与诊断中的身份字段；系统时钟异常时应阻止桥接创建并返回明确错误，而不是注册一个看似合法但时间戳错误的 client name。

### 执行调整

- 新增 `system_time_to_controller_start_unix_millis`，集中转换 controller registration 使用的 Unix 毫秒时间戳。
- `LuaRuntimeSpaceControllerBridge::new` 改为通过 helper 计算 `started_at_ms`，并用 `?` 传播早于 epoch 的错误。
- 移除 controller registration 路径中的 `unwrap_or_default()` 默认 0 行为。
- 新增 `controller_start_unix_millis_accepts_post_epoch_time`，验证正常 epoch 之后时间戳可用于注册名。
- 新增 `controller_start_unix_millis_rejects_pre_epoch_time`，验证早于 epoch 的时间戳会显式失败且错误带上下文。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test host::controller -- --nocapture` 通过，6 个 controller 测试全部通过。
- 回归验证：`cargo test host -- --nocapture` 通过，54 个 host 范围测试全部通过。
- 全量验证：`cargo test` 通过，337 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 空白审核：`git diff --check` 通过。
- 搜索复查：`host/controller.rs` 中 controller registration 时间戳路径已不再使用 `unwrap_or_default()`；`host/callbacks.rs` 的 operation id 时间戳默认值仍作为后续候选保留。

### 代码审核与遗留事项

- 本轮没有改变 controller endpoint 默认值、auto-spawn 配置、进程模式映射、Tokio runtime 创建、client connect、binding scope 解析或 shutdown 逻辑。
- 正常系统时钟下的 client name 格式保持 `luaskills-{process_id}-{backend_suffix}-{started_at_ms}`；唯一行为变化是系统时钟早于 Unix epoch 时桥接创建失败。
- 修改部分代码审核确认没有引入默认 0、备用时间来源、候选注册名、错误吞并、连接后补救或兼容式兜底；异常时间会在创建 controller client 前停止。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续排查 `host/callbacks.rs` 的 skill operation id 时间戳默认值、GitHub/source 解析默认空段、skill manager 回滚消息格式，以及 runtime engine 中其他静默默认值路径。

## 2026-07-05 第 150 轮：阻止 skill operation progress id 使用伪 epoch 时间戳

### 问题探索

- 基线延续第 149 轮闭环状态：`cargo test` 通过，337 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 本轮沿第 149 轮遗留候选继续排查 `host/callbacks.rs` 的 skill operation id 时间戳默认值，并追踪到唯一生产调用点位于 skill install/update apply 流程。
- 已追清执行流：`LuaEngine::apply_skill_lifecycle_action` 创建 `RuntimeSkillOperationProgressEmitter`；emitter 构造 `operation_id`；后续所有 `RuntimeSkillOperationProgressEvent` 都携带该 `operation_id`，宿主进度回调通过 JSON FFI 或标准 FFI 接收该字段。
- 已确认字段归属：`operation_id` 是进度事件的稳定对外身份字段，格式为 `skill-{action}-{skill_fragment}-{timestamp}`；其中 `timestamp` 由 callbacks 内部 `SystemTime::now()` 生成，不由调用方输入。
- 旧实现问题：`SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis()` 会在系统时钟早于 Unix epoch 时生成 `skill-*-*-0`，把异常时钟伪装成合法 operation id。
- 长期优化判断：progress operation id 用于将同一生命周期操作的多条进度事件关联起来；系统时间异常时应在创建 progress emitter 前失败，而不是发出带伪 epoch 的事件序列。

### 执行调整

- 将 `RuntimeSkillOperationProgressEmitter::new` 从返回 `Self` 改为返回 `Result<Self, String>`。
- 将 `build_skill_operation_id` 从返回 `String` 改为返回 `Result<String, String>`，并移除该路径中的 `unwrap_or_default()`。
- 新增 `system_time_to_skill_operation_unix_millis`，集中转换 operation id 使用的 Unix 毫秒时间戳，早于 epoch 时返回包含上下文的错误。
- `LuaEngine::apply_skill_lifecycle_action` 改为显式传播 progress emitter 创建失败，不再继续进入 manager、reload 或 commit 流程。
- 更新 callbacks 单元测试中的所有 emitter 创建点，显式断言创建成功。
- 新增 `skill_operation_unix_millis_accepts_post_epoch_time`，验证正常 epoch 之后时间戳可用于 operation id。
- 新增 `skill_operation_unix_millis_rejects_pre_epoch_time`，验证早于 epoch 的时间戳不会再生成伪 operation id。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test host::callbacks -- --nocapture` 通过，4 个 callbacks 测试全部通过。
- 回归验证：`cargo test host -- --nocapture` 通过，56 个 host 范围测试全部通过。
- 回归验证：`cargo test runtime::engine -- --nocapture` 通过，154 个 engine 范围测试全部通过。
- 全量验证：`cargo test` 通过，339 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 空白审核：`git diff --check` 通过。
- 搜索复查：`host/callbacks.rs` 中 operation id 时间戳路径已不再使用 `unwrap_or_default()`；`duration_since(UNIX_EPOCH)` 只保留在显式 `Result` helper 内。

### 代码审核与遗留事项

- 本轮没有改变进度事件结构、sequence 递增、percent 计算、download progress 映射、回调注册表、JSON FFI 转发或 skill lifecycle 的正常执行顺序。
- 正常系统时钟下的 operation id 格式仍是 `skill-{action}-{skill_fragment}-{timestamp}`；唯一行为变化是系统时钟早于 Unix epoch 时不再创建 progress emitter。
- 修改部分代码审核确认没有引入默认 0、备用时间来源、候选 operation id、错误吞并、事件后补救或兼容式兜底；异常时间会在第一条 progress 事件发出前停止。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续排查 GitHub/source 解析默认空段、skill manager 回滚消息格式、runtime engine apply/reload 错误消息拼接兜底，以及其他静默默认值路径。

## 2026-07-05 第 151 轮：让 GitHub source 解析拒绝空段兜底

### 问题探索

- 基线延续第 150 轮闭环状态：`cargo test` 通过，339 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 本轮同时复查 runtime engine apply/reload 错误消息拼接兜底与 GitHub/source 解析默认空段；已确认 apply/reload 路径没有吞掉 rollback/restore 失败，优先级低于 GitHub source 解析阶段的空字符串兜底。
- 已追清执行流：`resolve_requested_skill_id` 会在 GitHub install/update request 没有显式 skill id 时，先调用 `normalize_github_repo_locator` 规范化 source，再调用 `github_repo_skill_id` 从 repo 段派生 skill id；`prepare_install_skill_from_github` 也会用同一规范化结果校验仓库派生 id 与请求 id 是否一致。
- 已确认字段归属：GitHub source locator 是调用方传入的来源定位值，但 owner/repo 结构由 skill manager 负责解析与校验；派生 skill id 必须来自明确的 repo 段。
- 旧实现问题：`normalize_github_repo_locator` 使用 `segments.next().unwrap_or_default()` 将缺失 owner 或 repo 段折叠为空字符串；`github_repo_skill_id` 使用 `rsplit('/').next().unwrap_or_default()` 将非 owner/repo 定位值继续当作候选 repo 段处理。
- 长期优化判断：GitHub repository locator 是强结构输入，应在解析阶段要求准确的 owner/repo 两段；缺段、空段、额外路径都应该直接报结构错误，而不是通过默认空字符串或 repo-only 候选继续流转。

### 执行调整

- `normalize_github_repo_locator` 改为显式读取 owner、repo 和第三段，要求定位值必须恰好是两个有效段。
- 移除 `normalize_github_repo_locator` 中 owner/repo 空字符串默认值兜底。
- `github_repo_skill_id` 改为使用 `rsplit_once('/')`，缺少 owner/repo 分隔符时直接返回结构错误。
- 移除 `github_repo_skill_id` 中 repo 段默认空字符串兜底。
- 新增 `github_request_derives_skill_id_from_repository_source`，验证 GitHub URL 入口仍可正常派生 skill id。
- 新增 `github_repo_locator_normalization_requires_exact_owner_repo_segments`，覆盖合法 URL、合法 owner/repo、缺 owner、缺 repo、额外路径。
- 新增 `github_repo_skill_id_rejects_locator_without_repo_segment`，验证 repo-only 定位值不会被继续当作派生来源。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test github_repo -- --nocapture` 通过，2 个 GitHub repo 目标测试通过。
- 修改后：`cargo test github_request_derives_skill_id_from_repository_source -- --nocapture` 通过，1 个入口派生测试通过。
- 回归验证：`cargo test skill_manager -- --nocapture` 通过，1 个命中过滤器的测试通过；该过滤器较窄，仅作为补充记录。
- 回归验证：`cargo test skill -- --nocapture` 通过，96 个 skill 范围测试全部通过。
- 全量验证：`cargo test` 通过，342 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 空白审核：`git diff --check` 通过。
- 搜索复查：GitHub source 解析路径已不再使用 `unwrap_or_default()`；`src/skill/manager.rs` 中剩余 `unwrap_or_default()` 位于回滚错误消息拼接候选路径。
- 验证过程注意：探索阶段有一条宽 `rg` 正则因复杂转义写法失败，随后已用固定搜索重跑并完成复核。

### 代码审核与遗留事项

- 本轮没有改变 GitHub release 查询、release asset 选择、checksum 校验、下载缓存、安装 staging、更新比较或 managed install record 写入逻辑。
- 合法 `owner/repo` 与 `https://github.com/owner/repo` source 保持正常；缺 owner、缺 repo、额外路径或 repo-only 输入更早返回结构化错误。
- 修改部分代码审核确认没有引入候选字段名、空段兼容、repo-only fallback、多来源兜底或错误吞并；解析结果只能来自明确 owner/repo 结构。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续排查 skill manager 回滚消息格式、runtime engine apply/reload 错误消息拼接兜底，以及其他静默默认值路径。

## 2026-07-05 第 152 轮：统一 runtime engine 生命周期恢复错误消息

### 问题探索

- 基线延续第 151 轮闭环状态：`cargo test` 通过，342 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 本轮同时复查 skill manager 回滚消息格式与 runtime engine apply/uninstall reload/commit 错误拼接；已确认两者都没有吞掉 rollback 失败，但 runtime engine 有四处重复拼接，且同时处理 rollback 与 runtime restore 两类恢复动作，维护风险更高。
- 已追清执行流：uninstall 路径在 staged uninstall 后先 reload runtime，失败时 rollback staged uninstall 并再次 reload restore；commit uninstall 失败时也执行 rollback 与 restore。install/update apply 路径同样在 reload 或 commit 失败时执行 `rollback_prepared_skill_apply` 与 `reload_from_roots` restore。
- 已确认字段归属：主错误来自 reload 或 commit；rollback 结果来自 skill manager；restore 结果来自 runtime engine 重新加载 roots。三类信息都是真实恢复流程输出，不应通过空字符串默认值参与拼接。
- 旧实现问题：四处重复使用 `rollback_result.err().map(...).unwrap_or_default()` 与 `restore_result.err().map(...).unwrap_or_default()`，靠空字符串表示恢复成功，并在格式串里手工写 `{}.{}{}`。
- 长期优化判断：恢复诊断属于生命周期失败消息的核心审计信息，应由统一 helper 根据真实失败步骤追加，而不是在每个分支手写空字符串兜底。

### 执行调整

- 新增 `format_lifecycle_recovery_error`，统一格式化主失败信息、rollback 失败和 runtime restore 失败。
- uninstall reload 失败路径改为通过 helper 追加 rollback/restore 诊断。
- uninstall commit 失败路径改为通过 helper 追加 rollback/restore 诊断。
- install/update apply reload 失败路径改为通过 helper 追加 rollback/restore 诊断。
- install/update apply commit 失败路径改为通过 helper 追加 rollback/restore 诊断。
- 新增 `lifecycle_recovery_error_keeps_base_message_when_recovery_succeeds`，验证恢复都成功时不再依赖空字符串拼接。
- 新增 `lifecycle_recovery_error_appends_failed_recovery_steps`，验证 rollback 与 runtime restore 真实失败时都会被追加到消息中。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test lifecycle_recovery_error -- --nocapture` 通过，2 个目标测试通过。
- 回归验证：`cargo test runtime::engine -- --nocapture` 通过，156 个 engine 范围测试全部通过。
- 全量验证：`cargo test` 通过，344 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 空白审核：`git diff --check` 通过。
- 搜索复查：`runtime/engine.rs` 中 `rollback_message` 与 `restore_message` 临时变量已不存在；四处生命周期恢复消息都调用 `format_lifecycle_recovery_error`。

### 代码审核与遗留事项

- 本轮没有改变 rollback、runtime restore、reload、commit、progress event、lifecycle event 或 install/update/uninstall 的执行顺序。
- 恢复成功时消息保留主失败信息；rollback 或 restore 失败时才追加对应诊断，不再通过空字符串参与格式化。
- 修改部分代码审核确认没有引入候选恢复路径、错误吞并、默认空消息、重复 reload、额外 rollback 或兼容式兜底；消息只反映真实执行过的恢复动作结果。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续排查 skill manager 回滚消息格式、PATHEXT 默认值路径、runtime engine 其它错误消息兜底，以及其他静默默认值路径。

## 2026-07-05 第 153 轮：统一 skill manager 卸载收尾回滚错误消息

### 问题探索

- 基线延续第 152 轮闭环状态：`cargo test` 通过，344 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 本轮对比复查了 runtime engine 的 PATHEXT 默认路径和 skill manager 卸载收尾错误拼接；PATHEXT 当前用于 Windows 可执行扩展搜索缺省值，属于后续执行策略优化候选，优先级低于已经存在重复空字符串拼接的卸载错误消息路径。
- 已追清直接卸载执行流：`uninstall_skill_in_plane` 根据 skill id 拼出技能目录后调用 `prepare_uninstall_skill_at_path_in_plane` 暂存卸载变更，再调用 `commit_prepared_skill_uninstall` 持久化最终状态；提交失败时立即调用 `rollback_prepared_skill_uninstall` 恢复暂存卸载。`uninstall_skill_at_path_in_plane` 走同一 prepare、commit、rollback 链路，只是由调用方传入已解析目录。
- 已确认调用边界：runtime engine 的状态变更路径会调用 `uninstall_skill_at_path_in_plane`；runtime engine 更完整的 staged reload/finalize 生命周期路径已经在第 152 轮使用 `format_lifecycle_recovery_error` 统一处理 rollback 与 restore 诊断，本轮不重复改动 engine。
- 已确认字段归属：主失败来自 `commit_prepared_skill_uninstall`；回滚结果只来自同一个 manager 的 `rollback_prepared_skill_uninstall`。二者都是明确执行后的真实结果，不需要候选来源或兼容式兜底。
- 旧实现问题：两个直接卸载入口重复构造 `rollback_message`，通过 `rollback_error.err().map(...).unwrap_or_default()` 用空字符串表示回滚成功，再用 `"Failed to finalize uninstall: {}.{}"` 拼接错误消息，导致回滚成功时产生多余句号，也让错误格式规则分散在两个分支里。
- 长期优化判断：卸载收尾失败消息属于生命周期审计信息，应由一个 helper 基于真实 rollback 结果追加诊断；回滚成功时保留主失败，回滚失败时追加具体错误，不能靠空字符串兜底表达状态。

### 执行调整

- 新增 `format_uninstall_finalization_error`，统一格式化卸载收尾失败和回滚失败诊断。
- `uninstall_skill_in_plane` 的提交失败分支改为调用统一 helper，不再手写 `rollback_message` 空字符串兜底。
- `uninstall_skill_at_path_in_plane` 的提交失败分支改为调用统一 helper，不再重复同一段错误拼接逻辑。
- 新增 `uninstall_finalization_error_keeps_base_message_when_rollback_succeeds`，验证回滚成功时消息仅保留主失败信息。
- 新增 `uninstall_finalization_error_appends_rollback_failure`，验证回滚失败时追加明确 rollback 诊断。

### 文件变更清单

- 修改：`src/skill/manager.rs`，新增卸载收尾错误格式化 helper，并替换两个直接卸载入口的重复拼接逻辑。
- 修改：`src/skill/manager/tests.rs`，新增两个 helper 行为测试并引入测试目标。
- 修改：`TASK_LOG.md`，追加本轮任务记录。

### 关键代码调整详情

- `format_uninstall_finalization_error` 接收主错误消息和 rollback 结果；只有 rollback 返回 `Err` 时才追加 `. rollback failed: ...`。
- `uninstall_skill_in_plane` 仍保持 prepare、commit、rollback 的原有顺序，只把提交失败后的错误消息构造委托给 helper。
- `uninstall_skill_at_path_in_plane` 与默认路径卸载入口保持同一错误格式，减少后续维护时两处漂移。
- 新增测试直接覆盖 helper 的两个状态分支，避免用脆弱的文件系统权限场景来模拟 commit 失败。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test uninstall_finalization_error -- --nocapture` 通过，2 个目标测试通过。
- 回归验证：`cargo test skill -- --nocapture` 通过，98 个 skill 范围测试全部通过。
- 全量验证：`cargo test` 通过，346 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 空白审核：`git diff --check` 通过。
- 搜索复查：`src/skill/manager.rs` 与 `src/skill/manager/tests.rs` 中 `rollback_message` 已无命中；卸载收尾失败路径只命中统一 helper 和对应测试。

### 代码审核与遗留事项

- 本轮没有改变卸载 prepare、commit、rollback、状态记录删除、备份目录恢复或 runtime reload 的执行顺序。
- 回滚成功时不再产生多余句号；回滚失败时仍保留明确的 rollback 失败诊断。
- 修改部分代码审核确认没有引入候选路径、错误吞并、多来源兼容、默认空消息、额外 rollback 或额外 reload。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续排查 runtime engine 的 PATHEXT 默认值路径、runtime engine 其它错误消息兜底，以及其他静默默认值路径。

## 2026-07-05 第 154 轮：拆分 runtime engine 的 PATHEXT 缺失与显式空值语义

### 问题探索

- 基线延续第 153 轮闭环状态：`cargo test` 通过，346 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 已追清执行流：`vulcan.process.which` 调用 `resolve_vulcan_process_which`；命令名查找会遍历宿主 `PATH`，每个基础路径交给 `find_vulcan_process_candidate`，再由 `vulcan_process_candidate_paths` 在 Windows 上按 PATHEXT 扩展候选路径。runlua 的 shell launcher 发现和 `process.exec` 的非默认 shell 路径解析也复用同一 `resolve_vulcan_process_which`。
- 已确认字段归属：`PATH` 与 `PATHEXT` 都来自宿主进程环境；PATHEXT 的存在、缺失、具体字符串内容都由宿主环境唯一决定，runtime engine 只负责解析，不应把显式空值当作缺失值。
- 旧实现问题：`vulcan_process_windows_pathexts` 使用 `std::env::var("PATHEXT").ok().map(...)` 再 `unwrap_or_default()`，把 PATHEXT 缺失、无法读取为 Unicode、显式为空或只有分隔符的情况折叠成空列表，并统一退回 `.com/.exe/.bat/.cmd` 默认扩展。
- 长期优化判断：PATHEXT 缺失时使用默认 Windows 扩展是合理宿主兜底；但 PATHEXT 已存在且显式为空时，应尊重该环境配置，不再追加扩展。PATHEXT 存在但不可表示为 UTF-8 时，应显式返回错误，而不是继续猜默认值。
- 探索过程注意：有一条宽 `rg` 正则因引号转义失败，随后已用固定字符串搜索重跑并完成复核。

### 执行调整

- 新增 `default_vulcan_process_windows_pathexts`，只负责 PATHEXT 缺失时的默认扩展列表。
- 新增 `parse_vulcan_process_windows_pathexts`，只负责解析已存在的 PATHEXT 字符串；空条目会被过滤，解析结果为空时保持为空。
- `vulcan_process_windows_pathexts` 改为返回 `Result<Vec<String>, String>`，显式区分存在、缺失和非 Unicode 三类情况。
- `vulcan_process_candidate_paths` 改为返回 `Result<Vec<PathBuf>, String>`，将 PATHEXT 解析错误向上交给 `find_vulcan_process_candidate` 和 `resolve_vulcan_process_which`。
- `resolve_vulcan_process_which` 保持查找失败返回 `Ok(None)`，仅在当前目录或可执行查找环境不可解析时返回错误。
- 新增 `vulcan_process_candidate_paths_respects_empty_windows_pathext`，验证显式为空或只有分隔符的 PATHEXT 不再回退默认扩展。
- 新增 `vulcan_process_candidate_paths_uses_default_windows_pathext_when_missing`，验证 PATHEXT 缺失时仍使用默认扩展。
- 调整既有 `vulcan_process_candidate_paths_appends_windows_pathexts`，显式解包新的 `Result` 并继续验证 PATHEXT 扩展顺序。

### 文件变更清单

- 修改：`src/runtime/engine.rs`，拆分 PATHEXT 默认值与解析逻辑，并把候选路径生成改为可返回错误。
- 修改：`src/runtime/engine/tests.rs`，补充 Windows PATHEXT 显式空值与缺失值的边界测试。
- 修改：`TASK_LOG.md`，追加本轮任务记录。

### 关键代码调整详情

- `vulcan_process_windows_pathexts` 现在通过 `std::env::var("PATHEXT")` 的 `Ok`、`NotPresent`、`NotUnicode` 三个分支保留真实环境状态。
- `parse_vulcan_process_windows_pathexts` 继续规范化大小写与缺失点号，但不会在解析结果为空时自行注入默认值。
- `vulcan_process_candidate_paths` 在基础路径已有扩展名时仍只返回原始路径；无扩展名时才读取 PATHEXT 并追加扩展候选。
- `resolve_vulcan_process_which` 对显式路径和 PATH 遍历路径都传播候选生成错误，Lua 层仍通过原有 `mlua::Error::runtime` 报出错误文本。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test vulcan_process_candidate_paths -- --nocapture` 通过，3 个目标测试通过。
- 回归验证：`cargo test process_which -- --nocapture` 通过，2 个 process.which 相关测试通过。
- 回归验证：`cargo test process_exec -- --nocapture` 通过，5 个 process.exec 相关测试通过。
- 回归验证：`cargo test runtime::engine -- --nocapture` 通过，158 个 runtime engine 范围测试全部通过。
- 全量验证：`cargo test` 通过，348 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 空白审核：`git diff --check` 通过。
- 搜索复查：PATHEXT 路径中 `from_env` 与 `unwrap_or_default()` 已无命中；剩余 `unwrap_or_default()` 命中位于 `src/runtime/engine/tests.rs` 的 `tool_name` 测试请求字段，非本轮执行流。

### 代码审核与遗留事项

- 本轮没有改变 PATH 遍历顺序、显式路径判断、可执行文件判定、runlua launcher 默认 shell 选择或 `process.exec` 命令执行逻辑。
- PATHEXT 缺失和 PATHEXT 显式为空现在语义分离；缺失使用默认扩展，显式为空不追加扩展。
- 修改部分代码审核确认没有引入候选环境来源、静默默认值吞并、PATH 顺序改变、额外可执行探测或错误吞并。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续排查 runtime engine 其它错误消息兜底、`host_result` 默认值路径、runlua 请求默认值路径，以及测试文件中遗留的宽泛默认值用法。

## 2026-07-05 第 155 轮：收紧 host_result capability 的错误字段解析

### 问题探索

- 基线延续第 154 轮闭环状态：`cargo test` 通过，348 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 已追清执行流：宿主通过 `LuaInvocationContext.request_context.client_capabilities` 传入原始能力对象；runtime 在 `populate_vulcan_request_context` 中调用 `resolve_host_result_capability` 生成 `vulcan.context.host_result` helper；工具返回解析时 `parse_tool_call_output` 再次调用同一 capability 解析结果来决定第四返回值 `host_result` 是否启用、允许哪些 kind、以及 payload 大小限制。
- 已确认字段归属：`client_capabilities.host_result` 只能来自宿主请求上下文；`enabled`、`allowed_kinds`、`max_payload_bytes` 都是宿主声明的桥接能力字段，不应由 runtime 猜测字段类型或过滤错误字段。
- 旧实现问题：`allowed_kinds` 通过 `and_then(Value::as_array)` 读取后使用 `unwrap_or_default()`，当字段缺失、字段不是数组、数组元素不是字符串时都会变成空列表；而 `RuntimeHostResultCapability::allows_kind` 把空列表解释为“不限制 kind”，会把宿主本来想限制的错误配置放大成允许全部 kind。
- 同类问题：`max_payload_bytes` 通过 `and_then(Value::as_u64)` 和 `filter(|value| *value > 0)` 把错误类型、负数、零值或无法转换的数字吞成 `None`，等价于取消 payload 大小限制。
- 长期优化判断：capability 是安全边界的一部分。缺失 capability 或缺失字段可以有明确默认值；但字段一旦显式存在，就必须符合协议类型，否则应报错并停止当前调用上下文构建或工具返回解析。
- 探索过程注意：本轮曾按旧路径查找 `src/runtime_options.rs` 失败，随后确认真实类型定义位于 `src/host/options.rs` 与 `src/runtime/context.rs`；另有一条 `src/ffi*` glob 在 PowerShell 路径语法下失败，后续已通过真实路径搜索完成复核。

### 执行调整

- 新增 `disabled_host_result_capability`，集中表达宿主未开启 host_result 时的禁用态能力快照。
- `resolve_host_result_capability` 改为返回 `Result<RuntimeHostResultCapability, String>`。
- `client_capabilities.host_result` 缺失时仍返回禁用态；但字段存在且不是对象时改为显式报错。
- `host_result.enabled` 缺失时仍默认为 `false`；但字段存在且不是布尔值时改为显式报错。
- `host_result.allowed_kinds` 缺失时仍表示不限制 kind；但字段存在且不是非空字符串数组时改为显式报错。
- `host_result.max_payload_bytes` 缺失或为 null 时仍表示无限制；但字段存在且不是正整数时改为显式报错。
- `populate_vulcan_request_context` 和 `parse_tool_call_output` 均传播 capability 解析错误。
- 新增 `resolve_host_result_capability_allows_missing_allowed_kinds`，验证 debug 路径常用的 `enabled=true` 且缺少 `allowed_kinds` 仍保持合法。
- 新增 `resolve_host_result_capability_rejects_malformed_allowed_kinds`，验证错误类型的 `allowed_kinds` 不再变成 unrestricted kind 列表。

### 文件变更清单

- 修改：`src/runtime/engine/host_result.rs`，收紧 host_result capability 字段解析并移除 `allowed_kinds` 的默认空列表吞错。
- 修改：`src/runtime/engine.rs`，传播 host_result capability 解析错误。
- 修改：`src/runtime/engine/tests.rs`，新增 host_result capability 正常缺省和错误字段测试。
- 修改：`TASK_LOG.md`，追加本轮任务记录。

### 关键代码调整详情

- `resolve_host_result_capability` 现在区分“字段缺失”和“字段存在但格式错误”：缺失走协议默认值，格式错误直接返回带字段路径的错误信息。
- `allowed_kinds` 的空列表语义仍然是“不限制 kind”，但只有字段缺失或显式空数组能产生该结果；字符串、数字、对象、空字符串元素都会报错。
- `max_payload_bytes` 的 `None` 语义仍然是“不限制大小”，但只有字段缺失或 null 能产生该结果；零、负数、非整数类型都会报错。
- `parse_tool_call_output` 在读取 Lua 返回值前先解析 capability；如果宿主 capability 本身错误，工具结果不会继续按放宽权限处理。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test resolve_host_result_capability -- --nocapture` 通过，2 个目标测试通过。
- 回归验证：`cargo test host_result -- --nocapture` 通过，2 个 host_result 相关测试通过。
- 回归验证：`cargo test change_set -- --nocapture` 通过，9 个 change_set 相关测试通过。
- 回归验证：`cargo test runtime::engine -- --nocapture` 通过，160 个 runtime engine 范围测试全部通过。
- 全量验证：`cargo test` 通过，350 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 空白审核：`git diff --check` 通过。
- 搜索复查：`src/runtime/engine/host_result.rs` 中 `unwrap_or_default()` 已无命中；剩余 `unwrap_or_default()` 命中位于 `src/runtime/engine/tests.rs` 的 `tool_name` 测试请求字段，非本轮执行流。

### 代码审核与遗留事项

- 本轮没有改变 host_result 缺失时禁用、`allowed_kinds` 缺失时不限制、`max_payload_bytes` 缺失或 null 时不限制的既有显式默认语义。
- 本轮仅收紧显式错误字段，不改变 `change_set` payload 规范化、payload 大小实际校验、kind allowlist 匹配或 Lua 多返回值结构。
- 修改部分代码审核确认没有引入候选字段来源、权限放宽 fallback、错误吞并、额外 Lua 调用或 host_result payload 结构变化。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续排查 runlua 请求默认值路径、`managed_io` append 读取默认值路径、`ffi_standard` host_result 分配吞错路径，以及测试文件中遗留的宽泛默认值用法。

## 2026-07-05 第 156 轮：显式渲染 runlua 非 UTF-8 返回字符串

### 问题探索

- 基线延续第 155 轮闭环状态：`cargo test` 通过，350 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 已追清执行流：`execute_runlua_request_json_inline` 执行隔离 runlua 后，`collect_runlua_return_values` 从 Lua 返回表读取每个返回值，交给 `render_runlua_value` 渲染成 Markdown；普通字符串返回值走 `LuaValue::String` 分支，非字符串或 JSON 转换失败时走 `render_lua_value_inline`。
- 已确认字段归属：runlua 返回值是隔离 Lua VM 的真实执行结果；字符串内容来自 Lua 字节串，不保证一定能表示为 UTF-8。返回值渲染层负责把该事实呈现给宿主，而不是丢弃内容。
- 旧实现问题：`render_runlua_value` 与 `render_lua_value_inline` 都在 `LuaValue::String` 上使用 `to_str().map(...).unwrap_or_default()`，非法 UTF-8 字符串会被渲染成空字符串，导致成功执行结果中的返回值内容被静默吞掉。
- 已确认可复用路径：父模块已有 `render_lua_print_argument`，对非法 UTF-8 Lua 字符串会返回显式 `<invalid UTF-8 Lua string: ...>` 诊断；该策略已由既有 `render_lua_print_argument_marks_invalid_utf8_string` 测试覆盖。
- 长期优化判断：runlua 返回值是调试和自动化执行的重要证据，非法 UTF-8 应显示为诊断文本，不能用空字符串伪装成“返回了空内容”。

### 执行调整

- `render_runlua_value` 的 Lua 字符串分支改为调用 `render_lua_print_argument`，非法 UTF-8 字符串会显示显式诊断。
- `render_lua_value_inline` 改为统一委托 `render_lua_print_argument`，保证 fallback 文本渲染与 print 参数渲染使用同一策略。
- 新增 `execute_runlua_request_inline_marks_invalid_utf8_return_string`，通过真实 runlua 请求返回 `string.char(255)`，验证成功结果中包含非法 UTF-8 诊断。

### 文件变更清单

- 修改：`src/runtime/engine/runlua.rs`，移除 runlua 返回值渲染中的空字符串吞错。
- 修改：`src/runtime/engine/tests.rs`，新增非法 UTF-8 runlua 返回值集成测试。
- 修改：`TASK_LOG.md`，追加本轮任务记录。

### 关键代码调整详情

- `render_runlua_value` 不再直接调用 `LuaString::to_str().unwrap_or_default()`；字符串返回值统一经过 `render_lua_print_argument`。
- `render_lua_value_inline` 不再重复维护字符串、数字、布尔、nil 和 debug fallback 的渲染分支，而是复用已有的安全打印渲染逻辑。
- 新增测试覆盖隔离 runlua VM、返回值收集、Markdown 渲染完整链路，避免只测私有 helper 而漏掉真实入口。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test execute_runlua_request_inline_marks_invalid_utf8_return_string -- --nocapture` 通过，1 个目标测试通过。
- 回归验证：`cargo test runlua -- --nocapture` 通过，43 个 runlua 相关测试通过。
- 回归验证：`cargo test runtime::engine -- --nocapture` 通过，161 个 runtime engine 范围测试全部通过。
- 全量验证：`cargo test` 通过，351 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 空白审核：`git diff --check` 通过。
- 搜索复查：runlua 返回值渲染路径中 `unwrap_or_default()` 已无命中；`src/runtime/engine/runlua.rs` 剩余 `unwrap_or_default()` 位于 `process.exec` timeout 文本路径，非本轮执行流。

### 代码审核与遗留事项

- 本轮没有改变 runlua 执行、返回值收集顺序、JSON 返回值 pretty print、成功/失败 Markdown 结构、print 捕获或 luaexec VM 生命周期。
- 修改部分代码审核确认没有引入候选返回值来源、错误吞并、额外 Lua 执行、结果结构变化或非法 UTF-8 替换为空字符串。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续排查 `process.exec` timeout 文本中的默认值、`managed_io` append 读取默认值路径、`ffi_standard` host_result 分配吞错路径，以及测试文件中遗留的宽泛默认值用法。

## 2026-07-05 第 157 轮：绑定 process.exec 超时状态与显式超时时长

### 问题探索

- 基线延续第 156 轮闭环状态：`cargo test` 通过，351 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 已追清执行流：`vulcan.process.exec` 由 `parse_exec_request` 解析 Lua 输入，`timeout_ms` 字段经 `table_get_optional_timeout_field` 校验为正整数后写入 `ExecRequest.timeout_ms: Option<u64>`；`execute_exec_request` 将该字段转成 `Duration`，在子进程轮询期间达到限制时 kill 子进程并生成 `ExecResult`。
- 已确认字段归属：超时时长唯一来源是 Lua 调用方显式传入的 `timeout_ms`；没有传入时 `ExecRequest.timeout_ms` 为 `None`，也不会进入超时分支。
- 旧实现问题：`execute_exec_request` 用独立布尔值 `timed_out` 表示是否超时，但错误文本又从 `request.timeout_ms.unwrap_or_default()` 取时长；这把“发生超时”与“触发超时的时长”拆成两个可能漂移的状态，并用默认 0 ms 掩盖不变量破裂。
- 长期优化判断：超时结果应由一个状态同时承载是否超时和触发超时的原始请求时长，不能用默认值伪造时长。即使当前控制流下 `None` 不会超时，也应让类型表达该不变量。

### 执行调整

- 将 `execute_exec_request` 中的独立 `timed_out` 布尔状态替换为 `timed_out_after_ms: Option<u64>`。
- 达到 timeout 时记录触发本次终止的具体 `timeout_ms`，后续 `timed_out` 布尔值由该 Option 派生。
- 超时错误文本改为只在 `timed_out_after_ms` 为 `Some(timeout_value)` 时生成，不再调用 `request.timeout_ms.unwrap_or_default()`。
- 新增 `execute_runlua_request_inline_reports_vulcan_process_exec_timeout_ms`，通过真实 `vulcan.process.exec` 命令触发 50ms 超时并验证结果中的 `timed_out`、`success` 和错误文本。
- 新测试加入 `process_env_test_guard`，避免同组 process_exec 测试修改 PATH 时影响 Windows shell 命令内部解析 `ping`。

### 文件变更清单

- 修改：`src/runtime/engine/runlua.rs`，用 `timed_out_after_ms` 绑定超时状态与具体请求时长。
- 修改：`src/runtime/engine/tests.rs`，新增 process.exec 超时错误文本集成测试，并保护 PATH 相关并发环境。
- 修改：`TASK_LOG.md`，追加本轮任务记录。

### 关键代码调整详情

- `execute_exec_request` 在轮询子进程时直接匹配 `request.timeout_ms`，达到限制后写入 `timed_out_after_ms = Some(timeout_ms)`。
- `ExecResult.timed_out` 继续作为布尔输出存在，但由 `timed_out_after_ms.is_some()` 派生，避免状态双写。
- 超时错误文本使用 `if let Some(timeout_value) = timed_out_after_ms` 生成，消除了 `0 ms` 兜底消息的可能性。
- 新增测试覆盖真实 Lua 入口、shell 命令执行、timeout kill、结构化结果返回和 Markdown 渲染链路。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test execute_runlua_request_inline_reports_vulcan_process_exec_timeout_ms -- --nocapture` 通过，1 个目标测试通过。
- 回归验证：首次 `cargo test process_exec -- --nocapture` 出现 1 个失败；原因是新增测试未持有 PATH 环境锁，而同组 Windows PATH 相关测试会临时修改 PATH，导致 shell 内部 `ping` 解析失败并快速退出。已为新增测试补充 `process_env_test_guard` 后重跑通过。
- 回归验证：`cargo test process_exec -- --nocapture` 通过，6 个 process_exec 相关测试全部通过。
- 回归验证：`cargo test runlua -- --nocapture` 通过，44 个 runlua 相关测试全部通过。
- 回归验证：`cargo test runtime::engine -- --nocapture` 通过，162 个 runtime engine 范围测试全部通过。
- 全量验证：`cargo test` 通过，352 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 空白审核：`git diff --check` 通过。
- 搜索复查：`src/runtime/engine/runlua.rs` 的 timeout 错误文本路径中 `unwrap_or_default()` 已无命中；剩余 `unwrap_or_default()` 命中位于 `src/runtime/engine/tests.rs` 的 `tool_name` 测试请求字段，非本轮执行流。

### 代码审核与遗留事项

- 本轮没有改变 timeout 字段校验规则、子进程启动方式、PATH/PATHEXT 查找、stdout/stderr 捕获、exit code 处理或 `ExecResult` 对外字段结构。
- 修改部分代码审核确认没有引入候选超时时长来源、默认 0ms 兜底、错误吞并、额外进程等待或成功状态漂移。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续排查 `managed_io` append 读取默认值路径、`ffi_standard` host_result 分配吞错路径、`runtime/config` 默认值路径，以及测试文件中遗留的宽泛默认值用法。

## 2026-07-05 第 158 轮：阻止 managed_io 追加更新模式吞掉读取失败

### 问题探索

- 基线延续第 157 轮闭环状态：`cargo test` 通过，352 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 已追清执行流：`vulcan.io.open` 和兼容 `io.open` 通过 `open_from_args` 解析 path、mode 与 encoding；`parse_open_mode("a+")` 生成 `ManagedIoModeKind::Append` 且 `update = true`；`ManagedIoFile::open` 会在创建句柄前读取已有文件内容放入内存 buffer，后续 `file:write` 在 append-update 模式下写到 buffer 末尾，`file:read` 可从同一 buffer 读取。
- 已确认字段归属：append-update 的初始 buffer 只能来自目标文件的真实已有内容，或者目标文件不存在时的新文件空内容；目录、权限错误、设备错误等不是“空文件”。
- 旧实现问题：`(ManagedIoModeKind::Append, true)` 直接使用 `fs::read(&path).unwrap_or_default()`，把任何读取失败都变成空 buffer。目录路径、权限错误或其它 IO 错误会被伪装成成功打开空文件，直到后续 flush/write 才可能在更远的位置暴露。
- 长期优化判断：`a+` 对不存在文件创建空 buffer 是合理语义，但只有 `ErrorKind::NotFound` 能表示该语义。其它读失败必须在 open 阶段显式报错，避免把打开失败伪装成空文件并破坏审计链路。

### 执行调整

- `ManagedIoFile::open` 的 append-update 分支改为显式匹配 `fs::read(&path)`。
- 读取成功时保留已有文件内容作为初始 buffer。
- `ErrorKind::NotFound` 时返回空 buffer，保留缺失文件可通过 `a+` 创建的语义。
- 其它读取错误返回 `vulcan.io.open: ...`，不再吞成空内容。
- 新增 `managed_io_open_append_update_creates_missing_file`，验证缺失文件通过 `a+` 可以写入、读取并落盘。
- 新增 `managed_io_open_append_update_rejects_directory_path`，验证目录路径在 `a+` 下会立即报 `vulcan.io.open` 错误。

### 文件变更清单

- 修改：`src/runtime/managed_io.rs`，收紧 append-update 初始内容读取逻辑。
- 修改：`src/runtime/managed_io/tests.rs`，新增 append-update 缺失文件与目录错误两个边界测试。
- 修改：`TASK_LOG.md`，追加本轮任务记录。

### 关键代码调整详情

- 新增 `std::io::ErrorKind` 引入，用于区分目标文件缺失与其它读失败。
- `ManagedIoFile::open` 中 `Append + update` 分支不再调用 `unwrap_or_default()`；三类结果分别处理为已有内容、缺失新文件空 buffer、真实打开错误。
- 新增测试通过公开 `vio.open(path, 'a+')` 入口覆盖真实 Lua 调用路径，而不是直接构造私有结构。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test managed_io_open_append_update -- --nocapture` 通过，2 个目标测试通过。
- 回归验证：`cargo test managed_io -- --nocapture` 通过，18 个 managed_io 相关测试全部通过。
- 回归验证：`cargo test runtime::managed_io -- --nocapture` 通过，12 个 runtime::managed_io 模块测试全部通过。
- 回归验证：`cargo test runtime::engine -- --nocapture` 通过，162 个 runtime engine 范围测试全部通过。
- 全量验证：`cargo test` 通过，354 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 空白审核：`git diff --check` 通过。
- 搜索复查：`src/runtime/managed_io.rs` 中 `unwrap_or_default()` 已无命中；append-update 分支只对 `ErrorKind::NotFound` 返回空 buffer。

### 代码审核与遗留事项

- 本轮没有改变 read、write、append 非 update、write update、tmpfile、popen、encoding 解码或 flush 落盘规则。
- 缺失文件通过 `a+` 创建仍然可用；目录和其它非缺失读失败现在会在 open 阶段显式失败。
- 修改部分代码审核确认没有引入候选内容来源、错误吞并、延迟失败、额外文件写入或 append 写入位置变化。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续排查 `ffi_standard` host_result 分配吞错路径、`runtime/config` 默认值路径、`host/database` 默认值路径，以及测试文件中遗留的宽泛默认值用法。

## 2026-07-05 第 159 轮：传播 ffi_standard host_result 分配错误

### 问题探索

- 基线延续第 158 轮闭环状态：`cargo test` 通过，354 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 已追清执行流：`luaskills_ffi_call_skill` 解析 `tool_name`、`args_json` 与 `invocation_context` 后，通过 `with_engine(...).call_skill(...)` 获取 `RuntimeInvocationResult`，随后调用 `alloc_invocation_result` 分配 `FfiRuntimeInvocationResult` 并写入 `result_out`。
- 已确认字段归属：`RuntimeInvocationResult.host_result` 是运行时结果链路携带的可选 `RuntimeHostResult`；C ABI 对应字段是 `FfiRuntimeInvocationResult.host_result: *mut FfiRuntimeHostResult`，其中 `payload_json` 由 `alloc_host_result` 对 `RuntimeHostResult.payload` 序列化得到。
- 旧实现问题：`alloc_host_result` 返回 `Result<FfiRuntimeHostResult, String>`，但 `alloc_invocation_result` 使用 `alloc_host_result(host_result).ok()` 后再 `unwrap_or(ptr::null_mut())`。一旦宿主结果分配或序列化失败，错误会被吞掉，并表现为一次成功调用但 `host_result` 为空指针。
- 搜索确认：`luaskills_ffi_call_skill` 是 `alloc_invocation_result` 的实际调用点；`luaskills_ffi_run_lua` 走 JSON 文本返回路径，不经过该分配函数。
- 长期优化判断：既然 `alloc_host_result` 的类型已经声明可能失败，调用结果分配必须把该失败传播到 FFI 状态层，而不是伪装成没有宿主结果。这样才能保留审计链路中的结构化结果完整性。

### 执行调整

- 将 `alloc_invocation_result` 的返回类型从 `FfiRuntimeInvocationResult` 调整为 `Result<FfiRuntimeInvocationResult, String>`。
- `host_result` 分配分支改为显式 `match value.host_result.as_ref()`；存在宿主结果时调用 `alloc_host_result(host_result)?`，不存在时才返回空指针。
- `luaskills_ffi_call_skill` 在引擎调用成功后继续匹配 `alloc_invocation_result`，分配失败时通过 `ffi_error_status` 返回错误，不再把错误吞成成功结果。
- 校正 `alloc_host_result` 的函数说明，避免继续把宿主结果误写成调用结果。
- 新增 `alloc_invocation_result_preserves_host_result_payload`，直接覆盖运行时调用结果到 C ABI 调用结果的宿主结果保留行为，并通过公开释放函数验证释放路径匹配。

### 文件变更清单

- 修改：`src/ffi_standard.rs`，传播 `host_result` 分配错误并修正相关函数文档。
- 修改：`src/ffi_standard/tests.rs`，新增调用结果分配保留 `host_result` 的单元测试。
- 修改：`TASK_LOG.md`，追加本轮任务记录。

### 关键代码调整详情

- `alloc_invocation_result` 不再使用 `.ok()` 丢弃 `alloc_host_result` 的错误，而是使用 `?` 将错误返回给上层。
- `luaskills_ffi_call_skill` 的成功分支新增对 `alloc_invocation_result` 的二级匹配：`Ok(ffi_result)` 才写入 `result_out`，`Err(error)` 直接返回 FFI 错误状态。
- 新测试构造带 `RuntimeHostResult { kind: "change_set", payload: ... }` 的 `RuntimeInvocationResult`，断言 C ABI 结果中的 `host_result` 非空、`kind` 正确、`payload_json` 可解析且 `payload_bytes` 与实际 JSON 字节数一致。
- 新测试把结果重新放回堆指针并调用 `luaskills_ffi_invocation_result_free`，确认本轮没有改变原有释放契约。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test alloc_invocation_result_preserves_host_result_payload -- --nocapture` 通过，1 个目标测试通过。
- 回归验证：`cargo test ffi_standard -- --nocapture` 通过，15 个 ffi_standard 相关测试全部通过。
- 全量验证：`cargo test` 通过，355 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 空白审核：`git diff --check` 通过。
- 搜索复查：`alloc_invocation_result` 中已无 `alloc_host_result(...).ok()` 或 `unwrap_or(ptr::null_mut())` 吞错路径；剩余 `.ok()` 命中位于 `read_port` 的 `u16::try_from` 可选端口解析，非本轮执行流。

### 代码审核与遗留事项

- 本轮没有改变 C ABI 结构体布局、`FfiRuntimeHostResult` 字段含义、`RuntimeInvocationResult` 对外结构、`run_lua` JSON 返回路径或 `luaskills_ffi_invocation_result_free` 的释放契约。
- 缺失 `host_result` 的正常情况仍然返回空指针；只有存在 `host_result` 且嵌套分配失败时才改为显式 FFI 错误。
- 修改部分代码审核确认没有引入候选字段来源、多路兜底、错误吞并、结构化结果丢失或内存释放路径漂移。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续排查 `runtime/config` 默认值路径、`host/database` 默认值路径、`ffi_standard` 其它 JSON 序列化 expect 路径，以及测试文件中遗留的宽泛默认值用法。

## 2026-07-05 第 160 轮：显式暴露 managed runtime 状态探测错误

### 问题探索

- 基线延续第 159 轮闭环状态：`cargo test` 通过，355 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 已追清执行流：Lua 侧 `vulcan.runtime.python.status()` 和 `vulcan.runtime.node.status()` 分别进入 `managed_python_status` / `managed_node_status`；当依赖声明与运行时安装清单可解析时，会调用 `resolve_*_env_plan` 得到 `ManagedRuntimeEnvPlan`，再由 `managed_runtime_status_from_plan` 构造返回给 Lua 的状态表。
- 已确认字段归属：状态表中的 `ready` 唯一来自 `managed_env_is_ready(plan)`；该函数先检查 `managed_env_marker_path(plan.env_dir)` 是否存在，不存在才是正常未创建；存在时会通过 `read_managed_env_marker` 读取并解析 `.luaskills-env.json`，读取或解析失败会返回 `Err`。
- 旧实现问题：`managed_runtime_status_from_plan` 使用 `managed_env_is_ready(plan).unwrap_or(false)`，把 marker 读取失败、JSON 损坏等真实错误直接压成 `ready = false`，并返回 `"managed runtime environment is configured but not yet created"`。这会把“环境未创建”和“环境状态损坏/不可读”混为一谈。
- 对比确认：`ensure_managed_env` 对同一个 `managed_env_is_ready(plan)` 使用 `?` 显式传播错误，说明 marker 读取失败在运行时环境管理语义中不是普通未就绪状态。
- 长期优化判断：status API 可以继续返回结构化 table，但必须在状态表中保留 readiness-check 错误，不能用普通 `ready=false` 掩盖环境损坏。

### 执行调整

- 将 `managed_runtime_status_from_plan` 中的 `unwrap_or(false)` 改为显式匹配 `managed_env_is_ready(plan)` 的结果。
- `Ok(ready)` 分支保持原有成功状态表语义：ready 为 true 时返回 ready message，ready 为 false 时返回未创建 message。
- `Err(error)` 分支返回结构化状态表：保留 `available=true`、`configured=true`、plan 元数据与 `ready=false`，同时新增 `message = "managed runtime environment status check failed"` 和 `error` 字段。
- 新增 `managed_runtime_status_reports_invalid_env_marker_error`，构造损坏的 `.luaskills-env.json`，验证 status 表会包含 marker 解析错误和宿主可见 marker 路径。

### 文件变更清单

- 修改：`src/runtime/engine.rs`，让 managed runtime 状态表显式暴露 marker 读取/解析错误。
- 修改：`src/runtime/engine/tests.rs`，新增损坏 marker 状态探测测试。
- 修改：`TASK_LOG.md`，追加本轮任务记录。

### 关键代码调整详情

- `managed_runtime_status_from_plan` 新增 `readiness_result` 局部结果，避免直接把错误降级为布尔 false。
- 错误分支仍保持 status API 的结构化 table 返回契约，不把状态查询变成 Lua 异常；调用方可以通过 `status.error` 精确区分“未创建”和“状态探测失败”。
- 新测试使用 `managed_env_marker_path(&env_dir)` 创建真实 marker 路径，写入非法 JSON，再直接调用 `managed_runtime_status_from_plan(&plan)` 覆盖生产状态构造函数。
- 新测试断言 `available/configured` 仍为 true、`ready` 为 false、`message` 为状态检查失败，并验证 `error` 同时包含 `Failed to parse` 与 `render_host_visible_path(marker_path)`。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test managed_runtime_status_reports_invalid_env_marker_error -- --nocapture` 通过，1 个目标测试通过。
- 回归验证：`cargo test managed_runtime -- --nocapture` 通过，14 个 managed_runtime 相关测试全部通过。
- 回归验证：`cargo test runtime::engine -- --nocapture` 通过，163 个 runtime engine 范围测试全部通过。
- 全量验证：`cargo test` 通过，356 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 空白审核：`git diff --check` 通过。
- 搜索复查：`managed_env_is_ready(plan).unwrap_or(false)` 已无命中；剩余 `unwrap_or(false)` 位于可选布尔字段解析、文件可执行性判断、managed runtime worker envelope 布尔读取等其它链路，非本轮 marker readiness 执行流。

### 代码审核与遗留事项

- 本轮没有改变 `resolve_python_env_plan`、`resolve_node_env_plan`、`ensure_managed_env`、受管 worker 调用、环境创建、marker 写入或 Lua status 函数的 table 返回契约。
- 未声明 `dependencies.yaml`、未声明 `python_runtime/node_runtime`、plan 解析失败等既有状态分支保持不变。
- 修改部分代码审核确认没有引入候选 ready 来源、错误吞并、Lua 异常契约漂移、环境创建副作用或 marker 路径猜测。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续排查 `runtime/config` 默认值路径、`host/database` 默认值路径、`runtime/engine` 其它布尔降级路径，以及测试文件中遗留的宽泛默认值用法。

## 2026-07-05 第 161 轮：校验 managed runtime worker 信封协议

### 问题探索

- 基线延续第 160 轮闭环状态：`cargo test` 通过，356 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 已追清执行流：`vulcan.runtime.python.invoke` / `vulcan.runtime.node.invoke` 解析调用参数后构造 JSON payload，经 `invoke_pooled_managed_runtime` 获取或创建 worker，再由 `invoke_managed_runtime_worker` 通过 stdin 发送一行 JSON 请求，并从 worker stdout 读取一行 JSON envelope。
- 已确认字段归属：worker envelope 的生产端是内置 `managed_python_worker_source` 与 `managed_node_worker_source`；成功 envelope 明确包含 `ok/value/stdout/stderr`，失败 envelope 明确包含 `ok/value/stdout/stderr/error/trace`。Rust 侧 `ManagedRuntimeWorkerInvokeResult.envelope` 是这一行 JSON envelope 的唯一来源。
- 旧实现问题：`managed_runtime_worker_result_to_json` 从 `result.envelope` 读取 `ok/value/stdout/stderr/error/trace` 时使用 `unwrap_or(false)`、`unwrap_or(Value::Null)` 与空字符串兜底。合法 JSON 但缺少 `ok`、`stdout`、`stderr` 或字段类型错误时，会被伪装成普通 `ok=false` 结果，甚至可能把坏 worker 放回池中继续复用。
- 补充发现：Node worker 成功返回 `undefined` 时，`JSON.stringify({ value: undefined })` 会省略 `value` 字段，进而触发同类 envelope 缺字段问题。长期协议应把 JS 的 `undefined` 归一化为 JSON `null`，而不是让协议字段缺失。
- 长期优化判断：worker stdout 是受管运行时的内部线协议边界，协议字段缺失或类型错误必须显式暴露为协议错误，并在归还池之前丢弃该 worker，不能由 Lua-facing 转换层静默补默认值。

### 执行调整

- 在 `invoke_managed_runtime_worker` 解析 worker stdout JSON 后立即调用 `validate_managed_runtime_worker_envelope`。
- 新增 worker envelope 校验辅助函数，要求 envelope 是对象，`ok` 是布尔值，`value` 字段存在，`stdout/stderr` 是字符串，`trace` 只能缺失、为 null 或字符串；当 `ok=false` 时要求 `error` 是非空字符串。
- 当 envelope 合法 JSON 但不满足协议时，设置 `discard_worker = true`，并把 envelope 替换为显式 `managed runtime worker returned malformed JSON envelope: ...` 错误 envelope。
- 将 `managed_runtime_worker_result_to_json` 改为走已校验转换函数；若未来传入未校验 envelope，也返回显式协议错误 payload，不再用缺省字段伪造结果。
- 调整 Node worker 源码，将 handler 返回的 `undefined` 归一化为 `null`，保证成功 envelope 始终包含 `value` 字段。
- 新增 `managed_runtime_worker_rejects_malformed_json_envelope`，通过真实子进程返回缺少 `ok` 字段的 JSON envelope，验证 worker 被标记丢弃且最终 Lua-facing payload 带明确协议错误。

### 文件变更清单

- 修改：`src/runtime/engine.rs`，新增 managed runtime worker envelope 协议校验、协议错误转换和 Node `undefined` 归一化。
- 修改：`src/runtime/engine/tests.rs`，新增坏 envelope worker 集成测试与跨平台坏 worker 命令夹具。
- 修改：`TASK_LOG.md`，追加本轮任务记录。

### 关键代码调整详情

- `invoke_managed_runtime_worker` 对 stdout 行先做 JSON 解析，再对解析出的 `Value` 做协议校验；校验失败时立即设置 `discard_worker = true`。
- 新增 `managed_runtime_worker_protocol_error_envelope`，用于把协议错误转成格式正确的失败 envelope，避免后续转换层再遇到缺字段。
- 新增 `managed_runtime_worker_required_envelope_field`、`managed_runtime_worker_required_bool_envelope_field`、`managed_runtime_worker_required_string_envelope_field` 等辅助函数，把必填字段与类型错误变成精确诊断。
- `managed_runtime_worker_validated_result_to_json` 只从校验后的 envelope 复制字段；`error` 与 `trace` 仍按协议保持可选，但不再对必填字段做默认兜底。
- `managed_runtime_worker_result_to_json` 在遇到未校验坏 envelope 时返回结构化协议错误 payload，并保留 `timed_out`、`worker_reused`、`env_hash`、`env_dir` 等元数据。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test managed_runtime_worker_rejects_malformed_json_envelope -- --nocapture` 通过，1 个目标测试通过。
- 回归验证：`cargo test managed_runtime -- --nocapture` 通过，15 个 managed_runtime 相关测试全部通过。
- 回归验证：`cargo test runtime::engine -- --nocapture` 通过，164 个 runtime engine 范围测试全部通过。
- 全量验证：`cargo test` 通过，357 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 空白审核：`git diff --check` 通过。
- 搜索复查：`managed_runtime_worker_result_to_json` 旧有的 `ok/value/stdout/stderr/error/trace` 默认化读取已消失；剩余 `unwrap_or(false)`、`unwrap_or(Value::Null)`、`String::new()` 命中位于其它路径，非本轮 worker envelope 转换执行流。

### 代码审核与遗留事项

- 本轮没有改变 worker 池大小、worker 获取/释放 API、Python/Node 环境解析、`ensure_managed_env`、timeout 语义、stdout 行读取方式或 Lua-facing payload 的字段集合。
- 正常 worker 成功/失败 envelope 仍按原字段返回；新增行为只影响合法 JSON 但协议字段缺失或类型错误的坏 envelope。
- 修改部分代码审核确认没有引入候选字段来源、静默默认值、坏 worker 复用、Lua 异常契约漂移或环境创建副作用。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续排查 `runtime/engine` 其它布尔降级路径、`runtime/config` 默认值路径、`host/database` 默认值路径，以及测试文件中遗留的宽泛默认值用法。

## 2026-07-05 第 162 轮：暴露 authority 入口注册表失效目标

### 问题探索

- 基线延续第 161 轮闭环状态：`cargo test` 通过，357 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 已扫描剩余默认化候选：`runtime/config` 的缺失命名空间返回空 map、LanceDB 可选二进制载荷缺省、help 文件扩展名无扩展返回 false 等均有明确业务语义；`Path::exists()` 的理论元数据错误问题暂未在当前 Windows 环境中找到稳定复现证据，本轮不据此写猜测修复。
- 已追清执行流：标准 C ABI `luaskills_ffi_is_skill` 与 JSON FFI `luaskills_ffi_is_skill_json` 都会解析 authority 与 tool name 后调用 `engine.is_skill_for_authority(...)`；对应的 skill-owner 查询会调用 `engine.skill_name_for_tool_for_authority(...)`。
- 已确认字段归属：`is_skill_for_authority` 与 `skill_name_for_tool_for_authority` 都先从 `entry_registry` 取得 `ResolvedEntryTarget`，再通过 `target.skill_storage_key` 去 `skills` 中查找所属 `LoadedSkill`，以判断 DelegatedTool 是否应隐藏 ROOT skill。
- 旧实现问题：`entry_target_visible_to_authority` 对 `skills.get(&target.skill_storage_key)` 使用 `map(...).unwrap_or(false)`。当入口注册表里存在 target，但所属 skill 已不在 `skills` 中时，DelegatedTool 查询会把内部注册表不一致伪装成“不可见/未命中”；标准 FFI 和 JSON FFI 也会返回成功状态。
- 长期优化判断：entry registry target 指向缺失 loaded skill 是运行时内部一致性错误，不是正常权限过滤结果。宿主可见查询应显式报错，不能用 false/None 把坏状态藏起来。

### 执行调整

- 将 `entry_target_visible_to_authority` 从 `bool` 改为 `Result<bool, String>`，先校验 target 的 `skill_storage_key` 必须能解析到当前 `skills` 中的 `LoadedSkill`。
- 将 `is_skill_for_authority` 从 `bool` 改为 `Result<bool, String>`；只有 tool name 未注册时返回 `Ok(false)`，注册表目标失效时返回错误。
- 将 `skill_name_for_tool_for_authority` 从 `Option<String>` 改为 `Result<Option<String>, String>`；未注册工具仍返回 `Ok(None)`，失效 target 返回错误。
- 标准 C ABI 和 JSON FFI 去掉 `Ok(engine...)` 包装，直接传播上述引擎错误，使宿主收到 FFI 错误状态而不是成功 false/None。
- 新增 `authority_entry_queries_reject_stale_registry_targets`，构造一个 entry registry 指向不存在 storage key 的最小引擎，验证未注册工具和失效目标的查询语义被区分开。

### 文件变更清单

- 修改：`src/runtime/engine.rs`，收紧 authority-scoped entry 可见性查询并显式暴露 stale registry target。
- 修改：`src/ffi.rs`，让 JSON FFI 的 `is_skill` 与 `skill_name_for_tool` 直接传播引擎查询错误。
- 修改：`src/ffi_standard.rs`，让标准 C ABI 的 `is_skill` 与 `skill_name_for_tool` 直接传播引擎查询错误。
- 修改：`src/runtime/engine/tests.rs`，新增失效 entry registry target 的查询测试。
- 修改：`TASK_LOG.md`，追加本轮任务记录。

### 关键代码调整详情

- `entry_target_visible_to_authority` 新增对 `self.skills.get(&target.skill_storage_key)` 的显式 `ok_or_else`，错误消息同时包含 canonical tool name 与缺失 storage key。
- `is_skill_for_authority` 使用 `match self.entry_registry.get(name)` 明确区分未注册工具与已注册但失效的 target。
- `skill_name_for_tool_for_authority` 保持 DelegatedTool 隐藏 ROOT 的正常语义，但在 target 无法解析所属 skill 时返回同一类一致性错误。
- `luaskills_ffi_is_skill_json`、`luaskills_ffi_skill_name_for_tool_json`、`luaskills_ffi_is_skill`、`luaskills_ffi_skill_name_for_tool` 均改为直接返回引擎查询的 `Result`，由现有 FFI 错误通道输出。
- 新测试先断言 `"missing-tool"` 仍然是成功的 false/None，再插入 `ghost-skill-ping -> missing-storage-key` 的失效 target，并断言两个 authority 查询都会报错。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test authority_entry_queries_reject_stale_registry_targets -- --nocapture` 通过，1 个目标测试通过。
- 回归验证：`cargo test delegated_authority_query_helpers_hide_root_skills -- --nocapture` 通过，ROOT 隐藏语义未变。
- 回归验证：`cargo test runtime::engine -- --nocapture` 通过，165 个 runtime engine 范围测试全部通过。
- 回归验证：`cargo test ffi_standard -- --nocapture` 通过，15 个 ffi_standard 相关测试全部通过。
- 回归验证：`cargo test ffi -- --nocapture` 通过，37 个 FFI 相关测试全部通过。
- 全量验证：`cargo test` 通过，358 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 空白审核：`git diff --check` 通过。
- 搜索复查：`Ok(engine.is_skill_for_authority(...))` 与 `Ok(engine.skill_name_for_tool_for_authority(...))` 已无命中；authority 查询路径不再使用 `unwrap_or(false)` 隐藏失效 target。

### 代码审核与遗留事项

- 本轮没有改变 entry registry 重建算法、canonical name 冲突编号、ROOT 对普通 delegated 查询的隐藏规则、`list_entries_for_authority`、`list_skill_help_for_authority` 或实际 `call_skill` 调用能力。
- 正常未注册工具仍返回 false/None；只有 registry target 指向缺失 loaded skill 时改为显式错误。
- 修改部分代码审核确认没有引入候选 skill 来源、多路兜底、错误吞并、ROOT 可见性泄漏或 FFI ABI 结构变化。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续排查 `build_lua_call_dispatch_entries` 的 `filter_map` 跳过失效 target、`runtime/config` 的 `exists()` 元数据路径、`runtime/engine` 其它布尔降级路径，以及 `host/database` 默认值路径。

## 2026-07-05 第 163 轮：阻止 vulcan.call 分发构建跳过失效入口

### 问题探索

- 基线延续第 162 轮闭环状态：`cargo test` 通过，358 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 已追清执行流：`create_lua_vm` 与 `create_runlua_vm` 在 VM 初始化时调用 `populate_vulcan_call_for_lua`；该函数通过 `build_lua_call_dispatch_entries` 把 `entry_registry` 固化为闭包内的 `dispatch_entries`；运行时 `vulcan.call(name, args)` 再通过 `resolve_lua_call_dispatch_entry` 查找对应入口并进入嵌套 Lua 调用上下文。
- 已确认字段归属：`build_lua_call_dispatch_entries` 的每个 `ResolvedEntryTarget` 必须通过 `target.skill_storage_key` 在 `skills_map` 中找到所属 `LoadedSkill`，再通过 `target.local_name` 在该 skill 的 manifest metadata 中找到真实 entry。
- 旧实现问题：该函数使用 `filter_map`，并在闭包内对 `skills_map.get(...)` 与 `find_tool_by_local_name(...)` 使用 `?`。当 entry registry 中存在失效 target 时，构建阶段会静默丢弃该 target，后续 `vulcan.call` 只会表现为普通的 “Skill not found”，掩盖内部注册表不一致。
- 长期优化判断：`entry_registry` 已经声明某个 canonical entry 存在，若其所属 skill 或 local entry 丢失，这是运行时一致性错误，不是可被过滤掉的正常缺席状态。构建分发表时必须立即失败并给出可定位诊断。

### 执行调整

- 将 `build_lua_call_dispatch_entries` 从返回 `Vec<LuaCallDispatchEntry>` 改为返回 `Result<Vec<LuaCallDispatchEntry>, String>`。
- 去掉 `filter_map` 静默过滤路径，改为显式遍历 `entry_registry.values()` 并逐项校验所属 `LoadedSkill` 与 local entry。
- 当 `target.skill_storage_key` 无法解析到已加载 skill 时，返回包含 canonical name 与缺失 storage key 的 `vulcan.call registry target ...` 错误。
- 当 `target.local_name` 无法在所属 skill metadata 中找到时，返回包含 canonical name、local entry 与 skill id 的 `vulcan.call registry target ...` 错误。
- `populate_vulcan_call_for_lua` 对构建结果使用 `?` 传播错误，让 VM 初始化阶段暴露注册表不一致，而不是把坏 target 藏进后续调用缺失。
- 新增 `vulcan_call_dispatch_build_rejects_stale_registry_targets`，覆盖缺失 skill、缺失 local entry 与有效 target 三条路径。

### 文件变更清单

- 修改：`src/runtime/engine.rs`，收紧 `vulcan.call` 分发入口构建的一致性校验并传播错误。
- 修改：`src/runtime/engine/tests.rs`，新增 `build_lua_call_dispatch_entries` 的失效注册表目标测试，并引入 `BTreeMap` 测试数据。
- 修改：`TASK_LOG.md`，追加本轮任务记录。

### 关键代码调整详情

- `build_lua_call_dispatch_entries` 内部新增 `dispatch_entries` 显式累积变量，保留注册表顺序，同时让每个 target 的解析失败都能中断并返回错误。
- `skills_map.get(&target.skill_storage_key)` 改为 `ok_or_else(...) ?`，错误文本绑定 `target.canonical_name` 与 `target.skill_storage_key`。
- `skill.meta.find_tool_by_local_name(&target.local_name)` 改为 `ok_or_else(...) ?`，错误文本绑定 `target.canonical_name`、`target.local_name` 与 `target.skill_id`。
- `populate_vulcan_call_for_lua` 从直接接收 `Vec` 改为 `build_lua_call_dispatch_entries(skills_map, entry_registry)?`，沿用既有 `Result<(), String>` 初始化错误通道。
- 新测试直接构造最小 `LoadedSkill` 与三组 `BTreeMap<String, ResolvedEntryTarget>`，避免通过更高层加载流程制造间接状态，精确锁定分发表构建行为。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test vulcan_call_dispatch_build_rejects_stale_registry_targets -- --nocapture` 通过，1 个目标测试通过。
- 回归验证：`cargo test runtime::engine -- --nocapture` 通过，166 个 runtime engine 范围测试全部通过。
- 回归验证：`cargo test ffi -- --nocapture` 通过，37 个 FFI 相关测试全部通过。
- 全量验证：`cargo test` 通过，359 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 空白审核：`git diff --check` 通过。
- 搜索复查：`build_lua_call_dispatch_entries` 已无 `filter_map` 静默跳过路径；`populate_vulcan_call_for_lua` 会直接传播构建错误。

### 代码审核与遗留事项

- 本轮没有改变 entry registry 重建算法、canonical name 冲突编号、`vulcan.call` 的名称匹配策略、有效 target 的调用路径、ROOT 可见性规则、FFI ABI 或实际 Lua handler 执行逻辑。
- 正常有效 target 仍能构建一个分发入口；新增行为只影响 registry target 指向缺失 skill 或缺失 local entry 的内部不一致状态。
- 修改部分代码审核确认没有引入候选字段来源、多路兜底、静默默认值、错误吞并、闭包捕获生命周期问题或分发表顺序漂移。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续排查 `runtime/config` 的 `exists()` 元数据路径、`runtime/engine` 其它布尔降级路径、`list_entries`/help schema 日志跳过路径，以及 `host/database` 默认值路径。

## 2026-07-05 第 164 轮：显式暴露技能配置文件路径探测错误

### 问题探索

- 基线延续第 163 轮闭环状态：`cargo test` 通过，359 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 已追清执行流：宿主 FFI、引擎 API 与 Lua `vulcan.config` 最终都会进入 `SkillConfigStore`；读取路径是 `list_entries`/`list_skill_values`/`get_value` -> `with_document_read` -> `read_document_from`，写入与删除路径是 `set_value`/`delete_value` -> `with_document_mut` -> `read_document_from` -> `write_document_to`。
- 已确认字段归属：`file_path()` 返回的是统一技能配置文件路径；当文件确实不存在时，`read_document_from` 应返回空 `SkillConfigDocument`，这是配置尚未写入时的正常业务语义。
- 旧实现问题：`read_document_from` 使用 `if !file_path.exists()` 判断缺失。`Path::exists()` 会把元数据探测错误折叠为 `false`，导致非法路径或不可探测路径被当成“配置文件不存在”，进而返回空配置。
- 复现证据：新增测试先只加入断言，使用包含内嵌 NUL 的显式配置路径读取 `get_value`；旧实现失败为 `invalid config path probe should fail: None`，证明错误被隐藏成了空配置。
- 长期优化判断：配置文件“确认不存在”可以是空配置，但“无法探测路径状态”是文件系统错误，必须显式返回给宿主或 Lua 调用方，不能用默认空文档掩盖。

### 执行调整

- 新增 `skill_config_file_exists`，用 `Path::try_exists()` 区分已存在、确认缺失与探测失败三种状态。
- `read_document_from` 改为调用 `skill_config_file_exists(file_path)?`；只有明确返回 `false` 时才创建默认空文档。
- 探测失败时返回 `failed to inspect skill config file ...`，并沿用宿主可见路径渲染器输出路径。
- Windows `replace_file_atomically` 中目标文件是否存在的判断也从 `exists()` 改为 `try_exists()?`，避免元数据错误被折叠成一次 `rename` 尝试。
- 新增 `skill_config_store_reports_file_path_probe_errors`，覆盖非法配置路径不会被当作缺失文件的读取行为。

### 文件变更清单

- 修改：`src/runtime/config.rs`，收紧技能配置文件存在性探测并补充回归测试。
- 修改：`TASK_LOG.md`，追加本轮任务记录。

### 关键代码调整详情

- `skill_config_file_exists` 作为单一探测边界，返回 `Result<bool, String>`，错误消息绑定具体配置文件路径与底层 IO 错误。
- `read_document_from` 保留“确认缺失文件 -> 空配置”的语义，但不再把探测失败归入缺失文件。
- `replace_file_atomically` 的 Windows 分支新增 `destination_exists` 显式变量，目标路径探测失败时直接返回底层 IO 错误，由上层 promote 错误上下文包装。
- 新测试直接构造 `skill_config\0.json` 路径并调用 `get_value`，锁定 `read_document_from` 的真实读取入口，而不是绕过配置存储调用私有函数。

### 验证记录

- 修复前复现：`cargo test skill_config_store_reports_file_path_probe_errors -- --nocapture` 失败，错误表现为 `invalid config path probe should fail: None`。
- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test skill_config_store_reports_file_path_probe_errors -- --nocapture` 通过，1 个目标测试通过。
- 回归验证：`cargo test runtime::config -- --nocapture` 通过，15 个 runtime config 范围测试全部通过。
- 回归验证：`cargo test skill_config -- --nocapture` 通过，26 个 skill_config 相关测试全部通过。
- 全量验证：`cargo test` 通过，360 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 空白审核：`git diff --check` 通过。
- 搜索复查：`src/runtime/config.rs` 中生产路径不再使用 `file_path.exists()` 或 `destination_path.exists()` 判断配置文件状态；剩余 `exists()` 命中位于测试断言。

### 代码审核与遗留事项

- 本轮没有改变配置文件路径解析、默认 runtime root 捕获、配置 JSON schema、配置键校验、写入序列化格式、进程级锁语义、FFI ABI 或 Lua `vulcan.config` API 形状。
- 确认缺失的配置文件仍会读取为空文档；只有路径探测失败改为显式错误。
- 修改部分代码审核确认没有引入候选路径兜底、多来源字段、静默默认值、锁生命周期问题、临时文件写入顺序变化或配置命名空间语义漂移。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续排查 `runtime/engine` 其它布尔降级路径、`list_entries`/help schema 日志跳过路径、`host/database` 默认值路径，以及 `managed_io` 中默认参数边界。

## 2026-07-05 第 165 轮：入口列表构建不再跳过失效注册表目标

### 问题探索

- 基线延续第 164 轮闭环状态：`cargo test` 通过，360 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 已追清执行流：JSON FFI `luaskills_ffi_list_entries_json` 与标准 C ABI `luaskills_ffi_list_entries` 都调用 `engine.list_entries_for_authority(...)`；debug CLI 在准备工具列表时调用 `engine.list_entries()`；runtime reload 会先用 `list_entries()` 获取旧快照，再在替换状态后通过 `emit_entry_registry_delta` 读取新快照并发出 lifecycle delta。
- 已确认字段归属：入口描述来自 `entry_registry` 的 `ResolvedEntryTarget`，每个 target 必须通过 `skill_storage_key` 解析到 `self.skills` 的 `LoadedSkill`，再通过 `local_name` 找到对应 manifest entry；`input_schema` 来自该 entry 的已解析 schema。
- 旧实现问题：`LuaEngine::list_entries` 使用 `filter_map`，对缺失 skill、缺失 local entry 会直接返回 `None`；对 schema 解析失败只写日志并跳过该 entry。这样对外列表会静默少项，FFI、debug CLI 和 reload delta 都无法知道 runtime registry 已经不一致。
- 长期优化判断：entry registry 是运行时入口事实来源，列表构建阶段发现 target 无法解析或 schema 无法读取时应视为内部一致性错误。继续返回残缺列表会污染宿主侧能力发现和生命周期事件。

### 执行调整

- 将 `LuaEngine::list_entries` 从 `Vec<RuntimeEntryDescriptor>` 改为 `Result<Vec<RuntimeEntryDescriptor>, String>`。
- 去掉 `filter_map` 静默跳过路径，改为显式遍历 `entry_registry.values()`，逐个校验 loaded skill、local entry 与 resolved input schema。
- 将 `list_entries_for_authority` 改为返回 `Result<Vec<RuntimeEntryDescriptor>, String>`，先传播基础列表错误，再执行 ROOT 过滤。
- 将 reload 旧快照读取与 `emit_entry_registry_delta` 新快照读取改为传播 `list_entries` 错误，避免 lifecycle delta 基于残缺列表发出。
- JSON FFI、标准 C ABI 和 debug CLI 调用点改为直接传播新的 `Result`。
- 新增 `list_entries_rejects_stale_registry_targets`，覆盖缺失 skill 与缺失 local entry 两种旧静默跳过路径。

### 文件变更清单

- 修改：`src/runtime/engine.rs`，收紧入口列表构建和 reload delta 快照构建的错误传播。
- 修改：`src/ffi.rs`，让 JSON FFI 列表入口直接传播引擎列表错误。
- 修改：`src/ffi_standard.rs`，让标准 C ABI 列表入口直接传播引擎列表错误。
- 修改：`src/bin/luaskills-debug.rs`，让 debug CLI 准备工具列表时传播引擎列表错误。
- 修改：`src/runtime/engine/tests.rs`，更新新接口断言并新增失效注册表列表测试。
- 修改：`TASK_LOG.md`，追加本轮任务记录。

### 关键代码调整详情

- `list_entries` 新增 `descriptors` 显式累积变量，保持 canonical registry 顺序，同时保证任意 target 解析失败都会中断并返回错误。
- `self.skills.get(&target.skill_storage_key)` 改为 `ok_or_else(...) ?`，错误消息包含 canonical name 和缺失 storage key。
- `skill.meta.find_tool_by_local_name(&target.local_name)` 改为 `ok_or_else(...) ?`，错误消息包含 canonical name、local entry 和 skill id。
- `tool.resolved_input_schema()` 从日志后跳过改为 `map_err(...) ?`，schema 解析失败会阻止残缺列表对外输出。
- `emit_entry_registry_delta` 改为返回 `Result<(), String>`，无差异时返回 `Ok(())`，有差异时发出事件后返回 `Ok(())`。
- `reload_from_roots` 对旧快照和 delta 快照错误都转成 `Box<dyn std::error::Error>` 传播。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test list_entries_rejects_stale_registry_targets -- --nocapture` 通过，1 个目标测试通过。
- 回归验证：`cargo test list_entries_exposes_resolved_entry_input_schema -- --nocapture` 通过。
- 回归验证：`cargo test delegated_authority_query_helpers_hide_root_skills -- --nocapture` 通过。
- 回归验证：`cargo test vulcan_call_dispatch_build_rejects_stale_registry_targets -- --nocapture` 通过。
- 回归验证：`cargo test runtime::engine -- --nocapture` 通过，167 个 runtime engine 范围测试全部通过。
- 回归验证：`cargo test ffi -- --nocapture` 通过，37 个 FFI 相关测试全部通过。
- 回归验证：`cargo test ffi_standard -- --nocapture` 通过，15 个 ffi_standard 相关测试全部通过。
- 全量验证：`cargo test` 通过，361 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 空白审核：`git diff --check` 通过。
- 搜索复查：`LuaEngine::list_entries` 已无 `filter_map` 静默跳过路径；FFI 和 debug CLI 调用点均传播 `Result`。

### 代码审核与遗留事项

- 本轮没有改变 entry registry 重建算法、canonical name 冲突编号、ROOT 过滤规则、descriptor 字段含义、FFI ABI 结构布局、debug CLI 输出格式或实际技能调用逻辑。
- 有效入口列表仍按注册表顺序输出；新增行为只影响 registry target 指向缺失 skill、缺失 local entry 或 schema 不可解析的内部不一致状态。
- 修改部分代码审核确认没有引入候选字段来源、多路兜底、静默默认值、错误吞并、生命周期事件残缺快照或 authority 过滤倒置。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续排查 help 相关 `filter_map` 的 related entries 路径、packaged runtime manifest 的 `exists()` 路径、`host/database` 默认值路径，以及 `managed_io` 中默认参数边界。

## 2026-07-05 第 166 轮：帮助关联入口不再静默丢失

### 问题探索

- 基线延续第 165 轮闭环状态：`cargo test` 通过，361 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 已追清执行流：JSON FFI `luaskills_ffi_list_skill_help_json` 与标准 C ABI `luaskills_ffi_list_skill_help` 调用 `engine.list_skill_help_for_authority(...)`；该函数调用 `list_skill_help()` 并执行 ROOT 过滤。帮助详情入口 `render_skill_help_detail_for_authority(...)` 也会进入 `render_skill_help_detail(...)`，再构建同一个 help node descriptor。
- 已确认字段归属：help node 的 `related_entries` 不是 help 文件自由文本，而是由 manifest entry 的局部名称通过 `LoadedSkill.resolved_entry_names` 映射到 canonical runtime entry 名称；该映射由 `rebuild_entry_registry` 在加载后生成。
- 旧实现问题：`build_help_node_descriptor` 对 main help 和 topic help 都使用 `filter_map` 调用 `resolved_tool_name(...)`。当某个 manifest entry 缺少 canonical 映射时，help 列表和 help 详情都会静默省略该关联入口。
- 长期优化判断：entry 已存在于 manifest，但缺少加载期 canonical 映射，说明 runtime entry registry 生命周期不一致。help 输出残缺的 `related_entries` 会误导宿主侧帮助导航和工具关联展示，必须显式报错。

### 执行调整

- 将 `list_skill_help` 从返回 `Vec<RuntimeSkillHelpDescriptor>` 改为返回 `Result<Vec<RuntimeSkillHelpDescriptor>, String>`。
- 将 `list_skill_help_for_authority` 改为返回 `Result<Vec<RuntimeSkillHelpDescriptor>, String>`，先传播 help 构建错误，再执行 ROOT 过滤。
- 将 `build_help_node_descriptor` 改为返回 `Result<RuntimeHelpNodeDescriptor, String>`。
- 新增 `resolve_help_related_entry_name`，集中把 help 关联的 local entry 解析为 canonical runtime entry；解析不到时返回包含 flow name、skill id 和 local entry 的一致性错误。
- `render_skill_help_detail` 改为对 descriptor 构建使用 `?`，确保 help 详情和 help 列表共享同一错误边界。
- JSON FFI 和标准 C ABI 的 help 列表入口改为直接传播 `list_skill_help_for_authority` 的错误。
- 新增 `list_skill_help_rejects_unresolved_related_entries`，构造一个已加载 skill 但 `resolved_entry_names` 为空的状态，验证 help 列表不会把 `ping` 静默丢掉。

### 文件变更清单

- 修改：`src/runtime/engine.rs`，收紧 help related entries 构建和 help 列表错误传播。
- 修改：`src/ffi.rs`，让 JSON FFI help 列表入口直接传播引擎 help 构建错误。
- 修改：`src/ffi_standard.rs`，让标准 C ABI help 列表入口直接传播引擎 help 构建错误。
- 修改：`src/runtime/engine/tests.rs`，更新 help authority 测试并新增未解析关联入口测试。
- 修改：`TASK_LOG.md`，追加本轮任务记录。

### 关键代码调整详情

- `list_skill_help` 改为显式遍历 `self.skills.values()`，分别构建 main descriptor 和 flow descriptors；任意节点构建失败都会中断并返回错误。
- `build_help_node_descriptor` 去掉 main/topic 两处 `filter_map`，改用 `resolve_help_related_entry_name(...) ?` 填充 `related_entries`。
- `resolve_help_related_entry_name` 只接受 `LoadedSkill`、local entry name 和 flow name 三个事实来源，不引入候选字段或多路径兜底。
- `render_skill_help_detail` 在渲染 help payload 后构建 descriptor，若 related entry 映射缺失则通过既有 `Result<Option<RuntimeHelpDetail>, String>` 通道返回错误。
- FFI help 列表入口的 `with_engine` 闭包从 `Ok(engine.list_skill_help_for_authority(...))` 改为直接返回引擎 `Result`。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test list_skill_help_rejects_unresolved_related_entries -- --nocapture` 通过，1 个目标测试通过。
- 回归验证：`cargo test delegated_authority_query_helpers_hide_root_skills -- --nocapture` 通过。
- 回归验证：`cargo test list_entries_rejects_stale_registry_targets -- --nocapture` 通过。
- 回归验证：`cargo test runtime::engine -- --nocapture` 通过，168 个 runtime engine 范围测试全部通过。
- 回归验证：`cargo test ffi -- --nocapture` 通过，37 个 FFI 相关测试全部通过。
- 回归验证：`cargo test ffi_standard -- --nocapture` 通过，15 个 ffi_standard 相关测试全部通过。
- 全量验证：`cargo test` 通过，362 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 空白审核：`git diff --check` 通过。
- 搜索复查：help related entries 构建路径已无 `filter_map(|entry| ...)` 静默省略；剩余 `filter_map` 命中为 PATHEXT 空条目过滤。

### 代码审核与遗留事项

- 本轮没有改变 help 文件渲染、help topic 查找、ROOT help 隐藏规则、help descriptor 字段结构、FFI ABI 结构布局、帮助详情 content 类型或实际工具调用逻辑。
- 有效 help related entries 仍按 manifest entry 顺序输出；新增行为只影响 manifest entry 缺失 canonical 映射的内部不一致状态。
- 修改部分代码审核确认没有引入候选字段来源、多路兜底、静默默认值、错误吞并、help 详情与列表行为分裂或 authority 过滤倒置。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续排查 packaged runtime manifest 的 `exists()` 路径、`host/database` 默认值路径、`managed_io` 中默认参数边界，以及 `process.which` 可执行探测错误折叠路径。

## 2026-07-05 第 167 轮：打包运行时清单探测错误不再关闭校验

### 问题探索

- 基线延续第 166 轮闭环状态：`cargo test` 通过，362 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 已追清执行流：`load_from_roots` 在写入 runtime root 后调用 `validate_packaged_runtime_resources`；该函数收集 `resources_dir` 并逐个调用 `validate_packaged_runtime_packages_layout`。如果检测到 `lua-runtime-manifest.json`，后续会读取 `luaskills-packages-manifest.json`，再验证 manifest 中声明的各个 packages 文件与目录。
- 已确认字段归属：`resources_dir` 来自 host options 或每个 skill root 的同级 `resources` 目录；`lua-runtime-manifest.json` 是 packaged runtime marker；`luaskills-packages-manifest.json` 与其中的 `paths.*` 是完整性校验来源。
- 旧实现问题：`validate_packaged_runtime_packages_layout` 和 `validate_packaged_runtime_target` 使用 `Path::exists()`。当 marker path、packages manifest path 或 manifest-declared target path 无法被文件系统探测时，`exists()` 会返回 false，导致 marker 探测失败被当作“没有 packaged runtime”，目标探测失败被当作“缺失文件”。
- 长期优化判断：确认不存在可以继续保留原语义；但无法探测路径状态是文件系统错误，不能用“没有 marker”关闭 packaged runtime 校验，也不能把探测失败降级成普通缺失文件。

### 执行调整

- 新增 `packaged_runtime_path_exists`，用 `Path::try_exists()` 区分已存在、确认缺失与探测失败。
- `validate_packaged_runtime_target` 改为通过该 helper 检查 manifest-declared target，探测失败时直接返回带 label 和路径的错误。
- `validate_packaged_runtime_packages_layout` 对 `lua-runtime-manifest.json` marker 和 `luaskills-packages-manifest.json` 也改用该 helper。
- 保留原业务语义：确认没有 `lua-runtime-manifest.json` 时仍返回 `Ok(())`，确认缺少 packages manifest 或声明文件时仍返回原来的 missing/incomplete 类错误。
- 新增 `load_from_roots_rejects_packaged_runtime_marker_probe_errors`，通过包含内嵌 NUL 的 `resources_dir` 走真实 `load_from_roots`，验证 marker 探测失败不会被当成 marker 缺失。

### 文件变更清单

- 修改：`src/runtime/engine.rs`，收紧 packaged runtime 清单和目标路径存在性探测。
- 修改：`src/runtime/engine/tests.rs`，新增 packaged runtime marker 探测失败测试。
- 修改：`TASK_LOG.md`，追加本轮任务记录。

### 关键代码调整详情

- `packaged_runtime_path_exists` 作为统一探测边界，返回 `Result<bool, String>`，错误文本包含 manifest label、宿主可见路径和底层 IO 错误。
- `validate_packaged_runtime_target` 保持相对路径校验不变，只把 `candidate.exists()` 替换为 `packaged_runtime_path_exists(&candidate, label)?`。
- `validate_packaged_runtime_packages_layout` 的 marker 检查仍允许确认缺失时跳过 packaged runtime 校验，但不再允许探测失败时跳过。
- 新测试使用有效 skill root 和非法 `resources\0invalid` 路径，确保失败只来自 packaged runtime marker 探测，而不是技能加载路径。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test load_from_roots_rejects_packaged_runtime_marker_probe_errors -- --nocapture` 通过，1 个目标测试通过。
- 回归验证：`cargo test packaged_runtime -- --nocapture` 通过，4 个 packaged runtime 相关测试全部通过。
- 回归验证：`cargo test runtime::engine -- --nocapture` 通过，169 个 runtime engine 范围测试全部通过。
- 全量验证：`cargo test` 通过，363 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 空白审核：`git diff --check` 通过。
- 搜索复查：packaged runtime marker、packages manifest 和 manifest-declared target 检查均已通过 `packaged_runtime_path_exists`；剩余 `exists()` 命中属于其它待排查路径或测试清理断言。

### 代码审核与遗留事项

- 本轮没有改变 packaged runtime manifest schema、layout 值、路径相对性校验、packages 文件读取解析、host options 解析、skill root 加载顺序或技能实际加载逻辑。
- 确认不存在 marker 仍会跳过 packaged runtime 校验；确认缺失 packages manifest 或声明文件仍使用原有缺失错误。
- 修改部分代码审核确认没有引入候选路径兜底、多来源字段、静默默认值、错误吞并、manifest 解析顺序变化或普通非 packaged runtime 加载回归。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续排查 `host/database` 默认值路径、`managed_io` 默认参数边界、`process.which` 可执行探测错误折叠路径，以及 `fs.copy` 祖先路径 `exists()` 循环。

## 2026-07-05 第 168 轮：process.which 候选探测错误不再伪装成未命中

### 问题探索

- 基线延续第 167 轮闭环状态：`cargo test` 通过，363 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 已追清执行流：Lua `vulcan.process.which` 会解析 program 字符串后调用 `resolve_vulcan_process_which`；runlua shell launcher 解析也复用该函数。该函数对显式路径直接调用 `find_vulcan_process_candidate`，对命令名则遍历 PATH 后对每个候选调用同一个 helper。
- 已确认字段归属：候选路径由 `resolve_vulcan_process_search_path` 和 `vulcan_process_candidate_paths` 生成；是否可执行由 `is_vulcan_process_executable` 读取 filesystem metadata 后判断。
- 旧实现问题：`is_vulcan_process_executable` 对 `fs::metadata(path)` 使用 `unwrap_or(false)`。这会把所有 metadata 错误，包括非法路径、权限/探测失败等，全部当作“候选不可执行/未命中”。
- 事实修正：公开 Lua 入参层 `require_string_arg` 会先拒绝 NUL 字符，因此 NUL 不能作为 Lua 端到端触发 metadata 的用例；真实风险边界在 `find_vulcan_process_candidate` 这个 `process.which` 与 shell launcher 解析共用的核心 helper。
- 长期优化判断：`NotFound` 是 PATH 查找的正常未命中语义，应继续返回 false；其它 metadata 错误说明宿主文件系统状态不可判断，必须显式返回错误，不能伪装成命令不存在。

### 执行调整

- 将各平台的 `is_vulcan_process_executable` 从返回 `bool` 改为返回 `Result<bool, String>`。
- 对 `fs::metadata` 的 `ErrorKind::NotFound` 保持 `Ok(false)`，保留普通 PATH 未命中行为。
- 对非 `NotFound` metadata 错误返回 `process.which: failed to inspect executable candidate ...`，包含宿主可见候选路径和底层错误。
- 将 `find_vulcan_process_candidate` 从 iterator `find` 改为显式循环，以便对每个候选传播 `Result`。
- 新增 `vulcan_process_candidate_lookup_reports_metadata_probe_errors`，直接构造包含内嵌 NUL 的候选路径，验证核心候选解析不会把探测失败当未命中。

### 文件变更清单

- 修改：`src/runtime/engine.rs`，收紧 `process.which` 候选可执行探测错误传播。
- 修改：`src/runtime/engine/tests.rs`，新增候选 metadata 探测失败测试。
- 修改：`TASK_LOG.md`，追加本轮任务记录。

### 关键代码调整详情

- Unix 分支保留 `metadata.is_file() && executable bit` 判断，只将 metadata 获取失败拆分为 `NotFound -> Ok(false)` 和其它错误 -> `Err(...)`。
- Windows 与其它平台分支保留 `metadata.is_file()` 判断，并使用同样的错误拆分策略。
- `find_vulcan_process_candidate` 显式遍历 `vulcan_process_candidate_paths(base)?`，命中时返回 `Ok(Some(candidate))`，全部未命中时返回 `Ok(None)`，探测错误时立即返回 `Err`。
- 新测试没有绕过事实链路去伪造 Lua 输入；它直接锁定 `resolve_vulcan_process_which` 下游共用的候选解析边界，避免与 Lua 字符串 NUL 校验混淆。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test vulcan_process_candidate_lookup_reports_metadata_probe_errors -- --nocapture` 通过，1 个目标测试通过。
- 回归验证：`cargo test process_which -- --nocapture` 通过，2 个公开 `process.which` 相关测试全部通过。
- 回归验证：`cargo test runtime::engine -- --nocapture` 通过，170 个 runtime engine 范围测试全部通过。
- 全量验证：`cargo test` 通过，364 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 空白审核：`git diff --check` 通过。
- 搜索复查：`is_vulcan_process_executable` 已无 `unwrap_or(false)` 错误折叠；剩余 `unwrap_or(false)` 命中位于其它待排查路径或明确布尔解析语义。

### 代码审核与遗留事项

- 本轮没有改变 PATHEXT 扩展规则、PATH 遍历顺序、显式路径识别、host-visible 路径渲染、Lua `process.which` 返回值形状、runlua shell launcher 名称或可执行位判断规则。
- 普通不存在候选仍返回未命中；只有候选 metadata 探测失败改为显式错误。
- 修改部分代码审核确认没有引入候选路径兜底、多来源字段、静默默认值、错误吞并、PATH 搜索顺序变化或平台分支行为倒置。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续排查 `fs.copy` 祖先路径 `exists()` 循环、`host/database` 默认值路径、`managed_io` 默认参数边界，以及 runlua shell launcher availability 中的错误吞并。

## 2026-07-05 第 169 轮：fs.copy 目标祖先探测错误不再继续上溯

### 问题探索

- 基线延续第 168 轮闭环状态：`cargo test` 通过，364 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 已追清执行流：Lua `vulcan.fs.copy` 在复制目录时会调用 `validate_vulcan_fs_copy_directory_target`，该函数先 canonicalize 源目录，再调用 `resolve_vulcan_fs_copy_effective_destination_path` 解析目标的实际位置，防止目标位于源目录内部。
- 已确认字段归属：`resolve_vulcan_fs_copy_effective_destination_path` 会把目标路径转成绝对路径，然后沿目标或目标父级向上寻找第一个已存在祖先，canonicalize 该祖先后再把缺失后缀拼回去。
- 旧实现问题：上溯循环使用 `while !cursor.exists()`。`Path::exists()` 会把文件系统探测错误折叠成 false，导致非法父级或无法探测父级被当作“缺失父级”，继续向上走到更高祖先，最终可能返回一个看似有效的目标位置。
- 长期优化判断：`NotFound` 是正常“继续上溯”的语义；但无法探测父级状态是文件系统错误，不能被当成缺失父级处理，否则目录复制的自嵌套保护会基于错误目标位置做判断。

### 执行调整

- `resolve_vulcan_fs_copy_effective_destination_path` 的祖先上溯循环改为显式 `loop`。
- 使用已有 `path_entry_exists(cursor, ...)` 替代 `cursor.exists()`，只把 `ErrorKind::NotFound` 当作缺失祖先继续上溯。
- 对非 `NotFound` 的祖先探测错误返回 `fs.copy: failed to inspect destination ancestor ...`，包含当前祖先路径和底层错误。
- 新增 `vulcan_fs_copy_effective_destination_reports_ancestor_probe_errors`，直接构造父级包含内嵌 NUL 的目标路径，验证核心目标解析不会把探测失败当作缺失父级。

### 文件变更清单

- 修改：`src/runtime/engine.rs`，收紧 `fs.copy` 目标祖先路径探测错误传播。
- 修改：`src/runtime/engine/tests.rs`，新增 `fs.copy` 目标祖先探测失败测试。
- 修改：`TASK_LOG.md`，追加本轮任务记录。

### 关键代码调整详情

- `resolve_vulcan_fs_copy_effective_destination_path` 中新增 `ancestor_exists` 显式变量，用于区分已存在、确认缺失和探测错误。
- 正常缺失祖先仍保留原逻辑：记录 `missing_name`、推进到父目录、最终 canonicalize 第一个存在祖先并拼回后缀。
- 探测错误不再进入 `missing_name` 分支，而是直接通过 `path_entry_exists` 的 `Result` 返回。
- 新测试直接锁定核心解析 helper；公开 Lua path 参数会先拒绝 NUL，因此不使用端到端 Lua 调用制造伪证据。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test vulcan_fs_copy_effective_destination_reports_ancestor_probe_errors -- --nocapture` 通过，1 个目标测试通过。
- 回归验证：`cargo test vulcan_fs_copy -- --nocapture` 通过，7 个 fs.copy 相关测试全部通过。
- 回归验证：`cargo test runtime::engine -- --nocapture` 通过，171 个 runtime engine 范围测试全部通过。
- 全量验证：`cargo test` 通过，365 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 空白审核：`git diff --check` 通过。
- 搜索复查：`resolve_vulcan_fs_copy_effective_destination_path` 已无 `cursor.exists()`；祖先路径探测统一走 `path_entry_exists`。

### 代码审核与遗留事项

- 本轮没有改变 `fs.copy` overwrite 语义、文件复制、目录递归复制、符号链接拒绝策略、目标自嵌套判断、host-visible 路径渲染或 Lua API 返回值形状。
- 正常缺失父级仍会继续上溯并在后续创建目标路径；只有祖先路径探测失败改为显式错误。
- 修改部分代码审核确认没有引入候选路径兜底、多来源字段、静默默认值、错误吞并、目录复制判断顺序变化或符号链接处理回归。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续排查 `host/database` 默认值路径、`managed_io` 默认参数边界、runlua shell launcher availability 中的错误吞并，以及 Lua-facing `fs.exists`/`fs.is_dir` 的探测错误折叠。

## 2026-07-05 第 170 轮：fs.exists 与 fs.is_dir 探测错误不再返回 false

### 问题探索

- 基线延续第 169 轮闭环状态：`cargo test` 通过，365 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 已追清执行流：Lua-facing `vulcan.fs.exists(path)` 和 `vulcan.fs.is_dir(path)` 都先通过 `require_path_arg` 校验路径字符串，再分别调用 `Path::exists()` 与 `Path::is_dir()` 返回布尔值。
- 已确认公开语义：文档声明 `fs.exists` 判断路径是否存在，`fs.is_dir` 判断路径是否为目录；既有测试依赖缺失目标返回 false，但没有覆盖元数据探测失败。
- 旧实现问题：`Path::exists()` 与 `Path::is_dir()` 都会把底层 metadata 探测错误折叠成 false。非法路径、权限或其它探测失败会被伪装成“路径不存在/不是目录”。
- 事实修正：Lua 入参层会拒绝 NUL 字符，不能用公开 Lua 调用构造 NUL 路径直达 metadata；真实风险边界在两个布尔 helper 的 metadata 读取逻辑。
- 长期优化判断：`NotFound` 是正常缺失目标语义，应继续返回 false；其它 metadata 错误说明文件系统状态不可判断，应显式返回错误，不能伪装成普通 false。

### 执行调整

- 新增 `vulcan_fs_target_exists`，使用 `fs::metadata` 检查目标是否存在，并区分 `NotFound` 与其它错误。
- 新增 `vulcan_fs_target_is_dir`，使用 `fs::metadata` 检查目标是否为目录，并区分 `NotFound` 与其它错误。
- `vulcan.fs.exists` 改为调用 `vulcan_fs_target_exists(..., "fs.exists")`，探测失败通过 Lua runtime error 返回。
- `vulcan.fs.is_dir` 改为调用 `vulcan_fs_target_is_dir(..., "fs.is_dir")`，探测失败通过 Lua runtime error 返回。
- 新增 `vulcan_fs_target_boolean_helpers_report_probe_errors`，直接构造包含内嵌 NUL 的路径，验证 exists/is_dir 两条 helper 不会把探测失败折成 false。

### 文件变更清单

- 修改：`src/runtime/engine.rs`，收紧 `fs.exists` 与 `fs.is_dir` 的 metadata 探测错误传播。
- 修改：`src/runtime/engine/tests.rs`，新增布尔 filesystem helper 探测失败测试。
- 修改：`TASK_LOG.md`，追加本轮任务记录。

### 关键代码调整详情

- `vulcan_fs_target_exists` 保留目标 metadata 存在性语义：metadata 成功为 true，`NotFound` 为 false，其它错误为 `Err`。
- `vulcan_fs_target_is_dir` 保留目标 metadata 目录语义：metadata 成功时返回 `metadata.is_dir()`，`NotFound` 为 false，其它错误为 `Err`。
- Lua 闭包不再直接调用 `Path::exists()` 或 `Path::is_dir()`，而是把 helper 错误映射为 `mlua::Error::runtime`。
- 新测试直接覆盖 helper 边界，避免与 `require_path_arg` 的 NUL 校验事实冲突。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test vulcan_fs_target_boolean_helpers_report_probe_errors -- --nocapture` 通过，1 个目标测试通过。
- 回归验证：`cargo test vulcan_fs -- --nocapture` 通过，16 个 vulcan_fs 相关测试全部通过。
- 回归验证：`cargo test runtime::engine -- --nocapture` 通过，172 个 runtime engine 范围测试全部通过。
- 全量验证：`cargo test` 通过，366 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 空白审核：`git diff --check` 通过。
- 搜索复查：Lua-facing `fs.exists` 与 `fs.is_dir` 已无直接 `Path::exists()`/`Path::is_dir()` 错误折叠路径。

### 代码审核与遗留事项

- 本轮没有改变 `fs.stat`、`fs.copy`、`fs.remove`、`fs.mkdir`、path 参数校验、host-visible 路径渲染、文档声明的成功返回类型或普通缺失目标行为。
- 缺失目标仍返回 false；非目录目标仍在 `fs.is_dir` 返回 false；只有 metadata 探测失败改为显式错误。
- 修改部分代码审核确认没有引入候选路径兜底、多来源字段、静默默认值、错误吞并、符号链接语义重写或 Lua API 形状漂移。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续排查 `host/database` 默认值路径、`managed_io` 默认参数边界、runlua shell launcher availability 中的错误吞并，以及 `host_provided_ffi_root.is_dir()` 的探测错误折叠。

## 2026-07-05 第 171 轮：host_provided_ffi_root 目录探测错误不再静默跳过

### 问题探索

- 基线延续第 170 轮闭环状态：`cargo test` 通过，366 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 已追清执行流：`LuaEngine::new` 会规范化 `LuaRuntimeHostOptions`，随后构造 `NativeLibrarySearchGuard::new(&host_options)`；reload 候选也会复用同一个守卫构造路径。
- Windows 平台下，`NativeLibrarySearchGuard::new_windows` 读取 `host_options.host_provided_ffi_root`，旧逻辑直接调用 `Path::is_dir()` 判断是否注册 DLL 搜索目录。
- 旧实现问题：`Path::is_dir()` 会把底层 metadata 探测错误折叠成 false。宿主传入非法路径、权限异常或其它无法探测的 FFI 根目录时，运行时会像“目录不存在/不是目录”一样静默跳过注册。
- 已确认该路径不是 Lua 入参路径，而是宿主配置初始化路径；因此应在 `NativeLibrarySearchGuard` 内部修正，不应通过 Lua API 或依赖管理层做候选式兜底。
- 长期优化判断：缺失目录和已存在但非目录仍可保持“不注册目录”的语义；metadata 探测失败代表宿主配置不可判断，必须显式失败，避免后续原生库加载以更隐蔽的方式出错。

### 执行调整

- 新增 Windows 专用 `host_provided_ffi_root_is_directory`，用 `fs::metadata` 判断宿主 FFI 根目录是否为目录。
- `host_provided_ffi_root_is_directory` 仅将 `ErrorKind::NotFound` 视为 false，其它 metadata 错误返回 `failed to inspect host_provided_ffi_root ...`。
- `NativeLibrarySearchGuard::new_windows` 改为先调用该 helper，再决定是否执行 `SetDefaultDllDirectories` 和 `AddDllDirectory`。
- 新增 `native_library_search_guard_rejects_host_ffi_root_probe_errors`，用内嵌 NUL 路径验证守卫初始化不会把探测失败吞成普通跳过。

### 文件变更清单

- 修改：`src/runtime/engine.rs`，收紧 Windows 原生库搜索守卫的宿主 FFI 根目录探测错误传播。
- 修改：`src/runtime/engine/tests.rs`，新增 Windows 专属初始化边界测试。
- 修改：`TASK_LOG.md`，追加本轮任务记录。

### 关键代码调整详情

- `NativeLibrarySearchGuard::new_windows` 中新增 `ffi_root_is_directory` 明确变量，先完成可失败的目录探测，再决定是否注册 DLL 搜索目录。
- `host_provided_ffi_root_is_directory` 保留原有正常语义：目录返回 true，缺失或非目录返回 false。
- 对非 `NotFound` 的 metadata 错误不再静默跳过，而是返回包含 `host_provided_ffi_root` 和宿主可见路径的显式错误。
- 测试直接调用 `NativeLibrarySearchGuard::new`，覆盖 `LuaEngine::new` 与 reload 共享的真实守卫入口，避免从不相关层级制造伪证据。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test native_library_search_guard_rejects_host_ffi_root_probe_errors -- --nocapture` 通过，1 个目标测试通过。
- 回归验证：`cargo test runtime::engine -- --nocapture` 通过，173 个 runtime engine 范围测试全部通过。
- 全量验证：`cargo test` 通过，367 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 空白审核：`git diff --check` 通过。
- 搜索复查：`host_provided_ffi_root.is_dir()` 已不存在；守卫路径统一走 `host_provided_ffi_root_is_directory`。

### 代码审核与遗留事项

- 本轮没有改变非 Windows 平台行为、runtime root 派生规则、FFI 字段解析、依赖管理的 host FFI root 创建逻辑、`windows_wide_null_path` 转换语义或普通缺失目录的跳过行为。
- 缺失 FFI 根目录仍跳过注册；已存在但非目录仍跳过注册；只有无法完成 metadata 探测的配置改为显式错误。
- 修改部分代码审核确认没有引入候选路径兜底、多来源字段、静默默认值、全局 PATH 修改、DLL 搜索目录注册顺序变化或 reload 入口漂移。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续排查 `host/database` 默认值路径、`managed_io` 默认参数边界、runlua shell launcher availability 中的错误吞并，以及 `LuaEngine` 加载路径内剩余 `.exists()` 的探测错误折叠。

## 2026-07-05 第 172 轮：skill root 探测错误不再被当作空根目录

### 问题探索

- 基线延续第 171 轮闭环状态：`cargo test` 通过，367 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 已追清执行流：`LuaEngine::load_from_roots` 先校验 ROOT/PROJECT/USER 链，再刷新默认 skill config runtime root、校验打包运行时 resources，随后用 `skill_roots.iter().all(|root| !root.skills_dir.exists())` 判断是否直接返回。
- 旧引擎早退问题：`Path::exists()` 会把 metadata 探测错误折叠成 false。单个非法 `skills_dir` 会被当成“所有 root 都不存在”，最终 `load_from_roots` 返回 `Ok(())`，宿主会误以为加载成功。
- 已继续追到真实收集器：`collect_effective_skill_instances_from_roots` 调用 `collect_named_skill_dirs`，而 `collect_named_skill_dirs` 内部也先用 `root.exists()` 判断空根。
- 旧收集器问题：当 root 链中既有有效 root 又有非法 root 时，引擎早退不会触发，但 `collect_named_skill_dirs` 仍会把非法 root 当空目录，导致该层 skill 被静默忽略。
- 文档边界确认：FFI 集成文档明确 `skills_dir` 必须是目录；普通缺失 root 可以保持空根语义，但探测失败不是确认缺失，必须显式失败。

### 执行调整

- 在 `LuaEngine` 中新增 `runtime_skill_root_dir_exists`，用 `try_exists()` 检查单个 root 的 `skills_dir`，保留缺失为 false，探测失败为错误。
- 在 `LuaEngine` 中新增 `any_runtime_skill_root_dir_exists`，替代旧的 `skill_roots.iter().all(...exists())` 早退判断。
- 在 skill manager 中新增 `skill_root_path_exists`，让 `collect_named_skill_dirs` 在目录遍历前使用可失败探测。
- 新增 `load_from_roots_rejects_skill_root_probe_errors`，验证非法 root 不再被 `load_from_roots` 当成空 root 链。
- 新增 `collect_effective_skill_instances_rejects_skill_root_probe_errors`，验证 skill manager 收集器不会把非法 root 当空目录。

### 文件变更清单

- 修改：`src/runtime/engine.rs`，收紧 `load_from_roots` root 链早退判断的文件系统探测错误传播。
- 修改：`src/skill/manager.rs`，收紧 `collect_named_skill_dirs` 对 root 目录存在性的探测错误传播。
- 修改：`src/runtime/engine/tests.rs`，新增引擎加载非法 skill root 的回归测试。
- 修改：`src/skill/manager/tests.rs`，新增 skill manager 收集非法 skill root 的回归测试。
- 修改：`TASK_LOG.md`，追加本轮任务记录。

### 关键代码调整详情

- `runtime_skill_root_dir_exists` 错误信息包含 root 名称、宿主可见路径和底层错误，避免只报告“没有 skill”这类不可定位结果。
- `any_runtime_skill_root_dir_exists` 按 root 链顺序逐个探测，只在所有 root 都确认缺失时才允许 `load_from_roots` 保持原有早退行为。
- `skill_root_path_exists` 被接入 `collect_named_skill_dirs`，覆盖 `collect_effective_skill_instances_from_roots`、`resolve_effective_skill_instance_from_roots` 和 `resolve_declared_skill_instance_from_roots` 的共享读取路径。
- 两条测试都用内嵌 NUL 路径制造真实 metadata 探测失败；引擎测试显式提供有效缺失的 `resources_dir`，避免打包运行时 marker 探测抢先失败。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test load_from_roots_rejects_skill_root_probe_errors -- --nocapture` 通过，1 个目标测试通过。
- 修改后：`cargo test collect_effective_skill_instances_rejects_skill_root_probe_errors -- --nocapture` 通过，1 个目标测试通过。
- 回归验证：`cargo test runtime::engine -- --nocapture` 通过，174 个 runtime engine 范围测试全部通过。
- 回归验证：`cargo test skill::manager -- --nocapture` 通过，19 个 skill manager 范围测试全部通过。
- 全量验证：`cargo test` 通过，369 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 空白审核：`git diff --check` 通过。
- 搜索复查：`skill_roots.iter().all(|root| !root.skills_dir.exists())` 已不存在；`collect_named_skill_dirs` 已不再调用 `root.exists()`。

### 代码审核与遗留事项

- 本轮没有改变 ROOT/PROJECT/USER 顺序校验、缺失 root 作为空目录的既有语义、skill config 默认 root 推导、打包运行时 resources 校验、单个 skill manifest 解析或管理面权限规则。
- 确认缺失的 root 仍会被当作空根；只有无法完成 metadata 探测的 root 会显式报错。
- 修改部分代码审核确认没有引入候选路径兜底、多来源字段、静默默认值、root 链重排、额外目录创建或 skill 加载优先级变化。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续排查 `ensure_skill_dependencies`、`load_skill_dependency_manifest`、`load_single_skill` 和 runlua package path 构造中的剩余 `.exists()` 探测错误折叠。

## 2026-07-05 第 173 轮：dependencies.yaml 探测错误不再被当作无依赖清单

### 问题探索

- 基线延续第 172 轮闭环状态：`cargo test` 通过，369 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 已追清加载执行流：`LuaEngine::load_from_roots` 解析出有效 skill 后先调用 `ensure_skill_dependencies`，该函数旧逻辑通过 `skill_dir.join("dependencies.yaml").exists()` 判断是否需要加载依赖清单。
- 旧加载问题：`Path::exists()` 会把 metadata 探测错误折叠成 false。依赖清单路径无法探测时，运行时会把它当作“没有 dependencies.yaml”，继续加载 skill，导致依赖可能没有准备就绪。
- 已追清管理执行流：卸载与更新清理路径需要在变更前读取旧的 `dependencies.yaml`，用于后续清理已卸载或已更新 skill 的依赖。
- 旧管理问题：卸载路径也存在手写 `dependencies_path.exists()` 分支，探测失败会被当成没有旧依赖清单，导致清理输入缺失。
- 长期优化判断：`dependencies.yaml` 是可选文件，确认缺失应继续返回 `None`；但探测失败不是缺失，必须显式报错并中止相关依赖准备或清理流程。

### 执行调整

- 新增 `skill_dependency_manifest_path_exists`，用 `try_exists()` 探测单个可选 `dependencies.yaml`，只把确认缺失当作 false。
- `load_skill_dependency_manifest` 改为调用 `skill_dependency_manifest_path_exists`，把探测失败作为 `Err` 返回。
- `ensure_skill_dependencies` 改为复用 `load_skill_dependency_manifest`，避免加载路径保留独立的存在性判断。
- `mutate_skill_state_and_reload` 和显式卸载路径都改为复用 `load_skill_dependency_manifest` 捕获旧依赖清单。
- 新增 `load_skill_dependency_manifest_reports_probe_errors` 和 `ensure_skill_dependencies_reports_manifest_probe_errors`，分别覆盖可选清单读取与加载前依赖准备两个入口。

### 文件变更清单

- 修改：`src/runtime/engine.rs`，统一 `dependencies.yaml` 可选清单探测和读取路径。
- 修改：`src/runtime/engine/tests.rs`，新增依赖清单探测失败的加载器与依赖准备测试。
- 修改：`TASK_LOG.md`，追加本轮任务记录。

### 关键代码调整详情

- `skill_dependency_manifest_path_exists` 的错误信息包含依赖清单路径和底层错误，避免把不可探测路径伪装成缺失清单。
- `ensure_skill_dependencies` 现在通过 `let Some(manifest) = self.load_skill_dependency_manifest(skill_dir)? else { ... }` 区分缺失、空清单和探测失败。
- 卸载前旧清单读取不再手写 `dependencies_path.exists()`，而是与更新清理共享同一个 `load_skill_dependency_manifest` 边界。
- 两条测试直接构造包含内嵌 NUL 的 skill 目录，让派生出的 `dependencies.yaml` 触发真实 metadata 探测失败。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test load_skill_dependency_manifest_reports_probe_errors -- --nocapture` 通过，1 个目标测试通过。
- 修改后：`cargo test ensure_skill_dependencies_reports_manifest_probe_errors -- --nocapture` 通过，1 个目标测试通过。
- 回归验证：`cargo test runtime::engine -- --nocapture` 通过，176 个 runtime engine 范围测试全部通过。
- 全量验证：`cargo test` 通过，371 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 空白审核：`git diff --check` 通过。
- 搜索复查：`dependencies_path.exists()` 已不存在；`dependencies.yaml` 可选清单路径统一经过 `skill_dependency_manifest_path_exists`。

### 代码审核与遗留事项

- 本轮没有改变 `dependencies.yaml` 作为可选文件的语义、空清单跳过依赖安装的语义、依赖管理器安装逻辑、生命周期权限校验、reload 流程或 skill entry 解析。
- 确认缺失的 `dependencies.yaml` 仍返回 `None`；清单存在但为空仍不触发依赖安装；只有无法完成 metadata 探测的路径改为显式错误。
- 修改部分代码审核确认没有引入候选路径兜底、多来源字段、静默默认值、额外目录创建、依赖清理顺序变化或生命周期事件状态漂移。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续排查 `load_single_skill` 中 `skill.yaml` 与 Lua entry 的 `.exists()` 探测错误折叠，以及 runlua package path 构造中的剩余 `.exists()`。

## 2026-07-05 第 174 轮：load_single_skill 必需文件探测错误不再伪装成缺失

### 问题探索

- 基线延续第 173 轮闭环状态：`cargo test` 通过，371 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 已追清加载执行流：`LuaEngine::load_from_roots` 在依赖准备完成后调用 `load_single_skill`；`load_single_skill` 先检查 `skill.yaml`，解析 manifest 后再逐个检查 `tool.lua_entry` 派生出的 Lua 入口文件。
- 旧 `skill.yaml` 问题：`load_single_skill` 用 `skill_yaml.exists()` 判断清单是否存在。非法路径、权限异常或其它 metadata 探测失败会被折叠成 false，并返回“skill.yaml not found”。
- 旧 Lua entry 问题：每个 entry 通过 `tool_entry_path(dir, tool)` 派生 `lua_path` 后再调用 `lua_path.exists()`。探测失败同样会被折叠成“Lua entry not found”。
- 已确认 `validate_skill_relative_path` 只校验路径相对性、前缀和穿越，不负责文件系统 metadata 探测；因此修正必须落在 `load_single_skill` 的文件存在性边界。
- 长期优化判断：必需文件确认缺失时应保留原有“not found”错误；但文件系统不可探测不是缺失，必须显式报错，避免宿主按缺文件方向排查。

### 执行调整

- 新增 `required_skill_file_path_exists`，用 `try_exists()` 探测必需 skill 文件，区分确认缺失和探测失败。
- `load_single_skill` 的 `skill.yaml` 判断改为调用 `required_skill_file_path_exists(&skill_yaml, "skill.yaml", dir)`。
- `load_single_skill` 的 Lua entry 判断改为先构造 `Lua entry {tool.lua_entry}` 标签，再通过同一个 helper 探测 `lua_path`。
- 新增 `load_single_skill_reports_skill_yaml_probe_errors`，验证非法 skill 目录派生出的 `skill.yaml` 探测失败不会返回缺失清单错误。
- 新增 `load_single_skill_reports_lua_entry_probe_errors`，验证 manifest 中 Lua entry 路径包含内嵌 NUL 时会在入口探测处显式失败。

### 文件变更清单

- 修改：`src/runtime/engine.rs`，收紧 `load_single_skill` 必需文件存在性探测错误传播。
- 修改：`src/runtime/engine/tests.rs`，新增 `skill.yaml` 与 Lua entry 探测失败测试。
- 修改：`TASK_LOG.md`，追加本轮任务记录。

### 关键代码调整详情

- `required_skill_file_path_exists` 的错误信息包含文件标签、具体文件路径、所属 skill 目录和底层错误，便于区分“缺失文件”和“路径不可探测”。
- `skill_yaml_exists` 只在 helper 返回 false 时沿用原有 `skill.yaml not found in ...` 错误；helper 返回 Err 时直接中止加载。
- `lua_entry_exists` 只在 helper 返回 false 时沿用原有 `Lua entry ... not found in ...` 错误；helper 返回 Err 时直接中止加载。
- Lua entry 测试通过 YAML 转义 `\0` 让 manifest 解析成功，再让派生出的 Lua 路径在真实 metadata 探测处失败。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test load_single_skill_reports_skill_yaml_probe_errors -- --nocapture` 通过，1 个目标测试通过。
- 修改后：`cargo test load_single_skill_reports_lua_entry_probe_errors -- --nocapture` 通过，1 个目标测试通过。
- 回归验证：`cargo test runtime::engine -- --nocapture` 通过，178 个 runtime engine 范围测试全部通过。
- 全量验证：`cargo test` 通过，373 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 空白审核：`git diff --check` 通过。
- 搜索复查：`skill_yaml.exists()` 与 `lua_path.exists()` 已不在 `src/runtime/engine.rs` 中出现；`load_single_skill` 必需文件探测统一经过 `required_skill_file_path_exists`。

### 代码审核与遗留事项

- 本轮没有改变 manifest 解析规则、`skill_id` 禁止声明规则、entry 名称校验、Lua module 校验、help 路径校验、实际 Lua 编译流程或缺失必需文件的错误文案。
- 确认缺失的 `skill.yaml` 与 Lua entry 仍返回原有 not found 错误；只有无法完成 metadata 探测的路径改为显式错误。
- 修改部分代码审核确认没有引入候选路径兜底、多来源字段、静默默认值、路径前缀放宽、额外目录创建或 entry 加载顺序变化。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续排查 `src/skill/manager.rs` 中独立的 `skill_yaml.exists()`、runlua package path 构造中的剩余 `.exists()`，以及加载链其它必需文件读取边界。

## 2026-07-05 第 175 轮：skill manager 启用探针不再把 manifest 探测失败当默认启用

### 问题探索

- 基线延续第 174 轮闭环状态：`cargo test` 通过，373 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 已追清执行流：`collect_effective_skill_instances_from_roots` 先通过 `collect_named_skill_dirs` 枚举各层 skill 目录，再调用 `is_effective_disable_override` 与 `is_skill_manifest_enabled` 判断该目录是否生效。
- 旧逻辑问题：`is_skill_manifest_enabled` 使用 `skill_yaml.exists()` 判断 `skill.yaml` 是否存在。metadata 探测失败会被折叠成 false，然后返回 `Ok(true)`，等同于“清单缺失，默认启用”。
- 风险边界确认：该函数影响 ROOT/PROJECT/USER 覆盖链的生效解析，也影响按 skill id 解析生效实例的管理路径；一旦 manifest 路径不可探测，旧行为会把不可判断状态误当启用。
- 已确认完整 manifest 读取函数 `read_skill_manifest_from_directory` 直接 `read_to_string`，不会在读取前用 `exists()` 吞掉探测错误；本轮问题集中在启用状态轻量探针。
- 长期优化判断：`skill.yaml` 确认缺失时仍保留默认启用语义；但探测失败不是缺失，必须显式失败，避免误启用不可判断的 skill。

### 执行调整

- 新增 `skill_manifest_path_exists`，使用 `try_exists()` 探测单个 `skill.yaml`，只把确认缺失当作 false。
- `is_skill_manifest_enabled` 改为调用 `skill_manifest_path_exists(&skill_yaml)?`，探测失败时直接返回错误。
- 新增 `is_skill_manifest_enabled_defaults_missing_manifest_to_enabled`，锁住缺失清单仍默认启用的原有语义。
- 新增 `is_skill_manifest_enabled_rejects_manifest_probe_errors`，用内嵌 NUL skill 目录验证 manifest 探测失败不会被当作默认启用。

### 文件变更清单

- 修改：`src/skill/manager.rs`，收紧启用状态轻量探针的 manifest 文件系统探测错误传播。
- 修改：`src/skill/manager/tests.rs`，新增缺失清单默认启用与 manifest 探测失败测试。
- 修改：`TASK_LOG.md`，追加本轮任务记录。

### 关键代码调整详情

- `skill_manifest_path_exists` 错误信息包含 `skill.yaml` 路径和底层错误，避免将不可探测路径伪装成缺失清单。
- `is_skill_manifest_enabled` 只有在 `skill_manifest_path_exists` 返回 false 时才继续执行 `Ok(true)` 默认启用分支。
- 探测成功且清单存在时，原有 YAML 读取、解析、禁止声明 `skill_id` 和 `enable` 默认值逻辑保持不变。
- 探测失败测试直接调用私有启用探针边界；真实目录枚举无法产生内嵌 NUL 的目录项，因此不通过 `collect_named_skill_dirs` 伪造路径。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test is_skill_manifest_enabled_defaults_missing_manifest_to_enabled -- --nocapture` 通过，1 个目标测试通过。
- 修改后：`cargo test is_skill_manifest_enabled_rejects_manifest_probe_errors -- --nocapture` 通过，1 个目标测试通过。
- 回归验证：`cargo test skill::manager -- --nocapture` 通过，21 个 skill manager 范围测试全部通过。
- 全量验证：`cargo test` 通过，375 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 空白审核：`git diff --check` 通过。
- 搜索复查：`skill_yaml.exists()` 已不在 `src/skill/manager.rs` 中出现；启用状态探针统一经过 `skill_manifest_path_exists`。

### 代码审核与遗留事项

- 本轮没有改变缺失 `skill.yaml` 默认启用语义、空目录禁用 override 语义、manifest 解析规则、`skill_id` 禁止声明规则、ROOT/PROJECT/USER 覆盖优先级或安装/更新完整 manifest 校验流程。
- 确认缺失的 `skill.yaml` 仍默认启用；清单存在时仍按 `enable` 字段判断；只有无法完成 metadata 探测的路径改为显式错误。
- 修改部分代码审核确认没有引入候选路径兜底、多来源字段、静默默认值、额外目录创建、root 链重排或启用状态默认值漂移。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续排查 runlua package path 构造中的剩余 `.exists()`、skill manager 安装/更新临时目录探测，以及其它生命周期路径中的文件系统错误折叠。

## 2026-07-05 第 176 轮：disabled record 探测错误不再被当作未禁用

### 问题探索

- 基线延续第 175 轮闭环状态：`cargo test` 通过，375 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 已追清执行流：`LuaEngine::load_from_roots` 在加载每个 skill 前调用 `SkillManager::is_skill_enabled`，该函数通过 disabled-state record 判断 skill 是否被停用。
- 旧启用状态问题：`is_skill_enabled` 直接调用 `self.disabled_record_path(skill_id).exists()`。metadata 探测失败会被折叠成 false，最终返回 enabled。
- 已继续追到同一状态边界：`enable_skill_in_plane`、`disabled_record`、`remove_disabled_record` 也都围绕同一个 disabled record 路径做存在性判断。
- 旧读取/删除问题：`disabled_record` 会把探测失败当作 `None`，`enable` 与 rollback 删除路径会把探测失败当作记录不存在。
- 长期优化判断：确认缺失 disabled record 仍表示 skill 未禁用；但探测失败不是缺失，必须显式失败，避免误启用或跳过状态清理。

### 执行调整

- 新增 `disabled_record_path_exists`，用 `try_exists()` 探测单个 disabled-state record。
- `is_skill_enabled` 改为通过 `disabled_record_path_exists` 判断是否存在停用记录。
- `enable_skill_in_plane` 改为通过同一个 helper 判断是否需要删除停用记录。
- `disabled_record` 改为通过同一个 helper 区分缺失记录和探测失败。
- `remove_disabled_record` 改为通过同一个 helper 判断 rollback 删除目标是否存在。
- 新增 `disabled_record_returns_none_when_missing`，锁住缺失 disabled record 仍返回 `None` 的语义。
- 新增 `disabled_record_rejects_probe_errors`，用内嵌 NUL lifecycle root 验证可选 disabled record 读取不会把探测失败当成缺失记录。

### 文件变更清单

- 修改：`src/skill/manager.rs`，统一 disabled-state record 存在性探测路径。
- 修改：`src/skill/manager/tests.rs`，新增缺失 disabled record 与探测失败测试。
- 修改：`TASK_LOG.md`，追加本轮任务记录。

### 关键代码调整详情

- `disabled_record_path_exists` 的错误信息包含 disabled record 路径和底层错误，便于区分“记录缺失”和“路径不可探测”。
- `is_skill_enabled` 现在只在 helper 返回 false 时认为 skill enabled；helper 返回 Err 时直接中止加载/查询路径。
- `disabled_record` 现在只在 helper 返回 false 时返回 `Ok(None)`；探测失败时直接返回错误。
- `enable_skill_in_plane` 与 `remove_disabled_record` 不再用 `path.exists()` 跳过删除逻辑，避免探测失败被当作无需删除。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test disabled_record_returns_none_when_missing -- --nocapture` 通过，1 个目标测试通过。
- 修改后：`cargo test disabled_record_rejects_probe_errors -- --nocapture` 通过，1 个目标测试通过。
- 回归验证：`cargo test skill::manager -- --nocapture` 通过，23 个 skill manager 范围测试全部通过。
- 全量验证：`cargo test` 通过，377 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 空白审核：`git diff --check` 通过。
- 搜索复查：disabled record 相关路径已统一经过 `disabled_record_path_exists`，不存在 `disabled_record_path(...).exists()` 直连判断。

### 代码审核与遗留事项

- 本轮没有改变缺失 disabled record 表示未禁用的语义、disabled record JSON 格式、disable 写入逻辑、enable 删除语义、ROOT 层级保护或 rollback 恢复策略。
- 确认缺失的 disabled record 仍返回 `None` 或 enabled；只有无法完成 metadata 探测的路径改为显式错误。
- 修改部分代码审核确认没有引入候选路径兜底、多来源字段、静默默认值、额外目录创建、生命周期状态反转或权限边界漂移。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续排查 install record 存在性探测、skill manager 安装/更新临时目录探测、runlua package path 构造中的剩余 `.exists()`，以及其它生命周期路径中的文件系统错误折叠。

## 2026-07-05 第 177 轮：install record 探测错误不再被当作非受管安装

### 问题探索

- 基线延续第 176 轮闭环状态：`cargo test` 通过，377 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 已追清执行流：`prepare_update_skill` 在确认 skill 目录存在后调用 `prepare_managed_skill_update`，后者通过 `install_record(skill_id)` 读取受管安装记录来决定能否自动更新。
- 旧读取问题：`install_record` 使用 `install_record_path(skill_id).exists()` 判断记录是否存在。metadata 探测失败会被折叠成 false，最终表现为“skill 不是 install workflow 管理的”。
- 已继续追到回滚/清理路径：`remove_install_record` 同样使用 `path.exists()` 判断是否需要删除安装记录，用于恢复旧记录或移除当前记录。
- 旧删除问题：安装记录路径不可探测时，删除路径会返回 `false`，等同于“记录不存在”，可能掩盖 rollback 清理失败。
- 长期优化判断：确认缺失 install record 仍表示没有受管安装记录；但探测失败不是缺失，必须显式失败，避免错误地阻断更新或跳过状态清理。

### 执行调整

- 新增 `install_record_path_exists`，用 `try_exists()` 探测单个受管安装记录。
- `install_record` 改为通过 `install_record_path_exists` 区分缺失记录和探测失败。
- `remove_install_record` 改为通过同一个 helper 判断是否需要删除安装记录。
- 新增 `install_record_returns_none_when_missing`，锁住缺失 install record 仍返回 `None` 的语义。
- 新增 `install_record_rejects_probe_errors`，用内嵌 NUL lifecycle root 验证可选 install record 读取不会把探测失败当成缺失记录。

### 文件变更清单

- 修改：`src/skill/manager.rs`，统一 managed install record 存在性探测路径。
- 修改：`src/skill/manager/tests.rs`，新增缺失 install record 与探测失败测试。
- 修改：`TASK_LOG.md`，追加本轮任务记录。

### 关键代码调整详情

- `install_record_path_exists` 的错误信息包含 install record 路径和底层错误，便于区分“记录缺失”和“路径不可探测”。
- `install_record` 现在只在 helper 返回 false 时返回 `Ok(None)`；探测失败时直接返回错误。
- `remove_install_record` 现在只在 helper 返回 false 时返回 `Ok(false)`；探测失败时直接返回错误。
- 测试通过非法 lifecycle root 派生出不可探测的 `vulcan-codekit.yaml`，直接覆盖 install record 可选读取边界。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test install_record_returns_none_when_missing -- --nocapture` 通过，1 个目标测试通过。
- 修改后：`cargo test install_record_rejects_probe_errors -- --nocapture` 通过，1 个目标测试通过。
- 回归验证：`cargo test skill::manager -- --nocapture` 通过，25 个 skill manager 范围测试全部通过。
- 全量验证：`cargo test` 通过，379 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 空白审核：`git diff --check` 通过。
- 搜索复查：install record 相关读取和删除路径已统一经过 `install_record_path_exists`，不存在 `install_record_path(...).exists()` 直连判断。

### 代码审核与遗留事项

- 本轮没有改变缺失 install record 表示非受管安装的语义、install record YAML 格式、安装记录持久化逻辑、自动更新来源分发逻辑、rollback 恢复策略或 ROOT 层级保护。
- 确认缺失的 install record 仍返回 `None` 或删除 `false`；只有无法完成 metadata 探测的路径改为显式错误。
- 修改部分代码审核确认没有引入候选路径兜底、多来源字段、静默默认值、额外目录创建、生命周期状态反转或更新权限边界漂移。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续排查 skill manager 安装/更新临时目录探测、target skill 目录存在性探测、runlua package path 构造中的剩余 `.exists()`，以及其它生命周期路径中的文件系统错误折叠。

## 2026-07-05 第 178 轮：卸载包目录探测错误不再被当作目录缺失

### 问题探索

- 基线延续第 177 轮闭环状态：`cargo test` 通过，379 个测试全部通过；`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 已追清执行流：`uninstall_skill` 先按当前 skill root 拼出 `skill_dir`，随后调用 `prepare_uninstall_skill_at_path_in_plane` 暂存卸载，最后提交删除状态记录。
- 旧逻辑问题：`prepare_uninstall_skill_at_path_in_plane` 使用 `skill_dir.exists()` 判断是否需要把当前 skill 包移动到 uninstall backup。
- 风险边界确认：metadata 探测失败会被折叠成 false，结果变成 `skill_removed = false`，消息为 `skill package directory not found`，真正的路径不可探测原因会被吞掉。
- 已确认该路径在 `ensure_state_layout`、disabled record、install record 读取之后执行；因此用有效 lifecycle root 加非法 skill root 可以精准触达包目录探测边界。
- 长期优化判断：确认缺失 skill 包目录仍应返回未删除；但探测失败不是缺失，必须显式失败，避免卸载流程错误地跳过待删除包。

### 执行调整

- 新增 `skill_package_dir_exists`，用 `try_exists()` 探测单个 lifecycle 操作使用的 skill 包目录。
- `prepare_uninstall_skill_at_path_in_plane` 改为通过 `skill_package_dir_exists(skill_dir)?` 判断是否需要暂存移动到 uninstall backup。
- 新增 `uninstall_skill_reports_not_removed_when_package_dir_is_missing`，锁住确认缺失包目录仍返回未删除的原有语义。
- 新增 `uninstall_skill_rejects_package_dir_probe_errors`，用有效 lifecycle root 加内嵌 NUL skill root 验证包目录探测失败不会被当作目录缺失。

### 文件变更清单

- 修改：`src/skill/manager.rs`，收紧卸载包目录存在性探测错误传播。
- 修改：`src/skill/manager/tests.rs`，新增卸载包目录缺失与探测失败测试。
- 修改：`TASK_LOG.md`，追加本轮任务记录。

### 关键代码调整详情

- `skill_package_dir_exists` 的错误信息包含 skill 包目录路径和底层错误，便于区分“目录缺失”和“路径不可探测”。
- `prepare_uninstall_skill_at_path_in_plane` 只有在 helper 返回 true 时创建 uninstall backup 并移动当前包目录。
- helper 返回 false 时仍走原有未删除分支，保持 `skill package directory not found` 的业务语义。
- helper 返回 Err 时直接中止卸载准备，避免构造一个看似成功但没有移动包目录的卸载结果。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test uninstall_skill_reports_not_removed_when_package_dir_is_missing -- --nocapture` 通过，1 个目标测试通过。
- 修改后：`cargo test uninstall_skill_rejects_package_dir_probe_errors -- --nocapture` 通过，1 个目标测试通过。
- 回归验证：`cargo test skill::manager -- --nocapture` 通过，27 个 skill manager 范围测试全部通过。
- 全量验证：`cargo test` 通过，381 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 空白审核：`git diff --check` 通过。
- 搜索复查：卸载准备路径已统一经过 `skill_package_dir_exists`，`src/skill/manager.rs` 中不再存在 `skill_dir.exists()` 直连判断。

### 代码审核与遗留事项

- 本轮没有改变缺失 skill 包目录返回未删除的语义、uninstall backup 目录命名、状态记录读取/删除、提交与回滚流程或 ROOT 层级保护。
- 确认缺失的 skill 包目录仍返回 `skill_removed = false`；只有无法完成 metadata 探测的路径改为显式错误。
- 修改部分代码审核确认没有引入候选路径兜底、多来源字段、静默默认值、额外目录创建、生命周期状态反转或权限边界漂移。
- 修正后 `cargo test`、`cargo clippy --all-targets -- -D warnings` 与 `git diff --check` 均通过。
- 后续循环可继续排查安装/更新 target skill 目录存在性探测、install/update 临时目录探测、runlua package path 构造中的剩余 `.exists()`，以及其它生命周期路径中的文件系统错误折叠。

## 2026-07-06 第179轮：安装/更新 apply 阶段目标与备份目录探测显式化

### 探索记录

- 本轮沿 `prepare_install_skill` 与 `prepare_update_skill` 继续追到真实文件系统变更入口：`stage_skill_install_from_archive` 在下载解包、manifest 校验后检查最终 `target_dir`；`stage_skill_update_from_archive` 在解包与版本校验后检查当前已安装 `target_dir`，再移动到 update backup。
- 继续追到运行时 reload 后的收尾链路：`commit_prepared_skill_apply` 对 update 分支先持久化新安装记录，再清理旧 backup；`rollback_prepared_skill_apply` 对 install/update 分支负责删除暂存目标并恢复旧 backup。
- 问题确认：这些路径仍使用 `Path::exists()` 判断生命周期包目录。该 API 会把元数据探测错误折叠成 `false`，导致安装路径把“无法探测目标目录”伪装成可继续 rename，更新路径把“无法探测已安装目录”伪装成未安装，提交路径可能在 backup 探测失败后保留新 install record，回滚路径则可能先删除暂存目标再发现 backup 不可探测。
- 语义边界确认：这些目录都是单一来源的生命周期包目录，不存在协议定义的多版本字段或多来源兼容需求，因此不允许写候选式 fallback，只能把探测失败作为显式错误返回。

### 核心修复与调整概述

- `stage_skill_install_from_archive` 的目标目录存在性检查改为 `skill_package_dir_exists(&target_dir)?`，保留“已存在则拒绝安装”的业务语义，同时显式报告探测错误。
- `stage_skill_update_from_archive` 的已安装目录检查改为 `skill_package_dir_exists(&target_dir)?`，保留“确认缺失则拒绝更新”的业务语义，同时显式报告探测错误。
- `commit_prepared_skill_apply` 的 update backup 清理前改为显式探测；如果探测失败，会恢复 `previous_install_record` 并返回错误，避免新记录已经写入但旧 backup 状态未知的半提交。
- `rollback_prepared_skill_apply` 的 install 分支改为显式探测暂存目标；update 分支在删除暂存目标前先同时探测 target 与 backup，避免 backup 路径异常时先破坏可回滚目标。

### 文件变更清单

- 修改：`src/skill/manager.rs`，收紧安装/更新 apply 阶段 target、backup 目录存在性探测与更新提交补偿逻辑。
- 修改：`src/skill/manager/tests.rs`，新增预备 apply 测试夹具与三条路径探测失败测试。
- 修改：`TASK_LOG.md`，追加本轮任务记录。

### 关键代码调整详情

- 安装暂存完成前的 `target_dir.exists()` 被替换为 `skill_package_dir_exists(&target_dir)?`，让内嵌 NUL 等不可探测路径直接返回 `Failed to inspect skill package directory`。
- 更新暂存备份当前包前的 `!target_dir.exists()` 被替换为 `!skill_package_dir_exists(&target_dir)?`，只在确认缺失时返回 `installed skill directory ... does not exist`。
- 更新提交清理 backup 时，`skill_package_dir_exists` 的 `Err` 分支复用旧记录恢复路径；恢复成功时返回包含 `previous install record was restored` 的诊断，恢复失败时追加恢复失败原因。
- 更新回滚分支新增 `target_dir_exists` 与 `backup_dir_exists` 的前置探测，只有两个路径都可确认后才删除暂存目标或恢复旧备份。
- 新增 `rollback_staged_install_rejects_target_dir_probe_errors`，验证暂存安装目标路径不可探测时不会被当作缺失目录。
- 新增 `rollback_staged_update_rejects_backup_probe_errors_before_removing_target`，验证 update backup 探测失败时暂存目标 marker 仍然存在。
- 新增 `commit_staged_update_restores_previous_record_on_backup_probe_error`，验证 update commit 在 backup 探测失败后恢复旧安装记录。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test rollback_staged_install_rejects_target_dir_probe_errors -- --nocapture` 通过，1 个目标测试通过。
- 修改后：`cargo test rollback_staged_update_rejects_backup_probe_errors_before_removing_target -- --nocapture` 通过，1 个目标测试通过。
- 修改后：`cargo test commit_staged_update_restores_previous_record_on_backup_probe_error -- --nocapture` 通过，1 个目标测试通过。
- 回归验证：`cargo test skill::manager -- --nocapture` 通过，30 个 skill manager 范围测试全部通过。
- 全量验证：`cargo test` 通过，384 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 空白审核：`git diff --check -- src/skill/manager.rs src/skill/manager/tests.rs` 通过。
- 搜索复查：安装/更新 apply 路径中的 target 与 update backup 判断已统一经过 `skill_package_dir_exists`；当前 `src/skill/manager.rs` 剩余直接 `.exists()` 集中在 `TempDirGuard`、install/update 临时目录清理、uninstall rollback 的 target/backup 判断。

### 代码审核与遗留事项

- 修改部分代码审核确认没有引入候选字段、候选路径、多来源兼容、静默默认值、额外目录创建或历史兼容分支。
- 安装路径仍保持“确认目标已存在则拒绝安装”；更新路径仍保持“确认当前包缺失则拒绝更新”；仅将无法完成 metadata 探测的情况改为显式错误。
- 更新提交的半提交风险已收紧：backup 探测失败会恢复旧 install record，而不是让新记录继续作为最终状态。
- 更新回滚的破坏顺序已收紧：只有 target 与 backup 都能完成探测后才会删除暂存目标。
- 后续循环应继续处理 uninstall rollback 中的 `prepared.target_dir.exists()` 与 `backup_dir.exists()`，以及 install/update 临时目录清理和 `TempDirGuard` 中剩余的文件系统错误折叠。

## 2026-07-06 第180轮：卸载回滚 target 与 backup 目录探测显式化

### 探索记录

- 本轮从 `uninstall_skill` 与 `uninstall_skill_in_plane` 追到 `prepare_uninstall_skill_at_path_in_plane`，确认卸载准备阶段会把真实 skill 包目录移动到 `uninstall_backup`，并把原始目标目录、backup 目录和旧 disabled/install 记录封装进 `PreparedSkillUninstall`。
- 继续追到 `rollback_prepared_skill_uninstall`，确认 reload 或 commit 失败后会先处理文件系统恢复：若存在 backup，则删除当前 target，再把 backup rename 回 target，随后恢复 disabled/install 状态记录。
- 问题确认：`rollback_prepared_skill_uninstall` 中 `prepared.target_dir.exists()` 与 `backup_dir.exists()` 会把元数据探测错误折叠成 `false`。当 target 不可探测时，错误会被伪装成“无目标可删”；当 backup 不可探测时，旧实现可能先删除 target，再把 backup 异常当作不存在，导致回滚破坏顺序错误。
- 语义边界确认：卸载回滚的 target 与 backup 都来自同一个 `PreparedSkillUninstall` 暂存结果，不存在协议允许的多来源、多路径兼容需求；因此不能写候选路径兜底，只能显式传播探测失败。

### 核心修复与调整概述

- `rollback_prepared_skill_uninstall` 在删除任何目录前先调用 `skill_package_dir_exists(&prepared.target_dir)?` 和 `skill_package_dir_exists(backup_dir)?`。
- 只有 target 与 backup 的元数据探测都成功后，才按原有语义删除 target、恢复 backup。
- target 或 backup 探测失败时直接返回 `Failed to inspect skill package directory ...`，不再把不可探测路径当作缺失路径。
- backup 探测失败时不会删除当前 target，避免卸载回滚在恢复旧包前破坏可保留状态。

### 文件变更清单

- 修改：`src/skill/manager.rs`，收紧卸载回滚 target/backup 目录存在性探测和删除顺序。
- 修改：`src/skill/manager/tests.rs`，新增卸载回滚结果夹具与两条路径探测失败测试。
- 修改：`TASK_LOG.md`，追加本轮任务记录。

### 关键代码调整详情

- `prepared.target_dir.exists()` 被替换为 `skill_package_dir_exists(&prepared.target_dir)?`，target 路径不可探测时立即中止回滚。
- `backup_dir.exists()` 被替换为 `skill_package_dir_exists(backup_dir)?`，backup 路径不可探测时立即中止回滚。
- 新增 `target_dir_exists` 与 `backup_dir_exists` 前置变量，确保删除 target 前已经确认 backup 的可探测状态。
- 新增 `test_uninstall_result`，用于构造 `PreparedSkillUninstall` 测试夹具，避免测试依赖下载或真实 reload 流程。
- 新增 `rollback_staged_uninstall_rejects_target_dir_probe_errors`，验证 target 含内嵌 NUL 时不会被当作缺失路径。
- 新增 `rollback_staged_uninstall_rejects_backup_probe_errors_before_removing_target`，验证 backup 含内嵌 NUL 时 target marker 仍然存在。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test rollback_staged_uninstall_rejects_target_dir_probe_errors -- --nocapture` 通过，1 个目标测试通过。
- 修改后：`cargo test rollback_staged_uninstall_rejects_backup_probe_errors_before_removing_target -- --nocapture` 通过，1 个目标测试通过。
- 回归验证：`cargo test skill::manager -- --nocapture` 通过，32 个 skill manager 范围测试全部通过。
- 全量验证：`cargo test` 通过，386 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 空白审核：`git diff --check -- src/skill/manager.rs src/skill/manager/tests.rs` 通过。
- 搜索复查：`src/skill/manager.rs` 中卸载回滚路径已不再包含直接 `.exists()`；剩余直接 `.exists()` 只集中在 `TempDirGuard` 与 install/update 临时目录清理。

### 代码审核与遗留事项

- 修改部分代码审核确认没有引入候选字段、候选路径、多来源兼容、静默默认值、额外目录创建或历史兼容分支。
- 卸载回滚仍保持原有确认存在时删除 target、确认 backup 存在时恢复 backup、最终恢复 disabled/install 记录的业务顺序；只把不可探测路径从“缺失”改成显式错误。
- backup 探测失败的破坏顺序已收紧：不会先删除 target 再发现 backup 状态未知。
- 后续循环应继续处理 install/update 临时目录清理中的 `install_temp_root.exists()`、`temp_root.exists()`，以及 `TempDirGuard` 析构清理中的 `self.path.exists()`。

## 2026-07-06 第181轮：安装/更新暂存临时根目录探测显式化

### 探索记录

- 本轮从 `prepare_install_skill_from_*` 和 `prepare_*_managed_skill_update` 追到真实暂存入口：`stage_skill_install_from_archive` 与 `stage_skill_update_from_archive` 都会先计算 lifecycle 下的 `install_tmp` / `update_tmp` 目录，再清理同名陈旧目录、创建临时根、解压 zip、校验 manifest，最后移动到 target。
- 关键顺序确认：`install_temp_root.exists()` 与 `temp_root.exists()` 发生在 `extract_skill_package_zip` 之前，因此临时根路径不可探测时应在任何归档打开、解压或目标目录变更前中止。
- 问题确认：旧代码使用 `Path::exists()` 检查陈旧临时根，会把元数据探测错误折叠成 `false`。这会让不可探测的 staging root 继续进入 `create_dir_all`，报错语义变成“创建失败”，掩盖真实的路径探测失败。
- 语义边界确认：install/update 暂存根是 lifecycle staging 资源，不是 skill package 目录；因此没有复用 `skill_package_dir_exists`，而是新增专用 helper 来保留临时根语义。

### 核心修复与调整概述

- 新增 `staging_temp_root_exists(temp_root: &Path) -> Result<bool, String>`，专门用于 install/update staging temp root 的元数据探测。
- `stage_skill_install_from_archive` 的 `install_temp_root.exists()` 改为 `staging_temp_root_exists(&install_temp_root)?`。
- `stage_skill_update_from_archive` 的 `temp_root.exists()` 改为 `staging_temp_root_exists(&temp_root)?`。
- 临时根不可探测时现在直接返回 `Failed to inspect skill staging temp root ...`，不会继续尝试创建目录或打开未使用的归档路径。

### 文件变更清单

- 修改：`src/skill/manager.rs`，新增 staging temp root 探测 helper，并替换 install/update 暂存临时根存在性判断。
- 修改：`src/skill/manager/tests.rs`，新增两条直接覆盖 install/update stage 临时根探测失败的测试。
- 修改：`TASK_LOG.md`，追加本轮任务记录。

### 关键代码调整详情

- `staging_temp_root_exists` 使用 `try_exists()`，返回 true/false/Err 三态结果，错误消息包含 `skill staging temp root` 和 host-visible 路径。
- 安装暂存阶段在 `extract_skill_package_zip` 前就显式检查 `install_tmp/{skill_id}-{timestamp}`；探测失败立即返回，不进入目录创建。
- 更新暂存阶段在 `extract_skill_package_zip` 前就显式检查 `update_tmp/{skill_id}-{timestamp}`；探测失败立即返回，不进入目录创建。
- 新增 `stage_skill_install_rejects_temp_root_probe_errors`，用含内嵌 NUL 的 lifecycle root 验证 install staging 在解包前返回探测错误。
- 新增 `stage_skill_update_rejects_temp_root_probe_errors`，用含内嵌 NUL 的 lifecycle root 验证 update staging 在解包前返回探测错误。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test stage_skill_install_rejects_temp_root_probe_errors -- --nocapture` 通过，1 个目标测试通过。
- 修改后：`cargo test stage_skill_update_rejects_temp_root_probe_errors -- --nocapture` 通过，1 个目标测试通过。
- 回归验证：`cargo test skill::manager -- --nocapture` 通过，34 个 skill manager 范围测试全部通过。
- 全量验证：`cargo test` 通过，388 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 空白审核：`git diff --check -- src/skill/manager.rs src/skill/manager/tests.rs` 通过。
- 搜索复查：`src/skill/manager.rs` 中 install/update 暂存临时根已统一经过 `staging_temp_root_exists`；当前 manager 内直接 `.exists()` 仅剩 `TempDirGuard` 析构清理。

### 代码审核与遗留事项

- 修改部分代码审核确认没有引入候选字段、候选路径、多来源兼容、静默默认值、额外目录创建或历史兼容分支。
- 正常路径仍保持原有语义：确认存在陈旧临时根时先删除，确认缺失时直接创建，只有无法完成 metadata 探测时改为显式错误。
- 这轮没有改变 archive 解压、manifest 校验、target 移动、install record 写入或 update backup 逻辑。
- 后续循环应继续处理 `TempDirGuard` 析构清理中的 `self.path.exists()`，并可转入 `src/download/archive.rs` 中剩余的归档解压/测试临时目录 `.exists()` 折叠点。

## 2026-07-06 第182轮：TempDirGuard 析构清理去除 exists 预探测

### 探索记录

- 本轮从 `TempDirGuard` 的创建和使用点追踪到 install/update 暂存流程：`stage_skill_install_from_archive` 与 `stage_skill_update_from_archive` 在创建临时根后绑定 guard，成功移动 target 后显式 `disarm`，失败或提前返回时由 Drop 尽力清理 staging 目录。
- 现有 Drop 逻辑为 `if !self.disarmed && self.path.exists() { remove_dir_all(...) }`，其中 `exists()` 仍会把元数据探测错误折叠成 false，使非法路径或不可探测路径表现为“不需要清理”。
- 语义边界确认：Rust `Drop::drop` 不能向调用方返回 `Result`，因此无法像普通生命周期函数一样显式传播错误；更长期的做法是移除预探测，直接执行删除操作，并在可测试 helper 中保留 NotFound 与真实清理错误的区别。
- 未选择日志输出作为主方案：当前 skill manager 其他析构清理没有稳定日志通道，直接引入 stderr 或全局日志会扩大副作用；本轮只把清理语义集中到 helper，Drop 保持 best-effort。

### 核心修复与调整概述

- 新增 `remove_staging_dir_if_present(path: &Path) -> Result<bool, String>`，直接调用 `fs::remove_dir_all`，只把 `ErrorKind::NotFound` 视为已缺失并返回 false。
- `TempDirGuard::drop` 去除 `self.path.exists()` 预探测；guard 未 disarm 时直接调用 `remove_staging_dir_if_present(&self.path)` 并忽略结果，保持析构 best-effort 语义。
- 非法路径、权限错误等删除失败现在由 helper 以 `Failed to remove staging directory ...` 表达，不再通过 `exists()` 变成静默 false。

### 文件变更清单

- 修改：`src/skill/manager.rs`，新增 staging 目录删除 helper，并调整 `TempDirGuard::drop` 调用方式。
- 修改：`src/skill/manager/tests.rs`，新增缺失目录与非法路径两条 helper 测试。
- 修改：`TASK_LOG.md`，追加本轮任务记录。

### 关键代码调整详情

- `remove_staging_dir_if_present` 返回三态语义：删除成功为 `Ok(true)`，目录原本不存在为 `Ok(false)`，其他删除失败为 `Err(...)`。
- `TempDirGuard::drop` 不再执行 metadata 预探测，避免把探测失败折叠成“目录不存在”。
- `temp_dir_guard_removes_staging_root_on_drop` 继续验证真实临时目录在 Drop 后被移除。
- 新增 `remove_staging_dir_reports_missing_directory`，验证不存在的 staging 目录返回 false。
- 新增 `remove_staging_dir_reports_invalid_path_errors`，验证含内嵌 NUL 的路径返回显式清理错误。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test temp_dir_guard_removes_staging_root_on_drop -- --nocapture` 通过，1 个目标测试通过。
- 修改后：`cargo test remove_staging_dir_reports_missing_directory -- --nocapture` 通过，1 个目标测试通过。
- 修改后：`cargo test remove_staging_dir_reports_invalid_path_errors -- --nocapture` 通过，1 个目标测试通过。
- 回归验证：`cargo test skill::manager -- --nocapture` 通过，36 个 skill manager 范围测试全部通过。
- 全量验证：`cargo test` 通过，390 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 空白审核：`git diff --check -- src/skill/manager.rs src/skill/manager/tests.rs` 通过。
- 搜索复查：`src/skill/manager.rs` 中已没有直接 `.exists()` 调用；剩余 `.exists()` 仅在测试夹具清理和断言中出现。

### 代码审核与遗留事项

- 修改部分代码审核确认没有引入候选字段、候选路径、多来源兼容、静默默认值、额外目录创建或历史兼容分支。
- Drop 仍保持原有 best-effort 行为，不会在析构中 panic；错误表达通过可测试 helper 固化。
- `manager.rs` 生命周期路径中的 `.exists()` 折叠点已清理完毕。
- 后续循环可转入 `src/download/archive.rs` 中 `extract_skill_package_zip` 的 `skill_yaml.exists()`、payload 安装的 `target_path.exists()`，以及测试临时目录清理中的剩余 `.exists()`。

## 2026-07-06 第183轮：归档解压清单与 tar.gz 导出目标探测显式化

### 探索记录

- 本轮从 `install_downloaded_payload` 与 `extract_skill_package_zip` 进入 `src/download/archive.rs`，确认该模块同时承担 dependency payload 安装和 skill zip 解压两类职责。
- `extract_skill_package_zip` 的执行流为：创建临时根、打开 zip、逐条校验顶层目录、解压文件，最后检查 `{expected_skill_id}/skill.yaml` 是否存在。该路径代表 skill 包合法性的必需清单。
- `install_from_tar_gz_archive` 的执行流为：读取 tar.gz、按 export 匹配条目、写入目标文件、收集 executable 标记，最后逐个检查 export target 是否已生成。该路径代表依赖导出是否实际落盘。
- 问题确认：`skill_yaml.exists()` 与 `target_path.exists()` 都会把元数据探测错误折叠为 false，使“路径不可探测”伪装成“包缺少 skill.yaml”或“tar.gz 缺少 export”。
- 语义边界确认：skill manifest 与 installed export target 是两个不同业务路径，不能用一个模糊 helper 兜底；应分别命名并分别表达错误语义。

### 核心修复与调整概述

- 新增 `extracted_skill_manifest_exists(skill_yaml: &Path) -> Result<bool, String>`，专门检查已解压 skill manifest。
- 新增 `installed_export_target_exists(target_path: &Path) -> Result<bool, String>`，专门检查 tar.gz 安装后的导出目标。
- `extract_skill_package_zip` 的最终 manifest 检查改为通过 `extracted_skill_manifest_exists(&skill_yaml)?`。
- `install_from_tar_gz_archive` 的导出目标检查改为通过 `installed_export_target_exists(&target_path)?`。

### 文件变更清单

- 修改：`src/download/archive.rs`，新增两个存在性探测 helper，替换 skill manifest 与 tar.gz export target 的直接 `.exists()`。
- 修改：`TASK_LOG.md`，追加本轮任务记录。

### 关键代码调整详情

- `extracted_skill_manifest_exists` 使用 `try_exists()`，返回 true/false/Err 三态结果，错误消息包含 `extracted skill manifest` 与 host-visible 路径。
- `installed_export_target_exists` 使用 `try_exists()`，返回 true/false/Err 三态结果，错误消息包含 `installed export target` 与 host-visible 路径。
- 保留原有业务错误：确认缺失 manifest 时仍返回 `Skill package ... does not contain .../skill.yaml`。
- 保留原有业务错误：确认缺失 export target 时仍返回 `tar.gz archive ... does not contain required export ...`。
- 新增 `extracted_skill_manifest_probe_errors_are_reported`，验证含内嵌 NUL 的 manifest 路径返回显式探测错误。
- 新增 `installed_export_target_probe_errors_are_reported`，验证含内嵌 NUL 的 export target 路径返回显式探测错误。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test extracted_skill_manifest_probe_errors_are_reported -- --nocapture` 通过，1 个目标测试通过。
- 修改后：`cargo test installed_export_target_probe_errors_are_reported -- --nocapture` 通过，1 个目标测试通过。
- 范围验证：`cargo test download::archive -- --nocapture` 通过，4 个 archive 范围测试全部通过。
- 范围验证：`cargo test download -- --nocapture` 通过，16 个 download 范围测试全部通过。
- 全量验证：`cargo test` 通过，392 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 空白审核：`git diff --check -- src/download/archive.rs` 通过。
- 搜索复查：`src/download/archive.rs` 生产代码中已不再存在直接 `.exists()`；当前仅测试夹具清理仍有 `temp_root.exists()`。

### 代码审核与遗留事项

- 修改部分代码审核确认没有引入候选字段、候选路径、多来源兼容、静默默认值、额外目录创建或历史兼容分支。
- 这轮没有改变 zip 条目规范化、顶层目录限制、tar.gz export 匹配、文件写入或 executable 标记逻辑。
- 已确认缺失和探测失败的诊断边界被拆开：缺失仍走原业务错误，无法探测则返回显式 inspect 错误。
- 后续循环可继续排查 `src/download/manager.rs` 的缓存目标 `target_path.exists()`，以及 `src/dependency/manager.rs` 中 stale/current install root 的剩余 `.exists()` 折叠点。

## 2026-07-06 第184轮：下载缓存目标探测显式化

### 探索记录

- 本轮从 `DownloadManager::download` 追踪下载缓存执行流：先校验网络策略，再创建 cache root，然后通过 `cached_path_for_request` 派生确定性缓存路径，随后检查缓存命中；命中时读取 metadata、验证普通文件、发出 cached progress 并返回；未命中时才发起 HTTP 下载并写入缓存。
- 继续追到 `download_with_sha256`：它依赖 `download` 返回的缓存路径做 SHA-256 校验，校验失败时删除旧缓存并自动重下。因此缓存路径探测错误发生在所有网络和校验流程之前，是下载状态机的关键前置条件。
- 问题确认：`target_path.exists()` 会把元数据探测错误折叠为 false，使“缓存目标不可探测”伪装成“缓存未命中”，随后可能继续发起网络请求或尝试写入同一异常路径。
- 语义边界确认：缓存目标是由单个 `DownloadRequest` 派生出的唯一确定路径，不存在多来源、多路径兼容需求；探测失败必须显式返回，不能写 fallback。

### 核心修复与调整概述

- 新增 `cached_download_target_exists(target_path: &Path) -> Result<bool, String>`，专门检查下载缓存目标。
- `DownloadManager::download` 的缓存命中判断改为 `cached_download_target_exists(&target_path)?`。
- 缓存目标不可探测时现在返回 `Failed to inspect cached download target ...`，不会继续走网络下载或写缓存。
- 已有缓存目录损坏测试继续覆盖“确认存在但不是普通文件”的原有错误语义。

### 文件变更清单

- 修改：`src/download/manager.rs`，新增缓存目标探测 helper，替换下载入口中的直接 `.exists()`。
- 修改：`TASK_LOG.md`，追加本轮任务记录。

### 关键代码调整详情

- `cached_download_target_exists` 使用 `try_exists()`，返回 true/false/Err 三态结果，错误消息包含 `cached download target` 与 host-visible 路径。
- `download` 只在 helper 返回 true 时进入 metadata 读取与缓存命中路径。
- helper 返回 false 时保持原有缓存未命中语义，继续网络下载。
- helper 返回 Err 时立即中止下载，避免把缓存目标探测失败伪装成未命中。
- 新增 `cached_download_target_probe_errors_are_reported`，验证含内嵌 NUL 的缓存目标路径返回显式探测错误。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test cached_download_target_probe_errors_are_reported -- --nocapture` 通过，1 个目标测试通过。
- 回归验证：`cargo test download_rejects_cached_directory_instead_of_returning_it -- --nocapture` 通过，1 个缓存命中回归测试通过。
- 范围验证：`cargo test download::manager -- --nocapture` 通过，12 个 download manager 范围测试全部通过。
- 范围验证：`cargo test download -- --nocapture` 通过，17 个 download 范围测试全部通过。
- 全量验证：`cargo test` 通过，393 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 空白审核：`git diff --check -- src/download/manager.rs` 通过。
- 搜索复查：`src/download/manager.rs` 生产代码中已不再存在直接 `.exists()`；当前仅测试夹具清理仍有 `.exists()`。

### 代码审核与遗留事项

- 修改部分代码审核确认没有引入候选字段、候选路径、多来源兼容、静默默认值、额外目录创建或历史兼容分支。
- 这轮没有改变缓存路径派生、HTTP 下载、进度回调、校验和重下载清理逻辑。
- 缓存确认缺失仍保持原有未命中语义；只有无法完成 metadata 探测的路径改为显式错误。
- 后续循环可继续排查 `src/dependency/manager.rs` 中 stale/current install root、managed runtime build/env root 等剩余 `.exists()` 折叠点。

## 2026-07-06 第185轮：依赖导出目标探测显式化

### 探索记录

- 本轮从 `DependencyManager::ensure_skill_dependencies` 追踪到 `ensure_dependency`：每个 tool/lua/ffi 依赖会先进入 `find_existing_local_dependency_request` 做本地/宿主复用探测，未命中后才根据 scope、网络开关和 source 决定是否解析远程包并下载。
- 继续追到 `detect_dependency`：它负责检查 `ResolvedDependencyRequest.exports` 中声明的全部导出目标，路径来源唯一为 `request.install_root + export.target_path`，其中 `export.target_path` 在解析阶段由 manifest package exports 或模板展开得到。
- 问题确认：原实现使用 `request.exports.iter().all(... .exists())` 聚合导出目标存在性；当任一导出目标元数据探测失败时，`.exists()` 会把错误折叠为 false，使“导出目标不可探测”伪装成 `DependencyDetectionStatus::Missing`。
- 执行流影响确认：在本地复用阶段，错误会伪装成本地依赖缺失；在最终下载后复检阶段，错误会伪装成“下载后导出文件仍缺失”。这会掩盖真实文件系统错误，并可能误触发远程解析或下载流程。
- 语义边界确认：导出目标是单个 `ResolvedDependencyRequest` 内声明的确定安装结果，不存在多来源、多路径兼容需求；确认不存在才是 Missing，探测失败必须显式返回错误。

### 核心修复与调整概述

- 新增 `dependency_export_target_exists(dependency_name: &str, target_path: &Path) -> Result<bool, String>`，专门检查依赖导出目标存在性。
- `detect_dependency` 从 `.all(... exists())` 改为逐个导出目标调用 `dependency_export_target_exists`。
- 导出目标确认缺失时仍返回 `DependencyDetectionStatus::Missing`，保持原有未安装语义。
- 导出目标探测失败时现在返回 `Failed to inspect dependency export target ...`，不会继续伪装成本地缺失或下载后缺失。

### 文件变更清单

- 修改：`src/dependency/manager.rs`，新增依赖导出目标显式探测 helper，并替换 `detect_dependency` 中的直接 `.exists()` 聚合判断。
- 修改：`src/dependency/manager/tests.rs`，新增非法导出目标路径的显式错误测试。
- 修改：`TASK_LOG.md`，追加本轮任务记录。

### 关键代码调整详情

- `dependency_export_target_exists` 使用 `try_exists()` 返回 true/false/Err 三态结果，错误消息包含依赖名、宿主可见目标路径和底层探测错误。
- `detect_dependency` 现在按导出声明顺序检查目标路径；任一目标确认缺失时立即返回 Missing。
- `detect_dependency` 只有在所有声明导出目标都确认存在时才返回 Present。
- 新增 `detect_dependency_reports_export_target_probe_errors`，构造含内嵌 NUL 的 `export.target_path`，验证非法目标路径会触发显式探测错误。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 修改后：`cargo test detect_dependency_reports_export_target_probe_errors -- --nocapture` 通过，1 个目标测试通过。
- 范围验证：`cargo test dependency::manager -- --nocapture` 通过，15 个 dependency manager 范围测试全部通过。
- 范围验证：`cargo test dependency -- --nocapture` 通过，20 个 dependency 范围测试全部通过。
- 全量验证：`cargo test` 通过，394 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 空白审核：`git diff --check -- src/dependency/manager.rs src/dependency/manager/tests.rs` 通过。
- 搜索复查：`detect_dependency` 路径已不再使用 `.exists()`；`src/dependency/manager.rs` 生产代码中仍剩 `cleanup_updated_skill_dependencies` 与 `remove_skill_private_dependency_roots` 两处清理流 `.exists()`，留待后续循环单独追踪。

### 代码审核与遗留事项

- 修改部分代码审核确认没有引入候选字段、候选路径、多来源兼容、静默默认值或历史兼容分支。
- 本轮没有改变依赖安装根构造、远程解析、下载、归档安装、失败重试或清理策略。
- 错误语义只在“导出目标元数据不可探测”时收紧为显式错误；确认不存在的导出目标仍按 Missing 处理。
- 后续循环可继续排查 `src/dependency/manager.rs` 中 stale root 和 skill-private root 删除前的 `.exists()` 折叠点。

## 2026-07-06 第186轮：依赖私有根清理删除语义显式化

### 探索记录

- 本轮从 `cleanup_updated_skill_dependencies` 和 `cleanup_uninstalled_skill_dependencies_from_roots` 继续追踪依赖清理流。
- 更新清理调用链确认：运行时在技能 update 成功且结果为 `updated` 后加载新旧依赖 manifest，调用 `DependencyManager::cleanup_updated_skill_dependencies`；清理失败会作为 stale dependency cleanup warning 写入结果消息。
- 卸载清理调用链确认：运行时在技能 uninstall 成功后调用 `cleanup_uninstalled_skill_dependencies_from_roots`；部分入口会将依赖清理错误向上返回，另一个卸载流程会将错误记录为 warning。
- 路径来源确认：更新清理中的 stale root 来自 `collect_skill_local_dependency_roots` 对 previous/current manifest 的集合差集；卸载清理中的私有根来自 `tool_root/lua_root/ffi_root + removed_skill_id`。两者都是确定删除目标，不存在候选路径或多来源兼容需求。
- 问题确认：原实现使用 `if stale_root.exists()` 和 `if root.exists()` 再 `remove_dir_all`；当删除目标元数据探测失败时，`.exists()` 会把错误折叠为 false，使不可探测目标被当作“已经不存在”并静默跳过。

### 核心修复与调整概述

- 新增 `remove_stale_dependency_root(stale_root: &Path) -> Result<(), String>`，用于更新后删除过期依赖根。
- 新增 `remove_skill_private_dependency_root(root: &Path) -> Result<(), String>`，用于卸载后删除 skill 私有依赖根。
- 两个 helper 都改为直接执行 `fs::remove_dir_all`，仅将 `ErrorKind::NotFound` 视为已清理，其它错误显式返回。
- `cleanup_updated_skill_dependencies` 和 `remove_skill_private_dependency_roots` 不再进行删除前 `.exists()` 探测。

### 文件变更清单

- 修改：`src/dependency/manager.rs`，替换 stale root 与 skill-private root 删除前 `.exists()`，新增两个显式删除 helper。
- 修改：`src/dependency/manager/tests.rs`，新增更新清理和卸载清理的非法路径错误测试。
- 修改：`TASK_LOG.md`，追加本轮任务记录。

### 关键代码调整详情

- `cleanup_updated_skill_dependencies` 现在对 `previous_roots.difference(&current_roots)` 中的每个 stale root 调用 `remove_stale_dependency_root`。
- `remove_skill_private_dependency_roots` 现在对 tool/lua/ffi 三个 skill 私有根逐个调用 `remove_skill_private_dependency_root`。
- `remove_stale_dependency_root` 保留原错误前缀 `Failed to remove stale dependency root ...`，但不再依赖预先存在性探测。
- `remove_skill_private_dependency_root` 保留原错误前缀 `Failed to remove ...`，并对缺失根目录返回 Ok。
- 新增 `cleanup_updated_skill_dependencies_reports_invalid_stale_root_path`，验证非法 stale root 删除会显式报错。
- 新增 `cleanup_uninstalled_skill_dependencies_reports_invalid_private_root_path`，验证非法 skill-private root 删除会显式报错。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 目标验证：`cargo test reports_invalid -- --nocapture` 通过，4 个匹配测试全部通过，其中包含本轮新增的 2 个清理错误测试。
- 回归验证：`cargo test cleanup_updated_skill_dependencies -- --nocapture` 通过，5 个更新清理测试全部通过。
- 范围验证：`cargo test dependency::manager -- --nocapture` 通过，17 个 dependency manager 范围测试全部通过。
- 范围验证：`cargo test dependency -- --nocapture` 通过，22 个 dependency 范围测试全部通过。
- 全量验证：`cargo test` 通过，396 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 空白审核：`git diff --check -- src/dependency/manager.rs src/dependency/manager/tests.rs` 通过。
- 搜索复查：`src/dependency/manager.rs` 生产代码中已不再存在直接 `.exists()`；剩余 `.exists()` 均在测试夹具清理或断言中。

### 代码审核与遗留事项

- 修改部分代码审核确认没有引入候选路径、多来源兼容、静默 fallback、历史兼容分支或额外状态。
- 本轮没有改变依赖 root 收集、manifest 差集计算、运行时 update/uninstall 调用时机或 warning/错误传播策略。
- 缺失目录仍按“已清理”处理；只有无法删除或无法解释为缺失的文件系统错误会显式暴露。
- 后续循环可继续从全仓生产代码搜索剩余 `.exists()` 折叠点，例如 `src/runtime/engine.rs` 的数据库目录删除、import root 清理或运行时文件系统 helper。

## 2026-07-06 第187轮：技能数据库目录清理删除语义显式化

### 探索记录

- 本轮从 `LuaEngine::uninstall_skill_and_reload_in_root` 追踪技能卸载后的数据库清理流程。
- 调用链确认：技能卸载提交成功后，运行时分别调用 `remove_skill_database_dir(&database_root_for(resolved_root), skill_id, options.remove_sqlite, "sqlite")` 和 `remove_skill_database_dir(..., options.remove_lancedb, "lancedb")`。
- 返回语义确认：未请求删除时返回 `(false, true)` 表示保留；请求删除且目录确认缺失时返回 `(false, false)`；删除成功时返回 `(true, false)`；删除失败会被卸载流程转成 sqlite/lancedb cleanup warning 并写入结果消息。
- 路径来源确认：数据库目录唯一来源为 `database_root + database_label + skill_id`，其中 label 固定为 `sqlite` 或 `lancedb`，skill id 已在卸载入口校验，不存在多来源兼容需求。
- 问题确认：原实现先执行 `if !database_dir.exists()`，再 `remove_dir_all`；当目录存在性探测失败时，`.exists()` 会把错误折叠为 false，使不可探测的数据库目录被当作“确认缺失”，卸载结果不会记录 cleanup warning。

### 核心修复与调整概述

- `remove_skill_database_dir` 改为直接调用 `fs::remove_dir_all(&database_dir)`。
- `ErrorKind::NotFound` 仍返回 `(false, false)`，保持“请求删除但目录不存在”的原有语义。
- 除 NotFound 外的删除错误现在显式返回，继续走既有 sqlite/lancedb cleanup warning 通道。
- 删除前 `.exists()` 探测被移除，避免把探测失败静默折叠为缺失目录。

### 文件变更清单

- 修改：`src/runtime/engine.rs`，调整技能数据库目录清理 helper 的删除语义。
- 修改：`src/runtime/engine/tests.rs`，新增非法数据库清理目标路径测试。
- 修改：`TASK_LOG.md`，追加本轮任务记录。

### 关键代码调整详情

- `remove_skill_database_dir` 保留 `remove_requested == false` 时的 `(false, true)` 返回值。
- 请求删除时，helper 现在以 `remove_dir_all` 的结果作为权威事实来源。
- `remove_dir_all` 成功时返回 `(true, false)`。
- `remove_dir_all` 返回 `NotFound` 时返回 `(false, false)`。
- 其它错误仍使用 `failed to remove {database_label} directory ...` 前缀，并通过 `render_log_friendly_path` 输出路径。
- 新增 `skill_database_cleanup_invalid_target_path_is_reported`，验证含内嵌 NUL 的 database root 会显式报错，而不是被当作目录缺失。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 目标验证：`cargo test skill_database_cleanup -- --nocapture` 通过，2 个数据库清理测试全部通过。
- 回归验证：`cargo test uninstall -- --nocapture` 通过，10 个卸载相关测试全部通过。
- 范围验证：`cargo test runtime::engine -- --nocapture` 通过，179 个 runtime engine 范围测试全部通过。
- 全量验证：`cargo test` 通过，397 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 空白审核：`git diff --check -- src/runtime/engine.rs src/runtime/engine/tests.rs` 通过。
- 搜索复查：`remove_skill_database_dir` 中已不再存在 `database_dir.exists()`；现在只保留直接 `remove_dir_all` 与 NotFound 分支。

### 代码审核与遗留事项

- 修改部分代码审核确认没有引入候选路径、多来源兼容、静默 fallback、历史兼容分支或额外状态。
- 本轮没有改变卸载主流程、数据库根推导、结果字段写入、lancedb/sqlite label 或 warning 拼接策略。
- 缺失目录仍按“请求删除但无需删除”处理；非法路径、权限错误、非目录占位等删除失败会显式进入 warning/error 通道。
- 后续循环可继续排查 `src/runtime/engine.rs` 中 `prepare_managed_node_import_root` 的 import root 清理，以及其它运行时文件系统 helper 的 `.exists()` 折叠点。

## 2026-07-06 第188轮：受管 Node import root 清理探测显式化

### 探索记录

- 本轮从 `invoke_managed_node` 追踪到 `prepare_managed_node_import_root`：Node 调用前先解析受管环境 plan，确认 env ready，解析 skill 文件，然后准备 `plan.env_dir/.luaskills-skill` 作为 Node ESM bare import 根。
- 继续追到 `copy_managed_node_skill_import_root`：准备阶段会先清理旧 import root，再把当前 skill 目录复制到 `.luaskills-skill`，并跳过 skill 内的 `node_modules`。
- 问题确认：原实现先用 `import_root.exists()` 判断是否需要清理；当 `.luaskills-skill` 元数据探测失败时，`.exists()` 会把错误折叠为 false，随后直接进入复制流程。
- 继续确认同一分支的 `import_root.is_dir()`：它会在符号链接分支跟随目标并折叠元数据错误，可能把“符号链接目标不可探测”误判为“不是目录”。
- 路径来源确认：import root 唯一来源为 `plan.env_dir.join(".luaskills-skill")`，不属于候选路径或历史兼容结构；探测失败必须显式阻止后续复制。

### 核心修复与调整概述

- 新增 `remove_managed_node_import_root_if_present(import_root: &Path) -> Result<(), String>`，统一处理旧 import root 清理。
- 新增 `managed_node_import_root_should_remove_as_file(import_root: &Path, metadata: &fs::Metadata) -> Result<bool, String>`，显式判断符号链接应按文件删除还是按目录删除。
- `prepare_managed_node_import_root` 不再直接调用 `import_root.exists()`。
- 符号链接分支不再使用 `import_root.is_dir()` 隐式折叠探测错误。

### 文件变更清单

- 修改：`src/runtime/engine.rs`，调整受管 Node import root 准备阶段的旧根清理逻辑。
- 修改：`src/runtime/engine/tests.rs`，新增非法 import root 路径显式错误测试。
- 修改：`TASK_LOG.md`，追加本轮任务记录。

### 关键代码调整详情

- `remove_managed_node_import_root_if_present` 使用 `fs::symlink_metadata` 作为第一层事实来源。
- `symlink_metadata` 返回 `NotFound` 时视为无需清理，保持旧根缺失时可继续复制的语义。
- `symlink_metadata` 返回其它错误时返回 `Failed to inspect ...`，阻止后续复制。
- `managed_node_import_root_should_remove_as_file` 对非符号链接返回 false，继续走目录删除。
- 对符号链接目标使用 `fs::metadata` 显式跟随检查：目标为目录时按目录清理，目标缺失时按文件清理 dangling symlink，其它探测错误显式返回。
- 新增 `managed_node_import_root_invalid_existing_path_is_reported`，验证含内嵌 NUL 的 env dir 会在 import root 探测阶段显式失败。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 目标验证：`cargo test managed_node_import_root -- --nocapture` 通过，3 个 import root 目标测试全部通过。
- 范围验证：`cargo test managed_runtime -- --nocapture` 通过，15 个 managed runtime 相关测试全部通过。
- 范围验证：`cargo test managed_node -- --nocapture` 通过，3 个 managed Node 相关测试全部通过。
- 范围验证：`cargo test runtime::engine -- --nocapture` 通过，180 个 runtime engine 范围测试全部通过。
- 全量验证：`cargo test` 通过，398 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 空白审核：`git diff --check -- src/runtime/engine.rs src/runtime/engine/tests.rs` 通过。
- 搜索复查：`prepare_managed_node_import_root` 相关路径中已不再存在 `import_root.exists()` 或 `import_root.is_dir()`。

### 代码审核与遗留事项

- 修改部分代码审核确认没有引入候选路径、多来源兼容、静默 fallback、历史兼容分支或额外状态。
- 本轮没有改变 Node 运行时 plan 解析、env ready 检查、skill 文件解析、复制过滤 `node_modules` 或 worker payload 构造。
- 旧 import root 缺失仍允许继续复制；只有 import root 或其符号链接目标无法探测时显式报错。
- 后续循环可继续排查 `copy_managed_node_skill_import_root` 中递归复制时的 `source_path.is_dir()` / `source_path.is_file()` 折叠点，或其它 `runtime/engine.rs` 文件系统 helper。

## 2026-07-06 第189轮：受管 Node import root 递归复制类型判定显式化

### 探索记录

- 本轮继续从 `invoke_managed_node` 的 import root 准备流程追踪到 `copy_managed_node_skill_import_root`。
- 执行流确认：`prepare_managed_node_import_root` 清理旧 `.luaskills-skill` 后调用 `copy_managed_node_skill_import_root(skill_dir, import_root)`，递归复制当前 skill 目录，并跳过顶层或子目录中的 `node_modules` 项。
- 问题确认：原实现对每个 `read_dir` entry 重新调用 `source_path.is_dir()` / `source_path.is_file()`；这些 API 会把元数据探测错误折叠为 false，使无法分类的源目录项被静默跳过。
- 进一步确认：符号链接、FIFO 等非普通文件/目录类型在旧逻辑下可能被跟随、被跳过，或者生成不完整 import root；对 Node 执行入口来说，这会延迟到 worker 阶段才表现为模块缺失。
- 路径来源确认：`source_path` 唯一来自当前 `read_dir(source)` 的真实 entry，`destination_path` 唯一来自 `destination.join(entry.file_name())`，不存在候选路径或多来源兼容需求。

### 核心修复与调整概述

- `copy_managed_node_skill_import_root` 改为使用 `entry.file_type()` 作为目录项类型的权威事实来源。
- `entry.file_type()` 失败时返回 `Failed to inspect ... under ...`，不再静默跳过该目录项。
- 目录继续递归复制，普通文件继续 `fs::copy`。
- 非目录且非普通文件的目录项现在返回 `unsupported file type`，避免生成不完整 import root。

### 文件变更清单

- 修改：`src/runtime/engine.rs`，替换受管 Node import root 递归复制中的 `source_path.is_dir()` / `source_path.is_file()`。
- 修改：`src/runtime/engine/tests.rs`，新增 symlink 拒绝测试，并在 Unix 上新增 FIFO 拒绝测试。
- 修改：`TASK_LOG.md`，追加本轮任务记录。

### 关键代码调整详情

- 每个 `read_dir` entry 在过滤 `node_modules` 后先读取 `entry.file_type()`。
- `file_type.is_dir()` 时递归调用 `copy_managed_node_skill_import_root`。
- `file_type.is_file()` 时执行原有 `fs::copy` 并保留原错误消息前缀。
- 其它类型返回 `Failed to copy ... unsupported file type`。
- 新增 `managed_node_import_root_copy_rejects_symlink_entry`，验证符号链接目录项不会被跟随或静默跳过。
- 新增 Unix-only `managed_node_import_root_copy_rejects_unsupported_unix_file_type`，验证 FIFO 会被显式拒绝。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 目标验证：`cargo test managed_node_import_root -- --nocapture` 通过，当前平台 4 个 import root 目标测试全部通过。
- 范围验证：`cargo test managed_node -- --nocapture` 通过，4 个 managed Node 相关测试全部通过。
- 范围验证：`cargo test managed_runtime -- --nocapture` 通过，15 个 managed runtime 相关测试全部通过。
- 范围验证：`cargo test runtime::engine -- --nocapture` 通过，181 个 runtime engine 范围测试全部通过。
- 全量验证：`cargo test` 通过，399 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 空白审核：`git diff --check -- src/runtime/engine.rs src/runtime/engine/tests.rs` 通过。
- 搜索复查：`copy_managed_node_skill_import_root` 中已不再存在 `source_path.is_dir()` 或 `source_path.is_file()`。

### 代码审核与遗留事项

- 修改部分代码审核确认没有引入候选路径、多来源兼容、静默 fallback、历史兼容分支或额外状态。
- 本轮没有改变 import root 清理、`node_modules` 跳过规则、普通文件复制、目录递归或 Node worker payload 构造。
- Symlink 和其它特殊文件类型现在会显式失败；这是长期更稳的语义，避免 import root 复制越界或静默缺文件。
- 后续循环可继续排查 `src/runtime/engine.rs` 中其它 Lua-facing filesystem helper 的 `.exists()` / `.is_dir()` 折叠点。

## 2026-07-06 第190轮：vulcan.fs.mkdir 目标状态探测显式化

### 探索记录

- 本轮从 Lua-facing `vulcan.fs.mkdir` helper 追踪目录创建执行流：Lua 参数经 `require_path_arg` 和 `parse_vulcan_fs_recursive_option` 校验后，helper 根据目标路径状态决定返回 false、报错或创建目录。
- 原语义确认：目标已存在且是目录时返回 `false`；目标已存在但不是目录时报 `target already exists and is not a directory`；目标缺失时根据 recursive 参数调用 `create_dir_all` 或 `create_dir` 并返回 `true`。
- 问题确认：原实现使用 `target_path.exists()` 后再用 `target_path.is_dir()` 区分类型；两者都会折叠底层元数据探测错误，使不可探测目标可能被当作缺失路径进入创建流程，或被误判为非目录。
- 符号链接边界确认：旧语义会跟随符号链接判断是否为目录；dangling symlink 在 `.exists()` 下会像缺失一样落到创建流程。长期语义应明确区分“路径条目不存在”和“路径条目存在但未解析为目录”。
- 路径来源确认：mkdir target 唯一来自单个 Lua 参数 `path`，不存在候选路径、多来源或版本兼容需求。

### 核心修复与调整概述

- 新增 `VulcanFsMkdirTargetStatus`，将 mkdir 目标状态拆成 `Missing`、`ExistingDirectory` 和 `ExistingNonDirectory`。
- 新增 `vulcan_fs_mkdir_target_status(path: &Path) -> Result<VulcanFsMkdirTargetStatus, String>`，统一处理 mkdir 创建前目标探测。
- `vulcan.fs.mkdir` 改为基于状态 helper 分支，不再直接调用 `target_path.exists()` 或 `target_path.is_dir()`。
- 元数据探测失败现在返回 `fs.mkdir: failed to inspect ...`，不会继续创建目录。

### 文件变更清单

- 修改：`src/runtime/engine.rs`，新增 mkdir 目标状态枚举与探测 helper，并替换 `fs.mkdir` 调用点的直接 exists/is_dir 判定。
- 修改：`src/runtime/engine/tests.rs`，新增非法 mkdir 目标探测错误测试。
- 修改：`TASK_LOG.md`，追加本轮任务记录。

### 关键代码调整详情

- `vulcan_fs_mkdir_target_status` 先用 `fs::symlink_metadata` 检查路径条目本身。
- `symlink_metadata` 返回 `NotFound` 时返回 `Missing`，保留缺失路径可创建语义。
- `symlink_metadata` 返回其它错误时返回 `fs.mkdir: failed to inspect ...`。
- 非符号链接目录返回 `ExistingDirectory`，非符号链接非目录返回 `ExistingNonDirectory`。
- 符号链接会通过 `fs::metadata` 显式跟随目标：目标为目录返回 `ExistingDirectory`，目标缺失或非目录返回 `ExistingNonDirectory`，其它跟随探测错误显式返回。
- 新增 `vulcan_fs_mkdir_target_status_reports_invalid_target_probe`，验证非法路径不会被当成缺失目录。

### 验证记录

- 修改后：`cargo fmt` 通过。
- 目标验证：`cargo test fs_mkdir -- --nocapture` 通过，2 个 mkdir 目标测试全部通过。
- 范围验证：`cargo test vulcan_fs -- --nocapture` 通过，17 个 vulcan fs 相关测试全部通过。
- 范围验证：`cargo test runtime::engine -- --nocapture` 通过，182 个 runtime engine 范围测试全部通过。
- 全量验证：`cargo test` 通过，400 个测试全部通过。
- 静态验证：`cargo clippy --all-targets -- -D warnings` 通过，当前无 Clippy 警告。
- 空白审核：`git diff --check -- src/runtime/engine.rs src/runtime/engine/tests.rs` 通过。
- 搜索复查：`fs.mkdir` 生产调用点已不再存在 `target_path.exists()` 或 `target_path.is_dir()`；当前命中仅为测试断言。

### 代码审核与遗留事项

- 修改部分代码审核确认没有引入候选路径、多来源兼容、静默 fallback、历史兼容分支或额外状态。
- 本轮没有改变 `fs.mkdir` 参数解析、recursive 创建策略、已存在目录返回 false、已存在非目录报错或创建成功返回 true 的对外语义。
- Dangling symlink 现在明确归类为已存在非目录，而不是像缺失路径一样进入创建流程。
- 后续循环可继续排查 `src/runtime/engine.rs` 中 `fs.remove`、`fs.copy` 或进程查找相关 helper 的剩余文件系统探测折叠点。

## 2026-07-06 第191轮：运行时 package 路径配置根探测显式化

### 探索记录

- 本轮从 `LuaEngine::create_lua_vm` 与 `LuaEngine::create_lua_vm_with_host_options` 追踪到 `setup_package_paths`，确认每个池化 Lua VM 初始化时都会先配置 `package.path` 与 `package.cpath`。
- 路径来源确认：`lua_packages_dir` 唯一来自 `LuaRuntimeHostOptions::lua_packages_dir`，`host_provided_ffi_root` 唯一来自 `LuaRuntimeHostOptions::host_provided_ffi_root`；二者都是宿主显式配置，不存在多来源兼容需求。
- 执行语义确认：未配置 `lua_packages_dir` 时直接跳过；`lua_packages_dir` 已确认缺失时保持跳过；存在且可用时才将 `lua_packages/share/lua` 与 `lua_packages/lib/lua` 写入 Lua 搜索链；可选 FFI 根目录只用于补充 native module 搜索路径。
- 问题确认：原实现使用 `lua_packages.exists()` 和 `.filter(|root| root.exists())`，会把非法路径、权限错误或其它 metadata 探测失败折叠成 `false`，导致已配置但不可探测的搜索根被静默跳过。
- 长期语义确认：可加入搜索链的配置根必须是目录；文件路径或其它非目录条目不应被拼入 `package.path` / `package.cpath`。

### 核心修复与调整概述

- 新增 `configured_package_search_directory_exists`，统一检查配置型 package 搜索目录。
- 缺失目录仍返回 `false`，保持“没有可用包根则跳过”的现有初始化语义。
- metadata 探测失败现在返回显式错误，不再被当成缺失路径。
- 已配置路径存在但不是目录时现在返回显式配置错误，避免文件路径进入 Lua module 搜索链。
- `setup_package_paths` 改为通过该 helper 检查 `lua_packages_dir` 与 `host_provided_ffi_root`。

### 文件变更清单

- 修改：`src/runtime/engine.rs`，新增配置目录探测 helper，并替换 `setup_package_paths` 中的 `.exists()` 折叠点。
- 修改：`src/runtime/engine/tests.rs`，新增 package 路径初始化的非法路径与非目录配置回归测试。
- 修改：`TASK_LOG.md`，追加本轮任务记录。

### 关键代码调整详情

- `configured_package_search_directory_exists(path, option_name)` 使用 `fs::metadata` 作为权威事实来源。
- `metadata.is_dir()` 为 true 时返回 `Ok(true)`；`NotFound` 返回 `Ok(false)`；非目录返回 `configured {option_name} is not a directory`。
- 非 `NotFound` 的 metadata 错误返回 `failed to inspect configured {option_name} ...`，并保留 `render_log_friendly_path` 渲染后的路径。
- `setup_package_paths` 在 `lua_packages_dir` 不存在时继续 `Ok(())`；在探测失败或非目录时通过 `?` 中断 VM 初始化。
- `host_provided_ffi_root` 的分支改为显式 `match + if`，只在确认目录存在时返回 `Some(root.as_path())`，缺失时不追加 FFI cpath。
- 新增 `setup_package_paths_reports_invalid_lua_packages_dir_probe_errors`，覆盖非法 `lua_packages_dir` 不再被当成缺失路径。
- 新增 `setup_package_paths_reports_invalid_host_provided_ffi_root_probe_errors`，覆盖非法 FFI 根目录不再被静默排除。
- 新增 `setup_package_paths_rejects_non_directory_lua_packages_dir`，覆盖文件路径不能进入 package 搜索链。

### 验证记录

- 格式化：`cargo fmt` 通过。
- 目标验证：`cargo test setup_package_paths -- --nocapture` 通过，3 个本轮新增测试全部通过。
- 范围验证：`cargo test runtime::engine -- --nocapture` 通过，185 个 runtime engine 范围测试全部通过。
- 全量验证：`cargo test` 通过，403 个测试全部通过。
- 静态验证：`cargo clippy --all-targets --all-features -- -D warnings` 通过，无 Clippy 警告。
- 空白审核：`git diff --check -- src/runtime/engine.rs src/runtime/engine/tests.rs` 通过。
- 搜索复查：`src/runtime/engine.rs` 中已无 `lua_packages.exists()`，也无针对 `host_provided_ffi_root` 的 `.exists()` 过滤残留。

### 代码审核与遗留事项

- 修改部分代码审核确认没有引入候选路径、多来源兼容、静默 fallback、历史兼容分支或额外状态。
- 本轮没有改变 `package.path` / `package.cpath` 的拼接模板、平台后缀规则、Lua 默认搜索链前置策略或 VM 池创建流程。
- 缺失的可选 FFI 根目录仍会被跳过；但非法、不可探测或非目录的已配置搜索根会显式失败。
- 后续循环可继续排查 `src/runtime/engine.rs` 中剩余 Lua-facing filesystem helper、进程查找 helper 或测试夹具清理中的 `.exists()` / `.is_dir()` 折叠点。

## 2026-07-06 第192轮：受管运行时文件与依赖清单探测显式化

### 探索记录

- 本轮从 `invoke_managed_python` 与 `invoke_managed_node` 追踪受管运行时调用链：Lua 请求解析后，运行时先读取当前 skill 的 `dependencies.yaml`，解析受管 runtime plan，确保环境可用，再解析请求中的 skill-relative `file`。
- 继续追踪 status API：`managed_python_status` 与 `managed_node_status` 通过 `load_optional_current_managed_runtime_manifest` 读取可选 `dependencies.yaml`，缺失时返回未配置状态。
- 路径来源确认：受管 runtime handler 文件唯一来源为当前 `skill_dir` 加单个已校验的安全相对路径；`dependencies.yaml` 唯一来源为当前 `skill_dir.join("dependencies.yaml")`。
- 问题确认：`resolve_managed_runtime_skill_file` 使用 `resolved.is_file()`，`load_current_managed_runtime_manifest` 与 `load_optional_current_managed_runtime_manifest` 使用 `dependencies_path.is_file()`；这些 API 会把 metadata 探测失败折叠为 `false`。
- 长期语义确认：handler 路径和 `dependencies.yaml` 都必须是文件；目录占位或不可探测路径不应被当作缺失文件静默处理。

### 核心修复与调整概述

- 将 `skill_dependency_manifest_path_exists` 从单纯 `try_exists()` 提升为显式 `fs::metadata` 文件判定。
- 新增 `managed_runtime_skill_file_is_file`，专门检查受管 runtime handler 文件是否为普通文件。
- 缺失 handler 文件仍返回原有 `file not found` 语义；探测失败和非文件占位改为显式错误。
- required/optional 受管 runtime manifest 加载统一复用 `skill_dependency_manifest_path_exists`，并带上对应 Lua API 名称。
- `LuaEngine::load_skill_dependency_manifest` 也继承同一个 manifest 文件判定，目录型 `dependencies.yaml` 不再进入 YAML 读取或被当作缺失。

### 文件变更清单

- 修改：`src/runtime/engine.rs`，调整 dependency manifest 判定、新增受管 runtime handler 文件判定，并替换三个 `.is_file()` 折叠点。
- 修改：`src/runtime/engine/tests.rs`，新增受管 runtime handler、required/optional manifest、目录型 dependency manifest 回归测试。
- 修改：`TASK_LOG.md`，追加本轮任务记录。

### 关键代码调整详情

- `skill_dependency_manifest_path_exists` 现在使用 `fs::metadata`：文件返回 `Ok(true)`，`NotFound` 返回 `Ok(false)`，非文件返回 `dependency manifest is not a file`，其它错误返回 `failed to inspect dependency manifest`。
- `managed_runtime_skill_file_is_file(path, api_name, field_name)` 对 handler 源文件执行同样的 metadata 判定，并把错误映射成 Lua runtime error。
- `resolve_managed_runtime_skill_file` 只在 helper 返回 `Ok(false)` 时保留原有 `{field_name} not found`；探测失败或非文件路径直接返回显式错误。
- `load_current_managed_runtime_manifest` 在报告 `dependencies.yaml is required for managed runtimes` 前先保留探测错误。
- `load_optional_current_managed_runtime_manifest` 现在区分清单缺失和探测失败，status API 不再把非法路径折叠为未配置。
- 新增 `managed_runtime_skill_file_reports_probe_errors` 与 `managed_runtime_skill_file_rejects_directory_source_path`。
- 新增 `load_current_managed_runtime_manifest_reports_probe_errors` 与 `load_optional_current_managed_runtime_manifest_reports_probe_errors`。
- 新增 `load_skill_dependency_manifest_rejects_directory_manifest_path`。

### 验证记录

- 格式化：`cargo fmt` 通过。
- 目标验证：`cargo test managed_runtime_skill_file -- --nocapture` 通过，3 个匹配测试全部通过。
- 目标验证：`cargo test managed_runtime_manifest -- --nocapture` 通过，2 个本轮 manifest 探测测试通过。
- 目标验证：`cargo test load_skill_dependency_manifest -- --nocapture` 通过，2 个 dependency manifest 测试通过。
- 范围验证：`cargo test runtime::engine -- --nocapture` 通过，190 个 runtime engine 范围测试全部通过。
- 全量验证：`cargo test` 通过，408 个测试全部通过。
- 静态验证：`cargo clippy --all-targets --all-features -- -D warnings` 通过，无 Clippy 警告。
- 空白审核：`git diff --check -- src/runtime/engine.rs src/runtime/engine/tests.rs TASK_LOG.md` 通过。
- 搜索复查：`src/runtime/engine.rs` 中已无本轮目标 `resolved.is_file()` 与 `dependencies_path.is_file()` 直接折叠点。

### 代码审核与遗留事项

- 修改部分代码审核确认没有引入候选路径、多来源兼容、静默 fallback、历史兼容分支或额外状态。
- 本轮没有改变受管 runtime 请求字段校验、runtime plan 解析、环境准备、worker payload 构造或 status 响应结构。
- 缺失 `dependencies.yaml` 在 optional/status 路径仍返回未配置；required invoke 路径仍返回 required 错误；只有探测失败和非文件占位改为显式失败。
- 后续循环可继续排查 `src/runtime/engine.rs` 中 `fs.remove`、`fs.copy`、进程查找或测试夹具清理里的剩余 `.exists()` / `.is_dir()` / `.is_file()` 折叠点。
## 2026-07-06 第193轮：必需 skill 文件类型判定显式化

### 探索记录

- 本轮先按上一轮遗留方向复核 Lua-facing `fs.remove` 与 `fs.copy`：`fs.remove` 已使用 `fs::symlink_metadata` 读取目标条目类型，`fs.copy` 的 source/destination 分支也已经通过显式 metadata 与 `path_entry_exists` 保留探测错误，未发现新的直接折叠点。
- 随后继续搜索 `src/runtime/engine.rs` 中剩余 `.try_exists()`，锁定 `required_skill_file_path_exists`：该 helper 的命名与注释都声明检查“必需 skill 文件”，但实现只做 `path.try_exists()`，会把目录型 `skill.yaml` 或目录型 Lua 入口当作“存在”放行。
- 调用链确认：`load_single_skill` 先构造 `dir.join("skill.yaml")` 并调用 `required_skill_file_path_exists`；读取 YAML 后校验 `entry.lua_entry`，再通过 `tool_entry_path(dir, tool)` 构造 Lua 入口路径，并再次调用同一个 helper，之后才进入编译。
- 路径来源确认：`skill.yaml` 唯一来源是当前 skill 目录固定拼接；Lua 入口唯一来源是已通过 `validate_skill_relative_path` 校验的清单字段 `tool.lua_entry`。不存在候选路径、多来源兼容或历史 fallback 需求。
- 问题确认：旧实现只区分“存在/缺失/探测失败”，没有验证存在的路径是否为普通文件；目录占位会延迟到 YAML 读取或 Lua 编译阶段才失败，错误语义不精确，也违背“必需文件”契约。

### 核心修复与调整概述

- `required_skill_file_path_exists` 改为使用 `fs::metadata` 作为事实来源，并通过 `metadata.is_file()` 明确要求普通文件。
- 缺失路径仍返回 `Ok(false)`，保留原有 `skill.yaml not found` 与 `Lua entry ... not found` 语义。
- 非文件路径现在返回 `{file_label} is not a file for skill ...`，使目录占位、特殊文件等配置错误在加载前置阶段显式暴露。
- 非 `NotFound` 的 metadata 探测错误继续返回 `failed to inspect ...`，保持已有非法路径探测错误测试语义。

### 文件变更清单

- 修改：`src/runtime/engine.rs`，收紧 `required_skill_file_path_exists` 的文件类型判定。
- 修改：`src/runtime/engine/tests.rs`，新增目录型 `skill.yaml` 与目录型 Lua 入口的回归测试。
- 修改：`TASK_LOG.md`，追加本轮任务记录。

### 关键代码调整详情

- `required_skill_file_path_exists(path, file_label, skill_dir)` 现在匹配 `fs::metadata(path)`：普通文件返回 `Ok(true)`，`NotFound` 返回 `Ok(false)`，非普通文件返回显式类型错误，其它探测错误返回原有 `failed to inspect ...` 风格错误。
- 新增 `load_single_skill_rejects_directory_skill_yaml`，验证目录占用 `skill.yaml` 时不会进入 YAML 读取流程。
- 新增 `load_single_skill_rejects_directory_lua_entry`，验证清单声明的 `runtime/run.lua` 为目录时不会进入 Lua 编译流程。
- 测试中 Lua 入口期望路径按真实来源 `skill_dir.join("runtime/run.lua")` 构造，保持与清单字段 join 后的路径渲染一致，避免用分段 join 引入等价但不同文本的断言。

### 验证记录

- 格式化：`cargo fmt` 通过。
- 目标验证：`cargo test load_single_skill -- --nocapture` 通过，7 个匹配测试全部通过。
- 范围验证：`cargo test runtime::engine -- --nocapture` 通过，192 个 runtime engine 范围测试全部通过。
- 全量验证：`cargo test` 通过，410 个测试全部通过。
- 静态验证：`cargo clippy --all-targets --all-features -- -D warnings` 通过，无 Clippy 警告。
- 空白审核：`git diff --check -- src/runtime/engine.rs src/runtime/engine/tests.rs TASK_LOG.md` 通过。
- 搜索复查：`required_skill_file_path_exists` 已无 `path.try_exists()`；本轮只剩其它既有 helper 的 `.try_exists()` 命中，未混入本轮目标路径。

### 代码审核与遗留事项

- 修改部分代码审核确认没有引入候选路径、多来源兼容、静默 fallback、历史兼容分支或额外状态。
- 本轮没有改变 skill 根目录发现、清单字段解析、entry 名称校验、Lua module 校验或编译注册流程。
- 缺失 `skill.yaml` 与缺失 Lua 入口仍保持原有 not found 行为；只有路径存在但不是普通文件时改为显式配置错误。
- 本轮目标测试首次失败暴露出断言路径没有沿清单字段真实来源构造，已修正并复测通过。
- 后续循环可继续排查 `src/runtime/engine.rs` 中剩余 `packaged_runtime_path_exists`、`runtime_skill_root_dir_exists` 等 `.try_exists()` helper 是否存在同类类型语义缺失或探测折叠问题。
## 2026-07-06 第194轮：打包运行时清单目标类型校验显式化

### 探索记录

- 本轮继续扫描 `src/runtime/engine.rs` 中剩余 `.try_exists()` helper，先对比 `packaged_runtime_path_exists` 与 `runtime_skill_root_dir_exists`。
- `runtime_skill_root_dir_exists` 当前只用于判断是否至少存在一个 skill 根；后续 skill 实例收集仍会进入独立目录扫描流程。本轮未选择它作为优先修复点。
- `packaged_runtime_path_exists` 位于打包运行时资源校验链：`load_from_roots` 先调用 `validate_packaged_runtime_resources`，再进入 `validate_packaged_runtime_packages_layout`，该函数用 `lua-runtime-manifest.json` 作为 marker，并读取 `luaskills-packages-manifest.json` 中声明的资源路径。
- 路径来源确认：marker 路径唯一来源是 `resources_dir.join("lua-runtime-manifest.json")`；packages 清单唯一来源是 `resources_dir.join("luaskills-packages-manifest.json")`；manifest 内字段唯一来源是已解析的 `RuntimePackagesManifestPaths`，再通过 `runtime_root.join(relative_path)` 解析。
- 类型契约确认：`lua-runtime-manifest.json` 与 `luaskills-packages-manifest.json` 必须是文件；`install_manifest`、`compat_lua_packages_txt`、`platform_support`、`third_party_licenses`、`third_party_notices`、`help_index`、`license_index` 必须是文件；`package_help_root` 与 `module_help_root` 必须是目录。
- 问题确认：旧 helper 只判断“存在”，导致目录型 marker、目录型 packages 清单、目录占位文件目标、文件占位目录目标都能通过布局校验或延迟到后续读取阶段才失败，错误语义不精确。

### 核心修复与调整概述

- 新增 `PackagedRuntimeTargetKind`，将打包运行时清单目标明确分为 `File` 与 `Directory`。
- 将 `packaged_runtime_path_exists` 替换为 `packaged_runtime_target_exists`，统一执行 metadata 探测与类型校验。
- 缺失路径仍返回 `Ok(false)`，保持 marker 缺失时不启用包布局校验、声明目标缺失时报 `missing ...` 的既有语义。
- metadata 探测失败仍返回 `failed to inspect ...`；路径存在但类型不匹配时返回 `... is not a file/directory ...`。

### 文件变更清单

- 修改：`src/runtime/engine.rs`，新增打包运行时目标类型枚举，替换存在性 helper，并为每个 manifest 字段标明期望文件系统类型。
- 修改：`src/runtime/engine/tests.rs`，新增 marker、packages manifest、声明文件目标、声明目录目标的类型错误回归测试。
- 修改：`TASK_LOG.md`，追加本轮任务记录。

### 关键代码调整详情

- `PackagedRuntimeTargetKind::matches_metadata` 使用 `metadata.is_file()` 或 `metadata.is_dir()` 判断实际类型。
- `PackagedRuntimeTargetKind::diagnostic_noun` 统一输出 `file` / `directory`，避免错误文案在不同调用点分散。
- `validate_packaged_runtime_target(runtime_root, label, relative_path, expected_kind)` 现在不仅校验 runtime-relative 路径合法性，还要求目标类型与 manifest 字段契约一致。
- `validate_packaged_runtime_packages_layout` 对顶层 marker 和 packages 清单直接调用 `packaged_runtime_target_exists(..., File)`。
- 新增 `load_from_roots_rejects_packaged_runtime_directory_marker_file`，覆盖目录占位 `lua-runtime-manifest.json`。
- 新增 `load_from_roots_rejects_packaged_runtime_directory_packages_manifest`，覆盖目录占位 `luaskills-packages-manifest.json`。
- 新增 `load_from_roots_rejects_packaged_runtime_declared_file_as_directory`，覆盖 manifest 声明文件字段被目录占位。
- 新增 `load_from_roots_rejects_packaged_runtime_declared_directory_as_file`，覆盖 manifest 声明目录字段被文件占位。

### 验证记录

- 格式化：`cargo fmt` 通过。
- 目标验证：`cargo test packaged_runtime -- --nocapture` 通过，8 个匹配测试全部通过。
- 范围验证：`cargo test runtime::engine -- --nocapture` 通过，196 个 runtime engine 范围测试全部通过。
- 全量验证：`cargo test` 通过，414 个测试全部通过。
- 静态验证：`cargo clippy --all-targets --all-features -- -D warnings` 通过，无 Clippy 警告。
- 空白审核：`git diff --check -- src/runtime/engine.rs src/runtime/engine/tests.rs TASK_LOG.md` 通过。
- 搜索复查：旧 `packaged_runtime_path_exists` 已无命中；所有 `RuntimePackagesManifestPaths` 字段都已在校验调用中明确标注 `File` 或 `Directory`。

### 代码审核与遗留事项

- 修改部分代码审核确认没有引入候选路径、多来源兼容、静默 fallback、历史兼容分支或额外状态。
- 本轮没有改变 packaged runtime marker 的启用条件、manifest JSON 结构、相对路径安全校验、skill 根加载顺序或运行时资源目录推导规则。
- marker 缺失仍表示未检测到打包运行时布局；声明目标缺失仍返回原有 missing 错误；只有路径存在但类型不符合 manifest 契约时改为显式布局错误。
- 本轮两个测试首次失败都来自断言路径未沿 manifest 字段真实来源构造，已改为 `runtime_root.join("resources/...")` 形式并复测通过。
- 后续循环可继续排查 `runtime_skill_root_dir_exists` 是否需要从“存在路径”收紧为“存在目录”，以及其它 `.exists()` / `.is_dir()` / `.is_file()` 调用点是否仍有类型语义缺失。
## 2026-07-06 第195轮：运行时 skill 根目录类型校验显式化

### 探索记录

- 本轮沿第194轮遗留候选继续追踪 `runtime_skill_root_dir_exists`，确认它位于 `LuaEngine::load_from_roots` 的预探测阶段，用于判断是否至少存在一个可加载 skill 根。
- 继续追踪到 skill manager 的 `collect_effective_skill_instances_from_roots`：运行时预探测通过后，会调用 `collect_named_skill_dirs` 扫描每个 `RuntimeSkillRoot.skills_dir`。
- 问题进一步确认：`collect_named_skill_dirs` 内部还有 `skill_root_path_exists`，同样只使用 `try_exists()` 判断路径是否存在；当 skill root 被文件占位时，会继续进入 `fs::read_dir`，再以较低层的 read_dir 错误失败。
- 路径来源确认：运行时入口中的 skill root 唯一来源是宿主传入的 `RuntimeSkillRoot.skills_dir`；skill manager 中的 root 也是同一结构字段，不存在候选路径、多来源兼容或历史 fallback 需求。
- 语义确认：skill root 的契约是目录；缺失根可以被视为空根，但存在且不是目录的路径应立即作为配置错误暴露。

### 核心修复与调整概述

- 将 runtime 侧 `runtime_skill_root_dir_exists` 重命名并收紧为 `runtime_skill_root_dir_is_directory`。
- 将 skill manager 侧 `skill_root_path_exists` 重命名并收紧为 `skill_root_path_is_directory`。
- 两个 helper 都改用 `fs::metadata`：目录返回 `true`，`NotFound` 返回 `false`，非目录返回显式类型错误，其它 metadata 错误返回原有探测错误。
- 缺失 skill root 仍保持“空根”语义；只有文件、特殊文件等非目录占位变为明确错误。

### 文件变更清单

- 修改：`src/runtime/engine.rs`，收紧 `load_from_roots` 预探测阶段的 skill root 目录判定。
- 修改：`src/runtime/engine/tests.rs`，新增 runtime 入口的文件型 skill root 回归测试。
- 修改：`src/skill/manager.rs`，收紧实际收集入口的 skill root 目录判定。
- 修改：`src/skill/manager/tests.rs`，新增 skill manager 入口的文件型 skill root 回归测试。
- 修改：`TASK_LOG.md`，追加本轮任务记录。

### 关键代码调整详情

- `runtime_skill_root_dir_is_directory(root)` 现在对 `root.skills_dir` 使用 `fs::metadata`，非目录返回 `skill root 'ROOT' is not a directory: ...`。
- `any_runtime_skill_root_dir_exists` 的注释同步为“至少一个根目录存在”，避免文档继续表达任意路径存在即可。
- `skill_root_path_is_directory(root)` 现在对收集入口的 root 使用同样的目录判定，非目录返回 `Skill root is not a directory: ...`。
- `collect_named_skill_dirs` 只在确认 root 是目录后才执行 `fs::read_dir`，不再把文件型 root 延迟给目录遍历报错。
- 新增 `load_from_roots_rejects_file_skill_root`，覆盖 runtime 预探测入口。
- 新增 `collect_effective_skill_instances_rejects_file_skill_root`，覆盖 skill manager 收集入口。

### 验证记录

- 格式化：`cargo fmt` 通过。
- 目标验证：`cargo test file_skill_root -- --nocapture` 通过，2 个新增匹配测试全部通过。
- 目标验证：`cargo test skill_root_probe -- --nocapture` 通过，2 个既有非法路径探测测试通过。
- 目标验证：`cargo test collect_effective_skill_instances -- --nocapture` 通过，3 个收集相关测试通过。
- 范围验证：`cargo test runtime::engine -- --nocapture` 通过，197 个 runtime engine 范围测试全部通过。
- 范围验证：`cargo test skill::manager -- --nocapture` 通过，37 个 skill manager 范围测试全部通过。
- 全量验证：`cargo test` 通过，416 个测试全部通过。
- 静态验证：`cargo clippy --all-targets --all-features -- -D warnings` 通过，无 Clippy 警告。
- 空白审核：`git diff --check -- src/runtime/engine.rs src/runtime/engine/tests.rs src/skill/manager.rs src/skill/manager/tests.rs TASK_LOG.md` 通过。
- 搜索复查：旧 `skill_root_path_exists` 与 `runtime_skill_root_dir_exists` 已无残留；剩余 `try_exists()` 命中属于后续候选 helper，未混入本轮 root 路径。

### 代码审核与遗留事项

- 修改部分代码审核确认没有引入候选路径、多来源兼容、静默 fallback、历史兼容分支或额外状态。
- 本轮没有改变 ROOT/PROJECT/USER 优先级、空目录禁用语义、skill id 校验、manifest enable 过滤或 lifecycle reload 流程。
- 缺失 skill root 仍返回空集合或提前空加载；存在但不是目录的 root 现在会在 runtime 或 skill manager 入口显式失败。
- 后续循环可继续排查 `src/skill/manager.rs` 中剩余 `skill_manifest_path_exists`、`disabled_record_path_exists`、`install_record_path_exists`、`skill_package_dir_exists`、`staging_temp_root_exists` 等 `try_exists()` helper 的类型契约是否充分。
## 2026-07-06 第196轮：skill manager 清单启用探针文件类型显式化

### 探索记录

- 本轮继续排查 `src/skill/manager.rs` 中剩余 `try_exists()` helper，重点对比 `skill_manifest_path_exists`、`disabled_record_path_exists`、`install_record_path_exists`、`skill_package_dir_exists` 与 `staging_temp_root_exists`。
- 最终锁定 `skill_manifest_path_exists`：它只检查 `skill.yaml` 路径是否存在，但调用点 `is_skill_manifest_enabled` 随后会把该路径作为 YAML 文件读取。
- 调用链确认：`collect_effective_skill_instances_from_roots` 收集各 root 的 skill 目录后，对候选实例调用 `is_effective_disable_override` 和 `is_skill_manifest_enabled`；`is_skill_manifest_enabled` 构造 `skill_dir.join("skill.yaml")`，缺失时默认启用，存在时读取 YAML 并检查 `enable` 字段。
- 路径来源确认：该 `skill.yaml` 唯一来源是已解析 skill 目录固定拼接，不存在候选路径、多来源兼容或历史 fallback 需求。
- 问题确认：旧实现会把目录型 `skill.yaml` 当作存在，然后进入 `fs::read_to_string`，导致错误延迟到读取阶段；长期语义应在启用探针阶段就明确要求清单是普通文件。

### 核心修复与调整概述

- 将 `skill_manifest_path_exists` 重命名并收紧为 `skill_manifest_path_is_file`。
- 新 helper 使用 `fs::metadata`：普通文件返回 `true`，`NotFound` 返回 `false`，非文件返回显式类型错误，其它 metadata 错误返回原有探测错误。
- 缺失 `skill.yaml` 仍保持默认启用语义。
- 目录或特殊文件占位的 `skill.yaml` 现在返回 `Skill manifest is not a file: ...`，不再继续进入 YAML 读取。

### 文件变更清单

- 修改：`src/skill/manager.rs`，收紧 skill manifest enable 探针的文件类型判定。
- 修改：`src/skill/manager/tests.rs`，新增目录型 `skill.yaml` 的启用探针回归测试。
- 修改：`TASK_LOG.md`，追加本轮任务记录。

### 关键代码调整详情

- `skill_manifest_path_is_file(skill_yaml)` 现在基于 `fs::metadata` 判断 manifest 是否为普通文件。
- `is_skill_manifest_enabled` 继续在 helper 返回 `false` 时返回 `Ok(true)`，保持缺失 manifest 默认启用。
- 新增 `is_skill_manifest_enabled_rejects_directory_manifest`，验证目录型 `skill.yaml` 不会被当作缺失 manifest，也不会进入 YAML 读取流程。
- 既有 `is_skill_manifest_enabled_rejects_manifest_probe_errors` 继续验证非法路径探测错误不会被折叠为默认启用。

### 验证记录

- 格式化：`cargo fmt` 通过。
- 目标验证：`cargo test is_skill_manifest_enabled -- --nocapture` 通过，3 个匹配测试全部通过。
- 范围验证：`cargo test skill::manager -- --nocapture` 通过，38 个 skill manager 范围测试全部通过。
- 首次全量验证：`cargo test` 出现一次 runlua/process env 相关时序失败；首个失败用例 `execute_runlua_request_inline_reports_vulcan_process_exec_timeout_ms` 单独复跑通过。
- 全量复验：再次运行 `cargo test` 通过，417 个测试全部通过。
- 静态验证：`cargo clippy --all-targets --all-features -- -D warnings` 通过，无 Clippy 警告。
- 空白审核：`git diff --check -- src/skill/manager.rs src/skill/manager/tests.rs TASK_LOG.md` 通过。
- 搜索复查：旧 `skill_manifest_path_exists` 已无残留；本轮目标路径不再使用 `try_exists()`。

### 代码审核与遗留事项

- 修改部分代码审核确认没有引入候选路径、多来源兼容、静默 fallback、历史兼容分支或额外状态。
- 本轮没有改变 skill 实例收集顺序、override 空目录禁用语义、manifest `enable` 字段解析或缺失 manifest 默认启用策略。
- 目录型 manifest 现在是显式配置错误；缺失 manifest 与 metadata 探测失败的语义保持清晰区分。
- 后续循环可继续排查 `disabled_record_path_exists`、`install_record_path_exists`、`skill_package_dir_exists`、`staging_temp_root_exists` 的文件/目录类型契约。
## 2026-07-06 第197轮：skill manager 状态记录文件类型显式化

### 探索记录

- 本轮继续沿第196轮遗留候选排查 `src/skill/manager.rs` 中的 `disabled_record_path_exists` 与 `install_record_path_exists`。
- 调用链确认：`is_skill_enabled`、`enable_skill_in_plane`、`disabled_record`、`remove_disabled_record` 都通过 disabled 记录 helper 探测 `disabled_root/{skill_id}.json`；`install_record` 与 `remove_install_record` 通过 install 记录 helper 探测 `install_record_root/{skill_id}.yaml`。
- 路径来源确认：disabled 记录路径唯一来源是 `disabled_record_path(skill_id)`；install 记录路径唯一来源是 `install_record_path(skill_id)`。两者都由已校验 skill id 固定拼接扩展名，不存在候选路径、多来源兼容或历史 fallback 需求。
- 文件契约确认：disabled 记录是 JSON 文件，install 记录是 YAML 文件；缺失记录表示未停用或无安装记录，但路径存在且不是文件应视为状态目录损坏。
- 问题确认：旧 helper 只判断路径存在；目录型记录会延迟到 `read_to_string` 或 `remove_file` 才失败，错误语义不精确，也会让 `is_skill_enabled` 把目录型 disabled 记录误判为“已停用”。

### 核心修复与调整概述

- 将 `disabled_record_path_exists` 重命名并收紧为 `disabled_record_path_is_file`。
- 将 `install_record_path_exists` 重命名并收紧为 `install_record_path_is_file`。
- 两个 helper 都改用 `fs::metadata`：普通文件返回 `true`，`NotFound` 返回 `false`，非文件返回显式类型错误，其它 metadata 错误返回原有探测错误。
- 缺失记录仍保持原有 none/未停用语义；目录或特殊文件占位的记录路径现在明确失败。

### 文件变更清单

- 修改：`src/skill/manager.rs`，收紧 disabled/install 状态记录探测 helper 的文件类型判定，并同步调用点。
- 修改：`src/skill/manager/tests.rs`，新增目录型 disabled/install 记录回归测试。
- 修改：`TASK_LOG.md`，追加本轮任务记录。

### 关键代码调整详情

- `disabled_record_path_is_file(path)` 现在基于 `fs::metadata` 判断 JSON disabled 记录是否为普通文件。
- `install_record_path_is_file(path)` 现在基于 `fs::metadata` 判断 YAML install 记录是否为普通文件。
- `is_skill_enabled`、`enable_skill_in_plane`、`disabled_record`、`remove_disabled_record` 统一使用 `disabled_record_path_is_file`。
- `install_record` 与 `remove_install_record` 统一使用 `install_record_path_is_file`。
- 新增 `disabled_record_rejects_directory_record`，验证目录型 JSON 记录不会被当作已停用记录或进入 JSON 读取。
- 新增 `install_record_rejects_directory_record`，验证目录型 YAML 记录不会进入 YAML 读取。

### 验证记录

- 格式化：`cargo fmt` 通过。
- 目标验证：`cargo test disabled_record -- --nocapture` 通过，4 个匹配测试全部通过。
- 目标验证：`cargo test install_record -- --nocapture` 通过，3 个匹配测试全部通过。
- 范围验证：`cargo test skill::manager -- --nocapture` 通过，40 个 skill manager 范围测试全部通过。
- 全量验证：`cargo test` 通过，419 个测试全部通过。
- 静态验证：`cargo clippy --all-targets --all-features -- -D warnings` 通过，无 Clippy 警告。
- 空白审核：`git diff --check -- src/skill/manager.rs src/skill/manager/tests.rs TASK_LOG.md` 通过。
- 搜索复查：旧 `disabled_record_path_exists` 与 `install_record_path_exists` 已无残留；本轮目标路径不再使用 `try_exists()`。

### 代码审核与遗留事项

- 修改部分代码审核确认没有引入候选路径、多来源兼容、静默 fallback、历史兼容分支或额外状态。
- 本轮没有改变 disabled/install 记录的 JSON/YAML 格式、记录路径扩展名、缺失记录语义、序列化逻辑或生命周期提交/回滚流程。
- 目录型记录现在是显式状态损坏错误；缺失记录与 metadata 探测失败继续保持不同语义。
- 后续循环可继续排查 `skill_package_dir_exists` 与 `staging_temp_root_exists` 的目录类型契约，以及剩余 `.exists()` / `.is_dir()` / `.is_file()` 调用点。

## 2026-07-06 第198轮：skill manager 包目录类型校验显式化

### 探索记录

- 本轮沿第197轮遗留候选继续追踪 `src/skill/manager.rs` 中的 `skill_package_dir_exists`，确认它被安装、更新、卸载、提交清理与回滚恢复等生命周期路径共享。
- 调用链确认：`prepare_uninstall_skill_at_path_in_plane` 会先通过该 helper 判断包路径，再将当前包移动到 `uninstall_backup`；`stage_skill_install_from_archive` 会用它判断目标包路径是否已存在；`stage_skill_update_from_archive` 会用它判断已安装包是否存在后再移动到 `update_backup`。
- 回滚链路确认：`commit_prepared_skill_apply` 会用它判断 update backup 是否需要清理；`rollback_prepared_skill_apply` 与 `rollback_prepared_skill_uninstall` 会用它判断 staged target 与 backup 是否可删除或恢复。
- 路径来源确认：包路径唯一来自 `self.skill_root().join(skill_id)`、调用方显式传入的 `skill_dir`、或 prepared lifecycle 结构中已记录的 `target_dir` / `backup_dir`，不存在多来源兼容或历史 fallback 需求。
- 问题确认：旧 helper 只用 `try_exists()` 判断“存在”，普通文件占据包目录位置时会被误判为已安装包，随后可能被 `fs::rename` 移入备份目录，导致卸载或更新流程把损坏状态当成正常包处理。

### 核心修复与调整概述

- 将 `skill_package_dir_exists` 重命名并收紧为 `skill_package_dir_is_directory`，语义从“路径存在”改为“包路径必须是目录”。
- 新 helper 基于 `fs::metadata` 判断文件系统类型：目录返回 `Ok(true)`，缺失返回 `Ok(false)`，存在但不是目录时返回 `Skill package path is not a directory: ...`。
- 安装、更新、卸载、提交清理和回滚恢复中的 8 个调用点统一切换到目录型 helper。
- 缺失包目录继续保留原语义：卸载缺失包仍返回未删除，更新缺失包仍返回已安装目录不存在；仅“存在但类型错误”的状态被升级为显式错误。

### 文件变更清单

- 修改：`src/skill/manager.rs`，收紧 skill package 生命周期路径的目录类型判定，并同步所有调用点。
- 修改：`src/skill/manager/tests.rs`，新增普通文件占据包目录位置时卸载失败的回归测试。
- 修改：`TASK_LOG.md`，追加本轮任务记录。

### 关键代码调整详情

- `skill_package_dir_is_directory(skill_dir)` 现在匹配 `fs::metadata(skill_dir)`，只允许 `metadata.is_dir()` 作为存在的包目录。
- `prepare_uninstall_skill_at_path_in_plane` 在创建卸载备份和移动路径前先确认目标是目录，避免普通文件被当成可移除 skill 包。
- `stage_skill_install_from_archive` 在判断 target 是否已存在时也使用同一目录契约，避免文件占位被误认为合法已安装包目录。
- `stage_skill_update_from_archive` 在备份当前包前确认 target 是目录，避免把普通文件迁移成 update backup。
- `commit_prepared_skill_apply`、`rollback_prepared_skill_apply`、`rollback_prepared_skill_uninstall` 统一在清理或恢复目录前执行目录类型检查。
- 新增 `uninstall_skill_rejects_file_package_path`，通过真实 `uninstall_skill` 入口构造 `skills/vulcan-codekit` 为普通文件的场景，断言错误信息、路径渲染和文件仍留在原位。

### 验证记录

- 格式化：`cargo fmt` 通过。
- 目标验证：`cargo test uninstall_skill_rejects_file_package_path -- --nocapture` 通过，1 个匹配测试通过。
- 目标验证：`cargo test package_dir -- --nocapture` 通过，2 个匹配测试通过。
- 范围验证：`cargo test skill::manager -- --nocapture` 通过，41 个 skill manager 范围测试全部通过。
- 全量验证：`cargo test` 通过，420 个测试全部通过。
- 静态验证：`cargo clippy --all-targets --all-features -- -D warnings` 通过，无 Clippy 警告。
- 空白审核：`git diff --check -- src/skill/manager.rs src/skill/manager/tests.rs TASK_LOG.md` 通过。
- 搜索复查：旧 `skill_package_dir_exists` 已无残留；所有包生命周期路径均改用 `skill_package_dir_is_directory`。

### 代码审核与遗留事项

- 修改部分代码审核确认没有引入候选路径、多来源兼容、静默 fallback、历史兼容分支或额外状态。
- 本轮没有改变 skill id 校验、下载解压、install record 写入、disabled record 还原、生命周期 guard 或 reload 流程。
- 文件占位包路径现在是显式状态损坏错误；缺失包目录与 metadata 探测失败继续保持不同语义。
- 后续循环可继续排查 `staging_temp_root_exists` 的目录类型契约，以及剩余 `.exists()` / `.is_dir()` / `.is_file()` 调用点。

## 2026-07-06 第199轮：skill manager 暂存临时根目录类型校验显式化

### 探索记录

- 本轮沿第198轮遗留候选继续追踪 `src/skill/manager.rs` 中的 `staging_temp_root_exists`，确认它只被安装/更新归档暂存路径使用。
- 调用链确认：`stage_skill_install_from_archive` 构造 `lifecycle_root/install_tmp/{skill_id}-{timestamp}`，先探测该路径，再清理旧暂存根、创建目录、解压归档并读取清单。
- 调用链确认：`stage_skill_update_from_archive` 构造 `lifecycle_root/update_tmp/{skill_id}-{timestamp}`，同样在解压归档前经过该 helper。
- 路径来源确认：暂存根唯一来自 `self.config.lifecycle_root`、固定分段 `install_tmp` / `update_tmp`、已校验的 `skill_id` 与当前毫秒时间戳，不存在多来源兼容或历史 fallback 需求。
- 问题确认：旧 helper 只用 `try_exists()` 判断“存在”，普通文件占据最终暂存根位置时会进入 `remove_dir_all` 的低层错误；长期语义应在暂存探测阶段直接声明“暂存根必须是目录”。
- 测试策略确认：安装/更新入口的最终暂存根包含当前毫秒时间戳，稳定预置同名普通文件需要依赖时间碰撞，不适合作为回归测试；本轮采用 helper 级文件占位测试覆盖同一共享防线。

### 核心修复与调整概述

- 将 `staging_temp_root_exists` 重命名并收紧为 `staging_temp_root_is_directory`，语义从“路径存在”改为“暂存根存在时必须是目录”。
- 新 helper 基于 `fs::metadata` 判断文件系统类型：目录返回 `Ok(true)`，缺失返回 `Ok(false)`，存在但不是目录时返回 `Skill staging temp root is not a directory: ...`。
- 安装暂存和更新暂存两个调用点统一切换到目录型 helper。
- 缺失暂存根继续保留原语义：调用方会继续执行 `create_dir_all` 创建目录；仅“存在但类型错误”的状态被升级为显式错误。

### 文件变更清单

- 修改：`src/skill/manager.rs`，收紧 install/update staging 临时根路径的目录类型判定，并同步两个调用点。
- 修改：`src/skill/manager/tests.rs`，新增普通文件占据暂存临时根位置时 helper 显式失败的回归测试。
- 修改：`TASK_LOG.md`，追加本轮任务记录。

### 关键代码调整详情

- `staging_temp_root_is_directory(temp_root)` 现在匹配 `fs::metadata(temp_root)`，只允许 `metadata.is_dir()` 作为已存在的暂存临时根。
- `stage_skill_install_from_archive` 在清理旧 install temp root 前先确认目标是目录，避免普通文件被当成可清理暂存目录。
- `stage_skill_update_from_archive` 在清理旧 update temp root 前先确认目标是目录，避免普通文件错误延迟到 `remove_dir_all`。
- 新增 `staging_temp_root_rejects_file_path`，构造与 install/update 命名一致的文件型暂存根，断言错误信息、路径渲染和文件仍留在原位。

### 验证记录

- 格式化：`cargo fmt` 通过。
- 目标验证：`cargo test staging_temp_root -- --nocapture` 通过，1 个匹配测试通过。
- 目标验证：`cargo test stage_skill_ -- --nocapture` 通过，2 个既有暂存探测测试通过。
- 范围验证：`cargo test skill::manager -- --nocapture` 通过，42 个 skill manager 范围测试全部通过。
- 全量验证：`cargo test` 通过，421 个测试全部通过。
- 静态验证：`cargo clippy --all-targets --all-features -- -D warnings` 通过，无 Clippy 警告。
- 空白审核：`git diff --check -- src/skill/manager.rs src/skill/manager/tests.rs TASK_LOG.md` 通过。
- 搜索复查：旧 `staging_temp_root_exists` 已无残留；安装/更新暂存入口均改用 `staging_temp_root_is_directory`。

### 代码审核与遗留事项

- 修改部分代码审核确认没有引入候选路径、多来源兼容、静默 fallback、历史兼容分支或额外状态。
- 本轮没有改变归档下载、归档解压、manifest 读取、skill id/version 校验、TempDirGuard 清理或 install/update 提交流程。
- 文件占位暂存根现在是显式状态损坏错误；缺失暂存根与 metadata 探测失败继续保持不同语义。
- 后续循环可继续排查 `src/skill/manager.rs` 与 `src/runtime/engine.rs` 中剩余 `.exists()` / `.is_dir()` / `.is_file()` 调用点是否仍存在类型语义缺失。

## 2026-07-06 第200轮：技能配置文件类型校验显式化

### 探索记录

- 本轮扫描生产代码中的 `.try_exists()`、`.exists()`、`.is_dir()` 与 `.is_file()` 调用点后，选定 `src/runtime/config.rs` 的 `skill_config_file_exists` 作为目标。
- 调用链确认：`SkillConfigStore::with_document` 与 `SkillConfigStore::with_document_mut` 都会解析有效配置文件路径、获取路径级锁，然后调用 `read_document_from`。
- 执行流确认：`read_document_from` 先调用该 helper；helper 返回 `false` 时视为缺失配置并返回空文档，返回 `true` 时继续执行 `fs::read_to_string` 与 JSON 解析。
- 路径来源确认：配置路径唯一来自显式传入的 `explicit_file_path`，或默认 `runtime_root/config/skill_config.json`；两条来源都在 `file_path()` 中确定，不存在多来源兼容或历史 fallback 需求。
- 问题确认：旧 helper 只用 `try_exists()` 判断“存在”，目录占据配置文件路径时会被视为存在，然后延迟到 `read_to_string` 报错；长期语义应在配置读取入口直接声明“存在则必须是普通文件”。

### 核心修复与调整概述

- 将 `skill_config_file_exists` 重命名并收紧为 `skill_config_file_is_file`，语义从“路径存在”改为“配置路径存在时必须是文件”。
- 新 helper 基于 `fs::metadata` 判断文件系统类型：普通文件返回 `Ok(true)`，缺失返回 `Ok(false)`，存在但不是文件时返回 `skill config file is not a file '...'`。
- `read_document_from` 切换到文件型 helper，继续保留缺失配置文件代表空配置文档的语义。
- 目录型配置路径现在在 JSON 读取前失败，避免把配置状态损坏伪装成读取阶段错误。

### 文件变更清单

- 修改：`src/runtime/config.rs`，收紧技能配置文件探测 helper 的文件类型判定，并新增目录型配置路径回归测试。
- 修改：`TASK_LOG.md`，追加本轮任务记录。

### 关键代码调整详情

- `skill_config_file_is_file(file_path)` 现在匹配 `fs::metadata(file_path)`，只允许 `metadata.is_file()` 作为已存在的配置文件。
- `read_document_from` 在读取 JSON 前先通过文件类型校验，缺失路径仍返回 `SkillConfigDocument::default()`。
- 新增 `skill_config_store_rejects_directory_config_file`，通过真实 `SkillConfigStore::get_value` 入口构造目录型 `skill_config.json`，断言错误信息和宿主可见路径。
- 既有 `skill_config_store_reports_file_path_probe_errors` 继续覆盖 metadata 探测错误，`skill_config_parse_error_uses_host_visible_path` 继续覆盖普通文件内 JSON 解析错误。

### 验证记录

- 格式化：`cargo fmt` 通过。
- 目标验证：`cargo test skill_config_store_rejects_directory_config_file -- --nocapture` 通过，1 个匹配测试通过。
- 目标验证：`cargo test skill_config_store_reports_file_path_probe_errors -- --nocapture` 通过，1 个匹配测试通过。
- 目标验证：`cargo test skill_config_parse_error_uses_host_visible_path -- --nocapture` 通过，1 个匹配测试通过。
- 范围验证：`cargo test runtime::config -- --nocapture` 通过，16 个 runtime config 范围测试全部通过。
- 全量验证：`cargo test` 通过，422 个测试全部通过。
- 静态验证：`cargo clippy --all-targets --all-features -- -D warnings` 通过，无 Clippy 警告。
- 空白审核：`git diff --check -- src/runtime/config.rs TASK_LOG.md` 通过。
- 搜索复查：旧 `skill_config_file_exists` 已无残留；配置读取入口已统一改用 `skill_config_file_is_file`。

### 代码审核与遗留事项

- 修改部分代码审核确认没有引入候选路径、多来源兼容、静默 fallback、历史兼容分支或额外状态。
- 本轮没有改变配置路径解析、路径级锁、默认 runtime root 更新、JSON 文档结构、原子写入或 Windows `ReplaceFileW` 提交流程。
- 目录型配置路径现在是显式配置文件类型错误；缺失配置文件、metadata 探测失败与 JSON 解析失败继续保持不同语义。
- 后续循环可继续排查 `src/runtime/config.rs` 的原子替换目标路径类型、`src/download/*` 中的归档/缓存暂存路径，以及 `src/runtime/managed_runtime.rs` 中的环境目录与 marker 文件契约。

## 2026-07-06 第201轮：技能包解压清单文件类型校验显式化

### 探索记录

- 本轮继续排查 `src/download/archive.rs` 中剩余 `try_exists()` helper，重点对比 `extracted_skill_manifest_exists` 与 `installed_export_target_exists`。
- 调用链确认：`extract_skill_package_zip` 解压 zip 条目后构造 `temp_root/{expected_skill_id}/skill.yaml`，通过该 helper 判断清单是否存在，随后返回 `skill_dir` 给 `SkillManager`。
- 后续执行流确认：`SkillManager::stage_skill_install_from_archive` 与 `stage_skill_update_from_archive` 接收 `skill_dir` 后立即调用 `read_skill_manifest_from_directory`，该函数会把 `skill.yaml` 作为 YAML 文件读取。
- 路径来源确认：`skill.yaml` 唯一来源是 `temp_root.join(expected_skill_id).join("skill.yaml")`；`expected_skill_id` 已由调用方传入并在 zip 顶层目录校验中使用，不存在候选路径或历史兼容分支。
- 问题确认：旧 helper 只用 `try_exists()` 判断“存在”，目录型 `skill.yaml` 会通过解压边界，错误延迟到 manager 的 `read_to_string`；长期语义应在归档解压完成时直接要求清单是普通文件。
- 范围取舍：`installed_export_target_exists` 也仍是候选，但本轮只收紧技能包安装/更新主链路上的 `skill.yaml` 清单边界，导出目标留给后续循环。

### 核心修复与调整概述

- 将 `extracted_skill_manifest_exists` 重命名并收紧为 `extracted_skill_manifest_is_file`，语义从“清单路径存在”改为“清单路径存在时必须是文件”。
- 新 helper 基于 `fs::metadata` 判断文件系统类型：普通文件返回 `Ok(true)`，缺失返回 `Ok(false)`，存在但不是文件时返回 `Extracted skill manifest is not a file: ...`。
- `extract_skill_package_zip` 切换到文件型 helper，缺失清单仍返回原有 `Skill package ... does not contain .../skill.yaml` 错误。
- 目录型清单现在在归档解压边界失败，避免把包结构错误延迟到后续 YAML 读取阶段。

### 文件变更清单

- 修改：`src/download/archive.rs`，收紧已解压技能清单的文件类型判定，并新增目录型清单回归测试。
- 修改：`TASK_LOG.md`，追加本轮任务记录。

### 关键代码调整详情

- `extracted_skill_manifest_is_file(skill_yaml)` 现在匹配 `fs::metadata(skill_yaml)`，只允许 `metadata.is_file()` 作为已存在的 `skill.yaml`。
- `extract_skill_package_zip` 在返回 `skill_dir` 前通过文件类型校验，确保后续 manager 读取清单时不会收到目录型路径。
- 既有 `extracted_skill_manifest_probe_errors_are_reported` 切换到新 helper，继续覆盖 metadata 探测错误。
- 新增 `extracted_skill_manifest_rejects_directory_manifest`，构造 `vulcan-codekit/skill.yaml` 为目录的解压后结构，断言类型错误和宿主可见路径。

### 验证记录

- 格式化：`cargo fmt` 通过。
- 目标验证：`cargo test extracted_skill_manifest_rejects_directory_manifest -- --nocapture` 通过，1 个匹配测试通过。
- 目标验证：`cargo test extracted_skill_manifest_probe_errors_are_reported -- --nocapture` 通过，1 个匹配测试通过。
- 范围验证：`cargo test download::archive -- --nocapture` 通过，5 个 download archive 范围测试全部通过。
- 全量验证：`cargo test` 通过，423 个测试全部通过。
- 静态验证：`cargo clippy --all-targets --all-features -- -D warnings` 通过，无 Clippy 警告。
- 空白审核：`git diff --check -- src/download/archive.rs TASK_LOG.md` 通过。
- 搜索复查：旧 `extracted_skill_manifest_exists` 已无残留；技能包解压清单边界已统一改用 `extracted_skill_manifest_is_file`。

### 代码审核与遗留事项

- 修改部分代码审核确认没有引入候选路径、多来源兼容、静默 fallback、历史兼容分支或额外状态。
- 本轮没有改变 zip 顶层目录校验、zip 路径穿越校验、文件解压写入、依赖导出安装、tar.gz 导出匹配或 executable 标记流程。
- 目录型 `skill.yaml` 现在是显式包结构错误；缺失清单与 metadata 探测失败继续保持不同语义。
- 后续循环可继续排查 `installed_export_target_exists` 的导出目标文件契约，以及 `src/download/manager.rs` 中缓存暂存目录与缺失路径 helper 的类型语义。

## 2026-07-06 第202轮：tar.gz 依赖导出目标文件类型校验显式化

### 探索记录

- 本轮接续第201轮遗留候选，追踪 `src/download/archive.rs` 中的 `installed_export_target_exists`。
- 调用链确认：`install_downloaded_payload(..., DependencyArchiveType::TarGz, ...)` 进入 `install_from_tar_gz_archive`，遍历 tar.gz 条目并把匹配导出的条目写入 `install_root/{export.target_path}`。
- 导出校验确认：遍历结束后，函数再次遍历所有声明导出，用该 helper 判断每个 `target_path` 是否已经出现；缺失时返回 `tar.gz archive ... does not contain required export ...`。
- 路径来源确认：导出目标路径唯一来自 `join_relative_target(install_root, export.target_path)`，`export.target_path` 来自依赖 manifest 的单个声明字段，不存在多来源兼容或历史 fallback 需求。
- 问题确认：旧 helper 只用 `try_exists()` 判断“存在”，当 tar.gz 缺少声明导出但目标位置已有同名目录时，目录会被误判为导出已安装，导致必需导出缺失被错误放行。

### 核心修复与调整概述

- 将 `installed_export_target_exists` 重命名并收紧为 `installed_export_target_is_file`，语义从“导出目标存在”改为“导出目标存在时必须是文件”。
- 新 helper 基于 `fs::metadata` 判断文件系统类型：普通文件返回 `Ok(true)`，缺失返回 `Ok(false)`，存在但不是文件时返回 `Installed export target is not a file: ...`。
- `install_from_tar_gz_archive` 的最终导出校验切换到文件型 helper。
- 目录型目标不再能掩盖缺失导出；缺失文件仍继续走原有“归档不包含必需导出”错误。

### 文件变更清单

- 修改：`src/download/archive.rs`，收紧 tar.gz 已安装导出目标的文件类型判定，并新增真实安装入口回归测试。
- 修改：`TASK_LOG.md`，追加本轮任务记录。

### 关键代码调整详情

- `installed_export_target_is_file(target_path)` 现在匹配 `fs::metadata(target_path)`，只允许 `metadata.is_file()` 作为已安装导出目标。
- `install_from_tar_gz_archive` 在所有条目处理后，用文件型 helper 验证每个声明导出的目标路径。
- 既有 `installed_export_target_probe_errors_are_reported` 切换到新 helper，继续覆盖 metadata 探测错误。
- 新增 `tar_gz_install_rejects_directory_export_target_when_export_missing`，构造一个有效但不包含声明导出的 tar.gz，并在导出目标位置预置目录，断言真实 `install_downloaded_payload` 入口显式失败。

### 验证记录

- 格式化：`cargo fmt` 通过。
- 目标验证：`cargo test tar_gz_install_rejects_directory_export_target_when_export_missing -- --nocapture` 通过，1 个匹配测试通过。
- 目标验证：`cargo test installed_export_target_probe_errors_are_reported -- --nocapture` 通过，1 个匹配测试通过。
- 范围验证：`cargo test download::archive -- --nocapture` 通过，6 个 download archive 范围测试全部通过。
- 全量验证：`cargo test` 通过，424 个测试全部通过。
- 静态验证：`cargo clippy --all-targets --all-features -- -D warnings` 通过，无 Clippy 警告。
- 空白审核：`git diff --check -- src/download/archive.rs TASK_LOG.md` 通过。
- 搜索复查：旧 `installed_export_target_exists` 已无残留；tar.gz 导出目标校验已统一改用 `installed_export_target_is_file`。

### 代码审核与遗留事项

- 修改部分代码审核确认没有引入候选路径、多来源兼容、静默 fallback、历史兼容分支或额外状态。
- 本轮没有改变 raw/zip 安装流程、tar.gz 条目匹配、导出目标拼接规则、归档读取方式或 executable 标记流程。
- 目录型导出目标现在是显式文件类型错误；缺失导出、metadata 探测失败与归档读取失败继续保持不同语义。
- 后续循环可继续排查 `src/download/manager.rs` 中缓存临时根、下载目标与缺失路径判断的类型契约。

## 2026-07-06 第203轮：下载缓存命中目标文件类型校验显式化

### 探索记录

- 本轮进入 `src/download/manager.rs`，先扫描缓存目标、fresh 文本缓存清理、校验失败清理和下载落盘路径。
- 调用链确认：`DownloadManager::download` 创建缓存根后，通过 `cached_path_for_request` 派生确定性缓存路径，再调用 `cached_download_target_exists` 判断是否命中缓存。
- 后续执行流确认：旧 helper 返回 true 后，调用点再次读取 metadata 并判断 `metadata.is_file()`，目录型缓存会在第二段逻辑里失败。
- 路径来源确认：缓存目标路径唯一来自 `cache_root.join(format!("{}{}", request.cache_key, infer_download_extension(request.source_locator)))`，不存在候选路径或历史兼容分支。
- 问题确认：旧 helper 名义和语义仍是“路径存在”，把“缓存命中必须是文件”的真实契约分散到调用点；长期应把缓存命中边界收紧到 helper 层。

### 核心修复与调整概述

- 将 `cached_download_target_exists` 重命名并收紧为 `cached_download_target_is_file`，语义从“缓存目标存在”改为“缓存目标存在时必须是文件”。
- 新 helper 基于 `fs::metadata` 判断文件系统类型：普通文件返回 `Ok(true)`，缺失返回 `Ok(false)`，存在但不是文件时返回 `Cached download target is not a file: ...`。
- `DownloadManager::download` 的缓存命中判断切换到文件型 helper，目录型缓存会在进入缓存命中分支前失败。
- 保留后续 metadata 读取以获取缓存文件长度；该读取仍负责处理命中后文件系统状态变化的错误。

### 文件变更清单

- 修改：`src/download/manager.rs`，收紧下载缓存命中目标的文件类型判定，并更新相关测试断言。
- 修改：`TASK_LOG.md`，追加本轮任务记录。

### 关键代码调整详情

- `cached_download_target_is_file(target_path)` 现在匹配 `fs::metadata(target_path)`，只允许 `metadata.is_file()` 作为有效缓存命中。
- `DownloadManager::download` 在网络下载前通过文件型 helper 判断缓存是否命中；缺失路径继续触发下载流程。
- 既有 `cached_download_target_probe_errors_are_reported` 切换到新 helper，继续覆盖 metadata 探测错误。
- 既有 `download_rejects_cached_directory_instead_of_returning_it` 现在断言 helper 层的 `Cached download target is not a file: ...` 错误。

### 验证记录

- 格式化：`cargo fmt` 通过。
- 目标验证：`cargo test cached_download_target_probe_errors_are_reported -- --nocapture` 通过，1 个匹配测试通过。
- 目标验证：`cargo test download_rejects_cached_directory_instead_of_returning_it -- --nocapture` 通过，1 个匹配测试通过。
- 范围验证：`cargo test download::manager -- --nocapture` 通过，12 个 download manager 范围测试全部通过。
- 全量验证：`cargo test` 通过，424 个测试全部通过。
- 静态验证：`cargo clippy --all-targets --all-features -- -D warnings` 通过，无 Clippy 警告。
- 空白审核：`git diff --check -- src/download/manager.rs TASK_LOG.md` 通过。
- 搜索复查：旧 `cached_download_target_exists` 已无残留；下载缓存命中边界已统一改用 `cached_download_target_is_file`。

### 代码审核与遗留事项

- 修改部分代码审核确认没有引入候选路径、多来源兼容、静默 fallback、历史兼容分支或额外状态。
- 本轮没有改变缓存路径派生、网络下载、进度回调、fresh 文本缓存清理、校验失败重下或 GitHub release 解析流程。
- 目录型缓存目标现在是显式缓存文件类型错误；缺失缓存、metadata 探测失败与缓存文件读取失败继续保持不同语义。
- 后续循环可继续排查 `download_with_sha256` 的校验失败清理、`fetch_text_fresh` 的 stale cache 删除语义，以及 `src/runtime/managed_runtime.rs` 中环境目录和 marker 文件类型契约。

## 2026-07-06 第204轮：受管运行时环境 marker 文件类型校验显式化

### 探索记录

- 本轮进入 `src/runtime/managed_runtime.rs`，扫描环境目录、构建目录、安装清单、可执行文件、lockfile/package 文件与 marker 文件的类型判断。
- 调用链确认：`ensure_managed_env` 先调用 `managed_env_is_ready`；若返回 false，则根据运行时类型重建 Python 或 Node 环境。
- marker 检查确认：`managed_env_is_ready` 构造 `managed_env_marker_path(plan.env_dir)`，旧实现用 `marker_path.is_file()` 判断 marker 是否存在且为文件。
- 路径来源确认：marker 路径唯一来自 `plan.env_dir.join(".luaskills-env.json")`，其中 `plan.env_dir` 由 `build_env_plan` 根据 runtime root、runtime kind、runtime version 与环境 hash 派生，不存在候选路径或历史兼容分支。
- 问题确认：`Path::is_file()` 会把缺失、目录型 marker 与 metadata 探测失败都折叠为 false；目录型 marker 属于环境状态损坏，不应被当作“未就绪，可重建”的普通状态。

### 核心修复与调整概述

- 新增 `managed_env_marker_path_is_file`，显式区分 marker 文件存在、marker 缺失、marker 类型错误与 metadata 探测失败。
- `managed_env_is_ready` 切换到该 helper：缺失 marker 仍返回 `Ok(false)`；存在但不是文件时返回 `Managed runtime env marker is not a file: ...`。
- marker 目录占位不再触发静默重建，而是在环境就绪检查阶段报告状态损坏。
- metadata 探测失败不再被折叠为未就绪状态。

### 文件变更清单

- 修改：`src/runtime/managed_runtime.rs`，新增受管环境 marker 文件类型 helper，并补充目录型 marker 回归测试。
- 修改：`TASK_LOG.md`，追加本轮任务记录。

### 关键代码调整详情

- `managed_env_marker_path_is_file(marker_path)` 现在匹配 `fs::metadata(marker_path)`，只允许 `metadata.is_file()` 作为有效 marker。
- `managed_env_is_ready` 在读取 JSON marker 前先通过文件类型校验；缺失 marker 仍表示环境未就绪。
- 新增 `managed_env_ready_rejects_directory_marker`，通过真实 Python 环境计划解析路径构造目录型 `.luaskills-env.json`，断言 `managed_env_is_ready` 显式失败并包含宿主可见路径。
- 既有 `managed_env_ready_checks_expected_marker` 继续覆盖缺失 marker 返回 false、正确 marker 返回 true 的语义。

### 验证记录

- 格式化：`cargo fmt` 通过。
- 目标验证：`cargo test managed_env_ready_rejects_directory_marker -- --nocapture` 通过，1 个匹配测试通过。
- 目标验证：`cargo test managed_env_ready_checks_expected_marker -- --nocapture` 通过，1 个匹配测试通过。
- 范围验证：`cargo test runtime::managed_runtime -- --nocapture` 通过，9 个 managed runtime 范围测试全部通过。
- 全量验证：`cargo test` 通过，425 个测试全部通过。
- 静态验证：`cargo clippy --all-targets --all-features -- -D warnings` 通过，无 Clippy 警告。
- 空白审核：`git diff --check -- src/runtime/managed_runtime.rs TASK_LOG.md` 通过。
- 搜索复查：`managed_env_is_ready` 已不再使用 `Path::is_file()` 作为 marker 就绪边界，改用 `managed_env_marker_path_is_file`。

### 代码审核与遗留事项

- 修改部分代码审核确认没有引入候选路径、多来源兼容、静默 fallback、历史兼容分支或额外状态。
- 本轮没有改变环境 hash、环境目录派生、marker JSON 结构、Python/Node 环境创建、安装清单解析或可执行文件解析流程。
- 目录型 marker 现在是显式环境状态错误；缺失 marker、metadata 探测失败与 JSON 解析失败继续保持不同语义。
- 后续循环可继续排查 `create_node_env`、`prepare_build_dir`、`finish_build_dir` 中环境目录和构建目录的类型契约，以及安装 manifest 和 executable 的文件类型错误是否需要更明确。

## 2026-07-06 第205轮：受管运行时声明文件类型校验显式化

### 探索记录

- 本轮继续排查 `src/runtime/managed_runtime.rs` 中的路径类型判断，选定 `resolve_required_skill_file` 与 `resolve_optional_skill_file`。
- 调用链确认：`resolve_python_env_plan` 通过 `resolve_required_skill_file` 解析 `python_runtime.lockfile`；`resolve_node_env_plan` 通过 `resolve_optional_skill_file` 解析 `node_runtime.package_json`，并通过 `resolve_required_skill_file` 解析 `node_runtime.lockfile`。
- 路径来源确认：这些路径唯一来自 skill 的运行时依赖声明字段，先经 `resolve_skill_file(skill_dir, relative_path, field_label)` 做 skill 目录边界和父目录逃逸校验，不存在候选路径或历史兼容分支。
- 问题确认：旧实现用 `Path::is_file()`，会把缺失文件、目录型声明文件与 metadata 探测失败都折叠为 false，然后统一报 `{field_label} not found`。
- 语义边界确认：缺失 lockfile/package 文件仍应是 not found；路径存在但不是文件属于配置类型错误，应在计算 hash 或读取文件前显式报告。

### 核心修复与调整概述

- 新增 `managed_runtime_skill_file_path_is_file`，显式区分声明文件存在、缺失、类型错误与 metadata 探测失败。
- `resolve_required_skill_file` 和 `resolve_optional_skill_file` 切换到该 helper。
- 缺失文件继续返回原有 `{field_label} not found: ...` 错误。
- 目录型或其它非文件路径现在返回 `{field_label} is not a file: ...`，不再伪装成缺失。

### 文件变更清单

- 修改：`src/runtime/managed_runtime.rs`，新增受管运行时声明文件类型 helper，并补充目录型 Python lockfile 回归测试。
- 修改：`TASK_LOG.md`，追加本轮任务记录。

### 关键代码调整详情

- `managed_runtime_skill_file_path_is_file(path, field_label)` 现在匹配 `fs::metadata(path)`，只允许 `metadata.is_file()` 作为有效声明文件。
- `resolve_required_skill_file` 在返回路径前通过文件类型 helper 校验，缺失时保留 `{field_label} not found`。
- `resolve_optional_skill_file` 在非空相对路径场景下复用同一 helper，空字符串仍表示没有可选文件。
- 新增 `python_env_plan_rejects_directory_lockfile`，通过真实 `resolve_python_env_plan` 构造目录型 `python/requirements.lock`，断言 `python_runtime.lockfile is not a file: ...`。

### 验证记录

- 格式化：`cargo fmt` 通过。
- 目标验证：`cargo test python_env_plan_rejects_directory_lockfile -- --nocapture` 通过，1 个匹配测试通过。
- 目标验证：`cargo test python_env_plan_missing_lockfile_error_uses_host_visible_path -- --nocapture` 通过，1 个匹配测试通过。
- 范围验证：`cargo test runtime::managed_runtime -- --nocapture` 通过，10 个 managed runtime 范围测试全部通过。
- 全量验证：`cargo test` 通过，426 个测试全部通过。
- 静态验证：`cargo clippy --all-targets --all-features -- -D warnings` 通过，无 Clippy 警告。
- 空白审核：`git diff --check -- src/runtime/managed_runtime.rs TASK_LOG.md` 通过。
- 搜索复查：`resolve_required_skill_file` 与 `resolve_optional_skill_file` 已不再使用 `Path::is_file()`，统一改用 `managed_runtime_skill_file_path_is_file`。

### 代码审核与遗留事项

- 修改部分代码审核确认没有引入候选路径、多来源兼容、静默 fallback、历史兼容分支或额外状态。
- 本轮没有改变 skill 相对路径解析、父目录逃逸校验、hash 计算、Python/Node 环境计划结构或可选 package.json 空值语义。
- 目录型声明文件现在是显式类型错误；缺失声明文件、metadata 探测失败与文件读取失败继续保持不同语义。
- 后续循环可继续排查 `resolve_install_executable` 的 executable 类型契约，以及 `create_node_env`、`prepare_build_dir`、`finish_build_dir` 中环境目录和构建目录的类型契约。

## 2026-07-06 第206轮：受管运行时安装 executable 文件类型校验显式化

### 探索记录

- 本轮接续第205轮遗留候选，追踪 `src/runtime/managed_runtime.rs` 中的 `resolve_install_executable`。
- 调用链确认：`resolve_python_env_plan` 和 `resolve_node_env_plan` 都会通过 `runtime_install_dir` 定位已安装 runtime / package manager 目录，再调用 `resolve_install_executable` 读取安装清单并解析 executable。
- 路径来源确认：executable 路径唯一来自 `runtime-manifest.json` 的 `executable` 字段，并拼接到对应 `install_dir` 下，不存在候选路径或历史兼容分支。
- 问题确认：旧实现用 `Path::is_file()`，会把缺失 executable、目录型 executable 与 metadata 探测失败都折叠为 `managed runtime executable not found`。
- 语义边界确认：缺失 executable 仍应是 not found；路径存在但不是文件属于 runtime 安装包结构错误，应在环境计划解析阶段显式报告。

### 核心修复与调整概述

- 新增 `managed_runtime_executable_path_is_file`，显式区分 executable 文件存在、缺失、类型错误与 metadata 探测失败。
- `resolve_install_executable` 切换到该 helper。
- 缺失 executable 继续返回原有 `managed runtime executable not found: ...` 错误。
- 目录型或其它非文件路径现在返回 `managed runtime executable is not a file: ...`。

### 文件变更清单

- 修改：`src/runtime/managed_runtime.rs`，新增受管运行时 executable 文件类型 helper，并补充目录型 runtime executable 回归测试。
- 修改：`TASK_LOG.md`，追加本轮任务记录。

### 关键代码调整详情

- `managed_runtime_executable_path_is_file(executable)` 现在匹配 `fs::metadata(executable)`，只允许 `metadata.is_file()` 作为有效 executable。
- `resolve_install_executable` 在返回 executable 路径前通过文件类型 helper 校验，缺失时保留 not found 语义。
- 新增 `python_env_plan_rejects_directory_runtime_executable`，通过真实 `resolve_python_env_plan` 构造目录型 runtime executable，断言 `managed runtime executable is not a file: ...`。
- 该测试首次失败暴露出断言路径用单段斜杠字符串构造，未沿生产 `runtime_install_dir` 分段来源生成；已改为分段 `join` 后复测通过。

### 验证记录

- 格式化：`cargo fmt` 通过。
- 目标验证：`cargo test python_env_plan_rejects_directory_runtime_executable -- --nocapture` 通过，1 个匹配测试通过。
- 目标验证：`cargo test python_env_plan_resolves_manifests_and_lockfile -- --nocapture` 通过，1 个既有解析测试通过。
- 范围验证：`cargo test runtime::managed_runtime -- --nocapture` 通过，11 个 managed runtime 范围测试全部通过。
- 全量验证：`cargo test` 通过，427 个测试全部通过。
- 静态验证：`cargo clippy --all-targets --all-features -- -D warnings` 通过，无 Clippy 警告。
- 空白审核：`git diff --check -- src/runtime/managed_runtime.rs TASK_LOG.md` 通过。
- 搜索复查：`resolve_install_executable` 已不再使用 `Path::is_file()`，统一改用 `managed_runtime_executable_path_is_file`。

### 代码审核与遗留事项

- 修改部分代码审核确认没有引入候选路径、多来源兼容、静默 fallback、历史兼容分支或额外状态。
- 本轮没有改变安装清单 JSON 结构、schema/runtime/version/platform 校验、安装目录定位、Python/Node 环境计划结构或 executable 路径拼接规则。
- 目录型 executable 现在是显式安装包类型错误；缺失 executable、metadata 探测失败与 manifest 读取/解析失败继续保持不同语义。
- 后续循环可继续排查 `create_node_env`、`prepare_build_dir`、`finish_build_dir` 中环境目录和构建目录的类型契约，以及 `read_install_manifest` 对目录型 `runtime-manifest.json` 的错误语义。

## 2026-07-06 第207轮：受管运行时安装清单文件类型校验显式化

### 探索记录

- 本轮接续第206轮遗留候选，追踪 `src/runtime/managed_runtime.rs` 中的 `read_install_manifest`。
- 调用链确认：`resolve_install_executable` 首先调用 `read_install_manifest(install_dir)`，随后才校验 schema/runtime/version/platform 与 executable。
- 路径来源确认：安装清单路径唯一来自 `install_dir.join("runtime-manifest.json")`；`install_dir` 由 `runtime_install_dir` 或调用方直接传入，不存在候选清单路径或历史兼容分支。
- 问题确认：旧实现直接 `fs::read_to_string`，目录型 `runtime-manifest.json` 会延迟为读取阶段错误；缺失清单也依赖底层 IO 文本表达。
- 语义边界确认：安装清单路径存在但不是文件属于安装包结构错误；缺失清单、metadata 探测失败、读取失败和 JSON 解析失败应保持不同语义。

### 核心修复与调整概述

- 新增 `managed_runtime_install_manifest_path_is_file`，在读取 JSON 前显式校验 `runtime-manifest.json` 是文件。
- `read_install_manifest` 切换为先执行文件类型 helper，再执行 `read_to_string` 和 JSON 解析。
- 目录型清单现在返回 `managed runtime install manifest is not a file: ...`。
- 缺失清单现在返回 `managed runtime install manifest not found: ...`，不再依赖底层读取错误文本。

### 文件变更清单

- 修改：`src/runtime/managed_runtime.rs`，新增安装清单文件类型 helper，并补充目录型 `runtime-manifest.json` 回归测试。
- 修改：`TASK_LOG.md`，追加本轮任务记录。

### 关键代码调整详情

- `managed_runtime_install_manifest_path_is_file(manifest_path)` 现在匹配 `fs::metadata(manifest_path)`，只允许 `metadata.is_file()` 作为有效安装清单。
- `read_install_manifest` 在读取 JSON 文本前调用该 helper，清单类型错误会在解析前返回。
- 新增 `read_install_manifest_rejects_directory_manifest`，构造目录型 `runtime-manifest.json` 并通过真实读取入口断言类型错误和宿主可见路径。

### 验证记录

- 格式化：`cargo fmt` 通过。
- 目标验证：`cargo test read_install_manifest_rejects_directory_manifest -- --nocapture` 通过，1 个匹配测试通过。
- 范围验证：`cargo test runtime::managed_runtime -- --nocapture` 通过，12 个 managed runtime 范围测试全部通过。
- 全量验证：`cargo test` 通过，428 个测试全部通过。
- 静态验证：`cargo clippy --all-targets --all-features -- -D warnings` 通过，无 Clippy 警告。
- 空白审核：`git diff --check -- src/runtime/managed_runtime.rs TASK_LOG.md` 通过。
- 搜索复查：`read_install_manifest` 已在 `read_to_string` 前统一经过 `managed_runtime_install_manifest_path_is_file`。

### 代码审核与遗留事项

- 修改部分代码审核确认没有引入候选路径、多来源兼容、静默 fallback、历史兼容分支或额外状态。
- 本轮没有改变安装清单 JSON 结构、BOM 处理、schema/runtime/version/platform 校验、executable 解析或环境计划结构。
- 目录型安装清单现在是显式安装包类型错误；缺失清单、metadata 探测失败、读取失败与 JSON 解析失败继续保持不同语义。
- 后续循环可继续排查 `create_node_env`、`prepare_build_dir`、`finish_build_dir` 中环境目录和构建目录的类型契约。

## 2026-07-06 第208轮：受管运行时环境与构建目录类型校验显式化

### 探索记录

- 本轮接续第207轮遗留候选，追踪 `src/runtime/managed_runtime.rs` 中 `create_node_env`、`prepare_build_dir` 与 `finish_build_dir` 的目录清理入口。
- 调用链确认：`ensure_managed_env` 在环境未就绪时按 runtime 类型进入 Python 或 Node 创建流程；Python 创建走 `prepare_build_dir` 后由 `finish_build_dir` 原子替换目标环境目录，Node 创建直接清理并重建 `plan.env_dir`。
- 路径来源确认：环境目录唯一来自 `ManagedRuntimeEnvPlan.env_dir`；Python 临时构建目录唯一由 `plan.env_dir.parent()` 下的 `.building-{plan.env_hash}-{pid}` 派生，不存在候选目录、历史兼容路径或多来源路径。
- 问题确认：旧实现用 `.exists()` 决定是否调用 `remove_dir_all`，当环境目录或构建目录路径存在但不是目录时，会把类型错误延迟为底层删除错误。
- 语义边界确认：缺失目录仍应进入创建流程；已存在目录可以被清理；路径存在但不是目录属于受管运行时环境状态或构建状态损坏，应在删除前显式报告。

### 核心修复与调整概述

- 新增 `managed_runtime_directory_path_is_directory`，显式区分目录存在、目录缺失、类型错误与 metadata 探测失败。
- `create_node_env` 在清理 `plan.env_dir` 前改用目录类型 helper，拒绝文件型环境目录。
- `prepare_build_dir` 在清理临时构建目录前改用目录类型 helper，拒绝文件型 `.building-*` 路径。
- `finish_build_dir` 在替换目标环境目录前改用目录类型 helper，拒绝文件型目标环境目录。

### 文件变更清单

- 修改：`src/runtime/managed_runtime.rs`，新增受管运行时目录类型 helper，并补充文件型构建目录回归测试。
- 修改：`TASK_LOG.md`，追加本轮任务记录。

### 关键代码调整详情

- `managed_runtime_directory_path_is_directory(path, directory_label)` 现在匹配 `fs::metadata(path)`，只允许 `metadata.is_dir()` 作为可清理目录。
- `create_node_env` 和 `finish_build_dir` 对 `plan.env_dir` 使用 `managed env directory` 标签，目录缺失继续走创建或替换流程。
- `prepare_build_dir` 对精确派生的构建目录使用 `managed build directory` 标签，文件占用时返回 `managed build directory is not a directory: ...`。
- 新增 `prepare_build_dir_rejects_file_build_dir`，构造真实 `ManagedRuntimeEnvPlan` 并用 `env_hash` 与当前进程号占用同一个 `.building-*` 路径，断言类型错误和宿主可见路径。

### 验证记录

- 格式化：`cargo fmt` 通过。
- 目标验证：`cargo test prepare_build_dir_rejects_file_build_dir -- --nocapture` 通过，1 个匹配测试通过。
- 范围验证：`cargo test runtime::managed_runtime -- --nocapture` 通过，13 个 managed runtime 范围测试全部通过。
- 全量验证：`cargo test` 通过，429 个测试全部通过。
- 静态验证：`cargo clippy --all-targets --all-features -- -D warnings` 通过，无 Clippy 警告。
- 空白审核：`git diff --check -- src/runtime/managed_runtime.rs TASK_LOG.md` 通过。
- 搜索复查：`create_node_env`、`prepare_build_dir` 与 `finish_build_dir` 已不再通过 `.exists()` 决定目录删除，统一改用 `managed_runtime_directory_path_is_directory`。

### 代码审核与遗留事项

- 修改部分代码审核确认没有引入候选路径、多来源兼容、静默 fallback、历史兼容分支或额外状态。
- 本轮没有改变环境 hash、环境目录派生、Node 安装命令、Python 构建目录命名、marker 写入、rename/copy fallback 或清理策略。
- 文件型环境目录或构建目录现在是显式目录类型错误；缺失目录、metadata 探测失败和删除失败继续保持不同语义。
- 后续循环可继续排查 `src/runtime/managed_runtime.rs` 中剩余目录遍历、复制或运行时包存储目录的类型契约，也可转回其它模块的 `.exists()` / `.is_dir()` 折叠点。

## 2026-07-06 第209轮：受管运行时复制降级目录项类型显式化

### 探索记录

- 本轮接续第208轮遗留候选，继续追踪 `src/runtime/managed_runtime.rs` 中的 `copy_dir_recursive`。
- 调用链确认：`create_python_env` 先通过 `prepare_build_dir` 构建临时环境目录，`finish_build_dir` 优先用 `fs::rename(build_dir, env_dir)` 替换目标环境；只有 rename 失败时才进入 `copy_dir_recursive(&build_dir, &env_dir)` 作为复制降级。
- 路径来源确认：复制源目录唯一来自 `prepare_build_dir` 产生的 `.building-{env_hash}-{pid}`；复制目标目录唯一来自 `plan.env_dir`，不存在候选来源、历史兼容路径或多来源输入。
- 对比本地模式：`src/runtime/engine.rs` 的 `copy_vulcan_fs_directory_recursive` 与 `copy_managed_node_skill_import_root` 已使用 `DirEntry::file_type()` 显式区分目录、普通文件、符号链接和其它类型。
- 问题确认：旧实现用 `source_path.is_dir()` 判定递归，否则直接进入 `fs::copy`；这会跟随符号链接，并把特殊文件或目录项探测失败延迟到复制错误，诊断边界不清晰。

### 核心修复与调整概述

- `copy_dir_recursive` 改为通过 `entry.file_type()` 获取不跟随符号链接的目录项类型。
- 目录项为目录时继续递归复制，为普通文件时执行 `fs::copy`。
- 符号链接、FIFO 或其它非目录/非普通文件条目现在返回 `unsupported file type`，不再静默跟随或落入普通复制分支。
- 目录项类型探测失败现在返回 `Failed to inspect ... under ...`，不再折叠为复制失败。

### 文件变更清单

- 修改：`src/runtime/managed_runtime.rs`，收紧受管运行时复制降级的目录项类型判定，并补充符号链接与 Unix 特殊文件测试。
- 修改：`TASK_LOG.md`，追加本轮任务记录。

### 关键代码调整详情

- `copy_dir_recursive` 在每个 `DirEntry` 上读取 `entry.file_type()`，该 API 不跟随符号链接，符合长期可预测复制语义。
- 复制分支从“非目录都复制”改为“目录递归、普通文件复制、其它类型显式拒绝”。
- 新增 `copy_dir_recursive_rejects_symlink_entry`，构造源构建目录内的文件符号链接，断言复制降级返回 `unsupported file type` 并包含宿主可见路径。
- 新增 Unix 专属 `copy_dir_recursive_rejects_unsupported_unix_file_type`，用 FIFO 覆盖非目录/非普通文件条目的显式拒绝语义。

### 验证记录

- 格式化：`cargo fmt` 通过。
- 目标验证：`cargo test copy_dir_recursive_rejects_symlink_entry -- --nocapture` 通过，1 个匹配测试通过。
- 范围验证：`cargo test runtime::managed_runtime -- --nocapture` 通过，14 个 managed runtime 范围测试全部通过。
- 全量验证：`cargo test` 通过，430 个测试全部通过。
- 静态验证：`cargo clippy --all-targets --all-features -- -D warnings` 通过，无 Clippy 警告。
- 空白审核：`git diff --check -- src/runtime/managed_runtime.rs TASK_LOG.md` 通过。
- 搜索复查：`copy_dir_recursive` 已不再使用 `source_path.is_dir()`，目录项分类统一由 `entry.file_type()` 完成。

### 代码审核与遗留事项

- 修改部分代码审核确认没有引入候选路径、多来源兼容、静默 fallback、历史兼容分支或额外状态。
- 本轮没有改变 Python 环境构建命令、临时构建目录命名、目标环境目录派生、rename 优先策略、copy fallback 入口或构建目录清理策略。
- 符号链接和特殊文件现在是显式不支持的复制源类型；普通文件复制失败、目录读取失败和目录项类型探测失败继续保持不同语义。
- 当前 Windows 环境已执行符号链接拒绝测试；Unix FIFO 测试通过 `#[cfg(unix)]` 编译运行，后续可在 Unix CI 上覆盖。
- 后续循环可继续排查 `src/runtime/engine.rs`、`src/bin/luaskills-debug.rs` 或 `src/dependency/manager.rs` 中剩余 `.exists()` / `.is_dir()` 折叠点。

## 2026-07-06 第210轮：调试同步目标目录类型校验显式化

### 探索记录

- 本轮转入 `src/bin/luaskills-debug.rs`，追踪 `sync_debug_skill` 到 `synchronize_skill_into_runtime_root` 的同步流程。
- 调用链确认：`luaskills-debug sync` 解析命令后调用 `sync_debug_skill`，先通过 `load_bound_skill_manifest` 读取源 skill，再调用 `ensure_debug_runtime_layout` 创建运行时布局，最后把源目录同步到 `runtime_root/skills/{skill_id}`。
- 路径来源确认：同步目标路径唯一由 `runtime_root.join("skills").join(skill_id)` 派生；`skill_id` 来自源 skill 目录绑定后的 `manifest.effective_skill_id()`，不存在候选目标路径或历史兼容路径。
- 问题确认：旧实现用 `target_skill_path.exists()` 决定是否执行 `remove_dir_all`；当目标路径存在但不是目录时，会把类型错误延迟到底层删除错误。
- 语义边界确认：目标缺失时应直接复制；目标是目录时可以清理后重建；目标存在但不是目录属于调试运行时同步状态损坏，应在删除前显式报告。

### 核心修复与调整概述

- 新增 `debug_sync_target_path_is_directory`，显式区分同步目标目录存在、缺失、类型错误与 metadata 探测失败。
- `synchronize_skill_into_runtime_root` 改为先调用该 helper，再决定是否 `remove_dir_all`。
- 文件占用 `runtime_root/skills/{skill_id}` 时现在返回 `Previous synchronized skill path is not a directory: ...`。
- metadata 探测失败现在返回 `Failed to inspect previous synchronized skill ...`，不再被折叠进删除失败。

### 文件变更清单

- 修改：`src/bin/luaskills-debug.rs`，收紧调试同步目标路径的目录类型契约，并补充文件型目标路径回归测试。
- 修改：`TASK_LOG.md`，追加本轮任务记录。

### 关键代码调整详情

- `debug_sync_target_path_is_directory(target_skill_path)` 使用 `fs::metadata` 校验目标是否为目录，缺失时返回 `Ok(false)`。
- `synchronize_skill_into_runtime_root` 不再用 `.exists()` 驱动删除目标目录。
- 新增 `sync_debug_skill_rejects_file_target_skill_path`，通过真实 `sync_debug_skill` 入口使用现有 `demo-standard-ffi-skill` fixture，并用普通文件占用目标同步路径。
- 测试断言错误文本包含显式目录类型错误和 `render_debug_path` 渲染后的目标路径。

### 验证记录

- 格式化：`cargo fmt` 通过。
- 目标验证：`cargo test sync_debug_skill_rejects_file_target_skill_path -- --nocapture` 通过，1 个匹配测试通过。
- 范围验证：`cargo test --bin luaskills-debug -- --nocapture` 通过，10 个 luaskills-debug 测试全部通过。
- 全量验证：`cargo test` 通过，431 个测试全部通过。
- 静态验证：`cargo clippy --all-targets --all-features -- -D warnings` 通过，无 Clippy 警告。
- 空白审核：`git diff --check -- src/bin/luaskills-debug.rs TASK_LOG.md` 通过。
- 搜索复查：`synchronize_skill_into_runtime_root` 已不再使用 `target_skill_path.exists()` 作为删除条件，改用 `debug_sync_target_path_is_directory`。

### 代码审核与遗留事项

- 修改部分代码审核确认没有引入候选路径、多来源兼容、静默 fallback、历史兼容分支或额外状态。
- 本轮没有改变源 skill manifest 解析、runtime root 布局、skill id 派生、同目录短路、递归复制、输出结构或调试执行路径。
- 文件型同步目标现在是显式目录类型错误；目标缺失、metadata 探测失败和删除失败继续保持不同语义。
- 后续循环可继续排查 `prepare_debug_runtime` 的同步清单存在性判断、`collect_ignored_skill_ids` 的 skills 根目录探测，或 `src/dependency/manager.rs` 中剩余目录清理契约。

## 2026-07-06 第211轮：调试源 skill 路径探测错误显式化

### 探索记录

- 本轮继续排查 `src/bin/luaskills-debug.rs`，定位到 `load_bound_skill_manifest` 的源 skill 目录校验。
- 调用链确认：`sync_debug_skill` 会把 `--skill-path` 经 `absolutize_path` 后传入 `load_bound_skill_manifest`；`prepare_debug_runtime` 在按 skill id 运行时也会对已同步目录调用同一个 manifest 加载入口。
- 路径来源确认：该函数的 `skill_path` 是单一目录路径输入；同步场景来自用户提供源路径，按 id 运行场景来自 `runtime_root/skills/{skill_id}`，不存在候选路径或多来源兼容需求。
- 问题确认：旧实现用 `skill_path.is_dir()`，会把 metadata 探测失败折叠为 false，然后统一报 `Skill path ... is not a directory`。
- 语义边界确认：缺失路径或文件路径仍可报告为不是目录；包含内嵌 NUL 等无法探测的路径属于文件系统探测错误，应在读取 `skill.yaml` 前显式报告。

### 核心修复与调整概述

- 新增 `debug_skill_source_path_is_directory`，显式区分源 skill 路径是目录、缺失/非目录、metadata 探测失败。
- `load_bound_skill_manifest` 改为先调用该 helper，再保留原有非目录错误文本。
- metadata 探测失败现在返回 `Failed to inspect skill path ...`，不再伪装成普通非目录。
- 缺失路径和普通文件路径仍返回 `Skill path ... is not a directory`，保持面向用户的语义简洁。

### 文件变更清单

- 修改：`src/bin/luaskills-debug.rs`，收紧源 skill 路径目录探测，并补充非法路径探测错误回归测试。
- 修改：`TASK_LOG.md`，追加本轮任务记录。

### 关键代码调整详情

- `debug_skill_source_path_is_directory(skill_path)` 使用 `fs::metadata`，目录返回 true，缺失或非目录返回 false，非 NotFound 探测错误返回显式错误。
- `load_bound_skill_manifest` 不再直接调用 `skill_path.is_dir()`。
- 新增 `load_bound_skill_manifest_reports_skill_path_probe_errors`，构造包含内嵌 NUL 的 `PathBuf`，断言错误以 `Failed to inspect skill path` 开头。
- 既有 `load_bound_skill_manifest_parse_error_uses_host_visible_path` 继续覆盖真实目录下 `skill.yaml` 解析失败的路径渲染。

### 验证记录

- 格式化：`cargo fmt` 通过。
- 目标验证：`cargo test load_bound_skill_manifest_reports_skill_path_probe_errors -- --nocapture` 通过，1 个匹配测试通过。
- 范围验证：`cargo test --bin luaskills-debug -- --nocapture` 通过，11 个 luaskills-debug 测试全部通过。
- 全量验证：`cargo test` 通过，432 个测试全部通过。
- 静态验证：`cargo clippy --all-targets --all-features -- -D warnings` 通过，无 Clippy 警告。
- 空白审核：`git diff --check -- src/bin/luaskills-debug.rs TASK_LOG.md` 通过。
- 搜索复查：`load_bound_skill_manifest` 已不再使用 `skill_path.is_dir()`，改用 `debug_skill_source_path_is_directory`。

### 代码审核与遗留事项

- 修改部分代码审核确认没有引入候选路径、多来源兼容、静默 fallback、历史兼容分支或额外状态。
- 本轮没有改变路径绝对化、目录名 skill_id 绑定、manifest YAML 读取/解析、entry schema 解析、runtime root 布局或同步复制流程。
- 源路径探测失败现在是显式探测错误；缺失路径、普通文件路径和 manifest 读取/解析失败继续保持不同语义。
- 后续循环可继续排查 `prepare_debug_runtime` 的 `skill.yaml` 存在性判断、`collect_ignored_skill_ids` 的 skills 根目录探测，或转入 `src/dependency/manager.rs` 的目录清理入口。

## 2026-07-06 第212轮：调试已同步 skill 清单文件类型校验显式化

### 探索记录

- 本轮继续追踪 `src/bin/luaskills-debug.rs` 中的 `prepare_debug_runtime`。
- 调用链确认：按 `--skill-id` 运行时，`prepare_debug_runtime` 先构造 `synced_skill_path = runtime_root/skills/{skill_id}`，随后用 `skill.yaml` 判断该 skill 是否已同步，再调用 `load_bound_skill_manifest(&synced_skill_path)`。
- 路径来源确认：已同步清单路径唯一来自 `synced_skill_path.join("skill.yaml")`；`synced_skill_path` 唯一由 runtime root 与 skill id 派生，不存在候选清单路径或历史兼容路径。
- 问题确认：旧实现用 `synced_skill_path.join("skill.yaml").exists()`，会把目录型 `skill.yaml` 当作已存在，随后延迟到 YAML 读取阶段失败。
- 语义边界确认：缺失清单应继续触发“先 sync”的用户提示；清单路径存在但不是文件属于已同步 skill 结构损坏，应在读取 YAML 前显式报告。

### 核心修复与调整概述

- 新增 `debug_synced_skill_manifest_path_is_file`，显式区分已同步清单文件存在、缺失、类型错误与 metadata 探测失败。
- `prepare_debug_runtime` 改为先计算 `synced_skill_manifest_path`，再通过 helper 判断是否可加载。
- 目录型 `skill.yaml` 现在返回 `Synchronized skill manifest is not a file: ...`。
- 缺失 `skill.yaml` 继续走原有“Run luaskills-debug sync ... first”提示。

### 文件变更清单

- 修改：`src/bin/luaskills-debug.rs`，收紧已同步 skill 清单文件类型契约，并补充目录型清单回归测试。
- 修改：`TASK_LOG.md`，追加本轮任务记录。

### 关键代码调整详情

- `debug_synced_skill_manifest_path_is_file(manifest_path)` 使用 `fs::metadata`，只允许 `metadata.is_file()` 作为可加载清单。
- `prepare_debug_runtime` 不再用 `.exists()` 判断 `skill.yaml` 是否可用。
- 新增 `prepare_debug_runtime_rejects_directory_synced_skill_manifest`，构造 `runtime_root/skills/demo-skill/skill.yaml` 目录，并通过真实 `prepare_debug_runtime` 入口断言类型错误。
- 既有按源路径同步并执行、按 skill id 执行的测试继续覆盖正常调试运行路径。

### 验证记录

- 格式化：`cargo fmt` 通过。
- 目标验证：`cargo test prepare_debug_runtime_rejects_directory_synced_skill_manifest -- --nocapture` 通过，1 个匹配测试通过。
- 范围验证：`cargo test --bin luaskills-debug -- --nocapture` 通过，12 个 luaskills-debug 测试全部通过。
- 全量验证：`cargo test` 通过，433 个测试全部通过。
- 静态验证：`cargo clippy --all-targets --all-features -- -D warnings` 通过，无 Clippy 警告。
- 空白审核：`git diff --check -- src/bin/luaskills-debug.rs TASK_LOG.md` 通过。
- 搜索复查：`prepare_debug_runtime` 已不再使用 `skill.yaml.exists()`，改为 `debug_synced_skill_manifest_path_is_file`。

### 代码审核与遗留事项

- 修改部分代码审核确认没有引入候选路径、多来源兼容、静默 fallback、历史兼容分支或额外状态。
- 本轮没有改变 skill id 解析、runtime root 布局、sync 提示文本、manifest YAML 解析、engine 加载、entry 过滤或调用执行流程。
- 目录型已同步清单现在是显式文件类型错误；缺失清单、metadata 探测失败和 YAML 读取/解析失败继续保持不同语义。
- 后续循环可继续排查 `collect_ignored_skill_ids` 的 skills 根目录探测、`paths_refer_to_same_directory` 的存在性判断，或转入 `src/dependency/manager.rs` 的目录清理入口。

## 2026-07-06 第213轮：失败依赖安装根目录清理类型校验显式化

### 探索记录

- 本轮转入 `src/dependency/manager.rs`，追踪依赖安装失败后的清理与重试流程。
- 调用链确认：`ensure_dependency` 在已存在缓存归档安装失败时，先记录告警，再调用 `cleanup_failed_dependency_install_attempt(download_path, resolved_request.install_root)`，随后重新下载并重试安装。
- 路径来源确认：`install_root` 唯一来自 `ResolvedDependencyRequest.install_root`，该路径由依赖类型、scope、name、version 与 platform 组合派生，不存在候选安装根或历史兼容路径。
- 清理顺序确认：失败重试前先清理下载文件，再清理安装根；下载文件与安装根分别有单一入参和单一清理语义。
- 问题确认：旧的 `remove_failed_dependency_install_root` 直接调用 `fs::remove_dir_all`，缺失目录通过 `NotFound` 特判视为已清理，但文件型安装根会延迟到底层删除错误，缺少明确的“安装根不是目录”状态诊断。
- 语义边界确认：缺失安装根应继续视为无需清理；存在且是目录时执行递归删除；存在但不是目录属于失败安装产物状态损坏，应在递归删除前显式报错；真实目录删除失败应继续保留删除失败语义。

### 核心修复与调整概述

- 新增 `failed_dependency_install_root_is_directory`，在删除前显式区分失败依赖安装根目录存在、缺失、类型错误与 metadata 探测失败。
- `remove_failed_dependency_install_root` 改为先调用目录类型 helper，确认是目录后才执行 `remove_dir_all`。
- 文件型失败安装根现在返回 `Failed dependency install root is not a directory before reinstall: ...`，不再依赖底层递归删除错误文本。
- 缺失安装根仍返回成功，保持重试清理流程对已清理状态的幂等语义。

### 文件变更清单

- 修改：`src/dependency/manager.rs`，补充失败依赖安装根目录类型探测 helper，并接入失败安装重试清理流程。
- 修改：`src/dependency/manager/tests.rs`，收紧文件型安装根回归测试的错误断言。
- 修改：`TASK_LOG.md`，追加本轮任务记录。

### 关键代码调整详情

- `failed_dependency_install_root_is_directory(install_root)` 使用 `fs::metadata` 检查具体安装根路径。
- `metadata.is_dir()` 为真时返回 `Ok(true)`，表示可以安全进入递归删除。
- `ErrorKind::NotFound` 返回 `Ok(false)`，表示安装根已不存在且无需清理。
- metadata 成功但不是目录时返回显式类型错误，metadata 探测失败时返回显式探测错误。
- `cleanup_failed_dependency_install_attempt_rejects_install_root_file` 现在断言完整类型错误文本和宿主可见路径，避免测试继续接受模糊底层删除错误。

### 验证记录

- 格式化：`cargo fmt` 通过。
- 目标验证：`cargo test cleanup_failed_dependency_install_attempt_rejects_install_root_file -- --nocapture` 通过，1 个匹配测试通过。
- 范围验证：`cargo test dependency::manager -- --nocapture` 通过，17 个 dependency manager 范围测试全部通过。
- 全量验证：`cargo test` 通过，433 个测试全部通过。
- 静态验证：`cargo clippy --all-targets --all-features -- -D warnings` 通过，无 Clippy 警告。
- 空白审核：`git diff --check -- src/dependency/manager.rs src/dependency/manager/tests.rs TASK_LOG.md` 通过。
- 搜索复查：`remove_failed_dependency_install_root` 已通过 `failed_dependency_install_root_is_directory` 区分缺失、类型错误和删除失败。

### 代码审核与遗留事项

- 修改部分代码审核确认没有引入候选路径、多来源兼容、静默 fallback、历史兼容分支或额外状态。
- 本轮没有改变依赖解析、下载 URL 选择、缓存归档安装、重新下载、归档解压、export 检测或 install root 派生逻辑。
- 失败安装根的缺失、类型错误、metadata 探测失败和真实目录删除失败现在具备独立诊断边界。
- 后续循环可继续排查 `remove_stale_dependency_root`、`remove_skill_private_dependency_root`、`collect_ignored_skill_ids` 或 dependency manager 中其它 `.exists()` / `.is_dir()` 折叠点。

## 2026-07-06 第214轮：更新后过期依赖根目录清理类型校验显式化

### 探索记录

- 本轮继续排查 `src/dependency/manager.rs`，聚焦更新 skill 后的过期依赖根目录清理入口。
- 调用链确认：`cleanup_updated_skill_dependencies` 在单个 skill 更新成功后执行，先通过 `collect_skill_local_dependency_roots` 分别收集旧清单与新清单在当前平台下的技能私有依赖安装根，再遍历 `previous_roots.difference(&current_roots)` 清理只存在于旧清单中的根目录。
- 路径来源确认：过期根目录唯一来自旧清单中的 skill-local 依赖声明，经 `push_dependency_root_if_applicable` 和 `build_dependency_install_root` 按依赖类型、scope、skill_id、dependency name、version 与 platform 派生，不存在候选路径或历史兼容路径。
- 问题确认：旧的 `remove_stale_dependency_root` 直接执行 `fs::remove_dir_all`，仅把 `NotFound` 视为已清理；当过期根路径被普通文件占用时，会把状态类型错误延迟到底层递归删除错误。
- 语义边界确认：过期根缺失应继续视为无需清理；过期根存在且是目录时才允许递归删除；过期根存在但不是目录属于依赖存储状态损坏，应在删除前显式报错；真实目录删除失败继续保留删除失败语义。

### 核心修复与调整概述

- 新增 `stale_dependency_root_is_directory`，在删除前显式区分过期依赖根存在、缺失、类型错误与 metadata 探测失败。
- `remove_stale_dependency_root` 改为先调用目录类型 helper，确认是目录后才执行 `remove_dir_all`。
- 文件型过期根现在返回 `Stale dependency root is not a directory before update cleanup: ...`，不再依赖底层递归删除错误。
- 非法路径等 metadata 探测失败现在返回 `Failed to inspect stale dependency root ... before update cleanup: ...`，与真实目录删除失败分离。

### 文件变更清单

- 修改：`src/dependency/manager.rs`，补充过期依赖根目录类型探测 helper，并接入更新后清理流程。
- 修改：`src/dependency/manager/tests.rs`，新增文件型过期根回归测试，并更新非法路径测试断言。
- 修改：`TASK_LOG.md`，追加本轮任务记录。

### 关键代码调整详情

- `stale_dependency_root_is_directory(stale_root)` 使用 `fs::metadata` 检查从旧清单派生出的具体过期依赖根。
- `metadata.is_dir()` 为真时返回 `Ok(true)`，允许后续递归删除。
- `ErrorKind::NotFound` 返回 `Ok(false)`，表示该过期根已经不存在。
- metadata 成功但不是目录时返回显式类型错误，metadata 探测失败时返回显式探测错误。
- 新增 `cleanup_updated_skill_dependencies_rejects_file_stale_root`，通过真实更新清理入口构造旧清单有依赖、新清单为空、过期根路径被文件占用的回归场景。
- `cleanup_updated_skill_dependencies_reports_invalid_stale_root_path` 现在断言探测失败语义，避免非法路径继续落入删除失败诊断。

### 验证记录

- 格式化：`cargo fmt` 通过。
- 目标验证：`cargo test cleanup_updated_skill_dependencies_rejects_file_stale_root -- --nocapture` 通过，1 个匹配测试通过。
- 目标验证：`cargo test cleanup_updated_skill_dependencies_reports_invalid_stale_root_path -- --nocapture` 通过，1 个匹配测试通过。
- 范围验证：`cargo test dependency::manager -- --nocapture` 通过，18 个 dependency manager 范围测试全部通过。
- 全量验证：`cargo test` 通过，434 个测试全部通过。
- 静态验证：`cargo clippy --all-targets --all-features -- -D warnings` 通过，无 Clippy 警告。
- 空白审核：`git diff --check -- src/dependency/manager.rs src/dependency/manager/tests.rs TASK_LOG.md` 通过。
- 搜索复查：`remove_stale_dependency_root` 已通过 `stale_dependency_root_is_directory` 区分缺失、类型错误、metadata 探测失败和删除失败。

### 代码审核与遗留事项

- 修改部分代码审核确认没有引入候选路径、多来源兼容、静默 fallback、历史兼容分支或额外状态。
- 本轮没有改变依赖根集合收集、manifest 差集计算、平台适配判断、scope 判断、install root 派生或更新清理触发时机。
- 过期依赖根的缺失、类型错误、metadata 探测失败和真实目录删除失败现在具备独立诊断边界。
- 后续循环可继续排查 `remove_skill_private_dependency_root` 的卸载后私有依赖根清理语义，或转入 `collect_ignored_skill_ids`、`paths_refer_to_same_directory` 等目录探测折叠点。

## 2026-07-06 第215轮：卸载后私有依赖根目录清理类型校验显式化

### 探索记录

- 本轮继续排查 `src/dependency/manager.rs`，聚焦卸载 skill 后的私有依赖根目录清理入口。
- 调用链确认：`cleanup_uninstalled_skill_dependencies` 会构造包含 ROOT 与可选 PROJECT 的 `RuntimeSkillRoot` 列表，再调用 `cleanup_uninstalled_skill_dependencies_from_roots`；当前后者实际只调用 `remove_skill_private_dependency_roots(removed_skill_id)`。
- 路径来源确认：`remove_skill_private_dependency_roots` 固定遍历 `self.config.tool_root.join(skill_id)`、`self.config.lua_root.join(skill_id)`、`self.config.ffi_root.join(skill_id)` 三个私有依赖根；这些路径只由依赖配置根和已移除技能标识符派生，不存在候选路径或历史兼容路径。
- 范围事实确认：`skill_roots` 与 `removed_manifest` 当前在卸载清理实现中仅通过 `let _ = (...)` 消除未使用告警，尚未参与实际删除决策；本轮不扩大语义范围。
- 问题确认：旧的 `remove_skill_private_dependency_root` 直接调用 `fs::remove_dir_all`，仅把 `NotFound` 视为已清理；当私有根路径被普通文件占用时，会把类型错误延迟到底层递归删除错误。
- 语义边界确认：缺失私有根应继续视为无需清理；私有根存在且是目录时才允许递归删除；私有根存在但不是目录属于卸载后依赖存储状态损坏，应在删除前显式报错；真实目录删除失败继续保留删除失败语义。

### 核心修复与调整概述

- 新增 `skill_private_dependency_root_is_directory`，在卸载清理删除前显式区分私有依赖根存在、缺失、类型错误与 metadata 探测失败。
- `remove_skill_private_dependency_root` 改为先调用目录类型 helper，确认是目录后才执行 `remove_dir_all`。
- 文件型私有根现在返回 `Skill-private dependency root is not a directory before uninstall cleanup: ...`，不再依赖底层递归删除错误。
- 非法路径等 metadata 探测失败现在返回 `Failed to inspect skill-private dependency root ... before uninstall cleanup: ...`，与真实目录删除失败分离。
- 真实目录删除失败消息调整为 `Failed to remove skill-private dependency root ... before uninstall cleanup: ...`，让错误来源与卸载场景更明确。

### 文件变更清单

- 修改：`src/dependency/manager.rs`，补充卸载私有依赖根目录类型探测 helper，并接入卸载清理流程。
- 修改：`src/dependency/manager/tests.rs`，新增文件型私有根回归测试，并更新非法路径测试断言。
- 修改：`TASK_LOG.md`，追加本轮任务记录。

### 关键代码调整详情

- `skill_private_dependency_root_is_directory(root)` 使用 `fs::metadata` 检查从已移除技能标识符派生出的具体私有依赖根。
- `metadata.is_dir()` 为真时返回 `Ok(true)`，允许后续递归删除。
- `ErrorKind::NotFound` 返回 `Ok(false)`，表示该私有根已经不存在。
- metadata 成功但不是目录时返回显式类型错误，metadata 探测失败时返回显式探测错误。
- 新增 `cleanup_uninstalled_skill_dependencies_rejects_file_private_root`，通过真实卸载清理入口构造 `tool_root/{skill_id}` 被普通文件占用的回归场景。
- `cleanup_uninstalled_skill_dependencies_reports_invalid_private_root_path` 现在断言探测失败语义，避免非法路径继续落入删除失败诊断。

### 验证记录

- 格式化：`cargo fmt` 通过。
- 目标验证：`cargo test cleanup_uninstalled_skill_dependencies_rejects_file_private_root -- --nocapture` 通过，1 个匹配测试通过。
- 目标验证：`cargo test cleanup_uninstalled_skill_dependencies_reports_invalid_private_root_path -- --nocapture` 通过，1 个匹配测试通过。
- 范围验证：`cargo test dependency::manager -- --nocapture` 通过，19 个 dependency manager 范围测试全部通过。
- 全量验证：`cargo test` 通过，435 个测试全部通过。
- 静态验证：`cargo clippy --all-targets --all-features -- -D warnings` 通过，无 Clippy 警告。
- 空白审核：`git diff --check -- src/dependency/manager.rs src/dependency/manager/tests.rs TASK_LOG.md` 通过。
- 搜索复查：`remove_skill_private_dependency_root` 已通过 `skill_private_dependency_root_is_directory` 区分缺失、类型错误、metadata 探测失败和删除失败。

### 代码审核与遗留事项

- 修改部分代码审核确认没有引入候选路径、多来源兼容、静默 fallback、历史兼容分支或额外状态。
- 本轮没有改变卸载清理入口签名、ROOT/PROJECT skill root 构造、`removed_manifest` 处理、三个私有依赖根的访问顺序或依赖配置根派生方式。
- 私有依赖根的缺失、类型错误、metadata 探测失败和真实目录删除失败现在具备独立诊断边界。
- 后续循环可继续排查 `collect_ignored_skill_ids` 的 skills 根目录探测、`paths_refer_to_same_directory` 的存在性判断，或 dependency manager 中剩余目录扫描折叠点。

## 2026-07-06 第216轮：调试忽略 skill 收集的 skills 根目录探测显式化

### 探索记录

- 本轮转入 `src/bin/luaskills-debug.rs`，追踪 `collect_ignored_skill_ids` 的运行时 `skills/` 根目录探测。
- 调用链确认：`prepare_debug_runtime` 在完成 runtime root 绝对化、布局创建、目标 skill 解析、已同步清单校验和 manifest 加载后，调用 `collect_ignored_skill_ids(&runtime_root.join("skills"), &skill_id)`，再把结果写入 `LuaRuntimeHostOptions.ignored_skill_ids`。
- 路径来源确认：`skills_dir` 唯一来自 `runtime_root.join("skills")`；`runtime_root` 由调试命令参数绝对化得到，不存在候选 skills 根路径或历史兼容路径。
- 完整入口事实确认：`prepare_debug_runtime` 会先调用 `ensure_debug_runtime_layout` 创建 `runtime_root/skills`，因此文件型 `skills` 根在完整入口通常会更早于 `collect_ignored_skill_ids` 失败；但 `collect_ignored_skill_ids` 本身仍是独立 helper，需要具备清晰输入契约。
- 问题确认：旧实现使用 `skills_dir.exists()`，会把探测失败折叠进布尔判断，并让文件型 `skills` 根延迟到 `read_dir` 枚举错误。
- 语义边界确认：缺失 `skills/` 根目录表示没有其它 skill 可忽略，应返回空列表；存在且是目录时才枚举；存在但不是目录属于 runtime 布局状态损坏，应在枚举前显式报错；metadata 探测失败应独立于目录枚举失败。

### 核心修复与调整概述

- 新增 `debug_runtime_skills_path_is_directory`，在收集忽略 skill 前显式区分运行时 `skills/` 路径存在、缺失、类型错误与 metadata 探测失败。
- `collect_ignored_skill_ids` 改为先调用该 helper；确认缺失时仍返回空列表，保持原有缺失语义。
- 文件型 `skills/` 根现在返回 `Runtime skills path is not a directory: '...'`，不再延迟为目录枚举失败。
- 非法路径等 metadata 探测失败现在返回 `Failed to inspect runtime skills directory '...': ...`，与 `read_dir` 枚举失败分离。

### 文件变更清单

- 修改：`src/bin/luaskills-debug.rs`，补充运行时 `skills/` 根目录类型探测 helper，并接入忽略 skill 收集流程。
- 修改：`TASK_LOG.md`，追加本轮任务记录。

### 关键代码调整详情

- `debug_runtime_skills_path_is_directory(skills_dir)` 使用 `fs::metadata` 检查具体 `runtime_root/skills` 路径。
- `metadata.is_dir()` 为真时返回 `Ok(true)`，允许后续 `read_dir` 枚举。
- `ErrorKind::NotFound` 返回 `Ok(false)`，使缺失 `skills/` 根继续得到空忽略列表。
- metadata 成功但不是目录时返回显式类型错误，metadata 探测失败时返回显式探测错误。
- 新增 `collect_ignored_skill_ids_accepts_missing_skills_dir`，验证缺失 `skills/` 根返回空列表。
- 新增 `collect_ignored_skill_ids_rejects_file_skills_dir`，验证文件型 `skills/` 根在枚举前报类型错误。
- 新增 `collect_ignored_skill_ids_reports_skills_dir_probe_errors`，验证非法路径不会被当作缺失目录处理。

### 验证记录

- 格式化：`cargo fmt` 通过。
- 目标验证：`cargo test collect_ignored_skill_ids_accepts_missing_skills_dir -- --nocapture` 通过，1 个匹配测试通过。
- 目标验证：`cargo test collect_ignored_skill_ids_rejects_file_skills_dir -- --nocapture` 通过，1 个匹配测试通过。
- 目标验证：`cargo test collect_ignored_skill_ids_reports_skills_dir_probe_errors -- --nocapture` 通过，1 个匹配测试通过。
- 范围验证：`cargo test --bin luaskills-debug -- --nocapture` 通过，15 个 luaskills-debug 测试全部通过。
- 全量验证：`cargo test` 通过，438 个测试全部通过。
- 静态验证：`cargo clippy --all-targets --all-features -- -D warnings` 通过，无 Clippy 警告。
- 空白审核：`git diff --check -- src/bin/luaskills-debug.rs TASK_LOG.md` 通过。
- 搜索复查：`collect_ignored_skill_ids` 已不再使用 `skills_dir.exists()`，改为 `debug_runtime_skills_path_is_directory`。

### 代码审核与遗留事项

- 修改部分代码审核确认没有引入候选路径、多来源兼容、静默 fallback、历史兼容分支或额外状态。
- 本轮没有改变调试 runtime 布局创建、同步清单加载、忽略列表排序、目标 skill 排除、UTF-8 文件名过滤、skill_id 校验或 host options 构造。
- 运行时 `skills/` 根的缺失、类型错误、metadata 探测失败和目录枚举失败现在具备独立诊断边界。
- 后续循环可继续排查 `paths_refer_to_same_directory` 的存在性判断，或转入 `ensure_debug_runtime_layout` 的目录创建类型错误前置诊断。

## 2026-07-06 第217轮：调试同步同目录判断探测错误显式化

### 探索记录

- 本轮继续排查 `src/bin/luaskills-debug.rs`，聚焦 `paths_refer_to_same_directory` 的同目录判断。
- 调用链确认：`synchronize_skill_into_runtime_root` 先构造 `target_skill_path = runtime_root.join("skills").join(skill_id)`，随后调用 `paths_refer_to_same_directory(source_skill_path, target_skill_path)`；只有判断为同一物理目录时才跳过删除与复制。
- 路径来源确认：左侧 `source_skill_path` 来自调试命令 `--skill-path` 绝对化后的源 skill 目录；右侧 `target_skill_path` 唯一来自 runtime root、`skills` 固定段和已绑定 skill id，不存在候选目标路径或历史兼容路径。
- 问题确认：旧实现使用 `left.exists() || right.exists()` 的布尔判断，会把 metadata 探测失败折叠进存在性逻辑；非法路径可能延迟到 canonicalize 或被误处理为普通不存在。
- 语义边界确认：同目录判断只负责在两侧都是已存在目录时执行 canonicalize 比较；缺失或非目录路径应返回 false，让后续同步目标校验继续处理；metadata 探测失败应在 canonicalize 前显式报错。

### 核心修复与调整概述

- 新增 `same_directory_candidate_path_is_directory`，在同目录比较前显式探测 source/target 两侧候选路径。
- `paths_refer_to_same_directory` 改为只有两侧都确认是已存在目录时才进入 canonicalize。
- source 侧探测失败现在返回 `Failed to inspect source path ... before same-directory comparison: ...`。
- target 侧探测失败现在返回 `Failed to inspect target path ... before same-directory comparison: ...`。
- 缺失或非目录路径仍返回 `Ok(false)`，保持后续目标路径类型校验负责报错的原有职责边界。

### 文件变更清单

- 修改：`src/bin/luaskills-debug.rs`，补充同目录比较候选路径探测 helper，并接入同步前短路判断。
- 修改：`TASK_LOG.md`，追加本轮任务记录。

### 关键代码调整详情

- `same_directory_candidate_path_is_directory(path, path_label)` 使用 `fs::metadata` 检查单侧候选路径。
- `metadata.is_dir()` 为真时返回 `Ok(true)`，允许参与 canonicalize 比较。
- `ErrorKind::NotFound` 返回 `Ok(false)`，表示该侧不存在且不构成同目录。
- metadata 成功但不是目录时返回 `Ok(false)`，让同步目标目录校验继续给出更具体的目标类型错误。
- metadata 探测失败时返回带有 source/target 标签的显式错误。
- 新增 `paths_refer_to_same_directory_reports_source_probe_errors`，验证非法 source 路径不会进入 canonicalize。
- 新增 `paths_refer_to_same_directory_reports_target_probe_errors`，验证非法 target 路径不会进入 canonicalize。

### 验证记录

- 格式化：`cargo fmt` 通过。
- 目标验证：`cargo test paths_refer_to_same_directory_reports_source_probe_errors -- --nocapture` 通过，1 个匹配测试通过。
- 目标验证：`cargo test paths_refer_to_same_directory_reports_target_probe_errors -- --nocapture` 通过，1 个匹配测试通过。
- 范围验证：`cargo test --bin luaskills-debug -- --nocapture` 通过，17 个 luaskills-debug 测试全部通过。
- 全量验证：`cargo test` 通过，440 个测试全部通过。
- 静态验证：`cargo clippy --all-targets --all-features -- -D warnings` 通过，无 Clippy 警告。
- 空白审核：`git diff --check -- src/bin/luaskills-debug.rs TASK_LOG.md` 通过。
- 搜索复查：`paths_refer_to_same_directory` 已不再使用双侧 `exists()`，改为 `same_directory_candidate_path_is_directory`。

### 代码审核与遗留事项

- 修改部分代码审核确认没有引入候选路径、多来源兼容、静默 fallback、历史兼容分支或额外状态。
- 本轮没有改变同步目标路径派生、同目录短路语义、同步目标目录类型校验、旧目标删除、递归复制或符号链接拒绝策略。
- 同目录比较中的缺失路径、非目录路径、metadata 探测失败和 canonicalize 失败现在具备清晰职责边界。
- 根据用户收尾指示，本轮记录后不再开启新的循环轮次；后续若继续，可优先排查 `ensure_debug_runtime_layout` 的目录创建前置类型诊断。
