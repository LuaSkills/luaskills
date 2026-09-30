#!/usr/bin/env python3
"""Verify public core publication and real registry consumption before any SDK release.
在任何 SDK 发布前验证公共核心发布及真实 registry 消费。
"""

import argparse
from datetime import datetime, timezone
import io
import os
from pathlib import Path, PurePosixPath, PureWindowsPath
import re
import signal
import subprocess
import tarfile
import tempfile
import urllib.error
import urllib.parse
import urllib.request
import zipfile

import candidate
import create_draft

# Publication authority is one exact public repository, never a caller-selected mirror.
# 发布权威是一个精确公共仓库，绝不是调用方选择的镜像。
REPOSITORY = "LuaSkills/luaskills"
# All repository API addresses derive from this fixed ownership boundary.
# 所有仓库 API 地址均从此固定归属边界派生。
REPO_API = f"https://api.github.com/repos/{REPOSITORY}"
# Registry identity must match Cargo's canonical crates.io source string.
# Registry 身份必须匹配 Cargo 规范的 crates.io 来源字符串。
REGISTRY_SOURCE = "registry+https://github.com/rust-lang/crates.io-index"
# One limit bounds every downloaded body, including large native archives.
# 一个上限约束所有下载正文，包含大型原生归档。
MAX_BODY_BYTES = 1024 * 1024 * 1024
# Every process has a finite upper bound; Rust invocation deadlines are shorter internally.
# 每个进程具有有限上限；Rust 调用内部截止时间更短。
PROCESS_TIMEOUT_SECONDS = 1800


class SafeRedirect(urllib.request.HTTPRedirectHandler):
    """Keep HTTPS redirects but never forward a GitHub bearer token to another host.
    保留 HTTPS 重定向，但绝不将 GitHub bearer token 转发到其他主机。
    """

    def redirect_request(self, req, fp, code, msg, headers, newurl):
        """Return a safe request for redirect parameters, or reject an insecure destination.
        按重定向参数返回安全请求，或拒绝不安全目标。
        """
        if urllib.parse.urlsplit(newurl).scheme != "https":
            raise ValueError("Publication download redirected outside HTTPS")
        # Request inherits urllib's redirect protocol and strips cross-host credentials.
        # 请求沿用 urllib 重定向协议，并去除跨主机凭据。
        redirected = super().redirect_request(req, fp, code, msg, headers, newurl)
        if redirected is not None and urllib.parse.urlsplit(req.full_url).netloc != urllib.parse.urlsplit(newurl).netloc:
            redirected.remove_header("Authorization")
        return redirected


class Http:
    """Provide bounded read-only HTTP; no login, publication or secret-bearing diagnostics.
    提供有界只读 HTTP；无登录、发布或包含秘密的诊断。
    """

    def __init__(self):
        """Construct a redirect-safe opener using existing optional GitHub environment credentials.
        使用已有可选 GitHub 环境凭据构造重定向安全的 opener。
        """
        # Token is only supplied to the fixed GitHub API host and never written to evidence.
        # Token 仅提供给固定 GitHub API 主机，绝不写入证据。
        self.token = os.environ.get("GH_TOKEN") or os.environ.get("GITHUB_TOKEN")
        # Opener rejects plaintext redirects and removes credentials on cross-host redirects.
        # Opener 拒绝明文重定向，并在跨主机重定向时去除凭据。
        self.opener = urllib.request.build_opener(SafeRedirect())

    def get(self, url, binary=False):
        """Read URL as exact bytes with media selection; return bytes and response headers.
        按媒体选择读取 URL 精确字节；返回字节及响应头。
        """
        # Host and scheme are validated before a credential is attached.
        # 添加凭据前验证主机及协议。
        parsed = urllib.parse.urlsplit(url)
        if parsed.scheme != "https" or parsed.username is not None or parsed.password is not None:
            raise ValueError("Invalid publication HTTPS URL")
        # Headers contain no caller-controlled endpoint or account mutation.
        # 请求头不包含调用方控制的端点或账号修改。
        headers = {"User-Agent": "LuaSkills-publication-prerequisites", "Accept": "application/octet-stream" if binary else "application/json", "Cache-Control": "no-cache"}
        if parsed.netloc == "api.github.com":
            headers["X-GitHub-Api-Version"] = "2022-11-28"
            if self.token:
                headers["Authorization"] = f"Bearer {self.token}"
        try:
            with self.opener.open(urllib.request.Request(url, headers=headers), timeout=60) as response:
                if response.status != 200:
                    raise ValueError("Publication endpoint did not return HTTP 200")
                # Body size is capped before accepting any remote byte evidence.
                # 接受任何远程字节证据前限制正文大小。
                content = response.read(MAX_BODY_BYTES + 1)
                if len(content) > MAX_BODY_BYTES:
                    raise ValueError("Publication body exceeds the download limit")
                return content, dict(response.headers.items())
        except urllib.error.HTTPError as error:
            # Close error response streams without exposing request headers or redirected signed URLs.
            # 关闭错误响应流，不暴露请求头或重定向签名 URL。
            status = error.code
            error.close()
            raise ValueError(f"Publication HTTP lookup failed with status {status}") from None
        except urllib.error.URLError:
            raise ValueError("Publication HTTP lookup failed") from None

    def json(self, url):
        """Decode the exact response from URL with the candidate's duplicate-key guard; return object.
        使用候选的重复键护栏解码 URL 精确响应；返回对象。
        """
        return candidate.decode_json(self.get(url)[0])


def full_sha(value):
    """Validate value as a full lowercase commit/object SHA; return the unchanged value.
    验证 value 是完整小写提交或对象 SHA；返回原值。
    """
    if not isinstance(value, str) or not re.fullmatch(r"[0-9a-f]{40}", value):
        raise ValueError("Core identity must be a full lowercase Git SHA")
    return value


def positive_id(value):
    """Validate an API numeric ID, excluding JSON booleans; return that ID.
    验证 API 数字 ID，排除 JSON 布尔值；返回该 ID。
    """
    if type(value) is not int or value <= 0:
        raise ValueError("Publication API ID must be a positive integer")
    return value


def resolve_tag(http, tag, commit):
    """Resolve lightweight or recursively annotated tag to commit; return the complete object chain.
    将轻量或递归附注标签解析到 commit；返回完整对象链。
    """
    # Git reference is fetched by exact tag, never inferred from release.target_commitish.
    # 按精确标签获取 Git 引用，绝不从 release.target_commitish 推断。
    reference = http.json(f"{REPO_API}/git/ref/tags/{urllib.parse.quote(tag, safe='')}")
    if reference["ref"] != f"refs/tags/{tag}":
        raise ValueError("GitHub returned a different tag reference")
    # Each object is retained so a changed annotation chain is also detected on recheck.
    # 保留每个对象，使重新检查也能检测附注链变化。
    chain = []
    # Object starts at the exact ref's confirmed object field.
    # 对象从精确引用已确认的 object 字段开始。
    obj = reference["object"]
    while True:
        # SHA is authenticated by the GitHub object API and bounded against cycles/deep chains.
        # SHA 由 GitHub 对象 API 认证，并限制环及过深链。
        sha = full_sha(obj["sha"])
        if sha in {entry["sha"] for entry in chain} or len(chain) >= 32:
            raise ValueError("Invalid recursive annotated tag chain")
        chain.append({"sha": sha, "type": obj["type"]})
        if obj["type"] == "commit":
            if sha != commit or http.json(f"{REPO_API}/git/commits/{sha}")["sha"] != commit:
                raise ValueError("Public core tag does not resolve to the frozen commit")
            return chain
        if obj["type"] != "tag":
            raise ValueError("Public core tag resolves to a non-commit object")
        # Annotation is selected by the previous object's exact immutable SHA.
        # 附注按上一对象精确不可变 SHA 选择。
        annotation = http.json(f"{REPO_API}/git/tags/{sha}")
        if annotation["sha"] != sha:
            raise ValueError("Annotated tag API returned a different object")
        obj = annotation["object"]


