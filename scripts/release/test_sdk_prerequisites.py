"""Exercise public publication gates with real tar/zip fixtures and mock read-only HTTP/process boundaries.
使用真实 tar、zip 夹具及模拟只读 HTTP、进程边界验证公共发布门禁。
"""

import argparse
import contextlib
import copy
import io
import json
import os
from pathlib import Path
import subprocess
import shutil
import sys
import tarfile
import tempfile
import time
import unittest
from unittest.mock import patch
import urllib.error
import warnings
import zipfile

import candidate
import sdk_prerequisites as gate
import test_candidate as fixtures


class Response(io.BytesIO):
    """Expose real byte-stream HTTP response semantics for offline opener tests.
    为离线 opener 测试暴露真实字节流 HTTP 响应语义。
    """

    def __init__(self, content, headers=None):
        """Store exact content and optional headers; return a successful response stream.
        保存精确 content 及可选 headers；返回成功响应流。
        """
        super().__init__(content)
        # Status models a real successful API/download response.
        # 状态模拟真实成功的 API 或下载响应。
        self.status = 200
        # Headers include real pagination hints when supplied by the fixture.
        # 夹具提供时，响应头包含真实分页提示。
        self.headers = {} if headers is None else headers


class PublicationHttp(gate.Http):
    """Use actual Http credential/media handling with an offline response opener.
    使用实际 Http 凭据及媒体处理，同时采用离线响应 opener。
    """

    def __init__(self, owner):
        """Bind this client to owner's mutable fixture server; never read real credentials.
        将此客户端绑定到 owner 可变夹具服务器；绝不读取真实凭据。
        """
        # Synthetic credential verifies API authentication and never contacts a real service.
        # 合成凭据验证 API 认证，绝不联系真实服务。
        self.token = "synthetic-offline-secret"
        # Opener is this fixture server, retaining real urllib Request construction in Http.get.
        # Opener 是此夹具服务器，保留 Http.get 中真实 urllib Request 构造。
        self.opener = self
        # Owner supplies immutable bytes plus explicit failure mutations.
        # Owner 提供不可变字节及显式失败变异。
        self.owner = owner
        # Calls track lookup order so drift can happen after successful initial downloads.
        # 调用记录查询顺序，使漂移能发生在初次成功下载之后。
        self.calls = []

    def open(self, request, timeout):
        """Handle an actual urllib Request and timeout; return fixture stream or explicit HTTP error.
        处理实际 urllib Request 及 timeout；返回夹具流或显式 HTTP 错误。
        """
        # URL is exact and records no credential-bearing request headers.
        # URL 精确，且记录不包含含凭据的请求头。
        url = request.full_url
        self.calls.append(url)
        self.owner.assertEqual(timeout, 60)
        if url.startswith("https://api.github.com/"):
            self.owner.assertEqual(request.get_header("Authorization"), "Bearer synthetic-offline-secret")
        else:
            self.owner.assertIsNone(request.get_header("Authorization"))
        if url in self.owner.http_errors:
            raise urllib.error.HTTPError(url, 404, "fixture missing", {}, io.BytesIO())
        if url == gate.REPO_API:
            return Response(candidate.encode({"full_name": gate.REPOSITORY}))
        if url == f"{gate.REPO_API}/releases/tags/{self.owner.tag}":
            return Response(candidate.encode(self.owner.release))
        if url.startswith(f"{gate.REPO_API}/releases/7/assets?"):
            # Two real pages exercise the Link-driven traversal regardless of page length.
            # 两个真实分页验证基于 Link 的遍历，不依赖页长。
            page = int(url.rsplit("page=", 1)[1])
            entries = list(self.owner.assets.values())
            midpoint = len(entries) // 2
            return Response(candidate.encode(entries[:midpoint] if page == 1 else entries[midpoint:]), {"Link": '<https://api.github.com/fixture>; rel="next"'} if page == 1 else {})
        if url == f"{gate.REPO_API}/git/ref/tags/{self.owner.tag}":
            # A second ref read may observe a real release-time tag mutation.
            # 第二次引用读取可能观察到发布期间真实标签变异。
            commit = "d" * 40 if self.owner.tag_drift and self.calls.count(url) > 1 else self.owner.commit
            if self.owner.annotated:
                return Response(candidate.encode({"ref":f"refs/tags/{self.owner.tag}","object":{"sha":"a"*40,"type":"tag"}}))
            return Response(candidate.encode({"ref":f"refs/tags/{self.owner.tag}","object":{"sha":commit,"type":"commit"}}))
        if url == f"{gate.REPO_API}/git/tags/{'a'*40}":
            return Response(candidate.encode({"sha":"a"*40,"object":{"sha":"b"*40,"type":"tag"}}))
        if url == f"{gate.REPO_API}/git/tags/{'b'*40}":
            return Response(candidate.encode({"sha":"b"*40,"object":{"sha":self.owner.commit,"type":"commit"}}))
        if url.startswith(f"{gate.REPO_API}/git/commits/"):
            return Response(candidate.encode({"sha":url.rsplit("/", 1)[1], "tree":{"sha":self.owner.root_tree}}))
        if url == f"{gate.REPO_API}/git/trees/{self.owner.root_tree}":
            return Response(candidate.encode({"sha":self.owner.root_tree,"truncated":False,"tree":[{"path":".github","type":"tree","sha":self.owner.github_tree}]}))
        if url == f"{gate.REPO_API}/git/trees/{self.owner.github_tree}":
            return Response(candidate.encode({"sha":self.owner.github_tree,"truncated":False,"tree":[{"path":"workflows","type":"tree","sha":self.owner.workflows_tree}]}))
        if url == f"{gate.REPO_API}/zipball/{self.owner.commit}":
            # GitHub negotiates the archive redirect as JSON even though the resulting body is ZIP bytes.
            # GitHub 用 JSON 媒体协商归档重定向，最终正文仍是 ZIP 字节。
            self.owner.assertEqual(request.get_header("Accept"), "application/json")
            return Response(self.owner.commit_zip)
        for name, asset in self.owner.assets.items():
            if url == asset["url"]:
                self.owner.assertEqual(request.get_header("Accept"), "application/octet-stream")
                # Content drift with unchanged ID/size/updated_at must still fail final byte recheck.
                # 即使 ID、大小及更新时间未变，内容漂移仍必须使最终字节重查失败。
                body = self.owner.asset_bytes[name]
                if name == self.owner.byte_drift and self.calls.count(url) > 1:
                    body = bytes([body[0] ^ 1]) + body[1:]
                return Response(body)
        if url == f"https://crates.io/api/v1/crates/luaskills/{self.owner.version}":
            return Response(candidate.encode({"version": self.owner.crate_record}))
        if url == f"https://crates.io/api/v1/crates/luaskills/{self.owner.version}/download":
            self.owner.assertEqual(request.get_header("Accept"), "application/octet-stream")
            return Response(self.owner.crate)
        raise AssertionError(f"Unexpected fixture HTTP URL: {url}")


