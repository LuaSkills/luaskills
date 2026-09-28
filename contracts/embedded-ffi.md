# 嵌入式运行时传输协议

本文由 `src/ffi_embedded.rs`、`src/ffi_embedded/` 与两个公开头文件共同校验。协议、结构或命令变化时同步更新本文和原生调用测试。当前已接通版本一传输、运行时生命周期，以及插件、池、操作、会话与能力队列命令，持久模式、历史对账及显式存储／写入线程恢复入口见下文。三个 SDK 的开发实现已接入这些入口并有本地原生消费证据；既有安装包测试不代替后续变更重新打包验收，语言生命周期以各 SDK 源码和测试为准。高级升级事务、宿主迁移及跨平台发布验收仍待完成，原生入口或本地 SDK 验证不等于正式发布。

## 所有权与关闭

宿主通过 `luaskills_ffi_embedded_transport_new_v1` 显式声明所有预算，获得一个不透明 `uint64_t` 身份。新注册表独立于旧引擎注册表，注册表锁仅用于元数据定位，不包围执行。身份在本次动态库加载期间不复用，跨卸载重载不保留有效性。SDK 必须完整保留 64 位身份，不能用 JavaScript 的普通数字承载它。

关闭禁止新增运行时注册，并向全部已有槽请求关闭，保留诊断及排空所需命令；实际释放要求关闭已请求、全部请求预留已退出、全部结果已显式释放，以及全部运行时已实际排空并移除。返回成功的原生函数已经交还控制权后，宿主才可完成线程汇合与动态库卸载。单独观察一次释放成功不能证明其他外部线程已返回调用栈。

结果采用独立的 `FfiEmbeddedResultV1`：只读字节地址、精确长度与分配身份。仅通过相同传输的 `luaskills_ffi_embedded_result_free_v1` 释放；复制结构不增加所有者，读取者必须在释放前结束。重复释放、错传输、错误身份、被修改地址或长度均返回明确错误。释放函数只比较地址，不解引用外来结果指针。该契约不改变旧 `FfiOwnedBuffer` 必须恰好释放一次的调用方责任，新结果不能交给旧释放函数。

## 入场与响应预算

配置没有隐式无限值或自动归一化。所有额度为正且不超过本平台 `isize::MAX`，逐响应额度不大于聚合字节额度。配置大小前缀在完整结构读取前校验；调用方仍对地址可读性及可写输出负责。

每个请求在命令分发前同时预留一个结果槽和最坏响应字节。结果发布后，字节额度转换为实际拥有长度；解析失败、响应超限或栈展开归还未发布预留。关闭和释放遵循同一所有权状态，不能在实际调用仍产生结果时提前卸载。输入在形成切片前检查长度与空地址；输出在任何可能失败操作前清零。

序列化写入器在生成字节时限制长度，不先生成无界字节向量再截断。变更命令在变更前完成成功回执编码及分配：固定回执直接保留，动态身份按核心唯一身份生成规则的最大序号预留并保留同一分配。实际结果编码不能超过已证明上限。禁止先预检查后丢弃分配，再在变更完成后重新申请响应存储。能力队列批次在分发标记与交付之间保持原子所有权；不得先摘除宿主请求再因编码失败丢弃请求身份。

核心代理已提供 `HostRequestBroker::take_json(limit, max_bytes)`：在同一分发权威下先编码、预留实际输出容量，再标记已分发；超大队首保持排队，且不会跳过它投递后续较小请求。如果后续请求超限，已成功分发的前缀仍作为完整 JSON 数组返回。预算包含数组括号、逗号、UTF-8 及 JSON 转义。普通 Rust 批次与 JSON 批次共用同一单次投递状态转换，不能重复取得处理权。撤权、取消与截止在编码前后检查。

`host_requests_take` 从整个响应额度中预先扣除协议包裹字节，并在调用代理前保留完整外层字节分配；代理返回的数组字节直接追加到已保留的响应存储，不重新序列化。外层结果槽与字节仍须通过传输统一入场。无法交付的队首保持未分发，因此取消和关闭不需要等待一个从未收到身份的 SDK 处理器。

