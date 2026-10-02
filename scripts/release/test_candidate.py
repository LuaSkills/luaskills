"""Offline fixtures test candidate completeness and identity failures without Cargo or publication.
离线夹具验证候选完整性及身份失败，无需 Cargo 或发布。
"""

import argparse
import gzip
import io
import json
from pathlib import Path
import re
import shutil
import subprocess
import tarfile
import tempfile
import sys
import unittest
from unittest.mock import patch
import urllib.error
import urllib.parse

import candidate
import create_draft


class CandidateTests(unittest.TestCase):
    """Exercise all mandatory platform gates using explicitly synthetic native library fixture bytes.
    使用明确的合成原生库夹具字节覆盖全部必需平台门禁。
    """

    @classmethod
    def setUpClass(cls):
        """Read the actual HEAD source archive once; do not claim fixtures are real native builds.
        仅读取一次实际 HEAD 源码归档；不宣称夹具是真实原生构建。
        """
        # Root is the current repository, used only for read-only source evidence.
        # 根是当前仓库，仅用于只读源码证据。
        cls.root = Path(__file__).resolve().parents[2]
        # Commit identifies the real source archive; workflow-history tests create commits only in disposable repositories.
        # 提交标识真实源码归档；工作流历史测试仅在可丢弃仓库中创建提交。
        cls.commit = candidate.git(cls.root, "rev-parse", "HEAD").decode().strip()
        # Archive is the exact committed source snapshot rather than a dirty-worktree SHA claim.
        # 归档是精确已提交源码快照，而非脏工作树的 SHA 声明。
        cls.source = gzip.compress(candidate.git(cls.root, "archive", "--format=tar", cls.commit), mtime=0)
        with tempfile.TemporaryDirectory() as directory:
            # Path stores the immutable read-only archive for normal archive parsing.
            # 路径保存不可变只读归档，供正常归档解析使用。
            path = Path(directory) / "source.tar.gz"
            path.write_bytes(cls.source)
            cls.source_files = candidate.archive_files(path)
        # Version is taken from frozen Cargo.toml, not hard-coded future release data.
        # 版本来自冻结 Cargo.toml，不硬编码未来发布数据。
        cls.version = candidate.tomllib.loads(cls.source_files["Cargo.toml"].decode())["package"]["version"]
        # Contract is the actual offline contract in the frozen archive.
        # 契约是冻结归档中的实际离线契约。
        cls.contract = json.loads(cls.source_files["contracts/embedded/v1/contract.json"])
        # Roots are read from the same authoritative declaration as real candidate validation.
        # 根来自与真实候选验证相同的权威声明。
        declaration = cls.source_files["build_support/identity.rs"].decode()
        # Match identifies the actual declared list; test fixtures preserve the runtime-owned structure.
        # 匹配标识实际声明列表；测试夹具保留运行时拥有的结构。
        match = re.search(r"pub const INPUT_ROOTS: &\[&str\] = &\[(.*?)\];", declaration, re.S)
        # InputRoots and Format come from actual Rust source rather than release-script assumptions.
        # InputRoots 及 Format 来自实际 Rust 源码，而非发布脚本假设。
        roots = re.findall(r'"([^"]+)"', match.group(1))
        cls.format = int(re.search(r"pub const INPUTS_FORMAT_VERSION: u32 = (\d+);", declaration).group(1))
        cls.inputs = {name: candidate.digest(content) for name, content in cls.source_files.items() if any(name == root or name.startswith(root + "/") for root in roots)}

    def setUp(self):
        """Allocate isolated temporary fixture paths; no workspace output is written.
        分配隔离临时夹具路径；不写入工作区产物。
        """
        # Temporary directory is owned only by this test and cleaned by unittest.
        # 临时目录仅由此测试拥有，并由 unittest 清理。
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        # Base stores fixture input and aggregate outputs.
        # Base 保存夹具输入及汇总输出。
        self.base = Path(self.temporary.name)
        self.input = self.base / "input"
        self.output = self.base / "aggregate"

    def platform_fixture(self, platform):
        """Build synthetic platform files with real frozen source hashes; return the package files and manifest.
        使用真实冻结源码摘要构建合成平台文件；返回包文件及清单。
        """
        # Target comes from the mandatory release platform declaration.
        # 目标来自必需发布平台声明。
        triple, operating_system, architecture, names = candidate.PLATFORMS[platform]
        # Fields intentionally describe a fixture compiler, preventing confusion with a real build.
        # 字段刻意描述夹具编译器，避免与真实构建混淆。
        fields = {"source_sha256": candidate.source_digest(self.inputs, self.format), "contract_sha256": candidate.digest(self.source_files["contracts/embedded/v1/contract.json"]), "package_lock_sha256": candidate.digest(self.source_files["Cargo.lock"]), "target": triple, "target_os": operating_system, "target_arch": architecture, "pointer_width": "64", "opt_level": "3", "debug_info": "false", "rustflags_sha256": candidate.digest(b""), "rustc": "explicit offline fixture compiler"}
        # Report preserves build.rs ownership and original machine-readable shape.
        # 报告保留 build.rs 归属及原始机器可读形状。
        report = candidate.encode({"format_version": self.format, "fields": fields, "cargo_features": [], "source_files": self.inputs})
        # Build adds only the runtime's actual report digest and feature field.
        # 构建身份仅增加运行时实际报告摘要及功能字段。
        build = {**fields, "inputs_sha256": candidate.digest(report), "cargo_features": []}
        # Description uses actual runtime field names and the actual offline contract commands.
        # 描述使用实际运行时字段名及实际离线契约命令。
        description = {"core_version": self.version, "description_version": self.contract["compatibility"]["description_version"], "protocol_version": self.contract["protocol_version"], "abi_structure_version": self.contract["protocol_version"], "commands": self.contract["commands"], "runtime_commands": self.contract["runtime_commands"], "capabilities": self.contract["compatibility"]["required_capabilities"], "execution_backends": ["in_process"], "build": build}
        # Files combine real contract/header bytes with explicitly synthetic library bytes.
        # 文件将真实契约及头文件字节与明确的合成库字节组合。
        files = {name: content for name, content in self.source_files.items() if name.startswith("include/") and name.endswith(".h")}
        for name in names:
            files[f"lib/{name}"] = f"synthetic offline fixture for {platform}/{name}".encode()
        files["contracts/embedded/v1/contract.json"] = self.source_files["contracts/embedded/v1/contract.json"]
        files["licenses/Cargo.lock"] = self.source_files["Cargo.lock"]
        files["embedded-build-inputs.json"] = report
        files["embedded-core-description.json"] = candidate.encode(description)
        # Cargo identity explicitly belongs to a synthetic fixture, while its release/hash are shared across all native hosts.
        # Cargo 身份显式属于合成夹具，其版本及提交摘要在全部原生宿主间共享。
        cargo_bytes = f"cargo 1.94.0 (111111111 2026-01-01)\nrelease: 1.94.0\ncommit-hash: {'1' * 40}\nhost: {triple}\nos: explicit offline fixture\n".encode()
        files[candidate.CARGO_VERSION_EVIDENCE_FILE] = cargo_bytes
        # Manifest is release-owned evidence for these fixtures, not a native build attestation.
        # 清单是这些夹具的发布流程证据，不是原生构建证明。
        manifest = {"schema_version": candidate.MANIFEST_VERSION, "platform": platform, "source_commit": self.commit, "core_version": self.version, "source_archive_sha256": candidate.digest(self.source), "build": build, "cargo": candidate.cargo_identity(cargo_bytes), "files": {name: candidate.digest(content) for name, content in files.items()}}
        return files, manifest

    def write_platform(self, platform):
        """Create one complete four-archive fixture and collect it; return its directory.
        创建一个完整四归档夹具并收集它；返回其目录。
        """
        # Directory separates each workflow artifact's transferred files.
        # 目录区分每个工作流产物的传输文件。
        directory = self.input / platform
        directory.mkdir(parents=True)
        # Files and manifest share one exact package identity.
        # 文件及清单共享一个精确包身份。
        files, manifest = self.platform_fixture(platform)
        files["ffi-sdk-manifest.json"] = candidate.encode(manifest)
        (directory / f"luaskills-ffi-sdk-{platform}.tar.gz").write_bytes(candidate.archive_bytes(files))
        self.write_auxiliary(directory, platform, files)
        (directory / f"luaskills-source-{self.version}-{self.commit}.tar.gz").write_bytes(self.source)
        candidate.collect(argparse.Namespace(output=directory, source_commit=self.commit, version=self.version, platform=platform))
        return directory

    def write_auxiliary(self, directory, platform, core_files):
        """Write real-shaped auxiliary archive fixtures with a pinned Rust dependency and matching FFI library bytes.
        写入真实形状的附属归档夹具，包含冻结 Rust 依赖及匹配的 FFI 库字节。
        """
        for mode in ("ffi", "rust"):
            # Files preserve the exact current demo manifest fields and candidate dependency shape.
            # 文件保留精确当前 demo 清单字段及候选依赖形状。
            files = {"demo-manifest.json": candidate.encode({"platform": platform, "mode": mode, "release_tag": f"v{self.version}"})}
            if mode == "ffi":
                for name in candidate.PLATFORMS[platform][3]:
                    files[f"lib/{name}"] = core_files[f"lib/{name}"]
            else:
                files["Cargo.toml"] = f'[dependencies]\nluaskills = {{ git = "https://github.com/LuaSkills/luaskills.git", rev = "{self.commit}" }}\n'.encode()
            (directory / f"luaskills-demo-{mode}-{platform}.tar.gz").write_bytes(candidate.archive_bytes(files))
        # Binary follows the same actual OS-derived path as the debug packager.
        # 二进制遵循调试打包器相同的实际 OS 派生路径。
        binary = "bin/luaskills-debug.exe" if candidate.PLATFORMS[platform][1] == "windows" else "bin/luaskills-debug"
        (directory / f"luaskills-debug-tool-{platform}.tar.gz").write_bytes(candidate.archive_bytes({"debug-tool-manifest.json": candidate.encode({"platform": platform, "release_tag": f"v{self.version}", "binary": binary}), binary: b"explicit debug executable fixture"}))

    def all_platforms(self):
        """Write every mandatory native-platform fixture; return aggregate arguments.
        写入每个必需原生平台夹具；返回汇总参数。
        """
        for platform in candidate.PLATFORMS:
            self.write_platform(platform)
        return argparse.Namespace(input=self.input, output=self.output, source_commit=self.commit, version=self.version)

    def test_five_platform_roundtrip_and_sdk_inputs(self):
        """Verify successful aggregation, exact draft asset set and SDK input identity.
        验证成功汇总、精确草稿资产集合及 SDK 输入身份。
        """
        candidate.aggregate(self.all_platforms())
        self.assertEqual(len(create_draft.verified_assets(self.output, self.commit, self.version)), 48)
        candidate.sdk_inputs(argparse.Namespace(input=self.output, output=self.base / "sdk", source_commit=self.commit, version=self.version, platform="windows-x64"))
        # Inputs retain the exact full descriptor and library hash expected by each SDK gate.
        # 输入保留每个 SDK 门禁期望的精确完整描述及库摘要。
        inputs = candidate.read_json(self.base / "sdk/sdk-validation-inputs.json")
        self.assertEqual(candidate.digest(Path(inputs["library"]).read_bytes()), inputs["library_sha256"])
        self.assertEqual(candidate.read_json(Path(inputs["description"]))["build"], inputs["build"])

    def test_missing_platform_fails(self):
        """Reject an incomplete platform set before creating aggregate output.
        创建汇总产物前拒绝不完整平台集合。
        """
        self.write_platform("windows-x64")
        with self.assertRaisesRegex(ValueError, "every supported platform"):
            candidate.aggregate(argparse.Namespace(input=self.input, output=self.output, source_commit=self.commit, version=self.version))
        self.assertFalse(self.output.exists())

    def test_sdk_inputs_reject_incomplete_aggregate(self):
        """Refuse native SDK staging from a manually truncated aggregate manifest.
        拒绝从手动截断的汇总清单暂存原生 SDK 输入。
        """
        candidate.aggregate(self.all_platforms())
        # Path holds the fixture aggregate metadata, changed without touching real workspace evidence.
        # 路径保存夹具汇总元数据，修改不触及真实工作区证据。
        path = self.output / "candidate-manifest.json"
        # Manifest intentionally removes every platform except Windows.
        # 清单刻意移除 Windows 之外的每个平台。
        manifest = candidate.read_json(path)
        manifest["platforms"] = [record for record in manifest["platforms"] if record["platform"] == "windows-x64"]
        path.write_bytes(candidate.encode(manifest))
        with self.assertRaisesRegex(ValueError, "complete five-platform"):
            candidate.sdk_inputs(argparse.Namespace(input=self.output, output=self.base / "sdk", source_commit=self.commit, version=self.version, platform="windows-x64"))
        self.assertFalse((self.base / "sdk").exists())

    def test_auxiliary_rust_demo_rejects_future_tag_dependency(self):
        """Reject an otherwise valid demo set when Rust dependency still points at a future release tag.
        当 Rust 依赖仍指向未来发布标签时，拒绝其余有效的 demo 集合。
        """
        # Directory and core files preserve the original fixture's expected native library bytes.
        # 目录及核心文件保留原始夹具预期原生库字节。
        directory = self.write_platform("windows-x64")
        core_files, _ = self.platform_fixture("windows-x64")
        # Rust archive changes only its dependency revision kind while keeping the original tag/platform manifest.
        # Rust 归档仅改变依赖修订类型，同时保留原始标签及平台清单。
        archive = directory / "luaskills-demo-rust-windows-x64.tar.gz"
        files = candidate.archive_files(archive)
        files["Cargo.toml"] = f'[dependencies]\nluaskills = {{ git = "https://github.com/LuaSkills/luaskills.git", tag = "v{self.version}" }}\n'.encode()
        archive.write_bytes(candidate.archive_bytes(files))
        with self.assertRaisesRegex(ValueError, "same frozen source commit"):
            candidate.verify_auxiliary(directory, "windows-x64", self.commit, self.version, core_files)

    def test_duplicate_platform_fails(self):
        """Reject duplicate evidence despite its identical platform and source labels.
        即使平台及源码标签相同，也拒绝重复证据。
        """
        # Arguments identify the same exact candidate throughout this test.
        # 参数在此测试全程标识同一精确候选。
        args = self.all_platforms()
        (self.input / "candidate-windows-x64.json").write_bytes((self.input / "windows-x64/candidate-windows-x64.json").read_bytes())
        with self.assertRaisesRegex(ValueError, "Duplicate platform"):
            candidate.aggregate(args)

    def test_cross_commit_fails(self):
        """Reject one platform record tied to a different source commit.
        拒绝绑定到不同源码提交的一个平台记录。
        """
        # Arguments remain tied to the real archive commit.
        # 参数保持绑定到真实归档提交。
        args = self.all_platforms()
        # Path and record are intentionally altered only inside the temporary fixture.
        # 路径及记录仅在临时夹具中刻意修改。
        path = self.input / "windows-x64/candidate-windows-x64.json"
        record = candidate.read_json(path)
        record["source_commit"] = "0" * 40
        path.write_bytes(candidate.encode(record))
        with self.assertRaisesRegex(ValueError, "source/version mismatch"):
            candidate.aggregate(args)

    def test_archive_tampering_fails(self):
        """Reject changed downloaded archive bytes before any output is written.
        写入任何产物前拒绝变化的下载归档字节。
        """
        # Arguments select the complete original fixture set.
        # 参数选择完整原始夹具集合。
        args = self.all_platforms()
        (self.input / "windows-x64/luaskills-demo-rust-windows-x64.tar.gz").write_bytes(b"tampered")
        with self.assertRaisesRegex(ValueError, "Archive or sidecar mismatch"):
            candidate.aggregate(args)
        self.assertFalse(self.output.exists())

    def test_native_target_mismatch_fails(self):
        """Reject a renamed library's mismatched embedded native target.
        拒绝重命名库中不匹配的嵌入式原生目标。
        """
        # Files and manifest are an internally consistent linux fixture, mislabeled as Windows.
        # 文件及清单是内部一致的 Linux 夹具，被错误标记为 Windows。
        files, manifest = self.platform_fixture("linux-x64")
        manifest["platform"] = "windows-x64"
        for name in candidate.PLATFORMS["windows-x64"][3]:
            files[f"lib/{name}"] = b"fixture"
        # Cargo host is independently changed to Windows so this test isolates the runtime build target gate.
        # Cargo 宿主独立改为 Windows，使此测试只验证运行时构建目标门禁。
        files[candidate.CARGO_VERSION_EVIDENCE_FILE] = files[candidate.CARGO_VERSION_EVIDENCE_FILE].replace(candidate.PLATFORMS["linux-x64"][0].encode(), candidate.PLATFORMS["windows-x64"][0].encode())
        manifest["cargo"] = candidate.cargo_identity(files[candidate.CARGO_VERSION_EVIDENCE_FILE])
        manifest["files"] = {name: candidate.digest(content) for name, content in files.items()}
        # Source archive remains the actual unchanged Git snapshot.
        # 源码归档保持为实际未变化 Git 快照。
        source = self.base / "source.tar.gz"
        source.write_bytes(self.source)
        with self.assertRaisesRegex(ValueError, "native target"):
            candidate.verify_payload(files, manifest, source, self.commit, self.version, "windows-x64")

    def test_report_or_descriptor_tampering_fails(self):
        """Reject a forged build identity even if outer file checksum declarations are updated.
        即使外层文件摘要声明已更新，也拒绝伪造构建身份。
        """
        # Files initially agree with the real frozen source identity.
        # 文件起初与真实冻结源码身份一致。
        files, manifest = self.platform_fixture("windows-x64")
        manifest["build"]["source_sha256"] = "0" * 64
        # Source is still the exact original archive.
        # 源码仍是精确原始归档。
        source = self.base / "source.tar.gz"
        source.write_bytes(self.source)
        with self.assertRaisesRegex(ValueError, "descriptor and build report"):
            candidate.verify_payload(files, manifest, source, self.commit, self.version, "windows-x64")

    def test_existing_aggregate_is_never_overwritten(self):
        """Reject immutable output conflicts after validating every incoming platform.
        验证每个传入平台后拒绝不可变产物冲突。
        """
        # Arguments use a complete valid fixture set.
        # 参数使用完整有效夹具集合。
        args = self.all_platforms()
        self.output.mkdir()
        with self.assertRaisesRegex(ValueError, "already exists"):
            candidate.aggregate(args)

    def test_dirty_source_is_never_assigned_commit_identity(self):
        """Require a clean checkout even when HEAD and requested version match.
        即使 HEAD 及请求版本一致，也要求干净检出。
        """
        with patch.object(candidate, "git", side_effect=[f"{self.commit}\n".encode(), b" M scripts/release/candidate.py\n"]):
            with self.assertRaisesRegex(ValueError, "Dirty candidates"):
                candidate.source_identity(self.root, self.commit, self.version)

    def test_duplicate_json_keys_fail(self):
        """Reject duplicate release fields instead of accepting the last value.
        拒绝重复发布字段，而非接受最后一个值。
        """
        # Path contains intentionally invalid duplicate-field evidence.
        # 路径包含刻意无效的重复字段证据。
        path = self.base / "duplicate.json"
        path.write_text('{"source_commit":"a","source_commit":"b"}')
        with self.assertRaisesRegex(ValueError, "Duplicate JSON key"):
            candidate.read_json(path)

    def test_build_log_uses_manifest_owner_and_rejects_stale_duplicates(self):
        """Resolve the owning Cargo manifest instead of guessing package-ID syntax or scanning build directories.
        解析所属 Cargo 清单，而非猜测包身份语法或扫描构建目录。
        """
        # Log binds an opaque Cargo package ID to the exact current root manifest.
        # 日志将不透明 Cargo 包身份绑定到精确当前根清单。
        path = self.base / "build.jsonl"
        # Messages deliberately use an opaque ID to ensure there is no package name substring assumption.
        # 消息刻意使用不透明身份，确保没有包名子字符串假设。
        messages = [{"reason": "compiler-artifact", "package_id": "opaque-root-id", "manifest_path": str(self.root / "Cargo.toml")}, {"reason": "build-script-executed", "package_id": "opaque-root-id", "out_dir": str(self.root / "target/release/build/one/out")}, {"reason": "build-finished", "success": True}]
        path.write_text("\n".join(json.dumps(message) for message in messages))
        self.assertEqual(candidate.build_report(self.root, path), self.root / "target/release/build/one/out/embedded-build-inputs.json")
        messages.insert(-1, {"reason": "build-script-executed", "package_id": "opaque-root-id", "out_dir": str(self.root / "target/release/build/stale/out")})
        path.write_text("\n".join(json.dumps(message) for message in messages))
        with self.assertRaisesRegex(ValueError, "exactly one"):
            candidate.build_report(self.root, path)

    def test_existing_remote_objects_fail_without_mutation(self):
        """Reject an existing release/tag using a read-only request; do not issue upload or replacement calls.
        使用只读请求拒绝既有发布或标签；不发出上传或替换调用。
        """
        with patch.object(create_draft, "request", return_value={"id": 123}) as request:
            with self.assertRaisesRegex(ValueError, "cannot overwrite"):
                create_draft.require_absent("https://fixture.invalid/release", "fixture-token")
            request.assert_called_once_with("https://fixture.invalid/release", "fixture-token")

    def test_package_dry_run_and_immutable_real_temporary_archive(self):
        """Execute actual package writes using a synthetic native descriptor; verify archive bytes, report and no overwrite.
        使用合成原生描述执行实际打包写入；验证归档字节、报告及禁止覆盖。
        """
        # Root contains actual frozen source bytes extracted only inside this test's temporary directory.
        # 根包含仅在此测试临时目录内解压的实际冻结源码字节。
        root = self.base / "checkout"
        root.mkdir()
        with tarfile.open(fileobj=io.BytesIO(self.source), mode="r:gz") as archive:
            archive.extractall(root, filter="data")
        # Fixture files include a deliberately synthetic native library and an actual-shaped descriptor.
        # 夹具文件包含明确的合成原生库及具有实际形状的描述。
        files, manifest = self.platform_fixture("windows-x64")
        for name in candidate.PLATFORMS["windows-x64"][3]:
            (root / "target/release").mkdir(parents=True, exist_ok=True)
            (root / "target/release" / name).write_bytes(files[f"lib/{name}"])
        # OutDir is provided by the fixture's Cargo message log, never discovered by scanning.
        # OutDir 由夹具 Cargo 消息日志提供，绝不通过扫描发现。
        out_dir = root / "target/release/build/root/out"
        out_dir.mkdir(parents=True)
        (out_dir / "embedded-build-inputs.json").write_bytes(files["embedded-build-inputs.json"])
        # Log binds the root manifest to the synthetic compiler's actual fixture output directory.
        # 日志将根清单绑定到合成编译器的实际夹具输出目录。
        log = self.base / "package-build.jsonl"
        log.write_text("\n".join(json.dumps(message) for message in [{"reason": "compiler-artifact", "package_id": "fixture-root", "manifest_path": str(root / "Cargo.toml")}, {"reason": "build-script-executed", "package_id": "fixture-root", "out_dir": str(out_dir)}, {"reason": "build-finished", "success": True}]))
        # Metadata uses actual root license paths and version; no network or Cargo invocation occurs.
        # 元数据使用实际根许可路径及版本；不发生网络或 Cargo 调用。
        metadata = self.base / "metadata.json"
        metadata.write_bytes(candidate.encode({"packages": [{"id": "fixture-root", "name": "luaskills", "version": self.version, "manifest_path": str(root / "Cargo.toml"), "source": None, "license": "MIT", "license_file": None}]}))
        # CargoVersion is the explicit release-only capture consumed by the actual package function.
        # CargoVersion 是实际打包函数消费的显式仅发布捕获。
        cargo_version = self.base / candidate.CARGO_VERSION_EVIDENCE_FILE
        cargo_version.write_bytes(files[candidate.CARGO_VERSION_EVIDENCE_FILE])
        # Arguments name one explicit source snapshot while the native reader is the only synthetic boundary.
        # 参数命名一个显式源码快照，原生读取器是唯一合成边界。
        args = argparse.Namespace(root=root, source_commit=self.commit, version=self.version, platform="windows-x64", build_log=log, metadata=metadata, cargo_version=cargo_version, output=self.base / "package", dry_run=True)
        with patch.object(candidate, "git", side_effect=lambda directory, *arguments: self.commit.encode() if arguments[0] == "rev-parse" else b"" if arguments[0] == "status" else gzip.decompress(self.source)), patch.object(candidate, "native_description", return_value=files["embedded-core-description.json"]), patch("builtins.print"):
            candidate.package(args)
            self.assertFalse(args.output.exists())
            args.dry_run = False
            candidate.package(args)
            # Packaged manifest hashes are verified by the real collector, not an implementation-string assertion.
            # 包内清单摘要由真实收集器验证，而非实现字符串断言。
            packaged = candidate.archive_files(args.output / "luaskills-ffi-sdk-windows-x64.tar.gz")
            self.assertEqual(packaged["embedded-core-description.json"], files["embedded-core-description.json"])
            self.assertEqual(packaged["embedded-build-inputs.json"], files["embedded-build-inputs.json"])
            self.write_auxiliary(args.output, "windows-x64", packaged)
            candidate.collect(args)
            # Original hashes must remain intact after a rejected packaging overwrite.
            # 打包覆盖被拒绝后，原始摘要必须保持完整。
            original = (args.output / "luaskills-ffi-sdk-windows-x64.tar.gz").read_bytes()
            with self.assertRaisesRegex(ValueError, "already exists"):
                candidate.package(args)
            self.assertEqual((args.output / "luaskills-ffi-sdk-windows-x64.tar.gz").read_bytes(), original)

    @unittest.skipUnless(shutil.which("powershell"), "PowerShell 5.1 is required for the actual Windows packager fixture")
    def test_actual_powershell_rust_demo_pins_source_revision(self):
        """Run the actual PowerShell Rust demo packager in a temporary source tree; parse its resulting archive dependency.
        在临时源码树运行实际 PowerShell Rust demo 打包器；解析所得归档依赖。
        """
        # Root is a disposable fixture checkout so existing repository staging paths are never deleted.
        # 根是可丢弃夹具检出，绝不删除既有仓库暂存路径。
        root = self.base / "demo-checkout"
        root.mkdir()
        with tarfile.open(fileobj=io.BytesIO(self.source), mode="r:gz") as archive:
            archive.extractall(root, filter="data")
        # Current script is copied explicitly because HEAD does not yet contain the pending candidate changes.
        # 显式复制当前脚本，因为 HEAD 尚不包含待提交候选变更。
        script = root / "scripts/build/package_demo.ps1"
        script.write_bytes((self.root / "scripts/build/package_demo.ps1").read_bytes())
        # Output is a new temporary archive directory with no externally visible release operation.
        # 产物是新的临时归档目录，没有对外可见发布操作。
        output = self.base / "demo-output"
        subprocess.run(["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", str(script), "-Mode", "rust", "-Platform", "windows-x64", "-OutputDir", str(output), "-ReleaseTag", f"v{self.version}", "-SourceCommit", self.commit], cwd=root, check=True, capture_output=True)
        # Files are produced by real PS/tar execution rather than synthetic package construction.
        # 文件由真实 PS 及 tar 执行生成，而非合成包构造。
        files = candidate.archive_files(output / "luaskills-demo-rust-windows-x64.tar.gz")
        self.assertEqual(candidate.tomllib.loads(files["Cargo.toml"].decode("utf-8-sig"))["dependencies"]["luaskills"], {"git": "https://github.com/LuaSkills/luaskills.git", "rev": self.commit})
        self.assertEqual(json.loads(files["demo-manifest.json"])["release_tag"], f"v{self.version}")

    def isolated_workflow_history(self):
        """Create disposable real source/equal/changed Git commits; return the advancing equal and changed default SHAs.
        创建可丢弃的真实源码、同树及异树 Git 提交；返回前进的同树及异树默认提交 SHA。

        No arguments are required; replace only this test's root, commit and source archive with the isolated source snapshot.
        无需参数；仅将当前测试的根目录、提交及源码归档替换为隔离源码快照。
        """
        # Root is owned by setUp's cleanup, with no shared Git objects, refs, index or checkout mutations.
        # 根目录由 setUp 的清理逻辑拥有，不共享 Git 对象、引用或索引，也不修改原检出。
        self.root = self.base / "workflow-history"
        self.root.mkdir()
        with tarfile.open(fileobj=io.BytesIO(self.source), mode="r:gz") as archive:
            archive.extractall(self.root, filter="data")

        def fixture_git(*arguments):
            """Run explicit Git arguments only in the fixture; return stdout, propagating failures without global configuration changes.
            仅在夹具内运行显式 Git 参数；返回标准输出并传播失败，不修改全局配置。

            Arguments are Git subcommand tokens; invocation-local identity and byte preservation make commits independent of user settings.
            参数是 Git 子命令词元；仅当前调用使用的身份及字节保留配置使提交不依赖用户设置。
            """
            return subprocess.run(["git", "-C", str(self.root), "-c", "user.name=Release history fixture", "-c", "user.email=release-fixture@example.invalid", "-c", "core.autocrlf=false", *arguments], check=True, capture_output=True).stdout

        def snapshot(message, parent=None):
            """Commit the fixture's complete staged tree; return its real immutable SHA with the optional explicit parent.
            提交夹具完整暂存树；返回带有可选显式父提交的真实不可变 SHA。

            Message describes the snapshot; parent is a fixture SHA or None for the isolated root commit.
            message 描述快照；parent 是夹具 SHA，隔离根提交时为 None。
            """
            fixture_git("add", "--force", "--all")
            # Tree is written from fixture bytes; commit-tree bypasses hooks and publication without mocking Git.
            # 树从夹具字节写入；commit-tree 绕过钩子及发布，无需模拟 Git。
            tree = fixture_git("write-tree").decode().strip()
            return fixture_git("commit-tree", tree, "-m", message, *(["-p", parent] if parent is not None else [])).decode().strip()

        fixture_git("init", "--template=", "--object-format=sha1")
        self.commit = snapshot("Frozen release source fixture")
        # Advance changes only a non-workflow file, proving a distinct commit with identical workflow objects.
        # 前进仅改变非工作流文件，证明不同提交具有相同工作流对象。
        advance = self.root / "release-history-fixture.txt"
        self.assertFalse(advance.exists(), "Release history fixture path must remain distinct from source files")
        advance.write_bytes(b"Explicit default-branch advance fixture\n")
        # EqualCommit is a real descendant of the frozen source, with a distinct root tree.
        # EqualCommit 是冻结源码的真实后代，具有不同根树。
        equal_commit = snapshot("Default advances without workflow changes", self.commit)
        # Workflow is an explicit extra fixture file, so existing production workflow bytes remain untouched.
        # Workflow 是显式额外夹具文件，因此现有生产工作流字节保持原样。
        workflow = self.root / ".github/workflows/release-history-fixture.yml"
        self.assertFalse(workflow.exists(), "Workflow fixture path must remain distinct from source workflows")
        workflow.write_bytes(b"name: Explicit changed workflow fixture\n")
        # ChangedCommit advances again while changing the actual workflow tree object.
        # ChangedCommit 再次前进，并改变实际工作流树对象。
        changed_commit = snapshot("Default advances with workflow changes", equal_commit)
        fixture_git("update-ref", "HEAD", self.commit)
        self.source = gzip.compress(candidate.git(self.root, "archive", "--format=tar", self.commit), mtime=0)
        # Validate ancestry and immutable tree identity before exercising the production evidence gate.
        # 在运行生产证据门禁前，验证祖先关系及不可变树身份。
        fixture_git("merge-base", "--is-ancestor", self.commit, equal_commit)
        fixture_git("merge-base", "--is-ancestor", equal_commit, changed_commit)
        self.assertNotEqual(self.commit, equal_commit)
        self.assertNotEqual(candidate.git(self.root, "rev-parse", f"{self.commit}^{{tree}}"), candidate.git(self.root, "rev-parse", f"{equal_commit}^{{tree}}"))
        self.assertEqual(candidate.git(self.root, "rev-parse", f"{self.commit}:.github/workflows"), candidate.git(self.root, "rev-parse", f"{equal_commit}:.github/workflows"))
        self.assertNotEqual(candidate.git(self.root, "rev-parse", f"{self.commit}:.github/workflows"), candidate.git(self.root, "rev-parse", f"{changed_commit}:.github/workflows"))
        return equal_commit, changed_commit

    def actual_default_api(self, default_commit):
        """Model authenticated GitHub responses from real local immutable Git objects; return the read-only API fixture.
        使用真实本地不可变 Git 对象模拟认证 GitHub 响应；返回只读 API 夹具。
        """
        # Repository and branch labels are fixture metadata; no remote branch name is guessed by the implementation.
        # 仓库及分支标签是夹具元数据；实现不猜测远端分支名称。
        repository = "Fixture/luaskills"
        default_branch = "integration/default"
        # RootTree is the actual tree of this actual historical commit, read through real Git.
        # RootTree 是此真实历史提交的实际树，通过真实 Git 读取。
        root_tree = candidate.git(self.root, "rev-parse", f"{default_commit}^{{tree}}").decode().strip()

        def api(url, token, method="GET", content=None, content_type="application/json"):
            """Serve exact repository/ref/commit/tree fixture responses; reject all mutation or unknown endpoint calls.
            提供精确仓库、引用、提交及树夹具响应；拒绝全部写入或未知端点调用。
            """
            if method != "GET" or token != "fixture-token":
                raise AssertionError("Source verification must use authenticated GET only")
            # Prefix is the explicit fixture repository API, unrelated to any real credentials.
            # 前缀是显式夹具仓库 API，与任何真实凭据无关。
            prefix = "https://fixture.invalid/repos/Fixture/luaskills"
            if url == prefix:
                return {"full_name": repository, "default_branch": default_branch}
            if url == prefix + "/git/ref/heads/" + urllib.parse.quote(default_branch, safe=""):
                return {"ref": f"refs/heads/{default_branch}", "object": {"type": "commit", "sha": default_commit}}
            if url == prefix + "/git/commits/" + default_commit:
                return {"sha": default_commit, "tree": {"sha": root_tree}}
            if url.startswith(prefix + "/git/trees/"):
                # Sha addresses a real Git tree; ls-tree yields its actual paths, kinds and child IDs.
                # Sha 标识真实 Git 树；ls-tree 提供其实际路径、类型及子身份。
                sha = url.rsplit("/", 1)[1]
                # Entries mirror the API's actual Git tree shape from current local object contents.
                # 条目从当前本地对象内容映射 API 实际 Git 树形状。
                entries = []
                for line in candidate.git(self.root, "ls-tree", sha).decode().splitlines():
                    # Header and path are separated by Git's explicit tab delimiter.
                    # 头部及路径由 Git 显式制表符分隔。
                    header, path = line.split("\t", 1)
                    # Mode, kind and object identity preserve real Git output without source fallback.
                    # 模式、类型及对象身份保留真实 Git 输出，不回退来源。
                    mode, kind, identity = header.split()
                    entries.append({"path": path, "mode": mode, "type": kind, "sha": identity})
                return {"sha": sha, "truncated": False, "tree": entries}
            raise AssertionError(f"Unexpected source evidence endpoint: {url}")
        return api

    def frozen_default_evidence(self):
        """Read actual matching source/default Git trees through the offline API fixture; return freeze evidence.
        通过离线 API 夹具读取实际匹配的源码及默认 Git 树；返回冻结证据。
        """
        with patch.object(create_draft, "request", side_effect=self.actual_default_api(self.commit)):
            return create_draft.draft_source_evidence(self.root, self.commit, "Fixture/luaskills", "https://fixture.invalid", "fixture-token")

    def test_draft_source_accepts_real_matching_git_trees(self):
        """Verify equal actual Git subtrees and preserve the API-declared branch, full commit and root tree evidence.
        验证相同实际 Git 子树，并保留 API 声明的分支、完整提交及根树证据。
        """
        # Evidence uses actual source HEAD objects with a fixture-declared non-main default branch.
        # 证据使用实际源码 HEAD 对象，并以夹具声明非 main 默认分支。
        evidence = self.frozen_default_evidence()
        self.assertEqual(evidence["default_branch"], "integration/default")
        self.assertEqual(evidence["default_commit"], self.commit)
        self.assertEqual(evidence["default_workflows_tree"], candidate.git(self.root, "rev-parse", f"{self.commit}:.github/workflows").decode().strip())
        self.assertEqual(evidence["default_root_tree"], candidate.git(self.root, "rev-parse", f"{self.commit}^{{tree}}").decode().strip())
        self.assertEqual(create_draft.verify_frozen_source_evidence(evidence, self.root, self.commit, "Fixture/luaskills"), evidence["source_workflows_tree"])

    def test_draft_source_rejects_real_different_git_trees(self):
        """Compare isolated real workflow Git trees and reject a source/default mismatch without any POST.
        比较隔离的真实工作流 Git 树，并拒绝源码与默认分支差异，不执行任何 POST。
        """
        # ChangedCommit comes from an isolated real descendant with a verified different workflow tree.
        # ChangedCommit 来自隔离的真实后代，具有已验证的不同工作流树。
        _, changed_commit = self.isolated_workflow_history()
        with patch.object(create_draft, "request", side_effect=self.actual_default_api(changed_commit)) as request:
            with self.assertRaisesRegex(ValueError, "merge workflow changes first"):
                create_draft.draft_source_evidence(self.root, self.commit, "Fixture/luaskills", "https://fixture.invalid", "fixture-token")
            self.assertTrue(all(len(call.args) < 3 or call.args[2] == "GET" for call in request.call_args_list))

    def test_draft_source_fails_closed_without_default_branch_evidence(self):
        """Reject missing default-branch API evidence instead of assuming main or accepting candidate publication.
        拒绝缺失默认分支 API 证据，而非假定 main 或接受候选发布。
        """
        with patch.object(create_draft, "request", return_value={"full_name": "Fixture/luaskills"}) as request:
            with self.assertRaises(KeyError):
                create_draft.draft_source_evidence(self.root, self.commit, "Fixture/luaskills", "https://fixture.invalid", "fixture-token")
            request.assert_called_once_with("https://fixture.invalid/repos/Fixture/luaskills", "fixture-token")
        # Frozen evidence also must be complete before the draft script can use it.
        # 草稿脚本使用冻结证据前，也必须确保其完整。
        evidence = self.frozen_default_evidence()
        del evidence["default_root_tree"]
        with self.assertRaises(KeyError):
            create_draft.verify_frozen_source_evidence(evidence, self.root, self.commit, "Fixture/luaskills")

    def test_second_release_page_draft_blocks_post_after_tag_and_ref_404(self):
        """Run draft CLI preflight with a page-two draft and two misleading 404s; assert no POST occurs.
        使用第二页草稿及两个易误判的 404 运行草稿 CLI 前置检查；断言不发生 POST。
        """
        candidate.aggregate(self.all_platforms())
        # Evidence was independently verified using actual local Git trees before the release API fixture is installed.
        # 发布 API 夹具安装前，证据已通过实际本地 Git 树独立验证。
        evidence = self.base / "draft-source-evidence.json"
        evidence.write_bytes(candidate.encode(self.frozen_default_evidence()))
        # Tag is the exact candidate version's draft identity.
        # 标签是精确候选版本的草稿身份。
        tag = f"v{self.version}"

        def release_api(url, token, method="GET", content=None, content_type="application/json"):
            """Return complete release pages plus explicit tag/ref 404s; fail immediately if publication is attempted.
            返回完整发布分页及明确标签和引用 404；尝试发布时立即失败。
            """
            if method != "GET" or token != "fixture-token":
                raise AssertionError("Existing page-two draft must prevent all POST requests")
            if "/releases/tags/" in url or "/git/ref/tags/" in url:
                raise urllib.error.HTTPError(url, 404, "Not Found", {}, None)
            if url.endswith("releases?per_page=100&page=1"):
                return [{"tag_name": f"unrelated-{index}", "draft": False} for index in range(100)]
            if url.endswith("releases?per_page=100&page=2"):
                return [{"tag_name": tag, "draft": True}]
            raise AssertionError(f"Unexpected release endpoint: {url}")

        with patch.dict("os.environ", {"GITHUB_REPOSITORY": "Fixture/luaskills", "GITHUB_API_URL": "https://fixture.invalid", "GITHUB_TOKEN": "fixture-token"}), patch.object(sys, "argv", ["create_draft.py", "--candidate", str(self.output), "--source-commit", self.commit, "--version", self.version, "--source-evidence", str(evidence), "--root", str(self.root)]), patch.object(create_draft, "request", side_effect=release_api) as request, patch("sys.stderr", new_callable=io.StringIO) as errors:
            with self.assertRaises(SystemExit) as result:
                create_draft.main()
            self.assertEqual(result.exception.code, 1)
            self.assertIn("Release or draft already exists", errors.getvalue())
            self.assertTrue(any(call.args[0].endswith("releases?per_page=100&page=2") for call in request.call_args_list))
            self.assertTrue(any("/releases/tags/" in call.args[0] for call in request.call_args_list))
            self.assertTrue(any("/git/ref/tags/" in call.args[0] for call in request.call_args_list))
            self.assertTrue(all(len(call.args) < 3 or call.args[2] == "GET" for call in request.call_args_list))

    def test_release_listing_fails_closed_on_unknown_shape_and_api_failure(self):
        """Reject unknown or failed release-list responses even when both tag/ref lookups return 404.
        即使标签及引用查询均返回 404，也拒绝未知或失败的发布列表响应。
        """
        for listing in ({"unknown": "shape"}, urllib.error.HTTPError("https://fixture.invalid", 503, "Unavailable", {}, None)):
            # Replies make both initial absence checks succeed, followed by the problematic listing response.
            # 回复使前两个不存在检查成功，然后提供有问题的列表响应。
            replies = [urllib.error.HTTPError("https://fixture.invalid", 404, "Not Found", {}, None), urllib.error.HTTPError("https://fixture.invalid", 404, "Not Found", {}, None), listing]
            with patch.object(create_draft, "request", side_effect=replies) as request:
                with self.assertRaises((ValueError, urllib.error.HTTPError)):
                    create_draft.require_unused_release_tag("https://fixture.invalid", "Fixture/luaskills", f"v{self.version}", "fixture-token")
                self.assertTrue(any(call.args[0].endswith("releases?per_page=100&page=1") for call in request.call_args_list))
                self.assertTrue(all(len(call.args) < 3 or call.args[2] == "GET" for call in request.call_args_list))
            for reply in replies:
                if isinstance(reply, urllib.error.HTTPError):
                    reply.close()

    def test_default_commit_advance_with_equal_workflows_records_current_evidence(self):
        """Accept real default commit advancement only after equal workflows are re-read, and preserve both observed snapshots.
        仅在重读确认工作流相同时接受真实默认提交前进，并保留两个观察快照。
        """
        # EqualCommit is a real advancing default commit with the same immutable workflow tree as the source.
        # EqualCommit 是真实前进的默认提交，其不可变工作流树与源码相同。
        equal_commit, _ = self.isolated_workflow_history()
        candidate.aggregate(self.all_platforms())
        with patch.object(create_draft, "request", side_effect=self.actual_default_api(self.commit)):
            # FrozenEvidence records the isolated source commit before the real default advancement.
            # FrozenEvidence 记录真实默认分支前进之前的隔离源码提交。
            frozen_evidence = create_draft.draft_source_evidence(self.root, self.commit, "Fixture/luaskills", "https://fixture.invalid", "fixture-token")
        # Evidence path contains the older snapshot and remains unchanged by live revalidation.
        # 证据路径包含较早快照，并在实时复核中保持不变。
        evidence = self.base / "draft-source-evidence.json"
        evidence.write_bytes(candidate.encode(frozen_evidence))
        # LiveApi serves real newer default commit objects for the final pre-POST permission gate.
        # LiveApi 为最终 POST 前权限门禁提供真实较新默认提交对象。
        live_api = self.actual_default_api(equal_commit)

        def publication_fixture(url, token, method="GET", content=None, content_type="application/json"):
            """Model a permitted draft creation and uploads entirely offline after authenticated default-tree revalidation.
            在认证默认树复核后，完全离线模拟允许的草稿创建及上传。
            """
            if method == "POST":
                if url == "https://fixture.invalid/repos/Fixture/luaskills/releases":
                    return {"upload_url": "https://fixture.invalid/uploads{?name,label}", "html_url": "https://fixture.invalid/draft"}
                if url.startswith("https://fixture.invalid/uploads?name="):
                    return {"id": "offline-upload-fixture"}
                raise AssertionError(f"Unexpected mutation endpoint: {url}")
            if "/releases/tags/" in url or "/git/ref/tags/" in url:
                raise urllib.error.HTTPError(url, 404, "Not Found", {}, None)
            if url.endswith("releases?per_page=100&page=1"):
                return []
            return live_api(url, token, method, content, content_type)

        with patch.dict("os.environ", {"GITHUB_REPOSITORY": "Fixture/luaskills", "GITHUB_API_URL": "https://fixture.invalid", "GITHUB_TOKEN": "fixture-token"}), patch.object(sys, "argv", ["create_draft.py", "--candidate", str(self.output), "--source-commit", self.commit, "--version", self.version, "--source-evidence", str(evidence), "--root", str(self.root)]), patch.object(create_draft, "request", side_effect=publication_fixture) as request, patch("builtins.print"):
            create_draft.main()
            # FirstPost occurs after both authenticated listing and the full current default-tree read chain.
            # FirstPost 出现在认证列表及完整当前默认树读取链之后。
            first_post = next(index for index, call in enumerate(request.call_args_list) if len(call.args) >= 3 and call.args[2] == "POST")
            # TreeReads are located by endpoint identity, so additional read-only gates cannot invalidate the assertion by shifting an index.
            # TreeReads 按端点身份定位，因此新增只读门禁不会因下标移动使断言失效。
            tree_reads = [index for index, call in enumerate(request.call_args_list) if "/git/trees/" in call.args[0]]
            self.assertTrue(tree_reads)
            self.assertLess(max(tree_reads), first_post)
            self.assertTrue(all(len(call.args) < 3 or call.args[2] == "GET" for call in request.call_args_list[:first_post]))
        self.assertEqual(candidate.read_json(evidence), frozen_evidence)
        # CurrentEvidence retains the actual new default SHA and equal workflow tree, independently from the frozen snapshot.
        # CurrentEvidence 保留实际新默认 SHA 及相同工作流树，与冻结快照独立。
        current_evidence = candidate.read_json(self.output / "draft-source-current-evidence.json")
        self.assertEqual(current_evidence["default_commit"], equal_commit)
        self.assertNotEqual(current_evidence["default_commit"], frozen_evidence["default_commit"])
        self.assertEqual(current_evidence["default_workflows_tree"], frozen_evidence["source_workflows_tree"])

    def test_default_workflow_change_during_builds_blocks_every_post(self):
        """Reject real changed default workflow trees after freeze even when tag/ref/listing report no conflicts.
        即使标签、引用及列表均无冲突，也在冻结后拒绝实际变化的默认工作流树。
        """
        # ChangedCommit is a real advancing default commit with different immutable workflow objects.
        # ChangedCommit 是真实前进的默认提交，具有不同不可变工作流对象。
        _, changed_commit = self.isolated_workflow_history()
        candidate.aggregate(self.all_platforms())
        # Evidence is the accepted matching tree snapshot before the modeled long-running platform build.
        # 证据是模拟长时间平台构建前已接受的匹配树快照。
        evidence = self.base / "draft-source-evidence.json"
        evidence.write_bytes(candidate.encode(self.frozen_default_evidence()))
        # LiveApi reaches actual changed tree objects through the isolated advancing commit root.
        # LiveApi 通过隔离前进提交根到达实际变化树对象。
        live_api = self.actual_default_api(changed_commit)

        def failure_fixture(url, token, method="GET", content=None, content_type="application/json"):
            """Allow only read-only absence and current-source checks; any POST is a test failure.
            仅允许只读不存在检查及当前源码检查；任何 POST 都是测试失败。
            """
            if method != "GET":
                raise AssertionError("Changed default workflow trees must stop every POST")
            if "/releases/tags/" in url or "/git/ref/tags/" in url:
                raise urllib.error.HTTPError(url, 404, "Not Found", {}, None)
            if url.endswith("releases?per_page=100&page=1"):
                return []
            return live_api(url, token, method, content, content_type)

        with patch.dict("os.environ", {"GITHUB_REPOSITORY": "Fixture/luaskills", "GITHUB_API_URL": "https://fixture.invalid", "GITHUB_TOKEN": "fixture-token"}), patch.object(sys, "argv", ["create_draft.py", "--candidate", str(self.output), "--source-commit", self.commit, "--version", self.version, "--source-evidence", str(evidence), "--root", str(self.root)]), patch.object(create_draft, "request", side_effect=failure_fixture) as request, patch("sys.stderr", new_callable=io.StringIO) as errors:
            with self.assertRaises(SystemExit) as result:
                create_draft.main()
            self.assertEqual(result.exception.code, 1)
            self.assertIn("merge workflow changes first", errors.getvalue())
            self.assertTrue(all(len(call.args) < 3 or call.args[2] == "GET" for call in request.call_args_list))
            self.assertFalse((self.output / "draft-source-current-evidence.json").exists())

    def test_published_source_proofs_use_historical_immutable_trees_and_injected_lookup(self):
        """Validate published proof with the caller's checked HTTP client and real historical Git objects, without current default queries.
        使用调用方受检 HTTP 客户端及真实历史 Git 对象验证公开证明，不查询当前默认分支。
        """
        # Evidence is the actual-shaped freeze proof derived from real Git tree responses.
        # 证据是从真实 Git 树响应派生的实际形状冻结证明。
        evidence = self.frozen_default_evidence()
        # Api is an offline fixture over real immutable commit/tree objects.
        # Api 是基于真实不可变提交及树对象的离线夹具。
        api = self.actual_default_api(self.commit)
        # URLs record every historical lookup so the test proves current branch metadata is never consulted.
        # URLs 记录每个历史查询，使测试证明绝不查阅当前分支元数据。
        urls = []

        def lookup(url):
            """Delegate one URL to the explicit read-only checked transport fixture; retain its call evidence.
            将一个 URL 委托给显式只读受检传输夹具；保留其调用证据。
            """
            urls.append(url)
            return api(url, "fixture-token")

        with patch.object(create_draft, "request", side_effect=AssertionError("Injected lookup must prevent a second HTTP client")):
            self.assertEqual(create_draft.verify_published_source_evidence(evidence, self.commit, "Fixture/luaskills", "https://fixture.invalid", lookup=lookup), evidence)
        self.assertTrue(urls)
        self.assertTrue(all("/git/commits/" in url or "/git/trees/" in url for url in urls))
        # Corrupted historical roots are rejected even though their workflow labels remain unchanged.
        # 即使工作流标签保持不变，损坏的历史根也被拒绝。
        evidence["default_root_tree"] = "0" * 40
        with self.assertRaisesRegex(ValueError, "historical Git objects"):
            create_draft.verify_published_source_evidence(evidence, self.commit, "Fixture/luaskills", "https://fixture.invalid", lookup=lookup)

    def test_draft_source_asset_name_cannot_deviate_from_shared_authority(self):
        """Reject a caller-selected source evidence basename before any GitHub request or asset upload.
        在任何 GitHub 请求或资产上传前拒绝调用方自选的源码证据文件名。
        """
        with patch.object(sys, "argv", ["create_draft.py", "--candidate", str(self.output), "--source-commit", self.commit, "--version", self.version, "--source-evidence", str(self.base / "unexpected-source-proof.json")]), patch.object(create_draft, "verified_assets", return_value=[]), patch.object(create_draft, "request", side_effect=AssertionError("Invalid asset names must prevent HTTP requests")) as request, patch("sys.stderr", new_callable=io.StringIO) as errors:
            with self.assertRaises(SystemExit) as result:
                create_draft.main()
            self.assertEqual(result.exception.code, 1)
            self.assertIn("asset name differs", errors.getvalue())
            request.assert_not_called()

    def test_actual_cargo_identity_is_required_and_preserves_exact_verbose_bytes(self):
        """Require release/hash/host from actual Cargo-shaped bytes; reject rustc substitution and preserve raw newline identity.
        要求实际 Cargo 形状字节中的版本、提交摘要及宿主；拒绝以 rustc 替代并保留原始换行身份。
        """
        # Files provide the explicitly synthetic Cargo capture used by the package fixture.
        # 文件提供打包夹具使用的明确合成 Cargo 捕获。
        files, _ = self.platform_fixture("windows-x64")
        # Content includes CRLF to prove byte hashing does not silently normalize the capture.
        # Content 包含 CRLF，以证明字节摘要不会静默归一化捕获。
        content = files[candidate.CARGO_VERSION_EVIDENCE_FILE].replace(b"\n", b"\r\n")
        # Identity is independently checked for its mandatory fields and exact original bytes.
        # Identity 的必需字段及精确原始字节独立检查。
        identity = candidate.cargo_identity(content)
        self.assertEqual(identity["release"], "1.94.0")
        self.assertEqual(identity["commit_hash"], "1" * 40)
        self.assertEqual(identity["host"], candidate.PLATFORMS["windows-x64"][0])
        self.assertEqual(identity["version_verbose_sha256"], candidate.digest(content))
        self.assertEqual(identity["version_verbose"].encode(), content)
        with self.assertRaisesRegex(ValueError, "missing or invalid"):
            candidate.cargo_identity(b"rustc 1.94.0\nrelease: 1.94.0\ncommit-hash: " + b"1" * 40 + b"\nhost: x86_64-pc-windows-msvc\n")
        with self.assertRaisesRegex(ValueError, "missing or invalid"):
            candidate.cargo_identity(b"cargo 1.94.0 (fixture)\nrelease: 1.94.0\nhost: x86_64-pc-windows-msvc\n")

    def test_cargo_hosts_may_differ_but_release_and_commit_hash_must_match(self):
        """Reject inconsistent Cargo release or source hash across otherwise valid five-platform candidates.
        拒绝其余有效五平台候选之间不一致的 Cargo 版本或源码摘要。
        """
        # Arguments select one complete fixture set whose native Cargo hosts already differ legitimately.
        # 参数选择一个完整夹具集合，其中原生 Cargo 宿主已经合法不同。
        args = self.all_platforms()
        # Archive and record paths belong only to the Windows temporary candidate fixture.
        # 归档及记录路径仅属于 Windows 临时候选夹具。
        archive = self.input / "windows-x64/luaskills-ffi-sdk-windows-x64.tar.gz"
        record_path = self.input / "windows-x64/candidate-windows-x64.json"
        # Original files are reused to isolate each release-only Cargo mismatch without changing runtime build identity.
        # 复用原始文件以隔离每个仅发布 Cargo 差异，不改变运行时构建身份。
        original = candidate.archive_files(archive)
        for field, content in (("release", original[candidate.CARGO_VERSION_EVIDENCE_FILE].replace(b"1.94.0", b"1.95.0")), ("commit_hash", original[candidate.CARGO_VERSION_EVIDENCE_FILE].replace(b"1" * 40, b"2" * 40))):
            # Files retain every original byte except the deliberately mismatched Cargo capture.
            # Files 保留每个原始字节，仅刻意不匹配的 Cargo 捕获除外。
            files = dict(original)
            # Manifest and record are updated consistently so the aggregate must detect cross-host toolchain mismatch itself.
            # 清单及记录一致更新，使汇总必须自行发现跨宿主工具链差异。
            manifest = candidate.decode_json(files.pop("ffi-sdk-manifest.json"))
            files[candidate.CARGO_VERSION_EVIDENCE_FILE] = content
            manifest["cargo"] = candidate.cargo_identity(content)
            manifest["files"] = {name: candidate.digest(value) for name, value in files.items()}
            files["ffi-sdk-manifest.json"] = candidate.encode(manifest)
            archive.write_bytes(candidate.archive_bytes(files))
            # Record binds the changed archive bytes to its changed Cargo identity.
            # 记录将变化归档字节绑定到其变化 Cargo 身份。
            record = candidate.read_json(record_path)
            record["cargo"] = manifest["cargo"]
            record["archives"][archive.name] = candidate.digest(archive.read_bytes())
            record_path.write_bytes(candidate.encode(record))
            (archive.parent / f"{archive.name}.sha256").write_text(f"{record['archives'][archive.name]}  {archive.name}\n")
            with self.assertRaisesRegex(ValueError, f"Cross-platform actual Cargo identity mismatch: {field}"):
                candidate.aggregate(args)
            self.assertFalse(self.output.exists())


if __name__ == "__main__":
    unittest.main()
