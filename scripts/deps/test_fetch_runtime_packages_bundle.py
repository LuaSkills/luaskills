"""Deterministic tests for streamed runtime-package bundle staging.
运行时包 bundle 流式暂存的确定性测试。
"""

from __future__ import annotations

import hashlib
import importlib.util
import os
import tempfile
import unittest
from pathlib import Path
from unittest import mock


# Script path loaded as a module without requiring scripts to become a Python package.
# 在不要求 scripts 成为 Python 包的情况下作为模块加载的脚本路径。
SCRIPT_PATH = Path(__file__).with_name("fetch_runtime_packages_bundle.py")
# Import specification for the exact production bundle-fetcher script.
# 精确生产 bundle 获取脚本的导入规范。
MODULE_SPEC = importlib.util.spec_from_file_location(
    "fetch_runtime_packages_bundle",
    SCRIPT_PATH,
)
if MODULE_SPEC is None or MODULE_SPEC.loader is None:
    raise RuntimeError(f"failed to load bundle fetcher module: {SCRIPT_PATH}")
# Loaded production module exercised by every test below.
# 下列每个测试实际执行的生产模块。
BUNDLE_FETCHER = importlib.util.module_from_spec(MODULE_SPEC)
MODULE_SPEC.loader.exec_module(BUNDLE_FETCHER)


class RuntimePackagesBundleFetchTests(unittest.TestCase):
    """Verify bounded streaming, cleanup, and rollback behavior.
    验证有界流式处理、清理与回滚行为。
    """

    def test_streamed_download_writes_verified_payload(self) -> None:
        """Verify a valid multi-block response is written and retained.
        验证有效多块响应会被写入并保留。
        """

        # Payload split by the mocked response to exercise repeated bounded reads.
        # 由模拟响应拆分以覆盖重复有界读取的载荷。
        payload = b"first-block" + b"second-block"
        response = mock.MagicMock()
        response.__enter__.return_value = response
        response.read.side_effect = [b"first-block", b"second-block", b""]
        with tempfile.TemporaryDirectory() as temporary_directory:
            # Unique destination representing the temporary archive used by production staging.
            # 表示生产暂存流程所用临时归档的唯一目标。
            destination = Path(temporary_directory) / "bundle.zip"
            with mock.patch.object(
                BUNDLE_FETCHER.urllib.request,
                "urlopen",
                return_value=response,
            ):
                BUNDLE_FETCHER.download_verified_file(
                    "https://example.invalid/bundle.zip",
                    destination,
                    hashlib.sha256(payload).hexdigest(),
                    "bundle.zip",
                )
            self.assertEqual(destination.read_bytes(), payload)
            self.assertEqual(response.read.call_count, 3)

    def test_hash_mismatch_and_interruption_remove_partial_archive(self) -> None:
        """Verify failed transfers never leave a reusable partial archive.
        验证失败传输绝不会留下可复用的部分归档。
        """

        for side_effect in ([b"wrong", b""], [b"partial", OSError("interrupted")]):
            response = mock.MagicMock()
            response.__enter__.return_value = response
            response.read.side_effect = side_effect
            with tempfile.TemporaryDirectory() as temporary_directory:
                # Per-attempt unique archive path checked after the expected failure.
                # 在预期失败后检查的每次尝试唯一归档路径。
                destination = Path(temporary_directory) / "bundle.zip"
                with mock.patch.object(
                    BUNDLE_FETCHER.urllib.request,
                    "urlopen",
                    return_value=response,
                ):
                    with self.assertRaises((RuntimeError, OSError)):
                        BUNDLE_FETCHER.download_verified_file(
                            "https://example.invalid/bundle.zip",
                            destination,
                            hashlib.sha256(b"expected").hexdigest(),
                            "bundle.zip",
                        )
                self.assertFalse(destination.exists())

    def test_publish_failure_restores_previous_bundle(self) -> None:
        """Verify publication failure restores the complete previous bundle directory.
        验证发布失败会恢复完整旧 bundle 目录。
        """

        with tempfile.TemporaryDirectory() as temporary_directory:
            # Existing final bundle that must remain valid after candidate publication fails.
            # 候选发布失败后必须保持有效的现有最终 bundle。
            root = Path(temporary_directory)
            bundle_root = root / "v1"
            staged_root = root / "staged"
            bundle_root.mkdir()
            staged_root.mkdir()
            (bundle_root / "state.txt").write_text("old", encoding="utf-8")
            (staged_root / "state.txt").write_text("new", encoding="utf-8")
            # Real rename retained so only the staged-to-final publication step is injected to fail.
            # 保留的真实重命名，使注入失败仅发生在暂存到最终目录的发布步骤。
            real_replace = os.replace

            def replace_with_publication_failure(source: Path, destination: Path) -> None:
                """Fail only the candidate publication rename and allow backup rollback.
                仅让候选发布重命名失败，并允许备份回滚。
                """

                if Path(source) == staged_root and Path(destination) == bundle_root:
                    raise OSError("publication failed")
                real_replace(source, destination)

            with mock.patch.object(
                BUNDLE_FETCHER.os,
                "replace",
                side_effect=replace_with_publication_failure,
            ):
                with self.assertRaises(OSError):
                    BUNDLE_FETCHER.publish_staged_bundle(staged_root, bundle_root)
            self.assertEqual(
                (bundle_root / "state.txt").read_text(encoding="utf-8"),
                "old",
            )


if __name__ == "__main__":
    unittest.main()
