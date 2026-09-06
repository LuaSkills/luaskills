"""
Host-provider SQLite demo that routes LuaSkills database calls through one host callback.
通过一个宿主回调把 LuaSkills 数据库调用路由到宿主侧的 SQLite host-provider 演示。
"""

import ctypes
import json
import os
import sys
from pathlib import Path


FFI_PROVIDER_MODE_DYNAMIC_LIBRARY = 0
FFI_PROVIDER_MODE_HOST_CALLBACK = 1
FFI_CALLBACK_MODE_STANDARD = 0
FFI_CALLBACK_MODE_JSON = 1


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
        # Current V1 layout places the system Lua root before the download cache root.
        # 当前 V1 布局将系统 Lua 根放在下载缓存根之前。
        ("system_lua_lib_dir", ctypes.c_char_p),
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
        # Source-policy fields complete the public V1 host-options layout.
        # 来源策略字段补全公共 V1 宿主选项布局。
        ("official_skill_hub_base_url", ctypes.c_char_p),
        ("enable_private_url_skill_install", ctypes.c_uint8),
        ("private_skill_source_allowlist", ctypes.POINTER(ctypes.c_char_p)),
        ("private_skill_source_allowlist_len", ctypes.c_size_t),
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
    Borrowed byte-buffer view passed into the callback surface.
    传入 callback 接口面的借用字节缓冲视图。
    """

    _fields_ = [
        ("ptr", ctypes.POINTER(ctypes.c_uint8)),
        ("len", ctypes.c_size_t),
    ]


class FfiOwnedBuffer(ctypes.Structure):
    """
    Owned byte-buffer container returned through the callback surface.
    通过 callback 接口面返回的拥有型字节缓冲容器。
    """

    _fields_ = [
        ("ptr", ctypes.POINTER(ctypes.c_uint8)),
        ("len", ctypes.c_size_t),
    ]


CALLBACK_TYPE = ctypes.CFUNCTYPE(
    ctypes.c_int32,
    FfiBorrowedBuffer,
    ctypes.c_void_p,
    ctypes.POINTER(FfiOwnedBuffer),
    ctypes.POINTER(FfiOwnedBuffer),
)


def resolve_demo_root() -> Path:
    """
    Resolve the current host-provider demo root.
    解析当前 host-provider demo 根目录。
    """

    return Path(__file__).resolve().parent


def resolve_runtime_root() -> Path:
    """
    Resolve the dedicated demo runtime root.
    解析专用演示运行时根目录。
    """

    return resolve_demo_root() / "runtime_root"


def make_borrowed_json_buffer(payload: bytes):
    """
    Build one borrowed-buffer view over one JSON payload while keeping backing storage alive.
    在保持底层存储存活的前提下，为单个 JSON 载荷构造借用缓冲视图。
    """

    if payload:
        payload_array = (ctypes.c_uint8 * len(payload)).from_buffer_copy(payload)
        payload_ptr = ctypes.cast(payload_array, ctypes.POINTER(ctypes.c_uint8))
    else:
        payload_array = None
        payload_ptr = None
    return payload_array, FfiBorrowedBuffer(ptr=payload_ptr, len=len(payload))


def clone_bytes_into_owned_buffer(
    library: ctypes.CDLL,
    payload: bytes,
    buffer_out: ctypes.POINTER(FfiOwnedBuffer),
) -> None:
    """
    Clone one Python byte payload into one luaskills-owned FFI buffer.
    将单个 Python 字节载荷克隆到一个 luaskills 自主管理的 FFI 缓冲。
    """

    error_buffer = FfiOwnedBuffer()
    if payload:
        payload_array = (ctypes.c_uint8 * len(payload)).from_buffer_copy(payload)
        payload_ptr = ctypes.cast(payload_array, ctypes.POINTER(ctypes.c_uint8))
    else:
        payload_array = None
        payload_ptr = None
    status = library.luaskills_ffi_buffer_clone(
        payload_ptr,
        len(payload),
        buffer_out,
        ctypes.byref(error_buffer),
    )
    if status == 0:
        return
    message = (
        ctypes.string_at(error_buffer.ptr, error_buffer.len).decode("utf-8")
        if error_buffer.ptr
        else "Unknown buffer clone error"
    )
    if error_buffer.ptr:
        library.luaskills_ffi_buffer_free(error_buffer)
    raise RuntimeError(message)


def ensure_runtime_layout(root: Path) -> None:
    """
    Ensure the dedicated demo runtime layout exists before engine creation.
    在创建引擎前确保专用演示运行时目录结构存在。
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
        "host_managed/sqlite",
    ]:
        (root / relative_path).mkdir(parents=True, exist_ok=True)


