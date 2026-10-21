# 宿主工具结果桥接、宿主 LuaRuntime（`system_lua_lib`）与执行平面设计稿

## 1. 文档定位

本文是在分析当前 `vulcan-luaskills` 仓库实现后，针对外部《LuaSkills 宿主工具协议与结果桥接改造方案（2026-05-09）》整理出的**仓库内落地版设计稿**。

它的目标不是重复外部方案原文，而是回答下面三个更工程化的问题：

1. 当前仓库已经具备哪些可复用基础。
2. 外部方案中哪些方向可以直接采纳。
3. 真正落到本仓库时，还需要补哪些结构边界、调用边界与兼容策略。

本文优先面向以下读者：

- 正在维护 `luaskills` runtime 的 Rust 开发者。
- 需要将 `VulcanCode`、MCP 暴露层或插件系统与 LuaSkills 对接的宿主开发者。
- 需要评估 `host_result`、`exposure` 与宿主 LuaRuntime（`system_lua_lib`）是否适合进入正式公共协议的设计者。

### 1.1 本文处理的真实问题

这份设计稿不是为“让 skill 返回更多字段”而写，而是为了解决 `VulcanCode` 在推进“**将内置工具全面 LuaSkill 化**”时暴露出来的真实宿主问题。

最核心的问题是：

1. IDE 需要知道**这一轮 AI 调用了哪个工具、这一次调用到底改了哪些文件、是预览还是已应用、每个文件的 diff 是什么**。
2. 这些信息必须是**单次工具调用级结果**，而不是事后从仓库状态里倒推。
3. 仅靠 `git diff` 只能看到“当前工作区相对某个基线的总变化”，不能稳定表达“本轮 AI 的这一次操作结果”。
4. 一旦内置编辑能力全面 LuaSkill 化，如果 runtime 仍只返回文本摘要，IDE 就无法稳定生成每轮执行结果面板。

因此：

- `change_set` 的目标不是替代 `git diff`。
- `change_set` 的目标是让 IDE 获得**操作级结果**。

仓库级 diff 仍然有价值，但它解决的是“当前工作区整体变化”，不是“这一轮 AI 工具执行具体做了什么”。

### 1.2 本文中的“宿主专用”与 `system_lua_lib` 定义

本文中的“宿主专用”不是指“默认对 AI 隐藏”的普通 skill，而是指一套**宿主自己拥有的 Lua 运行时能力**。

对宿主产品侧来说，这套能力更准确的名字应是：

- `system_lua_lib`

对 runtime 实现层来说，它更接近：

- 宿主 LuaRuntime
- 宿主持久 Lua 实例
- 宿主控制的独立 Lua 执行面

它指向的不是市场安装 skill 集，而是一个**固定系统 lib 目录**。宿主创建这类实例时，应把 `pwd/cwd` 固定到这个系统 lib 目录，再由宿主自己决定加载哪个模块、初始化哪些全局状态、保留哪些长期上下文。

因此本文中的“宿主专用”或宿主 LuaRuntime（`system_lua_lib`），是指下面这类能力：

1. 由宿主自己编写。
2. 由宿主自己发布和随产品集成。
3. 不通过市场安装、用户安装或项目安装进入系统。
4. 参数协议由宿主定义，而不是市场型通用工具协议。
5. 返回协议由宿主定义，而不是必须遵守生态公共返回形态。
6. 生命周期、缓存、刷新频率、执行池模型都由宿主单独控制。

这类能力的典型例子是：

- 动态 AST Tree
- 实时结构快照同步
- 提示词结构注入前的宿主内部刷新能力
- 长期驻留或低延迟连续刷新的内部结构工具

因此这类能力不能只靠 `HostOnly` 来表达。`HostOnly` 只回答“AI 能不能看到”，并不回答：

- 谁拥有这项能力
- 谁管理这项能力
- 它是否允许市场安装
- 它是否使用通用工具契约
- 它是否必须独占或驻留执行平面
- 它的状态是无状态、租约态还是宿主常驻态

## 2. 当前仓库实现基线

结合当前仓库代码，可以确认以下事实：

1. `RuntimeInvocationResult` 仍然是**文本优先**结果模型。
2. Lua skill 返回约定当前仍是 `content[, overflow_mode[, template_hint]]`。
3. 宿主能力开关机制已经存在正式先例。
4. 请求级 `client_capabilities` 已经能够注入 Lua 运行时。
5. 入口描述层目前还不能表达“对谁可见”。
6. 标准 C ABI 与公共 `_json` FFI 都已经是正式对外接入面，不能轻易破坏。

对应代码落点如下：

- `src/runtime/result.rs`
  - `RuntimeInvocationResult` 当前只包含 `content`、`overflow_mode`、`template_hint`、`content_bytes`、`content_lines`。
- `src/runtime/engine.rs`
  - `parse_tool_call_output()` 当前最多接收三个 Lua 返回值。
  - `list_entries()` / `list_entries_for_authority()` 当前只输出基础入口描述，不包含开放级别。
  - `populate_vulcan_request_context()` 已把 `client_capabilities` 注入到 `vulcan.context.client_capabilities`。
