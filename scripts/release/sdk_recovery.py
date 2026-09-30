#!/usr/bin/env python3
"""Validate exact Actions attempts and immutable recovery artifacts without publishing.
验证精确 Actions 轮次及不可变恢复制品，不执行发布。
"""

import argparse
import io
import re
import zipfile

import candidate
import sdk_prerequisites

# Recovery evidence has separate candidate and completion identities.
# 恢复证据分别保留候选与完成身份。
SCHEMA_VERSION = 2
# A single body limit also bounds cumulative ZIP expansion.
# 一个正文上限同时限制 ZIP 累计展开大小。
MAX_ARTIFACT_BYTES = sdk_prerequisites.MAX_BODY_BYTES
# Exact attempts use deterministic bounded pagination, never the latest-run endpoint.
# 精确轮次使用确定且有界的分页，绝不使用最新运行端点。
JOBS_PER_PAGE = 100
# The page cap prevents malformed remote totals from causing unbounded lookups.
# 页数上限避免异常远程总数导致无限查询。
MAX_JOB_PAGES = 100
# The independent binding is directly covered by the official attestation subject.
# 独立绑定文件由官方 attestation subject 直接覆盖。
BINDING_FILENAME = "recovery-binding.json"


def api_fields(value, fields, label):
    """Require object value to contain documented fields under label; return the original object.
    要求对象 value 包含 label 下的文档字段；返回原对象。
    """
    if type(value) is not dict or not set(fields).issubset(value):
        raise ValueError(f"Malformed {label}")
    return value


def exact_object(value, fields, label):
    """Validate value against exact fields and label; return the unchanged object.
    按精确 fields 及 label 验证 value；返回原对象。
    """
    if type(value) is not dict or set(value) != set(fields):
        raise ValueError(f"Invalid {label} fields")
    return value


def repository_name(value):
    """Validate one canonical owner/repository value; return the unchanged name.
    验证一个规范 owner/repository 值；返回原名称。
    """
    if not isinstance(value, str) or not re.fullmatch(r"[A-Za-z0-9_-]+/[A-Za-z0-9_.-]+", value):
        raise ValueError("Invalid recovery repository")
    if value.split("/")[1] in (".", ".."):
        raise ValueError("Invalid recovery repository")
    return value


def sha256(value):
    """Validate a lowercase SHA-256 value; return the unchanged digest.
    验证小写 SHA-256 值；返回原摘要。
    """
    if not isinstance(value, str) or not re.fullmatch(r"[0-9a-f]{64}", value):
        raise ValueError("Invalid recovery SHA-256")
    return value


def safe_filename(value):
    """Validate one portable root filename value; return that exact filename.
    验证一个可移植根文件名 value；返回精确文件名。
    """
    if not isinstance(value, str) or not value or value in (".", "..") or any(ord(character) < 32 or character in '/\\:<>"|?*' for character in value):
        raise ValueError("Recovery inventory requires portable root filenames")
    # Windows reserves device basenames even when an extension is present.
    # Windows 即使存在扩展名也保留设备基本名称。
    basename = value.split(".", 1)[0].rstrip(" ").upper()
    if value.rstrip(" .") != value or re.fullmatch(r"CON|PRN|AUX|NUL|CONIN\$|CONOUT\$|COM[1-9¹²³]|LPT[1-9¹²³]", basename):
        raise ValueError("Recovery filename is not portable")
    return value


def workflow_name(value):
    """Validate an exact repository workflow path value; return it without probing aliases.
    验证精确仓库工作流路径 value；返回原值，不探测别名。
    """
    if not isinstance(value, str) or not value.startswith(".github/workflows/"):
        raise ValueError("Invalid recovery workflow path")
    safe_filename(value[len(".github/workflows/"):])
    if not value.endswith((".yml", ".yaml")):
        raise ValueError("Invalid recovery workflow extension")
    return value


def invocation_uri(repository, run_id, run_attempt):
    """Return the official invocation URI for repository and positive run/attempt IDs.
    根据 repository 及正数运行、轮次 ID 返回官方调用 URI。
    """
    return f"https://github.com/{repository_name(repository)}/actions/runs/{sdk_prerequisites.positive_id(run_id)}/attempts/{sdk_prerequisites.positive_id(run_attempt)}"