def github_snapshot(http, tag, commit):
    """Read the official release, exact tag and all paginated assets; return their identity evidence.
    读取正式发布、精确标签及所有分页资产；返回身份凭证。
    """
    if http.json(REPO_API)["full_name"] != REPOSITORY:
        raise ValueError("Public core repository ownership mismatch")
    # Release must be public and final; missing booleans or timestamps fail closed.
    # 发布必须公开且正式；缺失布尔值或时间戳时关闭门禁。
    release = http.json(f"{REPO_API}/releases/tags/{urllib.parse.quote(tag, safe='')}")
    release_id = positive_id(release["id"])
    if release["url"] != f"{REPO_API}/releases/{release_id}" or release["tag_name"] != tag or release["draft"] is not False or release["prerelease"] is not False or not release["published_at"]:
        raise ValueError("Core prerequisite requires the exact official final GitHub release")
    # Assets are read through the paginated release-ID endpoint, not an abbreviated inline list.
    # 资产经分页发布 ID 端点读取，不采用缩略内联列表。
    assets = {}
    # Seen IDs reject aliases or repeated assets across pages.
    # 已见 ID 拒绝别名或跨页重复资产。
    ids = set()
    for page in range(1, 101):
        # Exact page URL stays inside this release and repository boundary.
        # 精确分页 URL 保持在此发布及仓库边界内。
        content, headers = http.get(f"{REPO_API}/releases/{release_id}/assets?per_page=100&page={page}")
        entries = candidate.decode_json(content)
        if not isinstance(entries, list):
            raise ValueError("GitHub assets response is not a page array")
        for asset in entries:
            # Name is one safe basename and cannot escape the fresh download directory.
            # 名称是安全单文件名，不能逃离新下载目录。
            name = asset["name"]
            asset_id = positive_id(asset["id"])
            if not isinstance(name, str) or not name or name in (".", "..") or "/" in name or "\\" in name or ":" in name or name in assets or asset_id in ids:
                raise ValueError("Duplicate or unsafe official release asset identity")
            if asset["url"] != f"{REPO_API}/releases/assets/{asset_id}" or asset["state"] != "uploaded" or type(asset["size"]) is not int or asset["size"] <= 0 or not asset["updated_at"]:
                raise ValueError("Invalid official release asset metadata")
            assets[name] = {"id": asset_id, "size": asset["size"], "url": asset["url"], "updated_at": asset["updated_at"]}
            ids.add(asset_id)
        # GitHub's Link header declares another page, even when the current page is short.
        # 即使当前页较短，GitHub Link 请求头也能声明下一页。
        link = next((value for key, value in headers.items() if key.lower() == "link"), "")
        if not re.search(r';\s*rel="next"', link):
            break
    else:
        raise ValueError("GitHub asset pagination exceeds its bounded page limit")
    return {"repository": REPOSITORY, "release_id": release_id, "tag_name": tag, "published_at": release["published_at"], "tag_resolution": resolve_tag(http, tag, commit), "assets": assets}


def expected_names(version, commit):
    """Derive required published asset names from candidate's single platform/family authority; return set.
    从候选唯一平台及归档家族权威派生必需发布资产名称；返回集合。
    """
    # Source archive is shared by every platform and bound to exact version/commit.
    # 源码归档由所有平台共享，并绑定精确版本及提交。
    source = f"luaskills-source-{version}-{commit}.tar.gz"
    # Archive names are derived, never copied into a second independent platform table.
    # 归档名称均派生，绝不复制到第二个独立平台表。
    archives = {f"luaskills-{family}-{platform}.tar.gz" for platform in candidate.PLATFORMS for family in candidate.ARCHIVE_FAMILIES} | {source}
    return {"candidate-manifest.json"} | {f"candidate-{platform}.json" for platform in candidate.PLATFORMS} | archives | {f"{name}.sha256" for name in archives} | set(create_draft.DRAFT_SOURCE_EVIDENCE_ASSETS)


def download_asset(http, asset, path):
    """Download exact official asset ID exclusively to path; return its actual SHA-256.
    将精确正式资产 ID 独占下载到 path；返回实际 SHA-256。
    """
    # Bytes come from the asset API with octet-stream media, not a guessed browser URL.
    # 字节来自采用 octet-stream 媒体的资产 API，不采用猜测的浏览器 URL。
    content = http.get(asset["url"], binary=True)[0]
    if len(content) != asset["size"]:
        raise ValueError("Official asset body size mismatch")
    candidate.write_new(path, content)
    return candidate.digest(content)


def rooted_archive(content, prefix, zipped=False):
    """Read safe regular files under one exact prefix from tar/zip bytes; return relative file map.
    从 tar 或 zip 字节读取一个精确前缀下的安全普通文件；返回相对文件映射。
    """
    # Mapping rejects duplicate members, including platform-specific path aliases.
    # 映射拒绝重复成员，包含平台相关路径别名。
    result = {}
    # All members, including directories, have unique canonical names.
    # 所有成员均具有唯一规范名称，包含目录。
    seen = set()

    def add(name, regular, directory, body):
        """Validate member name/type/body against prefix and add exact relative bytes; return nothing.
        对照 prefix 验证成员名称、类型及正文，并加入精确相对字节；不返回值。
        """
        if name in seen or "\\" in name or ":" in name or name.startswith("/") or ".." in PurePosixPath(name).parts or PurePosixPath(name).as_posix() != name.rstrip("/"):
            raise ValueError("Unsafe or duplicate publication archive member")
        seen.add(name)
        if directory:
            if name.rstrip("/") != prefix and not name.startswith(prefix + "/"):
                raise ValueError("Publication archive directory outside its package prefix")
            return
        if not regular or not name.startswith(prefix + "/"):
            raise ValueError("Publication archive member outside its package prefix")
        result[name[len(prefix) + 1:]] = body

    if zipped:
        with zipfile.ZipFile(io.BytesIO(content)) as archive:
            for member in archive.infolist():
                # Unix type bits must not disguise a symlink as a downloadable file.
                # Unix 类型位不能将符号链接伪装成可下载文件。
                kind = (member.external_attr >> 16) & 0o170000
                add(member.filename, kind in (0, 0o100000), member.is_dir(), archive.read(member) if not member.is_dir() else b"")
    else:
        with tarfile.open(fileobj=io.BytesIO(content), mode="r:gz") as archive:
            for member in archive:
                add(member.name, member.isfile(), member.isdir(), archive.extractfile(member).read() if member.isfile() else b"")
    if not result:
        raise ValueError("Empty publication source archive")
    return result


def verify_commit_source(http, downloads, commit, source_path):
    """Compare frozen source with GitHub's exact commit zip; return archive and stable member identities.
    将冻结源码与 GitHub 精确提交 zip 比较；返回归档及稳定成员身份。
    """
    # A forged git-archive comment cannot establish source identity by itself.
    # 伪造的 git-archive 注释自身无法建立源码身份。
    content = http.get(f"{REPO_API}/zipball/{commit}", binary=True)[0]
    with zipfile.ZipFile(io.BytesIO(content)) as archive:
        # GitHub archive root is obtained from actual members; no short-SHA prefix is guessed.
        # GitHub 归档根从实际成员取得；不猜测短 SHA 前缀。
        roots = {member.filename.split("/", 1)[0] for member in archive.infolist()}
    if len(roots) != 1:
        raise ValueError("GitHub commit archive has ambiguous roots")
    if rooted_archive(content, roots.pop(), zipped=True) != candidate.archive_files(source_path):
        raise ValueError("Frozen release source differs from the official Git commit archive")
    candidate.write_new(downloads / "official-commit.zip", content)
    return {"archive_sha256": candidate.digest(content), "source_sha256": candidate.digest(candidate.encode({name: candidate.digest(body) for name, body in candidate.archive_files(source_path).items()}))}


