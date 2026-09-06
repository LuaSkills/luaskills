# 全代码仓库性能与冗余审核记录

## 文档状态

- 审核状态：已完成
- 审核日期：2026-08-23
- 审核仓库：`D:\projects\vulcan-luaskills`
- 审核原则：事实先行、逐文件留痕、疑问闭环、仅审核不修改生产代码

## 审核目标

本记录用于持续保存全仓审核范围、已审核文件、正式发现、待验证疑问、验证证据和最终处理建议。正式问题只有在调用链、类型、契约、测试或运行证据足够时才进入“确认问题”列表。

## 仓库基线

- 基线分支：`main`，跟踪 `origin/main`
- 基线提交：`c9bd51910a64fd6c4868b5c0df6d9f5b662fc63f`
- 审核开始前工作区：干净
- 建立记录后的工作区变化：新增本审核记录；`docs/plan/` 受 `.gitignore` 排除，因此计划文件不出现在 Git 状态中
- Git 跟踪文件总数：245
- AST 结构扫描：已覆盖仓库默认源码类型；生成物、缓存、`target` 和忽略文件未纳入
- 规模提示：核心 `src/` 共 65 个文件，其中 `src/runtime/engine.rs` 约 12409 行、`src/runtime/engine/tests.rs` 约 13116 行，是后续调用链与热路径审核重点

## 审核覆盖统计

| 路径分类 | 基线文件数 | 已审核 | 结构扫描 | 仅一致性筛查或不适用 | 待代码级审核 |
| --- | ---: | ---: | ---: | ---: | ---: |
| 核心源码 `src/` | 65 | 65 | 65 | 0 | 0 |
| 公共 FFI 头文件 `include/` | 2 | 2 | 2 | 0 | 0 |
| 内置技能 `skills/` | 5 | 5 | 5 | 0 | 0 |
| 示例与测试 `examples/`、`tests/` | 87 | 87 | 87 | 35 | 0 |
| 构建、依赖与工作流脚本 `scripts/`、`.github/` | 27 | 27 | 27 | 0 | 0 |
| 根配置与契约 | 5 | 5 | 5 | 1 | 0 |
| 文档、许可证与历史记录 | 54 | 54 | 54 | 54 | 0 |
| **合计** | **245** | **245** | **245** | **90** | **0** |

## 文件审核台账

状态定义：

- “结构扫描”只表示文件已进入 AST 或 Git 基线，不代表已完成代码级审核。
- “已审核”表示已读取对本次方向有影响的完整逻辑或确认文件不含可执行逻辑。
- “一致性筛查”适用于翻译文档、锁文件、许可证、占位文件等不需要逐函数性能审核的内容。

| 文件 | 分类 | 状态 | 本轮关注点 | 结果或关联编号 |
| --- | --- | --- | --- | --- |
| `.gitignore` | 根配置 | 已审核 | 审核边界、生成物与计划记录规则 | `target`、缓存、计划与完成记录被忽略；无运行性能问题 |
| `Cargo.toml` | 根配置 | 已审核 | crate 边界、依赖与构建特征 | 单 crate，同时产出三种库类型；作为后续全目标静态检查基线 |
| `deny.toml` | 根配置 | 已审核 | 依赖审查配置 | 仅许可证策略，无运行逻辑 |
| `src/lib.rs` | 核心源码 | 已审核 | 模块暴露与公共重导出 | 仅模块和公共 API 汇总，未发现本次方向问题 |
| `src/runtime/{cache,config,config_service,config_tool,context,encoding,engine,help,logging,managed_io,managed_package,managed_runtime,managed_runtime_services,managed_runtime_session,managed_session_events,path,process_session,result}.rs` | 核心源码 | 已审核 | 运行时热路径、缓存、配置、进程、托管环境、I/O 与 VM 生命周期 | `P02`、`P03`、`P04`、`P05`、`P09`、`P10`、`P11`、`P12`、`P13`、`P15`；另有低优先级观察项 |
| `src/runtime/engine/{bridge,host_result,lease,runlua}.rs` | 核心源码 | 已审核 | 引擎拆分模块、租约、宿主结果与 RunLua 调用 | 与 `src/runtime/engine.rs` 的调用链合并审核；未新增独立问题 |
| `src/host/{callbacks,controller,database,options}.rs` | 核心源码 | 已审核 | 宿主桥接、数据库绑定、控制器与限额 | `P01`、`P02`；限额为影响评估依据 |
| `src/dependency/manager.rs`、`src/download/{archive,manager}.rs` | 核心源码 | 已审核 | 依赖解析、归档解压、HTTP 下载与校验 | `P07`、`P08`、`P14` |
| `src/skill/{config,dependencies,manager,manifest,resolver}.rs` | 核心源码 | 已审核 | 技能发现、解析、配置、安装与进程级共享状态 | `P07`、`P12`、`P13` |
| `src/providers/{lancedb,mod,sqlite}.rs` | 核心源码 | 已审核 | 数据库 Provider 桥接及控制器调用 | `P01` 的实际共享调用方；未新增独立问题 |
| `src/ffi.rs`、`src/ffi/requests.rs`、`src/ffi_standard.rs`、`src/ffi_standard/types.rs` | 核心源码 | 已审核 | ABI 所有权、JSON 输入输出与标准 FFI 转换 | `P06` |

