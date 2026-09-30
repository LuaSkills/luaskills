"""Exercise exact recovery attempts, official-signature bindings and real ZIP byte boundaries offline.
离线验证精确恢复轮次、官方签名绑定及真实 ZIP 字节边界。
"""

import copy
import io
import unittest
import warnings
import zipfile

import candidate
import sdk_recovery as recovery


class MemoryHttp:
    """Provide exact read-only HTTP fixture responses and record endpoint ownership.
    提供精确只读 HTTP 夹具响应，并记录端点归属。
    """

    def __init__(self, responses):
        """Store URL-to-value responses and an empty call log; return a fixture client.
        保存 URL 到值的 responses 及空调用日志；返回夹具客户端。
        """
        # Responses are deliberately mutable to model wrong-source and partial evidence.
        # 响应刻意保持可变，以模拟错误源码及局部证据。
        self.responses = responses
        # Calls prove that an implementation never consulted latest-run metadata.
        # 调用记录证明实现绝未查询最新运行元数据。
        self.calls = []

    def json(self, url):
        """Read an exact JSON URL from responses; return a detached fixture object.
        从 responses 读取精确 JSON URL；返回独立夹具对象。
        """
        self.calls.append((url, False))
        if url not in self.responses:
            raise ValueError("Fixture HTTP 404")
        return copy.deepcopy(self.responses[url])

    def get(self, url, binary=False):
        """Read an exact binary URL with media flag; return fixture bytes and empty headers.
        按媒体标志读取精确二进制 URL；返回夹具字节及空响应头。
        """
        self.calls.append((url, binary))
        if url not in self.responses:
            raise ValueError("Fixture HTTP 404")
        return self.responses[url], {}


def zip_bytes(files):
    """Encode filename/body fixture pairs into a real ZIP; return exact archive bytes.
    将文件名及正文夹具对编码为真实 ZIP；返回精确归档字节。
    """
    # Stream carries genuine ZIP directory, CRC and member size records.
    # 流携带真实 ZIP 目录、CRC 及成员大小记录。
    stream = io.BytesIO()
    with zipfile.ZipFile(stream, "w", compression=zipfile.ZIP_DEFLATED) as archive:
        for name, body in files:
            archive.writestr(name, body)
    return stream.getvalue()


