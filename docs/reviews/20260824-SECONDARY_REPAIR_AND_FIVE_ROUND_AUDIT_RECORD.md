# 次级修复与连续五轮全仓复审动态记录

## 一、任务与结束条件

- 执行日期：2026-08-24
- Git 事实基线：`c9bd51910a64fd6c4868b5c0df6d9f5b662fc63f`
- 执行方案：`docs/completed/20260823/03-SECONDARY_REPAIR_PLAN.md`
- 前置完成方案：`docs/completed/20260823/02-PRIORITY_REPAIR_PLAN.md`
- 前置审核记录：`docs/reviews/20260823-PRIORITY_REPAIR_AND_FIVE_ROUND_AUDIT_RECORD.md`
- 本轮起点：保留前置优先修复的 38 个已修改跟踪文件、`docs/reviews/`、`src/runtime/file_system.rs` 和 `tests/demo_launcher_paths.rs`；所有后续归因必须与该起点区分。
- 结束条件：完成全部满足事实和触发条件的次级工作；不满足触发条件或需要产品决策的项目必须记录证据并明确关闭或保留；随后从最终修复状态连续完成 5 轮全仓复审，任一轮发现新增问题则修复、回归并把计数清零。

## 二、当前状态

| 项目 | 当前状态 | 说明 |
| --- | --- | --- |
| S0 基线 | 已完成 | 已冻结 HEAD、前置差异、工作包事实链和验证边界 |
| S1 低风险优化 | 5/5 已完成 | P09、P10、P14、P15、P11 均已实施并通过专项测试 |
| S2 Managed IO | 已完成 | Write 与 Update 均改为增量脏区间刷盘并保留失败重试状态 |
| S3 VM 加载代 | 已完成 | 非 debug 源码由加载代 `Arc` 快照统一拥有；debug 继续实时读取 |
| S4 运行时计划缓存 | 已完成 | owner/spec/runtime 计划命中复用，命中时重新校验全部安全身份 |
| S5 FFI 所有权 | 已完成 | ABI 不变，所有跨边界缓冲区统一为精确长度 boxed slice 所有权 |
| S6 构建链路 | 已完成 | 流式下载、单闭包扫描、根 helper、发布 smoke 与共享文件监听均完成 |
| 条件项与扩展 | 已核验并关闭 | P16/P17/P21/P25、X01-X04 均不满足实施前提，保持零生产 diff |
| 文件监听复用 | 已完成 | 两域共享一个同步 `notify` 后端；保留尾随防抖、最大窗口、原子替换和 Drop join |
| 连续无问题轮次 | 当前最终状态已重新达到 5/5 | 旧 5/5 仍仅是历史快照；后续 I01-I04 修复完成后，新的五轮接入/协议风险全仓复审已连续通过，权威记录见 `docs/reviews/20260824-INTEGRATION_PROTOCOL_RISK_REAUDIT.md` |

## 三、执行范围与分类台账

| 工作包 | 问题 | 分类 | 初始决定 | 预期直接证据 |
| --- | --- | --- | --- | --- |
| S1 | P09 | 逻辑/效率优化 | 实施 | JSON 成功响应完整序列化由 2 次降为 1 次 |
| S1 | P10 | 逻辑/效率优化 | 实施 | 单次 get 只解析/克隆一次包注册项 |
| S1 | P14 | 逻辑/效率优化 | 实施 | 缓存命中 metadata 调用由 2 次降为 1 次 |
| S1 | P15 | 并发/效率优化 | 实施 | VM 在池锁外析构 |
| S1 | P11 | 逻辑/效率优化 | 实施低复杂度方案 | 纯 miss 不再写锁全表 retain |
| S2 | P05 | IO 写放大优化 | 验证后实施 | 多次 flush 写入量与新增/脏数据相关 |
| S3 | P03 | 生命周期/结构优化 | 验证唯一所有者后实施 | 非 debug 源码在同一加载代只读取一次 |
| S4 | P04 | 缓存/安全边界优化 | 验证 key 后实施 | 同 owner/spec 不重复建计划，实时安全校验保留 |
| S5 | P06 | FFI 所有权优化 | 采用 ABI 不变候选 | ptr+len 布局不变，allocation/free 完全对称 |
| S6 | P18-P20 | 构建效率与维护优化 | 实施已证明部分 | 固定块下载、闭包扫描去重、发布包自包含 |
| 条件项 | P16/P17 | 条件性维护优化 | 先验证触发条件 | 未触发则零生产 diff |
| 条件项 | P21 | CI 效率候选 | 先做覆盖与时长证据 | 无完整证据不缩减五平台覆盖 |
| 条件项 | P25 | 依赖/产物候选 | 先做发布目标证据 | 无可测收益不制造版本 churn |
| 扩展 | X01 | 公共能力扩展 | 需要真实输出分布与产品语义 | 未获得唯一语义前不改公共契约 |
| 扩展 | X02/X03 | 工程扩展 | 随 P21/P25 证据处理 | 稳定分类和可重复基线 |
| 扩展 | X04 | 架构扩展 | 默认关闭 | 维护者未否定进程全局 Tool Cache |
| 监听 | 第十五节 | 架构/效率优化 | 按本方案授权实施 | 两域共享一个 OS watcher，无隐式 runtime 失效，关闭无残留 |

