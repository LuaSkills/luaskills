# LuaSkills 0.5.2 Patch Upgrade Guide / 补丁升级说明

## English

LuaSkills `0.5.2` is the stable patch release for the `0.5.1` managed-runtime host-root and B3-B7 feature line. It does not change the Rust, JSON FFI, standard C ABI V3, or language-SDK parameter contracts introduced in `0.5.1`.

Upgrade every ecosystem component together:

- Rust crate: `luaskills = "0.5.2"`
- TypeScript SDK: `@luaskills/sdk@0.5.2`
- Python SDK: `luaskills-sdk==0.5.2`
- Go module: `github.com/LuaSkills/luaskills-sdk-go@v0.5.2`
- FFI, demo, and debug-tool assets: GitHub tag `v0.5.2`

### Fixes

1. Windows System runtime leases retain the native filesystem identity for validation but remove only the `\\?\` verbatim spelling before changing the process working directory. A child opened without an explicit `cwd` therefore inherits the authorized package or workspace directory instead of letting `cmd.exe` fall back to the Windows directory.
2. Every `luaskills-*.tar.gz` GitHub Release asset has a same-name `.sha256` sidecar. The release workflow requires exactly four archives per platform, writes BOM-free sha256sum-compatible sidecars, and fails before upload if hashing or inventory validation fails.
3. The Go SDK runtime-lease example now supplies the required `SystemRuntimePackage`, uses the exact package root as `cwd`, and ships `system_lua_lib/runtime-lease-example/dependencies.json` in its fixture.

### Verification

- Run `cargo test --all-targets` on Windows, Linux x64/ARM64, and macOS x64/ARM64 with the exact managed Python and Node.js assets.
- Confirm the GitHub Release contains 20 `.tar.gz` archives and 20 matching `.tar.gz.sha256` assets.
- Install runtime assets using the published Python or TypeScript SDK, then run the published Python, TypeScript, and Go example workflows.
- On Windows, verify the runtime-lease child process reports the System package root as its current directory and produces no `UNC paths are not supported` diagnostic.

`0.5.1` remains immutable for registry provenance, but hosts should select `0.5.2` as the stable patch line.

## 中文

LuaSkills `0.5.2` 是 `0.5.1` 受管运行时宿主根与 B3-B7 功能线的稳定补丁版本。它不改变 `0.5.1` 已引入的 Rust、JSON FFI、标准 C ABI V3 或多语言 SDK 参数协议。

请同步升级全部生态组件：

- Rust crate：`luaskills = "0.5.2"`
- TypeScript SDK：`@luaskills/sdk@0.5.2`
- Python SDK：`luaskills-sdk==0.5.2`
- Go module：`github.com/LuaSkills/luaskills-sdk-go@v0.5.2`
- FFI、demo 与调试工具资产：GitHub 标签 `v0.5.2`

### 修复内容

1. Windows System runtime lease 继续使用原生文件对象身份完成校验，但在切换进程工作目录前只移除 `\\?\` verbatim 写法。未显式指定 `cwd` 的子进程会继承已授权包目录或工作区目录，不再让 `cmd.exe` 退回 Windows 目录。
2. 每个 `luaskills-*.tar.gz` GitHub Release 资产都有同名 `.sha256` sidecar。发布工作流强制每个平台恰好生成四个归档，写入无 BOM、兼容 sha256sum 的 sidecar，并在哈希或资产数量校验失败时阻止上传。
3. Go SDK runtime-lease 示例会传入必需的 `SystemRuntimePackage`，把精确包根作为 `cwd`，并在夹具中包含 `system_lua_lib/runtime-lease-example/dependencies.json`。

### 验证要求

- 在 Windows、Linux x64/ARM64、macOS x64/ARM64 上使用精确受管 Python/Node.js 资产运行 `cargo test --all-targets`。
- 确认 GitHub Release 包含 20 个 `.tar.gz` 归档和 20 个一一对应的 `.tar.gz.sha256` 资产。
- 使用正式 Python 或 TypeScript SDK 安装 runtime 资产，再运行正式 Python、TypeScript 与 Go 示例工作流。
- 在 Windows 上确认 runtime-lease 子进程返回 System 包根作为当前目录，且 stderr 不出现 `UNC paths are not supported`。

注册表中的 `0.5.1` 为保证供应链可追溯性保持不可变；宿主应选择 `0.5.2` 作为稳定补丁版本。