def resolve_luaskills_library() -> Path:
    """
    Resolve the luaskills dynamic library from environment or the local target directory.
    从环境变量或本地 target 目录解析 luaskills 动态库。
    """

    explicit = os.environ.get("LUASKILLS_LIB")
    if explicit:
        path = Path(explicit)
        if not path.exists():
            raise RuntimeError(f"LUASKILLS_LIB does not exist: {path}")
        return path

    candidate = resolve_demo_root().parents[2] / "target" / "debug" / "luaskills.dll"
    if candidate.exists():
        return candidate
    raise RuntimeError("Unable to resolve luaskills.dll; set LUASKILLS_LIB first.")


def resolve_vldb_sqlite_library() -> Path:
    """
    Resolve the vldb-sqlite dynamic library from environment, copied demo backends, or local workspace paths.
    从环境变量、已复制的 demo backend 目录或本地工作区路径解析 vldb-sqlite 动态库。
    """

    explicit = os.environ.get("VLDB_SQLITE_LIB")
    if explicit:
        path = Path(explicit)
        if not path.exists():
            raise RuntimeError(f"VLDB_SQLITE_LIB does not exist: {path}")
        return path

    demo_copy = resolve_demo_root() / "backends" / "vldb_sqlite.dll"
    if demo_copy.exists():
        return demo_copy

    candidates = [
        Path(r"D:\projects\VulcanLocalDataGateway\vldb-sqlite\target\debug\vldb_sqlite.dll"),
        Path(r"D:\projects\VulcanLocalDataGateway\vldb-sqlite\target\release\vldb_sqlite.dll"),
    ]
    for candidate in candidates:
        if candidate.exists():
            return candidate

    raise RuntimeError(
        "Unable to resolve vldb_sqlite.dll; set VLDB_SQLITE_LIB or run scripts/copy_local_backends.ps1 first."
    )


def read_json_envelope(library: ctypes.CDLL, raw_buffer: FfiOwnedBuffer) -> dict:
    """
    Read one LuaSkills JSON envelope and free the owned buffer.
    读取一份 LuaSkills JSON 包络并释放拥有型缓冲。
    """

    if not raw_buffer.ptr and raw_buffer.len != 0:
        raise RuntimeError("LuaSkills JSON FFI returned one null buffer with non-zero len")
    text = (
        ctypes.string_at(raw_buffer.ptr, raw_buffer.len).decode("utf-8")
        if raw_buffer.len
        else ""
    )
    library.luaskills_ffi_buffer_free(raw_buffer)
    return json.loads(text)


def must_json_ok(payload: dict) -> dict:
    """
    Raise one Python error when one LuaSkills JSON envelope reports failure.
    当 LuaSkills JSON 包络报告失败时抛出 Python 异常。
    """

    if payload.get("ok"):
        return payload.get("result")
    raise RuntimeError(payload.get("error") or "Unknown LuaSkills JSON FFI error")