其余文件已完成结构扫描与对应层级审核。上表中的花括号路径是精确文件名的紧凑展开；245 个基线文件的逐项状态、审核方式与问题关联见 `docs/reviews/20260823-FULL_REPOSITORY_CODE_AUDIT-FILE-LEDGER.md`。

## 确认问题

### P01 控制器数据库请求被全局互斥锁串行化

- 级别：高
- 位置：`src/host/controller.rs:17-20`、`111-120`、`180-226`
- 事实链：桥接结构把 `tokio::runtime::Runtime` 放入 `Mutex`；每次 `run` 从加锁开始一直持有到整个控制器 Future 完成。依赖 `vldb-controller-client 0.2.3` 中的 `ControllerClient` 是 `Arc<ControllerClientInner>` 的可克隆共享代理；当前锁并不保护客户端。仓库锁定的 Tokio 1.52.1 明确支持从多个线程并发调用 `Runtime::block_on`。
- 影响：同一个 SQLite/LanceDB 控制器桥接上的所有请求被强制排队；一次慢网络请求会阻塞其他互不相关的数据库请求，抵消多线程 Runtime 和可克隆客户端的并发能力。
- 建议：移除 Runtime 外层互斥，保存 `Runtime` 或克隆的 `Handle` 并直接并发调度；宿主线程已处于 Tokio 上下文时继续沿现有 Handle 分发路径。变更前补同桥接并发请求测试和关闭生命周期测试。

### P02 `until_text` 每 10ms 全量复制并重新解码双流缓冲区

- 级别：高
- 位置：`src/runtime/process_session.rs:1071-1127`、`1200-1225`；默认限额见 `src/host/options.rs:231-238`
- 事实链：等待标记文本时固定轮询休眠 10ms；每轮同时锁 stdout/stderr，把两个 `VecDeque` 完整收集成新 `Vec`，再完整解码并执行 `contains`。默认单流可达 1 MiB。
- 影响：无新输出时仍反复复制和解码最多约 2 MiB；等待时间越长、并发会话越多，CPU、分配和锁占用越明显。
- 建议：由读取线程通过通知原语唤醒等待者，并维护增量搜索游标；仅重新搜索新字节与标记长度所需的重叠窗口，同时正确保留跨块编码边界。

### P03 创建 Lua VM 时深克隆目录并重复读取全部入口源码

- 级别：高
- 位置：`src/runtime/engine.rs:129-140`、`764-766`、`9034-9064`、`9189-9231`、`9310-9368`
- 事实链：每个 VM 创建都会克隆完整技能表和入口注册表；随后逐入口再次 `read_to_string` 并编译。加载技能阶段只验证入口元数据，没有缓存非调试模式下稳定的源码内容。
- 影响：VM 池扩容或高并发冷启动时，目录规模会同时放大堆复制、文件系统读取和编译前准备成本。
- 建议：把一代已加载目录封装为不可变 `Arc` 快照，Engine 与 VM 共享；非调试模式在加载代内缓存入口源码，调试模式继续按调用重读以保留热更新语义。Lua 状态间的编译仍需分别执行。

### P04 每次托管 Python/Node 调用重复解析并哈希相同运行时资产

- 级别：中高
- 位置：`src/runtime/engine.rs:5652-5655`、`5744-5747`；`src/runtime/managed_runtime.rs:2051-2179`、`2933-3007`
- 事实链：每次调用先解析环境计划，解析过程校验根目录并解析/哈希 runtime、包管理器与锁文件；随后 `ensure_managed_env` 又立即执行一轮资产解析与哈希。进入创建路径后的再次校验发生在锁内，用于关闭检查与使用之间的竞态。
- 影响：即使环境和技能代没有变化，热调用仍承担重复磁盘访问和哈希成本，运行时二进制较大时更明显。
- 建议：按包 `owner_token`/加载代缓存解析后的计划；保留一次临近使用的实时资产校验，并保留创建锁内的二次校验，不能以性能优化为由删掉安全边界。

### P05 Managed IO 对写入缓冲反复全量落盘