## 四、基线记录

### 4.1 工作区基线

- 当前 HEAD 与初审冻结基线一致：`c9bd51910a64fd6c4868b5c0df6d9f5b662fc63f`。
- Git 跟踪文件：245 个。
- 前置优先方案造成 38 个跟踪文件修改；状态中的 3 个未跟踪入口分别为整个 `docs/reviews/` 目录、`src/runtime/file_system.rs`、`tests/demo_launcher_paths.rs`。
- 前置差异规模：38 个跟踪文件，6294 行新增、1096 行删除；这些变化不得在次级工作中回滚。

### 4.2 性能与操作计数

- P09：正常 JSON 响应完整序列化由 2 次降为 1 次。
- P10：单次配置 `get` 的包注册项解析/克隆由 2 次降为 1 次。
- P11：纯缓存 miss 不再获取写锁并执行全表过期扫描；过期回收仅在容量路径触发。
- P14：下载缓存命中复用第一次 metadata，避免第二次文件 metadata 查询。
- P15：被淘汰 VM 的析构从池锁内部移动到锁外。
- P05：确定性测试中累计写入 `1..=1000` 后 flush 由 500500 字节降为 1000 字节；20 次更新只产生 3 个脏区间相关操作。
- P03：同一非 debug 加载代的每个入口源码只读一次，VM 构造只克隆 `Arc`；reload 成功后原子替换代，失败保留旧代。
- P04：同 owner/spec/runtime 的 managed-runtime 计划从每次重建降为缓存命中；命中仍重算 lockfile、package manifest、运行时 manifest/可执行文件与受管根身份。
- P06：JSON 由 `serde_json::to_vec` 的原分配直接跨 FFI 转移，不再复制到第二个 `Vec`；0/1/Unicode/1 MiB 边界均验证。
- P19：bundle 读取改为 1 MiB 固定块流式写入并增量 SHA-256；失败只清理唯一临时文件，旧 bundle 不受损。
- P20：PowerShell/Bash 运行时依赖从按扩展多次递归改为一次根枚举、单一 Seen/Queue 闭包；新复制依赖继续入队直到稳定。
- 文件监听：底层 `notify` 实例由两个降为一个，业务防抖工作线程由两个降为一个；重复物理路径按引用计数只注册一次。

## 五、执行记录

