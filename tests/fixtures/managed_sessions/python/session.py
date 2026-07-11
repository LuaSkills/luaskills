"""Long-running JSON-lines Python sidecar used by System Plugin integration tests.
System Plugin 集成测试使用的长期 JSON 行 Python sidecar。
"""

# Standard-library modules used for protocol, process, and fixture-file operations.
# 用于协议、进程与夹具文件操作的标准库模块。
import json
import os
from pathlib import Path
import subprocess
import sys

# Package-relative marker imported from the sibling fixture module.
# 从同级夹具模块导入的包相对标记。
from helper import FIXTURE_MARKER


# Emit one compact JSON protocol record and flush it immediately.
# 输出一条紧凑 JSON 协议记录并立即刷新。
def emit(payload: dict[str, object]) -> None:
    """Write one response record to stdout.
    向 stdout 写入一条响应记录。

    Args:
        payload: JSON-compatible record fields.
        payload：与 JSON 兼容的记录字段。

    Returns:
        None.
        无返回值。
    """

    print(json.dumps(payload, separators=(",", ":")), flush=True)


# Descendant intentionally survives a natural root exit so tree cleanup remains observable.
# 后代进程会有意在根进程自然退出后继续存活，使进程树清理可被观察。
descendant = subprocess.Popen(
    [sys.executable, "-c", "import time; time.sleep(120)"],
    stdin=subprocess.DEVNULL,
    stdout=subprocess.DEVNULL,
    stderr=subprocess.DEVNULL,
)
# Initial record identifies both processes, cwd authorization, and relative import success.
# 初始记录标识两个进程、cwd 授权结果与相对导入成功状态。
started = {
    "event": "started",
    "root_pid": os.getpid(),
    "child_pid": descendant.pid,
    "cwd": os.getcwd(),
    "marker": FIXTURE_MARKER,
    # Environment evidence proves host credentials are absent while controlled values remain.
    # 环境证据用于证明宿主凭据不可见且受控值仍然存在。
    "host_secret_visible": "LUASKILLS_TEST_HOST_SECRET" in os.environ,
    "managed_context_present": "LUASKILLS_MANAGED_CONTEXT_JSON" in os.environ,
    "path": os.environ.get("PATH", ""),
}
# Optional first argument receives the startup record for rollback-cleanup assertions.
# 可选首参数会接收启动记录，供回滚清理断言使用。
pid_file = sys.argv[1] if len(sys.argv) > 1 else None
if pid_file is not None:
    Path(pid_file).write_text(json.dumps(started), encoding="utf-8")
emit(started)
print("stderr-started-python", file=sys.stderr, flush=True)

# Session-local counter proves state continuity and isolation.
# 会话本地计数器用于证明状态延续与隔离。
counter = 0
# Each stdin line is one strict JSON protocol command.
# 每一行 stdin 都是一条严格 JSON 协议命令。
for raw_line in sys.stdin:
    # Parsed command for the current protocol iteration.
    # 当前协议迭代解析得到的命令。
    command = json.loads(raw_line)
    # Stable action discriminator selected by the test.
    # 测试选择的稳定动作判别字段。
    action = command["action"]
    if action == "echo":
        counter += 1
        emit(
            {
                "event": "echo",
                "value": command["value"],
                "counter": counter,
                "marker": FIXTURE_MARKER,
            }
        )
    elif action == "spam":
        # Multiple flushed records exercise bounded buffering and event coalescing.
        # 多条刷新记录用于验证有界缓冲与事件合并。
        for index in range(64):
            emit({"event": "spam", "index": index, "payload": "x" * 64})
        emit({"event": "spam_end", "counter": counter})
    elif action == "exit":
        emit({"event": "exit", "counter": counter})
        break
    else:
        raise ValueError(f"unsupported action: {action}")