- 级别：高
- 位置：`src/runtime/managed_io.rs:124-164`、`320-343`、`1250-1273`
- 事实链：read/update/append 打开时把文件完整载入内存；Write 与 Update 模式的每次 flush 都用 `fs::write` 重写当前完整缓冲区。多次“追加少量数据后 flush”会反复写入历史全部内容。
- 影响：大文件存在双份内存和高峰分配；频繁 flush 的累计写放大可接近平方级。
- 建议：内部改为受管的 `File`/`BufReader`/`BufWriter` 或记录脏区间/已刷长度；Update 模式需明确随机修改和截断语义，不能直接套用 Append 增量策略。

### P06 JSON FFI 在边界上重复整包复制

- 级别：中
- 位置：`src/ffi.rs:208-230`、`343-370`；`src/ffi_standard.rs:292-339` 及运行时租约 JSON 入口
- 事实链：序列化先生成 `String`，再由 `owned_buffer_from_bytes` 执行一次 `to_vec`；标准 FFI 对调用方借用的 UTF-8 输入先复制成 `String` 再解析 JSON，返回 String 又复制到 owned buffer。
- 影响：每个 JSON ABI 调用在输入或输出上产生至少一次可避免的整包复制；大参数、结果集和高频租约调用放大内存带宽与峰值。
- 建议：让 owned buffer 直接接管 `String`/`Vec<u8>` 所有权，或使用 boxed slice 明确容量释放契约；对借用输入直接从 `&[u8]`/`&str` 反序列化。必须同步验证 C 头文件的长度、容量和释放 ABI。

### P07 HTTP 操作每次新建线程和客户端

- 级别：中
- 位置：`src/download/manager.rs:504-533`；调用方见 `src/skill/manager.rs:1908-1937`
- 事实链：每项 HTTP 任务都 `thread::spawn`，在线程内创建新客户端，然后调用方立即 `join`；技能安装流程还会为多个阶段重复创建 DownloadManager。
- 影响：失去连接、DNS 与 TLS 会话复用，并为 API、资产、校验和请求逐次支付线程创建/销毁成本。
- 建议：使用长生命周期阻塞工作线程/执行器和可复用客户端，或把下载层整体异步化；保留当前与宿主 Tokio 上下文隔离的设计目标。

### P08 下载先全量驻留内存，再落盘并重新读取校验

- 级别：高
- 位置：`src/download/manager.rs:197-270`
- 事实链：响应体先完整累积到 `Vec`，再一次性 `fs::write`；带 SHA-256 的路径随后重新读取文件计算哈希。进度回调还在每个 64 KiB 块克隆一次来源字符串。
- 影响：大资产同时占用响应大小级内存并额外产生一轮磁盘读取；校验失败重试时重复成本更高。
- 建议：流式写入同目录临时文件并同步更新 SHA-256，校验成功后原子替换目标；进度上下文共享不可变来源字符串，避免逐块克隆。

### P09 runtime-config JSON 响应被序列化两次

- 级别：中
- 位置：`src/runtime/config_tool.rs:266-314`、`529-543`
- 事实链：typed dispatch 先用 counting writer 序列化一次以验证响应大小；JSON dispatch 随后又 `serde_json::to_string` 完整序列化。
- 影响：所有成功 JSON 请求都重复遍历响应对象，`describe` 等大结果会重复分配和编码工作。
- 建议：JSON 路径一次序列化到 `Vec`/`String`，直接以实际字节长度执行限额验证并返回同一缓冲；typed API 保留现有独立限额验证。

### P10 配置热路径重复克隆同一包注册信息

- 级别：中低
- 位置：`src/runtime/config_service.rs:1133-1145`、`1488-1514`、`1609-1620`
- 事实链：`get_effective_value` 先通过 `declaration` 获取包，再通过 `store_for_skill` 重新获取同一个包；`package` 每次克隆完整注册项。
- 影响：Lua `config.get` 高频调用时产生重复查表和结构克隆，成本随声明/配置元数据增长。
- 建议：一次解析包注册项，在同一借用范围内取得 declaration 并复用 package 级 store 路径。

### P11 Tool Cache 未命中也触发写锁和全表过期扫描

- 级别：中
- 位置：`src/runtime/cache.rs:143-164`、`222-224`
- 事实链：读锁下只要未命中、scope 不同或已过期，就升级为写锁并对所有条目执行 `retain`；容量驱逐也通过线性扫描寻找最老项。默认上限为 1000。
- 影响：大量不存在键查询或多 scope 访问会把读路径变为串行写路径，并反复 O(n) 扫描共享全局缓存。
- 建议：精确删除已知过期键；用摊销清理、分桶或过期索引处理全局回收；容量驱逐使用顺序索引，避免每次线性求最小值。

### P12 技能配置文件锁注册表永久保留强引用

