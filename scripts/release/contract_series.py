"""Enforce the release-series rule for the embedded contract.
强制执行嵌入式契约的发布系列规则。

A patch release (`x.y.z` with `z > 0`) must ship the byte-identical contract of the newest published
release before it in the same `x.y` series; any contract change requires a new minor series. SDKs then
accept every patch of their series through the unchanged contract digest without regeneration.
补丁版本（`x.y.z` 且 `z > 0`）必须携带同一 `x.y` 系列中此前最新已发布版本逐字节相同的契约；
任何契约变化都必须开启新的次版本系列。SDK 因此可凭不变的契约摘要接受本系列每个补丁，无需重新生成。
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sys
import urllib.request
from pathlib import Path
from typing import Callable, Iterable

CONTRACT = "contracts/embedded/v1/contract.json"


def parse_version(version: str) -> tuple[int, int, int]:
    """Parse one exact `x.y.z` version; anything else raises ValueError.
    解析一个精确 `x.y.z` 版本；其他形式抛出 ValueError。
    """
    match = re.fullmatch(r"(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)", version)
    if match is None:
        raise ValueError(f"Expected an exact x.y.z version, received {version!r}")
    return int(match[1]), int(match[2]), int(match[3])


def previous_release(version: str, tags: Iterable[str]) -> str | None:
    """Return the newest published `v` tag in `version`'s series that precedes it, or None.
    返回 `version` 所在系列中位于其之前的最新已发布 `v` 标签；不存在时返回 None。
    """
    major, minor, patch = parse_version(version)
    earlier = []
    for tag in tags:
        match = re.fullmatch(rf"v{major}\.{minor}\.(0|[1-9]\d*)", tag)
        if match is not None and int(match[1]) < patch:
            earlier.append(int(match[1]))
    return f"v{major}.{minor}.{max(earlier)}" if earlier else None


def check(version: str, contract: bytes, tags: Iterable[str], fetch: Callable[[str], bytes]) -> str:
    """Check `contract` for `version` against published `tags`; return a summary or raise ValueError.
    依据已发布 `tags` 检查 `version` 的 `contract`；返回摘要，失败时抛出 ValueError。

    `fetch(tag)` returns the exact contract bytes published at that tag.
    `fetch(tag)` 返回该标签发布的精确契约字节。
    """
    major, minor, patch = parse_version(version)
    series = f"{major}.{minor}"
    if json.loads(contract).get("core_series") != series:
        raise ValueError(f"The contract does not declare core_series {series}")
    if patch == 0:
        return f"{version} opens series {series}; its contract starts the series"
    previous = previous_release(version, tags)
    if previous is None:
        raise ValueError(f"Patch release {version} has no published predecessor in series {series}")
    if fetch(previous) != contract:
        raise ValueError(
            f"{version} changes the embedded contract published by {previous}; "
            "a contract change needs a new minor release"
        )
    return f"{version} keeps the contract of {previous} byte for byte"


def github_get(url: str, token: str | None, accept: str) -> bytes:
    """Read one GitHub API URL with an optional token; HTTP errors propagate.
    以可选令牌读取一个 GitHub API 地址；HTTP 错误直接抛出。
    """
    headers = {"Accept": accept, "X-GitHub-Api-Version": "2022-11-28", "User-Agent": "luaskills-release"}
    if token:
        headers["Authorization"] = f"Bearer {token}"
    with urllib.request.urlopen(urllib.request.Request(url, headers=headers), timeout=60) as response:
        return response.read()


def published_tags(api: str, repository: str, token: str | None) -> list[str]:
    """Return every published, non-prerelease release tag of `repository`; drafts never count.
    返回 `repository` 的全部已发布非预发布版本标签；草稿不计入。
    """
    tags: list[str] = []
    page = 1
    while True:
        releases = json.loads(github_get(f"{api}/repos/{repository}/releases?per_page=100&page={page}", token, "application/vnd.github+json"))
        if not isinstance(releases, list):
            raise ValueError("Unexpected GitHub release listing")
        tags.extend(item["tag_name"] for item in releases if not item["draft"] and not item["prerelease"])
        if len(releases) < 100:
            return tags
        page += 1


def main() -> int:
    """Run the series check for one version against the public release history.
    针对公开发布历史为单个版本运行系列检查。
    """
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--version", required=True)
    parser.add_argument("--repository", default=os.environ.get("GITHUB_REPOSITORY", "LuaSkills/luaskills"))
    parser.add_argument("--root", type=Path, default=Path.cwd())
    args = parser.parse_args()
    api = os.environ.get("GITHUB_API_URL", "https://api.github.com")
    token = os.environ.get("GH_TOKEN") or os.environ.get("GITHUB_TOKEN")
    contract = (args.root / CONTRACT).read_bytes()
    try:
        summary = check(
            args.version,
            contract,
            published_tags(api, args.repository, token),
            lambda tag: github_get(f"{api}/repos/{args.repository}/contents/{CONTRACT}?ref={tag}", token, "application/vnd.github.raw"),
        )
    except ValueError as error:
        print(error, file=sys.stderr)
        return 1
    print(summary)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
