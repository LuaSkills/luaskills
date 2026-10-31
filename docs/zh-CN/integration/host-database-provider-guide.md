# SQLite 宿主注册

## 接入边界

从 0.7.3 起，技能数据库仅支持宿主注册的 SQLite 回调。LuaSkills 负责技能作用域、请求分发和回调生命周期；宿主负责数据库位置、连接、并发、事务、迁移、备份及资源释放。

技能层 LanceDB 支持、vldb 控制器客户端、自动启动、数据库动态库加载及相关 Rust/FFI 类型和选项已直接删除，没有保留符号或兼容占位。旧数据库文件不迁移、不删除。需要向量数据库的技能可以通过宿主管理的 Node.js 或 Python 扩展实现。

这是接口移除版本。标准 C 结构中的退役字段也已移除，必须使用本版本头文件重新编译宿主；旧 SDK 的结构声明和导出清单不能直接配合新动态库。仍在使用的 SQLite 动作编号、绑定身份、JSON 请求结构及缓冲释放规则保持不变。旧 `lancedb` 技能配置及旧数据库宿主选项会作为未知字段拒绝。

## Rust 实例注册

使用 `RuntimeDatabaseProviderCallbacks::default().with_sqlite(callback)` 或 `with_sqlite_json(callback)` 构造实例能力集合，然后调用 `LuaEngine::new_with_database_providers(options, callbacks)`。

每个实例保存独立集合，重载候选继续持有同一集合。显式空集合不会继承进程全局默认值。嵌套 `vulcan.call` 临时切换到目标技能绑定，返回后恢复源技能身份。

`LuaRuntimeHostOptions.sqlite_callback_mode` 选择 `standard` 或 `json`，默认 `standard`。选择的传输必须有对应注册，不会尝试其他回调或数据库后端。加载器沿用逐技能失败报告行为：缺少回调的 SQLite 技能不能完成加载，其他有效技能仍可加载。

原有 `set_sqlite_provider_callback` 和 `set_sqlite_provider_json_callback` 是进程级默认注册入口。使用 `LuaEngine::new` 时会捕获当时的默认回调；后续修改全局注册不会改变已经创建的引擎。

## C 接口

`FfiDatabaseProviderCallbacksV1` 包含 `user_data`、`sqlite` 和 `sqlite_json`。通过 `luaskills_ffi_engine_new_with_database_providers_v1` 传入该描述符与 `FfiLuaEngineOptionsV4`；成功返回零并输出引擎句柄，失败时句柄清零并返回自有错误缓冲。

描述符本身仅借用至构造调用结束；回调代码与 `user_data` 必须在引擎及所有在途调用结束之前保持有效。释放引擎之后再释放宿主数据库资源。回调不得跨 C 边界抛出异常，同线程不得重入同一个引擎。

结构化 SQLite 回调接收 `FfiSqliteProviderRequest`，其中包括动作整数、绑定上下文和 UTF-8 JSON 输入字节。成功通过 `response_json_out` 返回 JSON；失败通过 `error_out` 返回诊断。两个输出都必须使用 LuaSkills 分配器创建，不能直接返回宿主语言的临时字符串或内存。

JSON 回调接收完整请求 JSON，通过相同的 `FfiOwnedBuffer` 规则返回响应。现有进程级入口为 `luaskills_ffi_set_sqlite_provider_callback` 和 `luaskills_ffi_set_sqlite_provider_json_callback`。可使用 `luaskills_ffi_buffer_clone` 分配返回字节，收到库返回缓冲后使用 `luaskills_ffi_buffer_free` 释放。

## 绑定上下文与数据位置

请求固定包含 `action`、`binding`、`input`。`binding` 的 `space_label`、`skill_id`、`root_name`、`space_root`、`skill_dir`、`skill_dir_name`、`database_kind`、`binding_tag`、`default_database_path` 由运行时生成；技能输入不能覆盖绑定身份。

SQLite 默认建议路径为技能根父目录下的 `{database_dir_name}/sqlite/{skill_id}/{skill_id}.sqlite3`；`skill_id` 来自技能目录身份，不使用清单显示名称。设置 `runtime_root` 时遵循既有固定布局，`database_dir_name` 规范化为 `databases`；未设置时沿用宿主显式目录名。`binding_tag` 沿用既有生成规则。注册回调只生成绑定，不创建数据库目录、不打开文件；实际数据库位置仍由宿主决定。

宿主可以按绑定映射自己的私有目录，必须自己验证权限、隔离租户和管理连接。技能卸载仅处理代码及依赖，不删除数据库。原数据库清理选项和结果字段已移除；SQLite 与既有 LanceDB 数据生命周期完全由宿主负责。

## SQLite 动作

结构化动作保留原编号 0 至 15：`execute_script`、`execute_batch`、`query_json`、`query_stream`、`query_stream_wait_metrics`、`query_stream_chunk`、`query_stream_close`、`tokenize_text`、`upsert_custom_word`、`remove_custom_word`、`list_custom_words`、`ensure_fts_index`、`rebuild_fts_index`、`upsert_fts_document`、`delete_fts_document`、`search_fts`。

输入及结果业务字段由宿主契约决定；不支持的动作应明确返回错误。技能仅通过 `sqlite.enable: true` 请求绑定，调用 `vulcan.sqlite.*` 使用宿主能力。旧 `log_level`、`slow_log_enabled`、`slow_log_threshold_ms` 已移除，传入这些或其他未知 SQLite 配置字段会报错；数据库诊断属于宿主策略。

`vulcan.sqlite.status()` 和 `info()` 只报告绑定元信息；`initialized` 表示回调绑定就绪，不代表宿主已经打开数据库。不再返回动态库路径、名称、版本、阶段、日志配置或未经宿主声明的能力列表。请求中的 `input` 是技能业务输入，外层 `binding` 才是运行时生成的身份；宿主应按外层绑定选择数据库。

## 调试工具

独立调试工具 `luaskills-debug` 自己提供 SQLite 宿主，启用 SQLite 的技能可以直接执行 SQL。构建使用 `--features debug-sqlite`，不改变默认核心库的依赖。输入、事务、数据目录与支持边界见[调试工具 SQLite 宿主](debug-sqlite-host.md)。

## 内部操作日志

默认库不编译 SQLite 引擎。Rust 宿主实现 `OperationJournalBackend`，通过 `OperationJournal::from_backend` 和已有工作线程接口提供持久历史。宿主必须保证原子修订比较、身份及上下文不变、终态合法、容量上限、不确定提交后的阻断和显式恢复。公开校验方法 `OperationSnapshot::validate_finalization`、`validate_finalization_successor` 及 `OperationReconciliation::validate` 可用于实现相同约束。

需要原 SQLite 操作日志格式时，宿主可显式启用 `sqlite-journal` 功能，并调用 `OperationJournal::open`。该适配器保留既有表结构、摘要、独占打开、提交确认及恢复规则，与技能 SQLite 回调完全独立。C 嵌入式运行时的路径式持久化声明同样需要此显式功能；默认构建遇到该声明会返回错误，绝不静默降级为内存存储。原日志文件无需迁移。

## 示例与后续接入

[Python 宿主示例](../../../examples/ffi/host_provider_demo/README.md) 使用标准库 `sqlite3` 实现脚本执行和查询，不需要数据库动态库或控制器。其他动作由实际宿主按需实现。

VulcanCode 本轮不修改，待 LuaSkills 验证、打包发布后再调整依赖和宿主适配。
