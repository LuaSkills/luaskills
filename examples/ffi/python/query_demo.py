"""
Minimal Python ctypes query-helper example for the standard LuaSkills FFI surface.
LuaSkills 标准 FFI 接口的最小 Python ctypes 查询辅助示例。
"""

import ctypes

from demo import (
    FfiLuaEngineOptions,
    FfiLuaRuntimeHostOptions,
    FfiLuaVmPoolConfig,
    FfiOwnedBuffer,
    FfiRuntimeSkillRoot,
    LUASKILLS_SKILL_AUTHORITY_SYSTEM,
    ensure_standard_fixture_layout,
    load_library,
    must_ok,
    read_owned_buffer_text,
    standard_fixture_runtime_root,
)


class FfiStringArray(ctypes.Structure):
    """
    Plain string-array container returned by the standard query-helper API.
    标准查询辅助接口返回的原生字符串数组容器。
    """

    _fields_ = [
        ("items", ctypes.POINTER(FfiOwnedBuffer)),
        ("len", ctypes.c_size_t),
    ]


def read_string_array(values_ptr: ctypes.POINTER(FfiStringArray)) -> list[str]:
    """
    Read one returned string-array container into Python strings.
    将一个返回的字符串数组容器读取为 Python 字符串列表。
    """

    if not values_ptr:
        return []
    values = values_ptr.contents
    return [read_owned_buffer_text(values.items[index]) for index in range(values.len)]


