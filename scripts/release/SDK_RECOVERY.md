# SDK 双证明恢复公共底座

本文件及 `sdk_recovery.py` 只定义精确轮次、制品下载、签名绑定与完整清单的公共规则。它们不执行 Cargo、registry、Release、工作流重试或上传操作，也不自行实现加密验签。SDK 工作流、包结构、生产环境、发布意图及完成证明由各 SDK 保持唯一权威。

## 冻结边界

新增恢复证据使用 `schema_version: 2`，不要求已有原生矩阵或包证据的 Schema 同步升级。候选身份与完成身份必须分开，不得把原失败运行改写为成功。

原候选可来自明确的 `artifact-only` 运行。它证明已测试的不可变字节，不授权发布。当前明确的发布或恢复操作可以消费该原候选；必须保留原模式、实际状态和签名。默认 `artifact-only` 不得写 registry 或公共 Release。完成方必须自行验证当前生产环境、官方 OIDC、源码与实际标签、核心完整发布前提、冷 registry 消费及实际主 Release，并在独立签名完成证明中记录 `completion_intent`。

初版完成源码必须等于原固定 SDK 源码，但保留独立 `completion_source_sha` 字段。独立完成 Release 使用 `recovery-v{version}-r{completion_run_id}-a{completion_run_attempt}`。完成证明绑定实际主 Release ID、实际标签及源码，精确原 candidate artifact ID、原清单和原官方 bundle 摘要、完整包清单与新鲜消费证据；这些 SDK 专用关系不由公共底座猜测。

## 精确轮次接口

```python
verify_attempt(http, *, repository, workflow_path, source_sha,
               run_id, run_attempt, required_jobs, phase)
```

`http` 采用现有 `sdk_prerequisites.Http` 的只读 `json(url)` 和 `get(url, binary=True)` 接口。`source_sha` 必须是完整小写 40 位提交；`required_jobs` 必须从冻结 SDK 工作流的唯一声明派生，为非空、无重复的精确作业名称列表。公共底座不猜测矩阵 UI 名称。

只查询 `/repos/{repository}/actions/runs/{run_id}/attempts/{run_attempt}` 及该精确轮次下 `/jobs?per_page=100&page=N`，不查询最新运行。逐页总数必须一致，每个作业 ID 唯一，每个必需名称精确出现一次且状态为 `completed/success`。源码、所属仓库、源仓库、工作流物理路径及运行、轮次必须匹配。作业接口没有被虚构的 `run_attempt` 字段；轮次归属由精确轮次端点建立。

- `phase="candidate"`：允许实际 `completed` 的非空结果，包括门禁完成后发布失败；也允许实际 `in_progress/conclusion=null`，用于同一工作流在候选门禁完成后执行发布。实际状态与结果原样返回，绝不合成整体成功。排队、等待或不一致状态拒绝。仍必须另行通过原官方签名绑定及完整清单。
- `phase="completion"`：指定轮次整体必须为 `completed/success`，指定发布作业也必须成功。仅在整轮结束后供外部消费认证；运行中的发布作业不能用它认证自身已完成。

候选证据作业内部签名时，不调用要求该作业自身已经成功的外部 `verify_attempt`。当前生产发布作业等待候选证据作业结束后，才调用候选认证。

返回 JSON 的精确顶层字段为 `schema_version, phase, repository, workflow_path, source_sha, run_id, run_attempt, invocation_uri, attempt, required_jobs`；`attempt` 和所选作业保留 API 实际对象及状态。SDK 冻结的事件、分支、展示标题和具体发布意图，由 SDK 对这些实际对象继续校验。

只读 CLI：

```text
python scripts/release/sdk_recovery.py attempt --repository LuaSkills/example-sdk --workflow-path .github/workflows/sdk-release.yml --source-sha FULL_COMMIT --run-id 123 --run-attempt 2 --required-job aggregate --required-job native-windows --required-job candidate-evidence --phase candidate
```

CLI 输出实际 JSON，没有网络写入选项。

## 两阶段制品读取与认证

第一阶段：

```python
download_artifact(http, *, repository, source_sha, run_id,
                  artifact_id, artifact_name, max_unpacked_bytes=MAX_ARTIFACT_BYTES)
```

制品 ID 必须来自实际上传输出或恢复操作显式输入，不通过名称候选扫描、时间戳或最新轮次推断。API ID、名称、原运行及源码、未过期状态、实际 ZIP SHA-256 与下载大小逐项验证。ZIP 只接受精确根普通文件，拒绝目录、链接、设备、重复及大小写别名、目录穿越、嵌套路径和超限展开。读取不解压到磁盘。复用已有 HTTP、JSON 重复键拒绝和哈希实现；已有源归档读取器允许嵌套前缀且不预先限制展开，故此处使用针对根证据文件的有界 ZIP 读取器。