def candidate_artifact_name(run_id, run_attempt):
    """Return the single candidate artifact name for positive run/attempt IDs.
    根据正数运行、轮次 ID 返回唯一候选制品名称。
    """
    return f"candidate-evidence-r{sdk_prerequisites.positive_id(run_id)}-a{sdk_prerequisites.positive_id(run_attempt)}"


def validate_inventory(inventory):
    """Validate exact filename/size/SHA rows in inventory; return a canonical sorted copy.
    验证 inventory 的精确文件名、大小及 SHA 行；返回规范排序副本。
    """
    if type(inventory) is not list or not inventory:
        raise ValueError("Recovery inventory must be a nonempty list")
    # Portable aliases are rejected even when the current platform is case sensitive.
    # 即使当前平台区分大小写，也拒绝可移植文件名别名。
    names = set()
    # Rows contain no inferred sizes or alternative digest fields.
    # 行中不包含推测大小或备用摘要字段。
    rows = []
    for row in inventory:
        exact_object(row, ("filename", "size", "sha256"), "recovery inventory row")
        # Filename is the single physical member authority.
        # 文件名是唯一物理成员权威。
        name = safe_filename(row["filename"])
        if name.casefold() in names:
            raise ValueError("Duplicate recovery inventory filename")
        names.add(name.casefold())
        if type(row["size"]) is not int or row["size"] < 0 or row["size"] > MAX_ARTIFACT_BYTES:
            raise ValueError("Invalid recovery inventory size")
        rows.append({"filename": name, "size": row["size"], "sha256": sha256(row["sha256"])})
    return sorted(rows, key=lambda row: row["filename"])


def inventory_for(files):
    """Return a strict inventory for exact root filename-to-bytes files; reject other input.
    根据精确根文件名到字节的 files 返回严格清单；拒绝其他输入。
    """
    if type(files) is not dict or any(type(body) is not bytes for body in files.values()):
        raise ValueError("Recovery inventory requires exact byte files")
    return validate_inventory([{"filename": name, "size": len(body), "sha256": candidate.digest(body)} for name, body in files.items()])


def compare_inventory(expected, files):
    """Require expected inventory to describe every exact file byte; return the validated rows.
    要求 expected 清单描述每份精确文件字节；返回已验证行。
    """
    # Complete equality rejects extra members and same-name substitutions.
    # 完整相等判断拒绝额外成员及同名替换。
    rows = validate_inventory(expected)
    if rows != inventory_for(files):
        raise ValueError("Recovery artifact inventory differs from signed bytes")
    return rows