- `src/host/options.rs`
  - `LuaRuntimeCapabilityOptions` 已包含 `enable_skill_management_bridge` 与 `enable_managed_io_compat`。
- `src/runtime/context.rs`
  - `RuntimeRequestContext` 已正式携带 `client_capabilities: Value`。
- `src/skill/manifest.rs`
  - `SkillToolMeta` 当前没有 `exposure` 或 `management_plane` 一类字段。
- `src/runtime/entry.rs`
  - `RuntimeEntryDescriptor` 当前没有开放级别字段。
- `src/ffi.rs`
  - 公共 `_json` FFI 直接序列化 `RuntimeInvocationResult`，天然适合先承接新字段。
- `src/ffi_standard.rs`
  - 标准 C ABI 已有稳定结构体 `FfiLuaRuntimeHostOptions`、`FfiRuntimeEntryDescriptor`、`FfiRuntimeInvocationResult`，任何扩展都必须考虑 ABI 兼容。

## 3. 建议保留的总体方向

外部方案的整体方向与当前仓库结构是兼容的，以下三点建议保留：

### 3.1 LuaSkills 继续作为统一能力源

文件、搜索、编辑、AST、运行时帮助、工作记忆、宿主内部结构工具等能力，优先都应继续汇聚到 LuaSkills runtime 中，而不是分裂成多套“宿主私有协议”和“技能私有协议”。

### 3.2 继续采用“文本层 + 宿主结构层”的双层结果模型

保留现有文本结果：

- `content`
- `overflow_mode`
- `template_hint`

同时增加宿主结构化结果层，用于：

- diff 面板
- 结构树面板
- 宿主诊断面板
- artifact 或结构化摘要缓存

这比把所有结果都强行结构化更稳，也更兼容既有 skill。

### 3.3 继续采用“默认关闭，宿主显式开启”的能力策略

`host_result` 不应做成普通公开工具参数，而应继续走：

- host options 静态开关
- request context 动态能力协商

这与当前仓库已经存在的 `enable_skill_management_bridge` 设计哲学一致。

### 3.4 宿主 LuaRuntime（`system_lua_lib`）应作为宿主自有能力层单独成立

从 `VulcanCode` 的真实目标出发，宿主 LuaRuntime（`system_lua_lib`）不应再理解为“隐藏的普通 skill”，而应理解为：

- 宿主自有能力层
- 宿主深度集成能力层
- 宿主私有契约能力层

这类能力可以继续复用 LuaSkills 的统一外壳，例如：

- 统一 runtime
- 统一 entry registry
- 统一 `host_result` 信封
- 统一帮助与描述能力

但它们不应复用普通市场技能的管理方式。也就是说：

- 可以复用执行框架
- 不应复用市场化治理模型

## 4. 需要补强或修正的工程边界

外部方案方向是对的，但直接落到本仓库时，至少有七个点需要补强。

### 4.1 `host_result` 不建议在 runtime 内部长期保持“完全裸 JSON”

外部方案建议在 `RuntimeInvocationResult` 中直接增加：

```rust
pub host_result: Option<serde_json::Value>
```

这个方向可以作为 FFI 对外表现层，但不建议直接作为 runtime 内部唯一模型长期存在。

更稳妥的做法是：

1. 对外 JSON 形态仍然保持统一信封。
2. runtime 内部使用一个显式结构体承载统一信封。
3. `payload` 仍保持 `serde_json::Value`，以便保留扩展弹性。

推荐形态例如：

```rust
pub struct RuntimeHostResultEnvelope {
    pub version: String,
    pub kind: String,
    pub title: Option<String>,
    pub summary: Option<String>,
    pub payload: serde_json::Value,
}
```

这样做的好处是：

1. 可以在 runtime 中集中校验 `version`、`kind`、`payload`。
2. 可以统一处理大小限制、白名单限制和日志行为。
3. `ffi.rs` 仍然可以无损序列化该结构。
4. 标准 C ABI V2 也可以复用相同语义，不会把验证逻辑散落到多处。

### 4.2 `host_result` 的忽略策略要在 runtime 内部显式落地

第一版必须明确如下规则：

1. 只有 `request_context.client_capabilities.host_result.enabled == true` 时，第四返回值才进入结构化结果主链。
2. 宿主未开启 `host_result` 时，第四返回值直接忽略。
3. 宿主已开启 `host_result` 时，`kind`、`payload`、大小限制和 `change_set` 结构都必须严格校验。
4. 一旦宿主显式开启而 skill 返回了不合法的结构化结果，本次 skill 调用应明确失败，而不是静默吞掉。
5. 这条规则只影响第四返回值，不改变 `content` 仍是主文本结果这件事。

这组规则的原因很简单：`host_result` 是宿主消费能力，不是 skill 的必达主链结果。主链结果仍然是 `content`。

### 4.3 `exposure` 必须先有“声明来源”，再有“导出结果”

当前仓库中，入口元数据声明来源不能只看 `RuntimeEntryDescriptor`，还要看 entry 元信息到底从哪里来。

因此如果要正式引入：

```rust
enum RuntimeEntryExposure {
    AiPublic,
    HostOnly,
    Hybrid,
}
```

推荐的真实落地顺序应该是：