返回二元组 `(evidence, files)`。`files` 为根文件名到精确 `bytes` 的字典。`evidence` 的字段为 `schema_version, repository, source_sha, run_id, artifact, archive_sha256, inventory, attempt_bound`，其中 `artifact` 是实际 API 对象，`attempt_bound` 固定为 `false`。这一步不认证原轮次。

第二阶段需要独立、直接成为官方 attestation subject 的 `recovery-binding.json`：

```json
{
  "schema_version": 2,
  "kind": "sdk-candidate",
  "repository": "LuaSkills/example-sdk",
  "workflow_path": ".github/workflows/sdk-release.yml",
  "source_sha": "1111111111111111111111111111111111111111",
  "run_id": 123,
  "run_attempt": 2,
  "artifact_name": "candidate-evidence-r123-a2",
  "inventory": [{"filename": "sdk-package.tgz", "size": 1, "sha256": "0000000000000000000000000000000000000000000000000000000000000000"}]
}
```

示例摘要仅表示字段形式。真实清单必须使用全部包、原生矩阵报告及日志、核心前提和原聚合证据的实际字节。`inventory` 严格只接受唯一 `filename,size,sha256`，不含绑定文件自身或随后生成的官方签名 bundle，从而避免循环自哈希。名称算法唯一来源为 `candidate_artifact_name(run_id, run_attempt)`。

SDK 必须对下载制品中的实际官方 bundle 使用已有 `gh attestation verify` 验证器，严格验证预期仓库、工作流、提交、实际签名调用及全部 SDK 必需 subject。成功后把真实已验证输出归一化为：

- `verified_subjects`：根 subject 名称到完整小写 SHA-256 的字典；不得由待验证清单反向生成。
- `verified_invocation_uri`：已验证调用的实际 `https://github.com/{repository}/actions/runs/{run_id}/attempts/{run_attempt}`，不得由输入或元数据自声明替代。
- `verified_source_sha`：已验证 signer 源码完整提交，不能从待验证清单抄写。

公共接口：

```python
verify_signed_binding(binding_bytes, *, verified_subjects,
                      verified_invocation_uri, verified_source_sha,
                      repository, workflow_path, source_sha,
                      run_id, run_attempt, artifact_name)
```

必须传下载的原始 `recovery-binding.json` 字节。官方 subject 必须直接覆盖这些原始字节的摘要，并绑定原指定轮次与源码。绑定结构严格拒绝未知字段、重复 JSON 键、旧 Schema 或不同身份。该函数不执行验签；输入 `verified_*` 的信任来源始终是 SDK 的官方验证器，传入任意自声明值不能作为发布授权。

返回已验证包装 JSON：`schema_version, binding, binding_filename, binding_sha256, verified_invocation_uri, verified_source_sha`。随后调用：

```python
verify_inventory(verified_binding, files, *, attestation_filename)
```

`attestation_filename` 必须是冻结 SDK 工作流声明的官方 bundle 根正名。全部物理文件必须恰好等于签名清单成员、原始绑定文件和该 bundle，不允许漏项或额外文件；每个清单文件的实际名称、大小及 SHA 必须相等。返回规范排序的已验证载荷清单。包结构检查及官方 bundle 验证仍由 SDK 执行。任一原制品缺失、过期、签名错误或清单差异都必须在任何 registry/Release 写入前失败，不得重建候选、转用最新轮次或绕过完整原清单。

## 验证

```text
python -X utf8 scripts/release/test_sdk_recovery.py
```

测试采用内存只读 HTTP 与真实 ZIP，不运行 Cargo、不连接网络、不执行远程写入。覆盖旧轮次与最新轮次相反状态、候选整体失败及运行中状态、各必需门禁失败或缺失、重复分页、外来源码及仓库、完成整体失败、制品 ID、过期、摘要、大小、缺失及异常元数据、ZIP 路径与链接、展开上限、官方归一化签名身份绑定及完整清单任意差异。

接口依据：[工作流轮次 API](https://docs.github.com/en/rest/actions/workflow-runs)、[轮次作业 API](https://docs.github.com/en/rest/actions/workflow-jobs)、[制品 API](https://docs.github.com/en/rest/actions/artifacts)、[Windows 可移植文件命名约束](https://learn.microsoft.com/en-us/windows/win32/fileio/naming-a-file)。原签名验证以各 SDK 当前官方验证器为权威，公共底座不复制官方返回形状或加密实现。