def main() -> None:
    """
    Demonstrate is_skill, skill_name_for_tool, and prompt_argument_completions through the standard ABI.
    演示通过标准 ABI 调用 is_skill、skill_name_for_tool 和 prompt_argument_completions。
    """

    library = load_library()
    library.luaskills_ffi_buffer_free.argtypes = [FfiOwnedBuffer]
    library.luaskills_ffi_buffer_free.restype = None
    library.luaskills_ffi_engine_new.argtypes = [
        ctypes.POINTER(FfiLuaEngineOptions),
        ctypes.POINTER(ctypes.c_uint64),
        ctypes.POINTER(FfiOwnedBuffer),
    ]
    library.luaskills_ffi_load_from_roots.argtypes = [
        ctypes.c_uint64,
        ctypes.POINTER(FfiRuntimeSkillRoot),
        ctypes.c_size_t,
        ctypes.POINTER(FfiOwnedBuffer),
    ]
    library.luaskills_ffi_is_skill.argtypes = [
        ctypes.c_uint64,
        ctypes.c_int32,
        ctypes.c_char_p,
        ctypes.POINTER(ctypes.c_uint8),
        ctypes.POINTER(FfiOwnedBuffer),
    ]
    library.luaskills_ffi_skill_name_for_tool.argtypes = [
        ctypes.c_uint64,
        ctypes.c_int32,
        ctypes.c_char_p,
        ctypes.POINTER(FfiOwnedBuffer),
        ctypes.POINTER(FfiOwnedBuffer),
    ]
    library.luaskills_ffi_prompt_argument_completions.argtypes = [
        ctypes.c_uint64,
        ctypes.c_int32,
        ctypes.c_char_p,
        ctypes.c_char_p,
        ctypes.POINTER(ctypes.POINTER(FfiStringArray)),
        ctypes.POINTER(FfiOwnedBuffer),
    ]
    library.luaskills_ffi_string_array_free.argtypes = [
        ctypes.POINTER(FfiStringArray),
    ]
    library.luaskills_ffi_string_array_free.restype = None
    library.luaskills_ffi_engine_free.argtypes = [
        ctypes.c_uint64,
        ctypes.POINTER(FfiOwnedBuffer),
    ]

    root = standard_fixture_runtime_root()
    ensure_standard_fixture_layout(root)

    host = FfiLuaRuntimeHostOptions()
    host.temp_dir = str((root / "temp").resolve()).replace("\\", "/").encode("utf-8")
    host.resources_dir = str((root / "resources").resolve()).replace("\\", "/").encode("utf-8")
    host.lua_packages_dir = str((root / "lua_packages").resolve()).replace("\\", "/").encode("utf-8")
    host.host_provided_tool_root = str((root / "bin" / "tools").resolve()).replace("\\", "/").encode("utf-8")
    host.host_provided_lua_root = str((root / "lua_packages").resolve()).replace("\\", "/").encode("utf-8")
    host.host_provided_ffi_root = str((root / "libs").resolve()).replace("\\", "/").encode("utf-8")
    host.download_cache_root = str((root / "temp" / "downloads").resolve()).replace("\\", "/").encode("utf-8")
    host.dependency_dir_name = b"dependencies"
    host.state_dir_name = b"state"
    host.database_dir_name = b"databases"
    host.allow_network_download = 0
    host.github_base_url = None
    host.github_api_base_url = None
    host.github_token = None
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

    options = FfiLuaEngineOptions(
        pool=FfiLuaVmPoolConfig(min_size=1, max_size=1, idle_ttl_secs=30),
        host=host,
    )
    engine_id = ctypes.c_uint64()
    error_buffer = FfiOwnedBuffer()
    must_ok(
        library.luaskills_ffi_engine_new(
            ctypes.byref(options),
            ctypes.byref(engine_id),
            ctypes.byref(error_buffer),
        ),
        error_buffer,
        library,
    )
    print("Engine created:", engine_id.value)

    skill_roots = (FfiRuntimeSkillRoot * 1)(
        FfiRuntimeSkillRoot(
            name=b"ROOT",
            skills_dir=str((root / "skills").resolve()).replace("\\", "/").encode("utf-8"),
        )
    )
    error_buffer = FfiOwnedBuffer()
    must_ok(
        library.luaskills_ffi_load_from_roots(
            engine_id.value,
            skill_roots,
            len(skill_roots),
            ctypes.byref(error_buffer),
        ),
        error_buffer,
        library,
    )
    print("Loaded roots from:", root / "skills")

    is_skill_value = ctypes.c_uint8()
    error_buffer = FfiOwnedBuffer()
    must_ok(
        library.luaskills_ffi_is_skill(
            engine_id.value,
            LUASKILLS_SKILL_AUTHORITY_SYSTEM,
            b"demo-standard-ffi-skill-ping",
            ctypes.byref(is_skill_value),
            ctypes.byref(error_buffer),
        ),
        error_buffer,
        library,
    )
    print("Is skill tool:", bool(is_skill_value.value))

    skill_name_buffer = FfiOwnedBuffer()
    error_buffer = FfiOwnedBuffer()
    must_ok(
        library.luaskills_ffi_skill_name_for_tool(
            engine_id.value,
            LUASKILLS_SKILL_AUTHORITY_SYSTEM,
            b"demo-standard-ffi-skill-ping",
            ctypes.byref(skill_name_buffer),
            ctypes.byref(error_buffer),
        ),
        error_buffer,
        library,
    )
    print("Owning skill id:", read_owned_buffer_text(skill_name_buffer))
    library.luaskills_ffi_buffer_free(skill_name_buffer)

    values_ptr = ctypes.POINTER(FfiStringArray)()
    error_buffer = FfiOwnedBuffer()
    must_ok(
        library.luaskills_ffi_prompt_argument_completions(
            engine_id.value,
            LUASKILLS_SKILL_AUTHORITY_SYSTEM,
            b"demo-standard-ffi-skill-ping",
            b"note",
            ctypes.byref(values_ptr),
            ctypes.byref(error_buffer),
        ),
        error_buffer,
        library,
    )
    try:
        values = read_string_array(values_ptr)
        print("Prompt completion count:", len(values))
        print("Prompt completions:", values)
    finally:
        if values_ptr:
            library.luaskills_ffi_string_array_free(values_ptr)

    error_buffer = FfiOwnedBuffer()
    must_ok(
        library.luaskills_ffi_engine_free(
            engine_id,
            ctypes.byref(error_buffer),
        ),
        error_buffer,
        library,
    )
    print("Engine freed")


if __name__ == "__main__":
    main()
