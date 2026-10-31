"""Verify pre-created release tags without allowing mismatched commits or existing releases.
验证预先创建的发布标签，不允许提交不符或覆盖既有发布。
"""

import unittest
import urllib.error
from unittest.mock import patch

import create_draft


class ExistingReleaseTagTests(unittest.TestCase):
    """Exercise read-only destination checks with explicit GitHub ref and tag object fixtures.
    使用显式 GitHub 引用及标签对象夹具验证只读目标检查。
    """

    def setUp(self):
        """Declare one frozen commit and destination; return no value.
        声明一个冻结提交及目标；无返回值。
        """
        # Full immutable identities keep the test independent of any real release version.
        # 完整不可变身份使测试不依赖任何真实发布版本。
        self.commit = "a" * 40
        self.annotated = "b" * 40
        self.base = "https://fixture.invalid"
        self.repository = "Fixture/luaskills"
        self.tag = "v0.0.0-fixture"
        self.reference = {"ref": f"refs/tags/{self.tag}", "object": {"type": "commit", "sha": self.commit}}

    def absent(self):
        """Return the explicit HTTP 404 used for an absent published release; takes no arguments.
        返回用于表示公开发布不存在的显式 HTTP 404；无参数。
        """
        return urllib.error.HTTPError(self.base, 404, "Not Found", {}, None)

    def test_matching_lightweight_tag_allows_new_release_only(self):
        """Permit the matching tag after checking release absence and authenticated drafts; return no value.
        检查发布不存在及已认证草稿后允许匹配标签；无返回值。
        """
        with patch.object(create_draft, "request", side_effect=[self.absent(), self.reference, []]) as request:
            create_draft.require_unused_release_tag(self.base, self.repository, self.tag, "fixture-token", self.commit)
            self.assertEqual(request.call_count, 3)
            self.assertTrue(all(len(call.args) == 2 for call in request.call_args_list))
            self.assertIn("releases?per_page=100&page=1", request.call_args_list[-1].args[0])

    def test_matching_annotated_tag_is_peeled_by_exact_identity(self):
        """Follow one annotated tag object and preserve its expected identity; return no value.
        跟随一个附注标签对象并保持预期身份；无返回值。
        """
        self.reference["object"] = {"type": "tag", "sha": self.annotated}
        # The annotation owns the child commit; its own SHA must match the addressed object.
        # 附注对象拥有子提交；其自身 SHA 必须与请求的对象一致。
        annotation = {"sha": self.annotated, "object": {"type": "commit", "sha": self.commit}}
        with patch.object(create_draft, "request", side_effect=[self.absent(), self.reference, annotation, []]) as request:
            create_draft.require_unused_release_tag(self.base, self.repository, self.tag, "fixture-token", self.commit)
            self.assertTrue(request.call_args_list[2].args[0].endswith(f"git/tags/{self.annotated}"))

    def test_mismatched_commit_and_object_types_are_rejected(self):
        """Reject unrelated commits, non-commit objects and malformed identities before listing drafts.
        在列举草稿之前拒绝无关提交、非提交对象及格式错误的身份。
        """
        for git_object in ({"type": "commit", "sha": "c" * 40}, {"type": "tree", "sha": self.commit}, {"type": "commit", "sha": "short"}):
            with self.subTest(git_object=git_object):
                self.reference["object"] = git_object
                with patch.object(create_draft, "request", side_effect=[self.absent(), self.reference]) as request:
                    with self.assertRaises(ValueError):
                        create_draft.require_unused_release_tag(self.base, self.repository, self.tag, "fixture-token", self.commit)
                    self.assertEqual(request.call_count, 2)

    def test_matching_tag_cannot_overwrite_a_draft(self):
        """Reject an existing draft even though the pre-created tag matches; return no value.
        即使预先创建的标签匹配，也拒绝既有草稿；无返回值。
        """
        with patch.object(create_draft, "request", side_effect=[self.absent(), self.reference, [{"tag_name": self.tag, "draft": True}]]) as request:
            with self.assertRaisesRegex(ValueError, "Release or draft already exists"):
                create_draft.require_unused_release_tag(self.base, self.repository, self.tag, "fixture-token", self.commit)
            self.assertTrue(all(len(call.args) == 2 for call in request.call_args_list))

    def test_default_still_rejects_an_existing_ref(self):
        """Keep old default behavior unless an accepted commit is supplied explicitly; return no value.
        除非显式提供已验收提交，否则保持原有默认行为；无返回值。
        """
        with patch.object(create_draft, "request", side_effect=[self.absent(), self.reference]):
            with self.assertRaisesRegex(ValueError, "already exists"):
                create_draft.require_unused_release_tag(self.base, self.repository, self.tag, "fixture-token")

    def test_annotation_identity_and_cycles_fail_closed(self):
        """Reject mismatched annotation identities and cycles without any write request; return no value.
        拒绝附注身份不符及循环，不执行任何写请求；无返回值。
        """
        self.reference["object"] = {"type": "tag", "sha": self.annotated}
        for annotation in ({"sha": "c" * 40, "object": {"type": "commit", "sha": self.commit}}, {"sha": self.annotated, "object": {"type": "tag", "sha": self.annotated}}):
            with self.subTest(annotation=annotation):
                with patch.object(create_draft, "request", side_effect=[self.reference, annotation]) as request:
                    with self.assertRaises(ValueError):
                        create_draft.require_exact_release_tag(self.base, self.repository, self.tag, self.commit, "fixture-token")
                    self.assertEqual(request.call_count, 2)


if __name__ == "__main__":
    unittest.main()