def verify_registry(http, directory, version, commit, source_path, source_sha256, report):
    """Verify exact non-yanked registry package against frozen source; return registry identity evidence.
    对照冻结源码验证精确非撤销 registry 包；返回 registry 身份证据。
    """
    # Version endpoint is exact; latest-version lookup cannot satisfy this prerequisite.
    # 版本端点精确；最新版本查询无法满足此前置条件。
    api_url = f"https://crates.io/api/v1/crates/luaskills/{urllib.parse.quote(version, safe='')}"
    response = http.get(api_url)[0]
    # Version fields are confirmed from the official crates.io API response schema.
    # 版本字段从 crates.io 官方 API 响应结构确认。
    record = candidate.decode_json(response)["version"]
    if record["crate"] != "luaskills" or record["num"] != version or record["yanked"] is not False or not re.fullmatch(r"[0-9a-f]{64}", record["checksum"]) or record["dl_path"] != f"/api/v1/crates/luaskills/{version}/download":
        raise ValueError("Exact registry luaskills version is absent, yanked or invalid")
    # Download uses crates.io's confirmed exact-version path, without alternate mirrors.
    # 下载使用 crates.io 已确认的精确版本路径，无替代镜像。
    content = http.get("https://crates.io" + record["dl_path"], binary=True)[0]
    if candidate.digest(content) != record["checksum"]:
        raise ValueError("Registry crate actual checksum mismatch")
    # Crate paths are validated before the package prefix is removed.
    # 去除包前缀前验证 crate 路径。
    files = rooted_archive(content, f"luaskills-{version}")
    # VCS metadata is necessary but never substitutes for actual source byte comparison.
    # VCS 元数据是必需条件，但绝不替代实际源码字节比较。
    vcs = candidate.decode_json(files[".cargo_vcs_info.json"])
    if vcs["git"]["sha1"] != commit or vcs["git"].get("dirty", False) is not False or vcs["path_in_vcs"] != "":
        raise ValueError("Registry crate VCS commit, cleanliness or root mismatch")
    # Frozen inputs own the original Cargo manifest and exact package lockfile identity.
    # 冻结输入拥有原始 Cargo 清单及精确包锁文件身份。
    source = candidate.archive_files(source_path)
    # Cargo officially normalizes Cargo.toml; Cargo.toml.orig must match the tracked bytes.
    # Cargo 官方规范化 Cargo.toml；Cargo.toml.orig 必须匹配受跟踪字节。
    if files["Cargo.toml.orig"] != source["Cargo.toml"]:
        raise ValueError("Registry original Cargo manifest differs from frozen source")
    # Normalized package must still declare the exact name/version and default public library.
    # 规范化包仍必须声明精确名称、版本及默认公共库。
    manifest = candidate.tomllib.loads(files["Cargo.toml"].decode())
    if manifest["package"]["name"] != "luaskills" or manifest["package"]["version"] != version:
        raise ValueError("Registry normalized Cargo package identity mismatch")
    # Every copied crate file must come from this commit; only Cargo's three generated files differ.
    # 每个复制的 crate 文件必须来自此提交；仅 Cargo 的三个生成文件不同。
    for name, body in files.items():
        if name not in ("Cargo.toml", "Cargo.toml.orig", ".cargo_vcs_info.json") and (name not in source or body != source[name]):
            raise ValueError(f"Registry package source differs from frozen commit: {name}")
    # Build report supplies the complete authoritative identity input set verified by aggregate.
    # 构建报告提供经 aggregate 验证的完整权威身份输入集合。
    # Canonicalized map compensates only for Cargo's documented manifest normalization.
    # 规范化映射仅补偿 Cargo 文档明确的清单规范化。
    inputs = {name: candidate.digest(files["Cargo.toml.orig"] if name == "Cargo.toml" else files[name]) for name in report["source_files"]}
    if inputs != report["source_files"] or candidate.source_digest(inputs, report["format_version"]) != source_sha256:
        raise ValueError("Registry complete source input identity mismatch")
    candidate.write_new(directory / "version.json", response)
    candidate.write_new(directory / f"luaskills-{version}.crate", content)
    return {"name": "luaskills", "version": version, "checksum": record["checksum"], "vcs_commit": commit, "source_sha256": source_sha256}


def isolated_environment(cargo_home, target, toolchain):
    """Build bounded registry environment for fresh cargo_home/target and verified stable toolchain; return mapping.
    为新 cargo_home、target 及已验证稳定 toolchain 构造有界 registry 环境；返回映射。
    """
    if not isinstance(toolchain, str) or not re.fullmatch(r"\d+\.\d+\.\d+", toolchain):
        raise ValueError("Controlled toolchain requires an exact stable release")
    # Environment registry overrides and compiler wrappers cannot change the source authority.
    # 环境 registry 覆盖及编译器包装器不能改变来源权威。
    # Rustup's existing read-only storage is needed by its proxies; external selection and flags remain forbidden.
    # Rustup 代理需要既有只读存储；外部工具链选择及编译标志仍被禁止。
    env = {key: value for key, value in os.environ.items() if key.upper() == "RUSTUP_HOME" or (not key.upper().startswith(("CARGO_", "RUST", "GH_", "GITHUB_")) and key.upper() not in ("LUASKILLS_RUNTIME_ROOT",))}
    env.update({"CARGO_HOME": str(cargo_home), "CARGO_TARGET_DIR": str(target), "CARGO_BUILD_JOBS": "4", "RUST_TEST_THREADS": "1", "RUSTUP_TOOLCHAIN": toolchain, "CARGO_REGISTRIES_CRATES_IO_PROTOCOL": "sparse", "CARGO_TERM_COLOR": "never"})
    return env


def compiler_identity(content):
    """Parse actual rustc verbose bytes into release, commit and host identities; return strict fields.
    将实际 rustc verbose 字节解析为 release、commit 及 host 身份；返回严格字段。
    """
    # Each compiler field must be explicitly present once; no channel/version identity is guessed.
    # 每个编译器字段必须显式存在一次；不猜测渠道或版本身份。
    fields = {}
    # Header and keyed fields must describe the same actual compiler release.
    # 标头及键值字段必须描述同一个实际编译器版本。
    lines = content.decode().splitlines()
    for line in lines:
        if ": " in line:
            key, value = line.split(": ", 1)
            if key in fields:
                raise ValueError("Duplicate rustc verbose identity field")
            fields[key] = value
    if not re.fullmatch(r"\d+\.\d+\.\d+", fields["release"]) or not fields["host"] or not lines[0].startswith(f"rustc {fields['release']} ("):
        raise ValueError("Official consumption requires a stable explicit compiler identity")
    full_sha(fields["commit-hash"])
    return {"release": fields["release"], "commit_hash": fields["commit-hash"], "host": fields["host"], "version_verbose": content.decode().rstrip(), "version_verbose_sha256": candidate.digest(content)}


def release_toolchain(records):
    """Validate every official platform's compiler identity in records; return its sole stable release.
    验证 records 中每个正式平台的编译器身份；返回唯一稳定版本。
    """
    if len(records) != len(candidate.PLATFORMS) or {record["platform"] for record in records} != set(candidate.PLATFORMS):
        raise ValueError("Toolchain selection requires every official platform record")
    # Selection precedes executable probing, so every published host must name one shared compiler release/commit.
    # 工具链选择先于可执行文件探测，因此每个已发布宿主必须声明同一编译器版本及提交。
    compilers = [compiler_identity(record["build"]["rustc"].encode("utf-8")) for record in records]
    if any(compiler["host"] != candidate.PLATFORMS[record["platform"]][0] for record, compiler in zip(records, compilers)) or len({(compiler["release"], compiler["commit_hash"]) for compiler in compilers}) != 1:
        raise ValueError("Official platform compiler identities disagree")
    return compilers[0]["release"]


