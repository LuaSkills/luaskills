# 接入与协议风险连续复审记录

## 一、背景与结束条件

- 审核基线：`HEAD c9bd51910a64fd6c4868b5c0df6d9f5b662fc63f` 加当前全部未提交修改。
- 任务重点：公开 Rust API、标准 C ABI、JSON FFI、手写 Python/TypeScript/Go/C 绑定、配置契约、持久化格式、缓存迁移、事件语义与发布身份。
- 结束条件：从最后一次真实问题修复完成后，连续完成五轮全仓零问题复审；任一轮发现问题均清零重计。
- 当前计数：`5/5`。I04 修复后的五轮完整全仓门禁已连续通过，本轮目标已满足。

## 二、协议资产与权威来源

| 协议面 | 权威来源 | 消费方/验证方 |
| --- | --- | --- |
| Rust 公开 API | `src/lib.rs`、公开模块与 `pub` 符号 | Rust 外部兼容测试、公开调用点 |
| 标准 C ABI | `src/ffi_standard.rs`、`src/ffi_standard/types.rs` | `include/luaskills_ffi.h`、C/Python/TypeScript/Go 示例 |
| JSON FFI | `src/ffi.rs`、`src/ffi/requests.rs` | `include/luaskills_json_ffi.h`、Python/TypeScript JSON 客户端 |
| 技能配置契约 | `src/bin/generate_skill_config_contract.rs` 与相关常量 | `contracts/skill-config/v1/contract.json`、配置工具和 FFI |
| 持久化格式 | 配置文档、受管运行时 marker、安装记录与事件类型 | 读写测试、版本常量、升级文档 |
| 发行身份 | `Cargo.toml` 版本与 `luaskills_ffi_version*` | crate、DLL、SDK、Release 与升级说明 |

## 三、已确认并修复的问题

### I01：Python V1 标准 ABI 类型导出缺失

- 分类：实际 BUG、接入阻断、必须修改。
- 事实：`examples/ffi/python/lifecycle_demo.py` 与 `query_demo.py` 从共享 `demo.py` 导入 `FfiLuaEngineOptions`，但共享模块只保留了 V3 类型。生命周期示例启动即抛出 `ImportError`，没有进入 DLL 调用。
- 原因：主示例升级为 V3 后删除了共享 V1 engine wrapper，而仍使用 `luaskills_ffi_engine_new` 的两个 V1 示例没有同步。
- 修复：在共享 Python 绑定中恢复与公共头文件一致的 V1 `FfiLuaEngineOptions(pool, host)`，不改变 DLL 或头文件协议。
- 修改后收益：生命周期与查询示例重新可运行；V1/V3 示例可以共存，不再以删除旧绑定的方式制造假升级。
- 连带影响：无 ABI 变化，仅恢复缺失的 Python 类型；新增布局回归测试防止再次删除或错序。

### I02：多份手写 V1 Host Options 布局落后于公共头文件

- 分类：实际 BUG、内存布局/协议错位、必须修改。
- 事实：TypeScript `lifecycle_demo.ts` 与 `query_demo.ts`、Python `demo_runtime` 与 `host_provider_demo` 的 V1 手写结构遗漏 `system_lua_lib_dir` 及四个来源策略字段。TypeScript 生命周期示例用 release DLL 实测返回 `database_dir_name must not be null`，证明后续字段已经按错误偏移解释。
- 原因：公共 V1 结构追加兼容字段时，只同步了主 V3 示例，多个独立手写副本没有同时更新，也没有权威头文件对照测试。
- 修复：按 `include/luaskills_ffi.h` 的精确字段顺序补齐四份绑定及其显式默认值；新增 Rust 集成测试，从公共 C 头文件解析字段顺序并对照全部 V1 以及主 V2/V3 Python/TypeScript 绑定。
- 修改后收益：避免“脚本可加载但宿主参数静默错位”的高风险错误；后续头文件变更若遗漏绑定同步，会在全量 Rust 测试中直接失败。
- 连带影响：示例布局与既有 ABI 对齐，没有增加 ABI 字段；旧错误布局本身不具备兼容价值。

### I03：在线安装示例仍引用已不存在的仓库和旧技能标识

