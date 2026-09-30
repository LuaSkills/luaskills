#!/usr/bin/env python3
"""Create a new GitHub draft from accepted candidates; never update an existing tag or asset.
从已验收候选创建新 GitHub 草稿；绝不更新既有标签或资产。
"""

import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import urllib.error
import urllib.parse
import urllib.request

from candidate import ARCHIVE_FAMILIES, MANIFEST_VERSION, PLATFORMS, cargo_identity, digest, encode, git, read_json, write_new

# Exact draft-source assets are shared with the public-release prerequisite gate.
# 精确草稿源码资产与公开发布前置门禁共享。
DRAFT_SOURCE_EVIDENCE_ASSETS = ("draft-source-evidence.json", "draft-source-current-evidence.json")


def verified_assets(directory, commit, version):
    """Recheck exact aggregate hashes and identity; return only the explicitly accepted upload paths.
    重新检查精确汇总摘要及身份；仅返回显式已验收的上传路径。
    """
    # Manifest is the independent aggregate gate's output, rather than an arbitrary file glob.
    # 清单是独立汇总门禁输出，而非任意文件通配。
    manifest = read_json(directory / "candidate-manifest.json")
    if not re.fullmatch(r"[0-9a-f]{40}", commit) or manifest["schema_version"] != MANIFEST_VERSION or manifest["source_commit"] != commit or manifest["core_version"] != version:
        raise ValueError("Draft candidate source identity mismatch")
    if len(manifest["platforms"]) != len(PLATFORMS) or {record["platform"] for record in manifest["platforms"]} != set(PLATFORMS):
        raise ValueError("Draft requires all five accepted platforms")
    # Assets are selected solely from the accepted mandatory archive set and its checksum sidecars.
    # 资产仅从已验收必需归档集合及其校验和附属文件中选择。
    assets = [directory / "candidate-manifest.json"]
    for record in manifest["platforms"]:
        if record["source_commit"] != commit or record["core_version"] != version or record["schema_version"] != MANIFEST_VERSION:
            raise ValueError("Draft contains inconsistent platform evidence")
        if record["source_archive"] != manifest["source_archive"]:
            raise ValueError("Draft contains inconsistent frozen source archives")
        if cargo_identity(record["cargo"]["version_verbose"].encode("utf-8")) != record["cargo"] or record["cargo"]["host"] != PLATFORMS[record["platform"]][0]:
            raise ValueError("Draft actual Cargo identity is inconsistent")
        if set(record["archives"]) != {f"luaskills-{family}-{record['platform']}.tar.gz" for family in ARCHIVE_FAMILIES}:
            raise ValueError("Draft has an incomplete platform archive set")
        for name, checksum in record["archives"].items():
            # Archive path is derived from already validated family and platform keys.
            # 归档路径派生自已验证的种类及平台键。
            archive = directory / name
            if digest(archive.read_bytes()) != checksum or (directory / f"{name}.sha256").read_text() != f"{checksum}  {name}\n":
                raise ValueError(f"Draft asset checksum mismatch: {name}")
            assets.extend((archive, directory / f"{name}.sha256"))
        # Evidence is uploaded verbatim only after matching the aggregate-owned platform record.
        # 证据仅在匹配汇总拥有的平台记录后才原样上传。
        evidence = directory / f"candidate-{record['platform']}.json"
        if read_json(evidence) != record:
            raise ValueError("Draft platform evidence mismatch")
        assets.append(evidence)
    # Frozen source filename is explicit and commit-scoped.
    # 冻结源码文件名显式并按提交划定范围。
    source_name = f"luaskills-source-{version}-{commit}.tar.gz"
    if manifest["source_archive"]["name"] != source_name:
        raise ValueError("Draft source archive filename mismatch")
    # Source archive digest and sidecar bind this draft to the actual committed inputs.
    # 源码归档摘要及附属文件将草稿绑定到实际已提交输入。
    checksum = manifest["source_archive"]["sha256"]
    if digest((directory / source_name).read_bytes()) != checksum or (directory / f"{source_name}.sha256").read_text() != f"{checksum}  {source_name}\n":
        raise ValueError("Draft source archive checksum mismatch")
    assets.extend((directory / source_name, directory / f"{source_name}.sha256"))
    for key in ("source_sha256", "contract_sha256", "package_lock_sha256"):
        if len({record["build"][key] for record in manifest["platforms"]}) != 1:
            raise ValueError(f"Draft cross-platform source mismatch: {key}")
    for key in ("release", "commit_hash"):
        if len({record["cargo"][key] for record in manifest["platforms"]}) != 1:
            raise ValueError(f"Draft cross-platform actual Cargo mismatch: {key}")
    return assets


