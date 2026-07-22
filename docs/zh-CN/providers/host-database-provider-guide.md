# 宿主数据库 Provider 对接说明

## 1. 文档目标

本文档用于说明 `luaskills` 当前如何把 SQLite / LanceDB 数据库操作交给宿主自己处理。

本文档重点覆盖：

- 数据库 provider 模式分类
- 回调模式分类
- `space_label` / `binding_tag` 的语义
- 宿主需要实现的最小必要接口
- 独立 demo 的使用方式

## 2. 当前支持的数据库接入模式

当前每个数据库后端都支持三种 provider mode：

- `dynamic_library`
- `host_callback`
- `space_controller`

它们分别通过下列字段控制：

- `LuaRuntimeHostOptions.sqlite_provider_mode`
- `LuaRuntimeHostOptions.lancedb_provider_mode`
- `LuaRuntimeHostOptions.sqlite_callback_mode`
- `LuaRuntimeHostOptions.lancedb_callback_mode`

这些配置是**按后端分别设置**的，也就是说 SQLite 与 LanceDB 可以使用不同模式。

例如：

- SQLite 使用 `host_callback + json`
- LanceDB 使用 `dynamic_library`

如果宿主已经把某个数据库型 skill 的能力切换到原生、gRPC、VMM 或其他实现，可以在加载前通过 `LuaRuntimeHostOptions.ignored_skill_ids` 忽略对应 skill。  
被忽略的 skill 不会进入依赖准备、SQLite/LanceDB 绑定或 entry 注册阶段，因此不会产生数据库 provider 请求。

### `dynamic_library`

由 `luaskills` 自己加载本地动态库，并直接调用：

- SQLite backend
- LanceDB backend

这种模式下：

- lib 自己计算默认数据库路径
- lib 自己打开数据库
- skill 只做表操作与向量操作

### `host_callback`

由宿主注册数据库 provider 回调。

这种模式下：

- lib 不再决定数据库最终创建位置
- lib 不再要求数据库一定落在默认 sibling 目录下
- lib 只负责把数据库请求和稳定绑定上下文回调给宿主
- 宿主决定：
  - 数据库放在哪
  - 是否共享
  - 是否走本地文件
  - 是否转发给别的服务

### `space_controller`

由 lib 把数据库请求转发给外部空间控制器。

这种模式下：

- lib 不直接打开数据库
- lib 不依赖宿主本进程内的回调实现
- 数据库 ownership 与共享策略由控制器负责
- 代码层固定依赖 `vldb-controller-client`
- controller 服务程序由宿主自行复制到本地稳定路径，再通过 `LuaRuntimeHostOptions.space_controller.executable_path` 提供给 lib
- 如果宿主没有显式指定 `endpoint`，运行时默认使用共享端点 `http://127.0.0.1:19801`
- 如果宿主指定了不同端点，则可切换到独立 controller 实例
- 若 `auto_spawn=true`，则 `endpoint` 必须使用可映射为本地 bind 地址的形式，例如 `19801`、`:19801`、`127.0.0.1:19801`、`localhost:19801` 或 `[::1]:19801`
- 若 `endpoint` 指向远端主机名或远端服务地址，则必须将 `auto_spawn` 设为 `false`，并由宿主自行保证 controller 已经可连接

推荐理解方式：

- Rust SDK：走 crates.io 固定版本 `vldb-controller-client = 0.2.3`
- SDK 注册时宿主名使用 `client_name`，会话主键 `client_session_id` 由 controller 分配并由 SDK 内部自动管理
- `binding_tag` 仍保留为稳定数据库标签与诊断标签，但在 `space_controller` 模式下不会直接等同于 controller `binding_id`
- lib 会基于稳定 `binding_tag` 与当前 controller client 会话域生成客户端隔离的 controller `binding_id`
- `v0.2.1` 额外修复了共享本地 endpoint 在 `auto_spawn` 场景下的重复拉起协调风险
- controller 可执行程序：走宿主本地复制与管理
- 共享还是独占：由 `endpoint` 决定

## 3. 为什么要引入宿主 callback 模式

因为记忆型 skill 使用的：

- SQLite
- LanceDB
- BM25 / FTS

本质上都带有状态和共享语义。

如果多个宿主进程都直接打开同一个物理数据库，就会出现：

- SQLite 锁冲突
- LanceDB 并发写入风险
- 多进程下的共享与 ownership 混乱

宿主 `host_callback` 模式的目标就是：

**让 lib 保持统一的数据库调用接口，同时把真正的数据库 ownership 与数据库目录分配交给宿主。**

## 4. 回调模式分类

当 provider mode 选择 `host_callback` 时，还必须继续指定 callback mode：

- `standard`
- `json`

它们分别通过下列字段控制：

- `LuaRuntimeHostOptions.sqlite_callback_mode`
- `LuaRuntimeHostOptions.lancedb_callback_mode`

规则如下：