1. **对于普通 `managed skills`**：
   - `SkillToolMeta` 增加 `exposure` 字段。
   - 该字段在 `skill.yaml` 中可声明，且第一版默认值为 `ai_public`。
2. **对于宿主自有 `host_owned` entries**：
   - 需要来自宿主自有描述层或注册层。
   - 不应强行要求它们必须伪装成“市场 skill.yaml 声明”。
3. `RuntimeEntryDescriptor` 再把统一后的 `exposure` 对外带出。

如果只改 `RuntimeEntryDescriptor` 而不区分声明来源，运行时并没有稳定来源知道一个 entry 到底属于哪种开放级别。

### 4.4 仅有 `exposure` 不足以表达宿主专用能力

`exposure` 只描述“对谁可见”，但你当前的场景还需要至少三个额外维度：

1. **ownership**
   - 这项能力是宿主自有，还是市场/项目/用户受管。
2. **contract_kind**
   - 这项能力走通用工具契约，还是宿主私有输入输出契约。
3. **execution_plane**
   - 这项能力和普通工具共用运行池，还是独立运行池，还是常驻执行面。

如果没有这些维度，运行时无法区分下面两类对象：

- “只是对 AI 隐藏的普通工具”
- “宿主自己写、宿主自己管、宿主私有协议、宿主独立调度的内部能力”

而动态 AST、实时结构树、宿主内部快照同步显然属于第二类。

### 4.5 `exposure` 不应直接污染“低层宿主直连调用面”

这里要特别区分两个概念：

1. **host direct execution surface**
   - 宿主已经拿到 entry 名称后，直接调用 runtime 执行该能力。
2. **delegated / AI-facing export surface**
   - runtime 或其适配层向 AI、MCP 或普通工具列表暴露可见能力。

当前仓库中，[`call_skill`](../../../src/runtime/engine.rs) 的定位更接近第一种，即“当前已激活运行时执行面”，而不是“AI 工具可见性边界”。

因此更推荐的做法是：

1. 保持底层 `call_skill()` 的宿主直连能力不变。
2. 为查询与适配层增加 exposure-aware helper。
3. 由 MCP 适配层、宿主工具导出层或新的 delegated query helper 使用 `exposure` 做过滤。

换句话说：

- `HostOnly` 的意思应是“不能进入 AI 可见导出面”。
- 不是“宿主底层永远不能直接调这个 entry”。

否则宿主自己也无法直接调用宿主专用内部工具。

### 4.6 宿主 LuaRuntime（`system_lua_lib`）与当前 `SkillOperationPlane::System` 不是同一个概念

当前仓库已经存在：

```rust
enum SkillOperationPlane {
    Skills,
    System,
}
```

但这个 `System` 更接近：

- ROOT 写权限平面
- 生命周期操作平面
- authority 相关的系统控制面

它还不等于“宿主稳定内建、不可按普通 skill 管理、由宿主自己定义契约并可能拥有独立执行面的宿主 LuaRuntime（`system_lua_lib`）”。

因此若要正式引入宿主 LuaRuntime（`system_lua_lib`），建议明确区分多个正交维度：

1. `ownership`
   - `managed`
   - `host_owned`
2. `management_plane`
   - `managed`
   - `system`
3. `exposure`
   - `ai_public`
   - `host_only`
   - `hybrid`
4. `contract_kind`
   - `standard_tool`
   - `host_custom`
5. `execution_plane`
   - `shared_pool`
   - `dedicated_pool`
   - `resident_runtime`

这样才能表达下面这些不同对象：

- 普通可安装工具：`managed + managed + ai_public + standard_tool + shared_pool`
- 宿主专用结构工具：`host_owned + system + host_only + host_custom + dedicated_pool`
- 宿主长期驻留结构能力：`host_owned + system + host_only + host_custom + resident_runtime`
- 宿主稳定提供但可策略性给 AI 的只读能力：`host_owned + system + hybrid + host_custom + dedicated_pool`

### 4.7 状态生命周期与执行平面都需要进入正式设计，而不是留作实现细节

在动态 AST、结构树增量同步、提示词实时注入这类场景下，真正要建模的不是只有“它跑在哪个池里”，还包括“它的状态活多久、由谁持有、由谁驱动刷新”。

这里还需要补一条非常重要的工程判断：

**不建议为宿主 LuaRuntime 再重新发明一套全新的实例管理机制，而应优先复用 LuaSkills `0.3` 已有的 runtime session / lease 底座。**

原因是：

1. `0.3` 已经正式引入了租约模型。
2. 它已经有：
   - create
   - eval
   - status
   - list
   - close
3. 它已经有稳定的：
   - `lease_id`
   - `sid`
   - generation
   - busy / closed / expired / replaced 等状态语义
4. 对宿主来说，系统租约与普通租约在“对外提供接口”这件事上几乎是一致的。

因此更好的方向是：

- **底层机制同步**
- **入口命名分开**
- **生命周期策略分开**
- **上下文注入策略分开**

建议至少区分三类状态生命周期：

1. `stateless`
   - 每次调用独立执行，不保留跨调用状态。
2. `leased_stateful`
   - 状态存在于某个租约或会话里，由调用方显式 acquire / use / release。
