#!/usr/bin/env python3
"""Freeze read-only default-branch evidence before a draft build; never create refs or releases.
在草稿构建前冻结只读默认分支证据；绝不创建引用或发布。
"""

import argparse
import os
from pathlib import Path
import subprocess

from candidate import encode, write_new
from create_draft import DRAFT_SOURCE_EVIDENCE_ASSETS, draft_source_evidence


def main():
    """Require the actual remote default workflows tree to match source; write one new frozen evidence file.
    要求实际远端默认工作流树匹配源码；写入一个新的冻结证据文件。
    """
    # Parser accepts explicit frozen source and output paths, with no inferred default branch.
    # 解析器接受显式冻结源码及产物路径，不推断默认分支。
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument("--source-commit", required=True)
    parser.add_argument("--output", type=Path, required=True)
    # Arguments are consumed only by this read-only gate and exclusive evidence-file write.
    # 参数仅用于此只读门禁及独占证据文件写入。
    args = parser.parse_args()
    try:
        if args.output.name != DRAFT_SOURCE_EVIDENCE_ASSETS[0]:
            raise ValueError("Draft freeze output name differs from the shared release contract")
        # Evidence comes from authenticated repository/default-ref/commit/tree API responses and actual local Git.
        # 证据来自认证的仓库、默认引用、提交及树 API 响应，以及实际本地 Git。
        evidence = draft_source_evidence(args.root, args.source_commit, os.environ["GITHUB_REPOSITORY"], os.environ["GITHUB_API_URL"], os.environ["GITHUB_TOKEN"])
        write_new(args.output, encode(evidence))
        print(f"Verified draft default branch {evidence['default_branch']} at {evidence['default_commit']}")
    except (ValueError, KeyError, TypeError, OSError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"Draft source freeze failed: {error}\n")


if __name__ == "__main__":
    main()