class VldbSqliteJsonBridge:
    """
    Thin JSON bridge that forwards host-provider SQLite requests into vldb-sqlite.
    把宿主 provider SQLite 请求转发到 vldb-sqlite 的轻量 JSON 桥接器。
    """

    def __init__(self, library_path: Path, managed_root: Path):
        self._library = ctypes.CDLL(str(library_path))
        self._managed_root = managed_root

        self._library.vldb_sqlite_execute_script_json.argtypes = [ctypes.c_char_p]
        self._library.vldb_sqlite_execute_script_json.restype = ctypes.c_void_p
        self._library.vldb_sqlite_execute_batch_json.argtypes = [ctypes.c_char_p]
        self._library.vldb_sqlite_execute_batch_json.restype = ctypes.c_void_p
        self._library.vldb_sqlite_query_json_json.argtypes = [ctypes.c_char_p]
        self._library.vldb_sqlite_query_json_json.restype = ctypes.c_void_p
        self._library.vldb_sqlite_last_error_message.argtypes = []
        self._library.vldb_sqlite_last_error_message.restype = ctypes.c_char_p
        self._library.vldb_sqlite_clear_last_error.argtypes = []
        self._library.vldb_sqlite_clear_last_error.restype = None
        self._library.vldb_sqlite_string_free.argtypes = [ctypes.c_void_p]
        self._library.vldb_sqlite_string_free.restype = None

    def resolve_database_path(self, binding: dict) -> Path:
        """
        Resolve one host-managed database path from one stable binding tag.
        基于稳定 binding_tag 解析一个宿主管理数据库路径。
        """

        binding_tag = binding["binding_tag"]
        return self._managed_root / f"{binding_tag}.db"

    def _call_json(self, function_name: str, payload: dict) -> dict:
        """
        Invoke one vldb-sqlite JSON entry and decode the JSON response.
        调用一个 vldb-sqlite JSON 入口并解码 JSON 响应。
        """

        function = getattr(self._library, function_name)
        self._library.vldb_sqlite_clear_last_error()
        request_text = json.dumps(payload, ensure_ascii=False).encode("utf-8")
        response_ptr = function(request_text)
        if not response_ptr:
            message = self._library.vldb_sqlite_last_error_message()
            raise RuntimeError(
                message.decode("utf-8") if message else f"{function_name} returned null"
            )
        response_text = ctypes.string_at(response_ptr).decode("utf-8")
        self._library.vldb_sqlite_string_free(response_ptr)
        return json.loads(response_text)

    def handle_request_json(self, request_json: str) -> str:
        """
        Handle one LuaSkills SQLite JSON provider request and return one JSON response string.
        处理一份 LuaSkills SQLite JSON provider 请求并返回 JSON 响应字符串。
        """

        request = json.loads(request_json)
        action = request["action"]
        binding = request["binding"]
        payload = dict(request.get("input") or {})
        database_path = self.resolve_database_path(binding)
        database_path.parent.mkdir(parents=True, exist_ok=True)
        payload["db_path"] = database_path.as_posix()

        if action == "execute_script":
            result = self._call_json("vldb_sqlite_execute_script_json", payload)
        elif action == "execute_batch":
            result = self._call_json("vldb_sqlite_execute_batch_json", payload)
        elif action == "query_json":
            result = self._call_json("vldb_sqlite_query_json_json", payload)
        else:
            raise RuntimeError(f"Unsupported sqlite host-provider action in demo: {action}")

        return json.dumps(result, ensure_ascii=False)


