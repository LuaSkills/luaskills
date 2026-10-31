# Host-owned SQLite

[Documentation hub](../index.md) | [Detailed integration guide](../zh-CN/integration/host-database-provider-guide.md)

LuaSkills 0.7.3 routes skill SQLite operations exclusively through registered host callbacks. The host owns database files, connections, concurrency, migrations and cleanup.

The skill's `sqlite` declaration accepts only `enable`; retired logging options and unknown fields are rejected. `status()` and `info()` report binding readiness, without database-library placeholders or inferred host capabilities. Business input is separate from the authoritative outer binding. Suggested paths follow the fixed `databases` layout when `runtime_root` is set; otherwise they use the host's `database_dir_name`. The host still chooses the actual storage location.

Use `RuntimeDatabaseProviderCallbacks::with_sqlite` or `with_sqlite_json`, then `LuaEngine::new_with_database_providers`. Explicit registrations are engine-scoped and survive reloads. `sqlite_callback_mode` selects `standard` or `json`; a missing selected callback rejects the database binding. Existing process-wide SQLite registration remains available for `LuaEngine::new`.

C hosts use `FfiDatabaseProviderCallbacksV1` and `luaskills_ffi_engine_new_with_database_providers_v1` with v4 engine options. Callback code and context must outlive the engine and pending work. Return buffers must use LuaSkills allocation/free functions.

LanceDB skill support, controller integration and database dynamic-library loading have been removed, including their configuration fields and exported symbols. Recompile C hosts against the new header and update language SDK layouts. SQLite request semantics and action numbers remain unchanged. Existing database files are retained; vector storage can be implemented by a host-owned Node.js or Python extension.

The default library does not compile SQLite. Rust hosts provide durable operation history through `OperationJournalBackend` and `OperationJournal::from_backend`. The explicit `sqlite-journal` feature retains the existing SQLite journal format and recovery implementation. C path-based journal configuration requires that feature and fails explicitly when it is unavailable.

See the [Python sqlite3 host example](../../examples/ffi/host_provider_demo/README.md) for a database implementation with no controller dependency.

The standalone `luaskills-debug` executable registers its own bundled SQLite host. Build it with `--features debug-sqlite --bin luaskills-debug`; this feature does not enable `sqlite-journal`. Its `execute_script`, `execute_batch`, and `query_json` operations use the skill binding's database path, preserve existing files, and support positional parameters. Batch writes commit atomically. Native FTS5 remains available through SQL; controller-specific stream/tokenizer/FTS convenience actions return explicit unsupported errors. See the [debugger SQLite contract](../zh-CN/integration/debug-sqlite-host.md) for input and output shapes. Build the default core library separately with `--lib` and no debug feature.
