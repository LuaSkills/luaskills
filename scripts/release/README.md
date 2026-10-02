# 核心候选构建与离线验收

此流程只交付候选，不执行 Cargo 或 SDK 发布。默认 `candidate` 模式只上传 GitHub Actions 产物。
只有显式选择 `draft`，且相同完整 SHA 的既有五平台原生验收、锁定构建及汇总全部通过，独立阶段才创建新的 GitHub 草稿。
既有 tag、release 或同名本地产物冲突均明确失败，不能覆盖。草稿上传中途失败时保留失败状态及已创建草稿，不能自动覆盖或重试写入。

按 tag 查询仅返回已公开发布，不能证明不存在未公开草稿。草稿前置检查先检查 tag/ref，再以已认证令牌完整分页读取
发布列表，按 `tag_name` 拒绝任何同名草稿或发布。列表返回未知结构、缺失字段或 API 失败均停止，不执行 POST。

GitHub 的 `GITHUB_TOKEN` 不能获得工作流写权限；来源提交相对实际默认分支修改 `.github/workflows/` 时，创建发布会失败。
因此 `draft` 模式在冻结阶段通过已认证只读 API 读取实际仓库的 `default_branch`、该分支完整 SHA、根树及工作流子树，
并与来源完整提交的实际 Git 工作流树比较。两者必须相同，否则明确要求先合并工作流改动，再以相同工作流树的来源提交运行草稿。
`candidate` 模式只交付 Actions 产物，允许工作流差异，不要求远端默认分支证据，也不引入 PAT 或新权限。

