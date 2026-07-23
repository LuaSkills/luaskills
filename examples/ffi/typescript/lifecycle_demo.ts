/**
Minimal TypeScript lifecycle example for the standard LuaSkills FFI surface using koffi.
使用 koffi 调用 LuaSkills 标准 FFI 生命周期接口的最小 TypeScript 示例。
 */

import koffi from "koffi";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

/**
Full host-system authority for standard FFI query examples.
标准 FFI 查询示例使用的完整宿主系统权限。
 */
const LUASKILLS_SKILL_AUTHORITY_SYSTEM = 0;

/**
Resolve the dynamic library path from one explicit environment variable.
从一个显式环境变量解析动态库路径。
 */
function resolveLibraryPath(): string {
  const libraryPath = process.env.LUASKILLS_LIB;
  if (!libraryPath) {
    throw new Error("LUASKILLS_LIB is not set");
  }
  return libraryPath;
}

/**
Resolve the dedicated standard-ABI fixture runtime root bundled under standard_runtime.
解析位于 standard_runtime 下供标准 ABI 示例共用的专用夹具运行时根目录。
 */
function resolveStandardFixtureRuntimeRoot(): string {
  const currentFile = fileURLToPath(import.meta.url);
  return path.join(path.dirname(path.dirname(currentFile)), "standard_runtime", "runtime_root");
}

/**
Ensure the shared standard-ABI fixture runtime directory layout exists.
确保标准 ABI 共用夹具运行时目录结构存在。
 */
function ensureStandardFixtureLayout(root: string): void {
  for (const relativePath of [
    "skills",
    "dependencies",
    "state",
    "databases",
    "temp",
    "resources",
    "lua_packages",
    path.join("bin", "tools"),
    "libs",
  ]) {
    fs.mkdirSync(path.join(root, relativePath), { recursive: true });
  }
}

/**
Read one nested owned UTF-8 buffer into one JavaScript string without freeing it immediately.
将一个嵌套拥有型 UTF-8 缓冲读取为一个 JavaScript 字符串但不立即释放。
 */
function readOwnedBuffer(buffer: { ptr: Buffer | null; len: number | bigint } | null): string {
  if (!buffer?.ptr) {
    return "";
  }
  const bytes = koffi.decode(
    buffer.ptr,
    koffi.array("uint8_t", Number(buffer.len)),
  ) as number[];
  return Buffer.from(bytes).toString("utf8");
}

/**
Build one borrowed UTF-8 buffer whose payload stays alive for one standard FFI call.
构造一个在一次标准 FFI 调用期间保持有效的借用型 UTF-8 缓冲。
 */
function makeBorrowedBuffer(text: string): {
  payload: Buffer | null;
  buffer: { ptr: Buffer | null; len: number };
} {
  if (text.length === 0) {
    return {
      payload: null,
      buffer: { ptr: null, len: 0 },
    };
  }
  const payload = Buffer.from(text, "utf8");
  return {
    payload,
    buffer: {
      ptr: payload,
      len: payload.length,
    },
  };
}

/**
Raise one JavaScript error when the standard FFI call reports failure.
当标准 FFI 调用报告失败时抛出一个 JavaScript 错误。
 */
function mustOK(
  status: number,
  errorBuffer: { ptr: Buffer | null; len: number | bigint } | null,
  freeBuffer: (buffer: { ptr: Buffer | null; len: number | bigint }) => void,
): void {
  if (status === 0) {
    return;
  }
  const message = readOwnedBuffer(errorBuffer);
  if (errorBuffer) {
    freeBuffer(errorBuffer);
  }
  throw new Error(message || "Unknown FFI error");
}

/**
Load the current entry list and return its count.
读取当前入口列表并返回其数量。
 */