class RecoveryTests(unittest.TestCase):
    """Check specified-attempt authority and complete immutable artifact evidence.
    检查指定轮次权威及完整不可变制品证据。
    """

    def setUp(self):
        """Create one failed candidate attempt, successful gate jobs and signed-byte fixtures.
        创建一个失败候选轮次、成功门禁作业及签名字节夹具。
        """
        # Repository is synthetic and no test uses an outbound network transport.
        # 仓库为合成值，测试均不使用外部网络传输。
        self.repository = "LuaSkills/example-sdk"
        # Source is a full immutable commit, distinct from any later completion source.
        # 源码是完整不可变提交，与任何后续完成源码区分。
        self.source = "1" * 40
        # Workflow path is supplied by the SDK source instead of guessed from job names.
        # 工作流路径由 SDK 源码提供，不从作业名称猜测。
        self.workflow = ".github/workflows/sdk-release.yml"
        # Identity retains the original run and second attempt explicitly.
        # 身份明确保留原运行及第二轮次。
        self.identity = {"repository": self.repository, "workflow_path": self.workflow, "source_sha": self.source, "run_id": 41, "run_attempt": 2}
        # Required names are frozen declarations and include a deliberate display-space example.
        # 必需名称是冻结声明，并包含刻意使用展示空格的示例。
        self.required = ["aggregate", "native-windows", "native linux exact", "candidate-evidence"]
        # Run response keeps failure after otherwise successful candidate gates.
        # 运行响应保留候选门禁成功后发生的失败。
        self.run = {"id": 41, "run_attempt": 2, "repository": {"full_name": self.repository}, "head_repository": {"full_name": self.repository}, "head_sha": self.source, "path": self.workflow + "@main", "workflow_id": 31, "status": "completed", "conclusion": "failure"}
        # API and exact attempt endpoint are independently asserted throughout the tests.
        # 测试持续独立断言 API 及精确轮次端点。
        self.api = f"https://api.github.com/repos/{self.repository}"
        # Endpoint is never an unqualified latest workflow run.
        # 端点绝不是未限定轮次的最新工作流运行。
        self.endpoint = f"{self.api}/actions/runs/41/attempts/2"
        # Jobs model documented API fields rather than inventing a job run_attempt field.
        # 作业模拟文档 API 字段，不虚构作业 run_attempt 字段。
        self.jobs = [{"id": 101 + index, "run_id": 41, "head_sha": self.source, "run_url": f"{self.api}/actions/runs/41", "name": name, "status": "completed", "conclusion": "success"} for index, name in enumerate(self.required)]
        # Responses retain an unrelated failed latest attempt as an attractive but invalid fallback.
        # 响应保留无关失败最新轮次，作为诱人但无效的备用来源。
        self.http = MemoryHttp({self.endpoint: self.run, f"{self.endpoint}/jobs?per_page=100&page=1": {"total_count": len(self.jobs), "jobs": self.jobs}, f"{self.api}/actions/runs/41": {**self.run, "run_attempt": 3, "conclusion": "failure"}})
        # Payload represents tested packages plus their original native/core evidence.
        # 载荷表示已测试包及原始原生、核心证据。
        self.payload = {"sdk-package.tgz": b"immutable tested package", "native-matrix.json": b"native result", "core-prerequisites.zip": b"core frozen proof"}
        # Binding is signed independently and excludes itself and the subsequently generated bundle.
        # 绑定独立签名，并排除自身及随后生成的 bundle。
        self.binding = {"schema_version": 2, "kind": "sdk-candidate", **self.identity, "artifact_name": recovery.candidate_artifact_name(41, 2), "inventory": recovery.inventory_for(self.payload)}
        # Original binding bytes include canonical JSON formatting, not a reconstructed self-hash.
        # 原始绑定字节包含规范 JSON 格式，不是重建的自哈希。
        self.binding_bytes = candidate.encode(self.binding)
        # Physical files include exactly one official-verifier bundle outside the signed payload inventory.
        # 物理文件在签名载荷清单之外精确包含一个官方验证器 bundle。
        self.files = {**self.payload, recovery.BINDING_FILENAME: self.binding_bytes, "attestation.jsonl": b"SDK-official-verifier-fixture"}
        # Verified values model normalized successful SDK official verification, not cryptographic verification by this helper.
        # 已验证值模拟 SDK 官方成功验签归一化结果，不表示该辅助工具执行加密验签。
        self.verified = {"verified_subjects": {recovery.BINDING_FILENAME: candidate.digest(self.binding_bytes)}, "verified_invocation_uri": recovery.invocation_uri(self.repository, 41, 2), "verified_source_sha": self.source}

    def attempt(self, phase="candidate"):
        """Validate the current fixture under phase; return exact attempt evidence.
        在 phase 下验证当前夹具；返回精确轮次证据。
        """
        return recovery.verify_attempt(self.http, **self.identity, required_jobs=self.required, phase=phase)

    def signed(self, binding_bytes=None, verified=None):
        """Validate supplied or original binding bytes and official normalized evidence; return verified wrapper.
        验证所提供或原始绑定字节及官方归一化证据；返回已验证包装对象。
        """
        return recovery.verify_signed_binding(self.binding_bytes if binding_bytes is None else binding_bytes, **(self.verified if verified is None else verified), **self.identity, artifact_name=self.binding["artifact_name"])

    def artifact_http(self, content=None):
        """Create actual ZIP metadata for content or current files; return a detached HTTP fixture.
        为 content 或当前文件创建实际 ZIP 元数据；返回独立 HTTP 夹具。
        """
        # Body uses real ZIP bytes so digest/size mismatches exercise physical validation.
        # 正文采用真实 ZIP 字节，使摘要及大小不匹配触发物理验证。
        body = zip_bytes(self.files.items()) if content is None else content
        # Artifact metadata has source/run authority but deliberately no attempt timestamp inference.
        # 制品元数据具有源码及运行权威，但刻意不推测轮次时间戳。
        metadata = {"id": 51, "name": self.binding["artifact_name"], "expired": False, "workflow_run": {"id": 41, "head_sha": self.source}, "size_in_bytes": len(body), "digest": "sha256:" + candidate.digest(body)}
        # Explicit ID determines both endpoints.
        # 明确 ID 决定两个端点。
        endpoint = f"{self.api}/actions/artifacts/51"
        return MemoryHttp({endpoint: metadata, endpoint + "/zip": body})

    def download(self, http=None, **kwargs):
        """Download fixture artifact through explicit identity and optional limits; return evidence and bytes.
        按明确身份及可选限制下载夹具制品；返回证据及字节。
        """
        return recovery.download_artifact(self.artifact_http() if http is None else http, repository=self.repository, source_sha=self.source, run_id=41, artifact_id=51, artifact_name=self.binding["artifact_name"], **kwargs)

    def test_original_successful_gates_ignore_later_failure(self):
        """Keep original successful gate jobs despite a later failed attempt; never look up latest.
        在后续轮次失败时保留原成功门禁作业；绝不查询最新状态。
        """
        # Whole failure is retained, not promoted to successful publication.
        # 整体失败被保留，不提升为成功发布。
        result = self.attempt()
        self.assertEqual(result["attempt"]["conclusion"], "failure")
        self.assertEqual(result["run_attempt"], 2)
        self.assertEqual([job["name"] for job in result["required_jobs"]], self.required)
        self.assertNotIn((f"{self.api}/actions/runs/41", False), self.http.calls)

    def test_in_progress_candidate_preserves_actual_state(self):
        """Accept finished gate jobs while the same workflow publishes; preserve in_progress and null conclusion.
        在同一工作流发布期间接受已完成门禁；保留 in_progress 及空 conclusion。
        """
        self.run.update(status="in_progress", conclusion=None)
        self.assertEqual(self.attempt()["attempt"]["status"], "in_progress")
        self.assertIsNone(self.attempt()["attempt"]["conclusion"])

    def test_in_progress_candidate_requires_completed_evidence_job(self):
        """Reject an active candidate whose signature job has not finished, even when other gates succeed.
        即使其他门禁成功，也拒绝签名作业尚未完成的运行中候选。
        """
        self.run.update(status="in_progress", conclusion=None)
        self.jobs[-1].update(status="in_progress", conclusion=None)
        with self.assertRaises(ValueError):
            self.attempt()

    def test_gate_failures_never_use_latest_success(self):
        """Reject every failed required gate despite a successful latest run response.
        即使最新运行响应成功，也拒绝每个失败必需门禁。
        """
        self.http.responses[f"{self.api}/actions/runs/41"]["conclusion"] = "success"
        for index in range(len(self.required)):
            with self.subTest(name=self.required[index]):
                self.jobs[index]["conclusion"] = "failure"
                with self.assertRaises(ValueError):
                    self.attempt()
                self.jobs[index]["conclusion"] = "success"

    def test_missing_or_duplicate_required_job_rejected(self):
        """Reject absent gate names and distinct IDs sharing one required name.
        拒绝缺失门禁名称及具有不同 ID 的同名必需作业。
        """
        self.jobs[-1]["name"] = "different-gate"
        with self.assertRaises(ValueError):
            self.attempt()
        self.jobs[-1]["name"] = self.required[0]
        with self.assertRaises(ValueError):
            self.attempt()

    def test_wrong_attempt_repository_workflow_or_source_rejected(self):
        """Reject run identity changes across original attempt, repositories, workflow and source.
        拒绝原轮次、仓库、工作流及源码的运行身份变更。
        """
        for key, value in (("id", 42), ("run_attempt", 3), ("head_sha", "2" * 40), ("path", ".github/workflows/other.yml"), ("repository", {"full_name": "other/repo"}), ("head_repository", {"full_name": "other/repo"})):
            with self.subTest(key=key):
                # Original run is restored after each independent failure mutation.
                # 每个独立失败变异后恢复原运行。
                original = self.run[key]
                self.run[key] = value
                with self.assertRaises(ValueError):
                    self.attempt()
                self.run[key] = original

    def test_completion_requires_successful_whole_attempt_and_publish_job(self):
        """Reject failed or active completion attempts and failed publication jobs; accept actual success.
        拒绝失败或运行中完成轮次及失败发布作业；接受实际成功。
        """
        with self.assertRaises(ValueError):
            self.attempt("completion")
        self.run.update(status="in_progress", conclusion=None)
        with self.assertRaises(ValueError):
            self.attempt("completion")
        self.run.update(status="completed", conclusion="success")
        self.jobs[-1]["conclusion"] = "failure"
        with self.assertRaises(ValueError):
            self.attempt("completion")
        self.jobs[-1]["conclusion"] = "success"
        self.assertEqual(self.attempt("completion")["attempt"]["conclusion"], "success")

    def test_original_completion_success_ignores_failed_latest_attempt(self):
        """Authenticate original successful publication when the latest attempt failed; preserve explicit old identity.
        在最新轮次失败时认证原成功发布；保留明确旧身份。
        """
        self.run["conclusion"] = "success"
        self.jobs[-1]["name"] = "publish"
        # Only the source-declared publication gate is required by this completion fixture.
        # 当前完成夹具仅要求源码声明的发布门禁。
        evidence = recovery.verify_attempt(self.http, **self.identity, required_jobs=["publish"], phase="completion")
        self.assertEqual(evidence["run_attempt"], 2)
        self.assertEqual(evidence["attempt"]["conclusion"], "success")
        self.assertNotIn((f"{self.api}/actions/runs/41", False), self.http.calls)

    def test_inconsistent_status_and_malformed_api_rejected(self):
        """Reject queued/inconsistent states and absent documented response fields explicitly.
        明确拒绝排队或不一致状态及缺失文档响应字段。
        """
        for status, conclusion in (("queued", None), ("waiting", None), ("in_progress", "success"), ("completed", None)):
            with self.subTest(status=status):
                self.run.update(status=status, conclusion=conclusion)
                with self.assertRaises(ValueError):
                    self.attempt()
        del self.run["head_repository"]
        with self.assertRaises(ValueError):
            self.attempt()

    def test_boolean_identity_and_duplicate_declared_gates_rejected(self):
        """Reject bool IDs and ambiguous required job declarations before any HTTP lookup.
        在任何 HTTP 查询前拒绝布尔 ID 及含糊必需作业声明。
        """
        for identity in ({**self.identity, "run_id": True}, {**self.identity, "run_attempt": True}):
            with self.assertRaises(ValueError):
                recovery.verify_attempt(self.http, **identity, required_jobs=self.required, phase="candidate")
        with self.assertRaises(ValueError):
            recovery.verify_attempt(self.http, **self.identity, required_jobs=self.required + self.required[:1], phase="candidate")
        self.assertFalse(self.http.calls)

    def test_jobs_paginated_with_exact_endpoint(self):
        """Read required gates beyond page one and preserve exact attempt on every request.
        读取第一页之后的必需门禁，并在每个请求中保留精确轮次。
        """
        # Unrelated jobs have unique IDs and exact same run/source authority.
        # 无关作业具有唯一 ID 及精确相同运行、源码归属。
        unrelated = [{**self.jobs[0], "id": 300 + index, "name": f"unrelated-{index}"} for index in range(100)]
        self.http.responses[f"{self.endpoint}/jobs?per_page=100&page=1"] = {"total_count": 104, "jobs": unrelated}
        self.http.responses[f"{self.endpoint}/jobs?per_page=100&page=2"] = {"total_count": 104, "jobs": self.jobs}
        self.assertEqual(len(self.attempt()["required_jobs"]), 4)
        self.assertIn((f"{self.endpoint}/jobs?per_page=100&page=2", False), self.http.calls)

    def test_pagination_drift_empty_and_duplicate_ids_rejected(self):
        """Reject changing totals, empty partial pages and repeated job IDs across page boundaries.
        拒绝变化总数、空局部页及跨页重复作业 ID。
        """
        for page_two in ({"total_count": 6, "jobs": [self.jobs[0]]}, {"total_count": 5, "jobs": []}, {"total_count": 5, "jobs": [self.jobs[0]]}):
            with self.subTest(page=page_two):
                self.http.responses[f"{self.endpoint}/jobs?per_page=100&page=1"] = {"total_count": 5, "jobs": self.jobs}
                self.http.responses[f"{self.endpoint}/jobs?per_page=100&page=2"] = page_two
                with self.assertRaises(ValueError):
                    self.attempt()

    def test_foreign_or_malformed_job_rejected(self):
        """Reject job source/run/URL changes and missing response fields.
        拒绝作业源码、运行、URL 变更及缺失响应字段。
        """
        for key, value in (("run_id", 42), ("head_sha", "2" * 40), ("run_url", "https://api.github.com/repos/other/repo/actions/runs/41")):
            with self.subTest(key=key):
                # Mutation is restored before the next independent case.
                # 变异在下个独立用例之前恢复。
                original = self.jobs[0][key]
                self.jobs[0][key] = value
                with self.assertRaises(ValueError):
                    self.attempt()
                self.jobs[0][key] = original
        del self.jobs[0]["id"]
        with self.assertRaises(ValueError):
            self.attempt()

    def test_real_zip_download_and_binding_authenticate_original_bytes(self):
        """Validate actual ZIP bytes, keep metadata unbound, then authenticate exact signed inventory.
        验证实际 ZIP 字节，保持元数据未绑定，再认证精确签名清单。
        """
        # Download evidence cannot claim an attempt using timestamps or its own metadata.
        # 下载证据不能通过时间戳或自身元数据宣称轮次。
        evidence, files = self.download()
        self.assertIs(evidence["attempt_bound"], False)
        self.assertEqual(files, self.files)
        self.assertEqual(recovery.verify_inventory(self.signed(), files, attestation_filename="attestation.jsonl"), self.binding["inventory"])

    def test_artifact_expired_id_name_source_and_run_rejected(self):
        """Reject expired, foreign and mismatched artifact identities before downloading bytes.
        在下载字节之前拒绝过期、外来及不匹配制品身份。
        """
        for field, value in (("expired", True), ("id", 52), ("name", "candidate-evidence-r41-a3"), ("workflow_run", {"id": 42, "head_sha": self.source}), ("workflow_run", {"id": 41, "head_sha": "2" * 40})):
            with self.subTest(field=field, value=value):
                # Client is rebuilt so prior failures cannot poison a subsequent case.
                # 重新创建客户端，避免此前失败污染后续用例。
                http = self.artifact_http()
                http.responses[f"{self.api}/actions/artifacts/51"][field] = value
                with self.assertRaises(ValueError):
                    self.download(http)
                self.assertEqual(len(http.calls), 1)

    def test_missing_and_malformed_artifact_metadata_rejected(self):
        """Reject missing artifact endpoints and malformed required metadata.
        拒绝缺失制品端点及异常必需元数据。
        """
        with self.assertRaises(ValueError):
            self.download(MemoryHttp({}))
        for field, value in (("digest", None), ("digest", "md5:" + "1" * 32), ("digest", "sha256:" + "1" * 63), ("size_in_bytes", True), ("size_in_bytes", 0), ("size_in_bytes", recovery.MAX_ARTIFACT_BYTES + 1), ("expired", None)):
            with self.subTest(field=field, value=value):
                # Metadata mutation is confined to this exact fixture instance.
                # 元数据变异仅限于当前精确夹具实例。
                http = self.artifact_http()
                http.responses[f"{self.api}/actions/artifacts/51"][field] = value
                with self.assertRaises(ValueError):
                    self.download(http)
        # Missing fields must raise explicit validation errors rather than being guessed.
        # 缺失字段必须抛出明确验证错误，不得猜测。
        http = self.artifact_http()
        del http.responses[f"{self.api}/actions/artifacts/51"]["workflow_run"]
        with self.assertRaises(ValueError):
            self.download(http)

    def test_download_actual_digest_and_size_mismatch_rejected(self):
        """Reject any byte digest or download size mismatch against actual artifact metadata.
        拒绝实际制品元数据与字节摘要或下载大小的任何不匹配。
        """
        for field, value in (("digest", "sha256:" + "0" * 64), ("size_in_bytes", 1)):
            with self.subTest(field=field):
                # Real ZIP bytes remain valid so metadata mismatch alone causes the failure.
                # 真实 ZIP 字节仍有效，只有元数据不匹配导致失败。
                http = self.artifact_http()
                http.responses[f"{self.api}/actions/artifacts/51"][field] = value
                with self.assertRaises(ValueError):
                    self.download(http)

    def test_zip_traversal_nested_alias_and_directory_rejected(self):
        """Reject traversal, nested paths and portable filename aliases in genuine ZIPs.
        拒绝真实 ZIP 中的目录穿越、嵌套路径及可移植文件名别名。
        """
        for name in ("../package.tgz", "/package.tgz", "nested/package.tgz", "nested/", "C:package.tgz", "name\\package.tgz", "package.tgz ", "CON", "CON.txt", "COM¹.txt", "bad?.tgz", "bad*.tgz", "bad\n.tgz", "bad|.tgz", "bad<.tgz"):
            with self.subTest(name=name):
                with self.assertRaises(ValueError):
                    recovery.artifact_files(zip_bytes([(name, b"wrong")]))

    def test_zip_symlink_and_duplicate_rejected(self):
        """Reject a real Unix symlink entry and repeated or case-aliased ZIP names.
        拒绝真实 Unix 符号链接条目及重复或大小写别名 ZIP 名称。
        """
        # Symlink has genuine Unix type bits and an otherwise safe root filename.
        # 符号链接具有真实 Unix 类型位及原本安全的根文件名。
        link = zipfile.ZipInfo("package.tgz")
        link.create_system = 3
        link.external_attr = (0o120777 << 16)
        with self.assertRaises(ValueError):
            recovery.artifact_files(zip_bytes([(link, b"target")]))
        with warnings.catch_warnings():
            warnings.simplefilter("ignore", UserWarning)
            for names in (("package.tgz", "package.tgz"), ("package.tgz", "PACKAGE.TGZ")):
                with self.subTest(names=names):
                    with self.assertRaises(ValueError):
                        recovery.artifact_files(zip_bytes([(name, b"body") for name in names]))

    def test_zip_nul_normalization_and_dos_directory_rejected(self):
        """Reject Python-truncated NUL names and DOS directory entries without trailing slashes.
        拒绝被 Python 截短的 NUL 名称及没有尾斜杠的 DOS 目录条目。
        """
        # Equal-length raw header substitution produces a genuine central/local NUL filename.
        # 等长原始头替换生成真实中央及本地 NUL 文件名。
        body = zip_bytes([("package.tgz___evil", b"body")]).replace(b"package.tgz___evil", b"package.tgz\x00__evil")
        with self.assertRaises(ValueError):
            recovery.artifact_files(body)
        # DOS-only directory metadata is independent from a filename's trailing slash.
        # 仅 DOS 目录元数据与文件名尾斜杠相互独立。
        directory = zipfile.ZipInfo("package.tgz")
        directory.create_system = 0
        directory.external_attr = 0x10
        with self.assertRaises(ValueError):
            recovery.artifact_files(zip_bytes([(directory, b"body")]))

    def test_zip_malformed_crc_and_expansion_limit_rejected(self):
        """Reject non-ZIP/corrupted archives and a tiny compressed body's oversized expansion.
        拒绝非 ZIP 或损坏归档及小型压缩正文的过大展开。
        """
        with self.assertRaises(ValueError):
            recovery.artifact_files(b"not ZIP")
        # Body is highly compressible and exercises expansion independently from download size.
        # 正文高度可压缩，独立于下载大小验证展开。
        body = zip_bytes([("package.tgz", b"0" * 4096)])
        self.assertLess(len(body), 4096)
        with self.assertRaises(ValueError):
            self.download(self.artifact_http(body), max_unpacked_bytes=2048)
        with self.assertRaises(ValueError):
            recovery.artifact_files(body[:20])
        # Stored member corruption leaves a parseable archive while its actual CRC no longer matches.
        # 普通存储成员损坏后归档仍能解析，但实际 CRC 不再匹配。
        stream = io.BytesIO()
        with zipfile.ZipFile(stream, "w", compression=zipfile.ZIP_STORED) as archive:
            archive.writestr("package.tgz", b"unique-package-body")
        # Exact corruption changes physical member bytes without rebuilding metadata.
        # 精确损坏改变物理成员字节，不重建元数据。
        corrupted = stream.getvalue().replace(b"unique-package-body", b"unique-package-b0dy")
        with self.assertRaises(ValueError):
            recovery.artifact_files(corrupted)

    def test_binding_subject_invocation_and_source_must_be_official_exact(self):
        """Reject missing/wrong binding subjects, foreign invocations and source attestations.
        拒绝缺失或错误绑定 subject、外来调用及源码 attestation。
        """
        for changes in ({"verified_subjects": {}}, {"verified_subjects": {recovery.BINDING_FILENAME: "0" * 64}}, {"verified_invocation_uri": recovery.invocation_uri(self.repository, 41, 3)}, {"verified_source_sha": "2" * 40}, {"verified_invocation_uri": "https://github.com/other/repo/actions/runs/41/attempts/2"}):
            with self.subTest(changes=changes):
                with self.assertRaises(ValueError):
                    self.signed(verified={**self.verified, **changes})

    def test_signed_binding_schema_fields_identity_and_self_hash_rejected(self):
        """Reject authenticated but unsupported/foreign/self-hashing binding objects.
        拒绝已认证但不支持、外来或自哈希绑定对象。
        """
        for changes in ({"schema_version": 1}, {"schema_version": True}, {"kind": "sdk-completion"}, {"run_attempt": 3}, {"run_id": True}, {"source_sha": "2" * 40}, {"artifact_name": "candidate-evidence-r41-a3"}, {"unknown": 1}, {"inventory": [{"filename": recovery.BINDING_FILENAME, "size": 0, "sha256": "0" * 64}]}):
            with self.subTest(changes=changes):
                # Subjects authenticate these wrong objects, so structural/identity checks remain necessary.
                # Subject 认证这些错误对象，因此仍须结构及身份检查。
                body = candidate.encode({**self.binding, **changes})
                with self.assertRaises(ValueError):
                    self.signed(body, {**self.verified, "verified_subjects": {recovery.BINDING_FILENAME: candidate.digest(body)}})

    def test_binding_duplicate_json_keys_and_changed_original_bytes_rejected(self):
        """Reject duplicate signed JSON keys and whitespace changes outside the authenticated subject bytes.
        拒绝签名 JSON 重复键及已认证 subject 字节之外的空白变化。
        """
        # Duplicate keys are authenticated here to exercise the parser's exact-object ownership guard.
        # 此处认证重复键，以验证解析器的精确对象归属护栏。
        body = self.binding_bytes.rstrip()[:-1] + b',"run_id":41}'
        with self.assertRaises(ValueError):
            self.signed(body, {**self.verified, "verified_subjects": {recovery.BINDING_FILENAME: candidate.digest(body)}})
        with self.assertRaises(ValueError):
            self.signed(self.binding_bytes + b" ")

    def test_inventory_any_member_byte_size_hash_or_name_difference_rejected(self):
        """Reject every inventory difference, extra/missing bytes and changed signed binding bytes.
        拒绝任何清单差异、额外或缺失字节及变更签名绑定字节。
        """
        for files in ({**self.files, "sdk-package.tgz": b"changed"}, {**self.files, "extra.json": b"extra"}, {name: body for name, body in self.files.items() if name != "native-matrix.json"}, {**self.files, recovery.BINDING_FILENAME: self.binding_bytes + b" "}, {**self.files, "attestation.jsonl": b""}):
            with self.subTest(names=list(files)):
                with self.assertRaises(ValueError):
                    recovery.verify_inventory(self.signed(), files, attestation_filename="attestation.jsonl")
        for changes in ({"size": 0}, {"sha256": "0" * 64}, {"filename": "different.tgz"}):
            # Signed inventory mutation keeps the structural schema valid but must differ from physical bytes.
            # 签名清单变异保持结构 Schema 有效，但必然与物理字节不同。
            rows = copy.deepcopy(self.binding["inventory"])
            rows[0].update(changes)
            with self.assertRaises(ValueError):
                recovery.compare_inventory(rows, self.payload)

    def test_inventory_strict_schema_duplicate_portable_name_and_size(self):
        """Reject unknown inventory fields, aliases, duplicate names and invalid numeric sizes.
        拒绝未知清单字段、别名、重复名称及无效数字大小。
        """
        for rows in ([], [{"filename": "file", "size": True, "sha256": "0" * 64}], [{"filename": "file", "size": -1, "sha256": "0" * 64}], [{"filename": "file", "size": 0, "sha256": "0" * 64, "extra": 1}], [{"filename": "file", "size": 0, "sha256": "0" * 64}, {"filename": "FILE", "size": 0, "sha256": "0" * 64}]):
            with self.subTest(rows=rows):
                with self.assertRaises(ValueError):
                    recovery.validate_inventory(rows)


if __name__ == "__main__":
    unittest.main()
