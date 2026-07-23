"""
Runnable smoke demo that installs one managed LuaSkill and calls one tool through FFI.
通过 FFI 安装一个受管 LuaSkill 并调用一个工具的可运行烟测示例。
"""

import ctypes
import json
import os
import shutil
from pathlib import Path


DEMO_SKILL_ID = "luaskills-demo-skill"
DEMO_SKILL_REPO = "LuaSkills/luaskills-demo-skill"
DEMO_TOOL_NAME = "luaskills-demo-skill-demo-status"


class FfiLuaVmPoolConfig(ctypes.Structure):
    """
    Plain engine-pool config passed into the standard FFI surface.
    传入标准 FFI 接口的原生引擎池配置。
    """

    _fields_ = [
        ("min_size", ctypes.c_size_t),
        ("max_size", ctypes.c_size_t),
        ("idle_ttl_secs", ctypes.c_uint64),
    ]


class FfiLuaRuntimeHostOptions(ctypes.Structure):
    """
    Plain host options passed into the standard FFI surface.
    传入标准 FFI 接口的原生宿主选项。
    """

    _fields_ = [
        ("temp_dir", ctypes.c_char_p),
        ("resources_dir", ctypes.c_char_p),
        ("lua_packages_dir", ctypes.c_char_p),
        ("host_provided_tool_root", ctypes.c_char_p),
        ("host_provided_lua_root", ctypes.c_char_p),
        ("host_provided_ffi_root", ctypes.c_char_p),
        ("download_cache_root", ctypes.c_char_p),
        ("dependency_dir_name", ctypes.c_char_p),
        ("state_dir_name", ctypes.c_char_p),
        ("database_dir_name", ctypes.c_char_p),
        ("skill_config_root", ctypes.c_char_p),
        ("skill_config_lock_timeout_ms", ctypes.c_uint64),
        ("skill_config_watch_debounce_ms", ctypes.c_uint64),
        ("allow_network_download", ctypes.c_uint8),
        ("github_base_url", ctypes.c_char_p),
        ("github_api_base_url", ctypes.c_char_p),
        ("sqlite_library_path", ctypes.c_char_p),
        ("sqlite_provider_mode", ctypes.c_int32),
        ("sqlite_callback_mode", ctypes.c_int32),
        ("lancedb_library_path", ctypes.c_char_p),
        ("lancedb_provider_mode", ctypes.c_int32),
        ("lancedb_callback_mode", ctypes.c_int32),
        ("space_controller_endpoint", ctypes.c_char_p),
        ("space_controller_auto_spawn", ctypes.c_uint8),
        ("space_controller_executable_path", ctypes.c_char_p),
        ("space_controller_process_mode", ctypes.c_int32),
        ("cache_config", ctypes.c_void_p),
        ("runlua_pool_config", ctypes.c_void_p),
        ("reserved_entry_names", ctypes.POINTER(ctypes.c_char_p)),
        ("reserved_entry_names_len", ctypes.c_size_t),
        ("ignored_skill_ids", ctypes.POINTER(ctypes.c_char_p)),
        ("ignored_skill_ids_len", ctypes.c_size_t),
        ("enable_skill_management_bridge", ctypes.c_uint8),
        ("default_text_encoding", ctypes.c_char_p),
        ("disable_managed_io_compat", ctypes.c_uint8),
    ]


class FfiLuaEngineOptions(ctypes.Structure):
    """
    Plain engine options passed into the standard FFI surface.
    传入标准 FFI 接口的原生引擎选项。
    """

    _fields_ = [("pool", FfiLuaVmPoolConfig), ("host", FfiLuaRuntimeHostOptions)]


class FfiBorrowedBuffer(ctypes.Structure):
    """
    Borrowed byte-buffer view passed into `_json` FFI requests.
    传入 `_json` FFI 请求的借用字节缓冲视图。
    """

    _fields_ = [
        ("ptr", ctypes.POINTER(ctypes.c_uint8)),
        ("len", ctypes.c_size_t),
    ]


class FfiOwnedBuffer(ctypes.Structure):
    """
    Owned byte-buffer container returned by `_json` FFI calls.
    由 `_json` FFI 调用返回的拥有型字节缓冲容器。
    """

    _fields_ = [
        ("ptr", ctypes.POINTER(ctypes.c_uint8)),
        ("len", ctypes.c_size_t),
    ]


def runtime_root() -> Path:
    """
    Resolve the shared demo runtime root path.
    解析共享演示运行时根目录路径。
    """

    return Path(__file__).resolve().parent / "runtime_root"


def ensure_runtime_layout(root: Path) -> None:
    """
    Create the required runtime sibling directories when they are missing.
    在缺失时创建必需的运行时兄弟目录。
    """

    for relative_path in [
        "skills",
        "dependencies",
        "state",
        "databases",
        "temp",
        "resources",
        "lua_packages",
        "bin/tools",
        "libs",
    ]:
        (root / relative_path).mkdir(parents=True, exist_ok=True)


