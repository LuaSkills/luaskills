"""Managed Python invoke handler used by System Plugin integration tests.
System Plugin 集成测试使用的受管 Python invoke 处理器。
"""

# Standard-library environment access used only for boundary evidence.
# 仅用于边界证据的标准库环境访问。
import os

# Package-scoped dependency resolved from the exact managed virtual environment.
# 从精确受管虚拟环境解析的包级依赖。
from managed_session_dependency import DEPENDENCY_MARKER
# Package-relative module resolved from the immutable snapshot import root.
# 从不可变快照导入根解析的包相对模块。
from runtime.helper import FIXTURE_MARKER


# Module-local invocation count used to prove worker module-cache isolation.
# 用于证明 Worker 模块缓存隔离的模块内调用计数。
INVOCATION_COUNT = 0


# Execute one deterministic managed-runtime invocation.
# 执行一次确定性的受管运行时调用。
def main(args: dict[str, object], ctx: dict[str, object]) -> dict[str, object]:
    """Return dependency, module-state, argument, and trusted package evidence.
    返回依赖、模块状态、参数与可信包证据。

    Args:
        args: JSON-compatible invocation arguments.
        args：JSON 兼容的调用参数。
        ctx: Engine-controlled package and System lease context.
        ctx：引擎控制的包与 System 租约上下文。

    Returns:
        JSON-compatible evidence for integration assertions.
        用于集成断言的 JSON 兼容证据。
    """
    # InvocationCount intentionally persists only inside this loaded package module.
    # InvocationCount 有意仅在当前已加载包模块内部持久存在。
    global INVOCATION_COUNT
    INVOCATION_COUNT += 1
    # Optional oversized output proves the worker wrapper retains only its bounded tail.
    # 可选超大输出用于证明 Worker 包装器只保留有界尾部。
    if args.get("spam") is True:
        print("x" * (300 * 1024))
    # PackageContext is mandatory because the worker receives only trusted engine context.
    # PackageContext 是必需项，因为 Worker 只接收引擎提供的可信上下文。
    package_context = ctx["package"]
    print(f"python-invoke:{package_context['id']}:{INVOCATION_COUNT}")
    return {
        "counter": INVOCATION_COUNT,
        "dependency_marker": DEPENDENCY_MARKER,
        "relative_marker": FIXTURE_MARKER,
        "host_secret_visible": "LUASKILLS_TEST_HOST_SECRET" in os.environ,
        "package_id": package_context["id"],
        "path": os.environ.get("PATH", ""),
        "cwd": os.getcwd(),
        "source_path": os.path.abspath(__file__),
        "value": args["value"],
    }
