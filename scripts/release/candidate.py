#!/usr/bin/env python3
"""Build and verify local candidate evidence; never publish or overwrite assets.
构建并验证本地候选证据；绝不发布或覆盖资产。
"""

import argparse
import ctypes
import gzip
import hashlib
import io
import json
from pathlib import Path
import re
import subprocess
import tarfile
import tomllib
import zlib

# These release-only keys are declared here; runtime build fields remain owned by build.rs.
# 发布专用键仅在此声明；运行时构建字段仍由 build.rs 拥有。
MANIFEST_VERSION = 1
# Every supported platform is mandatory, with its native Cargo target and exact library names.
# 所有受支持平台均为必需项，包含原生 Cargo 目标及精确库名称。
PLATFORMS = {
    "linux-x64": ("x86_64-unknown-linux-gnu", "linux", "x86_64", ("libluaskills.so", "libluaskills.a")),
    "linux-arm64": ("aarch64-unknown-linux-gnu", "linux", "aarch64", ("libluaskills.so", "libluaskills.a")),
    "macos-x64": ("x86_64-apple-darwin", "macos", "x86_64", ("libluaskills.dylib", "libluaskills.a")),
    "macos-arm64": ("aarch64-apple-darwin", "macos", "aarch64", ("libluaskills.dylib", "libluaskills.a")),
    "windows-x64": ("x86_64-pc-windows-msvc", "windows", "x86_64", ("luaskills.dll", "luaskills.dll.lib", "luaskills.lib")),
}
# The existing four archive families remain the exact mandatory set for each platform.
# 既有四种归档仍是各平台精确的必需集合。
ARCHIVE_FAMILIES = ("ffi-sdk", "demo-ffi", "demo-rust", "debug-tool")
# Actual Cargo verbose bytes belong to release evidence, independently of the Rust runtime build ABI.
# 实际 Cargo 详细版本字节属于发布证据，独立于 Rust 运行时构建 ABI。
CARGO_VERSION_EVIDENCE_FILE = "cargo-version.txt"


def digest(content):
    """Return SHA-256 for exact content bytes, without newline normalization.
    返回精确内容字节的 SHA-256，不归一化换行。
    """
    return hashlib.sha256(content).hexdigest()


def cargo_identity(content):
    """Parse actual UTF-8 cargo --version --verbose bytes; return release/hash/host and complete hashed output.
    解析实际 UTF-8 cargo --version --verbose 字节；返回版本、提交摘要、宿主及完整已计算摘要的输出。
    """
    # Text preserves original newline bytes through the original content digest, with no inferred version or compiler substitution.
    # Text 通过原内容摘要保留原始换行字节，不推断版本或以编译器替代。
    text = content.decode("utf-8")
    # Lines are the confirmed Cargo verbose header and key/value rows; empty or duplicate required fields are errors.
    # Lines 是已确认 Cargo 详细输出头及键值行；空白或重复必需字段均为错误。
    lines = text.splitlines()
    if not lines:
        raise ValueError("Actual Cargo verbose identity is empty")
    # Values preserve Cargo's actual release/commit-hash/host keys; unrelated verbose rows remain in the original text.
    # Values 保留 Cargo 实际 release、commit-hash 及 host 键；无关详细行仍保留于原文。
    values = {}
    for line in lines[1:]:
        # Pair is parsed only from Cargo's confirmed colon-space key delimiter.
        # Pair 仅从 Cargo 已确认的冒号及空格键分隔符解析。
        pair = line.split(": ", 1)
        if len(pair) == 2 and pair[0] in ("release", "commit-hash", "host"):
            if pair[0] in values or not pair[1]:
                raise ValueError("Actual Cargo verbose identity has duplicate or empty fields")
            values[pair[0]] = pair[1]
    if set(values) != {"release", "commit-hash", "host"} or not lines[0].startswith(f"cargo {values['release']} (") or not re.fullmatch(r"[0-9a-f]{40}", values["commit-hash"]):
        raise ValueError("Actual Cargo release, commit hash or host evidence is missing or invalid")
    return {"release": values["release"], "commit_hash": values["commit-hash"], "host": values["host"], "version_verbose_sha256": digest(content), "version_verbose": text}


def decode_json(content):
    """Decode exact JSON bytes; reject duplicate keys instead of losing evidence.
    解码精确 JSON 字节；拒绝重复键，避免丢失证据。
    """
    def unique_pairs(pairs):
        """Return an object from unique pairs; fail for any duplicate key.
        从唯一键值对返回对象；遇任何重复键时失败。
        """
        # Object retains the original field ownership with no alias probing.
        # 对象保留原始字段归属，不探测别名。
        result = {}
        for key, value in pairs:
            if key in result:
                raise ValueError(f"Duplicate JSON key: {key}")
            result[key] = value
        return result
    return json.loads(content, object_pairs_hook=unique_pairs)


def read_json(path):
    """Read exact JSON bytes from path through the shared strict decoder; return their object.
    从路径读取精确 JSON 字节并使用共享严格解码器；返回其对象。
    """
    return decode_json(path.read_bytes())


def encode(value):
    """Return deterministic UTF-8 JSON bytes for a release-owned value.
    为发布流程拥有的值返回确定性 UTF-8 JSON 字节。
    """
    return (json.dumps(value, ensure_ascii=False, sort_keys=True, indent=2) + "\n").encode()


def git(root, *arguments):
    """Run read-only Git arguments in root; return stdout or propagate failure.
    在根目录执行只读 Git 参数；返回输出或传播失败。
    """
    return subprocess.run(["git", "-C", str(root), *arguments], check=True, capture_output=True).stdout


