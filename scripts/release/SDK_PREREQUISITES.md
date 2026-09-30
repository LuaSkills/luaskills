# SDK 正式发布公共前置门禁

三个 SDK 必须使用同一个冻结核心提交中的本脚本及相邻 `candidate.py`、`create_draft.py`；不得复制平台、归档家族、清单版本或草稿证据资产定义。权威集合只来自 `candidate.MANIFEST_VERSION`、`candidate.PLATFORMS`、`candidate.ARCHIVE_FAMILIES`、`create_draft.DRAFT_SOURCE_EVIDENCE_ASSETS`。不修改核心版本、锁文件或运行时源码。

## 冻结 CLI 与 Python 调用契约

```text
python scripts/release/sdk_prerequisites.py --core-tag vX.Y.Z --core-commit <40位小写SHA> --output <不存在的新目录> [--phase complete|github-only]
python scripts/release/sdk_prerequisites.py recheck --input <旧prerequisites.json> --output <不存在的新目录>
python scripts/release/sdk_prerequisites.py sdk-inputs --input <传输产物根/prerequisites.json> --platform <核心平台键> --output <不存在的新JSON文件>
python scripts/release/sdk_prerequisites.py toolchain-inputs --input <github-only产物根/prerequisites.json> --platform <核心平台键> --output <不存在的新JSON文件>
```

默认 `complete`。`github-only` 仅验收 GitHub 正式发布，永远输出 `complete=false`，不能授权 SDK 发布。`complete` 必须在线验收 crates.io 精确非撤销版本、包摘要及源码，使用已证明的发布 Cargo 重新规范化完整冻结源码清单，再创建全新 registry 消费工程并真实运行 Cargo 构建及运行验收。不得接收调用方提供的“成功日志”。`recheck` 在 SDK 发布前重新读取当前 Release、标签及全部正式资产字节，重新规范化及消费 registry；旧报告只提供精确身份与期望资产信息，不作为当前成功证明。

Python 入口 `run_gate(core_tag, core_commit, output, phase="complete") -> dict`、`recheck(input_path, output) -> dict`、`resolve_sdk_inputs(prerequisites_path, platform) -> dict` 与 `toolchain_inputs(prerequisites_path, platform) -> dict`；SDK 直接导入或运行公共入口。公共脚本要求 Python 3.11 或更新版本；Python SDK 最低版本 3.10 的作业先单独使用 3.11 或更新 Python 执行公共 `sdk-inputs` 命令输出 JSON，再用 3.10 消费 JSON，避免导入 `tomllib`。注入 HTTP/执行器仅用于单元测试，正式 CLI 不提供离线、跳过或伪造证明参数。执行器直接使用参数数组执行 `cargo`，无 `shell=True`、无 RTK 运行依赖；`generate-lockfile` 后执行 `metadata --locked`、`build --locked -j 4 --message-format=json`，再直接运行本次 Cargo JSON 确认的可执行文件。环境固定 `CARGO_BUILD_JOBS=4`、`RUST_TEST_THREADS=1`，每命令限定 1800 秒，Rust 调用及清理另有秒级截止时间；失败关闭门禁。

每个新 runner（包括发布前 `recheck` 的新作业及示例的独立正式消费作业）先运行 `github-only`，再运行 `toolchain-inputs`，以输出的 `toolchain` 执行 `rustup toolchain install <toolchain> --profile minimal`，最后运行 `complete` 或 `recheck`。`toolchain-inputs` 仅接受 `phase="github-only"`、`complete=false`、`registry=null` 输入，校验固定产物路径、原始正式清单及全部资产摘要，再通过完整公共 `github-only` 门禁重新核对当前 Release、标签、历史工作流证据及所有源码成员；自声明 JSON 不能决定安装值。`--platform` 使用 `candidate.PLATFORMS` 的精确宿主键；当前 Ubuntu x64 作业对应声明中的 `linux-x64`，不能把待验收的其他矩阵平台当作构建宿主。SDK 不复制版本常量或平台映射。

工具链 JSON 精确字段为 `schema_version`、`phase="github-only"`、`complete=false`、`core_tag`、`core_commit`、`core_version`、`platform`、`toolchain`、`rustc`、`cargo`、`github`。`toolchain` 是全部已验证平台记录一致的实际 `rustc.release`，严格匹配稳定版本 `数字.数字.数字`；`rustc` 包含 `release`、`commit_hash`、`host`、`version_verbose`、`version_verbose_sha256`，`cargo` 完整引用 `candidate.cargo_identity` 验证的实际 Cargo 字段，`github` 是本次重新核验的正式快照。此输出不授权 SDK 发布，不包含 registry 消费证明，也不由最低 `rust-version` 推算 Cargo。