def request(url, token, method="GET", content=None, content_type="application/json"):
    """Perform one authenticated GitHub API request; propagate every failure except explicit caller-handled 404.
    执行一次已认证 GitHub API 请求；传播全部失败，只有调用方显式处理的 404 例外。
    """
    # HTTP request contains no retry that could duplicate a release or asset mutation.
    # HTTP 请求不包含可能重复发布或资产写入的重试。
    operation = urllib.request.Request(url, data=content, method=method, headers={"Authorization": f"Bearer {token}", "Accept": "application/vnd.github+json", "X-GitHub-Api-Version": "2022-11-28", "Content-Type": content_type})
    with urllib.request.urlopen(operation, timeout=60) as response:
        return json.load(response)


def require_absent(url, token):
    """Require an explicitly absent release/tag; fail for existing objects or any non-404 API failure.
    要求发布或标签明确不存在；已存在对象及任何非 404 API 失败均报错。
    """
    try:
        request(url, token)
    except urllib.error.HTTPError as error:
        if error.code == 404:
            error.close()
            return
        raise
    raise ValueError("Release or tag already exists; immutable candidates cannot overwrite it")


def require_unused_release_tag(base, repository, tag, token):
    """Reject an existing ref, published release or authenticated paginated draft release before any POST.
    在任何 POST 前拒绝既有引用、已公开发布或认证分页列表中的草稿发布。
    """
    if not token:
        raise ValueError("Authenticated release-list evidence is required")
    require_absent(f"{base}/repos/{repository}/releases/tags/{urllib.parse.quote(tag, safe='')}", token)
    require_absent(f"{base}/repos/{repository}/git/ref/tags/{urllib.parse.quote(tag, safe='')}", token)
    # Page size is the documented maximum; continue until the server returns a short final page.
    # 页大小是文档规定上限；持续读取直到服务器返回不足一页的最终结果。
    page = 1
    while True:
        # Releases include drafts visible to this authenticated contents-write installation token.
        # 发布列表包含此已认证内容写权限安装令牌可见的草稿。
        releases = request(f"{base}/repos/{repository}/releases?per_page=100&page={page}", token)
        if not isinstance(releases, list) or len(releases) > 100:
            raise ValueError("Cannot prove complete authenticated release-list evidence")
        for release in releases:
            if not isinstance(release, dict) or not isinstance(release["tag_name"], str):
                raise ValueError("Release list contains unknown tag identity")
            if release["tag_name"] == tag:
                raise ValueError("Release or draft already exists; immutable candidates cannot overwrite it")
        if len(releases) < 100:
            return
        page += 1


def full_sha(value):
    """Validate one full lowercase Git object ID; return it without guessing or resolving names.
    验证一个完整小写 Git 对象身份；不猜测或解析名称，直接返回。
    """
    if not isinstance(value, str) or not re.fullmatch(r"[0-9a-f]{40}", value):
        raise ValueError("Full Git object identity is required for draft source evidence")
    return value


def source_workflows_tree(root, commit):
    """Read the actual committed workflows subtree from local Git; return its exact tree ID.
    从本地 Git 读取实际已提交工作流子树；返回精确树身份。
    """
    full_sha(commit)
    if git(root, "rev-parse", "HEAD").decode().strip() != commit:
        raise ValueError("Draft checkout HEAD differs from the frozen source commit")
    # Tree object belongs to the committed source, never the dirty working-directory files.
    # 树对象属于已提交源码，绝不是脏工作目录文件。
    tree = full_sha(git(root, "rev-parse", f"{commit}:.github/workflows").decode().strip())
    if git(root, "cat-file", "-t", tree).decode().strip() != "tree":
        raise ValueError("Draft source workflows path must identify a Git tree")
    return tree