def source_identity(root, commit, version, source_archive: Path | None = None):
    """Validate root's clean full commit and package version; return exact frozen gzip bytes.
    验证根目录的干净完整提交及包版本；返回精确冻结 gzip 字节。

    Root is the checkout, commit is its full SHA, and version is its Cargo package version.
    root 是检出目录，commit 是其完整 SHA，version 是其 Cargo 包版本。
    Source_archive is an optional frozen gzip Path whose complete tar bytes must match Git; None compresses locally.
    source_archive 是可选冻结 gzip 路径，其完整 tar 字节必须匹配 Git；None 表示在本地压缩。
    Missing, corrupt or mismatched supplied archives fail without generating replacement bytes.
    明确提供的归档缺失、损坏或不匹配时失败，不生成替代字节。
    """
    if not re.fullmatch(r"[0-9a-f]{40}", commit):
        raise ValueError("Source commit must be a full lowercase Git SHA")
    if git(root, "rev-parse", "HEAD").decode().strip() != commit:
        raise ValueError("Checkout HEAD does not match the frozen source commit")
    if git(root, "status", "--porcelain", "--untracked-files=all"):
        raise ValueError("Dirty candidates cannot claim a source commit; commit changes before packaging")
    # Cargo TOML is UTF-8 regardless of the Windows process locale.
    # Cargo 配置始终使用 UTF-8，不依赖 Windows 进程区域编码。
    if tomllib.loads((root / "Cargo.toml").read_text(encoding="utf-8"))["package"]["version"] != version:
        raise ValueError("Candidate version does not match Cargo.toml package.version")
    # Git tar is authoritative for all tracked bytes and member metadata, independently of gzip implementation differences.
    # Git tar 是全部受跟踪字节及成员元数据的权威，不受 gzip 实现差异影响。
    source_tar = git(root, "archive", "--format=tar", commit)
    if source_archive is None:
        return gzip.compress(source_tar, mtime=0)
    # Frozen bytes must survive platform packaging unchanged so aggregate can enforce the exact compressed SHA.
    # 冻结字节必须在平台打包过程中保持不变，使汇总可以强制验证精确压缩 SHA。
    frozen_source = source_archive.read_bytes()
    try:
        # Decoded tar must match the entire Git archive, not only extracted file contents.
        # 解码 tar 必须匹配整份 Git 归档，而不只是解压后的文件内容。
        frozen_tar = gzip.decompress(frozen_source)
    except (OSError, EOFError, zlib.error) as error:
        raise ValueError(f"Frozen source archive is not a valid gzip: {source_archive}") from error
    if frozen_tar != source_tar:
        raise ValueError(f"Frozen source archive does not match the exact Git tar for {commit}: {source_archive}")
    return frozen_source


def source_files(root):
    """Read identity.rs INPUT_ROOTS and hash exact files; return the Rust-compatible map.
    读取 identity.rs 的 INPUT_ROOTS 并计算精确文件摘要；返回兼容 Rust 的映射。
    """
    # The Rust declaration is authoritative; no second independent root list is maintained.
    # Rust 声明是权威；不维护第二份独立根列表。
    declaration = re.search(r"pub const INPUT_ROOTS: &\[&str\] = &\[(.*?)\];", (root / "build_support/identity.rs").read_text(encoding="utf-8"), re.S)
    if declaration is None:
        raise ValueError("Cannot read authoritative INPUT_ROOTS declaration")
    # Roots come only from the confirmed Rust string-literal declaration.
    # 根目录仅来自已确认的 Rust 字符串字面量声明。
    roots = re.findall(r'"([^"]+)"', declaration.group(1))
    if not roots:
        raise ValueError("Authoritative INPUT_ROOTS is empty")
    # Files use package-relative POSIX names, matching identity.rs on Windows and Unix.
    # 文件采用包相对 POSIX 名称，与 Windows 和 Unix 的 identity.rs 一致。
    files = {}
    for name in roots:
        # Pending traversal rejects symlinks before following any directory.
        # 待遍历列表在进入任何目录前拒绝符号链接。
        pending = [root / name]
        while pending:
            # Current input must be a regular file or directory inside this package.
            # 当前输入必须是此包内的普通文件或目录。
            path = pending.pop()
            if path.is_symlink() or not path.resolve().is_relative_to(root.resolve()):
                raise ValueError(f"Invalid source input: {path}")
            if path.is_dir():
                pending.extend(path.iterdir())
            elif path.is_file():
                files[path.relative_to(root).as_posix()] = digest(path.read_bytes())
            else:
                raise ValueError(f"Missing or nonregular source input: {path}")
    return dict(sorted(files.items()))


def source_digest(files, format_version):
    """Hash sorted files using identity.rs length-delimited domain; return its exact digest.
    使用 identity.rs 带长度边界的域计算排序文件摘要；返回精确摘要。
    """
    # State follows the existing Rust algorithm, including UTF-8 byte lengths.
    # 状态遵循既有 Rust 算法，包含 UTF-8 字节长度。
    state = hashlib.sha256(f"luaskills-build-inputs-v{format_version}\0".encode())
    for name, checksum in sorted(files.items()):
        for value in (name, checksum):
            # Encoded bytes preserve the Rust string representation exactly.
            # 编码字节精确保留 Rust 字符串表示。
            encoded = value.encode()
            state.update(len(encoded).to_bytes(8, "big"))
            state.update(encoded)
    return state.hexdigest()