- 级别：中
- 位置：`src/skill/config.rs:1373-1376`、`1497-1503`
- 事实链：进程级 `BTreeMap<PathBuf, Arc<Mutex<()>>>` 对每个出现过的路径插入强引用且从不移除。长生命周期 FFI 宿主可以创建使用不同配置根的多个引擎。
- 影响：历史路径和锁对象随进程生命周期只增不减。
- 建议：改为 `Weak<Mutex<()>>` 并在注册表锁内升级/清理；仓库中的 managed-runtime 环境锁注册表已采用 Weak 加 `retain`，可复用其生命周期模式。

### P13 全局 Tool Cache 重复构造后丢弃，且后续配置静默失效

- 级别：中低
- 位置：`src/runtime/cache.rs:268-274`；`LuaEngine::new` 调用处 `src/runtime/engine.rs:7223-7228`
- 事实链：每次建引擎都会先分配新的 `Arc<SharedToolCache>`，随后 `OnceLock::set`；首次之后的失败结果被忽略，新对象立即丢弃，后续引擎配置也不生效。
- 影响：每次引擎初始化发生无用分配；更重要的是“首个配置获胜”没有显式契约，造成冗余验证/配置表象。
- 建议：使用显式进程级初始化或 `get_or_init`；发现与既有配置冲突时返回明确错误或记录诊断，不能继续忽略 `set` 结果。

### P14 缓存下载目标连续执行两次 metadata/type 校验

- 级别：低
- 位置：`src/download/manager.rs:31-44`、`165-173`
- 事实链：辅助函数已执行 `fs::metadata` 和 `is_file`，命中后调用方立即再次执行同样检查，只为取得长度；第二次检查没有持有文件句柄，不能消除 TOCTOU。
- 影响：每次缓存命中多一次系统调用和重复分支，收益为零。
- 建议：辅助函数直接返回 `Option<Metadata>` 或长度，调用方复用同一次查询结果。

### P15 Lua VM 在池状态互斥锁内析构

- 级别：中
- 位置：`src/runtime/engine.rs:6811`、`6849`、`6890-6910`
- 事实链：空闲回收在持有 VM 池状态锁时 `swap_remove`，被移除 VM 在锁作用域内析构。析构成本可随 Lua 堆、userdata 和注册对象增长。
- 影响：获取/释放 VM 的其他线程会被析构阻塞；一次大 VM 回收可能抬高整个池的尾延迟。
- 建议：锁内仅摘除并收集待回收 VM，释放状态锁后再统一 drop；沿用仓库其他池先脱离共享状态、后执行重清理的模式。

### P16 四个内置 Hub 入口重复相同的桥接检查与错误包络

- 级别：低
- 位置：`skills/skill-search/runtime/{search,detail,resolve,sources}.lua`
- 事实链：四个文件除工具名、错误码和消息外结构完全相同，逐个执行 `vulcan`/`vulcan.host` 类型检查、`has` 查询和 `call`。引擎的 `register_vulcan_module` 在技能入口执行前固定安装 `vulcan.host` 表，因此前两层类型检查在受支持执行路径内重复。
- 影响：运行成本很小，但新增 Hub 能力时需复制同一分支与错误协议，容易发生行为漂移。
- 建议：保留必要的 capability `has` 判断，把统一错误包络与调用逻辑收敛到一个受管 helper 或 Rust 注册函数；不要删除“工具是否已注册”的能力检查。

### P17 同语言 FFI 示例重复维护整套 ABI 辅助函数

- 级别：低
- 位置：`examples/ffi/python/*.py`、`examples/ffi/typescript/{demo,lifecycle_demo,query_demo,json_runtime}.ts`
- 事实链：同一语言的多个示例重复定义动态库解析、runtime fixture 准备、borrowed/owned buffer 转换、结果校验等辅助逻辑；TypeScript 的 `resolveLibraryPath`、`ensureStandardFixtureLayout`、`readOwnedBuffer`、`makeBorrowedBuffer`、`mustOK` 在多个入口重复出现，Python 侧也存在相同模式。
- 影响：不影响库运行时，但 ABI 或路径契约变化需要多文件同步，示例容易形成互相矛盾的接入方式。
- 建议：每种语言建立一个最小共享 helper 模块，示例文件只保留各自业务流程；共享模块仍应保持可复制、可阅读，避免抽象成难以理解的演示框架。

### P18 构建脚本重复实现公共路径、目录和归档辅助逻辑

- 级别：低
- 位置：`scripts/build/package_{debug_tool,demo,ffi_sdk,lua_runtime}.{ps1,sh}`，以及多个 fetch 脚本
- 事实链：`Resolve-ProjectRoot`/`normalize_output_dir`、`Ensure-Dir`/`ensure_dir`、tar 创建、平台识别和 release asset 下载校验在多个同语言脚本中重复实现；示例 run 脚本还复制了同一套项目根查找函数。
- 影响：维护与安全修复必须跨文件重复，已有附带发现 `A01` 证明路径约定已经发生漂移。
- 建议：分别建立 PowerShell 与 POSIX shell 的仓库内公共 helper，并让打包脚本显式导入；生成到发布包的独立脚本仍可在打包阶段展开为自包含文件。

