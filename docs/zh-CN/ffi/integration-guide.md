# luaskills FFI 对接文档

## 1. 文档目标

本文档用于说明 `luaskills` 当前导出的 FFI 接口设计、启动条件、参数模型、调用顺序、返回规则、内存释放方式，以及 install / update / uninstall 等整条链路的处理逻辑。

如果您希望先按最短路径做宿主自检，而不是从头读完整文档，请先看：

- [FFI Beta 发布说明](beta-release-notes.md)
- [FFI 宿主接入检查清单](host-checklist.md)
- [宿主工具结果桥接、宿主 LuaRuntime（`system_lua_lib`）与执行平面设计稿](../architecture/host-tooling-result-bridge-design.md)

本文档覆盖当前对外公开的两层 FFI：

- 标准 C ABI
- 公共 `_json` FFI

两层接口位于同一个动态库或静态库中，主能力基本一一对应，但定位并不相同。

## 2. 整体设计

### 2.1 同一套核心实现

无论是标准 C ABI 还是公共 `_json` FFI，底层都调用同一套 `LuaEngine` 核运行时逻辑。  
这意味着：

- 行为一致
- 生命周期一致
- 错误语义一致
- 事务语义一致

### 2.2 两层 FFI 角色

#### 标准 C ABI

标准 C ABI 使用：

- C ABI 基本类型
- 结构体指针
- 指针加长度
- `error_out`
- 专用 free 函数

适合：

- C / C++
- Go
- 能稳定处理 C 结构与 out 指针的宿主
- 性能敏感场景
- 希望避免双向 JSON 编解码的宿主

它是：

- 低层正式契约
- 结构更稳定的底层接入面
- 后续社区生成绑定时的基础 ABI

#### 公共 `_json` FFI

公共 `_json` FFI 使用：

- 输入：JSON 文本缓冲
- 输出：统一 JSON 包络

适合：

- Python
- TypeScript / Node.js
- ctypes / cffi / ffi-napi 一类动态桥接
- 原型验证
- 快速接入
- 不想维护复杂 ABI 结构的宿主

它是：

- 高层易用接口
- 面向动态语言和快速集成的正式公共接口
- 对标准 C ABI 的便利封装，而不是临时调试层

### 2.3 为什么两层都保留

原因很直接：

- 标准 C ABI 更稳定，但接入门槛更高
- 公共 `_json` FFI 更易用，但双向编解码成本更高

因此当前协议定为：

- 对外继续同时维护两层能力
- 标准 C ABI 负责承载低层正式契约
- 公共 `_json` FFI 负责承载动态语言与快速接入入口

### 2.4 宿主数据库 provider 也遵循双层规则

当宿主需要自己接管：

- SQLite
- LanceDB

时，当前协议同样保留两层回调形式：

- 标准结构化回调
- JSON 回调

也就是说：

- 宿主如果已经能稳定处理 C ABI 结构和 out 指针，应优先接标准回调
- 宿主如果是 Python / Node / 快速原型，则可直接接 JSON 回调

数据库 provider 的详细对接说明见：

- [宿主数据库 Provider 对接说明](../providers/host-database-provider-guide.md)

### 2.5 固定术语

为了避免正文里同时出现“标准 ABI”“标准接口”“标准 FFI”“JSON 接口”等多套说法，当前统一固定为：

- `标准 C ABI`
  - 指低层、结构化、面向正式宿主契约的 FFI 接口层
- `公共 `_json` FFI`
  - 指高层、JSON 包络、面向动态语言和快速集成的 FFI 接口层
- `标准 C ABI 头文件`
  - 指 [include/luaskills_ffi.h](../../../include/luaskills_ffi.h)
- `公共 `_json` FFI 头文件`
  - 指 [include/luaskills_json_ffi.h](../../../include/luaskills_json_ffi.h)

如果正文里为了简化阅读出现：

- `标准 C ABI 接口`
- `公共 `_json` FFI 接口`

也都只是上面这两套固定术语的缩写表达。

### 2.6 当前基础对接主链能力

当前正式对接文档默认覆盖以下宿主主链能力：

- 公共 `runtime_lease_*_json`
- 带 authority 的 `system_runtime_lease_*_json`
- 严格 `system_package { id, root, dependencies_file }`、包/依赖隔离与专用 System VM
- 受管 Python/Node `session.open(...)`、引擎事件 poll/wait 与标准 ABI wake callback
- `host_result` 第四返回值桥接
- `change_set` 这一类结构化宿主结果

也就是说，本文档不是只覆盖最早的 `call_skill / run_lua` 主链，而是覆盖当前 `0.4.x` 稳定线实际对外承诺的完整宿主集成面。

## 3. 运行时资产、动态库与头文件

### 3.1 当前运行时资产边界

当前稳定协议线中，FFI 对接方需要明确区分两类发布来源。
这条边界从 `0.3` 建立，并延续到当前 `0.4.x` 稳定线：

| 资产 | 来源仓库 |
| --- | --- |
| `luaskills-ffi-sdk-{platform}.tar.gz` | `LuaSkills/luaskills` |
| `lua-runtime-packages-{platform}.tar.gz` | `LuaSkills/luaskills-packages` |
| `lua-deps-{platform}.tar.gz` | `LuaSkills/luaskills-packages` |

也就是说：

- `luaskills` 主仓库负责 crate、本体动态库、FFI SDK 和 demo 资产
- `luaskills-packages` 负责 Lua runtime packages、预编译 native deps、help 元数据和第三方 license 元数据
- SDK、demo 和宿主安装脚本应按这条边界组合下载，而不是再假设主仓库 release 自带完整 Lua runtime

若宿主直接消费 packaged runtime，还应确保 runtime 目录中存在下列 `luaskills-packages` 元数据：

- `resources/luaskills-packages-manifest.json`
- `resources/luaskills-packages/install-manifest.json`
- `resources/luaskills-packages/lua_packages.txt`
- `resources/luaskills-packages/platform-support.json`
- `resources/luaskills-packages/THIRD_PARTY_LICENSES.json`
- `resources/luaskills-packages/THIRD_PARTY_NOTICES.md`
- `resources/luaskills-packages/help/index.json`
- `resources/luaskills-packages/help/packages`
- `resources/luaskills-packages/help/modules`
- `licenses/luaskills-packages/index.json`

缺少这些文件时，当前运行时会把它视为不完整 packaged runtime，并在初始化阶段直接报错。

### 3.2 动态库与头文件

当前产物方向：

- `rlib`
- `cdylib`
- `staticlib`

核心文件：

- 标准 C ABI 导出：
  - [src/ffi_standard.rs](../../../src/ffi_standard.rs)
- 公共 JSON FFI 导出：
  - [src/ffi.rs](../../../src/ffi.rs)
- 标准头文件：
  - [include/luaskills_ffi.h](../../../include/luaskills_ffi.h)
- JSON 头文件：
  - [include/luaskills_json_ffi.h](../../../include/luaskills_json_ffi.h)
  - 该头文件会复用标准头文件中的共享结构体与释放辅助函数
- 示例：
  - [examples/ffi/c/demo.c](../../../examples/ffi/c/demo.c)
  - [examples/ffi/python/demo.py](../../../examples/ffi/python/demo.py)
  - [examples/ffi/go/demo.go](../../../examples/ffi/go/demo.go)
  - [examples/ffi/typescript/demo.ts](../../../examples/ffi/typescript/demo.ts)
  - [examples/ffi/c/README.md](../../../examples/ffi/c/README.md)
  - [examples/ffi/standard_runtime/README.md](../../../examples/ffi/standard_runtime/README.md)

当前 FFI 版本字符串统一派生自 crate 包版本：

- `env!("CARGO_PKG_VERSION")`

也就是说：

- 标准 C ABI `luaskills_ffi_version`
- 公共 JSON FFI `luaskills_ffi_version_json`
- 自描述结果中的 `ffi_version`

都与 `Cargo.toml` 中的 `version` 保持同源。

## 4. 引擎与句柄模型

### 4.1 引擎句柄

FFI 不直接暴露 `LuaEngine` 指针，而是通过内部注册表分配一个稳定的 `u64 engine_id`。

宿主流程：

1. 调用 `engine_new` 创建引擎
2. 获取 `engine_id`
3. 后续所有 load / help / call / install / update / uninstall 都通过这个 `engine_id` 定位引擎
4. 最后调用 `engine_free`

### 4.2 生命周期要求

一个 `engine_id` 的推荐生命周期：

1. `engine_new`
2. `load_from_roots`
3. 若干：
   - `list_entries`
   - `list_skill_help`
   - `render_skill_help_detail`
   - `call_skill`
   - `run_lua`
   - 生命周期操作
4. `engine_free`

如果未先 load，就直接调用依赖 skill 注册表的接口，结果通常是：

- 空列表
- 找不到 skill
- 找不到 entry

因此：

**创建引擎后，必须先完成至少一次加载。**

## 5. 标准 C ABI 接口返回与错误模型

### 5.1 标准 C ABI 接口统一规则

标准 C ABI 接口一般采用：

- 返回值：`int32_t`
- 成功：`0`
- 失败：非 `0`
- 错误消息：通过 `FfiOwnedBuffer *error_out`

若接口需要输出结构化结果，则使用：

- `*_out`
- 再配套 free 函数释放

也就是说：

- 标准 C ABI 接口的失败信息不再通过裸 `char **` 传出
- 调用方应把 `error_out` 当作 UTF-8 错误缓冲读取
- 读取完成后应通过 `luaskills_ffi_buffer_free` 释放
- 标准 C ABI 接口中的直接文本输出也在逐步收敛到 `FfiOwnedBuffer`
- 当前 `version_out`、`skill_id_out`、`result_json_out` 都应按拥有型缓冲读取与释放
- 当前 `FfiRuntimeInvocationResult`
  - `content`
  - `template_hint`
  也已改成 `FfiOwnedBuffer`
- 当前 `FfiSkillApplyResult`
  - `skill_id`
  - `status`
  - `message`
  - `version`
  - `source_locator`
  也已改成 `FfiOwnedBuffer`
- 当前 `FfiSkillUninstallResult`
  - `skill_id`
  - `message`
  也已改成 `FfiOwnedBuffer`
- 当前 `FfiRuntimeEntryParameterDescriptor`
  - `name`
  - `param_type`
  - `description`
  也已改成 `FfiOwnedBuffer`
- 当前 `FfiRuntimeEntryDescriptor`
  - `canonical_name`
  - `skill_id`
  - `local_name`
  - `root_name`
  - `skill_dir`
  - `description`
  也已改成 `FfiOwnedBuffer`
- 当前 `FfiRuntimeHelpNodeDescriptor`
  - `flow_name`
  - `description`
  也已改成 `FfiOwnedBuffer`
- 当前 `FfiRuntimeSkillHelpDescriptor`
  - `skill_id`
  - `skill_name`
  - `skill_version`
  - `root_name`
  - `skill_dir`
  也已改成 `FfiOwnedBuffer`
- 当前 `FfiRuntimeHelpDetail`
  - `skill_id`
  - `skill_name`
  - `skill_version`
  - `root_name`
  - `skill_dir`
  - `flow_name`
  - `description`
  - `content_type`
  - `content`
  也已改成 `FfiOwnedBuffer`

### 5.2 `_json` 接口统一规则

`_json` 接口统一返回一个 JSON 包络缓冲：

```json
{"ok":true,"result":{...}}
```

失败时：

```json
{"ok":false,"error":"..."}
```

返回值统一通过：

- `FfiOwnedBuffer`

承载，并通过：

- `luaskills_ffi_buffer_free`

释放。

请求输入统一通过：