def build_report(root, log):
    """Resolve the unique luaskills build-script OUT_DIR from this Cargo JSON log; return its report path.
    从本次 Cargo JSON 日志解析唯一 luaskills 构建脚本 OUT_DIR；返回报告路径。
    """
    # Messages are the actual --message-format=json output, never a stale directory scan.
    # 消息是实际 --message-format=json 输出，绝不是旧目录扫描。
    messages = [decode_json(line) for line in log.read_text(encoding="utf-8-sig").splitlines() if line.strip()]
    if not messages or messages[-1] != {"reason": "build-finished", "success": True}:
        raise ValueError("Cargo log must end with a successful build-finished message")
    # Compiler artifacts explicitly identify the owning manifest; Cargo package ID syntax is not inferred.
    # 编译产物显式标识所属清单；不推断 Cargo 包身份语法。
    package_ids = {message["package_id"] for message in messages if message["reason"] == "compiler-artifact" and Path(message["manifest_path"]).resolve() == (root / "Cargo.toml").resolve()}
    # Outputs must refer to exactly one build script for this root crate.
    # 输出必须恰好指向此根 crate 的一个构建脚本。
    outputs = {Path(message["out_dir"]) for message in messages if message["reason"] == "build-script-executed" and message["package_id"] in package_ids}
    if len(outputs) != 1:
        raise ValueError("Cargo log must identify exactly one luaskills build-script output")
    # Report is the actual artifact emitted by build.rs, with original bytes intact.
    # 报告是 build.rs 实际输出的产物，原始字节保持完整。
    report = outputs.pop() / "embedded-build-inputs.json"
    if not report.resolve().is_relative_to((root / "target").resolve()):
        raise ValueError("Build report must reside inside this checkout's target directory")
    return report


class BorrowedBuffer(ctypes.Structure):
    """Mirror FfiBorrowedBuffer for the read-only descriptor; owns no allocation.
    映射只读描述的 FfiBorrowedBuffer；不拥有分配。
    """
    # ABI fields match include/luaskills_ffi.h exactly.
    # ABI 字段精确匹配 include/luaskills_ffi.h。
    _fields_ = [("ptr", ctypes.c_void_p), ("len", ctypes.c_size_t)]


def native_description(library, contract):
    """Load the native library and copy its bounded borrowed descriptor; return JSON bytes.
    加载原生库并复制其有界借用描述；返回 JSON 字节。
    """
    # Handle stays alive until immutable borrowed bytes have been copied.
    # 句柄保持存活，直到不可变借用字节完成复制。
    handle = ctypes.CDLL(str(library.resolve()))
    # Export is the confirmed cdecl descriptor entry point, without creating runtime state.
    # 导出是已确认的 cdecl 描述入口，不创建运行时状态。
    describe = handle.luaskills_ffi_embedded_describe_v1
    describe.argtypes = [ctypes.POINTER(BorrowedBuffer)]
    describe.restype = ctypes.c_int32
    # Buffer owns no native allocation and must never enter a free function.
    # 缓冲不拥有原生分配，绝不可进入释放函数。
    buffer = BorrowedBuffer()
    if describe(ctypes.byref(buffer)) != 0 or not buffer.ptr or not 0 < buffer.len <= contract["compatibility"]["max_description_bytes"]:
        raise ValueError("Native embedded descriptor returned an invalid status or buffer")
    return ctypes.string_at(buffer.ptr, buffer.len)


def verify_build(root, platform, version, report_path, description):
    """Match actual native description to source, contract and exact report bytes; return validated report.
    将实际原生描述与源码、契约及精确报告字节匹配；返回已验证报告。
    """
    # Report schema fields come directly from current build.rs and EmbeddedBuildIdentity.
    # 报告结构字段直接来自当前 build.rs 及 EmbeddedBuildIdentity。
    report = read_json(report_path)
    # Files establish complete source coverage independently from the supplied report.
    # 文件映射独立于提供的报告建立完整源码覆盖。
    files = source_files(root)
    # Contract is the exact offline copy packaged with this binary.
    # 契约是随此二进制打包的精确离线副本。
    contract = read_json(root / "contracts/embedded/v1/contract.json")
    # Build is owned by EmbeddedCoreDescription, not a guessed enclosing envelope.
    # 构建身份属于 EmbeddedCoreDescription，不属于猜测的外层信封。
    build = description["build"]
    # Expected fields preserve every actual build.rs value and separately authenticate its report bytes.
    # 预期字段保留每个实际 build.rs 值，并独立认证报告字节。
    expected = {**report["fields"], "cargo_features": report["cargo_features"], "inputs_sha256": digest(report_path.read_bytes())}
    if build != expected or report["source_files"] != files or build["source_sha256"] != source_digest(files, report["format_version"]):
        raise ValueError("Native build identity, report or complete source file hashes disagree")
    if build["contract_sha256"] != digest((root / "contracts/embedded/v1/contract.json").read_bytes()) or build["package_lock_sha256"] != digest((root / "Cargo.lock").read_bytes()):
        raise ValueError("Contract or lockfile identity mismatch")
    if (root / "contracts/embedded/v1/contract.sha256").read_text().split()[0] != build["contract_sha256"]:
        raise ValueError("Offline contract sidecar mismatch")
    if description["core_version"] != version or contract["core_version"] != version or description["description_version"] != contract["compatibility"]["description_version"] or description["protocol_version"] != contract["protocol_version"] or description["abi_structure_version"] != contract["protocol_version"]:
        raise ValueError("Native core version or description version mismatch")
    if description["commands"] != contract["commands"] or description["runtime_commands"] != contract["runtime_commands"]:
        raise ValueError("Native command set differs from the offline contract")
    if not set(contract["compatibility"]["required_capabilities"]).issubset(description["capabilities"]):
        raise ValueError("Native core lacks required offline contract capabilities")
    if description["execution_backends"] != ["in_process"]:
        raise ValueError("Candidate core advertises an unsupported execution backend")
    # Target identity is checked against the mandatory native platform declaration.
    # 目标身份对照必需原生平台声明检查。
    triple, operating_system, architecture, _ = PLATFORMS[platform]
    if (build["target"], build["target_os"], build["target_arch"], build["pointer_width"]) != (triple, operating_system, architecture, "64"):
        raise ValueError("Native build target does not match the candidate platform")
    if build["cargo_features"] or build["opt_level"] != "3":
        raise ValueError("Candidate core must use default features and release optimization")
    return report