## 请求与失败

唯一描述请求为 `{"protocol_version":1,"command":{"type":"describe"}}`。请求结构与命令均严格反序列化，拒绝未知字段、重复字段、未知命令、错误 UTF-8、额外 JSON 尾随内容。未知版本不探测其他协议。

成功信封包含 `protocol_version`、`status: "ok"` 与 `result`；描述结果包含核心包版本、协议及结构版本、实际已接通的命令集合和有效传输配置。业务失败信封包含 `status: "error"` 与核心 `EmbeddedError` 的 `code`／`message`。原生返回码为零只表示信封已交付，SDK 必须继续判断信封状态，不能将传输成功误判为业务成功。

## 运行时生命周期命令

以下命令嵌在同一个版本一请求的 `command` 字段中；全部 ID 均作为精确字符串传递，不解析、归一化或寻找其他传输中的同名身份。

| 命令 | 参数 | 成功结果与语义 |
| --- | --- | --- |
| `runtime_reserve` | 无 | 返回 `{runtime_id}`；仅预留身份和有界元数据，不创建引擎或线程 |
| `runtime_initialize` | `runtime_id`、`engine_options`、`runtime_config` | 返回固定 `{runtime_id}` 回执；表示一次构造尝试已结束，实际成功或失败读取状态 |
| `runtime_status` | `runtime_id` | 返回初始化阶段、关闭状态、实际核心命名空间、实时用量、实际工作线程关闭证据及保留错误 |
| `runtime_close` | `runtime_id` | 返回固定身份回执；永久关闭入场，包括仍在进行的构造 |
| `runtime_free` | `runtime_id` | 实际排空后移除注册并返回固定身份回执；未关闭、仍在构造或被原生调用持有时明确报忙 |

两步创建保证宿主在构造能够启动线程前已经取得稳定控制身份。SDK 的高级创建方法应封装预留、初始化、状态校验及失败后的关闭释放，不向业务调用方暴露不完整运行时。`runtime_initialize` 在当前原生调用中同步构造，SDK 不能在需要处理回调的事件循环上阻塞调用；后续并发接口仍由核心操作记录承载。

初始化只允许从 `reserved` 进入一次 `initializing`，随后成为 `ready`、`failed` 或 `faulted`。配置或引擎构造显式失败保留在 `failed`，不得以相同身份自动重试；关闭后可以释放该记录，再显式创建新身份。初始化期间关闭不会丢失构造所有者，新核心构造完成后会接收到关闭请求。实际使用者持有独立原生租借，查询及关闭不持有传输注册表锁等待核心；核心引用必须先于使用者计数释放。

不可预期的原生构造 panic 标记为 `faulted`。因为无法证明部分构造的原生工作已安全收敛，该状态明确拒绝声称关闭完成或允许卸载动态库；宿主应报告基础设施故障并通过进程级恢复处理。该规则不把普通 Lua 插件执行错误升级为进程故障。

预留身份及其他生命周期变更的固定回执在变更前编码并校验响应容量；无法交付身份时不会创建注册。运行时数量包括未初始化、正在创建、正在关闭及失败但尚未释放的记录。业务状态来自真实核心，不由 SDK 自建池或关闭状态机。`engine_options` 继续使用既有 `LuaEngineOptions` 及宿主选项的字段／默认语义；新命令外壳与 `runtime_config` 严格拒绝未知字段。不可将对新字段的严格校验扩大解释为已修改旧引擎选项的兼容契约。

原生返回码为：0 成功、1 参数或请求无效、2 精确身份不存在、3 尚有所有权或尚未关闭、4 容量超限、5 已释放引用、6 内部错误、7 协议不支持。失败时不产生结果所有权，有效输出为空。每个 C 边界捕获可展开 Rust panic；调用方无效地址、进程终止及内存分配中止不属于可捕获错误。

## 运行时业务命令

