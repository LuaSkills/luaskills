/**
Minimal TypeScript example for the standard LuaSkills FFI surface using koffi.
使用 koffi 调用 LuaSkills 标准 FFI 接口的最小 TypeScript 示例。
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
    "system_lua_lib",
    path.join("dependencies", "runtimes"),
    path.join("dependencies", "envs"),
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
Run one version query, one root load, one structured entry-list read, one standard call_skill roundtrip, and one standard run_lua roundtrip.
通过标准 ABI 执行一次版本查询、一次根链加载、一次结构化入口列表读取、一次标准 call_skill 往返调用以及一次标准 run_lua 往返调用。
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
    system_lua_lib_dir: "str",
    download_cache_root: "str",
    dependency_dir_name: "str",
    state_dir_name: "str",
    database_dir_name: "str",
    skill_config_file_path: "str",
    allow_network_download: "uint8_t",
    github_base_url: "str",
    github_api_base_url: "str",
    official_skill_hub_base_url: "str",
    enable_private_url_skill_install: "uint8_t",
    private_skill_source_allowlist: "void *",
    private_skill_source_allowlist_len: "size_t",
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

  const FfiLuaRuntimeHostOptionsV2 = koffi.struct("FfiLuaRuntimeHostOptionsV2", {
    base: FfiLuaRuntimeHostOptions,
    runtime_root: "str",
  });

  // FfiLuaRuntimeManagedRuntimeConfig matches the public v3 C ABI policy byte-for-byte.
  // FfiLuaRuntimeManagedRuntimeConfig 与公开 v3 C ABI 策略保持逐字节一致。
  const FfiLuaRuntimeManagedRuntimeConfig = koffi.struct("FfiLuaRuntimeManagedRuntimeConfig", {
    worker_pool_max_size_per_environment: "size_t",
    worker_idle_ttl_secs: "uint64_t",
    persistent_session_limit_per_engine: "size_t",
    persistent_session_default_buffer_limit_bytes_per_stream: "size_t",
    has_invoke_default_timeout_ms: "uint8_t",
    invoke_default_timeout_ms: "uint64_t",
  });

  // FfiLuaRuntimeHostOptionsV3 appends two managed roots and the optional B3-B7 policy pointer.
  // FfiLuaRuntimeHostOptionsV3 追加两个受管根与可选 B3-B7 策略指针。
  const FfiLuaRuntimeHostOptionsV3 = koffi.struct("FfiLuaRuntimeHostOptionsV3", {
    base: FfiLuaRuntimeHostOptionsV2,
    managed_runtime_distribution_root: "str",
    managed_runtime_environment_root: "str",
    managed_runtime_config: koffi.pointer(FfiLuaRuntimeManagedRuntimeConfig),
  });

  // FfiLuaEngineOptionsV3 combines the stable VM pool prefix with the complete v3 host options.
  // FfiLuaEngineOptionsV3 将稳定 VM 池前缀与完整 v3 宿主选项组合起来。
  const FfiLuaEngineOptionsV3 = koffi.struct("FfiLuaEngineOptionsV3", {
    pool: FfiLuaVmPoolConfig,
    host: FfiLuaRuntimeHostOptionsV3,
  });

  const FfiOwnedBuffer = koffi.struct("FfiOwnedBuffer", {
    ptr: "void *",
    len: "size_t",
  });

  const FfiBorrowedBuffer = koffi.struct("FfiBorrowedBuffer", {
    ptr: "void *",
    len: "size_t",
  });

  const FfiLuaInvocationContext = koffi.struct("FfiLuaInvocationContext", {
    request_context_json: FfiBorrowedBuffer,
    client_budget_json: FfiBorrowedBuffer,
    tool_config_json: FfiBorrowedBuffer,
  });

  const FfiRuntimeSkillRoot = koffi.struct("FfiRuntimeSkillRoot", {
    name: "str",
    skills_dir: "str",
  });

  const FfiRuntimeEntryParameterDescriptor = koffi.struct("FfiRuntimeEntryParameterDescriptor", {
    name: FfiOwnedBuffer,
    param_type: FfiOwnedBuffer,
    description: FfiOwnedBuffer,
    required: "uint8_t",
  });

  const FfiRuntimeEntryDescriptor = koffi.struct("FfiRuntimeEntryDescriptor", {
    canonical_name: FfiOwnedBuffer,
    skill_id: FfiOwnedBuffer,
    local_name: FfiOwnedBuffer,
    root_name: FfiOwnedBuffer,
    skill_dir: FfiOwnedBuffer,
    description: FfiOwnedBuffer,
    input_schema_json: FfiOwnedBuffer,
    parameters: "void *",
    parameters_len: "size_t",
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
  const version = library.func("int luaskills_ffi_version(_Out_ FfiOwnedBuffer *version_out, _Out_ FfiOwnedBuffer *error_out)");
  const engineNew = library.func("int luaskills_ffi_engine_new_v3(const FfiLuaEngineOptionsV3 *options, _Out_ uint64_t *engine_id_out, _Out_ FfiOwnedBuffer *error_out)");
  const loadFromRoots = library.func("int luaskills_ffi_load_from_roots(uint64_t engine_id, const FfiRuntimeSkillRoot *skill_roots, size_t skill_roots_len, _Out_ FfiOwnedBuffer *error_out)");
  const listEntries = library.func("int luaskills_ffi_list_entries(uint64_t engine_id, int32_t authority, _Out_ void **entries_out, _Out_ FfiOwnedBuffer *error_out)");
  const callSkill = library.func("int luaskills_ffi_call_skill(uint64_t engine_id, const char *tool_name, FfiBorrowedBuffer args_json, const FfiLuaInvocationContext *invocation_context, _Out_ void **result_out, _Out_ FfiOwnedBuffer *error_out)");
  const runLua = library.func("int luaskills_ffi_run_lua(uint64_t engine_id, const char *code, FfiBorrowedBuffer args_json, const FfiLuaInvocationContext *invocation_context, _Out_ FfiOwnedBuffer *result_json_out, _Out_ FfiOwnedBuffer *error_out)");
  const freeEntryList = library.func("void luaskills_ffi_entry_list_free(void *value)");
  const freeInvocationResult = library.func("void luaskills_ffi_invocation_result_free(void *value)");
  const engineFree = library.func("int luaskills_ffi_engine_free(uint64_t engine_id, _Out_ FfiOwnedBuffer *error_out)");

  const versionOut = [{ ptr: null, len: 0 }];
  const versionError = [{ ptr: null, len: 0 }];
  mustOK(version(versionOut, versionError), versionError[0], freeBuffer);
  console.log("Version:", readOwnedBuffer(versionOut[0]));
  freeBuffer(versionOut[0]);

  // ManagedRuntimeConfigBuffer owns the exact native bytes referenced by the nested v3 host options.
  // ManagedRuntimeConfigBuffer 拥有嵌套 v3 宿主选项所引用的精确原生字节。
  const managedRuntimeConfigBuffer = Buffer.alloc(koffi.sizeof(FfiLuaRuntimeManagedRuntimeConfig));
  koffi.encode(managedRuntimeConfigBuffer, FfiLuaRuntimeManagedRuntimeConfig, {
    worker_pool_max_size_per_environment: 8,
    worker_idle_ttl_secs: 120,
    persistent_session_limit_per_engine: 128,
    persistent_session_default_buffer_limit_bytes_per_stream: 2 * 1024 * 1024,
    has_invoke_default_timeout_ms: 1,
    invoke_default_timeout_ms: 30_000,
  });

  // Options is the complete v3 engine payload retained through the synchronous constructor call.
  // Options 是在同步构造调用期间保持有效的完整 v3 引擎载荷。
  const options = {
    pool: { min_size: 1, max_size: 1, idle_ttl_secs: 30 },
    host: {
      base: {
        base: {
          temp_dir: path.join(runtimeRoot, "temp"),
          resources_dir: path.join(runtimeRoot, "resources"),
          lua_packages_dir: path.join(runtimeRoot, "lua_packages"),
          host_provided_tool_root: path.join(runtimeRoot, "bin", "tools"),
          host_provided_lua_root: path.join(runtimeRoot, "lua_packages"),
          host_provided_ffi_root: path.join(runtimeRoot, "libs"),
          system_lua_lib_dir: path.join(runtimeRoot, "system_lua_lib"),
          download_cache_root: path.join(runtimeRoot, "temp", "downloads"),
          dependency_dir_name: "dependencies",
          state_dir_name: "state",
          database_dir_name: "databases",
          skill_config_file_path: null,
          allow_network_download: 0,
          github_base_url: null,
          github_api_base_url: null,
          official_skill_hub_base_url: null,
          enable_private_url_skill_install: 0,
          private_skill_source_allowlist: null,
          private_skill_source_allowlist_len: 0,
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
        runtime_root: runtimeRoot,
      },
      managed_runtime_distribution_root: path.join(runtimeRoot, "dependencies", "runtimes"),
      managed_runtime_environment_root: path.join(runtimeRoot, "dependencies", "envs"),
      managed_runtime_config: managedRuntimeConfigBuffer,
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

  const entriesOut = [null];
  const entriesError = [{ ptr: null, len: 0 }];
  mustOK(listEntries(engineIdOut[0], LUASKILLS_SKILL_AUTHORITY_SYSTEM, entriesOut, entriesError), entriesError[0], freeBuffer);
  if (entriesOut[0]) {
    const entryList = koffi.decode(entriesOut[0], FfiRuntimeEntryDescriptorList) as {
      items: Buffer | null;
      len: number | bigint;
    };
    const entries = entryList.items
      ? (koffi.decode(
          entryList.items,
          koffi.array(FfiRuntimeEntryDescriptor, Number(entryList.len)),
        ) as Array<{
          canonical_name: { ptr: Buffer | null; len: number | bigint };
          skill_id: { ptr: Buffer | null; len: number | bigint };
          description: { ptr: Buffer | null; len: number | bigint };
          input_schema_json: { ptr: Buffer | null; len: number | bigint };
          parameters: Buffer | null;
          parameters_len: number | bigint;
        }>)
      : [];
    console.log("Entry count:", entries.length);
    if (entries.length > 0) {
      const firstEntry = entries[0];
      console.log("First canonical entry:", readOwnedBuffer(firstEntry.canonical_name));
      console.log("First entry skill id:", readOwnedBuffer(firstEntry.skill_id));
      console.log("First entry description:", readOwnedBuffer(firstEntry.description));
      console.log("First entry input schema:", readOwnedBuffer(firstEntry.input_schema_json));
      const parameters = firstEntry.parameters
        ? (koffi.decode(
            firstEntry.parameters,
            koffi.array(FfiRuntimeEntryParameterDescriptor, Number(firstEntry.parameters_len)),
          ) as Array<{
            name: { ptr: Buffer | null; len: number | bigint };
            param_type: { ptr: Buffer | null; len: number | bigint };
            required: number | bigint;
          }>)
        : [];
      console.log("First entry parameter count:", parameters.length);
      if (parameters.length > 0) {
        const firstParameter = parameters[0];
        console.log("First parameter name:", readOwnedBuffer(firstParameter.name));
        console.log("First parameter type:", readOwnedBuffer(firstParameter.param_type));
        console.log("First parameter required:", Number(firstParameter.required) !== 0);
      }
    } else {
      console.log("No entries were returned by the current fixture root.");
    }
    freeEntryList(entriesOut[0]);
  }

  const argsJson = makeBorrowedBuffer('{"note":"typescript"}');
  const requestContext = makeBorrowedBuffer('{"transport_name":"ts-demo"}');
  const clientBudget = makeBorrowedBuffer('{"budget":1}');
  const toolConfig = makeBorrowedBuffer('{"mode":"standard-demo"}');
  const invocationContext = [
    {
      request_context_json: requestContext.buffer,
      client_budget_json: clientBudget.buffer,
      tool_config_json: toolConfig.buffer,
    },
  ];
  const invocationOut = [null];
  const invocationError = [{ ptr: null, len: 0 }];
  mustOK(
    callSkill(
      engineIdOut[0],
      "demo-standard-ffi-skill-ping",
      argsJson.buffer,
      invocationContext,
      invocationOut,
      invocationError,
    ),
    invocationError[0],
    freeBuffer,
  );
  if (invocationOut[0]) {
    const invocationResult = koffi.decode(invocationOut[0], FfiRuntimeInvocationResult) as {
      content: { ptr: Buffer | null; len: number | bigint };
      template_hint: { ptr: Buffer | null; len: number | bigint };
      content_bytes: number | bigint;
      content_lines: number | bigint;
    };
    console.log("Call content:", readOwnedBuffer(invocationResult.content));
    console.log("Call content bytes:", Number(invocationResult.content_bytes));
    console.log("Call content lines:", Number(invocationResult.content_lines));
    console.log("Call template hint:", readOwnedBuffer(invocationResult.template_hint));
    freeInvocationResult(invocationOut[0]);
  }

  const runLuaArgs = makeBorrowedBuffer('{"note":"ts-lua"}');
  const runLuaResult = [{ ptr: null, len: 0 }];
  const runLuaError = [{ ptr: null, len: 0 }];
  mustOK(
    runLua(
      engineIdOut[0],
      "return { note = args.note, transport = vulcan.context.request.transport_name, budget = vulcan.context.client_budget.budget, mode = vulcan.context.tool_config.mode }",
      runLuaArgs.buffer,
      invocationContext,
      runLuaResult,
      runLuaError,
    ),
    runLuaError[0],
    freeBuffer,
  );
  console.log("Run Lua result JSON:", readOwnedBuffer(runLuaResult[0]));
  freeBuffer(runLuaResult[0]);

  const freeError = [{ ptr: null, len: 0 }];
  mustOK(engineFree(engineIdOut[0], freeError), freeError[0], freeBuffer);
  console.log("Engine freed");
}

main();