### P19 runtime-packages 构建下载整包驻留内存

- 级别：中
- 位置：`scripts/deps/fetch_runtime_packages_bundle.py:98-111`、`279-341`
- 事实链：`download_bytes` 对 release bundle 执行 `response.read()`，随后在内存中哈希，再把完整 bytes 写入归档文件并解压。
- 影响：构建机峰值内存至少增加一个完整 bundle 大小；与运行时下载器 `P08` 是同类全缓冲模式。
- 建议：流式写临时归档并在写入过程中更新哈希，校验后再解压；失败时清理临时文件，避免覆盖已缓存的有效 bundle。

### P20 原生依赖打包对重叠目录执行多轮递归扫描

- 级别：中
- 位置：`scripts/build/package_lua_runtime.ps1:195-231`、`350-395`、`660-663`；`scripts/build/package_lua_runtime.sh:116-200`、`322-323`
- 事实链：PowerShell 按每个扩展名分别递归枚举；linked dependency 队列同时扫描 `ScanRoot` 与 `LibsDir`，而第一次调用的 `ScanRoot` 已包含 `libs`；函数又分别对 runtime root 和 `target/release` 调用，每次重建 Seen 集并重新扫描/执行依赖解析。Shell 版本同样把重叠根加入 find，并在两次调用间丢弃 seen 状态。
- 影响：依赖目录和 release 目录较大时，目录遍历、`ldd`/`otool` 调用和磁盘访问成倍增加。
- 建议：一次收集所有不重叠根，单次枚举后按扩展过滤；整个打包阶段共享队列与 Seen 集，新增复制到 `libs` 的依赖继续入同一队列。

### P21 CI 矩阵在五个平台重复串行执行完整 Rust 测试套件

- 级别：中
- 位置：`.github/workflows/managed-runtime-sessions.yml:25-41`、`175-185`
- 事实链：五平台 native matrix 每个分支都执行 `cargo test --all-targets -- --test-threads=1`，因此平台无关测试也被完整重复五次且全部串行；同一矩阵已经在此前步骤完成平台专属 runtime 布局与 smoke 验证。
- 影响：PR CI 的编译/执行时长和 runner 消耗显著放大，慢测试会在五个平台重复支付。
- 建议：建立一次全量通用测试 job；native matrix 只运行受管会话、平台路径、动态库和进程树等确实依赖目标系统的专用测试集合。拆分前先建立显式 test target/filter，确保不降低跨平台验收覆盖。

### P22 每个受观察持久进程会话创建一个 10ms 轮询线程

- 级别：高
- 位置：`src/runtime/process_session.rs:2130-2192`；上限见 `src/host/options.rs:233`、`250-252`
- 事实链：有 observer 的每个会话都会创建独立 OS 线程，每 10ms 获取会话状态并探测一次子进程；默认单引擎最多 256 个持久会话，System 事件会话会安装 observer。
- 影响：满载时可出现 256 个仅用于退出探测的线程和每秒约 25600 次状态轮询，带来线程栈、调度、锁竞争和空闲 CPU 成本。
- 建议：把退出监听提升为引擎级 reaper/等待服务，使用平台进程等待能力或集中阻塞等待后发布事件；过渡方案至少应共享有限线程并采用自适应退避，不能为每会话保留固定高频轮询线程。

### P23 `vulcan.io.popen` 无上限捕获并丢弃 stderr

- 级别：高
- 位置：`src/runtime/managed_io.rs:1000-1049`、`1069-1099`
- 事实链：stdout 和 stderr 各启动一个线程并 `read_to_end` 到无上限 `Vec`；完成后 stdout 返回给 Lua，而 stderr 只绑定到 `_stderr` 后立即丢弃。当前 popen 选项只有编码和超时，没有输出字节上限。
- 影响：stderr 较大的命令会产生纯浪费的分配与复制，恶意或异常子进程可用双流输出推高宿主内存；即使最终超时，已捕获数据仍可能很大。
- 建议：如果 API 契约确定不暴露 stderr，直接重定向到 null；若错误诊断需要 stderr，则只保留有明确上限的尾部摘要。stdout 也应增加宿主管控的最大捕获字节数并返回截断诊断。

### P24 锁定的 `h2 0.4.13` 存在空 DATA 帧无界排队漏洞