def normalize_registry_manifest(directory, version, source_path, records, runner):
    """Use the proven release Cargo to normalize frozen source; return actual manifest/provenance proof.
    使用已证明的发布 Cargo 规范化冻结源码；返回实际清单及来源证明。
    """
    with tempfile.TemporaryDirectory(prefix="luaskills-manifest-normalization-") as temporary:
        # Full frozen snapshot contains exact tracked bytes, not a hand-authored approximation.
        # 完整冻结快照包含精确受跟踪字节，而非手写近似内容。
        project = Path(temporary) / "source"
        cargo_home = Path(temporary) / "cargo-home"
        project.mkdir()
        cargo_home.mkdir()
        for name, body in candidate.archive_files(source_path).items():
            candidate.write_new(project / name, body)
        # All subprocesses share one bounded, isolated registry environment.
        # 所有子进程共享一个有界隔离 registry 环境。
        env = isolated_environment(cargo_home, Path(temporary) / "target", release_toolchain(records))
        commands = []

        def execute(arguments, name):
            """Run arguments on frozen snapshot and retain named exact logs; return actual zero-exit stdout.
            在冻结快照执行 arguments 并保留具名精确日志；返回实际零退出 stdout。
            """
            # Only executed results can establish normalization, never a caller-supplied success file.
            # 仅执行结果能够建立规范化证明，绝不接受调用方成功文件。
            result = runner(arguments, project, env)
            candidate.write_new(directory / f"{name}.stdout", result.stdout)
            candidate.write_new(directory / f"{name}.stderr", result.stderr)
            commands.append({"command": arguments, "exit_code": result.returncode, "stdout_sha256": candidate.digest(result.stdout), "stderr_sha256": candidate.digest(result.stderr)})
            if result.returncode != 0:
                raise ValueError(f"Official Cargo normalization {name} failed")
            return result.stdout

        # Probe both executables themselves; compiler version alone cannot establish Cargo identity.
        # 分别探测两个可执行文件自身；仅编译器版本不能建立 Cargo 身份。
        compiler = compiler_identity(execute(["rustc", "--version", "--verbose"], "normalization-rustc"))
        cargo = candidate.cargo_identity(execute(["cargo", "--version", "--verbose"], "normalization-cargo"))
        # Exact host selects one verified native record from the sole platform declaration.
        # 精确 host 从唯一平台声明选择一个已验证原生记录。
        matching = [record for record in records if candidate.PLATFORMS[record["platform"]][0] == compiler["host"]]
        if len(matching) != 1:
            raise ValueError("Normalization toolchain host lacks exactly one official native platform")
        release_record = matching[0]
        if cargo["host"] != compiler["host"] or (cargo["release"], cargo["commit_hash"]) != (release_record["cargo"]["release"], release_record["cargo"]["commit_hash"]):
            raise ValueError("Normalization Cargo identity differs from the actual release Cargo")
        # Host-independent compiler identity allows legitimate OS-specific verbose fields only.
        # 与 host 无关的编译器身份仅允许合法操作系统相关 verbose 字段。
        release_compiler = compiler_identity(release_record["build"]["rustc"].encode())
        if (compiler["release"], compiler["commit_hash"], compiler["host"]) != (release_compiler["release"], release_compiler["commit_hash"], release_compiler["host"]):
            raise ValueError("Normalization compiler identity differs from the actual release compiler")
        # The package minimum comes directly from the frozen source manifest.
        # 包最低版本直接来自冻结源码清单。
        minimum = candidate.tomllib.loads((project / "Cargo.toml").read_text(encoding="utf-8"))["package"]["rust-version"]
        if not isinstance(minimum, str) or not re.fullmatch(r"\d+\.\d+(?:\.\d+)?", minimum) or tuple(map(int, compiler["release"].split("."))) < tuple(map(int, minimum.split("."))) + ((0,) if minimum.count(".") == 1 else ()):
            raise ValueError("Actual compiler is older than the frozen source rust-version")
        execute(["cargo", "package", "--locked", "--no-verify", "-j", "4"], "normalization-package")
        # Official Cargo produced this exact package path; candidate path scans are forbidden.
        # 官方 Cargo 生成此精确包路径；禁止候选路径扫描。
        package = Path(env["CARGO_TARGET_DIR"]) / "package" / f"luaskills-{version}.crate"
        normalized = rooted_archive(package.read_bytes(), f"luaskills-{version}")["Cargo.toml"]
        # All actual registry compile semantics must match Cargo's own frozen-source normalization.
        # 所有实际 registry 编译语义必须匹配 Cargo 自身对冻结源码的规范化。
        registry_manifest = rooted_archive((directory / f"luaskills-{version}.crate").read_bytes(), f"luaskills-{version}")["Cargo.toml"]
        if candidate.tomllib.loads(normalized.decode()) != candidate.tomllib.loads(registry_manifest.decode()):
            raise ValueError("Registry normalized Cargo manifest changes frozen compilation semantics")
        candidate.write_new(directory / "official-normalized-Cargo.toml", normalized)
        return {"commands": commands, "cargo": cargo, "rustc": compiler, "rust_version_minimum": minimum, "normalized_manifest_sha256": candidate.digest(normalized), "registry_manifest_sha256": candidate.digest(registry_manifest)}