冻结产物 `draft-source-evidence.json` 包含 `schema_version`、`repository`、`source_commit`、`source_workflows_tree`、
`default_branch`、`default_commit`、`default_root_tree`、`default_workflows_tree`。
创建草稿前再次只读获取当前默认分支及其 SHA/树；默认 SHA 可以前进，但当前工作流树仍必须等于冻结来源树，
不能推定它们一致。当前证据独占写入 `draft-source-current-evidence.json`，与原始冻结证据共同上传；缺证据、差异或 API 失败均在任何 POST 前停止。
默认分支名称来自实际 API，不假定为 `main`。依据：[GitHub 创建发布与权限规则](https://docs.github.com/en/rest/releases/releases#create-a-release)。

两个草稿证据资产名唯一由 `create_draft.DRAFT_SOURCE_EVIDENCE_ASSETS` 声明；CLI 不允许调用方改变冻结证据的 basename。
正式公开 Release 的精确集合为既有 48 项候选资产加这两个证据资产，共 50 项；共享 SDK 前置门禁直接导入该声明。
公开发布后的证据验收入口是 `verify_published_source_evidence(evidence, commit, repository, base, token=None, *, lookup=None)`：
按公开来源完整提交及证据中的历史 `default_commit` 读取不可变 commit/root/.github/workflows 对象，核对字段归属和树摘要。
它不查询发布之后的当前默认分支；`default_branch` 是当时捕获的标签，不推测历史默认分支名称。
`lookup` 可以注入公共门禁已有的受检只读 HTTP 客户端，不能跳过对象身份验证，也不会额外创建 `Bearer None` 请求。

`source_sha` 必须是包含本流程修改的最终完整提交，并与工作流触发提交完全相等；`version` 必须等于该提交的
`Cargo.toml` 中 `package.version`。脚本不会把当前脏工作树冒充为 HEAD 对应源码。本地功能通过仅证明当前字节；发布候选仍须以实际干净的完整提交及对应构建证据为准。

冻结阶段在完整提交及包版本核验通过后，复用既有 Python 环境，依次运行 `test_candidate.py`、
`test_sdk_prerequisites.py`、`test_sdk_recovery.py` 三个现有离线回归入口；任一失败均使冻结作业失败，
阻止依赖它的原生验收、候选构建、汇总与草稿创建。脚本与工作流来自同一个已核验完整 SHA。
五平台原生验收在既有 `cargo test --locked --all-targets -j 4 -- --test-threads=1` 之后，独立执行
`cargo test --locked --doc -j 4 -- --test-threads=1`，覆盖前者不运行的文档测试，并保留编译并发 4、共享测试串行的限制。
这两项仅补齐既有候选 CI 的前置检查，不执行 SDK 发布，也不改变候选资产、草稿权限或性能门槛。

## 构建与打包输入

在固定提交的干净源码中执行；捕获命令使用 PowerShell 7 的 UTF-8 输出，所有输出保持在忽略的 `target/` 下：

```powershell
rtk proxy cargo build --locked --release --lib --bins -j 4 --message-format=json > target/candidate-build.jsonl
rtk proxy cargo metadata --locked --format-version=1 > target/candidate-metadata.json
rtk proxy cargo --version --verbose > target/candidate-cargo-version.txt
rtk proxy powershell -NoProfile -File scripts/build/package_ffi_sdk.ps1 -Platform windows-x64 -OutputDir target/release-packages -SourceCommit <完整提交> -Version <实际包版本> -BuildLog target/candidate-build.jsonl -Metadata target/candidate-metadata.json -CargoVersion target/candidate-cargo-version.txt
```

Unix 包装器采用显式选项：`package_ffi_sdk.sh --platform <平台> --output <目录> --source-commit <完整提交> --version <实际包版本> --build-log <Cargo日志> --metadata <Cargo元数据> --cargo-version <实际Cargo详细版本输出>`。
PowerShell 的 `-DryRun` 与 Python 的 `--dry-run` 执行相同输入及原生身份检查，只打印清单，不写候选文件。
PowerShell 5.1 包装器仍可执行，但版本捕获文件必须为 UTF-8 无 BOM，不能直接使用其默认 UTF-16 重定向结果。

Cargo JSON 日志的 `compiler-artifact.manifest_path` 确认根包归属，`build-script-executed.out_dir` 指定唯一报告；不扫描旧构建目录。
`luaskills_ffi_embedded_describe_v1` 读取实际刚构建库的借用型描述，不创建运行时，不释放该借用缓冲。
描述的 `build` 与 `build.rs` 原始报告、源码根完整文件映射、契约及锁文件精确字节必须一致。

FFI 归档保留既有 `include/`、`lib/` 和 `ffi-sdk-manifest.json` 布局，并新增：

- `contracts/embedded/v1/`：实际离线契约及相关原始文件。
- `embedded-build-inputs.json`：本次构建脚本报告的原始字节。
- `embedded-core-description.json`：实际库读取描述的原始字节。
- `cargo-version.txt`：实际发布构建 Cargo 可执行文件的 `--version --verbose` 原始 UTF-8 字节。
- `licenses/LICENSE`、`licenses/THIRD_PARTY_NOTICES.md`、`licenses/Cargo.lock`：源码中现有材料。
- `licenses/dependencies.json` 及 `licenses/dependencies/`：实际 Cargo 元数据许可表达式与实际依赖源码文件；未找到许可文件时记录空列表，不编造授权或宣称完整许可审计。

同平台 FFI demo 库字节必须与权威 FFI 归档一致；Rust demo 依赖使用冻结提交的 `rev`；demo/debug-tool
现有清单的 `platform`、`release_tag` 及实际可执行文件均复核。源码另以 `git archive` 冻结为包含完整提交的
`luaskills-source-<版本>-<完整提交>.tar.gz`。这是精确 Git 源码归档，不宣称等同于未来 Cargo 规范化的 `.crate` 包。

## 唯一候选清单定义

`candidate.py` 中 `MANIFEST_VERSION`、`PLATFORMS`、`ARCHIVE_FAMILIES` 是发布流程的唯一声明。
运行时构建字段仍直接来自 `build.rs` / `EmbeddedBuildIdentity`；源码根和源码摘要域版本从实际冻结的
`build_support/identity.rs` 读取，不能另维护根列表。

`ffi-sdk-manifest.json` 的候选字段：`schema_version`、`package_name`、`platform`、`core_version`、`source_commit`、
`source_archive_sha256`、`headers`、`library_dir`、`build`、`cargo`、`files`。`files` 映射归档内每个其他文件的精确 SHA-256。

每平台 `candidate-<平台>.json`：`schema_version`、`source_commit`、`core_version`、`platform`、`source_archive`、
`build`、`cargo`、`archives`。`source_archive` 包含 `name` / `sha256`；`archives` 映射四个现有归档名称到 SHA-256。

`candidate.CARGO_VERSION_EVIDENCE_FILE` 是 Cargo 证据文件唯一名称；`candidate.cargo_identity(content)` 解析唯一发布身份结构：
`release`、`commit_hash`、`host`、`version_verbose_sha256`、`version_verbose`。完整文本重新编码必须得到原始 UTF-8 字节，摘要不归一化换行。
五平台归档和记录必须匹配同一真实 Cargo `release` 与 `commit_hash`；`host` 必须匹配各平台 native target，宿主、OS 及库详细行允许真实差异。
这份证据独立于 Rust 的 `EmbeddedBuildIdentity`，不能从 `rustc` 版本或工作流工具链名称推断 Cargo 身份。
后续 crates 发布验收按当前实际宿主选择对应平台记录，要求当前 Cargo 的真实版本及提交摘要一致，再由 Cargo 自身规范化冻结源码；
不能声称手工归一化的 manifest 等于实际 Cargo `.crate` 内容。本流程不执行该后续阶段。

汇总 `candidate-manifest.json`：`schema_version`、`source_commit`、`core_version`、`source_archive`、`platforms`。
`platforms` 保存全部五个平台记录，不以数组下标表示身份。构建报告摘要随目标编译器/平台变化；共享源码、契约、锁文件及源码归档摘要必须跨平台一致。

## SDK 消费接口

仅从已汇总验收的候选目录派生独立 SDK 验证输入：

```powershell
rtk proxy python scripts/release/candidate.py sdk-inputs --input target/verified-candidate --output target/sdk-validation-inputs/windows-x64 --platform windows-x64 --source-commit <完整提交> --version <实际包版本>
```

输出实际动态库、原始 `core-description.json` 与 `sdk-validation-inputs.json`。后者包含：
`schema_version`、`source_commit`、`core_version`、`platform`、`library`（绝对路径）、`library_sha256`、
`description`（绝对路径）、`description_sha256`、`archive_sha256`、`build`。
三个 SDK 可将 `library`、`library_sha256`、`description` 分别交给统一门禁的 `--library`、`--library-sha256`、`--description`；
必须比较实际描述完整对象，不把局部版本号或报告摘要当成完整兼容验收。
派生命令复核 FFI 归档、逐文件摘要、完整描述与报告、冻结源码身份；不会发布 SDK。

## 本地验证边界

```powershell
rtk proxy python -X utf8 -m unittest discover -s scripts/release -p test_candidate.py -v
rtk proxy python -X utf8 -m unittest discover -s scripts/release -p test_sdk_prerequisites.py -v
rtk proxy python -X utf8 -m unittest discover -s scripts/release -p test_sdk_recovery.py -v
rtk proxy python -X utf8 -m py_compile scripts/release/candidate.py scripts/release/create_draft.py scripts/release/freeze_draft_source.py scripts/release/test_candidate.py
```

离线测试使用真实冻结源码归档与明确合成的库字节，运行真实临时 tar 写入、sidecar、汇总和 SDK 输入派生。
Windows 另外实际运行临时目录内的 PowerShell Rust demo 打包器。工作流历史场景在独立临时仓库内创建真实提交并验证祖先及树身份；
不修改原仓库的引用、索引、配置或源码。测试不运行 Cargo、不触发远端工作流、不写 release、不执行 `cargo publish`，也不推送 Git。
远端五平台真实构建与草稿 API 属于单独发布阶段；离线通过不能代替它们。