def remote_child_tree(base, repository, parent_tree, name, token, *, lookup=None):
    """Read one complete GitHub Git tree and require one named directory; return the child's verified tree SHA.
    读取一个完整 GitHub Git 树并要求一个指定名称目录；返回子树已验证 SHA。
    """
    # Tree response must be complete and match the exact requested immutable parent object.
    # 树响应必须完整，并匹配精确请求的不可变父对象。
    # Lookup explicitly injects the caller's checked HTTP client when supplied; object evidence rules stay identical.
    # Lookup 在提供时显式注入调用方受检 HTTP 客户端；对象证据规则保持相同。
    tree = lookup(f"{base}/repos/{repository}/git/trees/{full_sha(parent_tree)}") if lookup is not None else request(f"{base}/repos/{repository}/git/trees/{full_sha(parent_tree)}", token)
    if not isinstance(tree, dict) or tree["sha"] != parent_tree or tree["truncated"] is not False or not isinstance(tree["tree"], list):
        raise ValueError("Remote default-branch Git tree evidence is incomplete")
    # Children are selected by the confirmed Git tree path and kind, with no fallback branch name.
    # 子项按已确认 Git 树路径及类型选择，不回退分支名称。
    children = [entry for entry in tree["tree"] if entry["path"] == name and entry["type"] == "tree"]
    if len(children) != 1:
        raise ValueError(f"Remote default-branch directory evidence is missing: {name}")
    return full_sha(children[0]["sha"])


def published_commit_trees(base, repository, commit, token, *, lookup=None):
    """Read immutable historical commit/root/workflows objects; return exact root and workflows tree IDs.
    读取不可变历史提交、根树及工作流对象；返回精确根树及工作流树身份。
    """
    full_sha(commit)
    if lookup is None and not token:
        raise ValueError("Published source evidence requires an explicit checked lookup or authentication token")
    # Commit URL is derived solely from the declared repository and full immutable commit identity.
    # 提交 URL 仅派生自声明仓库及完整不可变提交身份。
    url = f"{base}/repos/{repository}/git/commits/{commit}"
    # Description preserves the API's exact ownership of the root tree.
    # Description 保留 API 对根树的精确归属。
    description = lookup(url) if lookup is not None else request(url, token)
    if not isinstance(description, dict) or description["sha"] != commit or not isinstance(description["tree"], dict):
        raise ValueError("Published historical commit evidence is inconsistent")
    # RootTree and GithubTree are reached from this historical commit, never the current default branch.
    # RootTree 及 GithubTree 从此历史提交到达，绝不来自当前默认分支。
    root_tree = full_sha(description["tree"]["sha"])
    github_tree = remote_child_tree(base, repository, root_tree, ".github", token, lookup=lookup)
    return root_tree, remote_child_tree(base, repository, github_tree, "workflows", token, lookup=lookup)


def verify_published_source_evidence(evidence, commit, repository, base, token=None, *, lookup=None):
    """Validate release proof against immutable source/default commit trees; return the verified proof without querying current default.
    对照不可变源码及默认提交树验证发布证明；返回已验证证明，不查询当前默认分支。
    """
    if evidence["schema_version"] != MANIFEST_VERSION or evidence["repository"].casefold() != repository.casefold() or evidence["source_commit"] != full_sha(commit):
        raise ValueError("Published draft source evidence does not match this release")
    if not isinstance(evidence["default_branch"], str) or not evidence["default_branch"]:
        raise ValueError("Published historical default-branch label is missing")
    # Source workflows are independently bound to the release's actual full source commit through immutable API objects.
    # 源码工作流通过不可变 API 对象独立绑定到发布的实际完整源码提交。
    _, source_workflows = published_commit_trees(base, repository, commit, token, lookup=lookup)
    # Default root/workflows are bound to the captured historical default SHA, even after later default-branch changes.
    # 默认根及工作流绑定到捕获的历史默认 SHA，即使之后默认分支发生变化。
    default_root, default_workflows = published_commit_trees(base, repository, full_sha(evidence["default_commit"]), token, lookup=lookup)
    if evidence["source_workflows_tree"] != source_workflows or evidence["default_root_tree"] != default_root or evidence["default_workflows_tree"] != default_workflows or source_workflows != default_workflows:
        raise ValueError("Published source/default workflow tree identities disagree with historical Git objects")
    return evidence