def archive_bytes(files):
    """Archive exact relative file bytes deterministically; return gzip-compressed tar bytes.
    确定性归档精确相对文件字节；返回 gzip 压缩 tar 字节。
    """
    # Buffer contains only explicitly supplied regular files, without symlink extraction.
    # 缓冲仅包含显式提供的普通文件，无符号链接解压。
    buffer = io.BytesIO()
    with tarfile.open(fileobj=buffer, mode="w") as archive:
        for name, content in sorted(files.items()):
            # Member metadata is stable and carries no local source paths.
            # 成员元数据稳定，不携带本地源码路径。
            member = tarfile.TarInfo(name)
            member.size = len(content)
            member.mode = 0o644
            archive.addfile(member, io.BytesIO(content))
    return gzip.compress(buffer.getvalue(), mtime=0)


def write_new(path, content):
    """Write new bytes exclusively to path; reject all existing assets, even identical ones.
    以独占方式将新字节写入路径；拒绝所有已有资产，即使字节相同。
    """
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("xb") as handle:
        handle.write(content)


def package(args):
    """Validate frozen build inputs and package the FFI candidate; dry-run writes no files.
    验证冻结构建输入并打包 FFI 候选；预演不写任何文件。
    """
    # Root is explicit and canonical, so every package input has one confirmed owner.
    # 根目录显式且规范化，使每个包输入只有一个已确认归属。
    root = args.root.resolve()
    # Source archive binds commit identity to exact tracked source bytes.
    # 源码归档将提交身份绑定到精确受跟踪源码字节。
    source = source_identity(root, args.source_commit, args.version, args.source_archive)
    # Names are the platform's complete library set; no arbitrary release-directory glob is used.
    # 名称是平台完整库集合；不使用任意发布目录通配。
    names = PLATFORMS[args.platform][3]
    # Contract bounds native metadata copying before parsing.
    # 契约在解析前限定原生元数据复制范围。
    contract = read_json(root / "contracts/embedded/v1/contract.json")
    # Description bytes come from the just-built native library, never from a caller-supplied JSON fixture.
    # 描述字节来自刚构建的原生库，绝不来自调用方提供的 JSON 夹具。
    description_bytes = native_description(root / "target/release" / names[0], contract)
    # Description preserves the actual embedded build identity.
    # 描述保留实际嵌入式构建身份。
    description = decode_json(description_bytes)
    # Report is selected by this build's JSON messages and validated against the native identity.
    # 报告由本次构建 JSON 消息选择，并对照原生身份验证。
    report = build_report(root, args.build_log)
    verify_build(root, args.platform, args.version, report, description)
    # Files form the offline SDK consumption boundary.
    # 文件集合构成离线 SDK 消费边界。
    files = {f"include/{path.name}": path.read_bytes() for path in (root / "include").glob("*.h")}
    for name in names:
        files[f"lib/{name}"] = (root / "target/release" / name).read_bytes()
    for name in ("LICENSE", "THIRD_PARTY_NOTICES.md", "Cargo.lock"):
        files[f"licenses/{name}"] = (root / name).read_bytes()
    for path in (root / "contracts/embedded/v1").iterdir():
        if path.is_file():
            files[f"contracts/embedded/v1/{path.name}"] = path.read_bytes()
    files["embedded-build-inputs.json"] = report.read_bytes()
    files["embedded-core-description.json"] = description_bytes
    # CargoEvidence is captured separately from the exact build invocation, never inferred from rustc or a toolchain label.
    # CargoEvidence 与精确构建调用分别捕获，绝不从 rustc 或工具链标签推断。
    cargo_evidence = args.cargo_version.read_bytes()
    # Cargo is release-only metadata for later actual Cargo package normalization.
    # Cargo 是供后续实际 Cargo 包规范化使用的仅发布元数据。
    cargo = cargo_identity(cargo_evidence)
    if cargo["host"] != PLATFORMS[args.platform][0]:
        raise ValueError("Actual Cargo host does not match the native candidate platform")
    files[CARGO_VERSION_EVIDENCE_FILE] = cargo_evidence
    # Metadata is captured by cargo metadata --locked after the successful locked build.
    # 元数据在锁定构建成功后由 cargo metadata --locked 捕获。
    metadata = read_json(args.metadata)
    if len([item for item in metadata["packages"] if Path(item["manifest_path"]).resolve() == root / "Cargo.toml" and item["name"] == "luaskills" and item["version"] == args.version]) != 1:
        raise ValueError("Locked dependency metadata does not identify this exact root package")
    # Dependencies record actual source license declarations without inventing unknown license grants.
    # 依赖记录实际源码许可声明，不编造未知许可授权。
    dependencies = []
    for dependency in sorted(metadata["packages"], key=lambda item: item["id"]):
        # Directory is Cargo's actual downloaded source location, not a guessed cache path.
        # 目录是 Cargo 实际下载源码位置，不是猜测的缓存路径。
        directory = Path(dependency["manifest_path"]).parent
        # License files are actual source files, including a declared license_file when present.
        # 许可文件是实际源码文件，并在存在时包含声明的 license_file。
        license_files = {path for path in directory.iterdir() if path.is_file() and re.match(r"^(LICENSE|LICENCE|COPYING|COPYRIGHT)([.\-_]|$)", path.name, re.I)}
        if dependency["license_file"] is not None:
            license_files.add(directory / dependency["license_file"])
        # Copied names are package-specific and retain original license file contents.
        # 复制名称按包区分，并保留原始许可文件内容。
        copied = []
        for index, path in enumerate(sorted(license_files)):
            # Destination avoids collisions for alternate declared relative license paths.
            # 目标避免不同声明的相对许可路径发生冲突。
            destination = f"licenses/dependencies/{dependency['name']}-{dependency['version']}/{index}-{path.name}"
            files[destination] = path.read_bytes()
            copied.append(destination)
        dependencies.append({"name": dependency["name"], "version": dependency["version"], "source": dependency["source"], "license_expression": dependency["license"], "license_files": copied})
    files["licenses/dependencies.json"] = encode({"packages": dependencies})
    # Manifest binds the compiled descriptor and each packaged file to the immutable source archive.
    # 清单将编译描述及每个包内文件绑定到不可变源码归档。
    manifest = {"schema_version": MANIFEST_VERSION, "package_name": f"luaskills-ffi-sdk-{args.platform}", "platform": args.platform, "core_version": args.version, "source_commit": args.source_commit, "source_archive_sha256": digest(source), "headers": sorted(name for name in files if name.startswith("include/")), "library_dir": "lib", "build": description["build"], "cargo": cargo, "files": {name: digest(content) for name, content in sorted(files.items())}}
    files["ffi-sdk-manifest.json"] = encode(manifest)
    # Paths are version/SHA scoped so SDK staging cannot confuse two candidates of the same version.
    # 路径按版本及 SHA 划定范围，使 SDK 暂存不能混淆同版本的两个候选。
    source_name = f"luaskills-source-{args.version}-{args.source_commit}.tar.gz"
    # Archive name preserves the existing SDK installer contract.
    # 归档名称保留既有 SDK 安装器契约。
    archive_name = f"luaskills-ffi-sdk-{args.platform}.tar.gz"
    if args.dry_run:
        print(encode({"archive": archive_name, "source_archive": source_name, "manifest": manifest}).decode())
        return
    for name in (archive_name, source_name):
        if (args.output / name).exists():
            raise ValueError(f"Candidate asset already exists: {name}")
    write_new(args.output / archive_name, archive_bytes(files))
    write_new(args.output / source_name, source)
    print(f"Created candidate: {args.output / archive_name}")