def reset_demo_skill_state(root: Path) -> None:
    """
    Remove one previous demo-skill install so the smoke test starts from an empty state.
    删除先前的 demo skill 安装痕迹，使烟测从空状态开始。
    """

    for path in [
        root / "skills" / DEMO_SKILL_ID,
        root / "dependencies" / "tools" / DEMO_SKILL_ID,
        root / "dependencies" / "lua" / DEMO_SKILL_ID,
        root / "dependencies" / "ffi" / DEMO_SKILL_ID,
        root / "databases" / "sqlite" / DEMO_SKILL_ID,
        root / "databases" / "lancedb" / DEMO_SKILL_ID,
    ]:
        if path.exists():
            shutil.rmtree(path, ignore_errors=False)

    for path in [
        root / "state" / "installs" / f"{DEMO_SKILL_ID}.yaml",
        root / "state" / "skills" / "disabled" / f"{DEMO_SKILL_ID}.json",
    ]:
        if path.exists():
            path.unlink()


def load_library() -> ctypes.CDLL:
    """
    Load the luaskills dynamic library from one explicit environment variable.
    从一个显式环境变量加载 luaskills 动态库。
    """

    library_path = os.environ.get("LUASKILLS_LIB")
    if not library_path:
        raise RuntimeError("LUASKILLS_LIB is not set")
    return ctypes.CDLL(str(Path(library_path)))


def read_owned_buffer_text(buffer: FfiOwnedBuffer, library: ctypes.CDLL) -> str:
    """
    Read one owned UTF-8 buffer into one Python string and free it.
    将一个拥有型 UTF-8 缓冲读取为 Python 字符串并释放。
    """

    if not buffer.ptr:
        return ""
    text = ctypes.string_at(buffer.ptr, buffer.len).decode("utf-8")
    library.luaskills_ffi_buffer_free(buffer)
    return text


def must_ok(status: int, error_buffer: FfiOwnedBuffer, library: ctypes.CDLL) -> None:
    """
    Raise one Python exception when the standard FFI call reports failure.
    当标准 FFI 调用报告失败时抛出一个 Python 异常。
    """

    if status == 0:
        return
    message = read_owned_buffer_text(error_buffer, library) or "Unknown FFI error"
    raise RuntimeError(message)


def decode_json_response(raw_buffer: FfiOwnedBuffer, library: ctypes.CDLL) -> dict:
    """
    Decode one JSON envelope returned by one `_json` FFI function.
    解码一个由 `_json` FFI 函数返回的 JSON 包络。
    """

    if not raw_buffer.ptr and raw_buffer.len != 0:
        raise RuntimeError("FFI JSON call returned one null buffer with non-zero len")
    text = (
        ctypes.string_at(raw_buffer.ptr, raw_buffer.len).decode("utf-8")
        if raw_buffer.len
        else ""
    )
    library.luaskills_ffi_buffer_free(raw_buffer)
    payload = json.loads(text)
    if payload.get("ok") is not True:
        raise RuntimeError(payload.get("error") or "Unknown JSON FFI error")
    return payload["result"]


def call_json_ffi(library: ctypes.CDLL, function_name: str, payload: dict) -> dict:
    """
    Call one `_json` FFI function with one JSON payload.
    使用一个 JSON 载荷调用一个 `_json` FFI 函数。
    """

    ffi_function = getattr(library, function_name)
    ffi_function.argtypes = [FfiBorrowedBuffer]
    ffi_function.restype = FfiOwnedBuffer
    input_bytes = json.dumps(payload).encode("utf-8")
    input_array = (ctypes.c_uint8 * len(input_bytes)).from_buffer_copy(input_bytes)
    input_buffer = FfiBorrowedBuffer(
        ptr=ctypes.cast(input_array, ctypes.POINTER(ctypes.c_uint8)),
        len=len(input_bytes),
    )
    return decode_json_response(ffi_function(input_buffer), library)


def normalized_path(path: Path) -> str:
    """
    Convert one path into one normalized POSIX-like string for FFI JSON requests.
    将一个路径转换为供 FFI JSON 请求使用的规范 POSIX 风格字符串。
    """

    return str(path.resolve()).replace("\\", "/")