- provider mode 不等于 callback mode
- 只有 provider mode 为 `host_callback` 时，callback mode 才会参与分发
- lib 不再根据“是否注册了某种回调”来推断当前模式
- lib 始终先看宿主初始化时传入的 mode，再决定走哪条链路

### `standard`

- 走结构化 C ABI 回调
- 适合 C / C++ / Go / Rust 等更偏稳定 ABI 的宿主

### `json`

- 走 JSON 请求 / JSON 响应回调
- 适合 Python / Node / 快速原型验证

## 5. 绑定上下文

宿主 provider 回调不会只拿到一段 SQL 或一段向量请求。

lib 会一并提供稳定绑定上下文：

- `space_label`
- `skill_id`
- `binding_tag`
- `root_name`
- `space_root`
- `skill_dir`
- `skill_dir_name`
- `database_kind`
- `default_database_path`

其中最关键的是：

### `space_label`

由宿主提供的稳定空间标签，例如：

- `ROOT`
- `PROJECT`
- `USER`

对于项目级接管，**宿主必须保证这个标签稳定**。  
这是前置条件，不由 lib 自动推导。

正式 skill root 层级语义固定为 `ROOT -> PROJECT -> USER`，启动或加载时必须传入 `ROOT` root。`ROOT` 是系统控制级，`PROJECT` 与 `USER` 是普通用户管理面可操作层。数据库 provider 可以继续根据 `space_label` 做隔离，但不建议把更多项目子层级暴露给用户。

### `binding_tag`

由 lib 统一组合：

```text
{space_label}-{skill_id}
```

例如：

- `ROOT-vulcan-work-memory`
- `PROJECT-vulcan-ai-memory`

这个字段用于：

- 宿主管理数据库命名空间
- 共享数据库标签
- 多进程稳定复用

补充说明：

- 在 `host_callback` 模式下，宿主可以直接基于 `binding_tag` 路由或命名真实数据库
- 在 `space_controller` 模式下，`binding_tag` 仍然保持稳定，但 lib 会额外派生客户端隔离的 controller `binding_id`，避免不同 controller client 实例争抢同一 binding

### `default_database_path`

这是 lib 按内嵌旧规则推导出的默认数据库路径。

它的作用只是：

- 兼容
- 诊断
- 给宿主做参考

它**不是**宿主必须使用的真实数据库路径。

## 6. 标准回调模式

### SQLite

注册函数：

- `luaskills_ffi_set_sqlite_provider_callback`

头文件：

- [include/luaskills_ffi.h](../../../include/luaskills_ffi.h)

回调签名：

- 输入：
  - `const FfiSqliteProviderRequest *request`
  - `void *user_data`
- 输出：
  - `FfiOwnedBuffer *response_json_out`
  - `FfiOwnedBuffer *error_out`

### LanceDB

注册函数：

- `luaskills_ffi_set_lancedb_provider_callback`

头文件：

- [include/luaskills_ffi.h](../../../include/luaskills_ffi.h)

回调签名：

- 输入：
  - `const FfiLanceDbProviderRequest *request`
  - `void *user_data`
- 输出：
  - `FfiOwnedBuffer *meta_json_out`
  - `FfiOwnedBuffer *data_out`
  - `FfiOwnedBuffer *error_out`

### 标准回调的关键点

标准模式下，请求最外层是固定 C 结构：

- `action`
- `binding`
- `FfiBorrowedBuffer input_json`

也就是说：

- 外层协议是结构化的
- 动作内部参数仍然通过 `input_json` 承载
- `input_json` 不再依赖 NUL 终止字符串，而是显式 `ptr + len`

这样可以同时满足两件事：

1. 宿主拿到的是标准 ABI 请求，而不是一整坨 JSON
2. 内层动态参数不用被硬编码成大量脆弱 C 结构

### 宿主如何返回字符串和字节

标准回调下，如果宿主需要返回：

- `response_json_out`
- `meta_json_out`
- `data_out`

应优先使用 `luaskills` 提供的辅助函数：

- `luaskills_ffi_buffer_clone`
- `luaskills_ffi_buffer_free`

也就是说：

- 标准 callback 不再要求宿主直接写 `char **` 或 `uint8_t ** + len`
- 宿主应返回 `luaskills` 所有的 `FfiOwnedBuffer`
- `response_json_out` 与 `meta_json_out` 中的字节内容必须是合法 UTF-8 JSON 文本
- `data_out` 中的字节内容可为任意二进制载荷

除此之外，还必须遵守以下约束：

- callback 应在 `engine_new` 前注册，避免 engine 创建时拍到旧 callback 快照
- callback 不允许把 Rust panic、C++ exception 或其他异常机制穿过 C ABI 边界
- 若一个进程内需要多套不同数据库 callback 逻辑，应分别创建 engine，而不是依赖“创建后切换全局 callback”

## 7. JSON 回调模式

### SQLite

注册函数：

- `luaskills_ffi_set_sqlite_provider_json_callback`

头文件：

- [include/luaskills_json_ffi.h](../../../include/luaskills_json_ffi.h)

### LanceDB

注册函数：

- `luaskills_ffi_set_lancedb_provider_json_callback`