def verify_attempt(http, *, repository, workflow_path, source_sha, run_id, run_attempt, required_jobs, phase):
    """Read one exact attempt via http; validate frozen identity/jobs/phase and return actual JSON evidence.
    使用 http 读取一个精确轮次；验证冻结身份、作业及 phase，并返回实际 JSON 证据。
    """
    repository_name(repository)
    workflow_name(workflow_path)
    sdk_prerequisites.full_sha(source_sha)
    sdk_prerequisites.positive_id(run_id)
    sdk_prerequisites.positive_id(run_attempt)
    if phase not in ("candidate", "completion"):
        raise ValueError("Invalid recovery attempt phase")
    if type(required_jobs) is not list or not required_jobs or any(not isinstance(name, str) or not name for name in required_jobs) or len(set(required_jobs)) != len(required_jobs):
        raise ValueError("Required job names must be an exact nonempty unique list")
    # API ownership is derived locally; remote URLs are never used to choose another repository.
    # API 归属从本地派生；绝不使用远程 URL 选择另一个仓库。
    api = f"https://api.github.com/repos/{repository}"
    # The attempt endpoint preserves original status when a later retry differs.
    # 轮次端点在后续重试状态不同时仍保留原始状态。
    endpoint = f"{api}/actions/runs/{run_id}/attempts/{run_attempt}"
    # Run response is the exact requested attempt, never a synthesized successful run.
    # 运行响应是所请求的精确轮次，绝不是合成成功运行。
    run = api_fields(http.json(endpoint), ("id", "run_attempt", "repository", "head_repository", "head_sha", "path", "workflow_id", "status", "conclusion"), "recovery attempt response")
    api_fields(run["repository"], ("full_name",), "recovery repository")
    api_fields(run["head_repository"], ("full_name",), "recovery source repository")
    if run["id"] != run_id or type(run["id"]) is not int or run["run_attempt"] != run_attempt or type(run["run_attempt"]) is not int:
        raise ValueError("Recovery run/attempt differs from the requested identity")
    if run["repository"]["full_name"] != repository or run["head_repository"]["full_name"] != repository or run["head_sha"] != source_sha:
        raise ValueError("Recovery repository/source differs from frozen identity")
    # GitHub documents workflow paths with an optional @ref suffix; the physical path remains exact.
    # GitHub 文档允许工作流路径带可选 @ref 后缀；物理路径仍须精确匹配。
    if not isinstance(run["path"], str) or run["path"].split("@", 1)[0] != workflow_path:
        raise ValueError("Recovery workflow path differs from frozen identity")
    sdk_prerequisites.positive_id(run["workflow_id"])
    if run["status"] == "completed":
        if not isinstance(run["conclusion"], str) or not run["conclusion"]:
            raise ValueError("Completed recovery attempt has no actual conclusion")
    elif phase != "candidate" or run["status"] != "in_progress" or run["conclusion"] is not None:
        raise ValueError("Recovery attempt status is inconsistent with its phase")
    if phase == "completion" and (run["status"] != "completed" or run["conclusion"] != "success"):
        raise ValueError("Recovery completion attempt did not succeed")
    # IDs and rows remain unique across every page, so pagination cannot hide duplicated gates.
    # 所有页的 ID 及行保持唯一，分页不能隐藏重复门禁。
    jobs = []
    # Total is the server-declared complete count, rechecked on every page.
    # 总数是服务端声明的完整数量，每页重新核对。
    total = None
    for page in range(1, MAX_JOB_PAGES + 1):
        # Page URL includes the explicit original attempt and fixed page size.
        # 分页 URL 包含明确原始轮次及固定分页大小。
        response = api_fields(http.json(f"{endpoint}/jobs?per_page={JOBS_PER_PAGE}&page={page}"), ("total_count", "jobs"), "recovery jobs page")
        if type(response["total_count"]) is not int or response["total_count"] < 0 or type(response["jobs"]) is not list:
            raise ValueError("Malformed recovery jobs page")
        if total is None:
            total = response["total_count"]
        if response["total_count"] != total or total > JOBS_PER_PAGE * MAX_JOB_PAGES or len(response["jobs"]) > JOBS_PER_PAGE:
            raise ValueError("Recovery jobs pagination identity changed")
        jobs.extend(response["jobs"])
        if len(jobs) == total:
            break
        if not response["jobs"] or len(jobs) > total:
            raise ValueError("Incomplete recovery jobs pagination")
    if len(jobs) != total:
        raise ValueError("Recovery jobs exceed pagination limit")
    # Required rows are selected by frozen workflow names, not inferred matrix display formatting.
    # 必需行按冻结工作流名称选取，不推测矩阵展示格式。
    selected = {}
    # Every API job ID must be unique, including jobs outside required gates.
    # 每个 API 作业 ID 均须唯一，包含必需门禁之外的作业。
    seen = set()
    for job in jobs:
        api_fields(job, ("id", "run_id", "head_sha", "run_url", "name", "status", "conclusion"), "recovery job")
        sdk_prerequisites.positive_id(job["id"])
        if job["id"] in seen or job["run_id"] != run_id or type(job["run_id"]) is not int or job["head_sha"] != source_sha or job["run_url"] != f"{api}/actions/runs/{run_id}":
            raise ValueError("Recovery job belongs to a different source/run or duplicates an ID")
        seen.add(job["id"])
        if not isinstance(job["name"], str):
            raise ValueError("Malformed recovery job name")
        if job["name"] in required_jobs:
            if job["name"] in selected or job["status"] != "completed" or job["conclusion"] != "success":
                raise ValueError("Required recovery job is duplicate or did not succeed")
            selected[job["name"]] = job
    if set(selected) != set(required_jobs):
        raise ValueError("Missing required recovery job")
    return {"schema_version": SCHEMA_VERSION, "phase": phase, "repository": repository, "workflow_path": workflow_path, "source_sha": source_sha, "run_id": run_id, "run_attempt": run_attempt, "invocation_uri": invocation_uri(repository, run_id, run_attempt), "attempt": run, "required_jobs": [selected[name] for name in required_jobs]}