| 阶段 | 状态 | 事实与变更 | 验证 |
| --- | --- | --- | --- |
| S0 | 已完成 | 已读取完整方案、冻结工作区和前置差异，并逐项确认唯一调用链与所有者 | HEAD/文件数/diff 规模已记录 |
| S1 | 已完成 | 配置序列化与注册项解析去重；缓存 miss、metadata 查询和池锁内析构收敛 | 全部专项 Rust 测试通过 |
| S2 | 已完成 | Managed IO 使用可合并脏区间增量刷盘，刷盘失败保留待重试状态 | 全模块专项测试通过，确定性字节/区间计数通过 |
| S3 | 已完成 | `LoadedSkill`、engine registry 与 runlua 共享 `Arc<str>` 源码代；debug 保持实时读取 | 新增代复用、debug 实时、失败 reload 保代测试通过；`cargo check` 通过 |
| S4 | 已完成 | owner 上下文持有 managed-runtime 计划缓存；key 与失效身份覆盖 runtime/spec/manifest/lockfile/根归属 | Python、Node、跨 owner、lockfile/package/runtime 文件失效测试通过 |
| S5 | 已完成 | FFI 字节与数组统一精确长度 boxed slice 分配/回收，JSON 直接转移序列化分配 | Rust FFI 51 项专项测试、边界测试、Debug DLL 的 Python/TS/C/Go demo 通过；Miri 在当前 stable Windows 工具链不可用 |
| S6/P19 | 已完成 | 二进制 bundle 固定块下载、增量哈希、候选目录验证和失败回滚 | Python 3 项确定性测试与 `py_compile` 通过；真实 v0.1.8 bundle 获取通过 |
| S6/P20 | 已完成 | PowerShell/Bash 统一为单次初始扫描与单一依赖闭包队列，覆盖 `.so.*` | 两种 shell 语法检查通过；Windows 真实 runtime package smoke 成功 |
| S6/P18 | 已完成 | 新增 `project_root.ps1`，四个 PowerShell 打包脚本显式导入统一根解析 | 5 个 PowerShell 文件解析通过；仓库内与任意 cwd 根解析一致 |
| S6/监听 | 已完成 | 迁移 Vulcan Code 的路径去重、引用计数、最近祖先回退和 RAII 注销思路；改为同步共享后端并保留本仓库业务防抖 | 通用层 2 项、监听相关 7 项测试通过；无 Tokio runtime 前提；Drop 小于 1 秒 |
| S6/补充 BUG | 已完成 | package PowerShell 原先忽略 Python metadata 与 tar 原生命令非零退出码，可能发布不完整归档；现已显式失败并停止发布 | 缺少 active state 时退出码由错误的 0 修复为 1；补齐 bundle 后真实打包退出码 0，归档内容可列出 |
| S6/归档 helper | 已完成 | 预审进一步确认四个 PowerShell 打包器复制同一 tar helper，且全部可能忽略 tar 非零退出码；已收敛到 `archive_helpers.ps1` 并统一显式失败 | 全仓 PowerShell 18 文件解析通过；模拟 tar 退出码 7 会终止；四入口静态契约 2 项通过；真实 runtime package 仍成功 |
| S7/X01 | 已关闭 | 当前 `popen` stdout 确实整包读入 `Vec<u8>`，但仓库没有真实输出分布，也没有限额归属、超限错误/截断、drain/终止与 stderr 诊断的唯一产品契约 | 未猜测默认值，未修改公共 Rust/Lua/FFI 契约；保留为后续独立扩展 |
| S8/P21-X02 | 已关闭 | 当前 workflow 在五平台执行完整 managed-runtime 会话；GitHub 最近 22 次记录的总时长约 3–42 分钟，当前基线 #32 为 32 分 49 秒且 3 个非 Windows 平台失败，尚无“每个测试归属完整且稳定”的削减证据 | 保持五平台覆盖和 workflow 零 diff；先恢复绿色历史并积累分类数据后再进入 CI 拆分 |
| S9/P25-X03 | 已关闭 | Windows 当前依赖树的多版本主要是 `windows-sys`、`getrandom`、`hashbrown` 等目标/构建/传递依赖；未证明能在同一发布产物安全统一；`cargo bloat` 未安装且方案禁止未授权安装 | 记录 `cargo tree -d --all-features` 与现有 release 体积；不制造依赖版本 churn，不新增基线工具 |
| S10/P16 | 已关闭 | Official Hub 仍为既有少量自包含入口，未发现错误码、消息或 capability 漂移，也没有 wrapper 数量显著增长或维护成本数据 | 保持生产代码零 diff；由现有 manager 测试继续校验协议 |
| S10/P17 | 已关闭 | ABI 调整后 Python/TypeScript/C/Go 示例均已真实运行通过，未发现同语言 helper 漂移或复制维护失败 | 不再抽象示例框架，保持示例可独立阅读与复制 |
| S11/X04 | 已关闭 | 维护者未否定进程全局 Tool Cache 契约，本轮没有每引擎隔离的产品要求 | 按方案默认不执行，零生产 diff |
| 全量回归/测试夹具 | 已完成 | 首次并行全量测试发现超时夹具依赖 PATH 中 `ping`/`sleep`，单测通过但并行时可能提前退出；改为启动当前测试二进制内的受控睡眠子测试 | 新夹具专项连续 10 次通过；重新全量为 707 通过、3 忽略 |
| 全量回归/Rust | 已完成 | 格式、严格 Clippy、全目标全特性测试、release、crate package 与许可证检查均已执行 | `fmt` 通过；`clippy -D warnings` 零告警；707/3；release 成功；`cargo deny` 四类通过；package 83 文件、压缩 996.7 KiB |
| 全量回归/脚本 | 已完成 | 全仓脚本语法、下载回滚单测、统一根路径、launcher 与 Windows runtime package smoke 均已执行 | PowerShell 17、Bash 12、Python 18、YAML 2；下载 3 项；launcher 1 项；runtime archive 可列出 |
| 全量回归/FFI | 已完成 | 使用新 release DLL 重新执行 Python、TypeScript、C、Go 标准 ABI 示例 | Python demo、TypeScript typecheck/demo、C 显式 DLL import library 编译/demo、3 个 Go package 编译与 live demo 全通过 |
| 五轮前预审/空白 | 已完成 | 首次候选轮末尾的 `git diff --check` 发现 C demo 与 bundle 下载脚本各有一个新增 EOF 空行 | 已删除；`git diff --check` 退出码恢复为 0；候选轮不计数，连续计数保持 0/5 |
| 五轮审核/重复验证 | 已修复并重置计数 | 原第 3 轮发现 `fetch_deps.ps1` 在 `Resolve-ProjectRoot` 已保证“返回有效根目录或抛错”后仍执行 `$ProjectRoot` 空值兜底；该分支不可达，属于明显冗余验证且会误导维护者认为解析器可能静默返回空值 | 删除不可达分支；重新执行脚本语法、路径解析与打包入口验证；此前连续 2 轮清零，后续从修复后的稳定状态重新计数 |
| 五轮审核/数组空判断 | 已修复并再次重置计数 | 重启第 3 轮发现 `archive_helpers.ps1` 的 `$Members` 与 `fetch_deps.ps1` 的 `$Matches` 均由 `@(...)`/`@()` 保证为数组，却同时判断对象真值与 `Count -eq 0`；前一条件不提供额外状态区分 | 两处统一保留精确的 `Count -eq 0`；该项属于无行为变化的逻辑性修复，删除误导性验证；连续 2 轮再次清零 |
| 五轮审核/PowerShell 数组增长 | 已优化（计数保持 0） | `Resolve-ReleaseTagForSeries` 对 GitHub API 返回的最多 100 条 release 使用 `$Matches += ...`，每次命中都可能重新物化数组 | 改为 `@(...)` 包裹 `foreach` 的一次性结果收集；筛选、异常跳过、排序和返回契约不变，预期减少重复分配与复制 |
| 五轮审核/runlua 输出复制 | 已优化并再次重置计数 | 每个 `runlua` 请求创建独立 `Arc<Mutex<Vec<String>>>` 捕获打印行；执行结束后原实现对完整向量 `.clone()`，但该请求局部缓冲区之后不再承担任何消费语义 | 改为 `std::mem::take` 原子地移出向量并把捕获缓冲区留空；渲染内容和顺序不变，预期消除与打印行数及内容长度线性相关的整量复制 |
| 五轮审核/allowlist 临时分配 | 已优化（计数保持 0） | 私有来源 allowlist 对每个候选前缀执行 `format!("{}/", prefix)`，仅为校验路径边界创建临时字符串 | 改用 `strip_prefix` 后检查剩余部分为空或以 `/` 起始；精确匹配与兄弟前缀拒绝语义不变，消除每项一次额外字符串分配 |
| 五轮审核/Windows 后代夹具 | 已修复并重置计数 | 最终重计第 4 轮全量测试中 `closing_process_session_after_child_exit_still_cleans_descendants` 在 60 秒内未收到外部 Python 夹具 PID；定向 10 次虽全通过，但单次耗时在 2–48 秒间波动，证明夹具启动/发布不确定且会拖慢全量套件 | Windows 改为通过当前测试二进制绝对路径启动受控根/睡眠后代，以根 PID 派生的带外文件发布后代 PID，stdout 回退仅接受纯数字行；三个同夹具测试移除不再需要的 PATH 全局锁。修复后核心用例 20/20 稳定在 0.45–0.50 秒；该项是测试可靠性 BUG 修复，不改变生产进程树实现 |
| 五轮审核/Windows 继承管道夹具 | 已修复并再次重置计数 | 夹具修复后第 4 轮继续沿同类调用链复核，发现 `make_inherited_pipe_descendant_command` 仍通过 PATH 中的 `python` 拉起后代；它被单次收尾和观察器两项测试共用，与上一处故障具有相同的外部启动抖动、环境缺失及误导性“已依赖 Python”注释风险 | Windows 改为当前测试二进制的绝对路径受控根/休眠后代，PID 通过根 PID 对应的带外文件发布，后代仍真实继承 stdout/stderr；Unix 同步固定为 `/bin/sh` 与 `/bin/sleep`。单次收尾 20/20、观察器 10/10 稳定在 0.45–0.52 秒，全量 711/3；该项是测试可靠性 BUG 修复，不改变生产进程树实现 |
| 第 4 轮疑问/LNK4098 | 已验证关闭，非当前实际 BUG | Windows release 链接器提示 `LIBCMT` 与其他库冲突；追溯到 `luajit-src 210.7.1+18b087c` 在 MSVC `static` 模式下未传 `/MD`，其 `lua51.lib` 对象携带 `/DEFAULTLIB:LIBCMT`，而 Rust 默认使用动态 CRT | 当前 `luaskills.dll` 实际依赖仅包含 `VCRUNTIME140.dll` 与 `api-ms-win-crt-*` 动态 CRT，不含第二套静态 CRT；711/3、FFI 20、Python/TypeScript/C/Go release live demo 均通过。该告警来自上游构建指令且不会造成当前产物跨 CRT 释放，暂不以 `/NODEFAULTLIB` 或全局 `crt-static` 猜测性压制；后续应优先由 `luajit-src` 提供 MSVC mixed/动态 CRT 构建选项 |