- `FfiBorrowedBuffer`

传入。

也就是说：

- `_json` 接口的请求输入不再依赖 NUL 终止字符串
- 调用方必须显式提供 `ptr + len`
- `ptr` 只需在当前调用期间保持有效

## 6. 内存所有权与释放规则

### 6.1 字符串辅助函数

由 FFI 返回的独立堆分配字符串必须由调用方释放：

- `luaskills_ffi_string_free`

适用于：

- `luaskills_ffi_string_clone` 这类字符串辅助函数返回值

说明：

- 这组接口当前属于**公共 JSON FFI / 辅助函数层**
- 标准 C ABI 主结果通道不再推荐依赖裸 `char *`
- 标准 C ABI 应优先使用 `FfiOwnedBuffer` 与结构体专用 free 函数

### 6.2 字符串数组

释放函数：

- `luaskills_ffi_string_array_free`

说明：

- `FfiStringArray.items` 当前已经收敛为 `FfiOwnedBuffer *`
- 每个数组元素都是拥有型 UTF-8 文本缓冲
- 但调用方仍然不应手动逐项释放，而应继续把整个数组交给 `luaskills_ffi_string_array_free`

### 6.2.1 拥有型缓冲

当 callback 或后续扩展接口返回 `FfiOwnedBuffer` 时：

- 分配应优先使用 `luaskills_ffi_buffer_clone`
- 释放应使用 `luaskills_ffi_buffer_free`

适用于：

- 标准 C ABI 接口中的 `version_out`
- 标准 C ABI 接口中的 `skill_id_out`
- 标准 C ABI 接口中的 `result_json_out`
- `_json` 接口返回值
- JSON callback 的 `response_out`
- JSON callback 的 `error_out`
- 标准 provider callback 的 `response_json_out`
- 标准 provider callback 的 `meta_json_out`
- 标准 provider callback 的 `data_out`
- 标准 provider callback 的 `error_out`

### 6.3 结构化列表与结果

当前标准 C ABI 接口返回的结构化对象，都有对应 free 函数：

- `luaskills_ffi_entry_list_free`
- `luaskills_ffi_help_list_free`
- `luaskills_ffi_help_detail_free`
- `luaskills_ffi_invocation_result_free`
- `luaskills_ffi_skill_apply_result_free`
- `luaskills_ffi_skill_uninstall_result_free`

说明：

- 这些结构体内部已经开始逐步采用 `FfiOwnedBuffer` 承载文本字段
- 默认仍应把整个结构体交给对应专用 free 函数释放
- 不要在调用专用 free 前手动释放结构体内嵌的 `FfiOwnedBuffer` 字段，否则会导致重复释放
- `related_entries` 这类数组字段当前虽然仍以“数组 + 长度”形式存在，但数组元素本身也已经是 `FfiOwnedBuffer`
- 这类数组字段仍应通过宿主最终调用结构体专用 free 函数统一回收

规则只有一条：

**凡是 FFI 分配出来的结果结构，必须使用对应 free 函数释放。**

### 6.4 当前 ABI 迁移要点

当前正式协议线已经对现有 FFI 做完一轮接口收敛。
如果宿主参考的是更早的 `0.2.x / beta` 示例、旧草稿或早期 SDK 封装，请优先按下面的对应关系理解：

- 旧：标准 C ABI 接口大量使用 `char **error_out`
  - 新：标准 C ABI 接口统一改成 `FfiOwnedBuffer *error_out`
- 旧：`version_out` / `skill_id_out` / `result_json_out` 这类文本输出按裸字符串读取
  - 新：这些文本输出都应按 `FfiOwnedBuffer` 读取，并通过 `luaskills_ffi_buffer_free` 释放
- 旧：`_json` 请求输入依赖 NUL 终止字符串
  - 新：`_json` 请求输入统一改成 `FfiBorrowedBuffer`
- 旧：JSON provider callback 通过裸字符串返回响应
  - 新：JSON provider callback 通过 `FfiOwnedBuffer response_out / error_out` 返回
- 旧：标准 SQLite / LanceDB provider callback 的 `input_json` 是裸字符串
  - 新：标准 provider request 的 `input_json` 统一改成 `FfiBorrowedBuffer`
- 旧：标准 `call_skill / run_lua / render_skill_help_detail` 的请求级 JSON 输入按裸字符串传入
  - 新：这些输入当前统一改成 `FfiBorrowedBuffer`
- 旧：`FfiLuaInvocationContext` 里的三个 JSON 字段按裸字符串传入
  - 新：`FfiLuaInvocationContext` 当前统一改成三段 `FfiBorrowedBuffer`
- 旧：`FfiStringArray.items` 与 `related_entries` 数组元素按 `char **` 理解
  - 新：这类数组元素已经统一收敛为 `FfiOwnedBuffer *`
- 旧：结构体内部文本字段多数按裸 `char *` 处理
  - 新：标准结果结构中的大量文本字段已改成 `FfiOwnedBuffer`

迁移时最容易出错的只有两条：

1. 不要继续把标准错误输出按 `char *` 读取。
2. 不要手动释放结构体内嵌的 `FfiOwnedBuffer` 字段，仍应优先调用结构体专用 free 函数。

### 6.5 当前发布定位与安全边界

当前 FFI 发布面应按以下定位理解：

- 当前版本已经是 `0.4.x` 稳定线下的**受控宿主集成接口**
- 当前版本的主集成方式仍然是 Rust 直连，FFI 主要服务于非 Rust 宿主或跨语言桥接
- packaged runtime、FFI SDK 与 Lua runtime packages 已经拆成独立来源，宿主应接受这条新的资产边界
- FFI 是低层 ABI，不承诺“误用后仍然安全”，宿主必须严格遵守本文档中的所有权、线程与回调规则
- 当前运行时默认把 skill 当作**受信代码**看待，FFI 文档不提供 Lua skill 沙箱安全承诺

### 6.6 必须遵守的 FFI 契约

以下规则应视为强约束，而不是最佳实践建议：

- `luaskills_ffi_string_free` 只能释放 **luaskills 自己分配并返回** 的字符串
- `luaskills_ffi_string_clone` / `luaskills_ffi_bytes_clone` / `luaskills_ffi_buffer_clone` 用于把宿主自己的内存复制成 luaskills 自主管理的返回值
- 宿主不能把自己分配的 `malloc/new/string buffer` 直接交给 `luaskills_ffi_string_free`
- 宿主不能把自己分配的裸缓冲伪装成 `FfiOwnedBuffer` 交给运行时
- 所有传入 FFI 的裸指针、切片指针、输出指针都必须在调用期间保持有效
- 标准 callback 与 JSON callback 都**不能**把 Rust panic、C++ exception 或其他异常机制穿过 C ABI 边界
- 同一线程内，不支持在一个 engine 的 FFI 调用尚未返回时再次重入同一个 engine
- 若宿主需要数据库 callback 或运行时技能管理 callback，必须先注册 callback，再创建 engine
- callback 返回文本时，文本必须是合法 UTF-8；其中 JSON callback 与标准 provider callback 的 JSON 载荷还必须是合法 JSON 文本

### 6.7 回调与快照规则

当前回调模型需要宿主特别注意：

- database provider callback 会在 `engine_new` 时拍快照
- engine 创建完成后再修改回调注册表，不会 retroactive 地影响已存在 engine
- 若一个进程内需要多套数据库 provider callback 逻辑，应按 callback 集合分别创建 engine，让 engine 捕获各自的 provider callback 快照，而不是复用同一个 engine 再切换全局 provider callback
- 对动态语言宿主，应优先从 JSON callback 模式接入，因为所有权模型更简单，误用面更小
- `vulcan.host.*` 宿主工具桥接使用进程级 host-tool callback，并在 Lua 调用时读取当前全局 callback；正式接入仍建议在 `engine_new` 前完成注册，避免初始化顺序不稳定
- 若一个进程内需要多套 host-tool 逻辑，应由宿主在 callback 内自行路由，或避免在同一进程内混用
- 受管会话 wake callback 属于引擎级边沿通知，由每个引擎的单个串行后台调度线程调用；它只能唤醒或调度宿主任务，不能执行 Lua 或同步重入租约 eval
- 替换或清除 wake callback 会等待旧代际的在途调用收敛，宿主只能在调用成功返回后释放旧 `user_data`

## 7. 启动条件与前置要求

### 7.1 引擎创建阶段必须提供的关键配置

`LuaEngineOptions` 内部包含：

- VM 池配置
- 宿主目录与路径配置
- 下载能力配置
- 依赖 sibling 目录名配置
- 受保护 skill 配置

其中宿主选项里和运行时目录最直接相关的关键字段是：

- `runtime_root`
- `allow_network_download`

`runtime_root` 是新的规范入口。传入后，LuaSkills 会固定推导下面的运行时布局：

- `bin`：宿主提供的工具根目录，工具直接放在这里，不再使用 `bin/tools`
- `libs`：宿主提供的 FFI / 原生库根目录，Lua C module 与 DLL 依赖默认从这里补充搜索
- `lua_packages`：Lua 包根目录，同时作为 Lua package 探测根
- `resources`：打包运行时元数据与资源目录
- `skills`：已安装或已同步的 LuaSkills 包目录
- `temp` 与 `temp/downloads`：临时文件与下载缓存目录
- `dependencies`、`state`、`databases`：依赖、状态与数据库目录
- `config/skill_config.json`：统一 skill 配置文件
- `system_lua_lib`：宿主自有 system Lua 库目录

System Plugin 租约不把整个 `system_lua_lib` 当成一个共享包。每次 System `create` 必须额外提交：

```json
{
  "system_package": {
    "id": "host-indexer",
    "root": "D:/runtime/system_lua_lib/host-indexer",
    "dependencies_file": "dependencies.yaml"
  }
}
```

其中 `root` 必须是规范 `system_lua_lib` 下的绝对严格子目录；`dependencies_file` 是包相对路径，必须解析为包内普通文件，目录穿越和符号链接逃逸都会失败。该包根是专用 System VM 唯一新增的 Lua 模块根，兄弟插件不会共享 `require` 搜索域；Python/Node 环境也按精确包身份、清单和锁文件隔离。`node_runtime.version` 必须是精确 SemVer，且 Node.js 最低版本为 `22.0.0`；范围、标签、不完整版本及更旧版本都会在创建环境前失败。

每个存活 Worker 或持久会话快照都会持有对应受管环境的跨进程共享生命周期租约。环境最终发布或替换只尝试一次非阻塞独占租约；存在活动 Worker 或会话时立即返回稳定的 busy 错误，不会等待、强退使用方或原地替换环境。

旧字段 `temp_dir`、`resources_dir`、`lua_packages_dir`、`host_provided_tool_root`、`host_provided_lua_root`、`host_provided_ffi_root`、`download_cache_root`、`dependency_dir_name`、`state_dir_name` 与 `database_dir_name` 仍属于既有运行时布局接口。技能配置不沿用这套推导：宿主必须单独提供绝对用户级 `skill_config_root`，没有旧单文件配置字段或路径猜测。JSON FFI 直接在 host options 里传 `runtime_root`；标准 C ABI 使用对应版本结构传入运行时与受管根。

### 7.1.1 Space Controller 额外前置要求

当 SQLite 或 LanceDB 的 provider mode 选择 `space_controller` 时，宿主还需要额外准备：

- `LuaRuntimeHostOptions.space_controller.endpoint`
  - 可选
  - 缺失时默认使用共享端点 `http://127.0.0.1:19801`
- `LuaRuntimeHostOptions.space_controller.auto_spawn`
  - 是否允许自动唤起 controller
  - 若为 `true`，`endpoint` 必须使用本地可绑定地址格式，例如 `19801`、`:19801`、`127.0.0.1:19801`、`localhost:19801` 或 `[::1]:19801`