def archive_files(path):
    """Read only safe regular files from a candidate archive; return exact name-to-bytes mapping.
    仅从候选归档读取安全普通文件；返回精确名称到字节映射。
    """
    # Result rejects duplicate names so a later extraction cannot change the validated identity.
    # 结果拒绝重复名称，确保后续解压不能改变已验证身份。
    result = {}
    with tarfile.open(path, "r:gz") as archive:
        for member in archive:
            if member.name in result or member.name.startswith("/") or ".." in Path(member.name).parts or "\\" in member.name:
                raise ValueError(f"Unsafe candidate archive member: {member.name}")
            if member.isdir():
                continue
            if not member.isfile():
                raise ValueError(f"Nonregular candidate archive member: {member.name}")
            result[member.name] = archive.extractfile(member).read()
    return result


def verify_payload(files, manifest, source_path, commit, version, platform):
    """Revalidate packaged descriptor/report against the frozen source tar; return no inferred evidence.
    对照冻结源码 tar 重新验证包内描述及报告；不返回推断证据。
    """
    if manifest["schema_version"] != MANIFEST_VERSION or manifest["source_commit"] != commit or manifest["core_version"] != version or manifest["platform"] != platform:
        raise ValueError("FFI candidate manifest identity mismatch")
    if manifest["files"] != {name: digest(content) for name, content in sorted(files.items())}:
        raise ValueError("FFI candidate packaged file identity mismatch")
    if manifest["source_archive_sha256"] != digest(source_path.read_bytes()):
        raise ValueError("Frozen source archive identity mismatch")
    # ActualCargo authenticates the release-only version output against both its archived bytes and platform host.
    # ActualCargo 对照归档字节及平台宿主认证仅发布的版本输出。
    actual_cargo = cargo_identity(files[CARGO_VERSION_EVIDENCE_FILE])
    if manifest["cargo"] != actual_cargo or actual_cargo["host"] != PLATFORMS[platform][0]:
        raise ValueError("Packaged actual Cargo identity differs from candidate metadata or native host")
    # Git's archive comment authenticates which committed source snapshot was requested.
    # Git 归档注释确认请求了哪个已提交源码快照。
    with tarfile.open(source_path, "r:gz") as source_tar:
        if source_tar.pax_headers.get("comment") != commit:
            raise ValueError("Frozen source archive Git commit comment mismatch")
    # Source files are checked without extracting executable paths to the filesystem.
    # 源码文件通过读取检查，不将可执行路径解压到文件系统。
    source = archive_files(source_path)
    # Report and descriptor retain their native ownership and exact byte hashes.
    # 报告及描述保留原生归属及精确字节摘要。
    report = decode_json(files["embedded-build-inputs.json"])
    # Description is the bytes copied from the read-only native entry point during packaging.
    # 描述是打包时从只读原生入口复制的字节。
    description = decode_json(files["embedded-core-description.json"])
    # Build mirrors all authoritative build.rs fields and report bytes.
    # 构建身份映射全部权威 build.rs 字段及报告字节。
    build = {**report["fields"], "cargo_features": report["cargo_features"], "inputs_sha256": digest(files["embedded-build-inputs.json"])}
    if build != manifest["build"] or build != description["build"]:
        raise ValueError("Packaged native descriptor and build report disagree")
    # Declaration supplies the only source root and format version authority.
    # 声明提供唯一源码根及格式版本权威。
    declaration = source["build_support/identity.rs"].decode()
    # Roots are read from the actual frozen Rust declaration.
    # 根来自实际冻结 Rust 声明。
    match = re.search(r"pub const INPUT_ROOTS: &\[&str\] = &\[(.*?)\];", declaration, re.S)
    if match is None:
        raise ValueError("Frozen source INPUT_ROOTS declaration missing")
    # Inputs cover every frozen source file below the authoritative roots.
    # 输入覆盖权威根下每个冻结源码文件。
    roots = re.findall(r'"([^"]+)"', match.group(1))
    # Format is read independently instead of trusting the candidate report's self-declared domain.
    # 格式独立读取，不信任候选报告自行声明的域。
    format_match = re.search(r"pub const INPUTS_FORMAT_VERSION: u32 = (\d+);", declaration)
    if format_match is None or report["format_version"] != int(format_match.group(1)):
        raise ValueError("Packaged build input format mismatch")
    # Expected map preserves source bytes, including platform-independent checkout newline identity.
    # 预期映射保留源码字节，包括跨平台检出换行身份。
    expected = {name: digest(content) for name, content in source.items() if any(name == root or name.startswith(root + "/") for root in roots)}
    if report["source_files"] != expected or build["source_sha256"] != source_digest(expected, report["format_version"]):
        raise ValueError("Packaged source files differ from the frozen committed archive")
    if tomllib.loads(source["Cargo.toml"].decode())["package"]["version"] != version or description["core_version"] != version:
        raise ValueError("Packaged native version differs from frozen Cargo package version")
    if files["contracts/embedded/v1/contract.json"] != source["contracts/embedded/v1/contract.json"] or build["contract_sha256"] != digest(source["contracts/embedded/v1/contract.json"]) or files["licenses/Cargo.lock"] != source["Cargo.lock"] or build["package_lock_sha256"] != digest(source["Cargo.lock"]):
        raise ValueError("Packaged contract or lockfile differs from frozen source")
    # All expected platform libraries and current headers must be present.
    # 全部预期平台库及当前头文件必须存在。
    for name in PLATFORMS[platform][3]:
        if not files[f"lib/{name}"]:
            raise ValueError(f"Empty packaged library: {name}")
    for name, content in source.items():
        if name.startswith("include/") and name.endswith(".h") and files[name] != content:
            raise ValueError(f"Packaged header differs from frozen source: {name}")
    # Target fields are checked independently of platform labels.
    # 目标字段独立于平台标签检查。
    triple, operating_system, architecture, _ = PLATFORMS[platform]
    if (build["target"], build["target_os"], build["target_arch"], build["pointer_width"]) != (triple, operating_system, architecture, "64"):
        raise ValueError("Packaged platform and native target identity disagree")