头文件：

- [include/luaskills_json_ffi.h](../../../include/luaskills_json_ffi.h)

### JSON 回调规则

JSON 回调模式下，宿主收到的是一整份 JSON 请求字节缓冲。

接口形态是：

- 输入：
  - `FfiBorrowedBuffer request_json`
  - `void *user_data`
- 输出：
  - `FfiOwnedBuffer *response_out`
  - `FfiOwnedBuffer *error_out`
- 返回值：
  - `int32_t` 状态码

也就是说：

- `request_json` 不是 NUL 终止字符串约定，而是 `ptr + len`
- `response_out` 必须写入 UTF-8 JSON 文本缓冲
- `error_out` 也应写入 UTF-8 错误文本缓冲
- callback 成功时返回 `FFI_STATUS_OK`
- callback 失败时返回非零状态码，并尽量写入 `error_out`

例如 SQLite 请求会是：

```json
{
  "action": "query_json",
  "binding": {
    "space_label": "ROOT",
    "skill_id": "host-provider-sqlite-demo",
    "binding_tag": "ROOT-host-provider-sqlite-demo",
    "root_name": "ROOT",
    "space_root": "D:/.../runtime_root/skills",
    "skill_dir": "D:/.../runtime_root/skills/host-provider-sqlite-demo",
    "skill_dir_name": "host-provider-sqlite-demo",
    "database_kind": "sqlite",
    "default_database_path": "D:/.../databases/sqlite/host-provider-sqlite-demo"
  },
  "input": {
    "sql": "SELECT 1"
  }
}
```

宿主返回的仍然是 JSON 文本，但载体已经变成 `FfiOwnedBuffer`。

所以：

- JSON 模式对动态语言最友好
- Python / Node / 轻量桥接代码最适合先走这条路径

## 8. 必要接口要求

当宿主选择 `host_callback` 模式时，必须实现对应数据库所需的必要动作。

### SQLite 当前动作集合

- `execute_script`
- `execute_batch`
- `query_json`
- `query_stream`
- `query_stream_wait_metrics`
- `query_stream_chunk`
- `query_stream_close`
- `tokenize_text`
- `upsert_custom_word`
- `remove_custom_word`
- `list_custom_words`
- `ensure_fts_index`
- `rebuild_fts_index`
- `upsert_fts_document`
- `delete_fts_document`
- `search_fts`

### LanceDB 当前动作集合

- `create_table`
- `vector_upsert`
- `vector_search`
- `delete`
- `drop_table`

如果宿主启用了某个数据库的 `host_callback` 模式，却没有注册对应回调，运行时会直接报错，不会静默回退。

在 `beta` / `v0.1.0` 发布阶段，这条规则应被视为正式接入契约的一部分：

- `host_callback` 不会自动补全缺失 callback
- callback 注册顺序错误属于宿主初始化错误
- 建议固定采用“注册 callback -> 创建 engine -> load/reload -> 调用”的启动顺序

## 9. 宿主应该如何决定数据库落点

宿主不应该再直接信任：

- lib 默认数据库路径

而应该根据：

- `binding_tag`
- `space_label`
- `database_kind`

自己决定真实数据库位置。

例如宿主可以把：

```text
ROOT-vulcan-work-memory
```

映射到：

```text
C:/HostManagedData/sqlite/ROOT-vulcan-work-memory.db
```

或者：

```text
D:/project-a/.host-dbs/sqlite/PROJECT-vulcan-ai-memory.db
```

或者：

- 一个统一数据库服务中的租户命名空间

## 10. 独立 demo

当前仓库已经提供一个独立演示：

- [../examples/ffi/host_provider_demo/README.md](../../../examples/ffi/host_provider_demo/README.md)

这个 demo 的特点是：

- skill 依旧通过 `vulcan.sqlite.*` 调数据库
- `luaskills` 切到 `host_callback` 模式
- Python 宿主注册 JSON callback
- 宿主把 SQLite 请求转发给 `vldb-sqlite`
- 宿主最终数据库落点不使用 `default_database_path`
- 而是使用：

```text
runtime_root/host_managed/sqlite/<binding_tag>.db
```

这正好用来验证：

- 宿主接管数据库
- lib 只提供稳定绑定标签
- 真实数据库目录由宿主自己决定

## 11. 当前最推荐的落地顺序

如果宿主是动态语言或先做原型：

1. 先用 JSON callback
2. 先只接 SQLite
3. 先验证 `binding_tag` 路由
4. 再补 LanceDB

如果宿主是 Go / C / Rust / 高性能场景：

1. 用标准 callback
2. 外层请求走结构化 ABI
3. 内层动作参数仍走 `FfiBorrowedBuffer input_json`
4. 用 `buffer_clone` 填充 `FfiOwnedBuffer` 返回拥有型结果

## 12. 一句话总结

当前 `host_callback` 模式的核心原则是：

**lib 保持统一数据库接口与稳定绑定上下文，宿主负责决定数据库真实落点、真实 ownership 与真实执行后端。**