- `LuaRuntimeHostOptions.space_controller.executable_path`
  - 可选
  - 指向宿主已经复制到本地稳定目录的 `vldb-controller` 可执行文件
- `LuaRuntimeHostOptions.space_controller.process_mode`
  - `service`
  - `managed`

这里要特别注意：

- `luaskills` 代码层通过 crates.io 版本 `vldb-controller-client = 0.2.3` 固定依赖 controller Rust SDK
- 当前上游 Rust SDK 在注册阶段使用 `client_name`，具体 `client_session_id` 由 SDK 内部自动管理并自动回放附着与 backend 期望状态
- `v0.2.1` 额外修复了共享本地 endpoint 在 `auto_spawn` 场景下的重复拉起协调风险
- 真正被拉起的 controller 服务程序，不是通过 Cargo 把二进制嵌进宿主，而是由宿主自行复制并管理
- 也就是说，**Rust SDK 走 crates.io 固定版本，controller 可执行文件走宿主本地复制路径**
- 如果宿主要连接远端 controller 或使用远端主机名端点，必须关闭 `auto_spawn`，避免把远端地址错误地当成本地 bind 地址去拉起新进程

### 7.1.2 callback 注册前置要求

若宿主准备使用：

- `host_callback`
- `vulcan.runtime.skills.*` 的运行时技能管理桥接
- `vulcan.host.*` 的宿主工具桥接

则应在 `engine_new` 前完成所有必需 callback 注册。

原因不是初始化顺序习惯，而是当前运行时会在 engine 创建时捕获关键 callback 状态：

- 数据库 provider callback 采用 engine 私有快照
- 技能管理桥接在运行时会显式检查宿主 callback 是否可用
- 宿主工具桥接在 Lua 调用时显式检查 host-tool callback 是否可用

因此：

- 先 `engine_new` 再注册 callback，不应被视为可靠初始化顺序
- 正式宿主接入应把“注册 callback -> 创建 engine -> load/reload -> call”作为固定启动流程
- `load_from_roots` / `reload_from_roots` 的 root 链必须包含 `ROOT`；如果没有传入 `ROOT`，运行时会明确报错并拒绝加载。

### 7.2 生命周期接口的前置要求

若调用：

- `install`
- `update`
- `uninstall`
- `enable`
- `disable`

则需要：

- 引擎已创建
- 已提供有效的 skill roots
- skill roots 必须包含 `ROOT`
- roots 对应空间满足 sibling 目录协议
- 正式宿主应按 `ROOT -> PROJECT -> USER` 构造三层逻辑，且普通用户管理面只操作 `PROJECT` / `USER`
- 若是 GitHub 受管安装/更新，还需要允许网络下载

### 7.3 skill 调用前置要求

调用 `call_skill` 前应保证：

- 已成功 `load` 或 `reload`
- 目标工具名存在
- skill help / entry 注册表已经生成

## 8. 标准结构体说明

### 8.1 `FfiLuaVmPoolConfig`

作用：

- 描述 Lua VM 池的大小与回收策略

字段：

- `min_size`
- `max_size`
- `idle_ttl_secs`

### 8.2 `FfiToolCacheConfig`

作用：

- 描述共享工具缓存限制

字段：

- `max_entries`
- `default_ttl_secs`
- `max_ttl_secs`

### 8.2.1 `FfiBorrowedBuffer`

作用：

- 描述一段借用输入缓冲

字段：

- `ptr`
- `len`

约束：

- `ptr` 只在当前 FFI 调用期间有效
- `len > 0` 时，`ptr` 不得为 null

### 8.2.2 `FfiOwnedBuffer`

作用：

- 描述一段由 `luaskills` 拥有并负责释放的输出缓冲

字段：

- `ptr`
- `len`

约束：

- 该结构用于 `_json` 接口返回值、callback 返回值与后续扩展接口
- 该结构也已经用于标准结构体结果中的大量单值文本字段
- 释放必须走 `luaskills_ffi_buffer_free`
- 如果 `len > 0`，则 `ptr` 不得为 null

### 8.3 `FfiLuaRuntimeHostOptions` / V2 / V3

作用：

- `FfiLuaRuntimeHostOptions` 描述旧版宿主运行时路径、依赖目录名、下载策略、基础库路径等，并保持 v1 标准 C ABI 布局兼容
- `FfiLuaRuntimeHostOptionsV2` 在 `base` 中嵌入 `FfiLuaRuntimeHostOptions`，并新增 `runtime_root` 作为新宿主集成的规范目录入口
- `FfiLuaRuntimeHostOptionsV3` 在 `base` 中完整嵌入 V2，并新增 `managed_runtime_distribution_root`、`managed_runtime_environment_root` 与可选 `managed_runtime_config` 指针；V1/V2 尺寸完全不变
- `FfiLuaRuntimeManagedRuntimeConfig` 固定 Worker 池容量/空闲回收、单引擎持久会话上限、默认每流输出缓冲与默认 invoke 超时；空指针保留全部稳定默认值

关键字段：

- 路径字段
- `allow_network_download`
- GitHub base URL
- SQLite / LanceDB 动态库路径
- `sqlite_provider_mode`
- `sqlite_callback_mode`
- `lancedb_provider_mode`
- `lancedb_callback_mode`
- `runlua_pool_config`
- `reserved_entry_names`
- `ignored_skill_ids`
- `skill_config_root`
- `skill_config_lock_timeout_ms`
- `skill_config_watch_debounce_ms`
- `enable_skill_management_bridge`
- `default_text_encoding`
- `disable_managed_io_compat`
- `FfiLuaRuntimeHostOptionsV2.runtime_root`
- `FfiLuaRuntimeHostOptionsV3.managed_runtime_distribution_root`
- `FfiLuaRuntimeHostOptionsV3.managed_runtime_environment_root`
- `FfiLuaRuntimeHostOptionsV3.managed_runtime_config`

受管运行时根规则：