- 级别：中（RustSec 标记为低严重度，但与无界内存直接相关）
- 位置：`Cargo.lock`；依赖链经 `hyper`/`reqwest` 与 `tonic`/`vldb-controller-client`
- 事实链：`cargo deny check` 报告 `RUSTSEC-2026-0258`，当前 `h2 0.4.13` 会接受并无界排队空 DATA 帧；修复版本为 `0.4.16` 及以上。
- 影响：未及时 drain 的流可能出现无界内存增长，长度溢出时还可能 panic；HTTP 下载与控制器 gRPC 两条链路均进入该依赖图。
- 建议：优先更新锁文件使 `h2 >= 0.4.16`，然后运行全目标测试和真实下载/控制器连通验收；若上游约束阻止升级，应明确记录阻塞依赖而不是忽略 advisory。

### P25 依赖图保留多组可见重复版本

- 级别：低
- 位置：`Cargo.lock`
- 事实链：`cargo deny check` 与 `cargo tree -d --depth 1` 显示 `getrandom`、`hashbrown`、`windows-sys` 等多版本；其中一部分来自 build dependency 或不同目标，不会全部进入单个平台运行产物。
- 影响：确定存在额外下载/编译工作；最终二进制体积影响需按 release target 实测，不能仅凭锁文件断言全部重复进入运行时。
- 建议：先执行目标平台 `cargo bloat`/链接 map 量化，再优先推动能被上游 semver 统一的直接运行依赖；不要用强制 patch 合并不兼容主版本。

## 待验证疑问

### Q01 控制器 Runtime 互斥是否是依赖强制要求

- 初始疑问：`ControllerClient` 或 Tokio Runtime 是否要求同一时刻只能执行一次请求。
- 验证：依赖源码确认客户端为 `Arc` 共享的 Clone 代理；Tokio 1.52.1 本地源码明确 `Runtime::block_on(&self, ...)` 可从多线程并发调用。
- 最终状态：已关闭，转为确认问题 `P01`。

### Q02 托管环境内多轮资产校验是否都可删除

- 初始疑问：`resolve_*_env_plan`、`ensure_managed_env` 和创建锁内的重复校验是否全部冗余。
- 验证：调用链显示创建锁内的再次校验发生在互斥区内，能够关闭校验后到实际创建前的替换竞态；直接全部删除会破坏完整性边界。
- 最终状态：已关闭。仅计划解析/前置哈希可缓存，锁内实时校验必须保留，详见 `P04` 与排除项 `E01`。

## 已排除问题

### E01 托管运行时创建锁内的二次资产校验

- 位置：`src/runtime/managed_runtime.rs` 的 Python/Node 环境创建路径。
- 排除原因：该校验位于共享环境锁内，验证的是即将执行的真实资产，可防止前置解析后文件被替换。它与 `P04` 中可缓存的计划构建不属于同一个安全时点，不应作为“无必要验证”删除。

### E02 RuntimeSessionManager 的线性租约清理

- 位置：`src/runtime/managed_runtime_session.rs`
- 排除原因：同一运行时会话的租约数量有 8 个硬上限，线性扫描被严格限制在常数级小集合内；引入额外索引不会形成可证明收益。

### E03 config 文档读取路径并非每次访问磁盘

- 位置：`src/runtime/config_service.rs`
- 排除原因：`with_document_read` 优先使用缓存快照，只有缓存缺失时才读取磁盘；不能仅因函数名或调用层次将其认定为重复 I/O。

### E04 `vulcan.io.popen` 必须排空子进程 stderr，但无需完整保留

- 位置：`src/runtime/managed_io.rs:1000-1099`
- 排除边界：不读取 piped stderr 可能让子进程因管道写满而阻塞，所以“排空”本身有必要；`P23` 指向的是无上限保存后丢弃，而不是删除排空行为。可用 `Stdio::null` 或有界 drain 保留正确性。

## 附带一致性发现

### A01 源码仓库的四个 demo run 脚本引用不存在的依赖脚本路径

- 级别：高（示例可执行性），不计入本次性能问题数量
- 位置：`examples/demo-{ffi,rust}/run.{ps1,sh}`
- 事实链：四个脚本引用 `scripts/fetch_runtime_deps.ps1` 或 `.sh`；Git 跟踪文件中不存在这些路径。当前真实入口是 `scripts/deps/fetch_deps.ps1` 与 `.sh`，README 和打包脚本也使用后者。
- 影响：调用者只要把 `Fetch`/`TARGET` 设为非 `none`，脚本就会在运行 demo 前直接失败。
- 建议：统一改到 `scripts/deps/fetch_deps.*`，并增加只解析路径、不访问网络的 demo launcher smoke 检查。该项属于本轮旁路发现，本审核不修改生产或示例代码。

### A02 `cargo deny check` 的许可证策略当前失败

