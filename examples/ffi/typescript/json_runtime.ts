/**
Reusable JSON FFI runtime helpers for TypeScript host demos.
供 TypeScript 宿主演示复用的 JSON FFI 运行时辅助层。
 */

import koffi from "koffi";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

/**
Generic JSON object payload passed through helper boundaries.
在辅助层边界上传递的通用 JSON 对象载荷。
 */
export type JsonMap = Record<string, unknown>;

/**
Stable runtime-lease identity payload carried by host-side wrappers.
宿主侧包装层携带的稳定运行时租约身份载荷。
 */
export type RuntimeLeaseIdentity = {
  lease_id: string;
  sid: string;
  generation: number;
};

/**
Strict System Plugin package descriptor required by system runtime-lease creation.
system 运行时租约创建所必需的严格 System Plugin 包描述符。
 */
export type SystemRuntimePackageDescriptor = {
  /**
  Stable package identifier selected by the host.
  宿主选择的稳定包标识符。
   */
  id: string;
  /**
  Absolute package root strictly below the engine's system_lua_lib directory.
  严格位于引擎 system_lua_lib 目录下的绝对包根目录。
   */
  root: string;
  /**
  Package-relative dependency manifest path.
  包相对依赖清单路径。
   */
  dependencies_file: string;
};

/**
Host-owned path context accepted by strict System runtime-lease creation.
严格 System 运行时租约创建接受的宿主所有路径上下文。
 */
export type SystemRuntimeLeaseContext = {
  /** Child working directory under the package or authorized workspace root. */
  /** 位于包根或已授权工作区根下的子进程工作目录。 */
  cwd?: string;
  /** Canonical workspace root explicitly authorized by the host. */
  /** 宿主显式授权的规范工作区根。 */
  workspace_root?: string;
  /** Recursively read-only structured mount metadata exposed to Lua. */
  /** 暴露给 Lua 的递归只读结构化挂载元数据。 */
  mounts?: JsonMap;
};

/**
Stable event kinds emitted for System Plugin managed child sessions.
为 System Plugin 受管子会话发出的稳定事件类型。
 */
export type ManagedSessionEventKind =
  | "stdout_readable"
  | "stderr_readable"
  | "exited"
  | "failed";

/**
One sequenced host-visible event for a managed child session.
单个带序号、宿主可见的受管子会话事件。
 */
export type ManagedSessionEvent = {
  /** Opaque System lease id that owns the child session. */
  /** 拥有子会话的不透明 System 租约标识。 */
  system_lease_id: string;
  /** Stable host-selected System lease SID. */
  /** 宿主选择的稳定 System 租约 SID。 */
  sid: string;
  /** SID-local lease generation. */
  /** SID 内部的租约代际。 */
  generation: number;
  /** Engine-local id returned by the managed session status. */
  /** 由受管会话状态返回的引擎内局部标识。 */
  managed_session_id: number;
  /** Coalesced readiness or terminal event kind. */
  /** 合并后的就绪或终止事件类型。 */
  kind: ManagedSessionEventKind;
  /** Engine event-center-global monotonic sequence. */
  /** 引擎事件中心全局单调序号。 */
  sequence: number;
};

/**
One bounded destructive event-drain result.
单次有界破坏性事件排空结果。
 */
export type ManagedSessionEventBatch = {
  /** Events removed in sequence order. */
  /** 按序号顺序移除的事件。 */
  events: ManagedSessionEvent[];
  /** Pending logical events after this drain. */
  /** 本次排空后仍待处理的逻辑事件数。 */
  remaining: number;
  /** Whether wait returned because its deadline expired without an event. */
  /** wait 是否因截止时间到期且没有事件而返回。 */
  timed_out: boolean;
};

/**
Stable authority labels used by system JSON FFI wrappers.
system JSON FFI 包装层使用的稳定权限标签。
 */
export type SkillManagementAuthority = "system" | "delegated_tool";

/**
Full host-system authority label for system JSON FFI wrappers.
system JSON FFI 包装层使用的完整宿主系统权限标签。
 */
export const SKILL_AUTHORITY_SYSTEM: SkillManagementAuthority = "system";

/**
Delegated-tool authority label for user-facing system JSON FFI wrappers.
面向用户的 system JSON FFI 包装层使用的委托工具权限标签。
 */
export const SKILL_AUTHORITY_DELEGATED_TOOL: SkillManagementAuthority = "delegated_tool";

/**
Stable package id used only by the repository's standard FFI fixture.
仅供仓库标准 FFI 夹具使用的稳定包标识。
 */
const STANDARD_FIXTURE_SYSTEM_PACKAGE_ID = "ffi-demo-system-plugin";

/**
Stable JSON result value returned by generic JSON FFI helper calls.
通用 JSON FFI 辅助调用返回的稳定 JSON 结果值。
 */
export type JsonValue = unknown;

/**
Owned byte-buffer ABI shape returned by JSON FFI functions.
JSON FFI 函数返回的拥有型字节缓冲 ABI 结构。
 */
const FfiOwnedBuffer = koffi.struct("FfiOwnedBuffer", {
  ptr: "void *",
  len: "size_t",
});

/**
Borrowed byte-buffer ABI shape passed into JSON FFI functions.
传入 JSON FFI 函数的借用型字节缓冲 ABI 结构。
 */
const FfiBorrowedBuffer = koffi.struct("FfiBorrowedBuffer", {
  ptr: "void *",
  len: "size_t",
});

/**
Resolve the dynamic library path from one explicit environment variable.
从一个显式环境变量解析动态库路径。
 */