function printEntryCount(
  engineId: bigint,
  listEntries: (engineId: bigint, authority: number, entriesOut: Array<Buffer | null>, errorOut: Array<{ ptr: Buffer | null; len: number | bigint }>) => number,
  freeEntryList: (value: Buffer | null) => void,
  freeBuffer: (buffer: { ptr: Buffer | null; len: number | bigint }) => void,
  FfiRuntimeEntryDescriptorList: koffi.IKoffiCType,
): number {
  const entriesOut = [null];
  const errorOut = [{ ptr: null, len: 0 }];
  mustOK(listEntries(engineId, LUASKILLS_SKILL_AUTHORITY_SYSTEM, entriesOut, errorOut), errorOut[0], freeBuffer);
  try {
    if (!entriesOut[0]) {
      console.log("Current entry count:", 0);
      return 0;
    }
    const entryList = koffi.decode(entriesOut[0], FfiRuntimeEntryDescriptorList) as {
      items: Buffer | null;
      len: number | bigint;
    };
    const entryCount = Number(entryList.len);
    console.log("Current entry count:", entryCount);
    return entryCount;
  } finally {
    if (entriesOut[0]) {
      freeEntryList(entriesOut[0]);
    }
  }
}

/**
Invoke the shared fixture skill entry and return its textual content.
调用共享夹具技能入口并返回其文本内容。
 */
function callFixtureSkill(
  engineId: bigint,
  note: string,
  callSkill: (
    engineId: bigint,
    toolName: string,
    argsJson: { ptr: Buffer | null; len: number },
    invocationContext: null,
    resultOut: Array<Buffer | null>,
    errorOut: Array<{ ptr: Buffer | null; len: number | bigint }>,
  ) => number,
  freeInvocationResult: (value: Buffer | null) => void,
  freeBuffer: (buffer: { ptr: Buffer | null; len: number | bigint }) => void,
  FfiRuntimeInvocationResult: koffi.IKoffiCType,
): string {
  const argsJson = makeBorrowedBuffer(`{"note":"${note}"}`);
  const invocationOut = [null];
  const errorOut = [{ ptr: null, len: 0 }];
  mustOK(
    callSkill(
      engineId,
      "demo-standard-ffi-skill-ping",
      argsJson.buffer,
      null,
      invocationOut,
      errorOut,
    ),
    errorOut[0],
    freeBuffer,
  );
  if (!invocationOut[0]) {
    throw new Error("Invocation result pointer is null");
  }
  try {
    const invocationResult = koffi.decode(invocationOut[0], FfiRuntimeInvocationResult) as {
      content: { ptr: Buffer | null; len: number | bigint };
    };
    return readOwnedBuffer(invocationResult.content);
  } finally {
    freeInvocationResult(invocationOut[0]);
  }
}

/**
Run one lifecycle smoke flow through the standard ABI.
通过标准 ABI 执行一条生命周期烟测链路。
 */
