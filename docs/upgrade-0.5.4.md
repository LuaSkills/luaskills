# LuaSkills 0.5.4 Patch Upgrade Guide / 补丁升级说明

## English

LuaSkills `0.5.4` is a dependency-alignment patch for the `0.5.x` release line. It preserves the Rust API, JSON FFI request schemas, standard C ABI V3 structures, and language-SDK parameter contracts from `0.5.3`.

### Version alignment

- Rust crate: `luaskills = "0.5.4"`
- TypeScript SDK: `@luaskills/sdk@0.5.4`
- Python SDK: `luaskills-sdk==0.5.4`
- Go module: `github.com/LuaSkills/luaskills-sdk-go@v0.5.4`
- FFI, demo, and debug-tool assets: GitHub tag `v0.5.4`

### VLDB changes

- The Rust dependency is upgraded from `vldb-controller-client 0.2.1` to `0.2.3`.
- Managed runtime installers now select `vldb-controller v0.2.3`.
- Direct SQLite runtime installers now select `vldb-sqlite v0.1.6`.
- `vldb-lancedb` remains on `v0.1.5`.

`vldb-sqlite 0.1.6` caches and reuses Jieba tokenizer instances for FTS operations and releases database-specific dictionary state with the final connection. The public controller protocol used by LuaSkills is unchanged, so hosts do not need an API migration.

### Upgrade checks

1. Update the core crate and the language SDK to `0.5.4`.
2. Refresh managed runtime assets so controller mode uses `v0.2.3` and direct SQLite mode uses `v0.1.6`.
3. Rebuild the host lock file to remove the older controller client.
4. Run the host's existing SQLite FTS and controller integration tests.

## 中文

LuaSkills `0.5.4` 是 `0.5.x` 版本线的依赖对齐补丁。它保持 `0.5.3` 的 Rust API、JSON FFI 请求结构、标准 C ABI V3 结构以及多语言 SDK 参数协议不变。

### 版本对齐

- Rust crate：`luaskills = "0.5.4"`
- TypeScript SDK：`@luaskills/sdk@0.5.4`
- Python SDK：`luaskills-sdk==0.5.4`
- Go module：`github.com/LuaSkills/luaskills-sdk-go@v0.5.4`
- FFI、demo 与调试工具资产：GitHub 标签 `v0.5.4`

### VLDB 调整

- Rust 依赖从 `vldb-controller-client 0.2.1` 升级至 `0.2.3`。
- 受管运行时安装器改为选择 `vldb-controller v0.2.3`。
- SQLite 直连运行时安装器改为选择 `vldb-sqlite v0.1.6`。
- `vldb-lancedb` 保持 `v0.1.5`。

`vldb-sqlite 0.1.6` 会在 FTS 操作中缓存并复用 Jieba 分词器实例，并在最后一个数据库连接关闭时释放数据库专属词典状态。LuaSkills 使用的 controller 公共协议没有变化，因此宿主不需要迁移 API。

### 升级检查

1. 将核心 crate 与语言 SDK 更新至 `0.5.4`。
2. 刷新受管运行时资产，确保 controller 模式使用 `v0.2.3`，SQLite 直连模式使用 `v0.1.6`。
3. 重新生成宿主锁文件，移除旧版 controller client。
4. 执行宿主现有的 SQLite FTS 与 controller 集成测试。
