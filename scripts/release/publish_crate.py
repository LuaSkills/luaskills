#!/usr/bin/env python3
"""Publish the luaskills crate from a fresh LF clone of the release tag; never from a developer working tree.
从发布标签的全新 LF 克隆发布 luaskills crate；绝不从开发工作树发布。

The SDK prerequisite gate compares every packaged file with the tagged Git blobs byte for byte. A Windows working tree
with core.autocrlf=true packages CRLF bytes that can never pass that gate, and crates.io versions cannot be replaced.
SDK 前置门禁逐字节对照标签 Git blob 检查每个打包文件。core.autocrlf=true 的 Windows 工作树会打包 CRLF 字节，
永远无法通过该门禁，而 crates.io 版本无法替换。
"""

import argparse
import io
import json
from pathlib import Path
import subprocess
import tarfile
import tempfile

# Repository is the only accepted source of release tags.
# Repository 是发布标签唯一认可的来源。
REPOSITORY = "https://github.com/LuaSkills/luaskills.git"
# Cargo rewrites these packaged files; every other packaged file must equal its tagged blob.
# Cargo 会改写这些打包文件；其他每个打包文件都必须等于标签中的 blob。
CARGO_GENERATED = {"Cargo.toml", "Cargo.toml.orig", ".cargo_vcs_info.json"}


def run(*arguments, cwd=None, capture=False):
    """Run one command and fail on a non-zero exit; return stdout bytes when captured.
    运行一条命令，非零退出即失败；捕获时返回标准输出字节。
    """
    result = subprocess.run(arguments, cwd=cwd, check=True, stdout=subprocess.PIPE if capture else None)
    return result.stdout


def tagged_files(clone, commit):
    """Read exact tagged blob bytes through git archive with conversion disabled; return {path: bytes}.
    在禁用转换的前提下通过 git archive 读取标签 blob 的精确字节；返回 {路径: 字节}。
    """
    archive = run("git", "-c", "core.autocrlf=false", "-c", "core.eol=lf", "archive", "--format=tar", commit,
                  cwd=clone, capture=True)
    with tarfile.open(fileobj=io.BytesIO(archive)) as tar:
        return {member.name: tar.extractfile(member).read() for member in tar.getmembers() if member.isfile()}


def package_mismatches(crate, version, commit, source):
    """Compare a .crate archive with tagged source bytes; return human-readable mismatch descriptions.
    对照标签源码字节比较 .crate 归档；返回可读的不一致说明。
    """
    prefix = f"luaskills-{version}/"
    with tarfile.open(fileobj=io.BytesIO(crate), mode="r:gz") as tar:
        files = {member.name[len(prefix):]: tar.extractfile(member).read()
                 for member in tar.getmembers() if member.isfile() and member.name.startswith(prefix)}
    problems = []
    vcs = json.loads(files.get(".cargo_vcs_info.json", b"{}"))
    if vcs.get("git", {}).get("sha1") != commit or vcs["git"].get("dirty", False) or vcs.get("path_in_vcs") != "":
        problems.append(".cargo_vcs_info.json does not name the clean tagged commit at the repository root")
    if files.get("Cargo.toml.orig") != source.get("Cargo.toml"):
        problems.append("Cargo.toml.orig differs from the tagged Cargo.toml")
    problems.extend(f"{name} differs from the tagged blob" for name, body in sorted(files.items())
                    if name not in CARGO_GENERATED and source.get(name) != body)
    return problems


def main():
    """Clone the tag, package and compare, then publish only when asked and every byte matches.
    克隆标签、打包并比较；仅在请求发布且全部字节一致时才发布。
    """
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("version", help="Exact Cargo package version without the v prefix; tag v<version> must be pushed")
    parser.add_argument("--publish", action="store_true", help="Run cargo publish after the byte comparison passes")
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="luaskills-publish-") as temporary:
        clone = Path(temporary) / "luaskills"
        run("git", "-c", "core.autocrlf=false", "clone", "--quiet", "--depth", "1", "--config", "core.autocrlf=false",
            "--config", "core.eol=lf", "--branch", f"v{args.version}", REPOSITORY, str(clone))
        commit = run("git", "rev-parse", "HEAD", cwd=clone, capture=True).decode().strip()
        run("cargo", "package", "--locked", cwd=clone)
        crate = (clone / "target" / "package" / f"luaskills-{args.version}.crate").read_bytes()
        problems = package_mismatches(crate, args.version, commit, tagged_files(clone, commit))
        if problems:
            raise SystemExit("Refusing to publish; the package does not match the tag:\n  " + "\n  ".join(problems))
        print(f"Package luaskills {args.version} matches tagged commit {commit} byte for byte")
        if args.publish:
            run("cargo", "publish", "--locked", cwd=clone)


if __name__ == "__main__":
    main()