## 六、连续五轮复审记录

| 轮次 | 审核范围 | 新问题 | 处理与验证 | 连续计数 |
| --- | --- | --- | --- | ---: |
| 第 1 轮 | 全仓 Rust 编译/测试、PowerShell/Bash、依赖许可证、差异空白 | 无 | `fmt`、严格 Clippy、708 通过/3 忽略、PS 18、Bash 12、cargo-deny、diff-check 全通过 | 1 |
| 第 2 轮 | 全仓性能/效率模式、FFI 所有权、Managed IO、运行时计划、下载流式链路 | 无 | 旧 FFI 所有权模式 0；原生 watcher 1；专项 20/1忽略、1、18、3 项通过；严格 Clippy 与 708/3 全量通过 | 2 |
| 原第 3 轮（计数重置） | 全仓明显冗余验证与不可达兜底扫描 | `fetch_deps.ps1` 存在 1 处不可达根目录空值兜底 | 已删除并安排完整回归；按结束规则，此前 2 轮有效记录保留为历史证据，但连续计数清零 | 0 |
| 重启第 1 轮 | 全仓 Rust 编译/测试、PowerShell/Bash/Python/YAML、依赖许可证、差异空白 | 无 | `fmt`、严格 Clippy、708 通过/3 忽略、PS 18、Bash 12、Python 18、YAML 2、cargo-deny 四类、diff-check 全通过 | 1 |
| 重启第 2 轮 | 全仓性能/效率模式、重复转换、整包读取、FFI 所有权、Managed IO、运行时计划缓存、共享 watcher | 无 | 旧 FFI 所有权模式 0，原生 watcher 构造点 1；剩余整包读取均有既有输出/小型清单契约；708/3，Managed IO 26/1忽略，Python/Node 缓存各 1，FFI 20，下载 3，diff-check 全通过 | 2 |
| 重启第 3 轮（再次重置） | PowerShell/Rust 明显冗余验证、不可达分支与先验检查扫描 | 两处数组空判断重复；同一 release 筛选循环使用数组 `+=` 重复增长 | 已统一为精确 `Count -eq 0`，并改为一次性数组收集；重启第 1/2 轮保留为历史证据，但连续计数再次清零 | 0 |
| 最终第 1 轮 | 全仓 Rust 编译/测试、PowerShell/Bash/Python/YAML、依赖许可证、差异空白 | 无 | `fmt`、严格 Clippy、708 通过/3 忽略、PS 18、Bash 12、Python 18、YAML 2、cargo-deny 四类、diff-check 全通过 | 1 |
| 最终第 2 轮（再次重置） | 全仓性能路径、重复所有权转换、整包读取、FFI 所有权与 watcher 实例数 | `runlua` 请求结束时完整克隆请求局部打印输出 | 已改为 `mem::take` 转移所有权；最终第 1 轮保留为历史证据，连续计数再次清零 | 0 |
| 最终重计第 1 轮 | 全仓 Rust 编译/测试、PowerShell/Bash/Python/YAML、依赖许可证、差异空白 | 无 | `fmt`、严格 Clippy、708 通过/3 忽略、PS 18、Bash 12、Python 18、YAML 2、cargo-deny 四类、diff-check 全通过 | 1 |
| 最终重计第 2 轮 | 全仓所有权转移、临时分配、锁内快照、流式 IO、Managed IO、运行时计划缓存、共享 watcher | 无 | 生产旧 FFI/打印克隆模式 0、原生 watcher 1；Managed IO 26/1忽略、Python/Node 计划各 1、FFI 20、runlua 36、launcher 2、下载 3、diff-check 通过；首个全量汇总 707/3 后，独立 launcher 2/2 与原命令重跑 708/3 关闭发现差异 | 2 |
| 最终重计第 3 轮 | 全仓明显冗余验证、不可达/占位逻辑、重复脚本 helper、打包入口与路径独立性 | 无 | 同义空判断与 TODO/FIXME/未实现占位 0；全量合计 708/3；PS 18、release 选择、仓库外 cwd 真实 runtime 归档、tar 失败传播、launcher 2、diff-check 全通过 | 3 |
| 最终重计第 4 轮（计数重置） | unsafe/FFI 对称性、watcher 生命周期、全量并发进程测试 | Windows 后代夹具偶发 60 秒未发布 PID | 已替换为当前测试二进制的绝对路径受控夹具，并删除三处不必要 PATH 串行；此前 3 轮保留为历史证据，连续计数清零 | 0 |
| 夹具修复后第 1 轮 | 全仓 Rust 编译/测试、脚本语法、供应链、差异空白及受控 Windows 后代夹具 | 无 | `fmt`、严格 Clippy、710 通过/3 忽略、PS 18、Bash 12、Python 18、YAML 2、cargo-deny 四类、diff-check 全通过；原失败用例 20/20，另两项共享夹具测试通过 | 1 |
| 夹具修复后第 2 轮 | 全仓性能/重复处理、所有权转移、Managed IO、runlua、allowlist、watcher 与进程树 | 无 | 生产旧 FFI/打印克隆模式 0、原生 watcher 1；全量 710/3，Managed IO 26/1忽略、runlua 36、allowlist 1、watcher 9、进程树 1、下载 3、diff-check 全通过 | 2 |
| 夹具修复后第 3 轮 | 全仓冗余验证、占位/死逻辑、脚本 helper、路径独立与真实打包 | 无 | 冗余/占位模式 0、tar helper 1；全量 710/3、PS 18、release 选择、仓库外 cwd runtime 归档、tar 失败传播、launcher 2、diff-check 全通过 | 3 |
| 夹具修复后第 4 轮（再次重置） | 继续复核进程夹具、unsafe/FFI 对称性、watcher 生命周期 | Windows 继承管道场景仍残留外部 Python 夹具 | 已用当前测试二进制的受控夹具替换并完成高重复验证；此前 3 轮保留为历史证据，连续计数再次清零 | 0 |
| 继承管道夹具修复后第 1 轮 | 全仓 Rust 编译/测试、脚本语法、供应链、差异空白及两类受控 Windows 后代夹具 | 无 | `fmt`、严格 Clippy、711 通过/3 忽略、PowerShell 16、Bash 12、Python 15、YAML 9、cargo-deny 四类、diff-check 全通过；单次收尾 20/20、观察器与关闭清理各 10/10 | 1 |
| 继承管道夹具修复后第 2 轮 | 全仓性能/重复处理、FFI 所有权、Managed IO、runlua、运行时计划、共享 watcher 与进程树 | 无 | 全量 711/3、严格 Clippy；旧 FFI `Vec::from_raw_parts` 0、生产打印向量 clone 0、原生 watcher 构造 1；Managed IO 24、runlua 39、watcher 7、allowlist/两类计划/进程树各 1、下载 3、diff-check 全通过 | 2 |
| 继承管道夹具修复后第 3 轮 | 全仓冗余验证、不可达/占位逻辑、脚本 helper、路径独立、归档失败与 release 筛选 | 无 | 全量 711/3；TODO/FIXME/HACK/XXX、`todo!`、`unimplemented!`、常量真假分支均为 0；共享 tar helper 构造 1，模拟退出码 7 正确失败；仓库外 cwd FFI SDK 打包成功，Lua runtime 缺少 `third_party` 时按前置条件正确拒绝；release 选择、launcher 2、fmt、diff-check 全通过 | 3 |
| 继承管道夹具修复后第 4 轮 | unsafe/FFI 分配释放对称性、共享 watcher 注册/注销、进程回收、release 产物与跨语言 ABI | 无 | 全量 711/3、release 构建、严格 Clippy、FFI 20、file-watcher 2、业务 watcher 7、单 exit-watcher 1、Python/TypeScript/C/Go release live demo、fmt 与 diff-check 全通过；`LNK4098` 经静态库指令和 DLL import table 复核后归类为上游误导性告警而非当前运行时缺陷 | 4 |
| 继承管道夹具修复后第 5 轮 | 全仓文档/许可证/工作流、发布包内容、脚本语法、最终状态与差异卫生 | 无 | 全量 711/3、doc-test、fmt、严格 Clippy、PowerShell 16、Bash 12、YAML 9、cargo-deny 四类、diff-check 全通过；crate 83 文件、4.6 MiB/999.2 KiB 压缩，打包副本编译成功且包含 `THIRD_PARTY_NOTICES.md` | 5 |

