"""Offline regressions for the embedded contract series rule.
嵌入式契约系列规则的离线回归测试。
"""

from __future__ import annotations

import json
import unittest

import contract_series


def contract(series: str, marker: str = "a") -> bytes:
    """Return minimal contract bytes declaring `series`; `marker` varies the content.
    返回声明 `series` 的最小契约字节；`marker` 改变内容。
    """
    return json.dumps({"core_series": series, "marker": marker}).encode()


class ContractSeriesTest(unittest.TestCase):
    """The rule: patches keep the series contract, minors may change it.
    规则：补丁保持系列契约，次版本可以改变它。
    """

    def test_first_release_of_a_series_needs_no_predecessor(self) -> None:
        summary = contract_series.check("0.9.0", contract("0.9"), ["v0.8.2"], lambda tag: self.fail(tag))
        self.assertIn("opens series", summary)

    def test_patch_must_keep_the_newest_earlier_contract(self) -> None:
        fetched = []
        published = {"v0.9.0": contract("0.9", "old"), "v0.9.1": contract("0.9")}

        def fetch(tag: str) -> bytes:
            fetched.append(tag)
            return published[tag]

        contract_series.check("0.9.2", contract("0.9"), ["v0.9.0", "v0.9.1", "v0.9.3", "v0.8.9"], fetch)
        self.assertEqual(fetched, ["v0.9.1"])

    def test_patch_that_changes_the_contract_is_rejected(self) -> None:
        with self.assertRaisesRegex(ValueError, "new minor release"):
            contract_series.check("0.9.1", contract("0.9", "changed"), ["v0.9.0"], lambda tag: contract("0.9"))

    def test_patch_without_a_published_predecessor_is_rejected(self) -> None:
        with self.assertRaisesRegex(ValueError, "no published predecessor"):
            contract_series.check("0.9.1", contract("0.9"), ["v0.8.2", "v0.9.1"], lambda tag: contract("0.9"))

    def test_contract_must_declare_the_version_series(self) -> None:
        with self.assertRaisesRegex(ValueError, "core_series 0.9"):
            contract_series.check("0.9.0", contract("0.8"), [], lambda tag: b"")

    def test_inexact_versions_are_rejected(self) -> None:
        for version in ("0.9", "v0.9.0", "0.09.0", "0.9.0-rc.1"):
            with self.subTest(version=version), self.assertRaises(ValueError):
                contract_series.parse_version(version)


if __name__ == "__main__":
    unittest.main()
