# LuaSkills 0.5.3 Patch Upgrade Guide / 补丁升级说明

## English

LuaSkills `0.5.3` is the stable patch release for the `0.5.x` managed-runtime and host-integration line. It preserves the Rust API, JSON FFI request schemas, standard C ABI V3 structures, and language-SDK parameter contracts from `0.5.2`.

Upgrade every ecosystem component together:

- Rust crate: `luaskills = "0.5.3"`
- TypeScript SDK: `@luaskills/sdk@0.5.3`
- Python SDK: `luaskills-sdk==0.5.3`
- Go module: `github.com/LuaSkills/luaskills-sdk-go@v0.5.3`
- FFI, demo, and debug-tool assets: GitHub tag `v0.5.3`
- Lua runtime packages and native dependencies: latest stable `LuaSkills/luaskills-packages` release in the compatible `0.1` series

### Fixes

1. JSON objects, arrays, and Null now retain their source type across JSON → Lua → JSON round trips, including nested empty containers and Null values inside objects or arrays.
2. Lua code can explicitly construct typed containers with `vulcan.json.object()`, `vulcan.json.array()`, and `vulcan.json.null`. Each constructor also accepts one table and returns a shallow typed copy. An unmarked empty Lua table keeps the historical `[]` encoding.
3. Explicit JSON arrays reject string keys, mixed keys, non-positive indices, and holes. Explicit JSON objects reject every non-string key instead of silently discarding data.
4. Host Tool arguments, Runtime Lease eval results, RunLua results, System Plugin bridge values, JSON FFI results, and standard C ABI JSON results use the same container-type rules.
5. Windows host-visible path boundaries convert only valid verbatim drive paths and verbatim UNC paths. Mixed separators and unsupported namespaces such as Volume GUID, device, and pipe paths fail before module lookup, environment creation, or process creation.
6. Native canonical paths remain intact for filesystem identity checks and the private Python Worker/Session long-path channel, so the stricter host boundary does not weaken package, manifest, snapshot, cwd, or runtime isolation.

### Compatibility notes

- No FFI symbol, request field, callback signature, or standard C ABI structure changed in `0.5.3`.
- JSON decoded from an Object now encodes back as an Object even when empty. Hosts that previously worked around `{}` becoming `[]` should remove that workaround.
- Deliberately malformed explicitly typed Lua containers now return an error instead of producing a partially encoded value.
- Unsupported Windows verbatim namespaces are rejected with the stable error `unsupported Windows verbatim path namespace`; they are never passed to Lua module lookup or silently rewritten into relative paths.

### Verification

- Run `cargo fmt --all -- --check`, `cargo clippy --all-targets -- -D warnings`, and `cargo test --all-targets`.
- Confirm `{ "environment": {} }`, nested empty objects and arrays, and Null values remain structurally identical through RunLua, Runtime Lease, Host Tool, System Plugin, and FFI round trips.
- Confirm Windows drive and UNC verbatim paths normalize at host-visible boundaries while mixed or unsupported namespaces fail before resource creation.
- Confirm the GitHub Release contains 20 `.tar.gz` archives and 20 matching `.tar.gz.sha256` assets across Windows x64, Linux x64/ARM64, and macOS x64/ARM64.
- Install the published runtime through each language SDK and run every SDK Examples Release workflow after its package or module tag becomes available.

Published `0.5.2` artifacts remain immutable for registry provenance, but hosts should select `0.5.3` as the stable patch line.

## 中文

LuaSkills `0.5.3` 是 `0.5.x` 受管运行时与宿主集成功能线的稳定补丁版本。它保持 `0.5.2` 的 Rust API、JSON FFI 请求结构、标准 C ABI V3 结构和多语言 SDK 参数协议不变。

请同步升级全部生态组件：

- Rust crate：`luaskills = "0.5.3"`
- TypeScript SDK：`@luaskills/sdk@0.5.3`
- Python SDK：`luaskills-sdk==0.5.3`
- Go module：`github.com/LuaSkills/luaskills-sdk-go@v0.5.3`
- FFI、demo 与调试工具资产：GitHub 标签 `v0.5.3`
- Lua runtime packages 与原生依赖：兼容 `0.1` 协议线中最新稳定的 `LuaSkills/luaskills-packages` Release

### 修复内容

1. JSON Object、Array 与 Null 经 JSON → Lua → JSON 往返后保持来源类型，包括嵌套空容器、对象中的 Null 字段和数组中的 Null 元素。
2. Lua 可以使用 `vulcan.json.object()`、`vulcan.json.array()` 与 `vulcan.json.null` 显式构造类型化值。两个容器构造器也接受一个 table，并返回带类型标记的浅复制；未标记空 Lua table 仍按历史行为编码为 `[]`。
3. 显式 JSON Array 会拒绝字符串键、混合键、非正整数索引和数组空洞；显式 JSON Object 会拒绝所有非字符串键，不再静默丢弃数据。
4. Host Tool 参数、Runtime Lease eval 结果、RunLua 结果、System Plugin 固定桥、JSON FFI 与标准 C ABI JSON 出口使用同一套容器类型规则。
5. Windows 宿主可见路径边界只转换合法的 verbatim 盘符路径和 verbatim UNC 路径。混合分隔符以及 Volume GUID、设备、管道等不受支持的命名空间会在模块寻址、环境创建或进程创建前失败。
6. 文件对象身份校验和 Python 私有 Worker/Session 长路径通道继续保留原生规范路径，因此更严格的宿主边界不会削弱包、清单、快照、cwd 或运行时隔离。

### 兼容说明

- `0.5.3` 没有修改 FFI 导出符号、请求字段、回调签名或标准 C ABI 结构。
- 从 JSON Object 解码得到的值即使为空，再编码时也保持 Object。宿主若曾为 `{}` 变成 `[]` 增加临时兼容，应删除该兼容。
- 显式类型化 Lua 容器若结构非法，现在会返回错误，不再产生静默丢字段的部分结果。
- 不支持的 Windows verbatim 命名空间会返回稳定错误 `unsupported Windows verbatim path namespace`，不会进入 Lua 模块寻址，也不会被错误改写成相对路径。

### 验证要求

- 执行 `cargo fmt --all -- --check`、`cargo clippy --all-targets -- -D warnings` 与 `cargo test --all-targets`。
- 确认 `{ "environment": {} }`、嵌套空对象与空数组以及 Null 值经 RunLua、Runtime Lease、Host Tool、System Plugin 和 FFI 往返后结构完全一致。
- 确认 Windows verbatim 盘符路径与 UNC 路径会在宿主可见边界转换，混合或不支持的命名空间会在创建资源前失败。
- 确认 GitHub Release 在 Windows x64、Linux x64/ARM64、macOS x64/ARM64 上包含 20 个 `.tar.gz` 归档和 20 个一一对应的 `.tar.gz.sha256` 资产。
- 各语言 SDK 的包或 module tag 上游可见后，使用正式 SDK 安装 runtime 并运行每个 SDK 的 Examples Release 工作流。

已发布的 `0.5.2` 资产为保证注册表可追溯性保持不可变；宿主应选择 `0.5.3` 作为稳定补丁版本。