跨 Actions artifact job 时，生产者报告中的绝对路径保持原始证据，不能直接作为消费路径。公共 resolver 仅读取报告父目录下固定 `sdk-inputs/<platform>/<candidate.PLATFORMS[platform][3][0]>`、`core-description.json` 及 `sdk-validation-inputs.json`；要求报告映射与逐平台 JSON 精确一致，复验实际正式清单、源码及归档摘要，通过 `candidate.sdk_inputs` 重新核实完整 build、contract、source 与包锁文件，再返回当前机器的精确绝对路径。拒绝符号链接逃逸、候选扫描及静默回退；三个 SDK 不得自行复制重映射算法。

## 冻结输出

输出目录只可新建；所有文件独占写入。失败不产生 `prerequisites.json`。

- `downloads/candidate-manifest.json`：Release API 按资产 ID 下载的原始正式清单；`downloads/assets/` 保存每平台记录、四归档及各 sidecar、冻结源码及 sidecar、两份由 `create_draft.DRAFT_SOURCE_EVIDENCE_ASSETS` 声明的草稿源码证据（当前正式集合合计 50 项）。分开存放确保 `candidate.aggregate` 只读取逐平台记录，避免其 `candidate-*.json` glob 将总清单识别为额外平台记录。`downloads/official-commit.zip` 独立保存官方精确提交源码，对照全部冻结源码成员而不只信任可伪造的 git archive 注释。
- `candidate/`：调用 `candidate.aggregate` 重新验收、重新生成的候选；必须与正式 candidate-manifest 语义完全相同。
- `sdk-inputs/<platform>/sdk-validation-inputs.json`：调用 `candidate.sdk_inputs` 输出，伴随精确动态库及 `core-description.json`。平台键直接派生自唯一平台声明。
- `registry/`：完整阶段的 `.crate`、registry API 原始响应、消费工程源码及锁文件、实际消费者二进制与真实命令日志。消费者在仓库外的新临时工作区和独立 Cargo home 构建，避免仓库或用户 registry 替换；临时编译缓存在验收后回收。
- `prerequisites.json`：`schema_version`（引用 candidate 的版本）、`phase`、`complete`、`core_tag`、`core_commit`、`core_version`、`checked_at`、`github`、`registry`、`sdk_inputs`。

`github` 包含 `repository="LuaSkills/luaskills"`、`release_id`、`tag_name`、`published_at`、`tag_resolution`（Git ref及逐层 annotated tag 的 SHA/type）、`commit_archive_sha256`、`commit_source_sha256`、`draft_source_evidence`、`assets`（按资产名映射，包含 `id`、`size`、`sha256`、`url`、`updated_at`）。动态 GitHub zipball 的压缩字节摘要 `commit_archive_sha256` 仅用于本次审计；跨次重查比较所有已验证成员路径及内容的 `commit_source_sha256`，允许合法重压缩，但不允许成员字节改变。`draft_source_evidence` 按唯一资产名映射已通过共享验证器的历史证据对象。`registry` 在 GitHub 阶段为 `null`；完整阶段包含 `name`、`version`、`checksum`、`vcs_commit`、`source_sha256`、`manifest_normalization`、`consumer`。

`consumer` 精确字段为 `commands`（四项 `{command:[参数字符串],exit_code:0,stdout_sha256,stderr_sha256}`，顺序为 generate-lockfile、metadata、build、实际二进制）、`executable`、`executable_sha256`、`cargo_lock_sha256`、`package_id`、`source`、`result`。`result` 为 `{challenge:<本次随机64位hex>,runtime:true,pool_reuse:true,capability_calls:2,drained:true}`；布尔字段必须是真布尔值。日志存放 `registry/{lock,metadata,build,runtime}.{stdout,stderr}`；消费者文件在 `registry/consumer/`。随机挑战与本次实际编译产物路径一起拒绝旧输出及粘贴的成功文字。

