"""Check generated launcher protocols with real packagers and isolated executable fixtures.
使用真实打包器与隔离可执行夹具检查生成启动器的协议。
"""

import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import tarfile
import tempfile
import unittest

from candidate import archive_files


class DebugPackageTests(unittest.TestCase):
    """Own disposable build trees and test launcher arguments, without claiming native SQLite coverage.
    拥有可丢弃构建树并测试启动器参数，不将此宣称为原生 SQLite 覆盖。
    """

    def setUp(self):
        """Create and populate an isolated packager checkout; return no value.
        创建并填充隔离打包器检出；无返回值。
        """
        # Cleanup is restricted to the directory created by TemporaryDirectory.
        # 清理仅限 TemporaryDirectory 创建的目录。
        self.temporary = tempfile.TemporaryDirectory(prefix="debug-packager-test-")
        self.addCleanup(self.temporary.cleanup)
        # Root contains only copied build inputs and intentionally fake executables.
        # 根目录仅包含复制的构建输入及有意构造的假可执行文件。
        self.root = Path(self.temporary.name) / "checkout"
        self.root.mkdir()
        # Repository supplies the actual current scripts, including uncommitted fixes.
        # 仓库提供实际当前脚本，包括尚未提交的修复。
        repository = Path(__file__).resolve().parents[2]
        for relative in ["Cargo.toml", "LICENSE", "scripts/build/package_debug_tool.ps1", "scripts/build/package_debug_tool.sh", "scripts/build/project_root.ps1", "scripts/build/archive_helpers.ps1", "scripts/deps/fetch_deps.ps1", "scripts/deps/fetch_deps.sh"]:
            # Keep the documented source layout expected by the packager's root resolver.
            # 保持打包器根解析器预期的文档化源码布局。
            destination = self.root / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(repository / relative, destination)
        self.root.joinpath("target/release").mkdir(parents=True)
        # Windows uses Git Bash explicitly; WSL Bash cannot execute this fixture's Windows paths.
        # Windows 显式使用 Git Bash；WSL Bash 不能执行此夹具的 Windows 路径。
        self.bash = "C:/Program Files/Git/bin/bash.exe" if os.name == "nt" else shutil.which("bash")
        if not self.bash or not Path(self.bash).is_file():
            self.skipTest("Native Bash is required for actual packager execution")
        # PowerShell 5 is intentionally selected on Windows to catch BOM/encoding regressions.
        # Windows 有意选择 PowerShell 5，以捕获 BOM/编码回归。
        self.powershell = shutil.which("powershell") if os.name == "nt" else shutil.which("pwsh")

    def package(self, packager, platform):
        """Execute packager for platform in the isolated checkout; return its extracted directory.
        在隔离检出中为 platform 执行 packager；返回解包目录。
        """
        # Use a fixture output path so recursive packager cleanup never touches the working repository.
        # 使用夹具输出路径，使打包器递归清理永不触及工作仓库。
        output = self.root / "archives"
        if packager == "powershell":
            if not self.powershell:
                self.skipTest("PowerShell is required for this actual packager")
            command = [self.powershell, "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", str(self.root / "scripts/build/package_debug_tool.ps1"), "-Platform", platform, "-OutputDir", str(output), "-ReleaseTag", "v0.7.3"]
        else:
            command = [self.bash, str(self.root / "scripts/build/package_debug_tool.sh"), platform, "v0.7.3", str(output)]
        # Include captured diagnostics so a packaging failure identifies its actual owner.
        # 附带捕获的诊断，使打包失败能定位其实际归属。
        result = subprocess.run(command, cwd=self.root.parent, capture_output=True, timeout=60)
        self.assertEqual(result.returncode, 0, result.stdout.decode("utf-8", "replace") + result.stderr.decode("utf-8", "replace"))
        # Validate member safety before extraction; fixtures still go through the actual archive writer.
        # 解压前验证成员安全性；夹具仍经过实际归档写入器。
        archive_path = output / f"luaskills-debug-tool-{platform}.tar.gz"
        archive_files(archive_path)
        # Different iterations never share runtime or launcher state.
        # 不同迭代从不共享运行时或启动器状态。
        extracted = self.root / f"extracted-{packager}-{platform}"
        extracted.mkdir()
        with tarfile.open(archive_path, "r:gz") as archive:
            archive.extractall(extracted, filter="data")
        return extracted

    def test_unix_launcher_sync_and_stored_identity_from_both_packagers(self):
        """Run Unix launchers emitted by both packagers; preserve sync, stored ids and quoted JSON.
        运行两种打包器生成的 Unix 启动器；保留 sync、已存储标识和带引号 JSON。
        """
        # This Bash fixture records argv only; it deliberately provides no SQLite implementation.
        # 此 Bash 夹具仅记录 argv；刻意不提供 SQLite 实现。
        binary = self.root / "target/release/luaskills-debug"
        binary.write_text('#!/usr/bin/env bash\nprintf "%s\\0" "$@"\n', encoding="utf-8")
        binary.chmod(0o755)
        for packager in ("bash", "powershell"):
            with self.subTest(packager=packager):
                # Mark the fixture executable for Git Bash even when Windows tar omits Unix mode bits.
                # 即使 Windows tar 省略 Unix 权限位，也为 Git Bash 标记夹具可执行。
                extracted = self.package(packager, "linux-x64")
                subprocess.run([self.bash, "-c", 'chmod +x "$1"', "fixture", str(extracted / "bin/luaskills-debug")], check=True)
                # Explicit source and argument paths include spaces to catch accidental splitting.
                # 显式源码及参数路径包含空格，以捕获意外拆分。
                skill = extracted / "skills/source skill"
                skill.mkdir()
                # Inline payload stays data even when it contains shell punctuation.
                # 内联内容即使包含 shell 标点也保持为数据。
                payload = json.dumps({"sql": "SELECT '中文', \"quoted\"", "params": ["\\ & $value;"]}, ensure_ascii=False)
                for options in (["sync", "--skill-path", str(skill)], ["call", "--skill-id", "saved-skill", "--tool", "sql", "--args-json", payload]):
                    # A script file supplies native shell quoting without an extra Windows argv conversion.
                    # 脚本文件提供原生 shell 引用，不经过额外 Windows argv 转换。
                    runner = self.root / "run-fixture.sh"
                    runner.write_text('exec "$BASH" ' + shlex.join([str(extracted / "debug.sh"), *options]) + "\n", encoding="utf-8")
                    # NUL-delimited output makes argument boundaries observable without JSON escaping.
                    # NUL 分隔输出使参数边界可观察，无需 JSON 转义。
                    result = subprocess.run([self.bash, str(runner)], cwd=self.root.parent, capture_output=True, check=True, timeout=30)
                    arguments = result.stdout.decode("utf-8").rstrip("\0").split("\0")
                    self.assertEqual(arguments[0], options[0])
                    if options[0] == "call":
                        self.assertEqual(arguments[arguments.index("--skill-id") + 1], "saved-skill")
                        self.assertEqual(arguments[arguments.index("--args-json") + 1], payload)
                        self.assertNotIn("--skill-path", arguments)

    @unittest.skipUnless(os.name == "nt", "Windows PowerShell 5 parser is Windows-specific")
    def test_windows_generated_scripts_have_bom_and_parse_in_powershell5(self):
        """Parse every packaged PowerShell script with the shipped interpreter; return no value.
        使用随包指定解释器解析全部 PowerShell 脚本；无返回值。
        """
        self.root.joinpath("target/release/luaskills-debug.exe").write_bytes(b"fixture-only")
        for packager in ("bash", "powershell"):
            with self.subTest(packager=packager):
                # This checks actual archive members, including optional dependency setup scripts.
                # 此处检查实际归档成员，包括可选依赖初始化脚本。
                extracted = self.package(packager, "windows-x64")
                for script in extracted.rglob("*.ps1"):
                    self.assertTrue(script.read_bytes().startswith(b"\xef\xbb\xbf"), script)
                    # Parser errors are returned without executing dependency-download commands.
                    # 不执行依赖下载命令，直接返回解析器错误。
                    expression = "$tokens=$null; $errors=$null; [System.Management.Automation.Language.Parser]::ParseFile('" + str(script).replace("'", "''") + "', [ref]$tokens, [ref]$errors) | Out-Null; if ($errors.Count) { throw ($errors | Out-String) }"
                    subprocess.run([self.powershell, "-NoProfile", "-Command", expression], check=True, capture_output=True, timeout=30)


if __name__ == "__main__":
    unittest.main()