> “无问题”指没有发现新的实际 BUG、方案范围内未闭环问题或本轮引入的回归；已经证实但按触发条件明确保留的条件项必须继续列在最终边界中，不能借此伪报为不存在。

## 七、审核文件记录

### 7.1 冻结基线逐文件台账

- 冻结基线的 245 个 Git 跟踪文件已经逐文件完成首轮代码、配置、脚本、文档或不适用筛查；每个文件的序号、分类、审核方式和关联问题见 `docs/reviews/20260823-FULL_REPOSITORY_CODE_AUDIT-FILE-LEDGER.md`。
- 本次次级执行没有用目录抽样替代该台账；五轮复审均重新对账 245 个跟踪文件，并覆盖所有执行期间修改或新增的文件、直接调用方、测试和发布入口。

### 7.2 本任务修改与新增文件复核清单

以下 55 个已修改跟踪文件均完成差异级复核：

```text
Cargo.lock
Cargo.toml
deny.toml
examples/demo-ffi/run.ps1
examples/demo-ffi/run.sh
examples/demo-rust/run.ps1
examples/demo-rust/run.sh
examples/ffi/c/demo.c
examples/ffi/go/demo.go
examples/ffi/go/lifecycle_demo/main.go
examples/ffi/go/query_demo/main.go
include/luaskills_ffi.h
include/luaskills_json_ffi.h
scripts/build/package_debug_tool.ps1
scripts/build/package_demo.ps1
scripts/build/package_ffi_sdk.ps1
scripts/build/package_lua_runtime.ps1
scripts/build/package_lua_runtime.sh
scripts/deps/fetch_deps.ps1
scripts/deps/fetch_deps.sh
scripts/deps/fetch_managed_runtimes.ps1
scripts/deps/fetch_managed_runtimes.sh
scripts/deps/fetch_runtime_packages_bundle.py
scripts/ffi/fetch_ffi.ps1
scripts/ffi/fetch_ffi.sh
src/dependency/manager.rs
src/dependency/manager/tests.rs
src/download/archive.rs
src/download/manager.rs
src/ffi.rs
src/ffi_standard.rs
src/ffi_standard/tests.rs
src/ffi_standard/types.rs
src/host/controller.rs
src/providers/lancedb.rs
src/providers/mod.rs
src/providers/sqlite.rs
src/providers/sqlite/tests.rs
src/runtime/cache.rs
src/runtime/config.rs
src/runtime/config_service.rs
src/runtime/config_tool.rs
src/runtime/engine.rs
src/runtime/engine/lease.rs
src/runtime/engine/runlua.rs
src/runtime/engine/tests.rs
src/runtime/managed_io.rs
src/runtime/managed_io/tests.rs
src/runtime/managed_package.rs
src/runtime/managed_runtime.rs
src/runtime/mod.rs
src/runtime/process_session.rs
src/runtime/process_session/tests.rs
src/skill/manager.rs
src/skill/manager/tests.rs
```