- 分类：实际 BUG、外部接入地址失效、必须修改。
- 事实：`LuaSkills/luaskills-demo-skill` 的 GitHub Release API 返回 404，`git ls-remote` 也确认仓库不存在；实际发布仓库是 `LuaSkills/demo-skill`，当前标签为 `v0.2.1`。
- 原因：示例仓库迁移/更名后，核心仓库中的远端定位值、技能 ID、规范工具名和两处文档没有同步。
- 修复：改用 `LuaSkills/demo-skill`、`demo-skill`、`demo-skill-demo-status`，同步 FFI demo README 与中文接入指南。
- 修改后收益：在线 system install、规范工具解析与真实 Release 重新贯通；实测安装 `0.2.1` 并成功调用。
- 连带影响：旧不存在的地址不再兼容回退；这是纠正失效引用，不是新增安装源。

### I04：TypeScript 原生执行模式与源码语法/导入不一致

- 分类：实际 BUG、示例启动阻断、必须修改。
- 事实：`npm run runtime-lease` 使用 Node 原生 TypeScript 类型剥离执行，但入口导入不存在的 `json_runtime.js`；改为真实 `.ts` 路径后，Node 又明确拒绝 `json_runtime.ts` 中六处参数属性语法，说明 `tsc --noEmit` 通过并不代表 strip-only 运行兼容。
- 原因：类型检查采用完整 TypeScript 语法，而运行脚本承诺的是 Node 原生可擦除语法；原配置没有启用 `erasableSyntaxOnly`，也没有实际执行 runtime-lease 示例。
- 修复：入口显式导入 `json_runtime.ts`；六个类改为显式字段声明与构造函数赋值；`tsconfig.json` 启用 `erasableSyntaxOnly`，使未来不可擦除语法在类型检查阶段失败。
- 修改后收益：runtime-lease 示例可由文档声明的原生 Node TypeScript 路径直接运行；类型检查与真实运行时语法能力对齐。
- 连带影响：只改变示例内部 TypeScript 写法，不改变 JSON FFI 请求/响应协议。

## 四、已确认的接入风险但不应误修的项目

| 风险 | 当前事实 | 必须采取的处理 |
| --- | --- | --- |
| `PreparedSkill*` 内部字段改为 `pub(crate)` | 外部直接读写/构造这些破坏性事务字段的 Rust 源码不再编译；恢复公开会重新允许伪造删除/回滚目标 | 下一次发行必须作为安全性破坏变更披露；在 `0.x` 兼容规则下不得继续以 `0.5.x` 兼容版本发布，建议进入 `0.6.0` 发布线。当前审核不擅自执行跨 SDK 的版本发布决策 |
| 全局 Tool Cache 冲突行为 | 旧 unit 入口保持函数签名，但冲突从静默忽略变为 panic；新 `try_...` 返回 `Result`，引擎使用新入口 | 新接入统一使用 `try_configure_global_tool_cache`；旧入口只在进程启动时调用一次，并在升级说明披露行为收紧 |
| 哈希缓存路径 | 下载缓存键和不安全依赖片段改为 SHA-256，旧缓存不会自动复用 | 发布说明标明一次冷缓存；离线镜像必须通过可信打包流程预填充新布局，不能盲目探测旧路径 |
| Managed IO 外部同长覆盖 | Windows/Unix 已使用平台文件代检测可观察变化；宿主未暴露新代时不能保证无锁并发写一致性 | 继续明确为平台/并发协议边界；若产品要求绝对检测，需另行确认全内容校验或文件锁架构 |
| 当前 crate/DLL 仍报告 `0.5.5` | 未提交开发状态可以保持旧版本，但绝不能用相同版本身份发布包含破坏性变化的新产物 | 发布前必须统一核心、SDK、文档、Release 和资产版本；本地实测输出 `0.5.5` 只作为当前构建证据，不代表可发布结论 |

## 五、已完成的验证证据