def consumer_source():
    """Return Rust runtime/pool/capability consumer source based on current public API inspection.
    基于当前公共 API 源码检索返回 Rust runtime、pool 及 capability 消费程序源码。
    """
    return r'''use luaskills::{LuaEngine, LuaEngineOptions, LuaVmPoolConfig, LuaRuntimeHostOptions, LuaInvocationContext};
use luaskills::runtime::embedded::*;
use luaskills::runtime::embedded::capabilities::*;
use serde_json::json;
use std::{collections::BTreeSet, sync::{Arc, atomic::{AtomicUsize, Ordering}}, time::{Duration, Instant}, fs};

/// Execute real registry runtime, pool reuse, native capability and cleanup checks; return any failure.
/// 执行真实 registry runtime、池复用、原生能力及清理检查；返回任何失败。
fn main() -> Result<(), Box<dyn std::error::Error>> {
    // The challenge binds this process output to this invocation rather than a pasted success log.
    // 随机挑战将此进程输出绑定到本次调用，避免粘贴成功日志。
    let challenge = std::env::args().nth(1).ok_or("missing challenge")?;
    // A new isolated runtime root is owned by this consumption attempt.
    // 新隔离运行时根由本次消费尝试拥有。
    let root = std::env::current_dir()?.join("runtime-root");
    // Canonical system trust root follows the public engine's exact package ownership contract.
    // 规范系统信任根遵循公共引擎精确包归属契约。
    let trust = root.join("system_lua_lib");
    // Package belongs only to the explicitly registered probe plugin.
    // 包仅属于显式注册的探针插件。
    let package = trust.join("registry-probe");
    fs::create_dir_all(&package)?;
    fs::write(package.join("dependencies.yaml"), "{}\n")?;
    // Host options explicitly select trust and runtime roots without external services.
    // 宿主选项显式选择信任及运行时根，不使用外部服务。
    let host = LuaRuntimeHostOptions {runtime_root: Some(fs::canonicalize(&root)?), system_lua_lib_dir: Some(fs::canonicalize(&trust)?), ..Default::default()};
    // Engine actually owns and executes LuaJIT VMs from the downloaded registry package.
    // 引擎实际拥有并执行从 registry 下载包构建的 LuaJIT VM。
    let engine = Arc::new(LuaEngine::new(LuaEngineOptions::new(LuaVmPoolConfig {min_size: 1, max_size: 1, idle_ttl_secs: 30}, host))?);
    // Explicit finite budgets keep the consumer independent from evolving runtime defaults.
    // 显式有限预算使消费者独立于演进的运行时默认值。
    let config: EmbeddedRuntimeConfig = serde_json::from_value(json!({"max_registered_plugins":1,"max_registered_pools":1,"max_sessions":1,"max_registered_capabilities":1,"max_resident_vms":1,"max_running_calls":1,"max_queued_calls":1,"max_queued_bytes":4096,"max_operations":4,"max_effect_records_per_operation":4,"max_effect_bytes_per_operation":8192,"max_host_requests":1,"max_host_request_bytes":4096,"max_value_bytes":1024}))?;
    // Runtime scheduler and pool admission are the formal API consumed by downstream hosts.
    // Runtime 调度器及池入场是下游宿主消费的正式 API。
    let runtime = EmbeddedRuntime::new(engine, config.clone())?;
    runtime.register_plugin("registry-probe".into(), EmbeddedPluginConfig {max_registered_pools:config.max_registered_pools,max_sessions:config.max_sessions,max_resident_vms:config.max_resident_vms,max_running_calls:config.max_running_calls,max_queued_calls:config.max_queued_calls,max_queued_bytes:config.max_queued_bytes,max_operations:config.max_operations})?;
    // Actual callback count proves that both Lua invocations traverse the native capability boundary.
    // 实际回调计数证明两个 Lua 调用均经过原生能力边界。
    let count = Arc::new(AtomicUsize::new(0));
    // Callback counter clone remains owned by the immutable registry registration.
    // 回调计数器克隆由不可变 registry 注册拥有。
    let callback_count = Arc::clone(&count);
    runtime.capabilities().register(vec![CapabilityRegistrationRequest {
        descriptor: CapabilityDescriptor {name:"release.echo".into(),version:"1.0.0".into(),description:"Registry consumer echo".into(),input_schema:json!({"type":"object"}),output_schema:json!({"type":"object"}),execution:CapabilityExecution::Native,permissions:BTreeSet::from(["release.host".into()]),scope:CapabilityScope::Invocation,max_concurrent:1,max_call_ms:5000,max_input_bytes:1024,max_output_bytes:1024,effects:CapabilityEffects::ReadOnly,idempotency:CapabilityIdempotency::None},
        native: Some(Arc::new(move |request| {callback_count.fetch_add(1, Ordering::SeqCst); CapabilityOutcome {result:Ok(request.arguments.clone()),effects:EffectState::NotApplicable}})),
    }])?;
    // One reusable VM preserves its private Lua counter between two scheduled operations.
    // 一个可复用 VM 在两个调度操作间保留私有 Lua 计数器。
    let pool = runtime.register_pool(ModuleDefinition {finalizer:None,plugin_id:"registry-probe".into(),generation:"registry-one".into(),package_root:fs::canonicalize(&package)?.to_string_lossy().replace('\\', "/"),dependencies_file:"dependencies.yaml".into(),workspace_root:None,cwd:None,mounts:json!({}),security_partition:"registry-consumer".into(),source:"local n=0; return {call=function(a) n=n+1; local r=vulcan.host.call('release.echo',a); assert(r.ok); return {count=n,value=r.value} end}".into(),exports:vec![ModuleExport{name:"call".into(),input_schema:json!({"type":"object"}),output_schema:json!({"type":"object"})}]},PluginPoolConfig {kind:PoolKind::Shared,min_resident_vms:0,max_resident_vms:1,max_running_calls:1,max_queued_calls:1,reuse:InstanceReuse::Reusable,serial:true,backend:ExecutionBackend::InProcess,idle_ttl_ms:None,max_uses:None},CapabilityPermissions::new(BTreeSet::from(["release.host".into()]))?,"registry-one".into())?;
    // Two sequential calls must share actual VM state while each crosses the capability boundary.
    // 两个顺序调用必须共享实际 VM 状态，且每次均经过能力边界。
    for expected in 1..=2 {
        // Structured values exercise the actual native JSON conversion boundary.
        // 结构化值验证实际原生 JSON 转换边界。
        let arguments = json!({"challenge":challenge,"object":{},"array":[],"text":"中文\u{0}value"});
        // Handle observes the true scheduled operation and retained completion result.
        // Handle 观察真实调度操作及保留的完成结果。
        let handle = runtime.submit(EmbeddedCall{pool_id:pool.clone(),export:"call".into(),arguments:arguments.clone(),context:LuaInvocationContext::default()},Duration::from_secs(10))?;
        // Snapshot is returned by the public runtime after real Lua and native callback execution.
        // Snapshot 由公共运行时在真实 Lua 及原生回调执行后返回。
        let snapshot = handle.wait(Duration::from_secs(15))?;
        assert_eq!(snapshot.phase, OperationPhase::Succeeded, "{snapshot:?}");
        assert_eq!(snapshot.value, Some(json!({"count":expected,"value":arguments})));
        // Executed Lua retains Unknown overall effects; a read-only host callback cannot prove all Lua effects.
        // 已执行 Lua 的整体副作用保持 Unknown；只读宿主回调无法证明全部 Lua 副作用。
        assert_eq!(snapshot.effects, EffectState::Unknown);
        // Registered operations retain every host invocation, including this read-only capability's exact outcome.
        // 已注册操作保留每次宿主调用，包含此只读能力的精确结果。
        assert_eq!(snapshot.host_effects.len(), 1);
        assert!(snapshot.host_effects.iter().all(|effect| effect.capability_name == "release.echo"
            && effect.phase == HostEffectPhase::Completed && effect.effects == EffectState::NotApplicable));
        runtime.forget_operation(&snapshot.operation_id)?;
    }
    assert_eq!(count.load(Ordering::SeqCst), 2);
    runtime.close_pool(&pool)?;
    runtime.request_close()?;
    // Cleanup deadline verifies actual VM/thread retirement rather than shutdown intent.
    // 清理截止时间验证实际 VM 及线程退役，而非关闭意图。
    let deadline = Instant::now() + Duration::from_secs(15);
    while !runtime.poll_closed()? {assert!(Instant::now() < deadline, "runtime failed to drain"); std::thread::sleep(Duration::from_millis(10));}
    assert_eq!(runtime.resources()?.resident, 0);
    println!("{}", json!({"challenge":challenge,"runtime":true,"pool_reuse":true,"capability_calls":count.load(Ordering::SeqCst),"drained":true}));
    Ok(())
}
'''.encode()


def run_process(command, cwd, env):
    """Execute exact command in cwd with sanitized env and finite timeout; return actual process result.
    在 cwd 中以清理的 env 及有限超时执行精确 command；返回实际进程结果。
    """
    # Process owns a fresh group so timeout cleanup also reclaims compiler/runtime descendants.
    # 进程拥有新组，使超时清理也能回收编译器或运行时后代。
    process = subprocess.Popen(command, cwd=cwd, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, start_new_session=os.name != "nt", creationflags=subprocess.CREATE_NEW_PROCESS_GROUP if os.name == "nt" else 0)
    try:
        # Exact streams are captured without a shell, filtering or accepting external proof files.
        # 精确流不经 shell 或过滤捕获，也不接受外部证明文件。
        stdout, stderr = process.communicate(timeout=PROCESS_TIMEOUT_SECONDS)
        return subprocess.CompletedProcess(command, process.returncode, stdout, stderr)
    except subprocess.TimeoutExpired:
        if os.name == "nt":
            subprocess.run(["taskkill", "/PID", str(process.pid), "/T", "/F"], capture_output=True, check=True, timeout=30)
        else:
            os.killpg(process.pid, signal.SIGKILL)
        process.communicate(timeout=30)
        raise ValueError("Registry consumer command exceeded its bounded process deadline") from None