def verify_frozen_source_evidence(evidence, root, commit, repository):
    """Check frozen draft source evidence against actual local Git; return its committed workflows tree.
    对照实际本地 Git 检查冻结草稿源码证据；返回其已提交工作流树。
    """
    if evidence["schema_version"] != MANIFEST_VERSION or evidence["repository"] != repository or evidence["source_commit"] != commit:
        raise ValueError("Frozen draft source evidence does not match this candidate")
    # LocalTree independently proves the source identity stated by the freeze job.
    # LocalTree 独立证明冻结任务声明的源码身份。
    local_tree = source_workflows_tree(root, commit)
    if not isinstance(evidence["default_branch"], str) or not evidence["default_branch"] or full_sha(evidence["default_commit"]) != evidence["default_commit"] or full_sha(evidence["default_root_tree"]) != evidence["default_root_tree"]:
        raise ValueError("Frozen actual default-branch evidence is missing")
    if evidence["source_workflows_tree"] != local_tree or evidence["default_workflows_tree"] != local_tree:
        raise ValueError("Draft source workflows differ from the verified default branch; merge workflow changes first")
    return local_tree


def draft_source_evidence(root, commit, repository, base, token):
    """Read the actual remote default branch and require identical committed workflow trees; return frozen evidence.
    读取实际远端默认分支并要求已提交工作流树相同；返回冻结证据。
    """
    if not token:
        raise ValueError("Authenticated default-branch source evidence is required")
    # Local tree comes from this same frozen full source commit.
    # 本地树来自此相同冻结完整源码提交。
    local_tree = source_workflows_tree(root, commit)
    # Repository metadata is the sole authority for the actual default branch name.
    # 仓库元数据是实际默认分支名称的唯一权威。
    metadata = request(f"{base}/repos/{repository}", token)
    if not isinstance(metadata, dict) or not isinstance(metadata["full_name"], str) or metadata["full_name"].casefold() != repository.casefold() or not isinstance(metadata["default_branch"], str) or not metadata["default_branch"]:
        raise ValueError("Cannot establish the actual remote repository default branch")
    # DefaultBranch is never inferred as main or master.
    # DefaultBranch 绝不推断为 main 或 master。
    default_branch = metadata["default_branch"]
    # Ref resolves the actual default branch exactly once before reading immutable commit/tree objects.
    # 引用在读取不可变提交及树对象前，仅解析一次实际默认分支。
    ref = request(f"{base}/repos/{repository}/git/ref/heads/{urllib.parse.quote(default_branch, safe='')}", token)
    if not isinstance(ref, dict) or not isinstance(ref["object"], dict) or ref["ref"] != f"refs/heads/{default_branch}" or ref["object"]["type"] != "commit":
        raise ValueError("Remote default branch does not identify the required commit")
    # DefaultCommit remains separate from the candidate commit and is preserved as actual remote evidence.
    # DefaultCommit 与候选提交分离，并作为实际远端证据保留。
    default_commit = full_sha(ref["object"]["sha"])
    # Commit response owns the root tree; no field-path probing is permitted.
    # 提交响应拥有根树；不允许字段路径探测。
    default = request(f"{base}/repos/{repository}/git/commits/{default_commit}", token)
    if not isinstance(default, dict) or not isinstance(default["tree"], dict) or default["sha"] != default_commit:
        raise ValueError("Remote default commit evidence is inconsistent")
    # RootTree and GithubTree are immutable objects reached from the actual default commit.
    # RootTree 及 GithubTree 是从实际默认提交到达的不可变对象。
    root_tree = full_sha(default["tree"]["sha"])
    github_tree = remote_child_tree(base, repository, root_tree, ".github", token)
    # WorkflowsTree is compared by exact Git tree identity, including filenames, modes and file bytes.
    # WorkflowsTree 按精确 Git 树身份比较，覆盖文件名、模式及文件字节。
    workflows_tree = remote_child_tree(base, repository, github_tree, "workflows", token)
    if workflows_tree != local_tree:
        raise ValueError("Draft source workflows differ from the actual default branch; merge workflow changes first")
    return {"schema_version": MANIFEST_VERSION, "repository": repository, "source_commit": commit, "source_workflows_tree": local_tree, "default_branch": default_branch, "default_commit": default_commit, "default_root_tree": root_tree, "default_workflows_tree": workflows_tree}