3. `host_resident_stateful`
   - 状态由宿主自己持有和管理，调用侧不感知租约细节，能力以宿主常驻实例形式存在。

这里要特别说明：当前仓库已经有 runtime session / lease 一类语义，但它更接近“调用方可见的租约式执行面”。你现在说的 `ast-grep` 场景更接近第三类，也就是：

- 宿主知道某个工作区或工程实例长期存在
- 宿主触发一次 `refresh`
- 常驻实例直接基于既有状态做增量更新
- 宿主不需要每次重新传完整上下文，也不需要手工管理一组显式租约 ID

这类模型和普通 `run_lua` 式短调用、以及调用方显式租约式 session，都不是一回事。

原因如下：

1. 这类能力可能需要长期执行、持续刷新或状态常驻。
2. 有些能力天然适合租约态，但有些宿主能力不应要求宿主显式管理租约。
3. 这类能力和普通“短调用文本工具”共享 Lua 池时，会产生明显资源竞争。
4. 它们的调度目标也不同：
   - 普通工具更偏向短平快调用。
   - 租约态能力更偏向一个阶段内的连续交互。
   - 宿主结构能力更偏向低延迟刷新、状态保持和连续执行。
5. 如果把它们塞进常规 skill 池，会直接干扰普通 skills 的吞吐与稳定性。

因此建议正式同时引入 `state_model` 与 `execution_plane`，并明确：

- 普通 market / project / user skills 默认：
  - `state_model = Stateless`
  - `execution_plane = SharedPool`
- 需要调用方显式持有状态的能力：
  - `state_model = LeasedStateful`
  - `execution_plane = DedicatedPool`
- 宿主长期持有的结构能力：
  - `state_model = HostResidentStateful`
  - `execution_plane = DedicatedPool` 或 `ResidentRuntime`

`ast-grep` 一类场景非常适合最后一种模型：

1. 宿主先创建或预热一个工作区级常驻实例。
2. 实例内部保留增量索引、文件快照或结构状态。
3. 宿主后续只发送：
   - `refresh`
   - 变更文件列表
   - 或必要的最小增量信息
4. 实例直接在内部状态上做增量处理。

不推荐的做法是：

1. 每次都要求宿主传递完整工程信息。
2. 每次都重新构建整个状态。
3. 把这类能力塞进普通共享 Lua skill 池。

这条建议在你当前场景下不是“可选优化”，而是正式架构要求。

### 4.7.1 普通租约与系统租约应复用同一底座

如果按当前仓库实现继续演进，推荐直接沿着 `runtime session / lease` 这条线扩展：

1. 普通租约
   - 面向当前公开 session 模型
   - 默认有 TTL
   - 适合短期或宿主可见租约态交互
2. 系统租约
   - 复用同一套 lease manager / session manager
   - 默认不限时
   - 面向宿主长期持有的 LuaRuntime 实例

除了 TTL 与上下文语义之外，两者在宿主可见接口层也应尽量保持同步。换句话说，系统租约不应再发明一套完全不同的“看起来像租约、实际上不是租约”的工具暴露协议，而应继续复用租约体系已有的对象形状与操作动词。

也就是说，两者在底层最好共享：

- `lease_id`
- 状态查询
- 列表查询
- 关闭语义
- busy 语义
- replaced 语义
- 串行执行约束
- 对外结果基本形状
- 工具暴露层的调用家族一致性

真正分开的应是：

1. **入口名**
   - 普通租约入口继续保留当前 session / lease 风格
   - 系统租约入口独立命名，避免把“无限期宿主实例”混入普通公开租约语义
2. **TTL 规则**
   - 普通租约保留当前 TTL
   - 系统租约默认不限时，或由宿主自己决定回收时机
3. **上下文注入**
   - 普通租约仍是 skill/runtime 语义
   - 系统租约改为 `system_lua_lib` 宿主语义
4. **目录与路径模型**
   - 普通租约保留 skill 相关路径
   - 系统租约只保留宿主显式挂载路径

### 4.7.2 推荐不要直接污染现有公开租约语义

虽然底层应复用同一个租约系统，但我不建议直接把“无限期系统租约”硬塞进当前公开租约入口中，例如让所有 `create_runtime_session` 都天然支持无限期。

更稳妥的做法是：

1. 保持普通公开租约入口的现有语义不变。
2. 为系统租约增加独立入口。
3. 两者共享底层 manager 和大部分状态模型。

这样做的好处是：

1. 不破坏 `0.3` 已经对外形成的 session 心智模型。
2. 宿主内部能力可以安全扩展到“无限期、固定 lib 目录、显式挂载路径、宿主持有状态”。
3. 对外文档、FFI 和宿主接入面更容易解释。

### 4.8 标准 C ABI 的 V2 影响面不只 `FfiRuntimeInvocationResult`

外部方案里提到了不要破坏旧 ABI，这个结论是对的，但受影响的结构体范围需要写得更完整。

至少需要关注：

1. `FfiLuaRuntimeHostOptions`
   - 不应再增加 `enable_host_result_bridge` 一类宿主级总开关；`host_result` 的唯一开启入口应保持在请求级 `client_capabilities`。
2. `FfiRuntimeEntryDescriptor`
   - 需要承载 `exposure`，未来若引入 `management_plane` 也可能需要承载。