- 发行根直接包含 `python/` 与 `node/`，必须是已经存在的绝对目录
- 环境根直接创建 `python/py-<version>/<env_hash>` 与 `node/node-<version>/<env_hash>`，必须是绝对路径并由 LuaSkills 安全创建
- 显式 V3/JSON 字段优先于 `<runtime_root>/dependencies/runtimes` 与 `<runtime_root>/dependencies/envs`
- 不读取环境变量、系统 `PATH`、系统 Python、系统 Node 或外部 `node_modules` 作为降级候选
- JSON FFI 可调用 `luaskills_ffi_managed_runtime_resolve_json` 只读解析同一发行包；返回已解析安装根、可执行文件及两项 SHA-256，不创建 engine 或环境。Windows 绝对盘符/UNC 输入可带 `\\?\` / `\\?\UNC\`，但两个返回路径始终使用去除该前缀后的宿主可见形式；其他 verbatim 命名空间会在寻址前被拒绝，Rust 原生描述符仍保留规范 `PathBuf` 身份
- JSON FFI 可在 `host_options.managed_runtime_config` 配置 `worker_pool_max_size_per_environment=4`、`worker_idle_ttl_secs=60`、`persistent_session_limit_per_engine=256`、`persistent_session_default_buffer_limit_bytes_per_stream=1048576` 与可空 `invoke_default_timeout_ms`；这些是稳定默认值，所有已配置数值必须大于零
- 单次正数 `invoke.timeout_ms` 优先于引擎默认超时，单个 Session 的正数 `session.open.buffer_limit_bytes` 优先于引擎默认输出缓冲；Lua 不能修改其余引擎级策略
- 标准 C ABI 的 `has_invoke_default_timeout_ms` 必须严格为 `0` 或 `1`；为 `0` 时数值成员被忽略
- 完整示例见[宿主指定受管运行时根目录](../managed-runtime-host-roots.md)

已取消字段：

- `protected_skill_ids` 已移除。三层模型下默认保护边界是 `ROOT` 层归属，不再通过 skill id 清单声明保护。

数据库后端模式规则如下：

- `dynamic_library`
  - lib 自己加载数据库动态库
- `host_callback`
  - lib 把数据库请求回调给宿主
- `space_controller`
  - lib 把数据库请求转发给外部空间控制器

当 provider mode 为 `host_callback` 时，还必须继续指定 callback mode：

- `standard`
- `json`

也就是说：

- provider mode 决定数据库请求走哪一类后端
- callback mode 只在 `host_callback` 模式下决定“用标准回调还是 JSON 回调”
- SQLite 与 LanceDB 可以分别设置成不同组合

说明：

- `enable_skill_management_bridge = false`
  - Lua 侧仍可看到 `vulcan.runtime.skills` 命名空间，但安装、更新、启停、卸载桥接会被宿主策略直接拒绝
- `enable_skill_management_bridge = true`
  - 只表示宿主允许 Lua 使用这组桥接能力
  - 真正执行仍依赖宿主已注册运行时技能管理回调
- 如果宿主打开了开关但没有注册回调，Lua 会得到明确错误：
  - `Runtime skill management bridge is enabled but no host callback is registered`
- `default_text_encoding`
  - 可填 `utf-8`、`system`、`oem`、`gbk`、`gb18030`、`latin1`、`base64`
  - 影响 `vulcan.io`、托管 `io` 兼容层、`vulcan.process.exec` 与 `vulcan.process.session` 的省略编码路径
  - 未配置时 Windows 默认 `system`，其他平台默认 `utf-8`
- `disable_managed_io_compat = 0`
  - `luaexec` 与持久运行时租约会把全局 `io` 替换为 Rust 托管兼容层
- `disable_managed_io_compat != 0`
  - 保留 Lua 原生全局 `io`；`vulcan.io` 仍然注册，AI 代码可以显式调用

宿主强制忽略规则：

- `ignored_skill_ids` 匹配 skill 目录派生出的 `skill_id`
- 命中后该 skill 会在加载早期被跳过
- 被跳过的 skill 不会触发依赖准备、SQLite/LanceDB 绑定或 entry 注册
- 该字段适合宿主已经用原生、gRPC、VMM 或其他实现替代某个默认 skill 包时使用
- 这不是 skill 自声明的 capability 判定，也不会自动推断宿主已有能力

技能配置路径规则：

- `skill_config_root` 必须是绝对用户级目录；未配置时配置 API 明确返回 `CONFIG_PATH_UNAVAILABLE`。
- 普通有效包写入 `<skill_config_root>/skills/config.json`。
- `ROOT` 系统包写入 `<skill_config_root>/system-skills/config.json`。
- `skill_config_lock_timeout_ms` 可选范围为 1–60000，默认 5000。
- `skill_config_watch_debounce_ms` 可选范围为 1–5000，默认 200。
- 路径不从 `runtime_root`、skill roots 或当前目录推导。
- 配置按包级 `skill_id` 分组，不属于独立 entry。

隔离 `runlua` 专用池规则：

- `runlua_pool_config` 只作用于 `vulcan.runtime.lua.exec` 这条隔离执行链
- 未配置时默认使用：
  - `min_size = 1`
  - `max_size = 4`
- `idle_ttl_secs = 60`
- 该配置不会改变普通 skill VM 池，也不会改变标准 `call_skill / run_lua` 主池配置
- 当宿主预期会高频调用 `vulcan.runtime.lua.exec` 时，建议显式配置该字段，而不是误调普通 `pool`
- 当前 `vulcan.runtime.lua.exec` 统一在当前进程内执行，不再支持单独配置外部 `luaexec` 执行器路径

宿主配置管理建议：

- 当前库已经提供 Rust API、标准 C ABI 与公共 `_json` FFI 的跨 skill 配置管理入口
- 更推荐宿主把它们包装成一个统一工具，而不是暴露多个分散接口
- 推荐工具签名：

```text
runtime-config(action, skill_id?, key?, value?, values?, expected_revision?, mode?, root_name?, include_values?, store_scope?)
```

推荐动作：

- `describe`
- `validate`
- `list`
- `get`
- `set`
- `delete`
- `refresh`

也就是说：

- Lua / skill 作者使用 `vulcan.config.*`
- 宿主与 AI 使用一个统一的 `runtime-config` 工具

### 8.4 `FfiLuaEngineOptions`

作用：

- 引擎创建的总配置

组成：

- `pool`
- `host`

### 8.5 `FfiRuntimeSkillRoot`

作用：

- 描述一个命名 skill 根

字段：

- `name`
- `skills_dir`

### 8.6 `FfiLuaInvocationContext`

作用：

- 描述一次调用时的宿主附加上下文

字段：

- `request_context_json`
- `client_budget_json`
- `tool_config_json`

说明：

这些字段在标准 C ABI 接口里仍承载 JSON 内容，但当前已经统一改成：

- `FfiBorrowedBuffer`

也就是说：

- 它们仍然是动态 JSON 结构
- 但不再依赖 NUL 终止字符串
- 宿主应显式提供 `ptr + len`

原因是这些值本身是动态结构，固定 ABI 代价高且易碎。

### 8.7 `FfiSkillInstallRequest`

作用：

- 描述一次受管安装或更新请求

字段：

- `skill_id`
- `source`
- `source_type`

说明：

当前受管安装主链重点支持 GitHub。

`source_type` 在标准 C ABI 中采用稳定整数协议：

- `-1`
  - absent
  - 仅用于结果里表示来源不存在
- `0`
  - github
  - 表示 GitHub 仓库来源
- `1`
  - url
  - 表示 URL metadata 来源

头文件中对应常量为：

- `FFI_SOURCE_TYPE_ABSENT`
- `FFI_SOURCE_TYPE_GITHUB`
- `FFI_SOURCE_TYPE_URL`

### 8.8 `FfiSkillUninstallOptions`

作用：

- 描述一次卸载时是否删除数据库

字段：

- `remove_sqlite`
- `remove_lancedb`

## 9. 接口目录总览

### 9.1 基础接口

标准 C ABI 接口：

- `luaskills_ffi_version`
- `luaskills_ffi_describe`
- `luaskills_ffi_engine_new`
- `luaskills_ffi_engine_new_v2`
- `luaskills_ffi_engine_free`

公共 `_json` FFI 接口：

- `luaskills_ffi_version_json`
- `luaskills_ffi_describe_json`
- `luaskills_ffi_engine_new_json`
- `luaskills_ffi_engine_free_json`

说明：

- `describe_json` 会返回当前动态库实际导出的 `_json` 入口列表，SDK 或宿主 wrapper 应优先用它做能力探测，而不是只按文档假定某个入口一定存在。
- 当前部署约束只支持最新正式协议；`system_runtime_lease_*_json`、`load_from_roots`、`reload_from_roots` 与正式 lifecycle 入口都应被视为必需导出。

### 9.2 加载与重载接口

标准 C ABI 接口：

- `luaskills_ffi_load_from_roots`
- `luaskills_ffi_reload_from_roots`

公共 `_json` FFI 接口：

- `luaskills_ffi_load_from_roots_json`
- `luaskills_ffi_reload_from_roots_json`

### 9.3 描述与帮助接口

标准 C ABI 接口：

- `luaskills_ffi_list_entries`
- `luaskills_ffi_list_skill_help`
- `luaskills_ffi_render_skill_help_detail`
- `luaskills_ffi_prompt_argument_completions`
- `luaskills_ffi_is_skill`
- `luaskills_ffi_skill_name_for_tool`

公共 `_json` FFI 接口：

- `luaskills_ffi_list_entries_json`
- `luaskills_ffi_list_skill_help_json`
- `luaskills_ffi_render_skill_help_detail_json`
- `luaskills_ffi_prompt_argument_completions_json`
- `luaskills_ffi_is_skill_json`
- `luaskills_ffi_skill_name_for_tool_json`

说明：

- `list_entries / list_skill_help / render_skill_help_detail / prompt_argument_completions / is_skill / skill_name_for_tool` 都属于可见性查询，必须由宿主注入 authority。
- `System` 可看到 `ROOT / PROJECT / USER` 的运行时描述。
- `DelegatedTool` 只能看到 `PROJECT / USER`，不会返回 `ROOT` entries、help detail 或已知 ROOT tool name 的归属信息。

### 9.4 调用接口

标准 C ABI 接口：

- `luaskills_ffi_call_skill`
- `luaskills_ffi_run_lua`
- `luaskills_ffi_runtime_lease_create / eval / status / list / close`
- `luaskills_ffi_system_runtime_lease_create / eval / status / list / close`
- `luaskills_ffi_managed_session_events_poll`
- `luaskills_ffi_managed_session_events_wait`
- `luaskills_ffi_set_managed_session_wake_callback`

公共 `_json` FFI 接口：

- `luaskills_ffi_call_skill_json`
- `luaskills_ffi_run_lua_json`
- `luaskills_ffi_runtime_lease_create_json`
- `luaskills_ffi_runtime_lease_eval_json`
- `luaskills_ffi_runtime_lease_status_json`
- `luaskills_ffi_runtime_lease_list_json`
- `luaskills_ffi_runtime_lease_close_json`
- `luaskills_ffi_system_runtime_lease_create_json`
- `luaskills_ffi_system_runtime_lease_eval_json`
- `luaskills_ffi_system_runtime_lease_status_json`
- `luaskills_ffi_system_runtime_lease_list_json`
- `luaskills_ffi_system_runtime_lease_close_json`
- `luaskills_ffi_managed_session_events_poll_json`
- `luaskills_ffi_managed_session_events_wait_json`

说明：

- 普通 `call_skill / run_lua / runtime_lease_*_json` 不接收 authority，也不作为 root 可见性或技能管理权限边界。
- `call_skill` 调用当前已激活运行时视图中的 skill，包括已加载的 `ROOT / PROJECT / USER` skill。
- `run_lua` 执行当前运行时环境中的 Lua 代码；如果宿主不希望普通用户执行任意 Lua，应在工具封装层单独限制或不暴露该接口。
- `runtime_lease_*_json` 与 `system_runtime_lease_*_json` 都是持久运行时租约接口，同一 `lease_id` 的多次 `eval` 会复用同一个 Lua VM 并保留全局状态；宿主应使用稳定的 `sid` 绑定对话、任务或工作区，并在不需要时显式 close。
- `system_runtime_lease_*_json` 必须由宿主注入 `authority`；当前接受 `"system"` 与 `"delegated_tool"` 两种值，推荐对外 wrapper 固定注入 `"delegated_tool"`，把公共 `runtime_lease_*_json` 留给可信宿主内部链路。
- 公共租约 `create` 支持 `cwd`、`workspace_root`、`lua_roots`、`c_roots`、`mounts`。System `create` 是另一套严格请求：只接受 `cwd`、`workspace_root`、`mounts` 与必填 `system_package { id, root, dependencies_file }`，传入 `lua_roots / c_roots` 或其他未知字段会直接失败；`workspace_root` 必须是既有绝对目录，`cwd` 必须规范化到包根或该授权工作区内，未传时默认使用 `system_package.root`。
- System VM 不加载普通 Skill/entry 注册表，只把当前包根加入 Lua 模块路径，并在每次 eval 前重新验证该根。`vulcan.runtime.system_plugin` 与 `vulcan.runtime.mounts` 是支持索引、`pairs/ipairs`、长度和 JSON 返回序列化的递归只读 userdata 视图，三个宿主所有根字段都不能替换；专用 System VM 会移除全局 `rawset` 与 Lua `debug` 库，阻止绕过元表边界。`workspace_root` 是宿主明确授权的规范路径或 `nil`。
- `eval / status / close` 请求可选回传 `sid` 与 `generation`；推荐宿主 wrapper 始终把 `create` 返回的两项一并带回，提前发现串线或陈旧句柄。
- 推荐宿主把 `lease_id + sid + generation` 物化成一个本地 session handle 对象，而不是在业务代码中散落传递三个字段。
- `runtime_lease_list_json` 仅列出当前仍然活跃的租约；已 `close`、已过期或已被同 SID 新租约替换的旧 `lease_id` 不会出现在列表中。
- 旧 `lease_id` 在被关闭、过期或替换后会返回稳定错误码，例如 `lease_closed`、`lease_expired`、`lease_replaced`，便于宿主或 AI wrapper 做恢复与重建。
- 如果 wrapper 回传的 `sid` 或 `generation` 与当前租约不一致，会返回 `lease_sid_mismatch` 或 `lease_generation_mismatch`。
- `DelegatedTool` 对 ROOT 的隐藏只适用于管理、查询与 prompt completion 面，不限制运行时 skill 调用。

持久运行时最小调用示例：

```json
{
  "engine_id": 1,
  "sid": "conversation-123",
  "ttl_sec": 600,
  "replace": false
}
```

System Plugin 创建示例：

```json
{
  "engine_id": 1,
  "sid": "host-indexer/session-123",
  "ttl_sec": 0,
  "replace": false,
  "cwd": "D:/runtime/system_lua_lib/host-indexer",
  "workspace_root": "D:/projects/current",
  "mounts": { "project": { "root": "D:/projects/current" } },
  "system_package": {
    "id": "host-indexer",
    "root": "D:/runtime/system_lua_lib/host-indexer",
    "dependencies_file": "dependencies.yaml"
  },
  "authority": "system"
}
```

上例是公共 `_json` 导出的完整外层请求。标准 C ABI
`luaskills_ffi_system_runtime_lease_create(engine_id, request_json, ...)` 的 `request_json`
只接收严格 System 创建正文：`engine_id` 已由第一个 C 参数传入，标准 ABI 也不在 JSON
正文中接收 `authority`。对应载荷应写为：

```json
{
  "sid": "host-indexer/session-123",
  "ttl_sec": 0,
  "replace": false,
  "cwd": "D:/runtime/system_lua_lib/host-indexer",
  "workspace_root": "D:/projects/current",
  "mounts": { "project": { "root": "D:/projects/current" } },
  "system_package": {
    "id": "host-indexer",
    "root": "D:/runtime/system_lua_lib/host-indexer",
    "dependencies_file": "dependencies.yaml"
  }
}
```

在 Windows x86_64、Linux x86_64/aarch64、macOS x86_64/aarch64 上，同一 System `lease_id` 的第一次 eval 可以把 `vulcan.runtime.python.session.open(...)` 或 `vulcan.runtime.node.session.open(...)` 返回的 userdata 保存到 Lua 全局变量，后续 eval 继续 `write/read/status/close/kill`。Windows ARM/aarch64 是唯一明确不支持目标，会在环境、快照或进程预留前通过 `persistent_session.supported=false` 与稳定原因 `windows_arm_is_not_supported` 拒绝，且不会回退系统解释器。每个受管运行时 `status()` 都包含 `persistent_session.target_os` 与 `target_arch`。只有这两种受管 session 的 userdata `status()` 返回 `managed_session_id`；普通 `vulcan.process.session` 不返回该字段。该 id 与宿主事件的 `managed_session_id` 精确对应。

池化 Worker 的源码路径不依赖宿主 cwd：Unix 会通过固定快照目录描述符逐组件执行不跟随链接的 `openat` 校验，再以该固定快照作为 cwd 执行相对入口；Windows 使用由共享锁保护的绝对快照源码，并把短驱动器根或 UNC 共享根设为中立 cwd。Python Worker 还会把唯一 `snapshot_root` 固定到 `sys.path` 首位，并拒绝同一 Worker 内更换该根。

事件 wait 请求：

```json
{
  "engine_id": 1,
  "max_events": 64,
  "timeout_ms": 1000,
  "authority": "delegated_tool"
}
```

调用 `luaskills_ffi_managed_session_events_wait_json` 后，`result` 固定为：

```json
{
  "events": [
    {
      "system_lease_id": "rt_...",
      "sid": "host-indexer/session-123",
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

`poll` 与 `wait` 都是破坏性、有界读取，`max_events` 必须为正数；`events` 按全局单调 `sequence` 排序，`remaining` 是排空后仍待处理的逻辑事件数。只有 `wait` 在截止时间前没有事件时返回 `timed_out=true`；普通空 `poll` 返回 `false`。同一 session 的同类 readiness 事件会合并，但 stdout/stderr 数据继续保存在有界 session 缓冲中，宿主应在后续受控 eval 内调用对应 userdata 的 `read(...)`。

标准 ABI wake callback 只负责告诉宿主“事件队列已经从空变为非空”，不会替代 poll/wait，也不携带事件正文。callback 由每个引擎的单个串行后台调度线程执行，禁止调用 Lua、同步进入租约 eval 或跨 ABI 抛异常；返回非零会在同一边沿仍待处理时通过封顶指数退避自动重试，且不会阻塞事件发布线程。正确流程是 callback 投递宿主任务，任务再 poll/wait，并按事件里的 `system_lease_id + sid + generation` 调用受控 eval。替换/清除 callback 会取消待处理重试并等待在途调用，成功返回后旧 `user_data` 才可释放。

```c
// Wake the host dispatcher without entering Lua from this background callback.
// 从该后台回调唤醒宿主调度器，不在这里进入 Lua。
static int managed_session_wake(
    uint64_t engine_id,
    void *user_data,
    FfiOwnedBuffer *error_out
) {
    // This example cannot fail, so the diagnostic slot remains empty.
    // 此示例不会失败，因此诊断输出槽保持为空。
    (void)error_out;
    host_enqueue_managed_session_drain(user_data, engine_id);
    return 0;
}

// LuaSkills-owned diagnostic returned only when registration fails.
// 仅在注册失败时返回的 LuaSkills 所有诊断缓冲。
FfiOwnedBuffer callback_error = {0};
int callback_status = luaskills_ffi_set_managed_session_wake_callback(
    engine_id,
    managed_session_wake,
    host_dispatcher,
    &callback_error
);
if (callback_status != 0) {
    // Copy the diagnostic before releasing the LuaSkills-owned buffer.
    // 先复制诊断文本，再释放 LuaSkills 所有缓冲。
    host_log_owned_buffer(callback_error);
    luaskills_ffi_buffer_free(callback_error);
}
```

宿主队列任务随后调用 `luaskills_ffi_managed_session_events_poll` 或 `wait`；标准 ABI 在 `result_json_out` 中直接返回批次 JSON，而 `_json` 入口在统一包络的 `result` 中返回同一批次。关闭引擎或释放 `host_dispatcher` 前，先用 `callback = NULL` 清除注册，并在函数返回后再释放 `user_data`。

生命周期清理是强制的：eval 出错会回滚并终止本次 eval 新开的受管 session；租约 close、同 SID replace、过期、VM drop 与 engine free 会终止仍存活的完整子进程树并退役包 worker；Python/Node 每会话不可变快照在进程清理完成后删除，并在此之前持续持有共享环境生命周期租约。环境发布或替换需要非阻塞独占租约，仍有活动使用方时会返回 busy，而不会与存活进程竞态。

后续执行：

```json
{
  "engine_id": 1,
  "lease_id": "rt_...",
  "sid": "conversation-123",
  "generation": 1,
  "code": "counter = (counter or 0) + 1; return counter",
  "args": {},
  "timeout_ms": 60000
}
```

列出当前活跃租约：

```json
{
  "engine_id": 1,
  "sid": "conversation-123"
}
```

`call_skill` 的结构化宿主结果：

- `host_result` 默认关闭，只有宿主通过 `request_context.client_capabilities.host_result.enabled = true` 显式开启时才允许 skill 返回第四返回值。
- 支持结构化结果的 skill 应返回 `content, overflow_mode, template_hint, host_result`，其中 `host_result = { kind = "...", payload = { ... } }`。
- 当前推荐的标准结果种类是 `change_set`，用于把 IDE 每轮操作级结果独立返回给宿主，而不是依赖全局 `git diff` 兜底。
- `change_set.payload.files` 现在应始终存在；`modify` 文件记录应携带非空 `hunks`，每个 hunk 使用 `before + delete[] + insert[] + after` 表达具体“从什么改成了什么”。
- `before` 与 `after` 应表示紧贴修改块前后的连续上下文字符串，而不是整文件快照。
- `delete[].line` 使用旧文件行号，`insert[].line` 使用插入后的新文件行号；二者都应按升序排列，且行内容通过 `content` 明确给出。
- `create` / `delete` 文件记录应直接提供完整文件 `content`；`rename` 文件记录应提供 `old_path` 与 `new_path`。

### 9.5 生命周期接口

标准 C ABI 接口：

- `luaskills_ffi_disable_skill`
- `luaskills_ffi_system_disable_skill`
- `luaskills_ffi_enable_skill`
- `luaskills_ffi_system_enable_skill`
- `luaskills_ffi_uninstall_skill`
- `luaskills_ffi_system_uninstall_skill`
- `luaskills_ffi_install_skill`
- `luaskills_ffi_system_install_skill`
- `luaskills_ffi_update_skill`
- `luaskills_ffi_system_update_skill`

system 版本的标准 C ABI 生命周期接口，以及可见性查询、prompt completion 接口必须传入 `authority` 整数：

- `0` 表示 `System`，允许管理 `ROOT`
- `1` 表示 `DelegatedTool`，用于 system 工具、查询或补全被封装后开放给普通 tools 的场景，不允许写入或查询 `ROOT`

公共 `_json` FFI 接口：

- `luaskills_ffi_disable_skill_json`
- `luaskills_ffi_system_disable_skill_json`
- `luaskills_ffi_enable_skill_json`
- `luaskills_ffi_system_enable_skill_json`
- `luaskills_ffi_uninstall_skill_json`
- `luaskills_ffi_system_uninstall_skill_json`
- `luaskills_ffi_install_skill_json`
- `luaskills_ffi_system_install_skill_json`
- `luaskills_ffi_update_skill_json`
- `luaskills_ffi_system_update_skill_json`

system 版本的 `_json` 生命周期接口，以及可见性查询、prompt completion 接口必须在 JSON 请求中由宿主注入 `authority`：

```json
{ "authority": "system" }
```

或：

```json
{ "authority": "delegated_tool" }
```

公共 `_json` FFI 的 `install / update / uninstall` 请求支持可选 `target_root` 字段，用于封装显式 root 生命周期 API：

```json
{
  "target_root": {
    "name": "USER",
    "skills_dir": "D:/runtime/luaskills/user_skills"
  }
}
```

该字段不改变权限语义：普通 `Skills` plane 只能写 `PROJECT / USER`；system 入口只有在 `authority = system` 时可以写 `ROOT`；ROOT 同名 skill 仍具有全局系统占用语义。

### 9.6 Skill 配置接口

标准 C ABI 接口：

- `luaskills_ffi_skill_config_list`
- `luaskills_ffi_skill_config_describe`
- `luaskills_ffi_skill_config_validate`
- `luaskills_ffi_skill_config_get`
- `luaskills_ffi_skill_config_set_values`
- `luaskills_ffi_skill_config_delete`
- `luaskills_ffi_skill_config_refresh`
- `luaskills_ffi_skill_config_events_poll`

公共 `_json` FFI 接口：

- `luaskills_ffi_skill_config_list_json`
- `luaskills_ffi_skill_config_describe_json`
- `luaskills_ffi_skill_config_validate_json`
- `luaskills_ffi_skill_config_get_json`
- `luaskills_ffi_skill_config_set_json`
- `luaskills_ffi_skill_config_delete_json`
- `luaskills_ffi_skill_config_refresh_json`
- `luaskills_ffi_skill_config_events_poll_json`

说明：

- skill config 按技能包 `skill_id` 管理，不属于某个独立 entry；有效包所属根决定写入普通或系统用户级存储。
- 当前有效技能包必须在 `skill.yaml` 顶层声明配置项；宿主 `set` 不能写入未声明 key，也不能绕过类型和约束校验。
- `describe` 返回参数名、类型、约束、枚举说明、UI 元数据、技能包作者提供的文本和状态。JSON FFI 支持 `effective` 与不执行 Lua 的 `installed` 模式。
- 标准 C ABI 的 `include_values` 只接受 `0` 或 `1`；JSON FFI 中为布尔值且默认 `false`。
- `include_values=false` 时响应完全省略每项的 `value`；为 `true` 时返回未遮罩原始有效值。
- `validate` 是只读操作，返回 revision、store scope、`complete`、`missing`、`invalid`、业务问题和 orphaned 键。
- `set` 只接受一个非空批次并支持可选 revision CAS；标准 C ABI 名称为 `set_values`。`delete` 同样支持 CAS。
- `refresh` 显式刷新一个或两个缓存快照；events poll 返回本地写入、外部重载和结构化失败事件。
- 原始 `list/get/delete` 可以管理技能包升级后留下的未声明值；`set` 始终要求当前声明。
- LuaSkills 不对配置值执行用户授权，也不因 `sensitive=true` 自动遮罩。上级封装必须决定是否允许 `get/list`、是否允许 `include_values=true`、是否直接修改或先请求用户授权。
- 完整请求响应与权限建议见 [Skill 包级配置声明、运行时控制与宿主对接](../architecture/skill-config-system-design.md)。

### 9.7 宿主工具桥接 callback

Rust API：

- `set_host_tool_callback`

标准 C ABI 注册接口：

- `luaskills_ffi_set_host_tool_json_callback`

说明：

- 该 callback 供 Lua 侧 `vulcan.host.list()`、`vulcan.host.has()`、`vulcan.host.has_tool()`、`vulcan.host.call()` 使用。
- 标准 C ABI 当前只提供 JSON callback 入口，因为宿主工具参数和返回值天然是动态 table / JSON 结构。
- Lua 侧使用 table；FFI 边界内部使用 JSON 请求和 JSON 响应。
- 该桥接不支持 stream，宿主 callback 必须返回完整 JSON 结果。
- 权限、超时、审计、secret 管理、模型 provider 策略和工具 allowlist 均由宿主负责。

### 9.8 模型能力 callback

Rust API：

- `set_model_embed_callback`
- `set_model_llm_callback`

标准 C ABI 注册接口：

- `luaskills_ffi_set_model_embed_json_callback`
- `luaskills_ffi_set_model_llm_json_callback`

说明：

- 该 callback 供 Lua 侧 `vulcan.models.embed(text)` 与 `vulcan.models.llm(system, user)` 使用。
- Lua 侧只能传固定参数，不能传模型名、temperature、max_tokens、base_url、api_key、thinking、stream 或 provider-specific 参数。
- 宿主按能力分别注册 callback，未注册就表示该能力未开启。
- callback 请求会携带 caller context，包含 skill、entry、root、skill_dir、client_name、request_id 等可用字段。
- callback 成功时返回模型能力的裸响应载荷，例如 embedding 的 `vector/dimensions/usage` 或 LLM 的 `assistant/usage`。
- callback 失败时可返回 `{ "ok": false, "error": { "code": "...", "message": "...", "provider_message": "...", "provider_code": "...", "provider_status": 400 } }`，LuaSkills 会保留 provider 原始错误字段。
- 模型 provider 配置完全归宿主管理，不走 `skill_config`，Lua 侧不能读取、设置或覆盖。

推荐接入顺序：

1. 宿主先加载自己的模型配置，例如 `model_config.yaml`、环境变量、产品设置或工作区策略。
2. 宿主按能力分别注册 callback，只注册真实开放的能力。
3. 宿主创建 engine，并完成 `load_from_roots`。
4. Lua skill 通过 `vulcan.models.status()` 或 `vulcan.models.has(capability)` 判断能力是否可用。
5. 宿主关闭时清理进程级 callback，避免测试或长进程复用时留下旧闭包。

能力状态规则：

- 未注册 embed callback 时：
  - `vulcan.models.has("embed") == false`
  - `vulcan.models.status().capabilities.embed == false`
  - `vulcan.models.embed("x")` 返回 `model_unavailable`
- 注册 embed callback 后：
  - `vulcan.models.has("embed") == true`
  - `vulcan.models.status().capabilities.embed == true`
- LLM 能力同理。
- `status()` 本身不需要宿主注册 callback；它始终由 LuaSkills 根据当前 callback 注册状态生成。

embedding JSON callback 请求：

```json
{
  "text": "hello",
  "caller": {
    "skill_id": "demo-skill",
    "entry_name": "embed",
    "canonical_tool_name": "demo-skill-embed",
    "root_name": "USER",
    "skill_dir": "D:/runtime/luaskills/user_skills/demo-skill",
    "client_name": "mcp-host",
    "request_id": "req-001"
  }
}
```

LLM JSON callback 请求：

```json
{
  "system": "You are concise.",
  "user": "Summarize this note.",
  "caller": {
    "skill_id": "demo-skill",
    "entry_name": "ask",
    "canonical_tool_name": "demo-skill-ask",
    "root_name": "USER",
    "skill_dir": "D:/runtime/luaskills/user_skills/demo-skill",
    "client_name": "mcp-host",
    "request_id": "req-002"
  }
}
```

embedding 成功返回推荐使用裸载荷：

```json
{
  "vector": [0.1, 0.2, 0.3],
  "dimensions": 3,
  "usage": {
    "input_tokens": 12
  }
}
```

LLM 成功返回推荐使用裸载荷：

```json
{
  "assistant": "summary text",
  "usage": {
    "input_tokens": 12,
    "output_tokens": 8
  }
}
```

provider 失败时推荐返回结构化错误包络：

```json
{
  "ok": false,
  "error": {
    "code": "provider_error",
    "message": "模型服务拒绝了本次请求",
    "provider_message": "raw provider message after host-side redaction",
    "provider_code": "model_not_found",
    "provider_status": 404
  }
}
```

错误码建议：

- `model_unavailable`：能力 callback 未注册。
- `invalid_argument`：Lua 侧参数数量、类型或空字符串不合法。
- `provider_error`：模型 provider 返回错误。
- `timeout`：宿主模型调用超时。
- `budget_exceeded`：宿主预算、限流或策略拒绝本次调用。
- `internal_error`：LuaSkills 或宿主桥接内部错误。

宿主返回 provider 错误时应遵守两条规则：

- 尽量保留 provider 的原始错误文本、原始错误码和 HTTP 状态码，便于排查密钥、额度、限流和模型不存在等问题。
- 先在宿主侧完成脱敏，不要把 API key、Authorization header、签名、临时凭证或完整请求头传给 LuaSkills。

SDK 对接入口：

| SDK | 注册入口 | 清理入口 | 备注 |
| --- | --- | --- | --- |
| TypeScript / Node.js | `setModelEmbedJsonCallback`、`setModelLlmJsonCallback` | `clearModelEmbedJsonCallback`、`clearModelLlmJsonCallback` | 由 `LuaSkillsJsonFfi` 持有 callback 注册。 |
| Python | `set_model_embed_json_callback`、`set_model_llm_json_callback` | `clear_model_embed_json_callback`、`clear_model_llm_json_callback` | 由 `LuaSkillsJsonFfi` 持有 callback 注册。 |
| Go | `SetModelEmbedJSONCallback`、`SetModelLLMJSONCallback` | `ClearModelEmbedJSONCallback`、`ClearModelLLMJSONCallback` | 当前只暴露类型化边界，真实进程级注册需要宿主 cgo callback bridge。 |

排查建议：

- Lua 返回 `model_unavailable`：确认 callback 是否注册，且注册发生在创建或调用 engine 之前。
- Lua 返回 `invalid_argument`：确认 skill 没有传空字符串、table、数组、第三参数或 provider-specific 参数。
- Lua 返回 `provider_error` 但缺少 provider 字段：确认宿主 callback 返回的是结构化错误包络，而不是只通过 FFI `error_out` 返回纯文本。
- `caller` 字段为空：确认 skill 是通过正式 `call_skill` 或已加载 runtime 调用，而不是脱离 runtime 上下文的测试路径。
- TypeScript / Python 找不到 model callback symbol：确认动态库版本包含 `luaskills_ffi_set_model_embed_json_callback` 与 `luaskills_ffi_set_model_llm_json_callback`。

## 10. 每类接口的调用逻辑

### 10.1 `version` / `describe`

作用：

- 查询 FFI 版本
- 查询当前导出的入口名字

启动条件：

- 无需创建引擎

调用方式：

- 直接调用即可

调用逻辑：

- 不访问 skill roots
- 不访问运行时注册表
- 不需要任何宿主状态

### 10.2 `engine_new`

作用：

- 创建引擎与内部句柄

参数：

- `LuaEngineOptions`

返回：

- `engine_id`

调用逻辑：

1. 校验配置
2. 构建 `LuaEngine`
3. 放入内部全局注册表
4. 返回稳定句柄

### 10.3 `engine_free`

作用：

- 释放一个引擎句柄

调用逻辑：

1. 从注册表删除句柄
2. 丢弃引擎实例

### 10.4 `load_from_roots`

作用：

- 扫描技能目录
- 构建 entry 注册表
- 构建 help 树
- 解析 skill manifest
- 注入 provider 绑定

推荐：

- `roots` 是当前唯一正式模型
- `roots` 必须至少包含 `ROOT`；`ROOT` 可以暂时为空，但不能缺失

### 10.5 `reload_*`

作用：

- 重新扫描技能根并重建运行时视图
- 丢弃候选引擎的旧 entry、help、provider 与配置声明注册表
- 基于同一组成功加载的有效技能包重建全部注册表
- 候选构建失败时保留当前引擎及其配置声明注册表

### 10.6 `skill_config_*`

作用：

- 发现和校验每个有效技能包在 `skill.yaml` 顶层声明的配置结构
- 统一管理按 `skill_id + values` 批量持久化的规范字符串值

调用逻辑：

1. 验证宿主显式绝对 `skill_config_root` 与锁/监听时间参数。
2. 建立普通 `skills/config.json` 与系统 `system-skills/config.json` 双存储路由。
3. 从最后合法不可变快照读取；文件监听或显式 `refresh` 接受 revision 单调增加的外部替换。
4. `describe/validate` 提供声明、UI 元数据、类型、说明、约束、revision、状态和业务问题。
5. `set_values` 在跨进程锁内重新读取最新磁盘版本，对整批执行声明、类型和隔离业务校验，然后一次增加 revision。
6. `delete` 支持 revision CAS 并允许清理 orphaned；events poll 提供有界有序通知。

宿主包装建议：

- 推荐把这组底层 API 封装成单工具：
  - `runtime-config(action, skill_id?, key?, value?, values?, expected_revision?, mode?, root_name?, include_values?, store_scope?)`
- 推荐动作是 `describe/validate/list/get/set/delete/refresh`
- 不建议把这些动作暴露成多个独立终端工具，避免占用过多上下文
- 这组接口不是 ROOT 可见性查询接口；宿主必须按产品权限决定哪些 `skill_id` 可以读写
- LuaSkills 不执行用户授权或敏感值遮罩；宿主必须决定是否允许原始值查询、是否传 `include_values=true`、是否直接修改或先请求用户授权
- 面向模型的默认包装必须固定 `describe(mode=effective, include_values=false)`，不得让模型任意选择 `mode=installed` 或 `root_name`；installed 结果包含宿主诊断用物理路径，获授权后也应按产品需要裁剪 `absolute_path`

### 10.7 `list_entries`

作用：

- 按宿主注入 authority 列出当前运行时可见的工具入口描述

返回内容：

- canonical 名
- 所属 skill
- local name
- root name
- skill_dir
- description
- parameters

权限说明：

- 标准 C ABI 需要传入 `authority` 整数。
- `_json` 请求需要包含 `"authority": "system"` 或 `"authority": "delegated_tool"`。
- `DelegatedTool` 不返回 `ROOT` entries。

### 10.8 `list_skill_help`

作用：

- 按宿主注入 authority 列出每个可见 skill 的 help 树节点描述

返回内容：

- skill id
- skill 版本
- root 名
- skill 目录
- help 节点列表

权限说明：

- `DelegatedTool` 不返回 `ROOT` help 树。

### 10.9 `render_skill_help_detail`

作用：

- 按宿主注入 authority 渲染某个可见 skill 某个 help 流程节点的详情

参数：

- `skill_id`
- `flow_name`
- 可选请求上下文

说明：

- 标准 C ABI 接口中的请求上下文当前通过 `FfiBorrowedBuffer request_context_json` 传入
- 传空缓冲表示“不附带请求上下文”
- `DelegatedTool` 请求 `ROOT` help detail 时返回未找到或空结果，不暴露 ROOT 内容。

### 10.10 `prompt_argument_completions`

作用：

- 取 prompt 参数补全候选

调用条件：

- 目标 prompt 已存在
- 引擎已经 load

权限说明：

- 标准 C ABI 需要传入 `authority` 整数。
- `_json` 请求需要包含 `"authority": "system"` 或 `"authority": "delegated_tool"`。
- 当前接口仍返回空候选；后续若从 skill 元数据读取候选，必须保持同一 authority 可见性边界。

### 10.11 `is_skill`

作用：

- 按宿主注入 authority 判断一个 canonical tool name 是否属于可见 Lua skill

说明：

- `DelegatedTool` 对 ROOT tool name 返回 `false`。

### 10.12 `skill_name_for_tool`

作用：

- 按宿主注入 authority 解析一个 canonical tool name 可见的所属 skill id

说明：

- `DelegatedTool` 对 ROOT tool name 返回空归属。

### 10.13 `call_skill`

作用：

- 调用一个已加载的 skill entry

说明：

- `args_json` 当前通过 `FfiBorrowedBuffer` 传入
- `invocation_context` 中的三个 JSON 字段也都通过 `FfiBorrowedBuffer` 传入
- 这两部分仍承载 JSON 内容，但标准 ABI 不再要求宿主提供 NUL 终止字符串

参数：

- `tool_name`
- `args`
- 可选调用上下文

权限说明：

- 标准 C ABI 与 `_json` 请求都不接收 authority。
- 该接口调用当前已激活的运行时 skill；它不是 ROOT 可见性过滤接口，也不是技能管理权限判断接口。
- 如果宿主只想让普通用户调用经过筛选的工具集合，应在宿主工具路由层做白名单，而不是依赖 `call_skill` 的 authority。

调用逻辑：

1. 查 entry 注册表
2. 定位所属 skill
3. 构造 `vulcan.context.*`
4. 注入 `vulcan.deps.*`
5. 执行 Lua
6. 结构化返回结果

### 10.14 `vulcan.runtime.skills.*`

作用：

- 允许宿主把安装、更新、启停、卸载桥接为 Lua 可调用能力

正式桥接方法建议包含：

- `vulcan.runtime.skills.status()`
- `vulcan.runtime.skills.layers()`
- `vulcan.runtime.skills.install(input)`
- `vulcan.runtime.skills.update(input)`
- `vulcan.runtime.skills.uninstall(input)`
- `vulcan.runtime.skills.enable(input)`
- `vulcan.runtime.skills.disable(input)`

调用逻辑：

1. 先检查宿主能力开关是否允许
2. 再检查宿主是否注册技能管理回调
3. 将 Lua 输入转换为 JSON
4. 以固定 `DelegatedTool` authority 通过宿主回调转发结构化管理请求
5. 将宿主回调结果再转换回 Lua

设计意图：

- skill 不直接操控底层安装器
- 最终是否允许执行，由宿主策略决定
- 适合拥有自己 TUI、GUI 或专用管理界面的宿主
- 普通桥接只应操作 `PROJECT` / `USER` 层，不应暴露 `ROOT` 目标选项
- 普通桥接固定等价于 `DelegatedTool` 权限，宿主 callback 不应自行拼接 prepare / reload / commit，而应调用 LuaEngine 的普通显式 root API

`layers()` 用于发现宿主当前开放给普通桥接的可操作层级标签。推荐返回：

```json
{
  "default": "USER",
  "writable": true,
  "labels": ["PROJECT", "USER"],
  "layers": [
    {
      "label": "PROJECT",
      "writable": true,
      "description": "当前项目级 skill 层"
    },
    {
      "label": "USER",
      "writable": true,
      "description": "用户全局 skill 层"
    }
  ]
}
```

`ROOT` 不应出现在普通桥接的可操作层级列表中。`layers()` 基于当前已加载 root 链生成，只返回实际存在的 `PROJECT` / `USER`；若当前没有项目上下文，则不会返回 `PROJECT`；若只有 `ROOT`，则返回空 `labels` / `layers`、没有 `default`，且顶层 `writable=false`。当 bridge 关闭时，顶层 `writable` 与每个 layer 的 `writable` 都必须为 `false`。`layers()` 只是能力发现接口，install / update / uninstall 等请求仍必须由宿主回调做最终校验。

### 10.15 `vulcan.host.*`

作用：

- 允许宿主把固定工具注册为 Lua 可发现、可探测、可调用的能力
- 避免开放任意 `vulcan.xxx` 注入，把宿主扩展面收敛到 `list / has / call`

Lua 侧方法：

- `vulcan.host.list()`
- `vulcan.host.has(tool_name)`
- `vulcan.host.has_tool(tool_name)`
- `vulcan.host.call(tool_name, args_table)`

FFI 注册：

```text
luaskills_ffi_set_host_tool_json_callback(callback, user_data, error_out)
```

发送给 callback 的 JSON 请求结构：

```json
{
  "action": "call",
  "tool_name": "model.embed",
  "args": {
    "model": "text-embedding-3-small",
    "input": "hello"
  }
}
```

`action` 取值：

- `list`：`tool_name = null`，`args = {}`
- `has`：`tool_name` 为待探测工具名，`args = {}`
- `call`：`tool_name` 为待调用工具名，`args` 为 Lua table 转换出的 JSON

`list` 推荐返回工具元数据数组：

```json
[
  {
    "name": "model.embed",
    "description": "Create one embedding vector.",
    "input_schema": {
      "type": "object"
    }
  }
]
```

`has` 可以直接返回布尔值：

```json
true
```

`call` 推荐返回 table / object 包络：

```json
{
  "ok": true,
  "value": {
    "embedding": [0.1, 0.2, 0.3]
  },
  "meta": {
    "provider": "openai",
    "elapsed_ms": 120
  }
}
```

失败时推荐返回：

```json
{
  "ok": false,
  "error": {
    "code": "tool_not_found",
    "message": "host tool not found: model.embed"
  }
}
```

运行时默认行为：

- 未注册 callback 时，`list()` 返回空 table。
- 未注册 callback 时，`has()` / `has_tool()` 返回 `false`。
- 未注册 callback 或 callback 返回 FFI 错误时，`call()` 返回 `{ ok = false, error = ... }` table，而不是让 Lua skill 自己解析 FFI 错误。
- 如果 callback 的 `call` 响应不是 JSON object，运行时会包装为 `{ "ok": true, "value": ... }`，确保 Lua 侧拿到 table。

### 10.16 `run_lua`

作用：

- 执行一段任意 Lua 代码

适合：

- 调试
- 宿主 smoke test
- 系统能力验证

说明：

- `code` 仍然是普通 UTF-8 字符串
- `args_json` 当前通过 `FfiBorrowedBuffer` 传入
- 返回值 `result_json_out` 继续通过 `FfiOwnedBuffer` 返回
- 标准 C ABI 与 `_json` 请求都不接收 authority。
- 该接口执行当前运行时环境中的 Lua 代码，Lua 代码可以调用当前已激活的 skill。
- 如果宿主不希望普通用户执行任意 Lua，应在暴露前单独封装权限或不开放该接口。

## 11. 生命周期链路处理逻辑

### 11.1 disable

作用：

- 写停用标记
- reload

当前语义：

- 失败会回滚
- system 版本必须由宿主注入 authority；只有 `System` 可操作 `ROOT`

### 11.2 enable

作用：

- 删除停用标记
- reload

### 11.3 install

作用：

- 下载并安装一个受管 skill

当前主链：

1. 解析来源
2. 下载 release 资产
3. 校验 `checksums.txt`
4. 解包到 staging
5. 校验 manifest
6. 准备暂存目录
7. reload
8. 成功后提交 install record
9. 失败则回滚

当前特点：

- 已做事务化
- staging 失败会自动清理
- checksum 失败会自愈重下一次
- 普通用户管理面安装目标只能是实际存在的 `PROJECT` 或 `USER`
- 未指定目标时，普通 install 优先写入 `USER`，没有 `USER` 时写入 `PROJECT`；只有 `ROOT` 时失败
- `ROOT` 级安装应走 system tools 或受控 system updater
- system install 未指定目标时只默认写入 `ROOT`；缺少 `ROOT` 时明确失败，不回退到普通层
- 如果 `ROOT` 已存在同名 `skill_id`，任何 authority 都不能向 `PROJECT` / `USER` install 同名 skill；必须先用系统权限卸载 ROOT 层 skill

### 11.4 update

作用：

- 基于来源记录更新已安装 skill

当前主链：

1. 读取 install record
2. 查询来源最新版本
3. 下载并校验新包
4. 解包到 staging
5. 备份旧 skill
6. 放置新版本
7. reload
8. 提交 install record
9. 删除旧 backup
10. 差分清理旧依赖

事务语义：

- reload 失败会回滚到旧版本
- install record 提交失败也会回滚
- 旧依赖清理失败只产生 warning，不会把更新误报成失败
- 普通用户管理面更新目标只能是 `PROJECT` 或 `USER`
- `ROOT` 级更新应走 system tools 或受控 system updater
- 显式目标 root 必须属于当前 root 链，否则在解析目标 skill 前直接拒绝
- 如果 `ROOT` 已存在同名 `skill_id`，任何 authority 都不能向 `PROJECT` / `USER` update 同名 skill

### 11.5 uninstall

作用：

- 卸载 skill

当前主链：

1. 暂存要删除的 skill 目录
2. reload
3. 成功后正式提交卸载
4. 清理 install record / disabled 标记
5. 可选删除数据库
6. 清理 skill 私有依赖

事务语义：

- reload 失败会回滚 skill 目录与 install record
- 数据库清理失败只记 warning，不会把已成功卸载误报成失败
- 普通用户管理面卸载目标只能是 `PROJECT` 或 `USER`
- `ROOT` 级卸载应走 system tools 或受控 system updater
- 显式目标 root 必须属于当前 root 链，否则直接拒绝
- 即使 `ROOT` 存在同名 `skill_id`，显式目标 uninstall 仍允许删除 `PROJECT` / `USER` 中的同名残留

## 12. install / update 的来源模型

当前来源记录不属于 `skill.yaml`，而属于安装状态。

受管安装成功后，会写入：

- `state/installs/<skill_id>.yaml`

当前 GitHub 模型的关键字段是：

- source type
- repo locator
- tag
- version

只有通过 install 安装的 skill 才有来源记录。  
手工复制进来的 skill 不参与自动更新。

## 13. skill roots 与空间模型

FFI 宿主接入时，推荐优先使用 `RuntimeSkillRoot[]`。

每个根包含：

- `name`
- `skills_dir`

要求：

- 正式产品语义固定为 `ROOT`、`PROJECT`、`USER`
- 加载顺序固定为 `ROOT -> PROJECT -> USER`
- root 链必须包含 `ROOT`；缺少 `ROOT` 时启动或加载失败
- name 唯一
- 物理路径唯一
- 空间父目录不能冲突
- `skills_dir` 必须是目录

当前根链是有序覆盖链：

- 前面的优先级更高
- `ROOT` 是系统控制级
- `ROOT` 中存在同名 `skill_id` 时，该 id 由系统层全局占用，`PROJECT` 与 `USER` 中的同名 skill 不应加载
- `ROOT` 中存在同名 `skill_id` 时，无论 `System` 还是 `DelegatedTool`，都不能向 `PROJECT` / `USER` install 或 update 同名 skill
- `PROJECT` / `USER` 中的同名残留允许通过显式目标 uninstall 清理
- `PROJECT` 在 `ROOT` 无同名 skill 时覆盖 `USER`
- 普通 `vulcan.runtime.skills.*` 管理面只能操作 `PROJECT` / `USER`
- system tools 只有在 `System` authority 下可以操作 `ROOT`；`DelegatedTool` 在管理、查询与 prompt completion 面不可见 ROOT skills
- 不建议把 ROOT 调整能力暴露给普通用户；若必须开放 system tools，应由宿主 wrapper 固定注入 `DelegatedTool`

如果宿主只配置 `ROOT`，却仍希望让用户增删改 skill，本质上是在开放系统层操作。此时 LuaSkills 无法替宿主判断业务合法性，宿主必须自行处理权限、白名单、确认与审计。

如果宿主内部存在组织、工作区、模板等更复杂来源，建议在宿主侧折叠成单个对外 `PROJECT` 标签，而不是向用户暴露任意多层 root 链。

完整策略见 [Skill Root 层级与管理边界](../architecture/skill-root-layer-policy.md)。

## 14. 标准 C ABI 与公共 `_json` FFI 的选择建议

### 14.1 优先使用标准 C ABI 的情况

- C / C++ / C#
- 高性能宿主
- 想减少 JSON 编解码
- 想显式控制内存释放

### 14.2 优先使用公共 `_json` FFI 的情况

- Python
- TypeScript / Node.js
- Go SDK 主链
- 调试工具
- 快速接入
- 动态值很多的场景

TypeScript / Node.js 宿主建议优先使用独立仓库 [`luaskills-sdk-typescript`](https://github.com/LuaSkills/luaskills-sdk-typescript) 中的 `@luaskills/sdk`。SDK 仍然走公共 `_json` FFI，但会统一处理动态库加载、`FfiBorrowedBuffer` / `FfiOwnedBuffer`、JSON 包络、engine 生命周期、authority、root helper、skill-config、lifecycle 管理入口，以及 SQLite / LanceDB 的 JSON provider callback 注册与清理。

Python 宿主建议优先使用独立仓库 [`luaskills-sdk-python`](https://github.com/LuaSkills/luaskills-sdk-python) 中的 `luaskills-sdk`。SDK 仍然走公共 `_json` FFI，但会统一处理 `ctypes` 动态库加载、buffer 释放、JSON 包络、authority、root helper、skill-config、lifecycle 管理入口，以及 SQLite / LanceDB 的 JSON provider callback 注册与清理。

Go 宿主如果希望用 Go module 封装主链，建议优先使用独立仓库 [`luaskills-sdk-go`](https://github.com/LuaSkills/luaskills-sdk-go)。该 SDK 使用 cgo 调公共 `_json` FFI，调用方需要显式配置 `CGO_ENABLED=1`、C 编译器、链接库搜索路径与运行时动态库路径。Go 的 provider callback 当前保留为显式边界 API：需要正式使用 `host_callback + json` 时，应由宿主工程实现受控 cgo callback bridge，或先使用 TypeScript / Python SDK 承担 JSON callback。

### 14.3 混合使用策略

完全可以采用：

- 引擎创建 / load / list / 生命周期操作：标准 C ABI
- 动态调用 / 调试 / 跨语言壳层：公共 `_json` FFI

当前协议允许这种混合使用。

### 14.4 一页式选型结论

如果宿主正在做第一次技术选型，可以直接按下面的结论判断：

- Rust 宿主：
  - 优先直接引用 Rust API
  - 不建议额外包装成 FFI 再回调自己
- C / C++ / C# / 能稳定处理结构体与 out 指针的宿主：
  - 优先选择标准 C ABI
  - 适合把它当成正式低层契约长期维护
- Python / Node.js / TypeScript / 动态脚本宿主：
  - 优先选择公共 `_json` FFI
  - 适合快速接入、原型验证和减少 ABI 绑定成本
  - TypeScript / Node.js 优先使用 `@luaskills/sdk`，不要在业务代码里重复手写 buffer 和 JSON FFI 包络
  - Python 与 TypeScript SDK 已封装 JSON provider callback 的注册、错误转换与 buffer clone
- Go 宿主：
  - 如果追求标准 C ABI 的最低编解码成本，可以继续直接接标准 C ABI
  - 如果希望快速获得 engine/root/query/call/lifecycle 封装，优先使用独立 Go SDK 的公共 `_json` FFI 封装
  - 如果需要 provider callback，必须额外设计宿主拥有的 cgo callback bridge，不能把进程级回调生命周期交给业务层临时函数
- 一个宿主同时需要“稳定主链”和“动态扩展链”时：
  - 可以混合使用
  - 标准 C ABI 承载 `engine/load/list/call/lifecycle`
  - 公共 `_json` FFI 承载动态安装、调试和宿主快速桥接

一句话总结：

- 标准 C ABI 解决“正式低层接入”
- 公共 `_json` FFI 解决“快速跨语言接入”
- Rust API 解决“同语言直接集成”

## 15. C / Python / Go / TypeScript 示例说明

示例位置：

- C：
  - [examples/ffi/c/demo.c](../../../examples/ffi/c/demo.c)
- Python：
  - [luaskills-sdk-python](https://github.com/LuaSkills/luaskills-sdk-python)
  - [Python SDK provider callback example](https://github.com/LuaSkills/luaskills-sdk-python/blob/main/examples/provider_callback.py)
  - pip 安装后可运行 `python -m luaskills.examples.provider_callback`
  - [examples/ffi/python/demo.py](../../../examples/ffi/python/demo.py)
  - [examples/ffi/python/lifecycle_demo.py](../../../examples/ffi/python/lifecycle_demo.py)
  - [examples/ffi/python/query_demo.py](../../../examples/ffi/python/query_demo.py)
- Go：
  - [luaskills-sdk-go](https://github.com/LuaSkills/luaskills-sdk-go)
  - [Go SDK provider callback example](https://github.com/LuaSkills/luaskills-sdk-go/blob/main/examples/provider_callback/main.go)
  - [examples/ffi/go/demo.go](../../../examples/ffi/go/demo.go)
  - [examples/ffi/go/lifecycle_demo/main.go](../../../examples/ffi/go/lifecycle_demo/main.go)
  - [examples/ffi/go/query_demo/main.go](../../../examples/ffi/go/query_demo/main.go)
- TypeScript：
  - [luaskills-sdk-typescript](https://github.com/LuaSkills/luaskills-sdk-typescript)
  - [TypeScript SDK provider callback example](https://github.com/LuaSkills/luaskills-sdk-typescript/blob/main/examples/provider-callback.mjs)
  - [examples/ffi/typescript/README.md](../../../examples/ffi/typescript/README.md)
  - [examples/ffi/typescript/demo.ts](../../../examples/ffi/typescript/demo.ts)
  - [examples/ffi/typescript/lifecycle_demo.ts](../../../examples/ffi/typescript/lifecycle_demo.ts)
  - [examples/ffi/typescript/query_demo.ts](../../../examples/ffi/typescript/query_demo.ts)

当前示例主要演示：

- 查询版本
- 创建引擎
- 加载根链
- 读取结构化 `list_entries` 结果
- 通过 `FfiBorrowedBuffer` 调用标准 `call_skill`
- 读取结构化调用结果
- 通过 `FfiBorrowedBuffer` 调用标准 `run_lua`
- 读取 JSON 结果缓冲
- 释放引擎

其中：

- `c/demo.c` 更贴近底层标准 C ABI 契约
- Python / Go / TypeScript 示例更适合展示各语言桥接方式
- `python/lifecycle_demo.py` 额外聚焦标准 ABI 的 `disable_skill / enable_skill` 生命周期切换
- `python/query_demo.py` 额外聚焦标准 ABI 的查询辅助接口
- `go/lifecycle_demo/main.go` 额外聚焦标准 ABI 的 `disable_skill / enable_skill` 生命周期切换
- `go/query_demo/main.go` 额外聚焦标准 ABI 的查询辅助接口
- `typescript/lifecycle_demo.ts` 额外聚焦标准 ABI 的 `disable_skill / enable_skill` 生命周期切换
- `typescript/query_demo.ts` 额外聚焦标准 ABI 的查询辅助接口

这是一组覆盖“版本 -> 引擎 -> 加载 -> 结构化枚举 -> 标准调用 -> 标准 Lua 执行”的最小 smoke test。  
后续如果宿主要做完整接入，建议直接按本文档把：

- install / update / uninstall

接进去。

另外还提供一个可直接运行的完整烟测目录：

- [examples/ffi/standard_runtime/README.md](../../../examples/ffi/standard_runtime/README.md)
- [examples/ffi/demo_runtime/README.md](../../../examples/ffi/demo_runtime/README.md)

其中：

- `standard_runtime`
  - 提供标准 ABI 示例共用的最小 skill 夹具
  - 默认包含 `demo-standard-ffi-skill-ping`，可直接演示 `call_skill`
  - 同时提供稳定 runtime 目录布局，便于标准示例继续演示 `run_lua`
  - 也可直接配合 `python/lifecycle_demo.py` 演示 `disable_skill / enable_skill`
- `demo_runtime`
  - 提供动态安装与调用烟测链

`demo_runtime` 会：

- 使用仓库内空 runtime root
- 动态安装 `LuaSkills/demo-skill`
- 调用 `demo-skill-demo-status`
- 输出 success

### 15.1 示例选型速查

如果宿主只想先抓一条最短路径，请按目标直接看对应示例：

- 想先理解标准 ABI 的主调用链：
  - [examples/ffi/c/demo.c](../../../examples/ffi/c/demo.c)
  - [examples/ffi/python/demo.py](../../../examples/ffi/python/demo.py)
  - [examples/ffi/go/demo.go](../../../examples/ffi/go/demo.go)
  - [examples/ffi/typescript/demo.ts](../../../examples/ffi/typescript/demo.ts)
- 想看 `disable_skill / enable_skill`：
  - [examples/ffi/python/lifecycle_demo.py](../../../examples/ffi/python/lifecycle_demo.py)
  - [examples/ffi/go/lifecycle_demo/main.go](../../../examples/ffi/go/lifecycle_demo/main.go)
  - [examples/ffi/typescript/lifecycle_demo.ts](../../../examples/ffi/typescript/lifecycle_demo.ts)
- 想看 `is_skill / skill_name_for_tool / prompt_argument_completions`：
  - [examples/ffi/python/query_demo.py](../../../examples/ffi/python/query_demo.py)
  - [examples/ffi/go/query_demo/main.go](../../../examples/ffi/go/query_demo/main.go)
  - [examples/ffi/typescript/query_demo.ts](../../../examples/ffi/typescript/query_demo.ts)
- 想看标准 ABI 示例共用的最小 skill 夹具：
  - [examples/ffi/standard_runtime/README.md](../../../examples/ffi/standard_runtime/README.md)
- 想看公共 `_json` FFI 驱动的动态安装烟测：
  - [examples/ffi/demo_runtime/README.md](../../../examples/ffi/demo_runtime/README.md)
- 想看宿主接管 SQLite / LanceDB provider：
  - [examples/ffi/host_provider_demo/README.md](../../../examples/ffi/host_provider_demo/README.md)

建议阅读顺序：

1. 先从标准 ABI 的 `demo` 看最短闭环。
2. 再根据需要进入 `lifecycle_demo` 或 `query_demo`。
3. 最后再看 `demo_runtime` 和 `host_provider_demo` 这类更接近宿主集成的扩展场景。

## 16. 推荐接入顺序

推荐宿主集成顺序：

1. 接入 `version`
2. 接入 `engine_new` / `engine_free`
3. 接入 `load_from_roots`
4. 接入 `list_entries`
5. 接入 `call_skill`
6. 接入 `list_skill_help` / `render_skill_help_detail`
7. 接入生命周期接口

这样最稳。

## 17. 常见失败场景

### 17.1 `engine_new` 失败

常见原因：

- 目录配置无效
- 必填 sibling 目录名为空
- 宿主配置不合法

### 17.2 `load` 失败

常见原因：

- `skills_dir` 不是目录
- `skill.yaml` 不合法
- 入口脚本缺失
- help 树解析失败

### 17.3 `call_skill` 失败

常见原因：

- tool name 不存在
- 参数不合法
- Lua 运行时错误
- provider binding 错误

### 17.4 `install / update` 失败

常见原因：

- 网络错误
- release 资产缺失
- checksum 校验失败
- manifest 与来源不一致
- reload 失败触发事务回滚

## 18. 当前约束

### 18.1 标准 ABI 里仍保留 JSON 字段

原因不是偷懒，而是这几类值本来就是动态结构：

- request context
- budget snapshot
- tool config
- arbitrary Lua args
- arbitrary Lua return values

对这类值强行做固定 C ABI，收益很低，破坏性很高。

### 18.2 `_json` 是正式公共接口，不是临时调试接口

`_json` 不是次级能力，而是正式的高层公共接口。  
只是它更偏易用性与跨语言接入，不偏极致性能。

## 19. 对接建议结论

如果目标是接入严格 `system_package`、持久 Python/Node 会话、受管会话事件与 wake callback，请先按本篇完成基础 FFI 生命周期，再按 [System Plugin 受管运行时使用指南](../system-plugin-managed-runtime.md) 完成端到端链路与关闭验收。

如果宿主：

- 有稳定 FFI 封装能力
- 关注性能
- 希望减少 JSON 编解码

则优先用标准 C ABI。

如果宿主：

- 需要快速接入
- 语言本身更偏动态
- 希望先把链路打通

则优先用公共 `_json` FFI。

最推荐的工程实践是：

- 用标准 C ABI 承载底层稳定主链
- 用公共 `_json` FFI 承载动态语言接入、动态扩展与调试能力

这也是当前 `luaskills` FFI 设计的核心目的。