def build_engine_options(root: Path) -> FfiLuaEngineOptions:
    """
    Build one deterministic engine configuration for the smoke runtime root.
    为烟测运行时根构造一份确定性的引擎配置。
    """

    host = FfiLuaRuntimeHostOptions()
    host.temp_dir = normalized_path(root / "temp").encode("utf-8")
    host.resources_dir = normalized_path(root / "resources").encode("utf-8")
    host.lua_packages_dir = normalized_path(root / "lua_packages").encode("utf-8")
    host.host_provided_tool_root = normalized_path(root / "bin" / "tools").encode("utf-8")
    host.host_provided_lua_root = normalized_path(root / "lua_packages").encode("utf-8")
    host.host_provided_ffi_root = normalized_path(root / "libs").encode("utf-8")
    host.download_cache_root = normalized_path(root / "temp" / "downloads").encode("utf-8")
    host.dependency_dir_name = b"dependencies"
    host.state_dir_name = b"state"
    host.database_dir_name = b"databases"
    host.skill_config_root = None
    host.skill_config_lock_timeout_ms = 0
    host.skill_config_watch_debounce_ms = 0
    host.allow_network_download = 1
    host.github_base_url = None
    host.github_api_base_url = None
    host.sqlite_library_path = None
    host.sqlite_provider_mode = 0
    host.sqlite_callback_mode = 0
    host.lancedb_library_path = None
    host.lancedb_provider_mode = 0
    host.lancedb_callback_mode = 0
    host.space_controller_endpoint = None
    host.space_controller_auto_spawn = 0
    host.space_controller_executable_path = None
    host.space_controller_process_mode = 0
    host.cache_config = None
    host.runlua_pool_config = None
    host.reserved_entry_names = None
    host.reserved_entry_names_len = 0
    host.ignored_skill_ids = None
    host.ignored_skill_ids_len = 0
    host.enable_skill_management_bridge = 0
    host.default_text_encoding = None
    host.disable_managed_io_compat = 0

    return FfiLuaEngineOptions(
        pool=FfiLuaVmPoolConfig(min_size=1, max_size=1, idle_ttl_secs=30),
        host=host,
    )


def main() -> None:
    """
    Run one install-plus-call smoke test against the shared demo runtime root.
    针对共享演示运行时根执行一次安装加调用的烟测。
    """

    root = runtime_root()
    ensure_runtime_layout(root)
    reset_demo_skill_state(root)

    library = load_library()
    library.luaskills_ffi_buffer_free.argtypes = [FfiOwnedBuffer]
    library.luaskills_ffi_buffer_free.restype = None
    library.luaskills_ffi_version.argtypes = [
        ctypes.POINTER(FfiOwnedBuffer),
        ctypes.POINTER(FfiOwnedBuffer),
    ]
    library.luaskills_ffi_engine_new.argtypes = [
        ctypes.POINTER(FfiLuaEngineOptions),
        ctypes.POINTER(ctypes.c_uint64),
        ctypes.POINTER(FfiOwnedBuffer),
    ]
    library.luaskills_ffi_engine_free.argtypes = [
        ctypes.c_uint64,
        ctypes.POINTER(FfiOwnedBuffer),
    ]

    version_buffer = FfiOwnedBuffer()
    error_buffer = FfiOwnedBuffer()
    must_ok(
        library.luaskills_ffi_version(
            ctypes.byref(version_buffer), ctypes.byref(error_buffer)
        ),
        error_buffer,
        library,
    )
    print("FFI version:", read_owned_buffer_text(version_buffer, library))

    engine_id = ctypes.c_uint64()
    options = build_engine_options(root)
    error_buffer = FfiOwnedBuffer()
    must_ok(
        library.luaskills_ffi_engine_new(
            ctypes.byref(options), ctypes.byref(engine_id), ctypes.byref(error_buffer)
        ),
        error_buffer,
        library,
    )

    try:
        roots_payload = {
            "engine_id": engine_id.value,
            "skill_roots": [
                {
                    "name": "ROOT",
                    "skills_dir": normalized_path(root / "skills"),
                }
            ],
        }
        call_json_ffi(library, "luaskills_ffi_load_from_roots_json", roots_payload)

        install_result = call_json_ffi(
            library,
            "luaskills_ffi_system_install_skill_json",
            {
                "engine_id": engine_id.value,
                "authority": "system",
                "skill_roots": roots_payload["skill_roots"],
                "request": {
                    "source": DEMO_SKILL_REPO,
                    "source_type": "github",
                },
            },
        )
        print("Install status:", install_result["status"])

        invocation_result = call_json_ffi(
            library,
            "luaskills_ffi_call_skill_json",
            {
                "engine_id": engine_id.value,
                "tool_name": DEMO_TOOL_NAME,
                "args": {"name": "ffi-demo"},
            },
        )

        payload = json.loads(invocation_result["content"])
        if payload.get("ok") is not True:
            raise RuntimeError("Demo skill returned non-success payload")

        print("Skill call success")
        print(json.dumps(payload, ensure_ascii=False, indent=2))
        print("success")
    finally:
        error_buffer = FfiOwnedBuffer()
        must_ok(
            library.luaskills_ffi_engine_free(
                engine_id, ctypes.byref(error_buffer)
            ),
            error_buffer,
            library,
        )


if __name__ == "__main__":
    main()