function main(): void {
  const library = koffi.load(resolveLibraryPath());
  const runtimeRoot = resolveStandardFixtureRuntimeRoot();
  ensureStandardFixtureLayout(runtimeRoot);

  const FfiLuaVmPoolConfig = koffi.struct("FfiLuaVmPoolConfig", {
    min_size: "size_t",
    max_size: "size_t",
    idle_ttl_secs: "uint64_t",
  });

  const FfiLuaRuntimeHostOptions = koffi.struct("FfiLuaRuntimeHostOptions", {
    temp_dir: "str",
    resources_dir: "str",
    lua_packages_dir: "str",
    host_provided_tool_root: "str",
    host_provided_lua_root: "str",
    host_provided_ffi_root: "str",
    download_cache_root: "str",
    dependency_dir_name: "str",
    state_dir_name: "str",
    database_dir_name: "str",
    skill_config_root: "str",
    skill_config_lock_timeout_ms: "uint64_t",
    skill_config_watch_debounce_ms: "uint64_t",
    allow_network_download: "uint8_t",
    github_base_url: "str",
    github_api_base_url: "str",
    sqlite_library_path: "str",
    sqlite_provider_mode: "int32_t",
    sqlite_callback_mode: "int32_t",
    lancedb_library_path: "str",
    lancedb_provider_mode: "int32_t",
    lancedb_callback_mode: "int32_t",
    space_controller_endpoint: "str",
    space_controller_auto_spawn: "uint8_t",
    space_controller_executable_path: "str",
    space_controller_process_mode: "int32_t",
    cache_config: "void *",
    runlua_pool_config: "void *",
    reserved_entry_names: "void *",
    reserved_entry_names_len: "size_t",
    ignored_skill_ids: "void *",
    ignored_skill_ids_len: "size_t",
    enable_skill_management_bridge: "uint8_t",
    default_text_encoding: "str",
    disable_managed_io_compat: "uint8_t",
  });

  const FfiLuaEngineOptions = koffi.struct("FfiLuaEngineOptions", {
    pool: FfiLuaVmPoolConfig,
    host: FfiLuaRuntimeHostOptions,
  });

  const FfiOwnedBuffer = koffi.struct("FfiOwnedBuffer", {
    ptr: "void *",
    len: "size_t",
  });

  const FfiBorrowedBuffer = koffi.struct("FfiBorrowedBuffer", {
    ptr: "void *",
    len: "size_t",
  });

  const FfiRuntimeSkillRoot = koffi.struct("FfiRuntimeSkillRoot", {
    name: "str",
    skills_dir: "str",
  });

  const FfiRuntimeEntryDescriptorList = koffi.struct("FfiRuntimeEntryDescriptorList", {
    items: "void *",
    len: "size_t",
  });

  const FfiRuntimeInvocationResult = koffi.struct("FfiRuntimeInvocationResult", {
    content: FfiOwnedBuffer,
    overflow_mode: "int32_t",
    template_hint: FfiOwnedBuffer,
    content_bytes: "size_t",
    content_lines: "size_t",
  });

  const freeBuffer = library.func("void luaskills_ffi_buffer_free(FfiOwnedBuffer value)");
  const engineNew = library.func("int luaskills_ffi_engine_new(const FfiLuaEngineOptions *options, _Out_ uint64_t *engine_id_out, _Out_ FfiOwnedBuffer *error_out)");
  const loadFromRoots = library.func("int luaskills_ffi_load_from_roots(uint64_t engine_id, const FfiRuntimeSkillRoot *skill_roots, size_t skill_roots_len, _Out_ FfiOwnedBuffer *error_out)");
  const listEntries = library.func("int luaskills_ffi_list_entries(uint64_t engine_id, int32_t authority, _Out_ void **entries_out, _Out_ FfiOwnedBuffer *error_out)");
  const callSkill = library.func("int luaskills_ffi_call_skill(uint64_t engine_id, const char *tool_name, FfiBorrowedBuffer args_json, void *invocation_context, _Out_ void **result_out, _Out_ FfiOwnedBuffer *error_out)");
  const disableSkill = library.func("int luaskills_ffi_system_disable_skill(uint64_t engine_id, const FfiRuntimeSkillRoot *skill_roots, size_t skill_roots_len, int32_t authority, const char *skill_id, const char *reason, _Out_ FfiOwnedBuffer *error_out)");
  const enableSkill = library.func("int luaskills_ffi_system_enable_skill(uint64_t engine_id, const FfiRuntimeSkillRoot *skill_roots, size_t skill_roots_len, int32_t authority, const char *skill_id, _Out_ FfiOwnedBuffer *error_out)");
  const freeEntryList = library.func("void luaskills_ffi_entry_list_free(void *value)");
  const freeInvocationResult = library.func("void luaskills_ffi_invocation_result_free(void *value)");
  const engineFree = library.func("int luaskills_ffi_engine_free(uint64_t engine_id, _Out_ FfiOwnedBuffer *error_out)");

  const options = {
    pool: { min_size: 1, max_size: 1, idle_ttl_secs: 30 },
    host: {
      temp_dir: path.join(runtimeRoot, "temp"),
      resources_dir: path.join(runtimeRoot, "resources"),
      lua_packages_dir: path.join(runtimeRoot, "lua_packages"),
      host_provided_tool_root: path.join(runtimeRoot, "bin", "tools"),
      host_provided_lua_root: path.join(runtimeRoot, "lua_packages"),
      host_provided_ffi_root: path.join(runtimeRoot, "libs"),
      download_cache_root: path.join(runtimeRoot, "temp", "downloads"),
      dependency_dir_name: "dependencies",
      state_dir_name: "state",
      database_dir_name: "databases",
      skill_config_root: null,
      skill_config_lock_timeout_ms: 0,
      skill_config_watch_debounce_ms: 0,
      allow_network_download: 0,
      github_base_url: null,
      github_api_base_url: null,
      sqlite_library_path: null,
      sqlite_provider_mode: 0,
      sqlite_callback_mode: 0,
      lancedb_library_path: null,
      lancedb_provider_mode: 0,
      lancedb_callback_mode: 0,
      space_controller_endpoint: null,
      space_controller_auto_spawn: 0,
      space_controller_executable_path: null,
      space_controller_process_mode: 0,
      cache_config: null,
      runlua_pool_config: null,
      reserved_entry_names: null,
      reserved_entry_names_len: 0,
      ignored_skill_ids: null,
      ignored_skill_ids_len: 0,
      enable_skill_management_bridge: 0,
      default_text_encoding: null,
      disable_managed_io_compat: 0,
    },
  };

  const engineIdOut = [0n];
  const engineError = [{ ptr: null, len: 0 }];
  mustOK(engineNew(options, engineIdOut, engineError), engineError[0], freeBuffer);
  console.log("Engine created:", engineIdOut[0].toString());

  const rootArray = [
    {
      name: "ROOT",
      skills_dir: path.join(runtimeRoot, "skills"),
    },
  ];
  const loadError = [{ ptr: null, len: 0 }];
  mustOK(loadFromRoots(engineIdOut[0], rootArray, rootArray.length, loadError), loadError[0], freeBuffer);
  console.log("Loaded roots from:", path.join(runtimeRoot, "skills"));

  printEntryCount(engineIdOut[0], listEntries, freeEntryList, freeBuffer, FfiRuntimeEntryDescriptorList);
  console.log(
    "Call before disable:",
    callFixtureSkill(engineIdOut[0], "before-disable", callSkill, freeInvocationResult, freeBuffer, FfiRuntimeInvocationResult),
  );

  const disableError = [{ ptr: null, len: 0 }];
  mustOK(
    disableSkill(
      engineIdOut[0],
      rootArray,
      rootArray.length,
      LUASKILLS_SKILL_AUTHORITY_SYSTEM,
      "demo-standard-ffi-skill",
      "maintenance window",
      disableError,
    ),
    disableError[0],
    freeBuffer,
  );
  console.log("Skill disabled: demo-standard-ffi-skill");
  printEntryCount(engineIdOut[0], listEntries, freeEntryList, freeBuffer, FfiRuntimeEntryDescriptorList);

  const disabledArgs = makeBorrowedBuffer('{"note":"after-disable"}');
  const disabledResultOut = [null];
  const disabledError = [{ ptr: null, len: 0 }];
  const disabledStatus = callSkill(
    engineIdOut[0],
    "demo-standard-ffi-skill-ping",
    disabledArgs.buffer,
    null,
    disabledResultOut,
    disabledError,
  );
  if (disabledStatus === 0) {
    throw new Error("call_skill unexpectedly succeeded while the skill was disabled");
  }
  console.log("Call after disable failed as expected:", readOwnedBuffer(disabledError[0]));
  freeBuffer(disabledError[0]);

  const enableError = [{ ptr: null, len: 0 }];
  mustOK(
    enableSkill(
      engineIdOut[0],
      rootArray,
      rootArray.length,
      LUASKILLS_SKILL_AUTHORITY_SYSTEM,
      "demo-standard-ffi-skill",
      enableError,
    ),
    enableError[0],
    freeBuffer,
  );
  console.log("Skill enabled: demo-standard-ffi-skill");
  printEntryCount(engineIdOut[0], listEntries, freeEntryList, freeBuffer, FfiRuntimeEntryDescriptorList);
  console.log(
    "Call after enable:",
    callFixtureSkill(engineIdOut[0], "after-enable", callSkill, freeInvocationResult, freeBuffer, FfiRuntimeInvocationResult),
  );

  const freeError = [{ ptr: null, len: 0 }];
  mustOK(engineFree(engineIdOut[0], freeError), freeError[0], freeBuffer);
  console.log("Engine freed");
}

main();