| 验证 | 结果 |
| --- | --- |
| Rust 公开缓存兼容测试 | 旧 `fn(ToolCacheConfig)` 与新 `fn(ToolCacheConfig) -> Result` 类型均通过；缓存专项 16 项通过 |
| FFI 导出/头文件静态对照 | Rust 与两份头文件均识别 115 个 `luaskills_*` 函数，无单边符号 |
| FFI 专项测试 | 标准 FFI 18 项、JSON FFI 33 项通过 |
| 配置契约再生成 | 临时生成文件与 `contracts/skill-config/v1/contract.json` 逐字节一致 |
| 配置/运行时专项 | 配置存储 37 项、配置服务 16 项、受管运行时 35 项通过 |
| release DLL 主链 | C、Python、TypeScript、Go 均完成版本、engine、load、list、call、runlua、free |
| 生命周期/查询链 | Python、TypeScript、Go 的 lifecycle 与 query 示例修复后全部通过 |
| 在线安装链 | `LuaSkills/demo-skill` 最新 Release 安装成功，调用 `demo-skill-demo-status` 返回 `skill_version=0.2.1` |
| 绑定布局回归 | 公共头文件对 Python/TypeScript V1、V2、V3 手写结构测试通过 |
| TypeScript 原生 runtime-lease | `erasableSyntaxOnly` 类型检查通过；Node 原生执行完成 create/eval/status/close 与关闭后错误语义 |

## 六、已审核文件记录

本阶段已结构化读取或逐差异核对以下核心文件：

```text
Cargo.toml
src/lib.rs
src/runtime/cache.rs
src/runtime/config.rs
src/runtime/config_service.rs
src/runtime/config_tool.rs
src/runtime/managed_runtime.rs
src/skill/manager.rs
src/ffi.rs
src/ffi/requests.rs
src/ffi_standard.rs
src/ffi_standard/types.rs
include/luaskills_ffi.h
include/luaskills_json_ffi.h
contracts/skill-config/v1/contract.json
examples/ffi/c/demo.c
examples/ffi/python/demo.py
examples/ffi/python/lifecycle_demo.py
examples/ffi/python/query_demo.py
examples/ffi/typescript/demo.ts
examples/ffi/typescript/lifecycle_demo.ts
examples/ffi/typescript/query_demo.ts
examples/ffi/typescript/runtime_lease_demo.ts
examples/ffi/typescript/json_runtime.ts
examples/ffi/typescript/tsconfig.json
examples/ffi/go/demo.go
examples/ffi/go/lifecycle_demo/main.go
examples/ffi/go/query_demo/main.go
examples/ffi/demo_runtime/run_python_install_demo.py
examples/ffi/host_provider_demo/run_python_host_provider_demo.py
tests/public_api_compat.rs
```

全仓逐文件基础覆盖继续引用 `docs/reviews/20260823-FULL_REPOSITORY_CODE_AUDIT-FILE-LEDGER.md`；本次新增/修改文件会在最终五轮中重新登记。

## 七、轮次记录

| 阶段 | 重点 | 发现 | 处理 | 连续计数 |
| --- | --- | --- | --- | --- |
| 发现阶段 A | Rust 公开 API 与缓存行为 | 无新增运行时 BUG；确认破坏性版本与行为披露风险 | 记录为发行约束 | 未计数 |
| 发现阶段 B | C ABI、JSON FFI、缓冲所有权 | 无符号/签名/布局变更 | 51 项专项通过 | 未计数 |
| 发现阶段 C | 四语言真实 DLL 接入 | I01、I02 | 已修复并加入布局回归 | 归零 |
| 发现阶段 D | 在线安装与外部定位 | I03 | 已修复并实测 Release 安装/调用 | 归零 |
| 发现阶段 E | TypeScript 原生 JSON/runtime-lease 接入 | I04 | 已修复导入与不可擦除语法，并实测完整租约生命周期 | 归零 |
| 最终连续第 1 轮 | 全仓 Rust 编译/测试、公开布局回归、Clippy、rustfmt、Go 格式与差异卫生 | 无 | 720 通过、3 忽略；Clippy 零问题；格式与 `git diff --check` 通过 | 1 |
| 最终连续第 2 轮 | 配置契约、共享 watcher/事件、缓存、受管运行时、Managed IO 与持久化版本 | 无 | 契约逐字节一致；专项 37/16/19/16/35/22 通过、1 忽略；全仓 720/3 | 2 |
| 最终连续第 3 轮 | Release DLL 的 C/Python/TypeScript/Go 接入、生命周期、查询、运行时租约及在线安装 | 无 | C、Python、TypeScript、Go 全部动态接入成功；TypeScript 类型检查通过；在线安装 `demo-skill` 0.2.1 并调用成功；全仓 720/3 | 3 |
| 最终连续第 4 轮 | 安装与卸载、依赖、下载、进程会话、文件监听、标准/兼容 FFI 的接入后行为 | 无 | 专项 45/24/23/34/2/18/33 通过、1 忽略；公开 API/布局回归 2 通过；全仓 720/3；Clippy 零问题 | 4 |
| 最终连续第 5 轮 | 发布消费者视角的契约、许可证、依赖策略、包内容、跨语言语法/格式、全仓测试及工作区卫生 | 无 | 契约 SHA-256 一致；`cargo deny` 四类通过；crate 83 文件打包并验证；Python 15 文件、PowerShell 231 文件、TypeScript、Go、rustfmt、diff-check 通过；全仓 720/3；Clippy 零问题；无测试产物残留 | 5 |