def artifact_files(content, *, max_unpacked_bytes=MAX_ARTIFACT_BYTES):
    """Read bounded ZIP content as exact portable root file bytes; reject unsafe members before expansion.
    将有界 ZIP content 读取为精确可移植根文件字节；展开前拒绝不安全成员。
    """
    if type(content) is not bytes or len(content) > MAX_ARTIFACT_BYTES or type(max_unpacked_bytes) is not int or not 0 < max_unpacked_bytes <= MAX_ARTIFACT_BYTES:
        raise ValueError("Invalid recovery artifact byte limit")
    # Result never extracts to disk, eliminating filesystem aliases and traversal writes.
    # 结果绝不解压到磁盘，消除文件系统别名及目录穿越写入。
    result = {}
    # Declared expansion is checked before reading any member.
    # 读取任何成员前先检查声明展开大小。
    total = 0
    # Case-insensitive names preserve portability across all SDK runner platforms.
    # 不区分大小写的名称保持所有 SDK runner 平台间的可移植性。
    seen = set()
    try:
        with zipfile.ZipFile(io.BytesIO(content)) as archive:
            for member in archive.infolist():
                if member.orig_filename != member.filename:
                    raise ValueError("Recovery artifact ZIP member name was normalized")
                safe_filename(member.filename)
                # Unix type bits cannot disguise links, directories or devices as regular evidence.
                # Unix 类型位不能将链接、目录或设备伪装成普通证据。
                kind = (member.external_attr >> 16) & 0o170000
                if member.is_dir() or member.external_attr & 0x10 or kind not in (0, 0o100000) or member.flag_bits & 1 or member.filename.casefold() in seen:
                    raise ValueError("Unsafe or duplicate recovery artifact member")
                seen.add(member.filename.casefold())
                total += member.file_size
                if total > max_unpacked_bytes:
                    raise ValueError("Recovery artifact expansion exceeds its limit")
            for member in archive.infolist():
                # A bounded stream read validates actual bytes as well as untrusted ZIP sizes.
                # 有界流读取同时验证实际字节及不可信 ZIP 大小。
                with archive.open(member) as stream:
                    body = stream.read(member.file_size + 1)
                if len(body) != member.file_size:
                    raise ValueError("Recovery artifact member size mismatch")
                result[member.filename] = body
    except (zipfile.BadZipFile, RuntimeError, NotImplementedError, OSError) as error:
        raise ValueError("Malformed recovery artifact ZIP") from error
    if not result:
        raise ValueError("Empty recovery artifact ZIP")
    return result


def download_artifact(http, *, repository, source_sha, run_id, artifact_id, artifact_name, max_unpacked_bytes=MAX_ARTIFACT_BYTES):
    """Download one explicit artifact ID through http; return metadata evidence and safe file bytes, without claiming attempt binding.
    通过 http 下载一个明确 artifact_id；返回元数据证据及安全文件字节，不宣称已绑定轮次。
    """
    repository_name(repository)
    sdk_prerequisites.full_sha(source_sha)
    sdk_prerequisites.positive_id(run_id)
    sdk_prerequisites.positive_id(artifact_id)
    if not isinstance(artifact_name, str) or not artifact_name:
        raise ValueError("Recovery artifact name must be explicit")
    # Only the requested artifact ID can determine the metadata and download endpoints.
    # 只有所请求的制品 ID 能决定元数据及下载端点。
    endpoint = f"https://api.github.com/repos/{repository}/actions/artifacts/{artifact_id}"
    # Metadata establishes source/run/ID, while a later signed binding establishes the attempt.
    # 元数据建立源码、运行及 ID 身份，稍后的签名绑定建立轮次身份。
    artifact = api_fields(http.json(endpoint), ("id", "name", "expired", "workflow_run", "size_in_bytes", "digest"), "recovery artifact response")
    api_fields(artifact["workflow_run"], ("id", "head_sha"), "recovery artifact workflow run")
    if artifact["id"] != artifact_id or type(artifact["id"]) is not int or artifact["name"] != artifact_name or artifact["expired"] is not False:
        raise ValueError("Recovery artifact ID/name/expiry mismatch")
    if artifact["workflow_run"]["id"] != run_id or type(artifact["workflow_run"]["id"]) is not int or artifact["workflow_run"]["head_sha"] != source_sha:
        raise ValueError("Recovery artifact source/run mismatch")
    if type(artifact["size_in_bytes"]) is not int or not 0 < artifact["size_in_bytes"] <= MAX_ARTIFACT_BYTES:
        raise ValueError("Invalid recovery artifact download size")
    if not isinstance(artifact["digest"], str) or not artifact["digest"].startswith("sha256:"):
        raise ValueError("Missing recovery artifact SHA-256 digest")
    # Digest is the actual GitHub upload-artifact ZIP digest, never an inventory self-hash.
    # 摘要是 GitHub upload-artifact ZIP 的实际摘要，绝不是清单自哈希。
    expected_digest = sha256(artifact["digest"][len("sha256:"):])
    # Body is read through the existing authenticated, credential-safe HTTPS transport.
    # 正文通过已有经认证且凭据安全的 HTTPS 传输读取。
    content = http.get(f"{endpoint}/zip", binary=True)[0]
    if len(content) != artifact["size_in_bytes"] or candidate.digest(content) != expected_digest:
        raise ValueError("Recovery artifact downloaded bytes differ from API digest/size")
    # Files remain untrusted until the SDK's official attestation verifier authenticates a binding.
    # 文件在 SDK 官方 attestation 验证器认证绑定前仍不受信任。
    files = artifact_files(content, max_unpacked_bytes=max_unpacked_bytes)
    return {"schema_version": SCHEMA_VERSION, "repository": repository, "source_sha": source_sha, "run_id": run_id, "artifact": artifact, "archive_sha256": expected_digest, "inventory": inventory_for(files), "attempt_bound": False}, files