def consume_registry(directory, version, checksum, runner=run_process, *, toolchain):
    """Build registry version/checksum with verified toolchain using runner; return bounded runtime proof.
    使用 runner 及已验证 toolchain 构建 registry version、checksum；返回有界运行证明。
    """
    # Project is external to the repository so ancestor Cargo config cannot replace registry sources.
    # 工程位于仓库外，避免祖先 Cargo 配置替换 registry 来源。
    with tempfile.TemporaryDirectory(prefix="luaskills-registry-consumer-") as temporary:
        # Independent workspace, Cargo home and target are new for every invocation.
        # 每次调用均使用新独立工作区、Cargo home 及 target。
        project = Path(temporary) / "project"
        cargo_home = Path(temporary) / "cargo-home"
        project.mkdir()
        cargo_home.mkdir()
        # A version equality dependency forbids path/git/patch overrides in generated input.
        # 版本等值依赖禁止生成输入中的 path、git 或 patch 覆盖。
        manifest_bytes = f'[package]\nname = "luaskills-registry-consumer"\nversion = "0.0.0"\nedition = "2024"\n[workspace]\n[dependencies]\nluaskills = "={version}"\nserde_json = "1"\n'.encode()
        candidate.write_new(project / "Cargo.toml", manifest_bytes)
        candidate.write_new(project / "src/main.rs", consumer_source())
        # Cargo source authority is fixed while user secrets, target overrides and rustc wrappers are removed.
        # 固定 Cargo 来源权威，同时移除用户秘密、目标覆盖及 rustc 包装器。
        env = isolated_environment(cargo_home, project / "target", toolchain)
        # Commands record actual process output hashes and exit codes; caller logs are never accepted.
        # 命令记录实际进程输出摘要及退出码；绝不接受调用方日志。
        commands = []

        def execute(arguments, name):
            """Run exact argument list and retain logs under name; return stdout on actual zero exit.
            执行精确参数列表并以 name 保留日志；实际零退出时返回 stdout。
            """
            # Direct argv execution preserves Cargo JSON and requires no CI shell proxy installation.
            # 直接参数数组执行保留 Cargo JSON，无需 CI 安装 shell 代理。
            command = arguments
            result = runner(command, project, env)
            candidate.write_new(directory / f"{name}.stdout", result.stdout)
            candidate.write_new(directory / f"{name}.stderr", result.stderr)
            commands.append({"command": command, "exit_code": result.returncode, "stdout_sha256": candidate.digest(result.stdout), "stderr_sha256": candidate.digest(result.stderr)})
            if result.returncode != 0:
                raise ValueError(f"Registry consumer {name} failed with exit code {result.returncode}")
            return result.stdout

        execute(["cargo", "generate-lockfile"], "lock")
        # Lock is generated from the registry in this fresh project, then held immutable.
        # 锁文件在此新工程中从 registry 生成，随后保持不可变。
        lock_bytes = (project / "Cargo.lock").read_bytes()
        lock = candidate.tomllib.loads(lock_bytes.decode())
        matches = [package for package in lock["package"] if package["name"] == "luaskills"]
        if len(matches) != 1 or matches[0]["version"] != version or matches[0]["source"] != REGISTRY_SOURCE or matches[0]["checksum"] != checksum:
            raise ValueError("Consumer lockfile does not resolve the verified registry package")
        # Cargo metadata independently confirms which package and source were actually resolved.
        # Cargo metadata 独立确认实际解析了哪个包及来源。
        metadata = candidate.decode_json(execute(["cargo", "metadata", "--locked", "--format-version", "1"], "metadata"))
        packages = [package for package in metadata["packages"] if package["name"] == "luaskills"]
        if len(packages) != 1 or packages[0]["version"] != version or packages[0]["source"] != REGISTRY_SOURCE:
            raise ValueError("Actual consumer metadata does not identify the official registry core")
        # Fresh downloaded crate cache is compared byte-for-byte with the already checked API checksum.
        # 新下载 crate 缓存按字节对照已检查的 API 摘要。
        cached = list((cargo_home / "registry/cache").rglob(f"luaskills-{version}.crate"))
        if len(cached) != 1 or candidate.digest(cached[0].read_bytes()) != checksum:
            raise ValueError("Cargo consumed crate differs from verified official registry bytes")
        # Artifact path comes only from this successful compiler-artifact message.
        # 产物路径仅来自本次成功的 compiler-artifact 消息。
        messages = [candidate.decode_json(line) for line in execute(["cargo", "build", "--locked", "-j", "4", "--message-format=json"], "build").splitlines() if line.strip()]
        if not messages or messages[-1] != {"reason": "build-finished", "success": True}:
            raise ValueError("Consumer build lacks a successful Cargo build-finished message")
        artifacts = [Path(message["executable"]) for message in messages if message["reason"] == "compiler-artifact" and Path(message["manifest_path"]).resolve() == project / "Cargo.toml" and message["target"]["name"] == "luaskills-registry-consumer" and message["executable"] is not None]
        if len(artifacts) != 1 or not artifacts[0].resolve().is_relative_to(project / "target") or not artifacts[0].is_file():
            raise ValueError("Consumer build must identify exactly one actual new binary")
        # Challenge makes stale/pasted output fail even if it claims the same check names.
        # 随机挑战使旧或粘贴输出失败，即使声明相同检查名称。
        challenge = os.urandom(32).hex()
        runtime = candidate.decode_json(execute([str(artifacts[0]), challenge], "runtime"))
        if runtime != {"challenge": challenge, "runtime": True, "pool_reuse": True, "capability_calls": 2, "drained": True} or any(runtime[key] is not True for key in ("runtime", "pool_reuse", "drained")) or type(runtime["capability_calls"]) is not int:
            raise ValueError("Actual consumer runtime proof is incomplete or fabricated")
        if (project / "Cargo.lock").read_bytes() != lock_bytes:
            raise ValueError("Locked consumer inputs changed during verification")
        # Reviewable source and binary survive the temporary workspace, with exact content hashes.
        # 可审核源码及二进制保留到临时工作区之外，包含精确内容摘要。
        for name in ("Cargo.toml", "Cargo.lock", "src/main.rs"):
            candidate.write_new(directory / "consumer" / name, (project / name).read_bytes())
        candidate.write_new(directory / "consumer" / artifacts[0].name, artifacts[0].read_bytes())
        return {"commands": commands, "executable": str((directory / "consumer" / artifacts[0].name).resolve()), "executable_sha256": candidate.digest(artifacts[0].read_bytes()), "cargo_lock_sha256": candidate.digest(lock_bytes), "package_id": packages[0]["id"], "source": packages[0]["source"], "result": runtime}