def main():
    """Parse explicit accepted candidate inputs and create one draft; partial upload failures remain visible.
    解析显式已验收候选输入并创建一个草稿；部分上传失败保持可见。
    """
    # Parser requires the same frozen identity already validated by aggregation.
    # 解析器要求与汇总已验证相同的冻结身份。
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--candidate", required=True, type=Path)
    parser.add_argument("--source-commit", required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--source-evidence", required=True, type=Path)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument("--dry-run", action="store_true")
    # Arguments never infer a tag or source SHA from ambient branch names.
    # 参数绝不从环境分支名推断标签或源码 SHA。
    args = parser.parse_args()
    try:
        # Assets are fully checked before any network mutation or credentials access.
        # 在任何网络写入或凭据访问前完整检查资产。
        assets = verified_assets(args.candidate, args.source_commit, args.version)
        if args.source_evidence.name != DRAFT_SOURCE_EVIDENCE_ASSETS[0]:
            raise ValueError("Frozen source evidence asset name differs from the shared release contract")
        # Repository and token are authoritative GitHub workflow environment inputs.
        # 仓库及令牌是权威 GitHub 工作流环境输入。
        repository = os.environ["GITHUB_REPOSITORY"]
        # FrozenEvidence was generated by the draft-only freeze gate before expensive platform builds.
        # FrozenEvidence 在昂贵平台构建前，由仅草稿模式的冻结门禁生成。
        frozen_evidence = read_json(args.source_evidence)
        verify_frozen_source_evidence(frozen_evidence, args.root, args.source_commit, repository)
        assets.append(args.source_evidence)
        if args.dry_run:
            print(json.dumps([str(path) for path in assets], indent=2))
            return
        # API base supports the workflow's declared GitHub server, without candidate-controlled URLs.
        # API 基址支持工作流声明的 GitHub 服务器，不接受候选控制的 URL。
        base = os.environ["GITHUB_API_URL"]
        # Token is never written into the candidate manifest or logs.
        # 令牌绝不写入候选清单或日志。
        token = os.environ["GITHUB_TOKEN"]
        # Tag is frozen to the exact accepted Cargo version.
        # 标签冻结为精确已验收 Cargo 版本。
        tag = f"v{args.version}"
        require_unused_release_tag(base, repository, tag, token)
        # Re-read actual default workflows immediately before mutation because the default branch may advance during builds.
        # 在写入前立即重读实际默认工作流，因为默认分支可能在构建期间前进。
        # CurrentEvidence records the actual default SHA/tree observed after platform acceptance, allowing SHA changes only with equal workflows.
        # CurrentEvidence 记录平台验收后观察的实际默认 SHA 及树，仅在工作流相同时允许 SHA 变化。
        current_evidence = draft_source_evidence(args.root, args.source_commit, repository, base, token)
        # EvidencePath is exclusive and uploaded beside the original freeze evidence for review.
        # EvidencePath 独占写入，并与原始冻结证据一同上传供复核。
        evidence_path = args.candidate / DRAFT_SOURCE_EVIDENCE_ASSETS[1]
        write_new(evidence_path, encode(current_evidence))
        assets.append(evidence_path)
        # Draft creation is separate from publication; duplicate tag conflicts are fatal.
        # 草稿创建与公开发布分离；重复标签冲突是致命错误。
        release = request(f"{base}/repos/{repository}/releases", token, "POST", json.dumps({"tag_name": tag, "target_commitish": args.source_commit, "name": f"LuaSkills {tag} candidate", "draft": True, "body": f"Verified five-platform candidate from {args.source_commit}. Cargo and SDK publishing are separate stages."}).encode())
        # Upload URL is GitHub's returned release-specific URL, with its documented template removed.
        # 上传 URL 是 GitHub 返回的发布专用 URL，移除其声明的模板。
        upload_url = release["upload_url"].split("{", 1)[0]
        print(f"Created draft: {release['html_url']}")
        for asset in assets:
            request(f"{upload_url}?name={urllib.parse.quote(asset.name, safe='')}", token, "POST", asset.read_bytes(), "application/octet-stream")
    except (ValueError, KeyError, TypeError, OSError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"Draft creation failed; no existing assets were modified: {error}\n")


if __name__ == "__main__":
    main()