`sdk_inputs` 按平台映射到 `candidate.sdk_inputs` 的完整结果（`library`、`library_sha256`、`description`、`description_sha256`、`archive_sha256`、`build`、版本及提交）。SDK 可用这些字段直接执行本平台 ABI 与描述验收。

## 身份与失败边界

仅接受 `LuaSkills/luaskills` 非草稿、非预发行正式 Release。标签递归解析到精确提交，`target_commitish` 不构成证明。分页资产名称及 ID 必须唯一，所需资产不得缺失；下载摘要、sidecar、内嵌清单、描述、构建、源码、契约及 package-lock 统一复验。两份草稿源码证据通过 `create_draft.verify_published_source_evidence`，按历史 `source_commit` 与捕获的 `default_commit` 查询不可变 Git 对象，核对 root、`.github/workflows` 与仓库归属；发布后默认分支可以前进，不要求它仍等于历史快照。两份正式证据的资产 ID、原字节摘要及对象身份均参加独立重查。验证后再读取 Release、标签与资产列表，漂移失败。

crates.io 按精确版本读取 `version` 对象；必须非撤销，下载包 SHA-256 等于 API checksum，`.cargo_vcs_info.json` 的提交等于冻结提交。包源码身份输入与 GitHub 冻结源码字节完全一致；Cargo.toml.orig 必须等于冻结原清单。

实际规范化 Cargo.toml 的所有字段必须与官方 Cargo 对完整冻结快照执行 `cargo package --locked --no-verify -j 4` 的输出语义相同，不能只比较 name/version，也不能维护 Python 猜测版规范化器。该命令不编译或发布。FFI 包的 `candidate.CARGO_VERSION_EVIDENCE_FILE` 原始 UTF-8 详细输出，经 `candidate.cargo_identity` 验证并记入平台记录 `cargo`；五平台 release/commit_hash 必须相同，host 及操作系统相关详细行允许不同。实际规范化进程分别运行 `rustc --version --verbose` 与 `cargo --version --verbose`，按真实 host 选择精确平台记录，核对 Cargo 的 release/commit_hash 与编译器 release/commit_hash，并要求实际编译器满足冻结 Cargo.toml 的 `rust-version`。所有冻结清单文本显式按 UTF-8 读取。缺失旧记录 Cargo 身份时正式门禁失败；不会由 rustc 版本推测 Cargo，门禁本身不会安装工具链。

规范化与消费者共用唯一受控环境：保留已有 `RUSTUP_HOME` 作为只读工具链存储，删除调用方 `RUSTUP_TOOLCHAIN`、其他 `RUST*` 编译覆盖及 `CARGO*` 来源覆盖，重新设置为已验证发布编译器稳定版本的 `RUSTUP_TOOLCHAIN`。全部平台编译器 release/commit 必须一致，host 必须逐一匹配唯一平台声明；规范化实际探测确认该选择，消费者复用本次已确认的版本。不会依赖 runner 的默认 stable 或调用方默认工具链。

`registry.manifest_normalization` 保存 `commands`（actual rustc、actual cargo、Cargo package 三项真实命令与日志摘要）、`cargo`、`rustc`、`rust_version_minimum`、`normalized_manifest_sha256`、`registry_manifest_sha256`；日志为 `registry/normalization-{rustc,cargo,package}.{stdout,stderr}`，实际官方规范化清单另存 `registry/official-normalized-Cargo.toml`。消费工程仅允许精确 registry 依赖，不允许 patch/path/git 替换，Cargo metadata 必须确认实际 registry 来源及包 checksum。消费程序通过实际公开 runtime、pool 和 capability API 执行 Lua 到 Rust 回调往返，检查结果与资源释放。

正式发布必须先发布核心 GitHub 与 crates.io，再运行完整门禁，再运行各 SDK 门禁，紧邻 SDK 发布前运行公共 `recheck`。HTTP 查询成功或旧 JSON 的 `complete=true` 均不等于实际消费成功。

## 官方协议依据

- [GitHub Release 与分页资产 API](https://docs.github.com/en/rest/releases/releases)
- [GitHub Git refs API](https://docs.github.com/en/rest/git/refs)
- [GitHub annotated tag API](https://docs.github.com/en/rest/git/tags)
- [Cargo registry 包格式与打包](https://doc.rust-lang.org/cargo/commands/cargo-package.html)
- [crates.io 官方版本 API 实现](https://github.com/rust-lang/crates.io/tree/main/crates/crates_io/src/controllers)