3. `FfiRuntimeInvocationResult`
   - 需要承载 `host_result_json` 或等价字段。

因此更推荐：

- 公共 `_json` FFI 直接增字段。
- 标准 C ABI 统一采用显式 V2 结构或 V2 导出符号。

不建议只为 `FfiRuntimeInvocationResult` 做零散补丁。

## 5. 推荐的第一版协议模型

### 5.1 `RuntimeInvocationResult`

推荐扩展为：

```rust
pub struct RuntimeInvocationResult {
    pub content: String,
    pub overflow_mode: Option<ToolOverflowMode>,
    pub template_hint: Option<String>,
    pub content_bytes: usize,
    pub content_lines: usize,
    pub host_result: Option<RuntimeHostResultEnvelope>,
}
```

注意：

- `content` 仍然必须是主返回值。
- `host_result` 只是宿主结构层，不替代 `content`。

### 5.2 Lua 返回约定

第一版建议扩展为：

```lua
return content, overflow_mode, template_hint, host_result
```

约束如下：

1. `content` 必须是字符串。
2. `overflow_mode` 仍保持当前字符串常量协议。
3. `template_hint` 仍保持当前字符串协议。
4. `host_result` 只能是 `nil` 或可 JSON 化的 table。
5. 宿主未开启能力时，`host_result` 必须被静默忽略。

### 5.3 `change_set` 的第一性定位

`change_set` 在这套设计中的第一性定位必须明确为：

- **单次工具调用结果**
- **单轮 AI 操作结果**
- **IDE 可直接消费的执行结果**

它不是：

- 仓库总 diff
- git 状态替代物
- 多轮统一汇总结果

对 `VulcanCode` 来说，`change_set` 的最小职责是告诉 IDE：

1. 这一次工具调用是否真正修改了工作区。
2. 修改的是哪些文件。
3. 每个文件的状态是什么。
4. 每个修改文件的每个修改块，具体删除了哪些旧行、插入了哪些新行，以及修改块前后的连续上下文是什么。
5. 当前结果是 `preview` 还是 `applied`。

只有把它定义成“操作级结果”，IDE 才能稳定生成每轮执行结果，而不是靠外部 `git diff` 去推测本轮发生了什么。

### 5.4 入口元数据维度模型

推荐把入口元数据拆成至少六个正式维度：

```rust
enum RuntimeEntryOwnership {
    Managed,
    HostOwned,
}

enum RuntimeEntryExposure {
    AiPublic,
    HostOnly,
    Hybrid,
}

enum RuntimeEntryManagementPlane {
    Managed,
    System,
}

enum RuntimeEntryStateModel {
    Stateless,
    LeasedStateful,
    HostResidentStateful,
}

enum RuntimeEntryContractKind {
    StandardTool,
    HostCustom,
}

enum RuntimeEntryExecutionPlane {
    SharedPool,
    DedicatedPool,
    ResidentRuntime,
}
```

各维度含义如下：

1. `ownership`
   - 描述这项能力属于市场/项目/用户受管能力，还是宿主自有能力。
2. `exposure`
   - 描述这项能力是否进入 AI 工具可见面。
3. `management_plane`
   - 描述这项能力是否进入普通受管生命周期治理面。
4. `state_model`
   - 描述这项能力是否跨调用保留状态，以及状态由谁持有。
5. `contract_kind`
   - 描述它使用通用工具协议，还是宿主私有输入输出协议。
6. `execution_plane`
   - 描述它由哪个执行平面承载。

推荐默认值：

- 普通 skills 默认：
  - `ownership = Managed`
  - `exposure = AiPublic`
  - `management_plane = Managed`
  - `state_model = Stateless`
  - `contract_kind = StandardTool`
  - `execution_plane = SharedPool`

推荐典型宿主结构能力取值：

- `dynamic-ast-tree`
  - `ownership = HostOwned`
  - `exposure = HostOnly`
  - `management_plane = System`
  - `state_model = HostResidentStateful`
  - `contract_kind = HostCustom`
  - `execution_plane = DedicatedPool` 或 `ResidentRuntime`

- 某些宿主受控交互式分析能力
  - `ownership = HostOwned`
  - `exposure = HostOnly` 或 `Hybrid`
  - `management_plane = System`
  - `state_model = LeasedStateful`
  - `contract_kind = HostCustom`
  - `execution_plane = DedicatedPool`

### 5.5 `RuntimeEntryExposure`