def run_gate(core_tag, core_commit, output, phase="complete", *, http=None, runner=run_process, _expected=None):
    """Verify immutable public core identity into fresh output for phase; return accepted evidence only.
    按 phase 将不可变公共核心身份验证到新 output；仅返回已验收证据。
    """
    full_sha(core_commit)
    if not isinstance(core_tag, str) or not core_tag or phase not in ("complete", "github-only"):
        raise ValueError("Exact core tag and valid prerequisite phase are mandatory")
    # Directory must not exist, including empty directories or symlinks.
    # 目录必须不存在，包含空目录或符号链接。
    output = Path(output).absolute()
    if output.exists() or output.is_symlink():
        raise ValueError("Prerequisite output already exists; evidence cannot be overwritten")
    output.mkdir(parents=True)
    # HTTP injection is only an in-process test boundary; production CLI always creates the real client.
    # HTTP 注入仅是在进程内的测试边界；正式 CLI 始终创建真实客户端。
    http = Http() if http is None else http
    snapshot = github_snapshot(http, core_tag, core_commit)
    if "candidate-manifest.json" not in snapshot["assets"]:
        raise ValueError("Official release candidate-manifest.json is missing")
    # Manifest bytes establish version authority without guessing a version from a tag suffix.
    # 清单字节建立版本权威，不从标签后缀猜测版本。
    downloads = output / "downloads"
    snapshot["assets"]["candidate-manifest.json"]["sha256"] = download_asset(http, snapshot["assets"]["candidate-manifest.json"], downloads / "candidate-manifest.json")
    manifest = candidate.read_json(downloads / "candidate-manifest.json")
    version = manifest["core_version"]
    if type(manifest["schema_version"]) is not int or manifest["schema_version"] != candidate.MANIFEST_VERSION or manifest["source_commit"] != core_commit or not isinstance(version, str) or not re.fullmatch(r"\d+\.\d+\.\d+", version) or core_tag != f"v{version}":
        raise ValueError("Official manifest does not identify the frozen final core version/tag")
    if set(snapshot["assets"]) != expected_names(version, core_commit):
        raise ValueError("Official release has missing or unexpected prerequisite assets")
    for name, asset in snapshot["assets"].items():
        if name != "candidate-manifest.json":
            asset["sha256"] = download_asset(http, asset, downloads / "assets" / name)
    # Historical draft/default workflow evidence is verified through the draft owner's sole validator.
    # 历史草稿或默认工作流证据通过草稿所有者唯一验证器检查。
    snapshot["draft_source_evidence"] = {}
    for name in create_draft.DRAFT_SOURCE_EVIDENCE_ASSETS:
        snapshot["draft_source_evidence"][name] = create_draft.verify_published_source_evidence(candidate.read_json(downloads / "assets" / name), core_commit, REPOSITORY, "https://api.github.com", lookup=http.json)
    # Aggregate owns all nested source/build/description/contract/package-lock consistency checks.
    # Aggregate 拥有全部嵌套源码、构建、描述、契约及包锁文件一致性检查。
    candidate.aggregate(argparse.Namespace(input=downloads / "assets", output=output / "candidate", source_commit=core_commit, version=version))
    if candidate.read_json(output / "candidate/candidate-manifest.json") != manifest:
        raise ValueError("Official aggregate manifest differs from verified platform records")
    # Source checksum sidecar is validated even though aggregate regenerates its local copy.
    # 即使 aggregate 重新生成本地副本，也要验证源码校验和 sidecar。
    source_name = manifest["source_archive"]["name"]
    if (downloads / "assets" / f"{source_name}.sha256").read_bytes() != f"{manifest['source_archive']['sha256']}  {source_name}\n".encode():
        raise ValueError("Official frozen source sidecar mismatch")
    # Dynamic GitHub compression bytes are audit evidence; exact member bytes own source identity.
    # GitHub 动态压缩字节是审计证据；精确成员字节拥有源码身份。
    commit_source = verify_commit_source(http, downloads, core_commit, downloads / "assets" / source_name)
    snapshot["commit_archive_sha256"] = commit_source["archive_sha256"]
    snapshot["commit_source_sha256"] = commit_source["source_sha256"]
    # Shared SDK maps derive exclusively through candidate's verified per-platform extraction.
    # 共享 SDK 映射仅经 candidate 已验证的逐平台提取派生。
    sdk = {}
    for platform in candidate.PLATFORMS:
        candidate.sdk_inputs(argparse.Namespace(input=output / "candidate", output=output / "sdk-inputs" / platform, source_commit=core_commit, version=version, platform=platform))
        sdk[platform] = candidate.read_json(output / "sdk-inputs" / platform / "sdk-validation-inputs.json")
    # Registry remains explicitly absent when only GitHub evidence has been requested.
    # 仅请求 GitHub 证据时，registry 显式缺失。
    registry = None
    if phase == "complete":
        # The actual verified FFI report carries the complete source-file map for registry comparison.
        # 实际已验证 FFI 报告携带完整源码文件映射，供 registry 比较。
        report = candidate.decode_json(candidate.archive_files(output / "candidate" / f"luaskills-ffi-sdk-{next(iter(candidate.PLATFORMS))}.tar.gz")["embedded-build-inputs.json"])
        registry = verify_registry(http, output / "registry", version, core_commit, downloads / "assets" / source_name, next(iter(sdk.values()))["build"]["source_sha256"], report)
        registry["manifest_normalization"] = normalize_registry_manifest(output / "registry", version, downloads / "assets" / source_name, manifest["platforms"], runner)
        registry["consumer"] = consume_registry(output / "registry", version, registry["checksum"], runner, toolchain=registry["manifest_normalization"]["rustc"]["release"])
    # End-of-gate lookup closes metadata/tag drift across download and real consumption time.
    # 门禁结束时查询封闭下载及真实消费期间的元数据或标签漂移。
    current = github_snapshot(http, core_tag, core_commit)
    expected_snapshot = {key: value for key, value in snapshot.items() if key not in ("commit_archive_sha256", "commit_source_sha256", "draft_source_evidence")}
    expected_snapshot["assets"] = {name: {key: value for key, value in asset.items() if key != "sha256"} for name, asset in snapshot["assets"].items()}
    if current != expected_snapshot:
        raise ValueError("Official release, tag or asset identity drifted during prerequisite verification")
    # Actual bytes are fetched again by stable asset ID, not accepted merely from unchanged metadata.
    # 实际字节按稳定资产 ID 再次获取，不仅凭未变元数据接受。
    for asset in snapshot["assets"].values():
        if candidate.digest(http.get(asset["url"], binary=True)[0]) != asset["sha256"]:
            raise ValueError("Official asset bytes drifted during prerequisite verification")
    if phase == "complete":
        # Registry revocation or checksum drift after compilation invalidates the final report.
        # 编译后 registry 撤销或摘要漂移使最终报告无效。
        current_version = http.json(f"https://crates.io/api/v1/crates/luaskills/{version}")["version"]
        if current_version["crate"] != "luaskills" or current_version["num"] != version or current_version["yanked"] is not False or current_version["checksum"] != registry["checksum"]:
            raise ValueError("Registry publication drifted during consumption")
    # Complete is true only after this invocation ran real Cargo and the actual compiled consumer.
    # 仅当本次调用执行真实 Cargo 及实际编译消费者后 complete 才为 true。
    evidence = {"schema_version": candidate.MANIFEST_VERSION, "phase": phase, "complete": phase == "complete", "core_tag": core_tag, "core_commit": core_commit, "core_version": version, "checked_at": datetime.now(timezone.utc).isoformat(), "github": snapshot, "registry": registry, "sdk_inputs": sdk}
    if _expected is not None:
        # Recheck compares immutable prior ownership before creating any accepted fresh report.
        # 重查在创建任何已验收新报告前比较不可变历史归属。
        registry_keys = ("name", "version", "checksum", "vcs_commit", "source_sha256")
        if {key: value for key, value in evidence["github"].items() if key != "commit_archive_sha256"} != {key: value for key, value in _expected["github"].items() if key != "commit_archive_sha256"} or evidence["core_version"] != _expected["core_version"] or {key: evidence["registry"][key] for key in registry_keys} != {key: _expected["registry"][key] for key in registry_keys}:
            raise ValueError("Official core identity changed since the previous prerequisite proof")
    candidate.write_new(output / "prerequisites.json", candidate.encode(evidence))
    return evidence


def recheck(input_path, output, *, http=None, runner=run_process):
    """Revalidate old complete identity via fresh public bytes and actual Cargo; return fresh evidence.
    通过新公共字节及实际 Cargo 重新验证旧完整身份；返回新证据。
    """
    # Old evidence supplies an expected identity only, never a trusted runtime-success assertion.
    # 旧证据仅提供预期身份，绝不是可信运行成功断言。
    previous = candidate.read_json(Path(input_path))
    if previous["schema_version"] != candidate.MANIFEST_VERSION or previous["complete"] is not True or previous["phase"] != "complete" or previous["github"]["repository"] != REPOSITORY:
        raise ValueError("SDK publication recheck requires prior complete prerequisites")
    # Fresh gate always runs all consumption steps and cannot be downgraded by old flags.
    # 新门禁始终执行全部消费步骤，不可由旧标志降级。
    return run_gate(previous["core_tag"], previous["core_commit"], output, "complete", http=http, runner=runner, _expected=previous)