以下 12 个新增文件均完成全文审核；其中 5 个为审核记录文件：

```text
THIRD_PARTY_NOTICES.md
docs/reviews/20260823-FULL_REPOSITORY_CODE_AUDIT-FILE-LEDGER.md
docs/reviews/20260823-FULL_REPOSITORY_CODE_AUDIT.md
docs/reviews/20260823-FULL_REPOSITORY_ISSUE_CLASSIFICATION_AND_REVALIDATION.md
docs/reviews/20260823-PRIORITY_REPAIR_AND_FIVE_ROUND_AUDIT_RECORD.md
docs/reviews/20260824-SECONDARY_REPAIR_AND_FIVE_ROUND_AUDIT_RECORD.md
scripts/build/archive_helpers.ps1
scripts/build/project_root.ps1
scripts/deps/test_fetch_runtime_packages_bundle.py
src/runtime/file_system.rs
src/runtime/file_watcher.rs
tests/demo_launcher_paths.rs
```

## 八、最终分类结果

### 8.1 实际 BUG 与必须修改项

| 项目 | 事实分类 | 修复结果 | 修改后收益 |
| --- | --- | --- | --- |
| PowerShell runtime 打包忽略 Python metadata 非零退出 | 实际 BUG、必须修改 | 已显式检查退出码并中止发布 | 不再把缺失 active state 的不完整目录打成成功包 |
| 四个 PowerShell 打包入口忽略原生 tar 非零退出 | 实际 BUG、必须修改 | 已统一到受检 `archive_helpers.ps1` | 归档失败不再伪装为发布成功，错误行为跨入口一致 |
| Windows 后代清理测试依赖 PATH 中外部 Python | 测试可靠性 BUG、必须修改 | 已替换为当前测试二进制的绝对路径受控夹具 | 全量并发不再偶发 60 秒超时，核心路径约 0.45–0.52 秒稳定完成 |
| Windows 继承管道测试仍依赖外部 Python | 测试可靠性 BUG、必须修改 | 已用第二个受控根夹具替换并保留真实管道继承 | 单次收尾与观察器测试不再受外部启动抖动或环境缺失影响 |