def main() -> None:
    """
    Run one host-provider SQLite smoke test against the dedicated demo runtime.
    针对专用演示运行时执行一次 host-provider SQLite 烟测。
    """

    demo_root = resolve_demo_root()
    runtime_root = resolve_runtime_root()
    ensure_runtime_layout(runtime_root)

    luaskills_library_path = resolve_luaskills_library()
    vldb_sqlite_library_path = resolve_vldb_sqlite_library()

    library = ctypes.CDLL(str(luaskills_library_path))
    library.luaskills_ffi_string_clone.argtypes = [ctypes.c_char_p]
    library.luaskills_ffi_string_clone.restype = ctypes.c_void_p
    library.luaskills_ffi_string_free.argtypes = [ctypes.c_void_p]
    library.luaskills_ffi_string_free.restype = None
    library.luaskills_ffi_buffer_clone.argtypes = [
        ctypes.POINTER(ctypes.c_uint8),
        ctypes.c_size_t,
        ctypes.POINTER(FfiOwnedBuffer),
        ctypes.POINTER(FfiOwnedBuffer),
    ]
    library.luaskills_ffi_buffer_clone.restype = ctypes.c_int32
    library.luaskills_ffi_buffer_free.argtypes = [FfiOwnedBuffer]
    library.luaskills_ffi_buffer_free.restype = None
    library.luaskills_ffi_set_sqlite_provider_json_callback.argtypes = [
        CALLBACK_TYPE,
        ctypes.c_void_p,
        ctypes.POINTER(FfiOwnedBuffer),
    ]
    library.luaskills_ffi_set_sqlite_provider_json_callback.restype = ctypes.c_int32
    library.luaskills_ffi_engine_new_json.argtypes = [FfiBorrowedBuffer]
    library.luaskills_ffi_engine_new_json.restype = FfiOwnedBuffer
    library.luaskills_ffi_load_from_roots_json.argtypes = [FfiBorrowedBuffer]
    library.luaskills_ffi_load_from_roots_json.restype = FfiOwnedBuffer
    library.luaskills_ffi_call_skill_json.argtypes = [FfiBorrowedBuffer]
    library.luaskills_ffi_call_skill_json.restype = FfiOwnedBuffer
    library.luaskills_ffi_engine_free_json.argtypes = [FfiBorrowedBuffer]
    library.luaskills_ffi_engine_free_json.restype = FfiOwnedBuffer

    bridge = VldbSqliteJsonBridge(
        vldb_sqlite_library_path,
        runtime_root / "host_managed" / "sqlite",
    )

    @CALLBACK_TYPE
    def sqlite_callback(request_json, _user_data, response_out, error_out):
        try:
            request_bytes = (
                ctypes.string_at(request_json.ptr, request_json.len)
                if request_json.len
                else b""
            )
            request_json_text = request_bytes.decode("utf-8")
            response_json = bridge.handle_request_json(request_json_text).encode("utf-8")
            clone_bytes_into_owned_buffer(library, response_json, response_out)
            return 0
        except Exception as error:
            print(f"sqlite_callback failed: {error}", file=sys.stderr)
            try:
                clone_bytes_into_owned_buffer(
                    library,
                    str(error).encode("utf-8"),
                    error_out,
                )
            except Exception as clone_error:
                print(
                    f"sqlite_callback failed to clone error payload: {clone_error}",
                    file=sys.stderr,
                )
            return 1

    error_buffer = FfiOwnedBuffer()
    status = library.luaskills_ffi_set_sqlite_provider_json_callback(
        sqlite_callback,
        None,
        ctypes.byref(error_buffer),
    )
    if status != 0:
        message = (
            ctypes.string_at(error_buffer.ptr, error_buffer.len).decode("utf-8")
            if error_buffer.ptr
            else "Unknown callback registration error"
        )
        if error_buffer.ptr:
            library.luaskills_ffi_buffer_free(error_buffer)
        raise RuntimeError(message)

    host = FfiLuaRuntimeHostOptions()
    host.temp_dir = str((runtime_root / "temp").resolve()).replace("\\", "/").encode("utf-8")
    host.resources_dir = str((runtime_root / "resources").resolve()).replace("\\", "/").encode("utf-8")
    host.lua_packages_dir = str((runtime_root / "lua_packages").resolve()).replace("\\", "/").encode("utf-8")
    host.host_provided_tool_root = str((runtime_root / "bin" / "tools").resolve()).replace("\\", "/").encode("utf-8")
    host.host_provided_lua_root = str((runtime_root / "lua_packages").resolve()).replace("\\", "/").encode("utf-8")
    host.host_provided_ffi_root = str((runtime_root / "libs").resolve()).replace("\\", "/").encode("utf-8")
    # System Lua root occupies its exact public V1 slot for accurate ctypes layout.
    # 系统 Lua 根占据公共 V1 的精确槽位，以保持 ctypes 布局准确。
    host.system_lua_lib_dir = str((runtime_root / "system_lua_lib").resolve()).replace("\\", "/").encode("utf-8")
    host.download_cache_root = str((runtime_root / "temp" / "downloads").resolve()).replace("\\", "/").encode("utf-8")
    host.dependency_dir_name = b"dependencies"
    host.state_dir_name = b"state"
    host.database_dir_name = b"databases"
    host.skill_config_root = None
    host.skill_config_lock_timeout_ms = 0
    host.skill_config_watch_debounce_ms = 0
    host.allow_network_download = 0
    host.github_base_url = None
    host.github_api_base_url = None
    # Source-policy defaults keep private URL installation disabled for this provider demo.
    # 来源策略默认值使本 Provider 示例保持禁用私有 URL 安装。
    host.official_skill_hub_base_url = None
    host.enable_private_url_skill_install = 0
    host.private_skill_source_allowlist = None
    host.private_skill_source_allowlist_len = 0
    host.sqlite_library_path = None
    host.sqlite_provider_mode = FFI_PROVIDER_MODE_HOST_CALLBACK
    host.sqlite_callback_mode = FFI_CALLBACK_MODE_JSON
    host.lancedb_library_path = None
    host.lancedb_provider_mode = FFI_PROVIDER_MODE_DYNAMIC_LIBRARY
    host.lancedb_callback_mode = FFI_CALLBACK_MODE_STANDARD
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

    engine_request = {
        "options": {
            "pool_config": {
                "min_size": 1,
                "max_size": 1,
                "idle_ttl_secs": 30,
            },
            "host_options": {
                "temp_dir": host.temp_dir.decode("utf-8"),
                "resources_dir": host.resources_dir.decode("utf-8"),
                "lua_packages_dir": host.lua_packages_dir.decode("utf-8"),
                "host_provided_tool_root": host.host_provided_tool_root.decode("utf-8"),
                "host_provided_lua_root": host.host_provided_lua_root.decode("utf-8"),
                "host_provided_ffi_root": host.host_provided_ffi_root.decode("utf-8"),
                "download_cache_root": host.download_cache_root.decode("utf-8"),
                "dependency_dir_name": "dependencies",
                "state_dir_name": "state",
                "database_dir_name": "databases",
                "skill_config_root": None,
                "skill_config_lock_timeout_ms": None,
                "skill_config_watch_debounce_ms": None,
                "allow_network_download": False,
                "github_base_url": None,
                "github_api_base_url": None,
                "default_text_encoding": None,
                "sqlite_library_path": None,
                "sqlite_provider_mode": "host_callback",
                "sqlite_callback_mode": "json",
                "lancedb_library_path": None,
                "lancedb_provider_mode": "dynamic_library",
                "lancedb_callback_mode": "standard",
                "cache_config": None,
                "reserved_entry_names": [],
                "ignored_skill_ids": [],
                "capabilities": {
                    "enable_skill_management_bridge": False,
                    "enable_managed_io_compat": True,
                },
            },
        }
    }

    engine_request_bytes = json.dumps(engine_request).encode("utf-8")
    engine_request_storage, engine_request_buffer = make_borrowed_json_buffer(
        engine_request_bytes
    )
    engine_payload = must_json_ok(
        read_json_envelope(
            library,
            library.luaskills_ffi_engine_new_json(engine_request_buffer),
        )
    )
    _ = engine_request_storage
    engine_id = engine_payload["engine_id"]

    try:
        load_request = {
            "engine_id": engine_id,
            "skill_roots": [
                {
                    "name": "ROOT",
                    "skills_dir": str((runtime_root / "skills").resolve()).replace("\\", "/"),
                }
            ],
        }
        load_request_bytes = json.dumps(load_request).encode("utf-8")
        load_request_storage, load_request_buffer = make_borrowed_json_buffer(
            load_request_bytes
        )
        must_json_ok(
            read_json_envelope(
                library,
                library.luaskills_ffi_load_from_roots_json(load_request_buffer),
            )
        )
        _ = load_request_storage

        call_request = {
            "engine_id": engine_id,
            "tool_name": "host-provider-sqlite-demo-sqlite-smoke",
            "args": {
                "note": "host provider sqlite smoke from python demo",
            },
        }
        call_request_bytes = json.dumps(call_request).encode("utf-8")
        call_request_storage, call_request_buffer = make_borrowed_json_buffer(
            call_request_bytes
        )
        invocation = must_json_ok(
            read_json_envelope(
                library,
                library.luaskills_ffi_call_skill_json(call_request_buffer),
            )
        )
        _ = call_request_storage
        content = invocation["content"]
        payload = json.loads(content)
        if not payload.get("success"):
            raise RuntimeError(f"Host-provider demo skill returned unexpected payload: {payload}")

        binding_tag = "ROOT-host-provider-sqlite-demo"
        expected_db = runtime_root / "host_managed" / "sqlite" / f"{binding_tag}.db"
        print("Host-provider demo succeeded")
        print("SQLite backend:", vldb_sqlite_library_path)
        print("Managed database:", expected_db)
        print("Skill payload:", json.dumps(payload, ensure_ascii=False))
    finally:
        free_request_bytes = json.dumps({"engine_id": engine_id}).encode("utf-8")
        free_request_storage, free_request_buffer = make_borrowed_json_buffer(
            free_request_bytes
        )
        must_json_ok(
            read_json_envelope(
                library,
                library.luaskills_ffi_engine_free_json(free_request_buffer),
            )
        )
        _ = free_request_storage


if __name__ == "__main__":
    main()