def verify_auxiliary(directory, platform, commit, version, core_files):
    """Verify actual demo/debug manifests, pinned Rust dependency and copied FFI libraries for this candidate.
    验证此候选实际 demo 及调试清单、冻结 Rust 依赖及复制的 FFI 库。
    """
    for mode in ("ffi", "rust"):
        # Files are the established demo archive layout emitted by the existing packagers.
        # 文件使用既有打包器输出的既定 demo 归档布局。
        files = archive_files(directory / f"luaskills-demo-{mode}-{platform}.tar.gz")
        # Manifest field names come from current package_demo.ps1/sh, without alias probing.
        # 清单字段名来自当前 package_demo.ps1/sh，不探测别名。
        manifest = decode_json(files["demo-manifest.json"])
        if manifest["platform"] != platform or manifest["mode"] != mode or manifest["release_tag"] != f"v{version}":
            raise ValueError(f"Demo candidate manifest mismatch: {mode}/{platform}")
        if mode == "ffi":
            for name in PLATFORMS[platform][3]:
                if files[f"lib/{name}"] != core_files[f"lib/{name}"]:
                    raise ValueError(f"FFI demo library differs from the verified core: {name}")
        else:
            # Dependency is parsed from the packaged Cargo manifest, not checked by text substring.
            # 依赖从包内 Cargo 清单解析，不通过文本子字符串检查。
            dependency = tomllib.loads(files["Cargo.toml"].decode("utf-8-sig"))["dependencies"]["luaskills"]
            if dependency != {"git": "https://github.com/LuaSkills/luaskills.git", "rev": commit}:
                raise ValueError("Rust demo dependency must pin the same frozen source commit")
    # Debug package uses its existing platform binary path declaration.
    # 调试包使用既有平台二进制路径声明。
    debug = archive_files(directory / f"luaskills-debug-tool-{platform}.tar.gz")
    # Debug manifest is authoritative for the packaged executable location.
    # 调试清单是包内可执行文件位置的权威。
    manifest = decode_json(debug["debug-tool-manifest.json"])
    # ExpectedBinary is derived from the supported OS identity, not an arbitrary manifest path.
    # ExpectedBinary 派生自受支持 OS 身份，不采用任意清单路径。
    expected_binary = "bin/luaskills-debug.exe" if PLATFORMS[platform][1] == "windows" else "bin/luaskills-debug"
    if manifest["platform"] != platform or manifest["release_tag"] != f"v{version}" or manifest["binary"] != expected_binary or not debug[expected_binary]:
        raise ValueError("Debug candidate manifest or executable mismatch")


def collect(args):
    """Verify all four archives for one platform and emit immutable checksums and candidate evidence.
    验证一个平台全部四种归档，并输出不可变校验和及候选证据。
    """
    # Archives are exact expected filenames; additional archives are a packaging error.
    # 归档使用精确预期文件名；额外归档属于打包错误。
    archives = [f"luaskills-{family}-{args.platform}.tar.gz" for family in ARCHIVE_FAMILIES]
    # Source name is shared verbatim by all platform candidates.
    # 源码名称由全部平台候选原样共享。
    source_name = f"luaskills-source-{args.version}-{args.source_commit}.tar.gz"
    if {path.name for path in args.output.glob("*.tar.gz")} != {*archives, source_name}:
        raise ValueError("Platform candidate must contain exactly four archives and the frozen source archive")
    # FFI package supplies the verified native build identity.
    # FFI 包提供已验证的原生构建身份。
    files = archive_files(args.output / archives[0])
    # Manifest is the release-owned archive boundary checked without unpacking to disk.
    # 清单是发布流程拥有的归档边界，无需解压到磁盘即可检查。
    manifest = decode_json(files.pop("ffi-sdk-manifest.json"))
    verify_payload(files, manifest, args.output / source_name, args.source_commit, args.version, args.platform)
    verify_auxiliary(args.output, args.platform, args.source_commit, args.version, files)
    # Record ties every archive hash to one platform's actual embedded build.
    # 记录将每个归档摘要绑定到一个平台的实际嵌入式构建。
    record = {"schema_version": MANIFEST_VERSION, "source_commit": args.source_commit, "core_version": args.version, "platform": args.platform, "source_archive": {"name": source_name, "sha256": manifest["source_archive_sha256"]}, "build": manifest["build"], "cargo": manifest["cargo"], "archives": {name: digest((args.output / name).read_bytes()) for name in archives}}
    for name, checksum in record["archives"].items():
        write_new(args.output / f"{name}.sha256", f"{checksum}  {name}\n".encode())
    write_new(args.output / f"candidate-{args.platform}.json", encode(record))


