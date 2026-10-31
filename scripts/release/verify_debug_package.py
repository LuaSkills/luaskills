"""Exercise SQLite through launchers extracted from one native debug archive, without downloads.
通过一个原生调试归档中解压的启动器验证 SQLite，不下载依赖。
"""

import argparse
import json
import os
from pathlib import Path
import shutil
import shlex
import sqlite3
import subprocess
import tarfile
import tempfile

from candidate import archive_files


# The fixture uses the public skill manifest and host-owned SQLite protocol.
# 夹具使用公开技能清单与宿主拥有的 SQLite 协议。
MANIFEST = """name: packaged-sqlite
version: 0.1.0
sqlite:
  enable: true
entries:
  - name: sql
    lua_entry: runtime/sql.lua
    lua_module: packaged_sqlite
"""

# Dispatch the supplied action and serialize the real host response without another runtime.
# 分派传入动作并序列化实际宿主响应，不使用其他运行环境。
LUA = """-- Forward the requested SQL action and return its JSON result.
-- 转发请求的 SQL 动作并返回其 JSON 结果。
return function(args)
    return vulcan.json.encode(vulcan.sqlite[args.action](args.input))
end
"""


def powershell_literal(value):
    """Quote value as one PowerShell literal; return text safe for a script expression.
    将 value 引用为一个 PowerShell 字面量；返回可安全用于脚本表达式的文本。
    """
    return "'" + str(value).replace("'", "''") + "'"


def invoke(package, outside, launcher, action, sql_input, *, inline=False, selection="id", expected_error=None):
    """Run launcher with action/sql_input from outside package; return parsed SQLite output.
    从包外使用 launcher 执行 action/sql_input；返回解析后的 SQLite 输出。

    inline chooses JSON argument transport; selection chooses auto-discovery, path or stored id.
    inline 选择 JSON 参数传输方式；selection 选择自动发现、路径或已存储标识。
    expected_error requires a failed call containing that diagnostic, returning None on success.
    expected_error 要求调用失败且包含该诊断，验证成功后返回 None。
    """
    # Keep non-ASCII text, quotes and backslashes intact across the actual launcher boundary.
    # 在实际启动器边界保持非 ASCII 文本、引号与反斜杠完整。
    payload = json.dumps({"action": action, "input": sql_input}, ensure_ascii=False)
    # File transport is also checked independently of inline JSON forwarding.
    # 独立于内联 JSON 转发检查文件传输。
    arguments_file = outside / "SQL 参数.json"
    arguments_file.write_text(payload, encoding="utf-8")
    # The Windows script protocol uses named parameters; Bash uses long CLI options.
    # Windows 脚本协议使用命名参数；Bash 使用长命令行选项。
    windows_parameters = launcher[0] in ("powershell", "batch")
    # Each invocation starts a new debugger and must reopen the same package-owned database.
    # 每次调用启动全新调试器，必须重新打开同一个包拥有的数据库。
    arguments = ["-Command", "call"] if windows_parameters else ["call"]
    if selection == "id":
        arguments += ["-SkillId" if windows_parameters else "--skill-id", "packaged-sqlite"]
    elif selection == "path":
        arguments += ["-SkillPath" if windows_parameters else "--skill-path", str(package / "skills/packaged-sqlite")]
    elif selection != "auto":
        raise ValueError(f"Unknown skill selection: {selection}")
    arguments += ["-Tool" if windows_parameters else "--tool", "sql", "-Output" if windows_parameters else "--output", "content"]
    arguments += ["-ArgsJson" if windows_parameters else "--args-json", payload] if inline else ["-ArgsFile" if windows_parameters else "--args-file", str(arguments_file)]
    # Quote PowerShell expressions explicitly; never interpolate SQL into command syntax.
    # 显式引用 PowerShell 表达式；绝不将 SQL 插值为命令语法。
    if launcher[0] == "powershell":
        command = [launcher[1], "-NoProfile", "-ExecutionPolicy", "Bypass", "-Command",
                   "& " + powershell_literal(package / "debug.ps1") + " " + " ".join(arguments[index] + " " + powershell_literal(arguments[index + 1]) for index in range(0, len(arguments), 2))]
    elif launcher[0] == "batch":
        if inline:
            raise ValueError("Batch verification uses the documented ArgsFile transport")
        command = [str(package / "debug.bat"), *arguments]
    else:
        # Feed Bash a real UTF-8 script, avoiding Python/MSYS native argv conversions before the launcher.
        # 向 Bash 提供真正的 UTF-8 脚本，避免启动器之前 Python/MSYS 的原生参数转换。
        shell_invocation = outside / "invoke.sh"
        shell_invocation.write_text('exec "$BASH" ' + shlex.join([str(package / "debug.sh"), *arguments]) + "\n", encoding="utf-8")
        command = [launcher[1], str(shell_invocation)]
    # Block accidental HTTP bootstrap while keeping shell discovery available.
    # 阻止意外 HTTP 初始化，同时保留 shell 查找能力。
    environment = dict(os.environ, HTTP_PROXY="http://127.0.0.1:9", HTTPS_PROXY="http://127.0.0.1:9", ALL_PROXY="http://127.0.0.1:9", NO_PROXY="")
    # A failing launcher is evidence of a broken archive, including stale binaries without a host.
    # 启动器失败意味着归档损坏，包括缺少宿主的陈旧二进制。
    result = subprocess.run(command, cwd=outside, env=environment, capture_output=True, text=True, encoding="utf-8", errors="replace", timeout=90)
    if expected_error is not None:
        if result.returncode == 0 or expected_error not in result.stdout + result.stderr:
            raise RuntimeError(f"Expected SQLite failure {expected_error!r}: {result.returncode}\n{result.stdout}\n{result.stderr}")
        return None
    if result.returncode:
        raise RuntimeError(f"{launcher[0]} failed ({result.returncode}):\n{result.stdout}\n{result.stderr}")
    # PowerShell may print its optional Lua-package warning before the final content line.
    # PowerShell 可能在最后的内容行之前打印可选 Lua 包警告。
    return json.loads(result.stdout.strip().splitlines()[-1])