### 8.2 逻辑性修复与性能优化

| 工作包 | 分类 | 已完成调整 | 预计并已验证的结果 |
| --- | --- | --- | --- |
| S1/P09/P10/P11/P14/P15 | 低风险逻辑与效率优化 | 去除重复 JSON 序列化、重复注册项解析、纯 miss 写锁清扫、重复 metadata 与锁内 VM 析构 | 降低重复 CPU/IO、锁竞争与临界区时长；外部行为不变 |
| S2/P05 | IO 写放大优化 | Managed IO 只刷新增尾部或合并后的脏区间，失败保留可重试状态 | 1000 次累计 flush 的物理载荷由 500500 字节降为 1000 字节 |
| S3/P03 | 生命周期/结构优化 | 非 debug 源码由加载代 `Arc<str>` 快照共享 | 同一加载代只读一次源码；reload 失败继续保留旧代 |
| S4/P04 | 缓存与安全边界优化 | owner 局部缓存 managed-runtime 计划，命中时实时重验全部资产身份 | 避免重复建计划，同时不把缓存命中变成安全校验旁路 |
| S5/P06 | FFI 所有权优化 | 跨边界字节和数组统一为精确长度 boxed slice | 消除 JSON 第二次复制，分配与释放布局完全对称，ABI 不变 |
| S6/P18-P20 | 构建链路优化 | 根解析、归档 helper、流式 bundle、单次依赖闭包扫描统一 | 降低整包内存、重复遍历与脚本漂移，发布包可从任意 cwd 构建 |
| 文件监听 | 架构/效率优化 | 两个配置域共享一个同步 `notify` 后端和一个业务工作线程 | OS watcher 由两个降为一个，重复路径按引用计数注册，Drop 可确定 join |
| 审核追加项 | 明显冗余/重复转换清理 | 删除不可达根目录空值兜底、重复数组空判断、数组 `+=` 增长、runlua 全量 clone 与 allowlist 临时字符串 | 去除误导性防御和无效分配，不改变成功/失败契约 |

### 8.3 扩展项

- 本方案没有把扩展伪装成 BUG 修复，也没有新增未经需求确认的公共功能。
- X01（`popen` 输出限额/流式协议）、X02（CI 分层）、X03（依赖基线治理）、X04（每引擎 Tool Cache）均已按触发条件核验后关闭；缺少唯一产品语义、稳定时长数据、可测收益或维护者授权时保持零生产差异。
- 后续若实施这些项目，必须先确定调用方、错误语义、兼容边界和跨平台验收矩阵，再作为独立扩展方案执行。

### 8.4 必改、可能产生连带影响与误导性问题