def aggregate(args):
    """Require all five platform records for one source SHA/version; copy verified assets to a new directory.
    要求同一源码 SHA 及版本的全部五平台记录；将已验证资产复制到新目录。
    """
    # Records are exact required filenames; duplicate or missing platform records are fatal.
    # 记录使用精确必需文件名；重复或缺失平台记录均为致命错误。
    records = []
    # Expected names exclude unrelated workflow artifacts.
    # 预期名称排除无关工作流产物。
    expected_names = {f"candidate-{platform}.json" for platform in PLATFORMS}
    if {path.name for path in args.input.rglob("candidate-*.json")} != expected_names:
        raise ValueError("Candidate aggregation requires every supported platform")
    for platform in PLATFORMS:
        # Matching record must occur once, even when downloads use per-artifact subdirectories.
        # 匹配记录必须仅出现一次，即使下载使用每产物子目录。
        matches = list(args.input.rglob(f"candidate-{platform}.json"))
        if len(matches) != 1:
            raise ValueError(f"Duplicate platform evidence: {platform}")
        # Path owns this platform's archive set.
        # 路径拥有此平台归档集合。
        path = matches[0]
        # Record identity is rechecked before any publishable manifest is created.
        # 创建任何可发布清单前重新检查记录身份。
        record = read_json(path)
        if record["schema_version"] != MANIFEST_VERSION or record["platform"] != platform or record["source_commit"] != args.source_commit or record["core_version"] != args.version:
            raise ValueError(f"Candidate source/version mismatch: {platform}")
        if record["source_archive"]["name"] != f"luaskills-source-{args.version}-{args.source_commit}.tar.gz":
            raise ValueError("Unexpected frozen source archive filename")
        # Checksums authenticate downloaded bytes, independently from workflow transfer success.
        # 校验和认证下载字节，不依赖工作流传输成功。
        expected = {f"luaskills-{family}-{platform}.tar.gz" for family in ARCHIVE_FAMILIES}
        if set(record["archives"]) != expected:
            raise ValueError(f"Incomplete platform archive set: {platform}")
        for name, checksum in record["archives"].items():
            if digest((path.parent / name).read_bytes()) != checksum or (path.parent / f"{name}.sha256").read_text() != f"{checksum}  {name}\n":
                raise ValueError(f"Archive or sidecar mismatch: {name}")
        # Recheck packaged file hashes after transfer, not just the outer archive digest.
        # 传输后重新检查包内文件摘要，而不只检查外层归档摘要。
        files = archive_files(path.parent / f"luaskills-ffi-sdk-{platform}.tar.gz")
        # Manifest is the exact verified archive's embedded release identity.
        # 清单是精确已验证归档的嵌入式发布身份。
        manifest = decode_json(files.pop("ffi-sdk-manifest.json"))
        if manifest["build"] != record["build"] or manifest["cargo"] != record["cargo"] or manifest["source_archive_sha256"] != record["source_archive"]["sha256"]:
            raise ValueError(f"Packaged identity mismatch: {platform}")
        verify_payload(files, manifest, path.parent / record["source_archive"]["name"], args.source_commit, args.version, platform)
        verify_auxiliary(path.parent, platform, args.source_commit, args.version, files)
        records.append((path.parent, record))
    # Shared identities must be byte-equivalent across every actual target build.
    # 共享身份必须在每个实际目标构建间保持字节等价。
    for key in ("source_sha256", "contract_sha256", "package_lock_sha256"):
        if len({record["build"][key] for _, record in records}) != 1:
            raise ValueError(f"Cross-platform source identity mismatch: {key}")
    # Cargo host and platform library/version rows may differ; release and Cargo source commit must be identical on all five hosts.
    # Cargo 宿主及平台库版本行可以不同；版本及 Cargo 源码提交必须在全部五个宿主上一致。
    for key in ("release", "commit_hash"):
        if len({record["cargo"][key] for _, record in records}) != 1:
            raise ValueError(f"Cross-platform actual Cargo identity mismatch: {key}")
    if len({json.dumps(record["source_archive"], sort_keys=True) for _, record in records}) != 1:
        raise ValueError("Cross-platform frozen source archive mismatch")
    if args.output.exists():
        raise ValueError("Aggregate output already exists; candidate assets are immutable")
    for directory, record in records:
        for name in record["archives"]:
            write_new(args.output / name, (directory / name).read_bytes())
            write_new(args.output / f"{name}.sha256", (directory / f"{name}.sha256").read_bytes())
        write_new(args.output / f"candidate-{record['platform']}.json", encode(record))
    # Source archive is copied once after all five identical digests have been established.
    # 在确定全部五个摘要一致后，源码归档仅复制一次。
    directory, first = records[0]
    # Source filename is safe because it is checked against the frozen source identity.
    # 源码文件名因对照冻结源码身份检查而安全。
    source_name = f"luaskills-source-{args.version}-{args.source_commit}.tar.gz"
    if first["source_archive"]["name"] != source_name:
        raise ValueError("Unexpected frozen source archive filename")
    write_new(args.output / source_name, (directory / source_name).read_bytes())
    write_new(args.output / f"{source_name}.sha256", f"{first['source_archive']['sha256']}  {source_name}\n".encode())
    write_new(args.output / "candidate-manifest.json", encode({"schema_version": MANIFEST_VERSION, "source_commit": args.source_commit, "core_version": args.version, "source_archive": first["source_archive"], "platforms": [record for _, record in records]}))
    print(f"Verified all {len(records)} candidate platforms: {args.output}")