- 级别：中（发布治理），不计入本次性能问题数量
- 位置：`deny.toml:1-23`
- 事实链：`notify 8.2.0` 使用 `CC0-1.0`，当前 allow 列表未包含；`vldb-controller-client 0.2.3` 已在发布 Cargo.toml 声明 MIT，但仓库仍配置一个指向不存在 `../LICENSE` 的 clarification 文件，因此 cargo-deny 额外发出缺文件警告。
- 影响：依赖许可证门禁不能通过，CI 若启用该命令会阻断；陈旧 clarification 会掩盖真正的许可证元数据状态。
- 建议：由项目维护者确认是否接受 CC0-1.0；接受则加入策略，拒绝则替换依赖。删除或更新已经失效的 vldb clarification，并以 `cargo deny check licenses` 复验。

## 验证记录

| 时间 | 验证项 | 命令或方法 | 结果 | 影响范围 |
| --- | --- | --- | --- | --- |
| 2026-08-23 | 审核记录初始化 | 创建计划与动态审核台账 | 通过 | 全仓 |
| 2026-08-23 | Git 基线 | `git status --short --branch`、`git rev-parse HEAD`、`git ls-files` | 基线提交已记录；245 个跟踪文件 | 全仓 |
| 2026-08-23 | AST 结构扫描 | CodeKit AST Tree | 默认源码类型扫描完成；识别主要模块与大文件 | 全仓源码 |
| 2026-08-23 | 全目标 Clippy | `rtk cargo clippy --all-targets --all-features --message-format short` | 通过，无诊断 | Rust 全目标、全特征 |
| 2026-08-23 | 性能/冗余专项 Clippy | `rtk cargo clippy --all-targets --all-features --message-format short -- -W clippy::perf -W clippy::redundant_clone -W clippy::needless_collect -W clippy::unnecessary_to_owned` | 通过，无诊断；人工审核仍确认跨函数/生命周期问题 | Rust 全目标、全特征 |
| 2026-08-23 | 控制器并发事实核验 | 本地依赖源码：`vldb-controller-client 0.2.3`、`tokio 1.52.1` | 客户端为 Arc 共享代理；Runtime 支持多线程并发 block_on | `P01`、`Q01` |
| 2026-08-23 | Rust 格式 | `rtk cargo fmt --all -- --check` | 通过 | 全部 Rust 源码 |
| 2026-08-23 | Rust 全目标测试 | `rtk cargo test --all-targets --all-features` | 8 个套件，644 项通过，0 失败；3105 项因目标/cfg/filter 未执行 | Rust 全目标、全特征 |
| 2026-08-23 | Skill Config 契约再生成 | `rtk cargo run --quiet --bin generate_skill_config_contract -- target/audit-skill-config-contract.json` 后 `git diff --no-index` | 生成结果与提交契约完全一致；MSVC 链接阶段另有 `LNK4098` 警告，未影响输出 | `contracts/skill-config/v1/contract.json` |
| 2026-08-23 | PowerShell 语法 | PowerShell Parser 扫描 `scripts/`、`examples/` 下全部 `.ps1` | 通过 | PowerShell 脚本 |
| 2026-08-23 | Bash 语法 | Git Bash `bash -n` 扫描 12 个跟踪 `.sh` | 通过 | POSIX shell 脚本 |
| 2026-08-23 | Python 语法 | Python `ast.parse` 扫描 17 个 `.py` | 通过 | Python 脚本、示例与夹具 |
| 2026-08-23 | Node/TypeScript | `node --check` 6 个 `.mjs`；`rtk npm run typecheck` | 通过 | Node 与 TypeScript 示例/夹具 |
| 2026-08-23 | Go/C 示例编译 | 三个 Go 示例目录 `go test`；`gcc -fsyntax-only -Iinclude` | 通过 | Go 与 C FFI 示例、C 头文件 |
| 2026-08-23 | JSON/YAML 语法 | Python JSON 与 PyYAML 解析 | 6 个 JSON、12 个 YAML 全部通过 | 配置、契约、工作流与技能清单 |
| 2026-08-23 | 依赖门禁 | `rtk cargo deny check` | 未通过：`P24` advisory 与 `A02` 许可证策略；重复依赖警告归入 `P25` | Cargo 依赖图 |
| 2026-08-23 | 最终工作区 | `rtk git status --short --branch` | 生产代码未修改；仅新增 `docs/reviews/` 审核产物，计划目录受忽略规则管理 | 全仓 |

## 最终审核结果

### 汇总结论

- 基线覆盖：245/245 个 Git 跟踪文件已完成对应层级审核，待审核 0；精确逐文件记录见配套台账。
- 主线确认问题：25 项，其中高优先级 7 项、中优先级 13 项、低优先级 5 项。
- 附带一致性问题：2 项，分别是四个 demo launcher 的失效脚本路径，以及当前无法通过的许可证门禁。
- 已排除误报：4 项；托管环境锁内安全复验、popen stderr 排空、小上限租约扫描和 config 缓存读取均确认有必要，未建议直接删除。
- 待验证疑问：0；`Q01`、`Q02` 均已关闭并转入确认问题或排除边界。
- 自动检查：Rust 格式、Clippy、644 项测试和多语言语法/类型检查通过；依赖门禁因已记录问题未通过。