export function resolveLibraryPath(): string {
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
export function resolveStandardFixtureRuntimeRoot(): string {
  const currentFile = fileURLToPath(import.meta.url);
  return path.join(path.dirname(path.dirname(currentFile)), "standard_runtime", "runtime_root");
}

/**
Ensure the shared standard-ABI fixture runtime directory layout exists.
确保标准 ABI 共用夹具运行时目录结构存在。
 */
export function ensureStandardFixtureLayout(root: string): void {
  for (const relativePath of [
    "skills",
    "dependencies",
    path.join("dependencies", "runtimes"),
    path.join("dependencies", "envs"),
    "state",
    "databases",
    "temp",
    "resources",
    "lua_packages",
    "system_lua_lib",
    path.join("bin", "tools"),
    "libs",
  ]) {
    fs.mkdirSync(path.join(root, relativePath), { recursive: true });
  }
  // Canonical System Plugin package root used by system lease demos.
  // system 租约示例使用的规范 System Plugin 包根目录。
  const packageRoot = path.join(root, "system_lua_lib", STANDARD_FIXTURE_SYSTEM_PACKAGE_ID);
  fs.mkdirSync(packageRoot, { recursive: true });
  // Required package-relative manifest kept deliberately empty for the generic fixture.
  // 通用夹具所需的包相对清单，内容刻意保持为空依赖。
  const dependenciesFile = path.join(packageRoot, "dependencies.yaml");
  if (!fs.existsSync(dependenciesFile)) {
    fs.writeFileSync(dependenciesFile, "{}\n", "utf8");
  }
}

/**
Small JSON FFI adapter that owns buffer decoding and envelope validation.
负责缓冲解码与包络校验的小型 JSON FFI 适配器。
 */
export class JsonFfiClient {
  /**
  Bind one loaded luaskills dynamic library and the owned-buffer ABI shape.
  绑定一个已加载的 luaskills 动态库以及拥有型缓冲 ABI 结构。
   */
  private describeCache: JsonMap | null = null;

  constructor(private readonly library: koffi.IKoffiLib) {}

  /**
  Call one JSON FFI function with one payload and return the decoded result body.
  使用一个载荷调用单个 JSON FFI 函数并返回已解码的结果体。
   */
  call(functionName: string, payload: JsonMap): JsonMap {
    const result = this.callValue(functionName, payload);
    if (!result || typeof result !== "object" || Array.isArray(result)) {
      throw new Error("JSON FFI result body must be one object");
    }
    return result as JsonMap;
  }

  /**
  Call one JSON FFI function with one payload and return the decoded result value.
  使用一个载荷调用单个 JSON FFI 函数并返回已解码的结果值。
   */
  callValue(functionName: string, payload: JsonMap): JsonValue {
    const ffiFunction = this.library.func(
      `FfiOwnedBuffer ${functionName}(FfiBorrowedBuffer input_json)`,
    ) as (inputJson: { ptr: Buffer | null; len: number }) => {
      ptr: Buffer | null;
      len: number | bigint;
    };
    const request = makeBorrowedBuffer(JSON.stringify(payload));
    return this.decodeOwnedJsonBuffer(ffiFunction(request.buffer));
  }

  /**
  Call one no-argument JSON FFI function and return the decoded result value.
  调用一个无参数 JSON FFI 函数并返回解码后的结果值。
   */
  callWithoutInputValue(functionName: string): JsonValue {
    // Exact void prototype required by descriptor-style exports.
    // 描述符类导出所要求的精确 void 原型。
    const ffiFunction = this.library.func(
      `FfiOwnedBuffer ${functionName}(void)`,
    ) as () => { ptr: Buffer | null; len: number | bigint };
    return this.decodeOwnedJsonBuffer(ffiFunction());
  }

  /**
  Free one owned buffer returned by the luaskills JSON ABI.
  释放一个由 luaskills JSON ABI 返回的拥有型缓冲。
   */
  freeOwnedBuffer(value: { ptr: Buffer | null; len: number | bigint }): void {
    const freeBuffer = this.library.func(
      "void luaskills_ffi_buffer_free(FfiOwnedBuffer value)",
    ) as (buffer: { ptr: Buffer | null; len: number | bigint }) => void;
    freeBuffer(value);
  }

  /**
  Decode one owned JSON buffer returned by one `_json` FFI call and free it.
  解码一个由 `_json` FFI 调用返回的拥有型 JSON 缓冲并释放。
   */
  private decodeOwnedJsonBuffer(buffer: {
    ptr: Buffer | null;
    len: number | bigint;
  }): JsonValue {
    if (!buffer.ptr && Number(buffer.len) !== 0) {
      throw new Error("JSON FFI returned one null pointer with non-zero len");
    }
    const payloadText = readUtf8Pointer(buffer.ptr, buffer.len);
    if (buffer.ptr) {
      this.freeOwnedBuffer(buffer);
    }
    const envelope = JSON.parse(payloadText || "{}") as {
      ok?: boolean;
      error?: string;
      result?: JsonValue;
    };
    if (envelope.ok !== true) {
      throw new Error(envelope.error || "Unknown JSON FFI error");
    }
    return envelope.result;
  }

  /**
  Read and cache the exported JSON FFI descriptor payload for diagnostics.
  读取并缓存已导出 JSON FFI 描述载荷，供诊断使用。
   */
  describe(): JsonMap {
    if (this.describeCache === null) {
      // Descriptor export has no input buffer, unlike request-taking `_json` functions.
      // 描述符导出没有输入缓冲，不同于接收请求的 `_json` 函数。
      const described = this.callWithoutInputValue("luaskills_ffi_describe_json");
      if (!described || typeof described !== "object" || Array.isArray(described)) {
        throw new Error("JSON FFI descriptor body must be one object");
      }
      this.describeCache = described as JsonMap;
    }
    return this.describeCache;
  }

  /**
  Resolve one host-shared managed interpreter through the public read-only JSON FFI endpoint.
  通过公共只读 JSON FFI 入口解析一个由宿主共享的受管解释器。

  @param distributionRoot Existing absolute root that directly contains python and node.
  distributionRoot 参数：直接包含 python 与 node 的现有绝对根。
  @param runtime Exact managed interpreter family.
  runtime 参数：精确受管解释器类型。
  @param version Exact semantic runtime version.
  version 参数：精确语义化运行时版本。
  @param platform Exact normalized LuaSkills platform key.
  platform 参数：精确规范化 LuaSkills 平台键。
  @returns Canonical installation paths and SHA-256 identities validated by LuaSkills.
  返回值：LuaSkills 校验后的规范安装路径与 SHA-256 身份。
   */
  resolveManagedRuntimeInstall(
    distributionRoot: string,
    runtime: "python" | "node",
    version: string,
    platform: string,
  ): JsonMap {
    if (!path.isAbsolute(distributionRoot)) {
      throw new Error("distributionRoot must be an absolute path");
    }
    return this.call("luaskills_ffi_managed_runtime_resolve_json", {
      distribution_root: path.resolve(distributionRoot),
      runtime,
      version,
      platform,
    });
  }
}

/**
Shared host wrapper around the repository standard-runtime fixture root.
面向仓库 standard-runtime 夹具根目录的共享宿主包装器。
 */
export class StandardFixtureRuntimeClient {
  /**
  Bind one JSON client to one runtime root and ensure the fixture layout exists.
  将一个 JSON 客户端绑定到一个运行时根目录，并确保夹具目录结构存在。
   */
  constructor(
    private readonly client: JsonFfiClient,
    readonly runtimeRoot: string,
  ) {
    ensureStandardFixtureLayout(runtimeRoot);
  }

  /**
  Create one runtime engine configured for the shared fixture root.
  创建一个面向共享夹具根目录配置好的运行时引擎。
   */
  createEngine(
    defaultTextEncoding: string | null = "utf-8",
    enableManagedIoCompat = true,
  ): number {
    const payload = this.client.call(
      "luaskills_ffi_engine_new_json",
      this.buildEngineRequest(defaultTextEncoding, enableManagedIoCompat),
    );
    const engineId = payload.engine_id;
    return requireJsonSafeInteger(engineId, "engine_new_json engine_id", 1);
  }

  /**
  Load the shared skill root into one existing engine.
  把共享技能根加载进一个已有引擎。
   */
  loadRoot(engineId: number, rootName = "ROOT"): void {
    this.client.call("luaskills_ffi_load_from_roots_json", {
      engine_id: requireJsonSafeInteger(engineId, "engine_id", 1),
      skill_roots: [
        {
          name: rootName,
          skills_dir: path.join(this.runtimeRoot, "skills"),
        },
      ],
    });
  }

  /**
  Free one previously created runtime engine.
  释放一个先前创建的运行时引擎。
   */
  freeEngine(engineId: number): void {
    this.client.call("luaskills_ffi_engine_free_json", {
      engine_id: requireJsonSafeInteger(engineId, "engine_id", 1),
    });
  }

  /**
  Build one plain runtime-lease client that targets the public JSON FFI endpoints.
  构造一个指向公共 JSON FFI 入口的普通运行时租约客户端。
   */
  runtimeLeases(engineId: number): RuntimeLeaseClient {
    return new RuntimeLeaseClient(this.client, engineId);
  }

  /**
  Build one authority-bound runtime-lease client that targets the system JSON FFI endpoints.
  构造一个指向 system JSON FFI 入口并绑定 authority 的运行时租约客户端。
   */
  systemRuntimeLeases(
    engineId: number,
    authority: SkillManagementAuthority = SKILL_AUTHORITY_DELEGATED_TOOL,
  ): RuntimeLeaseClient {
    return new RuntimeLeaseClient(
      this.client,
      engineId,
      authority,
      this.fixtureSystemPackage(),
    );
  }

  /**
  Build one authority-bound managed-session event client for the target engine.
  为目标引擎构造一个绑定 authority 的受管会话事件客户端。

  @param engineId Stable numeric FFI engine handle.
  engineId 参数：稳定数值 FFI 引擎句柄。
  @param authority Host-injected authority forwarded to every event request.
  authority 参数：转发到每个事件请求的宿主注入权限。
  @returns One authority-bound managed-session event client.
  返回值：一个绑定权限的受管会话事件客户端。
   */
  managedSessionEvents(
    engineId: number,
    authority: SkillManagementAuthority = SKILL_AUTHORITY_DELEGATED_TOOL,
  ): ManagedSessionEventsClient {
    return new ManagedSessionEventsClient(this.client, engineId, authority);
  }

  /**
  Build the ordered fixture skill-root chain shared by the standard-runtime demos.
  构造 standard-runtime 示例共用的有序夹具技能根链。
   */
  fixtureSkillRoots(rootName = "ROOT"): JsonMap[] {
    return [
      buildRuntimeSkillRoot(
        rootName,
        path.join(this.runtimeRoot, "skills"),
      ),
    ];
  }

  /**
  Build one authority-bound engine helper that wraps system JSON FFI entrypoints.
  构造一个封装 system JSON FFI 入口并绑定 authority 的引擎辅助器。
   */
  systemClient(
    engineId: number,
    authority: SkillManagementAuthority = SKILL_AUTHORITY_DELEGATED_TOOL,
    rootName = "ROOT",
  ): SystemEngineJsonClient {
    return new SystemEngineJsonClient(
      this.client,
      engineId,
      authority,
      this.fixtureSkillRoots(rootName),
      this.fixtureSystemPackage(),
    );
  }

  /**
  Build the strict System Plugin descriptor used by repository fixture leases.
  构造仓库夹具租约使用的严格 System Plugin 描述符。
   */
  fixtureSystemPackage(): SystemRuntimePackageDescriptor {
    // Canonical fixture package directory created by ensureStandardFixtureLayout.
    // 由 ensureStandardFixtureLayout 创建的规范夹具包目录。
    const packageRoot = path.resolve(
      this.runtimeRoot,
      "system_lua_lib",
      STANDARD_FIXTURE_SYSTEM_PACKAGE_ID,
    );
    return {
      id: STANDARD_FIXTURE_SYSTEM_PACKAGE_ID,
      root: packageRoot,
      dependencies_file: "dependencies.yaml",
    };
  }

  /**
  Build one JSON engine creation request for the fixture runtime root.
  为夹具运行时根构造一个 JSON 引擎创建请求。
   */
  private buildEngineRequest(
    defaultTextEncoding: string | null,
    enableManagedIoCompat: boolean,
  ): JsonMap {
    return {
      options: {
        pool_config: {
          min_size: 1,
          max_size: 1,
          idle_ttl_secs: 30,
        },
        host_options: {
          runtime_root: path.resolve(this.runtimeRoot),
          managed_runtime_distribution_root: path.join(this.runtimeRoot, "dependencies", "runtimes"),
          managed_runtime_environment_root: path.join(this.runtimeRoot, "dependencies", "envs"),
          // ManagedRuntimeConfig explicitly demonstrates the stable B3-B7 JSON engine defaults.
          // ManagedRuntimeConfig 显式演示稳定的 B3-B7 JSON 引擎默认值。
          managed_runtime_config: {
            worker_pool_max_size_per_environment: 4,
            worker_idle_ttl_secs: 60,
            persistent_session_limit_per_engine: 256,
            persistent_session_default_buffer_limit_bytes_per_stream: 1024 * 1024,
            invoke_default_timeout_ms: null,
          },
          temp_dir: path.join(this.runtimeRoot, "temp"),
          resources_dir: path.join(this.runtimeRoot, "resources"),
          lua_packages_dir: path.join(this.runtimeRoot, "lua_packages"),
          host_provided_tool_root: path.join(this.runtimeRoot, "bin", "tools"),
          host_provided_lua_root: path.join(this.runtimeRoot, "lua_packages"),
          host_provided_ffi_root: path.join(this.runtimeRoot, "libs"),
          system_lua_lib_dir: path.join(this.runtimeRoot, "system_lua_lib"),
          download_cache_root: path.join(this.runtimeRoot, "temp", "downloads"),
          dependency_dir_name: "dependencies",
          state_dir_name: "state",
          database_dir_name: "databases",
          skill_config_root: null,
          skill_config_lock_timeout_ms: null,
          skill_config_watch_debounce_ms: null,
          allow_network_download: false,
          github_base_url: null,
          github_api_base_url: null,
          default_text_encoding: defaultTextEncoding,
          sqlite_library_path: null,
          sqlite_provider_mode: "dynamic_library",
          sqlite_callback_mode: "standard",
          lancedb_library_path: null,
          lancedb_provider_mode: "dynamic_library",
          lancedb_callback_mode: "standard",
          cache_config: null,
          runlua_pool_config: null,
          reserved_entry_names: [],
          ignored_skill_ids: [],
          capabilities: {
            enable_skill_management_bridge: false,
            enable_managed_io_compat: enableManagedIoCompat,
          },
        },
      },
    };
  }
}

/**
Stateful host helper that wraps one engine's runtime-lease JSON API.
包装单个引擎 runtime-lease JSON API 的有状态宿主辅助器。
 */
export class RuntimeLeaseClient {
  /**
  Bind one JSON client to one existing engine id.
  将一个 JSON 客户端绑定到一个已有引擎标识。
   */
  constructor(
    private readonly client: JsonFfiClient,
    private readonly engineId: number,
    private readonly systemToolAuthority?: SkillManagementAuthority,
    private readonly systemPackage?: SystemRuntimePackageDescriptor,
  ) {
    requireJsonSafeInteger(engineId, "engine_id", 1);
  }

  /**
  Dispatch one raw runtime-lease JSON request without applying success checks.
  分发单个原始运行时租约 JSON 请求而不附加成功校验。
   */
  callRaw(action: string, payload: JsonMap): JsonMap {
    const requestPayload: JsonMap = {
      ...payload,
      engine_id: this.engineId,
    };
    if (this.systemToolAuthority !== undefined) {
      requestPayload.authority = this.systemToolAuthority;
    }
    return this.client.call(this.runtimeLeaseFunctionName(action), requestPayload);
  }

  /**
  Create or replace one persistent runtime lease.
  创建或替换一个持久运行时租约。
   */
  create(
    sid: string,
    ttlSec = 600,
    replace = false,
    context: SystemRuntimeLeaseContext = {},
  ): JsonMap {
    // Base fields shared by public and System lease creation.
    // 公共与 System 租约创建共享的基础字段。
    const request: JsonMap = {
      sid,
      ttl_sec: requireJsonSafeInteger(
        ttlSec,
        "ttl_sec",
        this.systemToolAuthority === undefined ? 1 : 0,
      ),
      replace,
    };
    if (this.systemToolAuthority !== undefined) {
      if (this.systemPackage === undefined) {
        throw new Error("system runtime lease creation requires system_package");
      }
      request.system_package = this.systemPackage;
    }
    if (context.mounts !== undefined) {
      request.mounts = context.mounts;
    } else if (this.systemToolAuthority !== undefined) {
      request.mounts = {};
    }
    if (context.cwd !== undefined) {
      request.cwd = context.cwd;
    }
    if (context.workspace_root !== undefined) {
      request.workspace_root = context.workspace_root;
    }
    return requireRuntimeLeaseOK(
      this.callRaw("create", request),
      "runtime lease create",
    );
  }

  /**
  Create one runtime-lease handle object from a fresh create response.
  基于新的 create 响应创建一个运行时租约句柄对象。
   */
  createHandle(
    sid: string,
    ttlSec = 600,
    replace = false,
    context: SystemRuntimeLeaseContext = {},
  ): RuntimeLeaseHandle {
    return RuntimeLeaseHandle.fromPayload(
      this,
      this.create(sid, ttlSec, replace, context),
    );
  }

  /**
  Rebuild one runtime-lease handle object from one persisted payload.
  基于一份已持久化载荷重建一个运行时租约句柄对象。
   */
  bindHandle(payload: JsonMap): RuntimeLeaseHandle {
    return RuntimeLeaseHandle.fromPayload(this, payload);
  }

  /**
  Evaluate one Lua chunk inside one persistent runtime lease.
  在一个持久运行时租约中执行单个 Lua 代码块。
   */
  eval(
    leaseId: string,
    code: string,
    args: JsonMap = {},
    timeoutMs = 60_000,
    sid?: string,
    generation?: number,
  ): JsonMap {
    const payload: JsonMap = {
      lease_id: leaseId,
      timeout_ms: requireJsonSafeInteger(timeoutMs, "timeout_ms", 1),
      args,
      code,
    };
    if (sid !== undefined) {
      payload.sid = sid;
    }
    if (generation !== undefined) {
      payload.generation = requireJsonSafeInteger(generation, "generation");
    }
    return requireRuntimeLeaseOK(
      this.callRaw("eval", payload),
      "runtime lease eval",
    );
  }

  /**
  Read one runtime lease status payload with optional identity guards.
  读取单个运行时租约状态载荷，并可附带可选身份护栏。
   */
  status(leaseId: string, sid?: string, generation?: number): JsonMap {
    const payload: JsonMap = {
      lease_id: leaseId,
    };
    if (sid !== undefined) {
      payload.sid = sid;
    }
    if (generation !== undefined) {
      payload.generation = requireJsonSafeInteger(generation, "generation");
    }
    return this.callRaw("status", payload);
  }

  /**
  List active runtime leases and optionally filter by one SID.
  列出活跃运行时租约，并可按单个 SID 过滤。
   */
  list(sid?: string): JsonMap {
    return this.callRaw("list", {
      sid: sid ?? null,
    });
  }

  /**
  List active runtime-lease handles rebuilt from the current lease listing payload.
  基于当前租约列表载荷重建活跃运行时租约句柄列表。
   */
  listHandles(sid?: string): RuntimeLeaseHandle[] {
    const payload = this.list(sid);
    const leases = payload.leases;
    if (!Array.isArray(leases)) {
      throw new Error("runtime lease list payload is missing the leases array");
    }
    return leases.map((lease) => RuntimeLeaseHandle.fromPayload(this, lease as JsonMap));
  }

  /**
  Return the first active runtime-lease handle for one SID when present.
  返回某个 SID 的第一个活跃运行时租约句柄（如果存在）。
   */
  findHandle(sid: string): RuntimeLeaseHandle | null {
    const handles = this.listHandles(sid);
    return handles.length > 0 ? handles[0] : null;
  }

  /**
  Close one runtime lease and return its final status payload with optional identity guards.
  关闭单个运行时租约并返回其最终状态载荷，并可附带可选身份护栏。
   */
  close(leaseId: string, sid?: string, generation?: number): JsonMap {
    const payload: JsonMap = {
      lease_id: leaseId,
    };
    if (sid !== undefined) {
      payload.sid = sid;
    }
    if (generation !== undefined) {
      payload.generation = requireJsonSafeInteger(generation, "generation");
    }
    return this.callRaw("close", payload);
  }

  /**
  Resolve the concrete runtime-lease JSON FFI entrypoint name for one logical action.
  为单个逻辑动作解析具体的运行时租约 JSON FFI 入口名称。
   */
  private runtimeLeaseFunctionName(action: string): string {
    if (!this.systemToolAuthority) {
      return `luaskills_ffi_runtime_lease_${action}_json`;
    }
    return `luaskills_ffi_system_runtime_lease_${action}_json`;
  }

  /**
  Return whether this helper will dispatch runtime-lease requests to dedicated system entrypoints.
  返回当前辅助器是否会把运行时租约请求分发到专用 system 入口。
   */
  usesSystemRuntimeLeaseEndpoints(): boolean {
    return this.systemToolAuthority !== undefined;
  }

}

/**
Authority-bound JSON client for engine-level System managed-session events.
面向引擎级 System 受管会话事件、绑定 authority 的 JSON 客户端。
 */
export class ManagedSessionEventsClient {
  /**
  Bind one JSON client, engine id, and host-injected authority.
  绑定一个 JSON 客户端、引擎标识与宿主注入的 authority。

  @param client Low-level JSON FFI transport.
  client 参数：底层 JSON FFI 传输器。
  @param engineId Stable numeric FFI engine handle.
  engineId 参数：稳定数值 FFI 引擎句柄。
  @param authority Host-injected authority required by the event surface.
  authority 参数：事件接口要求的宿主注入权限。
   */
  constructor(
    private readonly client: JsonFfiClient,
    private readonly engineId: number,
    private readonly authority: SkillManagementAuthority,
  ) {
    requireJsonSafeInteger(engineId, "engine_id", 1);
  }

  /**
  Destructively poll at most maxEvents ready events without waiting.
  无等待地破坏性轮询最多 maxEvents 个就绪事件。

  @param maxEvents Positive maximum number of events removed from the queue.
  maxEvents 参数：从队列移除的正数事件数量上限。
  @returns One validated immediate event batch.
  返回值：一个经过校验的即时事件批次。
   */
  poll(maxEvents = 64): ManagedSessionEventBatch {
    return requireManagedSessionEventBatch(
      this.client.call("luaskills_ffi_managed_session_events_poll_json", {
        engine_id: this.engineId,
        max_events: requireJsonSafeInteger(maxEvents, "max_events", 1),
        authority: this.authority,
      }),
    );
  }

  /**
  Destructively wait for at most maxEvents events until timeoutMs expires.
  破坏性等待最多 maxEvents 个事件，直至 timeoutMs 到期。

  @param maxEvents Positive maximum number of events removed from the queue.
  maxEvents 参数：从队列移除的正数事件数量上限。
  @param timeoutMs Finite wait duration in milliseconds; zero is nonblocking.
  timeoutMs 参数：有限等待毫秒数；零表示非阻塞。
  @returns One validated event or timeout batch.
  返回值：一个经过校验的事件批次或超时批次。
   */
  wait(maxEvents = 64, timeoutMs = 1_000): ManagedSessionEventBatch {
    return requireManagedSessionEventBatch(
      this.client.call("luaskills_ffi_managed_session_events_wait_json", {
        engine_id: this.engineId,
        max_events: requireJsonSafeInteger(maxEvents, "max_events", 1),
        timeout_ms: requireJsonSafeInteger(timeoutMs, "timeout_ms"),
        authority: this.authority,
      }),
    );
  }
}

/**
Authority-bound helper that wraps one engine's system JSON FFI entrypoints.
封装单个引擎 system JSON FFI 入口并绑定 authority 的辅助器。
 */
export class SystemEngineJsonClient {
  /**
  Bind one JSON client, engine id, authority, and optional default skill-root chain.
  绑定一个 JSON 客户端、引擎标识、authority 与可选默认技能根链。
   */
  constructor(
    private readonly client: JsonFfiClient,
    private readonly engineId: number,
    private readonly authority: SkillManagementAuthority,
    private readonly defaultSkillRoots: JsonMap[] = [],
    private readonly defaultSystemPackage?: SystemRuntimePackageDescriptor,
  ) {
    requireJsonSafeInteger(engineId, "engine_id", 1);
  }

  /**
  Call one system JSON FFI function and require an object-shaped result payload.
  调用单个 system JSON FFI 函数并要求返回对象形状的结果载荷。
   */
  call(functionName: string, payload: JsonMap = {}): JsonMap {
    return this.client.call(functionName, this.withEngineAuthority(payload));
  }

  /**
  Call one system JSON FFI function and return any decoded JSON result shape.
  调用单个 system JSON FFI 函数并返回任意已解码 JSON 结果形状。
   */
  callValue(functionName: string, payload: JsonMap = {}): JsonValue {
    return this.client.callValue(functionName, this.withEngineAuthority(payload));
  }

  /**
  Build one authority-bound runtime-lease helper under the current engine wrapper.
  在当前引擎包装器下构造一个绑定 authority 的运行时租约辅助器。
   */
  runtimeLeases(): RuntimeLeaseClient {
    if (this.defaultSystemPackage === undefined) {
      throw new Error("system runtime lease helper requires system_package");
    }
    return new RuntimeLeaseClient(
      this.client,
      this.engineId,
      this.authority,
      this.defaultSystemPackage,
    );
  }

  /**
  Build one event helper whose poll and wait requests reuse the bound authority.
  构造一个让 poll 与 wait 请求复用当前 authority 的事件辅助器。

  @returns One event client bound to this engine and authority.
  返回值：一个绑定当前引擎与权限的事件客户端。
   */
  managedSessionEvents(): ManagedSessionEventsClient {
    return new ManagedSessionEventsClient(this.client, this.engineId, this.authority);
  }

  /**
  List runtime entries visible to the bound authority.
  列出当前绑定 authority 可见的运行时入口。
   */
  listEntries(): JsonMap[] {
    const result = this.callValue("luaskills_ffi_list_entries_json");
    if (!Array.isArray(result)) {
      throw new Error("list_entries_json did not return one array result");
    }
    return result.filter(isJsonMap);
  }

  /**
  List skill help trees visible to the bound authority.
  列出当前绑定 authority 可见的技能帮助树。
   */
  listSkillHelp(): JsonMap[] {
    const result = this.callValue("luaskills_ffi_list_skill_help_json");
    if (!Array.isArray(result)) {
      throw new Error("list_skill_help_json did not return one array result");
    }
    return result.filter(isJsonMap);
  }

  /**
  Render one help-detail payload visible to the bound authority.
  渲染当前绑定 authority 可见的一份帮助详情载荷。
   */
  renderSkillHelpDetail(
    skillId: string,
    flowName: string,
    requestContext?: JsonMap,
  ): JsonMap | null {
    const payload: JsonMap = {
      skill_id: skillId,
      flow_name: flowName,
    };
    if (requestContext !== undefined) {
      payload.request_context = requestContext;
    }
    const result = this.callValue("luaskills_ffi_render_skill_help_detail_json", payload);
    if (result === null || result === undefined) {
      return null;
    }
    if (!isJsonMap(result)) {
      throw new Error("render_skill_help_detail_json did not return one object result");
    }
    return result;
  }

  /**
  Read prompt-argument completion candidates visible to the bound authority.
  读取当前绑定 authority 可见的提示词参数补全候选项。
   */
  promptArgumentCompletions(promptName: string, argumentName: string): string[] | null {
    const result = this.callValue("luaskills_ffi_prompt_argument_completions_json", {
      prompt_name: promptName,
      argument_name: argumentName,
    });
    if (result === null || result === undefined) {
      return null;
    }
    if (!Array.isArray(result)) {
      throw new Error("prompt_argument_completions_json did not return one array result");
    }
    return result.filter((value): value is string => typeof value === "string");
  }

  /**
  Return whether one tool name resolves to one visible Lua skill entry.
  返回某个工具名是否解析为一个可见 Lua 技能入口。
   */
  isSkill(toolName: string): boolean {
    const result = this.call("luaskills_ffi_is_skill_json", {
      tool_name: toolName,
    });
    if (typeof result.value !== "boolean") {
      throw new Error("is_skill_json did not return one boolean value field");
    }
    return result.value;
  }

  /**
  Resolve the visible owning skill id for one tool name when available.
  在可见时解析某个工具名所属的技能标识。
   */
  skillNameForTool(toolName: string): string | null {
    const result = this.call("luaskills_ffi_skill_name_for_tool_json", {
      tool_name: toolName,
    });
    if (result.skill_id === null || result.skill_id === undefined) {
      return null;
    }
    if (typeof result.skill_id !== "string") {
      throw new Error("skill_name_for_tool_json did not return a nullable string field");
    }
    return result.skill_id;
  }

  /**
  Disable one skill through the system JSON FFI surface.
  通过 system JSON FFI 入口停用单个技能。
   */
  disableSkill(skillId: string, reason?: string, skillRoots?: JsonMap[]): JsonMap {
    const payload: JsonMap = {
      skill_roots: this.resolveSkillRoots(skillRoots),
      skill_id: skillId,
    };
    if (reason !== undefined) {
      payload.reason = reason;
    }
    return this.call("luaskills_ffi_system_disable_skill_json", payload);
  }

  /**
  Enable one skill through the system JSON FFI surface.
  通过 system JSON FFI 入口启用单个技能。
   */
  enableSkill(skillId: string, skillRoots?: JsonMap[]): JsonMap {
    return this.call("luaskills_ffi_system_enable_skill_json", {
      skill_roots: this.resolveSkillRoots(skillRoots),
      skill_id: skillId,
    });
  }

  /**
  Uninstall one skill through the system JSON FFI surface.
  通过 system JSON FFI 入口卸载单个技能。
   */
  uninstallSkill(
    skillId: string,
    skillRoots?: JsonMap[],
    targetRoot?: JsonMap,
    options: JsonMap = {},
  ): JsonMap {
    const payload: JsonMap = {
      skill_roots: this.resolveSkillRoots(skillRoots),
      skill_id: skillId,
      options,
    };
    if (targetRoot !== undefined) {
      payload.target_root = targetRoot;
    }
    return this.call("luaskills_ffi_system_uninstall_skill_json", payload);
  }

  /**
  Install one managed skill through the system JSON FFI surface.
  通过 system JSON FFI 入口安装单个受管技能。
   */
  installSkill(request: JsonMap, skillRoots?: JsonMap[], targetRoot?: JsonMap): JsonMap {
    const payload: JsonMap = {
      skill_roots: this.resolveSkillRoots(skillRoots),
      request,
    };
    if (targetRoot !== undefined) {
      payload.target_root = targetRoot;
    }
    return this.call("luaskills_ffi_system_install_skill_json", payload);
  }

  /**
  Update one managed skill through the system JSON FFI surface.
  通过 system JSON FFI 入口更新单个受管技能。
   */
  updateSkill(request: JsonMap, skillRoots?: JsonMap[], targetRoot?: JsonMap): JsonMap {
    const payload: JsonMap = {
      skill_roots: this.resolveSkillRoots(skillRoots),
      request,
    };
    if (targetRoot !== undefined) {
      payload.target_root = targetRoot;
    }
    return this.call("luaskills_ffi_system_update_skill_json", payload);
  }

  /**
  Attach the bound engine id and authority to one outgoing system JSON payload.
  为单个发出的 system JSON 载荷附加已绑定的引擎标识与 authority。
   */
  private withEngineAuthority(payload: JsonMap): JsonMap {
    return {
      ...payload,
      engine_id: this.engineId,
      authority: this.authority,
    };
  }

  /**
  Resolve the skill-root chain for one system lifecycle request.
  解析单个 system 生命周期请求使用的技能根链。
   */
  private resolveSkillRoots(skillRoots?: JsonMap[]): JsonMap[] {
    const resolved = skillRoots ?? this.defaultSkillRoots;
    if (resolved.length === 0) {
      throw new Error("system helper requires one explicit or default skill_roots chain");
    }
    return [...resolved];
  }
}

/**
Stable host-side runtime-lease handle that carries lease identity guards automatically.
自动携带租约身份护栏的稳定宿主侧运行时租约句柄。
 */
export class RuntimeLeaseHandle {
  /**
  Bind one session client to one concrete lease identity triplet.
  将一个会话客户端绑定到一个具体的租约身份三元组。
   */
  constructor(
    private readonly sessions: RuntimeLeaseClient,
    readonly leaseId: string,
    readonly sid: string,
    readonly generation: number,
  ) {}

  /**
  Construct one runtime-lease handle from one JSON payload that contains identity fields.
  从包含身份字段的一份 JSON 载荷中构造一个运行时租约句柄。
   */
  static fromPayload(sessions: RuntimeLeaseClient, payload: JsonMap): RuntimeLeaseHandle {
    return new RuntimeLeaseHandle(
      sessions,
      requireRuntimeLeaseStringField(payload, "lease_id"),
      requireRuntimeLeaseStringField(payload, "sid"),
      requireRuntimeLeaseNumberField(payload, "generation"),
    );
  }

  /**
  Export the stable lease identity fields for persistence or raw FFI calls.
  导出稳定租约身份字段，供持久化或原始 FFI 调用使用。
   */
  identityPayload(): RuntimeLeaseIdentity {
    return {
      lease_id: this.leaseId,
      sid: this.sid,
      generation: this.generation,
    };
  }

  /**
  Evaluate Lua code while automatically attaching the stored lease identity guards.
  执行 Lua 代码时自动附带已保存的租约身份护栏。
   */
  eval(code: string, args: JsonMap = {}, timeoutMs = 60_000): JsonMap {
    return this.sessions.eval(
      this.leaseId,
      code,
      args,
      timeoutMs,
      this.sid,
      this.generation,
    );
  }

  /**
  Read the current lease status while automatically attaching the stored identity guards.
  读取当前租约状态时自动附带已保存的身份护栏。
   */
  status(): JsonMap {
    return this.sessions.status(this.leaseId, this.sid, this.generation);
  }

  /**
  Close the current lease while automatically attaching the stored identity guards.
  关闭当前租约时自动附带已保存的身份护栏。
   */
  close(): JsonMap {
    return this.sessions.close(this.leaseId, this.sid, this.generation);
  }
}

/**
Require one runtime-lease payload to report success.
要求单个运行时租约载荷报告成功。
 */
export function requireRuntimeLeaseOK(payload: JsonMap, action: string): JsonMap {
  if (payload.ok === true) {
    return payload;
  }
  throw new Error(
    `${action} failed: ${String(payload.error_code || "unknown")}: ${String(payload.message || "Unknown runtime lease error")}`,
  );
}

/**
Read one required runtime-lease string field from one JSON payload.
从一份 JSON 载荷中读取一个必填的运行时租约字符串字段。
 */
export function requireRuntimeLeaseStringField(payload: JsonMap, fieldName: string): string {
  const value = payload[fieldName];
  if (typeof value === "string" && value.length > 0) {
    return value;
  }
  throw new Error(`runtime lease payload is missing required string field: ${fieldName}`);
}

/**
Read one required runtime-lease integer field from one JSON payload.
从一份 JSON 载荷中读取一个必填的运行时租约整数字段。
 */
export function requireRuntimeLeaseNumberField(payload: JsonMap, fieldName: string): number {
  return requireJsonSafeInteger(payload[fieldName], fieldName);
}

/**
Validate one JSON integer before it crosses a number-only JSON FFI boundary.
在单个整数跨越仅支持 number 的 JSON FFI 边界前进行校验。

Parameters: `value` is the decoded or caller-supplied value, `label` names diagnostics, and
`minimum` is the inclusive lower bound.
参数：`value` 是解码或调用方提供的值，`label` 用于诊断命名，`minimum` 是包含端点的下界。

Returns the unchanged safely representable JSON integer.
返回未改变且可安全表示的 JSON 整数。
 */
function requireJsonSafeInteger(
  value: unknown,
  label: string,
  minimum = 0,
): number {
  if (
    typeof value === "number"
    && Number.isSafeInteger(value)
    && value >= minimum
  ) {
    return value;
  }
  throw new Error(
    `${label} must be one safe integer greater than or equal to ${minimum}`,
  );
}

/**
Validate and return one managed-session event batch from the JSON FFI result.
校验并返回 JSON FFI 结果中的单个受管会话事件批次。

Parameters: `payload` is the decoded `_json` result object.
参数：`payload` 是已解码的 `_json` 结果对象。

Returns a fully shape-validated managed-session event batch.
返回一个已完成全部结构校验的受管会话事件批次。
 */
function requireManagedSessionEventBatch(payload: JsonMap): ManagedSessionEventBatch {
  // Raw event list validated before the payload is exposed through the typed wrapper.
  // 在通过类型化包装层暴露载荷前完成校验的原始事件列表。
  const events = payload.events;
  // Queue depth remaining after the destructive drain.
  // 破坏性排空后剩余的队列深度。
  const remaining = payload.remaining;
  // Explicit wait-timeout marker distinguished from an ordinary empty poll.
  // 与普通空轮询区分的显式等待超时标记。
  const timedOut = payload.timed_out;
  if (
    !Array.isArray(events)
    || typeof remaining !== "number"
    || !Number.isSafeInteger(remaining)
    || remaining < 0
    || typeof timedOut !== "boolean"
  ) {
    throw new Error("managed-session event result has an invalid batch shape");
  }
  for (const event of events) {
    if (!isJsonMap(event) || !isManagedSessionEvent(event)) {
      throw new Error("managed-session event result contains an invalid event");
    }
  }
  return {
    events: events as ManagedSessionEvent[],
    remaining,
    timed_out: timedOut,
  };
}

/**
Return whether one JSON object satisfies the stable managed-session event shape.
返回一个 JSON 对象是否满足稳定的受管会话事件结构。

Parameters: `event` is one decoded candidate event object.
参数：`event` 是一个已解码的候选事件对象。

Returns true only when every identity, kind, and sequence field is valid.
仅当全部身份、类型与序号字段均有效时返回 true。
 */
function isManagedSessionEvent(event: JsonMap): boolean {
  // Stable event kinds admitted by the Rust event protocol.
  // Rust 事件协议允许的稳定事件类型。
  const kinds: readonly ManagedSessionEventKind[] = [
    "stdout_readable",
    "stderr_readable",
    "exited",
    "failed",
  ];
  return (
    typeof event.system_lease_id === "string"
    && typeof event.sid === "string"
    && typeof event.generation === "number"
    && Number.isSafeInteger(event.generation)
    && event.generation >= 0
    && typeof event.managed_session_id === "number"
    && Number.isSafeInteger(event.managed_session_id)
    && event.managed_session_id >= 0
    && typeof event.kind === "string"
    && kinds.includes(event.kind as ManagedSessionEventKind)
    && typeof event.sequence === "number"
    && Number.isSafeInteger(event.sequence)
    && event.sequence >= 0
  );
}

/**
Build one JSON runtime skill-root object for lifecycle and load helpers.
为生命周期与加载辅助函数构造一个 JSON 运行时技能根对象。
 */
export function buildRuntimeSkillRoot(name: string, skillsDir: string): JsonMap {
  return {
    name,
    skills_dir: skillsDir,
  };
}

/**
Return whether one unknown JSON value is one plain object map.
返回某个未知 JSON 值是否为普通对象映射。
 */
function isJsonMap(value: unknown): value is JsonMap {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

/**
Read one external pointer and length as one UTF-8 string through koffi decoding.
通过 koffi 解码把一个外部指针和长度读取为 UTF-8 字符串。
 */
function readUtf8Pointer(ptr: Buffer | null, len: number | bigint): string {
  if (!ptr || Number(len) === 0) {
    return "";
  }
  const bytes = koffi.decode(ptr, koffi.array("uint8_t", Number(len))) as number[];
  return Buffer.from(bytes).toString("utf8");
}

/**
Build one borrowed UTF-8 buffer whose payload stays alive for one JSON FFI call.
构造一个在一次 JSON FFI 调用期间保持有效的借用型 UTF-8 缓冲。
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