class PrerequisiteTests(unittest.TestCase):
    """Reject release/tag/archive/registry/runtime impersonation across actual gate control flow.
    在实际门禁控制流中拒绝发布、标签、归档、registry 及运行冒充。
    """

    @classmethod
    def setUpClass(cls):
        """Reuse real committed-source candidate fixtures and their single platform authority.
        复用真实已提交源码候选夹具及其唯一平台权威。
        """
        fixtures.CandidateTests.setUpClass()

    def setUp(self):
        """Create complete five-platform tar assets and official commit zip in isolated temporary space.
        在隔离临时空间创建完整五平台 tar 资产及正式提交 zip。
        """
        # Candidate fixture helper uses actual committed source and synthetic native bytes explicitly.
        # 候选夹具助手使用实际已提交源码及明确合成的原生字节。
        helper = fixtures.CandidateTests()
        helper.setUp()
        self.addCleanup(helper.doCleanups)
        # Keep candidate's native files and Cargo fixtures; add explicitly synthetic compiler verbose evidence.
        # 保留 candidate 原生文件及 Cargo 夹具；增加明确合成的编译器 verbose 证据。
        original_fixture = helper.platform_fixture

        def platform_fixture(platform):
            """Extend existing fixture with valid-shaped synthetic compiler proof; return original package shape.
            用有效形状的合成编译器证明扩展既有夹具；返回原包形状。
            """
            # Existing helper retains ownership of source hashes, platform libraries and actual Cargo structure.
            # 既有助手保留源码摘要、平台库及实际 Cargo 结构归属。
            files, manifest = original_fixture(platform)
            report = candidate.decode_json(files["embedded-build-inputs.json"])
            report["fields"]["rustc"] = f"rustc 1.97.1 (explicit offline fixture)\nrelease: 1.97.1\ncommit-hash: {'e'*40}\nhost: {candidate.PLATFORMS[platform][0]}"
            files["embedded-build-inputs.json"] = candidate.encode(report)
            build = {**report["fields"], "cargo_features":report["cargo_features"], "inputs_sha256":candidate.digest(files["embedded-build-inputs.json"])}
            description = candidate.decode_json(files["embedded-core-description.json"])
            description["build"] = build
            files["embedded-core-description.json"] = candidate.encode(description)
            manifest["build"] = build
            manifest["files"] = {name:candidate.digest(body) for name, body in files.items()}
            return files, manifest

        helper.platform_fixture = platform_fixture
        with contextlib.redirect_stdout(io.StringIO()):
            candidate.aggregate(helper.all_platforms())
        # Base remains test-owned; never writes repository build or publication output.
        # Base 始终由测试拥有；绝不写入仓库构建或发布产物。
        self.base = helper.base
        self.commit = helper.commit
        self.version = helper.version
        self.tag = f"v{self.version}"
        self.source = helper.source_files
        # Git tree IDs come from the actual frozen source, while default historical commits are server fixtures.
        # Git 树 ID 来自实际冻结源码，而历史默认提交为服务器夹具。
        self.root_tree = candidate.git(helper.root, "rev-parse", f"{self.commit}^{{tree}}").decode().strip()
        self.github_tree = candidate.git(helper.root, "rev-parse", f"{self.commit}:.github").decode().strip()
        self.workflows_tree = candidate.git(helper.root, "rev-parse", f"{self.commit}:.github/workflows").decode().strip()
        # Assets represent the actual official release asset list split across two API pages.
        # Assets 表示实际正式发布资产列表，拆分为两个 API 分页。
        self.asset_bytes = {path.name: path.read_bytes() for path in helper.output.iterdir()}
        for index, name in enumerate(gate.create_draft.DRAFT_SOURCE_EVIDENCE_ASSETS):
            self.asset_bytes[name] = candidate.encode({"schema_version":candidate.MANIFEST_VERSION,"repository":gate.REPOSITORY,"source_commit":self.commit,"source_workflows_tree":self.workflows_tree,"default_branch":"captured-default-label","default_commit":self.commit if index == 0 else "f"*40,"default_root_tree":self.root_tree,"default_workflows_tree":self.workflows_tree})
        self.assets = {name:{"id": index + 100,"name":name,"size":len(body),"url":f"{gate.REPO_API}/releases/assets/{index + 100}","state":"uploaded","updated_at":"2026-09-30T00:00:00Z"} for index, (name, body) in enumerate(self.asset_bytes.items())}
        self.release = {"id":7,"url":f"{gate.REPO_API}/releases/7","tag_name":self.tag,"draft":False,"prerelease":False,"published_at":"2026-09-30T00:00:00Z","target_commitish":"not-a-commit-proof"}
        # Official commit zip independently authenticates source archive members.
        # 正式提交 zip 独立认证源码归档成员。
        buffer = io.BytesIO()
        with zipfile.ZipFile(buffer, "w") as archive:
            for name, body in self.source.items():
                archive.writestr("official-commit/" + name, body)
        self.commit_zip = buffer.getvalue()
        # Crate includes real original source bytes plus Cargo's documented generated manifest/VCS metadata.
        # Crate 包含真实原始源码字节及 Cargo 文档声明的生成清单与 VCS 元数据。
        self.crate_files = {f"luaskills-{self.version}/{name}":body for name, body in self.source.items()}
        self.crate_files[f"luaskills-{self.version}/Cargo.toml.orig"] = self.source["Cargo.toml"]
        self.crate_files[f"luaskills-{self.version}/.cargo_vcs_info.json"] = candidate.encode({"git":{"sha1":self.commit},"path_in_vcs":""})
        self.crate = candidate.archive_bytes(self.crate_files)
        # Mock official Cargo normalization is independent from tampered registry package bytes.
        # 模拟官方 Cargo 规范化独立于篡改的 registry 包字节。
        self.normalized_manifest = self.source["Cargo.toml"]
        self.toolchain_record = candidate.read_json(helper.output / "candidate-manifest.json")["platforms"][0]
        self.crate_record = {"crate":"luaskills","num":self.version,"yanked":False,"checksum":candidate.digest(self.crate),"dl_path":f"/api/v1/crates/luaskills/{self.version}/download"}
        # Mutation switches isolate explicit failures without skipping any production guard.
        # 变异开关隔离显式失败，不跳过任何生产护栏。
        self.http_errors = set()
        self.annotated = False
        self.tag_drift = False
        self.byte_drift = None
        self.process_failure = None
        self.http = PublicationHttp(self)

    def run_gate(self, phase="github-only", name="gate"):
        """Run actual gate in new test output with mock HTTP/process boundaries; return its evidence.
        在新测试输出中通过模拟 HTTP 或进程边界执行实际门禁；返回其证据。
        """
        with contextlib.redirect_stdout(io.StringIO()):
            return gate.run_gate(self.tag, self.commit, self.base / name, phase, http=self.http, runner=self.runner)

    def runner(self, command, project, env):
        """Model actual Cargo outputs/files for command using synthetic binary bytes; return CompletedProcess.
        使用合成二进制字节为 command 模拟实际 Cargo 输出及文件；返回 CompletedProcess。
        """
        self.assertEqual(env["CARGO_BUILD_JOBS"], "4")
        self.assertEqual(env["RUST_TEST_THREADS"], "1")
        self.assertEqual(env["RUSTUP_TOOLCHAIN"], "1.97.1")
        self.assertNotIn("GH_TOKEN", env)
        if command == ["rustc", "--version", "--verbose"]:
            return subprocess.CompletedProcess(command, 0, self.toolchain_record["build"]["rustc"].encode(), b"")
        if command == ["cargo", "--version", "--verbose"]:
            return subprocess.CompletedProcess(command, 0, self.toolchain_record["cargo"]["version_verbose"].encode(), b"")
        if command[:2] == ["cargo", "package"]:
            self.assertEqual((project / "Cargo.lock").read_bytes(), self.source["Cargo.lock"])
            self.assertEqual((project / "build.rs").read_bytes(), self.source["build.rs"])
            self.assertIn("--locked", command)
            self.assertIn("--no-verify", command)
            candidate.write_new(Path(env["CARGO_TARGET_DIR"]) / "package" / f"luaskills-{self.version}.crate", candidate.archive_bytes({f"luaskills-{self.version}/Cargo.toml":self.normalized_manifest}))
            return subprocess.CompletedProcess(command, 0, b"explicit synthetic Cargo package result", b"")
        # Manifest cannot use a source patch or a local/git dependency as a registry substitute.
        # 清单不能使用来源 patch 或本地、Git 依赖替代 registry。
        manifest = candidate.tomllib.loads((project / "Cargo.toml").read_text(encoding="utf-8"))
        self.assertEqual(manifest["dependencies"]["luaskills"], "=" + self.version)
        self.assertNotIn("patch", manifest)
        # Actual package ID matches Cargo's metadata and compiler ownership messages.
        # 实际包 ID 匹配 Cargo 元数据及编译器归属消息。
        package_id = f"registry+https://github.com/rust-lang/crates.io-index#luaskills@{self.version}"
        stdout = b""
        if command == ["cargo", "generate-lockfile"]:
            source = 'source = "' + gate.REGISTRY_SOURCE + '"\n' if self.process_failure != "path-lock" else ""
            (project / "Cargo.lock").write_text(f'[[package]]\nname="luaskills"\nversion="{self.version}"\n{source}checksum="{self.crate_record["checksum"]}"\n')
        elif command[:2] == ["cargo", "metadata"]:
            stdout = candidate.encode({"packages":[{"id":package_id,"name":"luaskills","version":self.version,"source":gate.REGISTRY_SOURCE}]})
            candidate.write_new(Path(env["CARGO_HOME"]) / "registry/cache/index-fixture" / f"luaskills-{self.version}.crate", self.crate)
        elif command[:2] == ["cargo", "build"]:
            self.assertIn("--locked", command)
            self.assertEqual(command[command.index("-j") + 1], "4")
            binary = project / "target/debug/luaskills-registry-consumer"
            candidate.write_new(binary, b"explicit offline synthetic consumer binary")
            stdout = (json.dumps({"reason":"compiler-artifact","manifest_path":str(project / "Cargo.toml"),"target":{"name":"luaskills-registry-consumer"},"executable":str(binary)}) + "\n" + json.dumps({"reason":"build-finished","success":True}) + "\n").encode()
            if self.process_failure == "no-artifact":
                stdout = (json.dumps({"reason":"build-finished","success":True}) + "\n").encode()
        else:
            self.assertTrue(Path(command[0]).is_file())
            challenge = "pasted-old-output" if self.process_failure == "pasted" else command[1]
            stdout = candidate.encode({"challenge":challenge,"runtime":True,"pool_reuse":True,"capability_calls":2,"drained":True})
            if self.process_failure == "failed-with-success-output":
                return subprocess.CompletedProcess(command, 1, stdout, b"real process failed")
        return subprocess.CompletedProcess(command, 0, stdout, b"")

    def reject(self, phase="github-only"):
        """Require gate failure for current fixture mutation and no accepted prerequisites file.
        要求当前夹具变异使门禁失败，且不存在已验收 prerequisites 文件。
        """
        with self.assertRaises((ValueError, KeyError)):
            self.run_gate(phase)
        self.assertFalse((self.base / "gate/prerequisites.json").exists())

    def replace_asset(self, name, body):
        """Replace fixture asset bytes and API size together; return nothing without altering checksums.
        同时替换夹具资产字节及 API 大小；不改校验和且不返回值。
        """
        self.asset_bytes[name] = body
        self.assets[name]["size"] = len(body)

    def test_github_only_is_incomplete_and_derives_every_sdk_input(self):
        """Accept real archive verification but never claim Cargo consumption in GitHub-only phase.
        接受真实归档验证，但 GitHub 阶段绝不声明 Cargo 消费。
        """
        proof = self.run_gate()
        self.assertFalse(proof["complete"])
        self.assertIsNone(proof["registry"])
        self.assertEqual(set(proof["sdk_inputs"]), set(candidate.PLATFORMS))
        for inputs in proof["sdk_inputs"].values():
            self.assertEqual(candidate.digest(Path(inputs["library"]).read_bytes()), inputs["library_sha256"])
            self.assertEqual(candidate.digest(Path(inputs["description"]).read_bytes()), inputs["description_sha256"])
        self.assertTrue(any("page=2" in url for url in self.http.calls))

    def test_toolchain_bootstrap_revalidates_public_identity_and_never_authorizes_release(self):
        """Verify full source/public assets again before exposing exact compiler/Cargo installation identity.
        暴露精确编译器或 Cargo 安装身份前，再次验证完整源码及公共资产。
        """
        proof = self.run_gate()
        platform = self.toolchain_record["platform"]
        self.http.calls.clear()
        with contextlib.redirect_stdout(io.StringIO()):
            inputs = gate.toolchain_inputs(self.base / "gate/prerequisites.json", platform, http=self.http)
        self.assertFalse(inputs["complete"])
        self.assertEqual(inputs["phase"], "github-only")
        self.assertEqual(inputs["toolchain"], "1.97.1")
        self.assertEqual(inputs["rustc"]["host"], candidate.PLATFORMS[platform][0])
        self.assertEqual(inputs["cargo"], self.toolchain_record["cargo"])
        self.assertEqual(inputs["github"]["commit_source_sha256"], proof["github"]["commit_source_sha256"])
        self.assertTrue(any(url.endswith(f"/zipball/{self.commit}") for url in self.http.calls))
        self.assertGreaterEqual(sum("/releases/assets/" in url for url in self.http.calls), 2 * len(self.assets))

    def test_toolchain_bootstrap_rejects_fabricated_json_and_changed_downloaded_record(self):
        """Self-declared release IDs and tampered local compiler records cannot choose a toolchain.
        自声明发布 ID 及篡改本地编译器记录无法选择工具链。
        """
        proof = self.run_gate()
        platform = self.toolchain_record["platform"]
        proof["github"]["release_id"] += 1
        report = self.base / "gate/prerequisites.json"
        report.write_bytes(candidate.encode(proof))
        with contextlib.redirect_stdout(io.StringIO()), self.assertRaisesRegex(ValueError, "formal release identity changed"):
            gate.toolchain_inputs(report, platform, http=self.http)
        proof["github"]["release_id"] -= 1
        report.write_bytes(candidate.encode(proof))
        (self.base / "gate/downloads/assets" / f"candidate-{platform}.json").write_bytes(b"{}")
        with self.assertRaisesRegex(ValueError, "downloaded asset checksum mismatch"):
            gate.toolchain_inputs(report, platform, http=self.http)

    def test_controlled_environment_preserves_rustup_storage_and_replaces_external_selection(self):
        """Use an actual child process to verify preserved storage and sanitized fixed release selection.
        使用实际子进程验证保留存储及清理后的固定发布版本选择。
        """
        with patch.dict(os.environ, {"RUSTUP_HOME": str(self.base / "custom-rustup-store"), "RUSTUP_TOOLCHAIN": "nightly", "RUSTFLAGS": "injected", "RUSTC_WRAPPER": "injected", "CARGO_HOME": "injected"}):
            env = gate.isolated_environment(self.base / "fresh-home", self.base / "fresh-target", "1.97.1")
        result = subprocess.run([sys.executable, "-X", "utf8", "-c", "import os,json; print(json.dumps({k:v for k,v in os.environ.items() if k.startswith(('RUST','CARGO'))}))"], env=env, capture_output=True, check=True)
        actual = json.loads(result.stdout)
        self.assertEqual(actual["RUSTUP_HOME"], str(self.base / "custom-rustup-store"))
        self.assertEqual(actual["RUSTUP_TOOLCHAIN"], "1.97.1")
        self.assertNotIn("RUSTFLAGS", actual)
        self.assertNotIn("RUSTC_WRAPPER", actual)
        self.assertEqual(actual["CARGO_HOME"], str(self.base / "fresh-home"))
        for invalid in ("stable", "nightly", "1.97.1; injected", "1.97.1-x86_64-pc-windows-msvc"):
            with self.assertRaisesRegex(ValueError, "exact stable release"):
                gate.isolated_environment(self.base / "home", self.base / "target", invalid)

    def test_shared_release_compiler_selection_rejects_platform_drift(self):
        """A host-specific compiler divergence fails before probing or launching any Cargo command.
        宿主特定编译器漂移在探测或启动任何 Cargo 命令前失败。
        """
        proof = self.run_gate()
        manifest = candidate.read_json(self.base / "gate/candidate/candidate-manifest.json")
        records = manifest["platforms"]
        changed = next(record for record in records if record["platform"] == self.toolchain_record["platform"])
        changed["build"]["rustc"] = changed["build"]["rustc"].replace("1.97.1", "1.97.2")
        with self.assertRaisesRegex(ValueError, "compiler identities disagree"):
            gate.release_toolchain(records)

    def test_actual_draft_source_assets_are_required_and_historically_verified(self):
        """Accept both real draft-source assets and verify their historical commit/tree owners.
        接受两份真实草稿源码资产，并验证其历史提交及树所有者。
        """
        proof = self.run_gate()
        self.assertEqual(set(proof["github"]["draft_source_evidence"]), set(gate.create_draft.DRAFT_SOURCE_EVIDENCE_ASSETS))
        for name in gate.create_draft.DRAFT_SOURCE_EVIDENCE_ASSETS:
            self.assertEqual(proof["github"]["assets"][name]["sha256"], candidate.digest(self.asset_bytes[name]))
        # Current default branch is deliberately never queried; historical snapshots remain authoritative.
        # 刻意不查询当前默认分支；历史快照保持权威。
        self.assertFalse(any("/git/ref/heads/" in url for url in self.http.calls))

    def test_draft_source_evidence_wrong_tree_and_missing_asset_close_gate(self):
        """A valid release cannot excuse forged historical workflow ownership or absent draft proof.
        有效发布不能豁免伪造历史工作流归属或缺失草稿证明。
        """
        name = gate.create_draft.DRAFT_SOURCE_EVIDENCE_ASSETS[0]
        evidence = candidate.decode_json(self.asset_bytes[name])
        evidence["default_root_tree"] = "0" * 40
        self.replace_asset(name, candidate.encode(evidence))
        self.reject()
        del self.assets[name]
        with self.assertRaises(ValueError):
            self.run_gate(name="missing-draft-proof")

    def test_normalized_registry_feature_change_is_rejected_by_official_cargo(self):
        """Correct name/version, source/VCS and checksum cannot hide a changed default feature set.
        正确名称、版本、源码、VCS 及摘要不能隐藏改变的默认功能集合。
        """
        # Preserve all original manifest fields except the explicit registry-only default feature mutation.
        # 保留所有原始清单字段，仅显式变异 registry 默认功能。
        original = self.source["Cargo.toml"]
        self.assertIn(b"[features]", original)
        self.crate_files[f"luaskills-{self.version}/Cargo.toml"] = original.replace(b"[features]", b'[features]\ndefault = ["contract-generation"]', 1)
        self.crate = candidate.archive_bytes(self.crate_files)
        self.crate_record["checksum"] = candidate.digest(self.crate)
        with self.assertRaisesRegex(ValueError, "compilation semantics"):
            self.run_gate("complete")
        self.assertFalse((self.base / "gate/prerequisites.json").exists())

    def test_normalization_cargo_release_drift_fails_before_packaging(self):
        """The consumer's actual Cargo must match the frozen package evidence rather than inferred rustc version.
        消费者实际 Cargo 必须匹配冻结包证据，而非推断的 rustc 版本。
        """
        # Alternate real-shaped output changes only Cargo's actual declared release, keeping compiler unchanged.
        # 另一真实形状输出仅改变 Cargo 实际声明版本，编译器保持不变。
        previous_release = self.toolchain_record["cargo"]["release"]
        self.toolchain_record["cargo"]["release"] = "0.0.0"
        self.toolchain_record["cargo"]["version_verbose"] = self.toolchain_record["cargo"]["version_verbose"].replace(f"cargo {previous_release} (", "cargo 0.0.0 (").replace(f"release: {previous_release}", "release: 0.0.0")
        with self.assertRaises(ValueError):
            self.run_gate("complete")
        self.assertFalse((self.base / "gate/registry/normalization-package.stdout").exists())

    def test_old_record_missing_actual_cargo_proof_is_not_inferred_from_rustc(self):
        """Require actual Cargo provenance even when a previous record includes full compiler evidence.
        即使旧记录含完整编译器证据，也必须要求实际 Cargo 来源证明。
        """
        name = f"candidate-{next(iter(candidate.PLATFORMS))}.json"
        record = candidate.decode_json(self.asset_bytes[name])
        del record["cargo"]
        self.replace_asset(name, candidate.encode(record))
        self.reject("complete")

    def test_unicode_frozen_manifest_is_read_with_utf8_mode_disabled(self):
        """Exercise real frozen-manifest reading in a separate non-UTF8-mode process, with fake Cargo clearly bounded.
        在独立非 UTF8 模式进程中验证真实冻结清单读取，并明确限定模拟 Cargo 边界。
        """
        # Real Unicode includes an emoji that the Windows CP936 locale cannot decode as UTF-8 bytes.
        # 真实 Unicode 包含表情符号，Windows CP936 区域编码无法按 UTF-8 字节解码。
        manifest = self.source["Cargo.toml"] + '\n[package.metadata.encoding_probe]\ntext = "中文🙂"\n'.encode("utf-8")
        # Frozen source remains a real tar, with the existing lockfile and all source bytes intact.
        # 冻结源码保持真实 tar，既有锁文件及所有源码字节完整。
        files = {**self.source, "Cargo.toml": manifest}
        source_path = self.base / "unicode-source.tar.gz"
        candidate.write_new(source_path, candidate.archive_bytes(files))
        directory = self.base / "unicode-registry"
        candidate.write_new(directory / f"luaskills-{self.version}.crate", candidate.archive_bytes({f"luaskills-{self.version}/Cargo.toml":manifest}))
        record_path = self.base / "unicode-record.json"
        candidate.write_new(record_path, candidate.encode(candidate.decode_json(self.asset_bytes["candidate-manifest.json"])["platforms"]))
        # Driver imports the actual production normalization function instead of duplicating its text-read line.
        # 驱动导入实际生产规范化函数，不复制其文本读取代码行。
        driver = self.base / "unicode-driver.py"
        script = f"import sys\nsys.path.insert(0, {str(Path(gate.__file__).parent)!r})\n" + r'''import json
import locale
from pathlib import Path
import subprocess
import candidate
import sdk_prerequisites as gate

# Input files and toolchain data are explicit offline fixtures; no real Cargo or publication runs.
# 输入文件及工具链数据是明确离线夹具；不运行真实 Cargo 或发布。
source, directory, record_file, version = sys.argv[1:]
records = candidate.read_json(Path(record_file))
record = records[0]

def fixture_runner(command, project, env):
    """Simulate only the Cargo boundary; the production UTF-8 manifest reader executes unchanged.
    仅模拟 Cargo 边界；生产 UTF-8 清单读取器原样执行。
    """
    if command == ["rustc", "--version", "--verbose"]:
        return subprocess.CompletedProcess(command, 0, record["build"]["rustc"].encode("utf-8"), b"")
    if command == ["cargo", "--version", "--verbose"]:
        return subprocess.CompletedProcess(command, 0, record["cargo"]["version_verbose"].encode("utf-8"), b"")
    assert command == ["cargo", "package", "--locked", "--no-verify", "-j", "4"]
    # This point is reached only after production read_text and minimum-version validation succeeded.
    # 仅当生产 read_text 及最低版本验证成功后，才能到达此处。
    manifest = (project / "Cargo.toml").read_bytes()
    parsed = candidate.tomllib.loads(manifest.decode("utf-8"))
    assert parsed["package"]["metadata"]["encoding_probe"]["text"] == "".join(map(chr, (0x4e2d, 0x6587, 0x1f642)))
    candidate.write_new(Path(env["CARGO_TARGET_DIR"]) / "package" / f"luaskills-{version}.crate", candidate.archive_bytes({f"luaskills-{version}/Cargo.toml":manifest}))
    return subprocess.CompletedProcess(command, 0, b"explicit offline package fixture", b"")

assert sys.flags.utf8_mode == 0
proof = gate.normalize_registry_manifest(Path(directory), version, Path(source), records, fixture_runner)
print(json.dumps({"utf8_mode":sys.flags.utf8_mode,"locale_encoding":locale.getencoding(),"minimum":proof["rust_version_minimum"]}))
'''
        candidate.write_new(driver, script.encode("utf-8"))
        # A real Python child with -X utf8=0 exposes CP936 on Windows rather than inheriting the test runner's mode.
        # 真实 Python 子进程以 -X utf8=0 在 Windows 暴露 CP936，不继承测试运行器模式。
        result = subprocess.run([sys.executable, "-X", "utf8=0", str(driver), str(source_path), str(directory), str(record_path), self.version], capture_output=True, timeout=30, check=False)
        self.assertEqual(result.returncode, 0, result.stderr.decode("utf-8", errors="replace"))
        proof = candidate.decode_json(result.stdout)
        self.assertEqual(proof["utf8_mode"], 0)
        self.assertEqual(proof["minimum"], candidate.tomllib.loads(self.source["Cargo.toml"].decode("utf-8"))["package"]["rust-version"])

    def test_recheck_accepts_legitimate_dynamic_zip_recompression(self):
        """Same validated commit members remain accepted when GitHub rebuilds its zip compression bytes.
        GitHub 重建 zip 压缩字节时，相同已验证提交成员仍应通过。
        """
        previous = self.run_gate("complete")
        buffer = io.BytesIO()
        with zipfile.ZipFile(buffer, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=1) as archive:
            for name, body in self.source.items():
                archive.writestr("official-commit/" + name, body)
        self.commit_zip = buffer.getvalue()
        with contextlib.redirect_stdout(io.StringIO()):
            fresh = gate.recheck(self.base / "gate/prerequisites.json", self.base / "recompressed", http=self.http, runner=self.runner)
        self.assertNotEqual(previous["github"]["commit_archive_sha256"], fresh["github"]["commit_archive_sha256"])
        self.assertEqual(previous["github"]["commit_source_sha256"], fresh["github"]["commit_source_sha256"])

    def test_recursive_annotated_tag_uses_object_commit(self):
        """Peel two annotation levels while ignoring a misleading target_commitish string.
        解开两层附注，同时忽略误导的 target_commitish 字符串。
        """
        self.annotated = True
        self.assertEqual([entry["type"] for entry in self.run_gate()["github"]["tag_resolution"]], ["tag", "tag", "commit"])

    def test_complete_requires_actual_command_and_runtime_challenge(self):
        """Test complete orchestration with explicitly synthetic process boundaries, never real-Cargo claims.
        使用明确合成进程边界测试完整编排，绝不声明真实 Cargo 验收。
        """
        proof = self.run_gate("complete")
        self.assertTrue(proof["complete"])
        consumer = proof["registry"]["consumer"]
        self.assertEqual(len(consumer["commands"]), 4)
        self.assertEqual(consumer["result"]["capability_calls"], 2)
        self.assertEqual(candidate.digest(Path(consumer["executable"]).read_bytes()), consumer["executable_sha256"])

    def test_draft_and_prerelease_are_rejected(self):
        """Reject either mutable draft status or final-version prerelease status before asset work.
        在资产处理前拒绝可变草稿或正式版本的预发行状态。
        """
        for flag in ("draft", "prerelease"):
            with self.subTest(flag=flag):
                self.release[flag] = True
                with self.assertRaises(ValueError):
                    gate.github_snapshot(self.http, self.tag, self.commit)
                self.release[flag] = False

    def test_missing_platform_record_closes_gate(self):
        """Reject missing supported-platform evidence rather than accepting a partial release.
        拒绝缺失受支持平台证据，不接受局部发布。
        """
        del self.assets[f"candidate-{next(iter(candidate.PLATFORMS))}.json"]
        self.reject()

    def test_duplicate_asset_ids_across_pages_close_gate(self):
        """Reject different names sharing one asset ID across pagination.
        拒绝分页中不同名称共享一个资产 ID。
        """
        values = list(self.assets.values())
        values[-1]["id"] = values[0]["id"]
        values[-1]["url"] = values[0]["url"]
        self.reject()

    def test_archive_actual_checksum_mismatch_closes_gate(self):
        """Tamper actual archive bytes while retaining original manifest/sidecar evidence.
        篡改实际归档字节，同时保留原清单及 sidecar 证据。
        """
        name = next(name for name in self.assets if name.startswith("luaskills-ffi-sdk-"))
        self.replace_asset(name, self.asset_bytes[name] + b"tampered")
        self.reject()

    def test_frozen_source_must_equal_official_commit_zip(self):
        """Reject a mismatched official commit member independently from candidate's self-consistency.
        独立于候选自身一致性，拒绝不同的正式提交成员。
        """
        buffer = io.BytesIO()
        with zipfile.ZipFile(buffer, "w") as archive:
            for name, body in self.source.items():
                archive.writestr("official-commit/" + name, b"wrong-source" if name == "build.rs" else body)
        self.commit_zip = buffer.getvalue()
        self.reject()

    def test_tag_drift_after_download_closes_gate(self):
        """Reject a tag moved after the initial successful snapshot and candidate verification.
        拒绝初次成功快照及候选验证后移动的标签。
        """
        self.tag_drift = True
        self.reject()

    def test_asset_bytes_drift_even_with_unchanged_metadata_closes_gate(self):
        """Re-download official bytes even when asset ID, length and updated_at remain unchanged.
        即使资产 ID、长度及更新时间未变，也重新下载正式字节。
        """
        self.byte_drift = "candidate-manifest.json"
        self.reject()

    def test_registry_yanked_and_bad_checksum_close_gate(self):
        """Reject exact registry version revocation and mismatched actual crate checksum.
        拒绝精确 registry 版本撤销及不匹配的实际 crate 摘要。
        """
        self.crate_record["yanked"] = True
        self.reject("complete")
        self.crate_record["yanked"] = False
        self.crate_record["checksum"] = "0" * 64
        with self.assertRaises(ValueError):
            self.run_gate("complete", "bad-checksum")

    def test_registry_vcs_commit_and_source_bytes_are_independent_guards(self):
        """Correct API checksum cannot excuse incorrect VCS SHA or changed source bytes.
        正确 API 摘要不能豁免错误 VCS SHA 或改变的源码字节。
        """
        for mode in ("vcs", "source"):
            with self.subTest(mode=mode):
                files = copy.deepcopy(self.crate_files)
                if mode == "vcs":
                    files[f"luaskills-{self.version}/.cargo_vcs_info.json"] = candidate.encode({"git":{"sha1":"c"*40},"path_in_vcs":""})
                else:
                    files[f"luaskills-{self.version}/build.rs"] = b"different-build-source"
                self.crate = candidate.archive_bytes(files)
                self.crate_record["checksum"] = candidate.digest(self.crate)
                with self.assertRaises(ValueError):
                    self.run_gate("complete", mode)

    def test_success_text_cannot_override_failed_process_or_missing_artifact(self):
        """Reject nonzero process exits, missing real binary and stale challenge output.
        拒绝非零进程退出、缺失真实二进制及旧随机挑战输出。
        """
        for mode in ("failed-with-success-output", "no-artifact", "pasted", "path-lock"):
            with self.subTest(mode=mode):
                self.process_failure = mode
                with self.assertRaises((ValueError, KeyError)):
                    self.run_gate("complete", mode)
                self.assertFalse((self.base / mode / "prerequisites.json").exists())

    def test_recheck_ignores_forged_old_consumer_and_reruns_real_boundary(self):
        """Old complete reports never replace newly executed process verification.
        旧完整报告绝不替代新执行的进程验证。
        """
        proof = self.run_gate("complete")
        proof["registry"]["consumer"] = {"result":"fabricated-success"}
        (self.base / "gate/prerequisites.json").write_bytes(candidate.encode(proof))
        self.process_failure = "failed-with-success-output"
        with contextlib.redirect_stdout(io.StringIO()), self.assertRaises(ValueError):
            gate.recheck(self.base / "gate/prerequisites.json", self.base / "recheck", http=self.http, runner=self.runner)
        self.assertFalse((self.base / "recheck/prerequisites.json").exists())

    def test_recheck_rejects_changed_release_asset_id(self):
        """Same name/hash bytes under a replaced asset ID violate publication identity.
        同名及同摘要字节更换资产 ID 时违反发布身份。
        """
        self.run_gate("complete")
        name = "candidate-manifest.json"
        self.assets[name]["id"] = 99999
        self.assets[name]["url"] = f"{gate.REPO_API}/releases/assets/99999"
        with contextlib.redirect_stdout(io.StringIO()), self.assertRaises(ValueError):
            gate.recheck(self.base / "gate/prerequisites.json", self.base / "recheck", http=self.http, runner=self.runner)
        self.assertFalse((self.base / "recheck/prerequisites.json").exists())

    def test_network_error_and_existing_output_fail_closed(self):
        """Missing endpoint and reused output cannot yield an accepted proof or overwrite any files.
        缺失端点及复用输出不能产生已验收证明或覆盖任何文件。
        """
        self.http_errors.add(gate.REPO_API)
        self.reject()
        sentinel = self.base / "gate/sentinel"
        sentinel.write_bytes(b"keep")
        with self.assertRaises(ValueError):
            self.run_gate()
        self.assertEqual(sentinel.read_bytes(), b"keep")

    def test_rooted_tar_and_zip_reject_traversal_and_duplicate_members(self):
        """Keep source package readers safe against duplicate/traversal members in both real formats.
        使源码包读取器安全拒绝两个真实格式中的重复或遍历成员。
        """
        for zipped in (False, True):
            with self.subTest(zipped=zipped):
                if zipped:
                    buffer = io.BytesIO()
                    with zipfile.ZipFile(buffer, "w") as archive:
                        archive.writestr("package/../escape", b"bad")
                    body = buffer.getvalue()
                else:
                    body = candidate.archive_bytes({"package/../escape":b"bad"})
                with self.assertRaises(ValueError):
                    gate.rooted_archive(body, "package", zipped)
                # Duplicate regular members are represented by the real archive formats, not a map.
                # 重复普通成员由真实归档格式表示，而非映射。
                buffer = io.BytesIO()
                if zipped:
                    with warnings.catch_warnings(), zipfile.ZipFile(buffer, "w") as archive:
                        warnings.simplefilter("ignore", UserWarning)
                        archive.writestr("package/file", b"first")
                        archive.writestr("package/file", b"second")
                else:
                    with tarfile.open(fileobj=buffer, mode="w:gz") as archive:
                        for content in (b"first", b"second"):
                            # Exact repeated member name catches archive-order overwrite impersonation.
                            # 精确重复成员名称捕获归档顺序覆盖冒充。
                            member = tarfile.TarInfo("package/file")
                            member.size = len(content)
                            archive.addfile(member, io.BytesIO(content))
                with self.assertRaises(ValueError):
                    gate.rooted_archive(buffer.getvalue(), "package", zipped)

    def test_redirect_never_forwards_github_token(self):
        """Exercise actual urllib redirect construction at the authenticated cross-host boundary.
        在已认证跨主机边界验证实际 urllib 重定向构造。
        """
        request = gate.urllib.request.Request("https://api.github.com/releases/assets/1", headers={"Authorization":"Bearer synthetic-secret"})
        redirected = gate.SafeRedirect().redirect_request(request, None, 302, "Found", {}, "https://release-assets.githubusercontent.com/asset")
        self.assertIsNone(redirected.get_header("Authorization"))
        with self.assertRaises(ValueError):
            gate.SafeRedirect().redirect_request(request, None, 302, "Found", {}, "http://release-assets.githubusercontent.com/asset")

    def test_portable_resolution_uses_verified_fixed_files_after_artifact_move(self):
        """Move the artifact while retaining old paths; derive new paths only from verified fixed layout.
        移动产物并保留旧路径；仅从已验证固定布局派生新路径。
        """
        proof = self.run_gate()
        moved = self.base / "transported"
        shutil.copytree(self.base / "gate", moved)
        platform = next(iter(candidate.PLATFORMS))
        with contextlib.redirect_stdout(io.StringIO()):
            inputs = gate.resolve_sdk_inputs(moved / "prerequisites.json", platform)
        self.assertNotEqual(inputs["library"], proof["sdk_inputs"][platform]["library"])
        self.assertTrue(Path(inputs["library"]).is_relative_to(moved))
        self.assertEqual(inputs["build"], proof["sdk_inputs"][platform]["build"])
        Path(inputs["description"]).write_bytes(b"forged-transported-description")
        with contextlib.redirect_stdout(io.StringIO()), self.assertRaises(ValueError):
            gate.resolve_sdk_inputs(moved / "prerequisites.json", platform)

    def test_registry_missing_build_input_cannot_claim_same_vcs_commit(self):
        """The VCS SHA and valid outer checksum do not excuse a missing identity input file.
        VCS SHA 及有效外层摘要不能豁免缺失身份输入文件。
        """
        del self.crate_files[f"luaskills-{self.version}/build.rs"]
        self.crate = candidate.archive_bytes(self.crate_files)
        self.crate_record["checksum"] = candidate.digest(self.crate)
        self.reject("complete")

    def test_source_sidecar_mismatch_is_not_hidden_by_local_regeneration(self):
        """Check original official source sidecar even though aggregate writes its own verified sidecar.
        即使 aggregate 写入自身已验证 sidecar，也要检查原正式源码 sidecar。
        """
        name = f"luaskills-source-{self.version}-{self.commit}.tar.gz.sha256"
        self.replace_asset(name, f"{'0'*64}  {name[:-7]}\n".encode())
        self.reject()

    def test_malformed_version_schema_and_github_only_recheck_fail_closed(self):
        """Missing registry schema and incomplete phase cannot masquerade as publication approval.
        缺失 registry 结构及不完整阶段不能冒充发布批准。
        """
        self.run_gate()
        with self.assertRaises(ValueError):
            gate.recheck(self.base / "gate/prerequisites.json", self.base / "reject-incomplete", http=self.http, runner=self.runner)
        del self.crate_record["checksum"]
        with self.assertRaises(KeyError):
            self.run_gate("complete", "missing-schema")
        self.assertFalse((self.base / "missing-schema/prerequisites.json").exists())

    def test_actual_process_timeout_reclaims_descendant_output_handles(self):
        """Run real sleeping Python parent/child processes; timeout must close inherited pipe handles.
        运行真实休眠 Python 父子进程；超时必须关闭继承的管道句柄。
        """
        # Both processes inherit stdout, so killing only the parent cannot finish communicate promptly.
        # 两个进程均继承 stdout，因此仅杀死父进程无法使 communicate 及时完成。
        command = [sys.executable, "-c", "import subprocess,sys,time; subprocess.Popen([sys.executable,'-c','import time;time.sleep(60)']); print('child-started',flush=True);time.sleep(60)"]
        # Clock measures the actual OS process termination rather than a fabricated CompletedProcess.
        # 时钟测量实际操作系统进程终止，而非伪造的 CompletedProcess。
        started = time.monotonic()
        with patch.object(gate, "PROCESS_TIMEOUT_SECONDS", 1), self.assertRaisesRegex(ValueError, "bounded process deadline"):
            gate.run_process(command, self.base, dict(os.environ))
        self.assertLess(time.monotonic() - started, 10, "Timed-out descendants retained output pipes")

    @unittest.skipUnless(os.name == "nt", "Windows adapter exercises the POSIX Python branch with real process cleanup")
    def test_posix_timeout_branch_resolves_signal_before_real_windows_cleanup(self):
        """Exercise POSIX timeout name resolution on Windows while actually terminating the process tree.
        在 Windows 验证 POSIX 超时名称解析，同时实际终止进程树。
        """
        # A real sleeping process ensures this is timeout execution, not a successful process fixture.
        # 真实休眠进程确保这是超时执行，而非成功进程夹具。
        command = [sys.executable, "-c", "import time;time.sleep(60)"]
        # Windows lacks POSIX SIGKILL; this opaque sentinel tests name/argument resolution only.
        # Windows 缺少 POSIX SIGKILL；此不透明哨兵仅测试名称及参数解析。
        posix_signal = object()
        # Captured real process handles allow unconditional cleanup after any test failure.
        # 捕获的真实进程句柄使任何测试失败后均能无条件清理。
        started_processes = []
        # Preserve the actual constructor so this test never fabricates process success or output.
        # 保留实际构造器，使此测试绝不伪造进程成功或输出。
        real_popen = subprocess.Popen

        def start_process(*arguments, **keywords):
            """Forward all constructor arguments to actual Popen; return and retain its process handle.
            将所有构造参数转发至实际 Popen；返回并保留进程句柄。
            """
            # Every handle represents a real OS process rather than a CompletedProcess fixture.
            # 每个句柄均表示真实操作系统进程，而非 CompletedProcess 夹具。
            process = real_popen(*arguments, **keywords)
            started_processes.append(process)
            return process

        def kill_group(pid, kill_signal):
            """Validate actual POSIX signal argument, then kill Windows pid tree; return no result.
            验证实际 POSIX signal 参数，再终止 Windows pid 进程树；不返回结果。
            """
            self.assertIs(kill_signal, posix_signal)
            subprocess.run(["taskkill", "/PID", str(pid), "/T", "/F"], check=True, capture_output=True, timeout=10)

        try:
            with patch.object(gate.subprocess, "Popen", start_process), patch.object(gate, "PROCESS_TIMEOUT_SECONDS", 1), patch.object(gate.os, "name", "posix"), patch.object(gate.os, "killpg", kill_group, create=True), patch.object(gate.signal, "SIGKILL", posix_signal, create=True), self.assertRaisesRegex(ValueError, "bounded process deadline"):
                gate.run_process(command, self.base, dict(os.environ))
        finally:
            for process in started_processes:
                if process.poll() is None:
                    subprocess.run(["taskkill", "/PID", str(process.pid), "/T", "/F"], check=True, capture_output=True, timeout=10)
                process.communicate(timeout=10)


if __name__ == "__main__":
    unittest.main()