### 风险集中区

1. 进程与 I/O：`P02`、`P05`、`P22`、`P23` 同时指向轮询、全缓冲、无界捕获和线程数量，属于最直接的 CPU/内存风险群。
2. VM 与托管运行时：`P03`、`P04`、`P15` 会放大冷启动、扩池和尾延迟。
3. 网络与控制器：`P01` 把可并发请求串行化；`P07`、`P08`、`P19` 缺少线程/连接复用与流式处理；`P24` 是已披露的无界排队依赖风险。
4. 全局缓存与配置：`P09` 至 `P14` 主要是重复序列化、重复查表/克隆、全表扫描和进程级状态生命周期问题。
5. 构建与 CI：`P17` 至 `P21`、`P25` 不直接拖慢在线请求，但持续增加打包、CI、接入示例和依赖维护成本。

### 验证边界

- 本轮没有修改任何生产、示例或 CI 代码，只新增审核报告与台账。
- 本轮没有执行专门性能基准，因此问题级别来自确定的复杂度、分配、锁范围、线程模型和限额事实，不代表已经测得具体吞吐提升百分比。
- 未执行真实 GitHub 下载、真实 space-controller/SQLite/LanceDB 连通、五平台 GitHub Actions matrix 或完整发布打包；相关结论基于源码、锁文件、依赖源码和本机静态/测试证据。
- 契约生成器在本机 MSVC 链接时出现 `LNK4098` CRT 默认库冲突警告，但生成文件正确且测试通过；该警告需要单独的 native link 配置审查，不能在没有链接 map 的情况下归因。

## 处理建议

### 第一优先级：先处理确定的失效与无界资源风险

1. 修复 `A01` 的 demo 路径，并处理 `P24` 的 `h2` 升级；随后修复 `A02` 许可证门禁，使依赖审计恢复为可用红线。
2. 为 `vulcan.io.popen` 增加 stdout 上限并停止完整保留后丢弃 stderr（`P23`）。
3. 把持久会话退出探测收敛为引擎级服务（`P22`），把 `until_text` 改为通知驱动的增量搜索（`P02`）。这两项涉及进程/事件架构，实施前需要确认方案与跨平台等待模型。
4. 把 Managed IO 和下载路径改为流式/增量落盘（`P05`、`P08`、`P19`），先定义临时文件、原子替换、截断与错误恢复契约。

### 第二优先级：解除并发与冷启动放大器

1. 移除控制器 Runtime 的全局串行锁并补并发测试（`P01`）。
2. 设计按加载代共享的不可变技能目录和源码快照（`P03`）。该项属于核心结构调整，实施前应先确认 debug 热更新与 reload 失效语义。
3. 按 `owner_token`/加载代缓存 managed runtime 计划，同时保留临近使用和锁内安全复验（`P04`）。
4. 把 VM 重析构移出池锁（`P15`），并减少 JSON FFI 整包复制（`P06`）。

### 第三优先级：收敛缓存、配置与构建重复

1. 依次处理 `P09` 至 `P14`：一次序列化、一次包解析、精确过期删除、Weak 锁注册表、显式全局 cache 初始化和 metadata 复用。
2. 把原生依赖扫描合并为一次全局队列（`P20`），再拆分通用测试 job 与平台专项 matrix（`P21`）。
3. 最后收敛内置 Hub wrapper、示例 helper 和脚本 helper（`P16` 至 `P18`），并按 release 目标量化依赖重复后再处理 `P25`。

### 建议建立的性能基线

- 控制器：同桥接 1/8/32 并发请求的吞吐与 P95/P99。
- 进程会话：1/32/256 个空闲 observer 会话的线程数与 CPU；64 KiB/1 MiB 双流缓冲下 `until_text` 等待成本。
- Lua VM：10/100/500 个入口下的首次加载、池扩容时间、磁盘读取次数和峰值内存。
- Managed IO：固定总字节下 1/10/1000 次 flush 的写入量与耗时。
- 下载：100 MiB 资产下载/校验的峰值 RSS、落盘字节与重试成本。
- FFI/config：1 KiB/1 MiB JSON 请求响应和 1/100/1000 项 config/cache 的分配次数与吞吐。

## 后续分类与二次复验

- 逐项 BUG/优化/扩展分类、必须性、连带影响、误导边界与修复顺序见 `docs/reviews/20260823-FULL_REPOSITORY_ISSUE_CLASSIFICATION_AND_REVALIDATION.md`。
- 二次复验修正了 `P12` 的文件位置，并收窄了 `P02`、`P05`、`P14`、`P16-P17`、`P21`、`P25` 的结论边界；后续执行应以二次复验记录为准。