def verify_signed_binding(binding_bytes, *, verified_subjects, verified_invocation_uri, verified_source_sha, repository, workflow_path, source_sha, run_id, run_attempt, artifact_name):
    """Bind exact manifest bytes to SDK-verified official subjects/invocation/source and frozen candidate identity; return verified JSON.
    将精确清单字节绑定到 SDK 已验证官方 subject、调用及源码和冻结候选身份；返回已验证 JSON。
    """
    repository_name(repository)
    workflow_name(workflow_path)
    sdk_prerequisites.full_sha(source_sha)
    sdk_prerequisites.positive_id(run_id)
    sdk_prerequisites.positive_id(run_attempt)
    if type(binding_bytes) is not bytes or len(binding_bytes) > MAX_ARTIFACT_BYTES:
        raise ValueError("Recovery binding requires bounded exact bytes")
    if artifact_name != candidate_artifact_name(run_id, run_attempt):
        raise ValueError("Recovery candidate artifact name does not identify its exact attempt")
    if type(verified_subjects) is not dict or not verified_subjects:
        raise ValueError("Official verified subjects are required")
    # These normalized values must come from the SDK's existing official gh verification output.
    # 这些归一化值必须来自 SDK 已有官方 gh 验签输出。
    for name, checksum in verified_subjects.items():
        safe_filename(name)
        sha256(checksum)
    if verified_subjects.get(BINDING_FILENAME) != candidate.digest(binding_bytes) or verified_source_sha != source_sha or verified_invocation_uri != invocation_uri(repository, run_id, run_attempt):
        raise ValueError("Official signature does not cover the exact candidate binding/source/attempt")
    # Binding is an independent subject; neither it nor its subsequently generated signature hashes itself.
    # 绑定是独立 subject；绑定自身及随后生成的签名均不自哈希。
    binding = exact_object(candidate.decode_json(binding_bytes), ("schema_version", "kind", "repository", "workflow_path", "source_sha", "run_id", "run_attempt", "artifact_name", "inventory"), "signed recovery binding")
    if type(binding["schema_version"]) is not int or binding["schema_version"] != SCHEMA_VERSION or binding["kind"] != "sdk-candidate":
        raise ValueError("Unsupported signed recovery binding schema/kind")
    # Expected identity is source-owned input, while signed identity is authenticated evidence; both must agree.
    # 预期身份是源码所属输入，签名身份是经认证证据；两者必须一致。
    expected = {"repository": repository, "workflow_path": workflow_path, "source_sha": source_sha, "run_id": run_id, "run_attempt": run_attempt, "artifact_name": artifact_name}
    if any(binding[name] != value for name, value in expected.items()) or type(binding["run_id"]) is not int or type(binding["run_attempt"]) is not int:
        raise ValueError("Signed recovery candidate identity differs from frozen input")
    # Inventory excludes this independent subject and is validated separately against complete physical bytes.
    # 清单排除该独立 subject，并另行对照完整物理字节验证。
    inventory = validate_inventory(binding["inventory"])
    if BINDING_FILENAME in {row["filename"] for row in inventory}:
        raise ValueError("Signed recovery binding cannot hash itself")
    return {"schema_version": SCHEMA_VERSION, "binding": binding, "binding_filename": BINDING_FILENAME, "binding_sha256": candidate.digest(binding_bytes), "verified_invocation_uri": verified_invocation_uri, "verified_source_sha": verified_source_sha}