最终连续五轮均从 I04 修复后的同一最终代码状态开始；发现阶段未计入零问题轮次。

## 八、最终分类与修复顺序结论

### 8.1 已修复的实际 BUG

1. `I02`：手写 FFI 结构体布局错位，可能把后续指针解释成错误字段，是最高接入风险；已通过权威头文件对照测试和四语言 DLL 实测关闭。
2. `I01`：Python V1 类型缺失导致生命周期/查询示例在载入 DLL 前直接失败；已恢复兼容类型并实测关闭。
3. `I03`：在线安装仓库、技能 ID 和工具名失效；已改为真实发布仓库并完成在线安装与调用。
4. `I04`：TypeScript 原生 strip-only 执行契约与导入/语法不一致；已通过真实 Node 运行和类型门禁关闭。

### 8.2 逻辑性优化与扩展

- 本轮没有把新增业务功能伪装成 BUG 修复。新增的 `tests/public_api_compat.rs` 属于防回归扩展：以 C 头文件为唯一权威来源，自动验证重复手写绑定布局；预期收益是将未来协议漂移从运行期内存错位提前到测试期失败。
- TypeScript `erasableSyntaxOnly` 属于开发门禁优化，不改变运行时协议；它使类型检查能力与文档承诺的 Node 原生执行能力一致。

### 8.3 必须修改、可能引发连带影响与误导性项目

- `I01` 至 `I04` 均为必须修改项，现已完成。它们修正消费者接入，不新增 DLL ABI 字段，也不改变 JSON 请求/响应协议。
- `PreparedSkill*` 字段收紧是有意的安全性 Rust 源码破坏变更；恢复公开字段会重新引入伪造事务目标风险，禁止以“兼容修复”为名回退。
- 全局 Tool Cache 冲突由静默忽略收紧为显式失败，旧入口的重复冲突调用可能 panic；新接入必须使用 `try_configure_global_tool_cache`，发布说明必须披露。
- SHA-256 缓存路径会导致一次冷缓存；不得用无校验旧路径探测规避。
- `cargo deny` 的重复依赖是依赖图事实，不等于单平台运行产物重复，当前无证据支持强行统一版本。
- MSVC `LNK4098` 是已追溯的上游 `luajit-src` 静态对象指令告警；当前 DLL import table 与四语言实测均未显示双 CRT 运行时，不能误报为已证实的 ABI 崩溃。
- `cargo package` 提示两项仓库测试未包含在发布包内；这是当前 `include` 白名单下的包内容事实，发布包能够编译验证，测试仍在源码仓库门禁执行，不属于消费者协议缺陷。

### 8.4 后续处理顺序

1. 发布前先确定新版本线。由于 `PreparedSkill*` 的 Rust 源码破坏变更，建议进入 `0.6.0`；该版本决策需维护者确认，本审核未擅自改号。
2. 同步 crate、DLL、SDK、文档、Release 与升级说明的版本身份，并明确 Tool Cache 冲突收紧、缓存冷启动及 `PreparedSkill*` 字段收紧。
3. 在发布流水线固定运行 `public_api_compat`、四语言 release DLL 示例、在线安装示例、契约再生成、`cargo deny` 与 `cargo package`。
4. 具备外部 `vldb_sqlite.dll` 后补跑 host-provider 动态联调；当前已完成布局、语法和宿主侧静态验证，但不能把缺少外部 DLL 的场景写成已完成实机验收。
