"""Verify callback lifetime boundaries without loading a native library.
验证回调寿命边界，不加载原生库。
"""

import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import MagicMock, patch

import run_python_host_provider_demo as demo


class CallbackLifetimeTests(unittest.TestCase):
    """Exercise successful and failed engine construction using an isolated host fixture.
    使用隔离宿主夹具验证引擎构造成功和失败的路径。
    Test methods take the test instance and return no value.
    测试方法接收测试实例，不返回值。
    """

    def check_registration_lifetime(self, fail_creation: bool) -> None:
        """Run the demo with a fake FFI library; fail_creation selects construction failure.
        使用模拟 FFI 库运行演示；fail_creation 选择构造失败路径。
        Assert publication ends immediately after construction and return no value.
        断言构造结束后立即解除全局发布，不返回值。
        """
        # Native entrypoints retain call order while allowing ctypes signature declarations.
        # 原生入口模拟保留调用顺序，同时允许设置 ctypes 签名声明。
        library = MagicMock()
        library.luaskills_ffi_set_sqlite_provider_json_callback.return_value = 0
        # The selected outcomes correspond to create, load, invoke and free, in that order.
        # 所选结果按顺序对应创建、加载、调用和释放。
        outcomes = RuntimeError("engine creation rejected") if fail_creation else [
            {"engine_id": 17},
            {},
            {"content": json.dumps({"success": True})},
            {},
        ]
        with tempfile.TemporaryDirectory(prefix="luaskills-host-callback-") as directory:
            with (
                patch.object(demo, "resolve_runtime_root", return_value=Path(directory)),
                patch.object(demo, "resolve_luaskills_library", return_value=Path(directory) / "fixture.dll"),
                patch.object(demo.ctypes, "CDLL", return_value=library),
                patch.object(demo, "read_json_envelope", return_value={}),
                patch.object(demo, "must_json_ok", side_effect=outcomes),
                patch("builtins.print"),
            ):
                if fail_creation:
                    with self.assertRaisesRegex(RuntimeError, "engine creation rejected"):
                        demo.main()
                else:
                    demo.main()
        # Preserve the exact callback pointer values recorded by the native setter.
        # 保留原生设置器记录的精确回调指针值。
        registrations = library.luaskills_ffi_set_sqlite_provider_json_callback.call_args_list
        self.assertEqual(len(registrations), 2)
        self.assertTrue(bool(registrations[0].args[0]))
        self.assertFalse(bool(registrations[1].args[0]))
        # Unregistration must precede all loading, invocation and engine teardown calls.
        # 解除注册必须先于所有加载、调用和引擎销毁调用。
        calls = [entry[0] for entry in library.mock_calls]
        self.assertEqual(calls[:3], [
            "luaskills_ffi_set_sqlite_provider_json_callback",
            "luaskills_ffi_engine_new_json",
            "luaskills_ffi_set_sqlite_provider_json_callback",
        ])
        self.assertEqual(library.luaskills_ffi_engine_free_json.call_count, 0 if fail_creation else 1)

    def test_success_clears_global_callback_before_invocation(self) -> None:
        """Successful construction leaves only the engine callback snapshot; return no value.
        成功构造后仅留下引擎回调快照；无返回值。
        """
        self.check_registration_lifetime(False)

    def test_failed_construction_clears_global_callback(self) -> None:
        """Failed construction must not retain the local Python callback; return no value.
        构造失败不得保留局部 Python 回调；无返回值。
        """
        self.check_registration_lifetime(True)


if __name__ == "__main__":
    unittest.main()