def verify(archive_path, platform, bash=None):
    """Verify archive_path on its native platform, optionally using bash; return launch evidence.
    在原生 platform 验证 archive_path，可选使用 bash；返回启动验收证据。
    """
    # Validate member types and paths before extracting executable content.
    # 解压可执行内容之前验证成员类型与路径。
    files = archive_files(archive_path)
    # The manifest must describe the same platform selected by the native CI job.
    # 清单必须描述原生 CI 作业选择的同一个平台。
    manifest = json.loads(files["debug-tool-manifest.json"].decode("utf-8-sig"))
    if manifest["platform"] != platform:
        raise ValueError("Debug package platform does not match verification platform")
    if platform.startswith("windows-") != (os.name == "nt"):
        raise ValueError("Debug package verification requires the native operating system")
    # Only the verifier owns this temporary directory; archives cannot select its cleanup path.
    # 临时目录仅由校验器拥有；归档不能选择其清理路径。
    with tempfile.TemporaryDirectory(prefix="debug-package-") as temporary:
        # Unicode and spaces expose launcher quoting failures hidden by repository-local runs.
        # Unicode 与空格暴露在仓库内运行时被掩盖的启动器引用错误。
        package = Path(temporary) / "调试 package"
        package.mkdir()
        with tarfile.open(archive_path, "r:gz") as archive:
            archive.extractall(package, filter="data")
        # Run from a different working directory so runtime paths must come from the package.
        # 从不同工作目录执行，使运行路径必须来自包本身。
        outside = Path(temporary) / "outside"
        outside.mkdir()
        # Install only this Lua fixture; no setup_runtime, controller or external runtime is used.
        # 只放入此 Lua 夹具；不使用 setup_runtime、控制器或外部运行时。
        skill = package / "skills/packaged-sqlite"
        skill.joinpath("runtime").mkdir(parents=True)
        skill.joinpath("skill.yaml").write_text(MANIFEST, encoding="utf-8")
        skill.joinpath("runtime/sql.lua").write_text(LUA, encoding="utf-8")
        # Windows PowerShell is the shipped batch wrapper's exact interpreter.
        # Windows PowerShell 是随包批处理包装器使用的确切解释器。
        launchers = [("powershell", "powershell.exe"), ("batch", "")] if os.name == "nt" else [("bash", "bash")]
        if os.name == "nt" and shutil.which("pwsh"):
            launchers.append(("powershell", shutil.which("pwsh")))
        if bash is not None:
            launchers.append(("bash", str(bash)))
        invoke(package, outside, launchers[0], "execute_script", {"sql": "CREATE TABLE notes(id INTEGER PRIMARY KEY, note TEXT NOT NULL);"}, selection="auto")
        # The marker exercises Unicode, embedded quotes, backslashes and shell punctuation as data.
        # 标记以数据形式验证 Unicode、内嵌引号、反斜杠及 shell 标点。
        marker = "SQLite 中文 'quote' \"double\" \\ & $value;"
        for index, launcher in enumerate(launchers, 1):
            # Atomic batches must bind the original text and commit before the next process opens it.
            # 原子批次必须绑定原始文本，并在下一个进程打开前提交。
            inserted = invoke(package, outside, launcher, "execute_batch", {"statements": [{"sql": "INSERT INTO notes VALUES (?, ?)", "params": [index, marker]}]}, inline=launcher[0] != "batch", selection="path")
            if inserted["changes"] != 1:
                raise ValueError(f"Unexpected batch response: {inserted}")
            # Query through another process and the stored id rather than re-importing the package.
            # 经另一个进程和已存储标识查询，不重新导入技能包。
            queried = invoke(package, outside, launcher, "query_json", {"sql": "SELECT note FROM notes WHERE id=?", "params": [index]})
            if queried != {"columns": ["note"], "rows": [{"note": marker}], "row_count": 1}:
                raise ValueError(f"SQLite value changed across launcher: {queried}")
            # A later constraint failure must undo the earlier write even after the process exits.
            # 后续约束失败必须撤销之前写入，即使进程已经退出。
            invoke(package, outside, launcher, "execute_batch", {"statements": [{"sql": "INSERT INTO notes VALUES (999, 'rollback')"}, {"sql": "INSERT INTO notes VALUES (1, 'duplicate')"}]}, expected_error="UNIQUE constraint failed")
            # Querying in a fresh process also catches forgotten commits and leaked transactions.
            # 在全新进程查询还能捕获遗忘的提交及泄漏的事务。
            rolled_back = invoke(package, outside, launcher, "query_json", {"sql": "SELECT count(*) AS total FROM notes WHERE id=999"})
            if rolled_back["rows"] != [{"total": 0}]:
                raise ValueError(f"Failed SQLite batch left partial data: {rolled_back}")
        # Independently read committed data after all debugger engines have released their handles.
        # 所有调试器引擎释放句柄后独立读取已提交数据。
        database = package / "runtime/databases/sqlite/packaged-sqlite/packaged-sqlite.sqlite3"
        with sqlite3.connect(database) as connection:
            if connection.execute("SELECT count(*) FROM notes").fetchone()[0] != len(launchers):
                raise ValueError("Persisted SQLite row count does not match launcher runs")
        connection.close()
        if package.joinpath("runtime/resources/luaskills-packages-manifest.json").exists():
            raise ValueError("SQLite smoke unexpectedly installed optional Lua dependencies")
        return {"platform": platform, "launchers": [item[0] + ":" + item[1] for item in launchers], "sqlite_rows": len(launchers), "bootstrap": False}


def main():
    """Parse native archive verification arguments and print evidence; return no value.
    解析原生归档校验参数并打印证据；无返回值。
    """
    # Paths are supplied explicitly by the packaging job, never inferred from unrelated archives.
    # 路径由打包作业显式提供，不从无关归档推断。
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--archive", type=Path, required=True)
    parser.add_argument("--platform", required=True)
    parser.add_argument("--bash", type=Path)
    # The result is local evidence, not a release or signature action.
    # 结果为本地证据，不执行发布或签名操作。
    args = parser.parse_args()
    print(json.dumps(verify(args.archive.resolve(), args.platform, args.bash), ensure_ascii=False))


if __name__ == "__main__":
    main()