def sdk_inputs(args):
    """Derive native SDK validation inputs from one accepted archive; preserve full descriptor bytes and digest.
    从一个已验收归档派生原生 SDK 验证输入；保留完整描述字节及摘要。
    """
    # Candidate metadata binds the platform archive to the same aggregate source/version.
    # 候选元数据将平台归档绑定到相同汇总源码及版本。
    manifest = read_json(args.input / "candidate-manifest.json")
    if manifest["schema_version"] != MANIFEST_VERSION or manifest["source_commit"] != args.source_commit or manifest["core_version"] != args.version:
        raise ValueError("SDK candidate source identity mismatch")
    if len(manifest["platforms"]) != len(PLATFORMS) or {record["platform"] for record in manifest["platforms"]} != set(PLATFORMS):
        raise ValueError("SDK inputs require a complete five-platform aggregate")
    for record in manifest["platforms"]:
        if record["source_commit"] != args.source_commit or record["core_version"] != args.version or record["schema_version"] != MANIFEST_VERSION or record["source_archive"] != manifest["source_archive"]:
            raise ValueError("SDK aggregate contains inconsistent platform evidence")
    # Record is selected by its explicit platform identity, without index assumptions.
    # 记录按显式平台身份选择，不假设下标。
    records = [record for record in manifest["platforms"] if record["platform"] == args.platform]
    if len(records) != 1:
        raise ValueError("SDK candidate platform record must occur exactly once")
    # Archive name is the established FFI SDK asset name for this platform.
    # 归档名称是此平台既定的 FFI SDK 资产名。
    name = f"luaskills-ffi-sdk-{args.platform}.tar.gz"
    if digest((args.input / name).read_bytes()) != records[0]["archives"][name]:
        raise ValueError("SDK candidate archive checksum mismatch")
    # Files remain validated before any native binary is staged for SDK use.
    # SDK 使用的任何原生二进制暂存前，文件保持已验证。
    files = archive_files(args.input / name)
    # Package identity preserves the exact actual build descriptor and report.
    # 包身份保留精确实际构建描述及报告。
    package_manifest = decode_json(files.pop("ffi-sdk-manifest.json"))
    # Source filename is verified before use to prevent candidate-controlled path traversal.
    # 源码名称在使用前验证，避免候选控制的路径遍历。
    source_name = f"luaskills-source-{args.version}-{args.source_commit}.tar.gz"
    if manifest["source_archive"]["name"] != source_name:
        raise ValueError("SDK candidate source archive filename mismatch")
    verify_payload(files, package_manifest, args.input / source_name, args.source_commit, args.version, args.platform)
    if args.output.exists():
        raise ValueError("SDK input directory already exists; native candidate inputs are immutable")
    # Library is the platform's actual dynamic library, never a library selected by suffix scanning.
    # 动态库是平台实际库，绝不通过后缀扫描选择。
    library_name = PLATFORMS[args.platform][3][0]
    write_new(args.output / library_name, files[f"lib/{library_name}"])
    write_new(args.output / "core-description.json", files["embedded-core-description.json"])
    # Inputs provide the exact absolute-path/hash/description interface shared by all three SDK gates.
    # 输入提供三个 SDK 门禁共享的精确绝对路径、摘要及描述接口。
    inputs = {"schema_version": MANIFEST_VERSION, "source_commit": args.source_commit, "core_version": args.version, "platform": args.platform, "library": str((args.output / library_name).resolve()), "library_sha256": digest(files[f"lib/{library_name}"]), "description": str((args.output / "core-description.json").resolve()), "description_sha256": digest(files["embedded-core-description.json"]), "archive_sha256": records[0]["archives"][name], "build": package_manifest["build"]}
    write_new(args.output / "sdk-validation-inputs.json", encode(inputs))
    print(encode(inputs).decode())


def main():
    """Parse candidate subcommands and explicit source identity; fail in English on missing evidence.
    解析候选子命令及显式源码身份；证据缺失时以英文失败。
    """
    # Parser exposes local build/validation actions only, with no publishing operation.
    # 解析器仅暴露本地构建及验证操作，不包含发布操作。
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    # Subcommands separate native packaging from transfer verification and aggregation.
    # 子命令区分原生打包、传输验证及汇总。
    commands = parser.add_subparsers(dest="command", required=True)
    for name, action in (("package", package), ("collect", collect), ("aggregate", aggregate), ("sdk-inputs", sdk_inputs)):
        # Command shares one explicit immutable source identity schema.
        # 命令共享一个显式不可变源码身份结构。
        command = commands.add_parser(name)
        command.set_defaults(action=action)
        command.add_argument("--source-commit", required=True)
        command.add_argument("--version", required=True)
        command.add_argument("--output", type=Path, required=True)
        if name in ("aggregate", "sdk-inputs"):
            command.add_argument("--input", type=Path, required=True)
        if name != "aggregate":
            command.add_argument("--platform", choices=PLATFORMS, required=True)
        if name == "package":
            command.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[2])
            command.add_argument("--source-archive", type=Path, help="Frozen gzip source archive whose complete tar bytes must match the source commit")
            command.add_argument("--build-log", type=Path, required=True)
            command.add_argument("--metadata", type=Path, required=True)
            command.add_argument("--cargo-version", type=Path, required=True)
            command.add_argument("--dry-run", action="store_true")
    # Arguments remain explicit; no fallback commit, path or package version is invented.
    # 参数保持显式；不编造回退提交、路径或包版本。
    args = parser.parse_args()
    try:
        args.action(args)
    except (ValueError, KeyError, OSError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"Candidate validation failed: {error}\n")


if __name__ == "__main__":
    main()