def resolve_sdk_inputs(prerequisites_path, platform):
    """Verify transported artifact's fixed platform inputs; return current-machine absolute paths.
    验证传输产物的固定平台输入；返回当前机器绝对路径。
    """
    if platform not in candidate.PLATFORMS:
        raise ValueError("Unknown core candidate platform")
    # Artifact root is exactly the prerequisite report's parent, never a scanned candidate location.
    # 产物根精确为前置报告父目录，绝不是扫描出的候选位置。
    root = Path(prerequisites_path).resolve().parent
    proof = candidate.read_json(Path(prerequisites_path))
    if proof["schema_version"] != candidate.MANIFEST_VERSION or set(proof["sdk_inputs"]) != set(candidate.PLATFORMS) or proof["github"]["repository"] != REPOSITORY:
        raise ValueError("Transported prerequisites have inconsistent core/platform ownership")
    full_sha(proof["core_commit"])
    # Producer mapping and its per-platform file must match exactly before paths are rebased.
    # 路径重定位前，生产者映射及逐平台文件必须精确匹配。
    directory = root / "sdk-inputs" / platform
    producer = candidate.read_json(directory / "sdk-validation-inputs.json")
    if producer != proof["sdk_inputs"][platform] or producer["source_commit"] != proof["core_commit"] or producer["core_version"] != proof["core_version"] or producer["platform"] != platform or producer["schema_version"] != candidate.MANIFEST_VERSION:
        raise ValueError("Transported SDK producer mapping disagrees with prerequisites")
    # File basis comes solely from candidate's library declaration for this platform.
    # 此平台文件基础仅来自 candidate 的库声明。
    library_name = candidate.PLATFORMS[platform][3][0]
    # Both source OS path forms are explicitly supported by artifact transportation.
    # 产物传输明确支持两种来源操作系统路径形式。
    old_library = PurePosixPath(producer["library"].replace("\\", "/"))
    old_description = PurePosixPath(producer["description"].replace("\\", "/"))
    if not (old_library.is_absolute() or PureWindowsPath(producer["library"]).is_absolute()) or old_library.parts[-3:] != ("sdk-inputs", platform, library_name) or old_description.parent != old_library.parent or old_description.name != "core-description.json":
        raise ValueError("Transported SDK producer paths violate the fixed artifact layout")
    # Authenticated manifest bytes preserve the actual formal release asset boundary.
    # 已认证清单字节保留实际正式发布资产边界。
    official_manifest = root / "downloads/candidate-manifest.json"
    if candidate.digest(official_manifest.read_bytes()) != proof["github"]["assets"]["candidate-manifest.json"]["sha256"]:
        raise ValueError("Transported official candidate manifest checksum mismatch")
    manifest = candidate.read_json(root / "candidate/candidate-manifest.json")
    if manifest != candidate.read_json(official_manifest):
        raise ValueError("Transported candidate differs from official source manifest")
    # Selected archive and shared source bytes must retain their original asset checksums.
    # 所选归档及共享源码字节必须保留原始资产校验和。
    archive_name = f"luaskills-ffi-sdk-{platform}.tar.gz"
    source_name = f"luaskills-source-{proof['core_version']}-{proof['core_commit']}.tar.gz"
    for name in (archive_name, source_name):
        if candidate.digest((root / "candidate" / name).read_bytes()) != proof["github"]["assets"][name]["sha256"]:
            raise ValueError("Transported native/source archive checksum mismatch")
    # Re-derive through the sole candidate function to validate full source/contract/build identity.
    # 经唯一 candidate 函数重新派生，以验证完整源码、契约及构建身份。
    with tempfile.TemporaryDirectory(prefix="luaskills-sdk-inputs-") as temporary:
        candidate.sdk_inputs(argparse.Namespace(input=root / "candidate", output=Path(temporary) / "derived", source_commit=proof["core_commit"], version=proof["core_version"], platform=platform))
        expected = candidate.read_json(Path(temporary) / "derived/sdk-validation-inputs.json")
        if {key: value for key, value in expected.items() if key not in ("library", "description")} != {key: value for key, value in producer.items() if key not in ("library", "description")}:
            raise ValueError("Transported SDK input identity differs from verified native candidate")
        for field, filename in (("library", library_name), ("description", "core-description.json")):
            # Fixed files cannot be replaced with symlinks escaping the transported artifact root.
            # 固定文件不能被逃离传输产物根的符号链接替换。
            path = directory / filename
            if path.is_symlink() or not path.resolve().is_relative_to(root) or path.read_bytes() != Path(expected[field]).read_bytes() or candidate.digest(path.read_bytes()) != producer[f"{field}_sha256"]:
                raise ValueError(f"Transported SDK exact {field} bytes mismatch")
    return {**producer, "library": str((directory / library_name).resolve()), "description": str((directory / "core-description.json").resolve())}


def toolchain_inputs(prerequisites_path, platform, *, http=None):
    """Revalidate GitHub-only artifact and public origin for platform; return non-authorizing toolchain inputs.
    为 platform 重新验证仅 GitHub 产物及公共来源；返回不授权发布的工具链输入。
    """
    # The old report supplies expected identity, never authority to choose an executable or install value.
    # 旧报告仅提供预期身份，绝不是选择可执行文件或安装值的权威。
    proof = candidate.read_json(Path(prerequisites_path))
    if proof["schema_version"] != candidate.MANIFEST_VERSION or proof["phase"] != "github-only" or proof["complete"] is not False or proof["registry"] is not None:
        raise ValueError("Toolchain bootstrap requires non-authorizing GitHub-only prerequisites")
    resolve_sdk_inputs(prerequisites_path, platform)
    # All downloaded public assets must retain their original bytes, including records and historical source proofs.
    # 全部已下载公共资产必须保留原始字节，包含记录及历史源码证明。
    root = Path(prerequisites_path).resolve().parent
    if set(proof["github"]["assets"]) != expected_names(proof["core_version"], proof["core_commit"]):
        raise ValueError("Toolchain bootstrap has an incomplete formal asset proof")
    for name, asset in proof["github"]["assets"].items():
        path = root / "downloads" / (name if name == "candidate-manifest.json" else "assets/" + name)
        if path.is_symlink() or not path.resolve().is_relative_to(root) or candidate.digest(path.read_bytes()) != asset["sha256"]:
            raise ValueError("Toolchain bootstrap downloaded asset checksum mismatch")
    # A fresh full public lookup prevents a fabricated local JSON/hash map from selecting a toolchain.
    # 新完整公共查询阻止伪造本地 JSON 或摘要映射选择工具链。
    with tempfile.TemporaryDirectory(prefix="luaskills-toolchain-inputs-") as temporary:
        current = run_gate(proof["core_tag"], proof["core_commit"], Path(temporary) / "verified", "github-only", http=http)
        if {key: value for key, value in current["github"].items() if key != "commit_archive_sha256"} != {key: value for key, value in proof["github"].items() if key != "commit_archive_sha256"} or current["core_version"] != proof["core_version"]:
            raise ValueError("Toolchain bootstrap formal release identity changed")
        # Fresh platform records are derived from authenticated bytes and candidate's own aggregate validator.
        # 新平台记录源自已认证字节及 candidate 自身汇总验证器。
        manifest = candidate.read_json(Path(temporary) / "verified/candidate/candidate-manifest.json")
        toolchain = release_toolchain(manifest["platforms"])
        records = [record for record in manifest["platforms"] if record["platform"] == platform]
        compiler = compiler_identity(records[0]["build"]["rustc"].encode("utf-8"))
        cargo = records[0]["cargo"]
        if cargo["host"] != compiler["host"]:
            raise ValueError("Toolchain bootstrap Cargo/compiler host mismatch")
    return {"schema_version": candidate.MANIFEST_VERSION, "phase": "github-only", "complete": False, "core_tag": current["core_tag"], "core_commit": current["core_commit"], "core_version": current["core_version"], "platform": platform, "toolchain": toolchain, "rustc": compiler, "cargo": cargo, "github": current["github"]}


def main():
    """Parse exact identity/fresh output or independent recheck; return nonzero for every missing proof.
    解析精确身份、新输出或独立重查；任何缺失证明均返回非零。
    """
    # CLI offers no test-injection, offline, skip-consumer or caller-supplied success-log switch.
    # CLI 不提供测试注入、离线、跳过消费者或调用方成功日志开关。
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--core-tag")
    parser.add_argument("--core-commit")
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--phase", choices=("complete", "github-only"), default="complete")
    parser.add_argument("command", nargs="?", choices=("recheck", "sdk-inputs", "toolchain-inputs"))
    parser.add_argument("--input", type=Path)
    parser.add_argument("--platform", choices=candidate.PLATFORMS)
    # Exact flags are validated as one mutually exclusive CLI contract.
    # 精确标志作为一个互斥 CLI 契约验证。
    args = parser.parse_args()
    try:
        if args.command in ("sdk-inputs", "toolchain-inputs"):
            if args.input is None or args.platform is None or args.core_tag is not None or args.core_commit is not None or args.phase != "complete":
                raise ValueError("Input resolution requires only --input, --platform and --output")
            result = resolve_sdk_inputs(args.input, args.platform) if args.command == "sdk-inputs" else toolchain_inputs(args.input, args.platform)
            candidate.write_new(args.output, candidate.encode(result))
            print(f"{args.command} verified: platform={args.platform} output={args.output}")
            return
        elif args.command == "recheck":
            if args.input is None or args.core_tag is not None or args.core_commit is not None or args.phase != "complete" or args.platform is not None:
                raise ValueError("Recheck requires only --input and --output")
            result = recheck(args.input, args.output)
        else:
            if args.core_tag is None or args.core_commit is None or args.input is not None or args.platform is not None:
                raise ValueError("Gate requires --core-tag, --core-commit and --output")
            result = run_gate(args.core_tag, args.core_commit, args.output, args.phase)
        print(f"Prerequisites verified: phase={result['phase']} complete={result['complete']} output={args.output}")
    except (ValueError, KeyError, TypeError, OSError, subprocess.SubprocessError, tarfile.TarError, zipfile.BadZipFile) as error:
        parser.exit(1, f"SDK prerequisites failed: {error}\n")


if __name__ == "__main__":
    main()