`pool_register.definition.package_root` 是可信 SDK 宿主显式授权的精确绝对包目录，可位于旧 System 根之外；定义必须由宿主核验，不能直接转发不可信插件参数。核心保留包内依赖清单、授权工作区及逻辑目录和原生对象身份校验，旧 System 租约信任根限制保持不变。字段形状未变，生成契约描述与摘要同步；实际安全边界见[精确模块包授权](embedded-runtime.md#当前类型化模块)。

业务命令放在 `{"protocol_version":1,"command":{"type":"runtime","runtime_id":"精确已初始化身份","operation":{...}}}` 中，`operation.type` 是下表命令名。外壳与类型化命令拒绝未知字段。嵌入的核心配置和声明继续以各自 Rust 类型及其序列化约束为权威；既有 `LuaInvocationContext` 的行为不由 FFI 重定义。

| 命令 | `operation` 中的其余字段 | 成功结果 |
| --- | --- | --- |
| `plugin_register` | `plugin_id`、`config: EmbeddedPluginConfig` | `null` |
| `plugin_status` | `plugin_id` | 核心插件状态与聚合用量 |
| `plugin_close`、`plugin_forget` | `plugin_id` | `null` |
| `capacity_register` | `plugin_id`、`config: EmbeddedCapacityConfig` | `{capacity_id}` |
| `capacity_status` | `capacity_id` | 正式容量状态，包含物理保证、成员数、排队及清理期活动量 |
| `capacity_close`、`capacity_forget` | `capacity_id` | `null` |
| `pool_register` | `definition: ModuleDefinition`、`policy: PluginPoolConfig`、`permissions: string[]`、`execution_revision`、可选 `capacity_id` | `{pool_id}` |
| `pool_status` | `pool_id` | `PoolUsage`，来自实际资源计数 |
| `pool_close`、`pool_forget` | `pool_id` | `null` |
| `pool_revoke_permission` | `pool_id`、`permission` | 是否实际移除了授权的布尔值 |
| `call_submit` | `call: EmbeddedCall`、`timeout_ms` | `{operation_id}` |
| `session_open` | `pool_id`、`timeout_ms` | `{session_id, operation_id}`；操作表示实际初始化 |
| `session_submit` | `session_id`、`export`、`arguments`、`context`、`timeout_ms` | `{operation_id}` |
| `session_status` | `session_id` | 核心会话快照 |
| `session_close`、`session_forget` | `session_id` | `null` |
| `operation_status` | `operation_id` | `OperationSnapshot` |
| `operation_list` | 可选 `pool_id`、可选保留游标 `after_operation_id`、正 `limit` | `OperationPage`，包含身份数组、末游标和 `has_more`；控制通道 |
| `operation_wait` | `operation_id`、`wait_ms` | 终态或本次等待到期时的 `OperationSnapshot` |
| `operation_cancel` | `operation_id` | 是否首次请求取消的布尔值 |
| `operation_forget` | `operation_id` | `null` |
| `capabilities_register` | `descriptors: CapabilityDescriptor[]` | `{registration_ids: string[]}`，顺序对应输入批次 |
| `capabilities_list` | `permissions: string[]` | 当前注册表中满足显式授权的能力声明数组 |
| `capability_status` | `registration_id` | 实际注册状态、活动所有权及排空证据 |
| `capability_unregister`、`capability_forget` | `registration_id` | `null` |
| `host_requests_take` | `limit` | 有界 `HostRequest[]` |
| `host_request_status` | `request_id` | `HostRequestStatus`，含实时取消原因 |
| `host_request_complete` | `request_id`、`outcome` | `null` |

`call_submit` 与 `session_submit` 的回执只证明已入场，不证明执行成功。`timeout_ms` 是原始执行预算；`wait_ms` 仅是观察者本次等待预算，不延长或取消执行。取消、关闭、注销只是请求生命周期推进，实际释放依据核心终态及排空证据。`forget` 只允许移除已满足对应释放条件的记录。

池持有不可变模块、代际、权限绑定和执行修订；同一 VM 可以复用本次绑定，不能在运行中按名称切换到其他代际。`pool_revoke_permission` 修改的就是该池现有绑定的权限权威，已经创建的 VM 也受其约束。`capabilities_list` 是显式宿主授权下的注册表发现；池内 Lua 的可见成员仍来自注册池时冻结的能力快照，不能将两者混用。

FFI 仅接受显式 `queued` 能力。包含 `native` 的整个注册批次在发布前返回 `unsupported`，不会静默改成队列调用。Rust 原生宿主继续使用核心原生闭包注册 API。队列请求携带精确注册身份及核心生成的调用身份；SDK 按 `registration_id` 选择已保留处理器，不按可变名称重新路由。业务 `arguments` 里的同名字段不替代可信 `caller`。

`host_request_complete.outcome` 只能是以下一种形状，未知字段、重复字段与形状混用均无效：

- 成功：`{"ok":true,"value":null,"effects":"committed"}`。`value` 必须存在，可以是任意允许的 JSON 值，包括 `null`。
- 失败：`{"ok":false,"error":{"code":"internal","message":"Host failure"},"effects":"unknown"}`。失败也可能携带 `committed`，表示错误发生前已经提交。

`effects` 以 `EffectState` 为权威，允许 `not_started`、`not_applicable`、`committed`、`rolled_back`、`unknown`，并继续接受核心对声明和结果的语义校验。判别布尔值与形状冲突时不消费处理器所有权。已分发请求即使被取消、能力被注销或运行时开始关闭，仍须在真实宿主处理结束后完成确认；终态操作保留逐宿主副作用证据，不能把取消解释成回滚。

运行时槽关闭后，新插件、容量、池、会话、操作及能力注册均拒绝入场；查询、撤权、取消、关闭、遗忘和宿主确认继续可用。此关闭门独立于核心初始化完成时机，不能利用「关闭已返回、构造刚完成」的间隙创建新工作。

容量分组通过构造前描述中的 `capacity_groups_v1` 声明。容量登记按核心最长不透明身份预留完整成功回执，
回执不足时不登记容量，也不扣留专用预留；关闭及遗忘同样先完成响应预留。配置和状态直接使用正式
Rust 类型，不复制第二套预算字段。当前新增命令沿用版本一 JSON 及 C 结构，精确契约摘要发生变化；
SDK 必须同步包内契约后才能消费候选库，不能仅依据包版本相同跳过描述校验。

`pool_register.capacity_id` 省略或显式空值表示原独立归属；提供字符串则必须是当前运行时中、
归属同一插件且未关闭的精确容量。未知、外来或关闭身份直接失败，不回退独立池。成员局部最小值为零，
避免重复保证。关闭容量只停止入场并请求原成员排空；空容量预留在全部成员显式遗忘、实际物理注销及
`capacity_forget` 成功后归还。状态、关闭和遗忘应走 SDK 控制通道，登记走工作通道。
此处的 C ABI 接通不表示三个 SDK 的类型句柄、安装包及各平台分发已完成验收。

`operation_list` 发现内存保留操作，按实际入场发布顺序返回身份，不按提前预留的身份字符串排序。`limit` 不得超过核心 `max_operations`；响应仍受 FFI 响应字节预算约束，超出时可缩小查询数量。返回的 `after_operation_id` 为本页最后身份，空页保留输入游标；`has_more` 仅说明本次观测尚有更多记录，后续发布仍可沿保留游标查询。游标已遗忘或来自另一运行时会返回未找到，与池过滤条件不符则拒绝；重新以空游标枚举，不能据缺失推断未执行。精确池过滤使用原操作上下文，池元数据遗忘后仍有效。分页不提供跨多次调用的全局快照，也不包含尚未转换的关闭预留。持久历史继续使用独立 `history_*` 接口。

可复用实例及固定会话的独立关闭操作在模块上下文中包含 `finalization_instance_id`。可复用实例的关闭可由分页发现后用 `operation_status` 查询，原业务结果保持不可变；会话仍可直接从 `session_status.finalization_operation` 取得身份。字段可选以读取早期历史，不得把缺失字段解释为从未关闭。三个 SDK 的类型化运行时提供 `list_operations`／`listOperations`／`ListOperations`，并将该查询送入控制通道。

## 持久模式与历史访问

`runtime_initialize` 新增可选 `persistence`：省略或空值表示既有内存模式；提供对象时，必须完整声明 `path`、`journal` 和 `worker`。路径必须为宿主管理目录中的绝对路径；`journal` 使用 Rust `OperationJournalConfig` 的记录数、单记录字节及主数据库文件上限；`worker` 使用 `OperationJournalWorkerConfig` 的待写回执数及字节上限。所有配置字段及边界来自 Rust 类型和生成 Schema。无效写入队列预算在创建数据库前拒绝；持久初始化失败不得静默改用内存模式。

槽在后续运行时构造之前保留日志及写入者；初始化失败后仍保留清理责任。`runtime_status.persistence` 返回实际写入者状态，未创建存储时为显式空值。关闭先等待核心和实际回调排空，再请求写入者关闭；写入线程退出且全部回执释放后才可以报告关闭。活动原生历史／恢复调用仍持有租借，未退出前不能 `runtime_free`。`closed` 是排空证据，不代表所有业务执行成功，也不表示所有历史已对账。

| 命令 | 输入 | 返回与通道 |
| --- | --- | --- |
| `operation_persistence_failure` | `operation_id` | 原检查点故障或空值，控制通道 |
| `operation_retry_checkpoint` | `operation_id` | 新请求为真、已有重试为假；没有故障返回忙碌错误，控制通道 |
| `storage_status` | 无 | 实际写入者状态，控制通道；不等磁盘，也不把空写入者故障当作日志健康证明 |
| `storage_recover` | 无 | 显式恢复原故障日志为真、健康时无需恢复为假，工作通道 |
| `storage_worker_recover` | 无 | 显式重建已实际退出的受监督失败线程为真，健康运行时为假，工作通道 |
| `history_get` | 原 `history_runtime_id`、`operation_id` | 历史行或空值，工作通道 |
| `history_next` | 可选原键游标 `after: {runtime_id, operation_id}` | 按二进制键序读取下一历史行，空游标从首行开始，末尾为空，工作通道 |
| `history_reconcile` | 原命名空间、操作、前驱修订及完整 `resolution` | 最终证明的持久后继修订，工作通道 |
| `history_forget` | 原 `history_runtime_id`、`operation_id`、`expected_revision` | 精确修订删除确认，工作通道 |

原核心命名空间来自历史行或 `runtime_status.core_runtime_id`，与外层 FFI 槽身份不同。历史绝不自动变成当前活动操作；空历史也不证明从未执行。多次枚举不提供并发写入下的整体快照。删除只允许原终态且副作用已解决，或已附加完整最终宿主对账证明的历史。同一核心命名空间仍保留的操作须先 `operation_forget`，才允许对账或删除；后者只删除内存记录，保持控制通道语义。

普通 Lua 执行的整体副作用仍可能为 `unknown`，即使调用成功且没有宿主能力记录；不能据此推断没有其他副作用。可信宿主核验外部事实后，可通过 `history_reconcile` 附加最终证明；核心不查询外部系统、不认证传入的 `resolver` 字符串，也不允许插件自行授予对账权限。宿主必须证明全部原执行及外部所有者已停止，授权实际对账者，核验非回调副作用与每条原宿主副作用，最后提供非空证据引用。原命名空间当前无活动操作仅是必要条件，不是外部所有者停止的完整证明。

`resolution` 包含稳定 `resolution_id`、`resolver`、整体 `evidence`、`execution`、已解决的聚合 `effects`，以及与原记录身份和顺序精确一致的逐项 `host_effects`。每项携带原 `effect_id`、已解决结果和非空证据引用。原终态只接受 `observed_terminal`；原非终态只接受 `stopped_without_result`，不会制造成功值。已知原结果不能被相反结论覆盖，聚合必须与逐项结论一致。全部字节共用原单记录预算，不能截断证据。数据库格式为第四版；`JournalOperation.reconciliation` 必须存在，尚未对账时显式为空。

对账以同一事务读取原修订、校验证据并写入唯一后继；原 `snapshot`、上下文、调用方和副作用身份不变。相同前驱及完整相同证明的精确重试返回同一后继，适用于存储恢复后的确认丢失；仅复用 `resolution_id` 不足以确认不同证据。不同修订或证明冲突，最终证明不允许通过普通检查点擦除或再次改写。删除使用返回的后继修订。恢复及对账不重放 Lua 或回调，历史中断记录不会成为新活动操作。证明仅保存引用，凭证及完整业务载荷由宿主审计系统管理。

存储恢复重新打开并校验同一原文件，不创建替代库，也不触发业务重放或自动检查点重试。`storage_worker_recover` 独立重建已实际退出、元数据完整且原活动／排队写入已由监督器处理的失败线程；实际线程未退出时返回忙碌，已显式关闭写入者时返回关闭错误。旧回执、配额和原存储对象保留，不会重新提交旧队列。健康运行线程无需恢复，返回假。响应交付容量在重建前预留；失败不会隐式转换为成功。

宿主按实际故障先修复基础设施及恢复日志／写入线程，再对原操作显式请求检查点重试并观察原状态。重建写入线程不证明数据库已恢复，也不复活进程退出后的执行栈。运行时关闭期间，原核心尚未排空且写入者尚未收到永久关闭时仍可恢复以完成原操作；不能撤销写入者自身的永久关闭。元数据锁中毒、回执观察无法证明或监督器之外线程异常时明确保留故障和所有权，不能凭猜测释放库或重试。内存模式的存储及历史命令明确返回不支持。

## SDK 调度与释放

事件泵需要持续取得请求、执行处理器、观察取消并发送真实完成确认。SDK 不得把等待操作终态的阻塞原生调用放在唯一负责回调的事件循环上，也不得让长时间 `operation_wait` 占满全部结果槽和传输字节，导致事件泵无法入场。异步驱动应使用有界并发和短查询，并为控制与确认留出可用入场容量；具体配置随 SDK 接入验证。

业务状态始终以核心查询为准。SDK 保存本语言处理器闭包及异步任务所有权，不能另建池调度器或自行宣称操作终态。注销期间保持旧 `registration_id` 的处理器可达，直到核心报告排空。超时返回给业务调用方后仍需在后台保留真实原生调用和处理器所有权；全部原生调用及结果释放前不能卸载动态库。

若有效配置使某类诊断或结果超出响应预算，查询明确报容量不足，不截断 JSON 或丢弃核心记录；接入方应根据实际最大快照和业务值选择传输预算。容量测试故意使用过小预算，只用于证明入场拒绝不改变业务状态，不构成生产默认配置建议。

## 离线生成契约

当前 Rust 请求及响应类型生成的 Schema 位于 [`embedded/v1/contract.json`](embedded/v1/contract.json)，精确摘要位于 [`embedded/v1/contract.sha256`](embedded/v1/contract.sha256)，生成及同步规则见[离线契约说明](embedded/v1/README.md)。运行时响应映射通过实际分发返回类型约束，不能只更新 SDK 字段表而不更新核心。Schema 负责线帧形状，核心继续负责权限、状态、预算关联及语义校验。

`contract-generation` 为默认关闭的离线构建功能，不改变旧 ABI。五平台工作流会比较当前源码重新生成的精确字节，并将真实 FFI 结果对照生成契约校验。SDK 类型、测试向量、兼容声明及正式核心构建身份仍需在后续 SDK 和发布阶段完成同步，不能将当前 Schema 生成视为完整 SDK 发布验收。

## 验证入口

- Rust 定向测试：`rtk proxy cargo +1.94.0 test --locked --lib ffi_embedded -j 4 -- --test-threads=1`。
- 实际 Lua 全链路覆盖：`src/runtime/engine/tests/embedded/ffi.rs`，仅通过五个新公开 C 入口驱动插件、池、普通调用、会话和能力事件泵。
- 真实动态库 C 消费者：`tests/ffi_embedded_transport.c`，显式传入待验收动态库路径。它包含公开头文件，解析实际导出并执行创建、描述、关闭、错误释放与真实释放，最后卸载库。
- 五平台编译、SDK 和发布产物测试将在对应阶段接入发布门禁；本文件不将本地 Windows 证据扩展为其他平台已通过。