- 必须修改：发布脚本吞错与两类外部 Python 夹具已全部修复；否则分别会产生损坏发布包或不稳定 CI。
- 会产生连带影响但已受测试约束：Managed IO 改变物理写入颗粒度；源码加载代改变非 debug 的读取时机；计划缓存增加失效身份；FFI 改变内部 allocator 所有权；共享 watcher 改变注册/注销与线程拓扑。上述项目均保持公开契约，并已用失败重试、reload、身份失效、边界长度、引用计数和 Drop 测试覆盖。
- 误导性问题：`LNK4098` 由上游 `luajit-src` 的 MSVC 静态对象指令触发，但当前 DLL import table 证明最终只使用动态 CRT，四语言 ABI 实测也通过；不能把警告直接写成跨 CRT 崩溃 BUG，也不能用全局 `crt-static` 猜测性压制。
- 条件性而非 BUG：cargo-deny 的重复 crate 警告来自目标/构建/传递依赖；CC0-1.0 已由维护者明确允许且许可证门禁通过；两者都不属于当前必须修改项。

## 九、最终验证与约束

| 验证类别 | 最终结果 | 边界 |
| --- | --- | --- |
| Rust 全量 | 连续最终五轮均为 9 套件、711 通过、3 忽略 | 当前 Windows x64、当前 stable 工具链 |
| Rust 静态/格式 | `cargo fmt --all -- --check`、全目标全特性 Clippy `-D warnings`、doc-test 全通过 | Miri 在当前 stable Windows 工具链不可用 |
| 发布 | release 成功；crate 83 文件、4.6 MiB、999.2 KiB 压缩；打包副本编译成功 | cargo package 对未提交的新增测试给出“未包含”提示，提交后由 VCS 文件集自动消失，不影响当前 crate 内容验证 |
| FFI | FFI 20 项；Python、TypeScript、C、Go 使用 release DLL 实际运行通过 | Go 必须显式链接 `luaskills.dll.lib`，不能让 MinGW 按 `-lluaskills` 误选 Rust 静态库 |
| 脚本/配置 | PowerShell 16、Bash 12、Python 15、YAML 9；下载 3、launcher 2 通过 | Bash 为 Windows 上 Git Bash 语法验证，不替代 Linux/macOS 实机 |
| 供应链 | cargo-deny advisories、bans、licenses、sources 全通过；notice 已进入 crate | 依赖重复只保留为有证据再治理的条件项 |
| 差异卫生 | `git diff --check` 通过 | 未创建 Git 提交，不改写用户既有提交历史 |

- 已实际运行平台：Windows x64。
- 未实际运行平台：Linux、macOS、Windows ARM64；现有五平台 CI 配置保持不变，不能把静态/语法证据表述为真实平台验收。
- 真实外部 Provider 凭证链路不在本地复验范围；本轮覆盖仓库夹具、标准 FFI、发布与现有集成测试。

## 十、后续处理建议与顺序

1. 第一优先：提交前在 Linux、macOS、Windows ARM64 CI 复跑完整矩阵，优先恢复 managed-runtime workflow 的稳定绿色历史；任何平台失败先按真实 BUG 处理。
2. 第二优先：向 `luajit-src` 上游推动 MSVC mixed/动态 CRT 构建选项，再在隔离分支验证 `LNK4098` 是否消失；不建议在本仓库直接加 `/NODEFAULTLIB` 或全局 `crt-static`。
3. 第三优先：积累 CI 测试归属与时长数据后再决定 X02；没有稳定数据前不削减平台或测试覆盖。
4. 第四优先：只有出现真实大输出调用方和明确超限语义后才设计 X01；先定义 drain、截断/报错、stderr 与后代清理契约。
5. 第五优先：P25/X03 仅在上游兼容范围证明可统一且能量化产物收益时实施；禁止只为清除重复依赖警告强锁版本。
6. 最后处理：X04 每引擎 Tool Cache 属于产品/架构选择，必须得到维护者明确授权后另立方案。

## 十一、后续回归复核更正与迁移边界（2026-08-24）

- 本记录第六、九节的 5/5 只说明当时快照，已被后续全量未提交审核发现的新回归推翻，不能继续作为当前完成证明。
- Managed IO 的元数据代检测已补充 Windows 卷 ID/文件 ID/ChangeTime 与 Unix dev/inode/ctime；宿主未暴露新代时不声称能检测同长度并发覆盖，不通过每次全文件哈希重新制造读放大。
- 下载缓存键全面改为 SHA-256 文件名，旧下载缓存首次访问不会自动复用；不安全依赖路径片段也改为 SHA-256。两者均不盲目探测旧路径，以免重新引入路径逃逸或身份碰撞。离线部署必须提前通过可信打包流程预填充新布局。
- `PreparedSkill*` 内部路径与记录继续保持 `pub(crate)`。这是破坏性事务真实性的安全边界，不能以兼容名义恢复；下一发布版本必须在版本与升级说明中披露源码不兼容。
- watcher 降级、下载重入、Unix C demo、缓存删除与全局缓存配置兼容性等完整修复记录见 `docs/reviews/20260824-UNCOMMITTED_REGRESSION_REPAIR_AND_REAUDIT.md`。
- 旧连续计数保持历史失效；后续接入/协议复审又发现并修复 I01-I04，再次清零后已从最终状态连续完成新的 5/5，见 `docs/reviews/20260824-INTEGRATION_PROTOCOL_RISK_REAUDIT.md`。