def verify_inventory(verified_binding, files, *, attestation_filename):
    """Validate every artifact file against verified_binding plus explicit attestation filename; return signed payload inventory.
    根据 verified_binding 及明确 attestation_filename 验证每个制品文件；返回签名载荷清单。
    """
    exact_object(verified_binding, ("schema_version", "binding", "binding_filename", "binding_sha256", "verified_invocation_uri", "verified_source_sha"), "verified recovery binding")
    if verified_binding["schema_version"] != SCHEMA_VERSION or verified_binding["binding_filename"] != BINDING_FILENAME:
        raise ValueError("Unsupported verified recovery binding")
    safe_filename(attestation_filename)
    if attestation_filename == BINDING_FILENAME:
        raise ValueError("Recovery attestation filename overlaps binding")
    if type(files) is not dict or any(type(body) is not bytes for body in files.values()):
        raise ValueError("Recovery inventory requires exact byte files")
    # Binding and signature bytes are carried physically but cannot be their own inventory subjects.
    # 绑定及签名字节以物理文件携带，但不能成为自身清单 subject。
    excluded = {BINDING_FILENAME, attestation_filename}
    # Rows contain precisely all immutable packages and original native/core evidence.
    # 行精确包含全部不可变包及原始原生、核心证据。
    rows = validate_inventory(verified_binding["binding"]["inventory"])
    if excluded.intersection(row["filename"] for row in rows) or set(files) != {row["filename"] for row in rows} | excluded:
        raise ValueError("Recovery artifact contains missing/extra/self-hashed evidence")
    if not files[attestation_filename] or candidate.digest(files[BINDING_FILENAME]) != verified_binding["binding_sha256"] or candidate.decode_json(files[BINDING_FILENAME]) != verified_binding["binding"]:
        raise ValueError("Recovery binding/signature bytes changed or are missing")
    return compare_inventory(rows, {name: body for name, body in files.items() if name not in excluded})


def main():
    """Parse the read-only exact-attempt CLI and print actual JSON evidence; return nothing.
    解析只读精确轮次 CLI，并打印实际 JSON 证据；不返回值。
    """
    # The CLI exposes no registry, Release, upload or rerun mutation operation.
    # CLI 不暴露 registry、Release、上传或重试变更操作。
    parser = argparse.ArgumentParser(description=__doc__)
    # Subcommands have explicit phase semantics instead of overloading a previous run ID.
    # 子命令采用明确阶段语义，不复用旧运行 ID 含义。
    commands = parser.add_subparsers(dest="command", required=True)
    # Exact-attempt lookup is the only network-facing CLI entry point.
    # 精确轮次查询是唯一面向网络的 CLI 入口。
    attempt = commands.add_parser("attempt", help="Verify one specified Actions attempt; emit actual JSON.")
    attempt.add_argument("--repository", required=True)
    attempt.add_argument("--workflow-path", required=True)
    attempt.add_argument("--source-sha", required=True)
    attempt.add_argument("--run-id", required=True, type=int)
    attempt.add_argument("--run-attempt", required=True, type=int)
    attempt.add_argument("--required-job", dest="required_jobs", action="append", required=True)
    attempt.add_argument("--phase", required=True, choices=("candidate", "completion"))
    # Arguments retain exact source-owned job names, including spaces if explicitly declared.
    # 参数保留源码所属的精确作业名称，包含明确声明时的空格。
    arguments = parser.parse_args()
    print(candidate.encode(verify_attempt(sdk_prerequisites.Http(), repository=arguments.repository, workflow_path=arguments.workflow_path, source_sha=arguments.source_sha, run_id=arguments.run_id, run_attempt=arguments.run_attempt, required_jobs=arguments.required_jobs, phase=arguments.phase)).decode("utf-8"), end="")


if __name__ == "__main__":
    main()