推荐第一版直接采用三态模型：

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeEntryExposure {
    AiPublic,
    HostOnly,
    Hybrid,
}
```

默认值建议：

- `AiPublic`

原因：

1. 最大限度兼容当前 skill 生态。
2. 只有新增宿主专用工具时才需要显式声明。
3. 不会让现有 entry 在升级后突然消失在工具列表中。

### 5.6 `client_capabilities.host_result`

建议沿用外部方案，但明确它只是**请求级协商对象**，不是业务参数：

```json
{
  "host_result": {
    "enabled": true,
    "allowed_kinds": [
      "change_set",
      "structured_tree",
      "vulcan_ast_live_tree"
    ],
    "max_payload_bytes": 524288
  }
}
```

这里的 `allowed_kinds` 应允许出现两类值：

1. 生态公共 kind
   - 例如 `change_set`、`structured_tree`
2. 宿主私有 kind
   - 例如仅供 `VulcanCode` 内部消费的结构刷新结果类型

runtime 只需要负责：

- 在 Lua 上下注入标准化 `vulcan.context.host_result` helper
- 在未开启时忽略第四返回值
- 在开启时严格校验并转交合法的结构化结果

不需要要求所有 `host_owned + host_custom` 的结果类型都进入公共市场语义。

### 5.7 `system_lua_lib` 的目录与实例边界

对宿主来说，`system_lua_lib` 应满足以下约束：

1. 它指向一个固定系统 lib 目录。
2. 宿主创建持久实例时，`pwd/cwd` 默认锁定到该目录。
3. 宿主通过该目录内的 Lua 模块组织自己的长期能力。
4. 宿主可以在实例初始化时显式执行：
   - `require(...)`
   - `_G.__host = ...`
   - 状态容器初始化
5. 后续宿主只需要发送最小控制命令或最小增量输入，不应每次重新传完整工程上下文。

这意味着它更像：

- 宿主可控制的 Lua 库目录
- 宿主自己的系统级 Lua 工作区

而不只是一个“额外的隐藏 skill”。

### 5.7.1 `system_lua_lib` 推荐直接建模成系统租约

从接口抽象上看，`system_lua_lib` 最好不要定义成一套与 `0.3` session 平行的新对象，而应直接建模成：

- **系统租约**

推荐语义是：

1. 宿主创建一个系统租约。
2. 该系统租约绑定一个固定 `system_lua_lib_dir`。
3. 宿主在该租约里初始化 Lua 全局状态、模块缓存与长期对象。
4. 宿主后续通过同一租约持续 `eval` 或调用约定脚本。
5. 宿主在不需要时显式 `close`。

这意味着：

- 普通租约与系统租约是同一种底座对象
- 只是入口不同、TTL 不同、上下文不同

### 5.7.2 系统租约的最小接口应与普通租约保持家族一致

既然要复用 `0.3` 租约底座，推荐系统租约也保持同一家族接口：

1. `open`
2. `eval`
3. `status`
4. `list`
5. `close`

不要轻易发明一套完全不同的对象命名或生命周期动词。

更准确地说，宿主在接入层最好能感知到：

1. 普通租约与系统租约属于同一种对象家族。
2. 两者都可以被列举、查询状态、执行、关闭。
3. 两者的差异主要体现在入口前缀、TTL 规则、上下文注入与路径模型，而不是工具暴露模型本身。

真正的区别应放在：

- 入口前缀不同
- 默认无限期
- 固定 `system_lua_lib_dir`
- skill 语义路径取消
- 宿主显式挂载路径生效

### 5.8 宿主可见租约与 skill 路径语义应当分离

如果宿主最终使用的是“创建一个持久 Lua 实例，再持续调用”的模型，那么这个模型在宿主协议层虽然很像租约，但它**不应继续继承 skill 语义路径注入**。

建议明确区分两类租约：

1. **内部 skill 执行租约**
   - 这是 runtime 为执行普通 skill entry 自己维护的内部机制。
   - 它可以继续保留：
     - `skill_dir`
     - `entry_dir`
     - `entry_file`
     - skill 依赖路径
     - skill 配置命名空间
2. **宿主可见 LuaRuntime 租约**
   - 这是宿主创建的通用 Lua 实例。
   - 它不应再自动注入：
     - `skill_dir`
     - `entry_dir`
     - `entry_file`
     - `skillpath`
     - `ffipath`
     - 基于当前 skill 推导的 `vulcan.deps.*`
     - 基于当前 skill 推导的 `vulcan.config.*`

原因很明确：

1. 这类租约并不天然绑定某个 market skill。
2. 它的运行语义是“宿主持有一个 Lua 实例”，不是“当前正在执行某个 skill entry”。
3. 如果继续注入 skill 路径语义，宿主最终仍然会被 skill 模型反向约束。
4. 这会让 `system_lua_lib` 再次退化成“伪装成 skill 的通用脚本容器”，边界会重新混乱。

因此，宿主可见 LuaRuntime 租约更好的做法是：

1. 明确取消 skill 语义路径。
2. 改成宿主显式挂载路径语义。

推荐最小路径字段例如：

- `system_lua_lib_dir`
- `cwd`
- `workspace_root`
- `lua_roots[]`
- `c_roots[]`
- `mounts[]`

这些字段全部由宿主决定，而不是由 runtime 假定“当前一定存在某个 skill 根目录”。

### 5.9 `managed_skill` 与 `system_lua_lib` 的路径上下文对比

建议把这两类模式的路径语义明确分开：

| 能力项 | `managed_skill` | `system_lua_lib` |
| --- | --- | --- |
| `skill_dir` | 有 | 无 |
| `entry_dir` | 有 | 无 |
| `entry_file` | 有 | 无 |
| `skillpath` | 有 | 无 |
| `ffipath` | 可由 skill 依赖派生 | 无自动派生 |
| `vulcan.deps.*` | 有 | 默认无 |
| `vulcan.config.*` 当前 skill 命名空间 | 有 | 默认无 |
| TTL | 默认有 | 默认无 |
| 生命周期入口 | 普通租约入口 | 系统租约入口 |
| `cwd` | 由 skill 或调用面决定 | 由宿主显式指定 |
| `system_lua_lib_dir` | 非必须 | 必有 |
| `lua_roots[]` | 可由 runtime 派生 | 由宿主显式指定 |
| `c_roots[]` | 可由 runtime 派生 | 由宿主显式指定 |

这个表的核心结论是：

- `managed_skill` 是 skill 语义环境
- `system_lua_lib` 是宿主 Lua 库环境

两者不应混成一个上下文模型。

## 6. 推荐的文件级落点

| 文件 | 当前职责 | 推荐改动 |
| --- | --- | --- |
| `src/runtime/result.rs` | 统一文本结果模型 | 增加 `host_result` 字段与统一信封结构；新增校验辅助函数 |
| `src/runtime/engine.rs` | Lua 返回解析、entry 列表、skill 调用主链 | 将 `parse_tool_call_output()` 扩成四返回值；增加 `host_result` 校验逻辑；为后续基于现有 runtime session 的系统租约路由预留承载点 |
| `src/runtime_context.rs` / `src/runtime_options.rs` | 请求级上下文与调用参数 | 保持 `request_context.client_capabilities.host_result` 作为唯一开启入口，不再引入宿主级 `enable_host_result_bridge` 总开关 |
| `src/runtime/context.rs` | 请求级上下文 | 保持 `client_capabilities` 原位；补充“skill 语义上下文”和“宿主 LuaRuntime 上下文”的区分 |
| `src/runtime/engine.rs` 的 runtime session 相关结构 | 当前 `0.3` 租约底座 | 建议直接扩展现有 session / lease 模型，支持系统租约入口与无限期生命周期策略 |
| `src/skill/manifest.rs` | skill.yaml 元数据来源 | 对 `managed skills` 的 `SkillToolMeta` 增加 `exposure`；必要时增加 `ownership` / `management_plane` / `state_model` / `contract_kind` 的可声明字段 |
| `src/runtime/entry.rs` | 对外 entry 描述 | 增加 `exposure`；后续可扩到 `ownership`、`management_plane`、`state_model`、`contract_kind`、`execution_plane` |
| `新增宿主 LuaRuntime 描述模块` | 承载 `host_owned` / `host_custom` 入口元数据与 `system_lua_lib` 目录约束 | 建议新增独立模块或注册层，不强行复用市场 skill manifest，并显式描述固定系统 lib 目录 |
| `新增宿主状态与执行平面模块` | 承载 `stateless` / `leased_stateful` / `host_resident_stateful` 与 `shared_pool` / `dedicated_pool` / `resident_runtime` 调度 | 建议独立模块或策略层，避免宿主长期任务干扰普通 Lua skill 池，并支持宿主常驻增量能力 |
| `新增宿主 LuaRuntime 路径挂载模块` | 承载 `system_lua_lib_dir`、`lua_roots[]`、`c_roots[]`、`mounts[]` | 建议独立定义宿主显式挂载路径模型，不再复用 skillpath / ffipath 自动注入语义 |
| `src/ffi.rs` | 公共 `_json` FFI | 直接让 JSON 返回新字段；必要时补 exposure-aware 查询请求参数，并预留 host-owned 查询面 |
| `src/ffi_standard.rs` | 标准 C ABI | 设计 `V2` 结构或新导出符号，避免破坏旧 ABI，并为 entry 元数据扩展、host_result V2、状态模型与宿主路径挂载模型留出口 |
| `docs/zh-CN/skill-development.md` | skill 作者手册 | 协议落地后补充第四返回值与 `exposure` 声明方式 |
| `docs/zh-CN/ffi/integration-guide.md` | FFI 对接文档 | 协议落地后补充 `host_result`、V2 ABI、宿主消费方式、状态模型、执行平面与路径挂载说明 |

## 7. 推荐的分阶段顺序

不建议一开始并行大改。更稳妥的顺序如下。

### 第一阶段：先把结果桥接骨架打稳

目标：

1. `RuntimeInvocationResult` 增加 `host_result`。
2. 明确 `request_context.client_capabilities.host_result` 是唯一开启入口。
3. `parse_tool_call_output()` 支持第四返回值。
4. runtime 内部完成 `host_result` 校验与忽略策略。
5. 公共 `_json` FFI 同步打通新字段。

这一阶段只需要打通 runtime 主链，不要求立刻有全部真实工具返回结构化结果。

### 第二阶段：先打通一个真实 `change_set`

推荐首个对象：

- `vulcan-file-edit`

第一版建议约束：

1. 只处理文本文件。
2. `modify` 必须返回 `hunks[]`，每个 hunk 使用 `before + delete[] + insert[] + after` 表达具体修改块。
3. 只区分 `preview` 与 `applied`。
4. `content` 只保留简短摘要。
5. `create` / `delete` 直接返回完整文件 `content`，`rename` 返回 `old_path/new_path`。

这一步优先级应高于系统租约扩展，因为它直接决定 IDE 是否能获得“每轮 AI 操作结果”。

### 第三阶段：基于现有 `0.3` 租约打通系统租约入口

目标：

1. 复用现有 runtime session / lease manager。
2. 增加系统租约入口。
3. 系统租约默认不限时。
4. 系统租约改用 `system_lua_lib` 路径与上下文语义。
5. 保持状态查询、列表、关闭等接口家族一致。

这一阶段的重点是“复用同一底座，不重复造轮子”。

### 第四阶段：补入口元数据维度与宿主 LuaRuntime 模型

目标：

1. `managed skills` 的 `SkillToolMeta` 支持 `exposure`。
2. `RuntimeEntryDescriptor` 至少输出 `exposure`，并预留 `ownership`、`contract_kind`、`execution_plane`。
3. 为 `host_owned` entries 与 `system_lua_lib` 增加独立描述来源或注册层。
4. 为适配层提供 exposure-aware 列表或过滤 helper。

这一阶段的重点是“声明来源、目录边界与治理分层”，不是单纯的工具列表隐藏。

### 第五阶段：宿主专用结构工具、有状态模型与独立执行平面

推荐首个对象：

- `dynamic-ast-tree`

推荐产品定位：

- `host_owned + system + host_only + host_custom`
- `host_resident_stateful`
- `dedicated_pool` 或 `resident_runtime`
- 宿主内部刷新结构树
- `content` 只做日志摘要
- 主结果走 `structured_tree`
- 必要时允许宿主私有 `host_result.kind`

推荐同时用 `ast-grep` 一类工具验证下面这条链路：

1. 宿主预热常驻实例。
2. 实例保留状态。
3. 宿主发送 `refresh` 或最小增量。
4. 实例直接做增量刷新。
5. 宿主获取最新结构结果，而不是每次重建整棵树。

这一阶段的重点不是把它“做成另一个普通 skill”，而是把它正式做成**宿主自有 LuaRuntime 能力层**。

### 第六阶段：最后补标准 C ABI V2

原因：

1. `_json` FFI 本身更容易迭代。
2. 标准 ABI 需要考虑结构体布局、分配释放与兼容期。
3. 先把 runtime 语义、系统租约、入口维度和宿主执行平面打稳，再做 ABI V2，返工会更少。

## 8. 第一版建议明确不做的事

为了避免第一版过重，建议明确暂不做以下内容：

1. 不做任意 `host_result.kind` 的开放注册系统。
2. 不做 `artifact`、`diagnostic_bundle` 等次级 kind 的首批支持。
3. 不把 `host_result` 做成公开工具参数。
4. 不要求低层 `call_skill()` 自身直接承担 AI 可见性策略边界。
5. 不一次性把所有宿主内建能力全部迁移进统一的 `system_lua_lib` 模型。
6. 不要求第一版就把所有 `host_owned` 能力都标准化成公共市场契约。

## 9. 推荐验收标准

正式落地后，至少应满足以下验收标准：

1. 旧 skill 无需修改仍可正常加载和调用。
2. 宿主未开启 `request_context.client_capabilities.host_result.enabled` 时，第四返回值不会导致报错。
3. `_json` FFI 能稳定返回 `host_result`。
4. `vulcan-file-edit` 能返回最小但结构明确的 `change_set`，包括 `modify` 的 hunk 级 `before/delete/insert/after` 信息。
5. 宿主可直接消费 `change_set` 驱动每轮执行结果面板，而不是依赖外部 `git diff` 反推。
6. `RuntimeEntryDescriptor` 能稳定输出 `exposure`，并为后续 `ownership` 等维度扩展预留空间。
7. 宿主可注册或加载 `host_owned` / `host_custom` 入口，且它们不进入普通市场安装治理面。
8. `dynamic-ast-tree` 一类工具可作为宿主自有能力被调用，不要求进入普通 AI 工具列表。
9. 宿主可为长期运行的结构能力指定独立执行平面，且不会与普通 Lua skill 池互相干扰。
10. 系统租约与普通租约复用同一套底层 lease/session 机制，但入口与 TTL 语义不同。
11. 宿主可驱动 `host_resident_stateful` 能力执行增量刷新，而不要求每次重传完整上下文。
12. 标准 C ABI 保持旧符号可用，新能力通过 V2 进入。

## 10. 结论

当前 `vulcan-luaskills` 已经具备承接这次协议升级的大部分基础：

- 有统一结果对象
- 有统一 Lua 调用解析点
- 有宿主能力开关先例
- 有请求级能力协商对象
- 有双 FFI 接入面

因此这次改造不应理解为“重做 runtime”，而应理解为：

1. 给现有文本结果模型增加一层正式宿主结构桥。
2. 基于 `0.3` 已有租约底座扩出系统租约入口，而不是另起一套实例机制。
3. 给现有 entry 描述模型增加一层正式开放级别语义。
4. 给宿主自有能力补上独立治理语义。
5. 给长期运行结构能力补上独立状态生命周期与执行平面语义。

一句话结论是：

**LuaSkills 现在最适合做的不是推翻现有文本技能主链，而是在现有主链上增量补齐 `host_result`、`change_set`、基于 `0.3` 租约复用出来的系统租约、宿主 LuaRuntime（`system_lua_lib`）、`state_model`、`execution_plane` 和 V2 FFI 这些正式协议线。**
