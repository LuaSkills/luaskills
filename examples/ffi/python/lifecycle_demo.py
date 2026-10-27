"""
Minimal Python ctypes lifecycle example for the standard LuaSkills FFI surface.
LuaSkills 标准 FFI 接口的最小 Python ctypes 生命周期示例。
"""

import ctypes

from demo import (
    FfiBorrowedBuffer,
    FfiLuaEngineOptions,
    FfiLuaRuntimeHostOptions,
    FfiLuaVmPoolConfig,
    FfiOwnedBuffer,
    FfiRuntimeEntryDescriptorList,
    FfiRuntimeInvocationResult,
    FfiRuntimeSkillRoot,
    LUASKILLS_SKILL_AUTHORITY_SYSTEM,
    ensure_standard_fixture_layout,
    load_library,
    make_borrowed_buffer,
    must_ok,
    read_owned_buffer_text,
    standard_fixture_runtime_root,
)


def print_entry_count(engine_id: int, library: ctypes.CDLL) -> int:
    """
    Print the current structured entry count returned by the standard ABI.
    输出标准 ABI 当前返回的结构化入口数量。
    """

    entries_ptr = ctypes.POINTER(FfiRuntimeEntryDescriptorList)()
    error_buffer = FfiOwnedBuffer()
    must_ok(
        library.luaskills_ffi_list_entries(
            engine_id,
            LUASKILLS_SKILL_AUTHORITY_SYSTEM,
            ctypes.byref(entries_ptr),
            ctypes.byref(error_buffer),
        ),
        error_buffer,
        library,
    )
    try:
        entry_count = entries_ptr.contents.len if entries_ptr else 0
        print("Current entry count:", entry_count)
        return entry_count
    finally:
        if entries_ptr:
            library.luaskills_ffi_entry_list_free(entries_ptr)


def call_fixture_skill(engine_id: int, note: str, library: ctypes.CDLL) -> str:
    """
    Call the standard fixture entry and return the rendered content string.
    调用标准夹具入口并返回渲染后的内容字符串。
    """

    args_storage, args_buffer = make_borrowed_buffer(f'{{"note":"{note}"}}')
    _borrowed_payload = args_storage
    invocation_result_ptr = ctypes.POINTER(FfiRuntimeInvocationResult)()
    error_buffer = FfiOwnedBuffer()
    must_ok(
        library.luaskills_ffi_call_skill(
            engine_id,
            b"demo-standard-ffi-skill-ping",
            args_buffer,
            None,
            ctypes.byref(invocation_result_ptr),
            ctypes.byref(error_buffer),
        ),
        error_buffer,
        library,
    )
    try:
        invocation_result = invocation_result_ptr.contents
        return read_owned_buffer_text(invocation_result.content)
    finally:
        if invocation_result_ptr:
            library.luaskills_ffi_invocation_result_free(invocation_result_ptr)


def main() -> None:
    """
    Demonstrate disable and enable lifecycle transitions through the standard ABI.
    演示通过标准 ABI 执行 disable 与 enable 生命周期切换。
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
    library.luaskills_ffi_list_entries.argtypes = [
        ctypes.c_uint64,
        ctypes.c_int32,
        ctypes.POINTER(ctypes.POINTER(FfiRuntimeEntryDescriptorList)),
        ctypes.POINTER(FfiOwnedBuffer),
    ]
    library.luaskills_ffi_call_skill.argtypes = [
        ctypes.c_uint64,
        ctypes.c_char_p,
        FfiBorrowedBuffer,
        ctypes.c_void_p,
        ctypes.POINTER(ctypes.POINTER(FfiRuntimeInvocationResult)),
        ctypes.POINTER(FfiOwnedBuffer),
    ]
    library.luaskills_ffi_system_disable_skill.argtypes = [
        ctypes.c_uint64,
        ctypes.POINTER(FfiRuntimeSkillRoot),
        ctypes.c_size_t,
        ctypes.c_int32,
        ctypes.c_char_p,
        ctypes.c_char_p,
        ctypes.POINTER(FfiOwnedBuffer),
    ]
    library.luaskills_ffi_system_enable_skill.argtypes = [
        ctypes.c_uint64,
        ctypes.POINTER(FfiRuntimeSkillRoot),
        ctypes.c_size_t,
        ctypes.c_int32,
        ctypes.c_char_p,
        ctypes.POINTER(FfiOwnedBuffer),
    ]
    library.luaskills_ffi_entry_list_free.argtypes = [
        ctypes.POINTER(FfiRuntimeEntryDescriptorList),
    ]
    library.luaskills_ffi_entry_list_free.restype = None
    library.luaskills_ffi_invocation_result_free.argtypes = [
        ctypes.POINTER(FfiRuntimeInvocationResult),
    ]
    library.luaskills_ffi_invocation_result_free.restype = None
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

    print_entry_count(engine_id.value, library)
    print("Call before disable:", call_fixture_skill(engine_id.value, "before-disable", library))

    error_buffer = FfiOwnedBuffer()
    must_ok(
        library.luaskills_ffi_system_disable_skill(
            engine_id.value,
            skill_roots,
            len(skill_roots),
            LUASKILLS_SKILL_AUTHORITY_SYSTEM,
            b"demo-standard-ffi-skill",
            b"maintenance window",
            ctypes.byref(error_buffer),
        ),
        error_buffer,
        library,
    )
    print("Skill disabled: demo-standard-ffi-skill")
    print_entry_count(engine_id.value, library)

    disabled_args_storage, disabled_args_buffer = make_borrowed_buffer('{"note":"after-disable"}')
    _disabled_payload = disabled_args_storage
    disabled_result_ptr = ctypes.POINTER(FfiRuntimeInvocationResult)()
    error_buffer = FfiOwnedBuffer()
    disabled_status = library.luaskills_ffi_call_skill(
        engine_id.value,
        b"demo-standard-ffi-skill-ping",
        disabled_args_buffer,
        None,
        ctypes.byref(disabled_result_ptr),
        ctypes.byref(error_buffer),
    )
    if disabled_status == 0:
        raise RuntimeError("call_skill unexpectedly succeeded while the skill was disabled")
    print(
        "Call after disable failed as expected:",
        read_owned_buffer_text(error_buffer),
    )
    library.luaskills_ffi_buffer_free(error_buffer)

    error_buffer = FfiOwnedBuffer()
    must_ok(
        library.luaskills_ffi_system_enable_skill(
            engine_id.value,
            skill_roots,
            len(skill_roots),
            LUASKILLS_SKILL_AUTHORITY_SYSTEM,
            b"demo-standard-ffi-skill",
            ctypes.byref(error_buffer),
        ),
        error_buffer,
        library,
    )
    print("Skill enabled: demo-standard-ffi-skill")
    print_entry_count(engine_id.value, library)
    print("Call after enable:", call_fixture_skill(engine_id.value, "after-enable", library))

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
