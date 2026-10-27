"""Check that the crate publisher rejects packages whose bytes differ from the tagged commit.
检查 crate 发布器拒绝字节与标签提交不一致的包。
"""

import io
import json
import tarfile
import unittest

from publish_crate import package_mismatches

COMMIT = "a" * 40
SOURCE = {"Cargo.toml": b'[package]\nname = "luaskills"\n', "src/lib.rs": b"pub fn f() {}\n"}


def crate(files, version="1.2.3"):
    """Build an in-memory .crate archive with Cargo's root prefix; return its gzip bytes.
    构建带 Cargo 根前缀的内存 .crate 归档；返回其 gzip 字节。
    """
    buffer = io.BytesIO()
    with tarfile.open(fileobj=buffer, mode="w:gz") as tar:
        for name, body in files.items():
            info = tarfile.TarInfo(f"luaskills-{version}/{name}")
            info.size = len(body)
            tar.addfile(info, io.BytesIO(body))
    return buffer.getvalue()


def packaged(**overrides):
    """Return the files Cargo would package from SOURCE, with optional per-file replacements.
    返回 Cargo 从 SOURCE 打包出的文件，可按文件替换。
    """
    files = {"Cargo.toml": b"normalized", "Cargo.toml.orig": SOURCE["Cargo.toml"], "src/lib.rs": SOURCE["src/lib.rs"],
             ".cargo_vcs_info.json": json.dumps({"git": {"sha1": COMMIT}, "path_in_vcs": ""}).encode()}
    files.update(overrides)
    return files


class PackageMismatchTests(unittest.TestCase):
    """Exercise the byte comparison that guards cargo publish.
    验证守护 cargo publish 的字节比较。
    """

    def test_exact_package_passes(self):
        self.assertEqual(package_mismatches(crate(packaged()), "1.2.3", COMMIT, SOURCE), [])

    def test_crlf_working_tree_bytes_are_rejected(self):
        crlf = {name: SOURCE[name].replace(b"\n", b"\r\n") for name in SOURCE}
        files = packaged(**{"Cargo.toml.orig": crlf["Cargo.toml"], "src/lib.rs": crlf["src/lib.rs"]})
        problems = package_mismatches(crate(files), "1.2.3", COMMIT, SOURCE)
        self.assertEqual(len(problems), 2)

    def test_untracked_file_and_dirty_or_foreign_commit_are_rejected(self):
        files = packaged(**{"extra.txt": b"x", ".cargo_vcs_info.json": json.dumps(
            {"git": {"sha1": COMMIT, "dirty": True}, "path_in_vcs": ""}).encode()})
        self.assertEqual(len(package_mismatches(crate(files), "1.2.3", COMMIT, SOURCE)), 2)
        self.assertEqual(len(package_mismatches(crate(packaged()), "1.2.3", "b" * 40, SOURCE)), 1)


if __name__ == "__main__":
    unittest.main()
