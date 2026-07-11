"""
Reusable JSON FFI runtime helpers for Python host demos.
供 Python 宿主演示复用的 JSON FFI 运行时辅助层。
"""

import ctypes
import json
from pathlib import Path
from typing import Literal, TypedDict, cast

from demo import FfiBorrowedBuffer, FfiOwnedBuffer, ensure_standard_fixture_layout


JsonMap = dict[str, object]
JsonValue = object


class SystemRuntimePackageDescriptor(TypedDict):
    """
    Strict System Plugin package descriptor required by system lease creation.
    system 租约创建所必需的严格 System Plugin 包描述符。
    """

    # Stable package identifier selected by the host.
    # 宿主选择的稳定包标识符。
    id: str
    # Absolute package root strictly below the engine's system_lua_lib directory.
    # 严格位于引擎 system_lua_lib 目录下的绝对包根目录。
    root: str
    # Package-relative dependency manifest path.
    # 包相对依赖清单路径。
    dependencies_file: str


class ManagedSessionEvent(TypedDict):
    """
    One sequenced host-visible event for a System managed child session.
    单个带序号、宿主可见的 System 受管子会话事件。
    """

    # Opaque System lease id that owns the child session.
    # 拥有子会话的不透明 System 租约标识。
    system_lease_id: str
    # Stable host-selected System lease SID.
    # 宿主选择的稳定 System 租约 SID。
    sid: str
    # SID-local lease generation.
    # SID 内部的租约代际。
    generation: int
    # Engine-local id returned by the managed session status.
    # 由受管会话状态返回的引擎内局部标识。
    managed_session_id: int
    # Coalesced readiness or terminal event kind.
    # 合并后的就绪或终止事件类型。
    kind: Literal["stdout_readable", "stderr_readable", "exited", "failed"]
    # Engine event-center-global monotonic sequence.
    # 引擎事件中心全局单调序号。
    sequence: int


class ManagedSessionEventBatch(TypedDict):
    """
    One bounded destructive event-drain result.
    单次有界破坏性事件排空结果。
    """

    # Events removed in sequence order.
    # 按序号顺序移除的事件。
    events: list[ManagedSessionEvent]
    # Pending logical events after this drain.
    # 本次排空后仍待处理的逻辑事件数。
    remaining: int
    # Whether wait returned because its deadline expired without an event.
    # wait 是否因截止时间到期且没有事件而返回。
    timed_out: bool


# Full host-system authority label for system JSON FFI wrappers.
# system JSON FFI 包装层使用的完整宿主系统权限标签。
SKILL_AUTHORITY_SYSTEM = "system"
# Delegated-tool authority label for user-facing system JSON FFI wrappers.
# 面向用户的 system JSON FFI 包装层使用的委托工具权限标签。
SKILL_AUTHORITY_DELEGATED_TOOL = "delegated_tool"
# Stable package id used only by the repository's standard FFI fixture.
# 仅供仓库标准 FFI 夹具使用的稳定包标识。
STANDARD_FIXTURE_SYSTEM_PACKAGE_ID = "ffi-demo-system-plugin"


class JsonFfiClient:
    """
    Small JSON FFI adapter that owns buffer decoding and envelope validation.
    负责缓冲解码与包络校验的小型 JSON FFI 适配器。
    """

    def __init__(self, library: ctypes.CDLL) -> None:
        """
        Capture one loaded luaskills dynamic library for repeated JSON FFI calls.
        捕获一个已加载的 luaskills 动态库以复用 JSON FFI 调用。
        """

        self.library = library
        self._describe_cache: JsonMap | None = None
        self.library.luaskills_ffi_buffer_free.argtypes = [FfiOwnedBuffer]
        self.library.luaskills_ffi_buffer_free.restype = None

    def call(self, function_name: str, payload: JsonMap) -> JsonMap:
        """
        Call one JSON FFI function with one payload and return the decoded result body.
        使用一个载荷调用单个 JSON FFI 函数并返回已解码的结果体。
        """

        result = self.call_value(function_name, payload)
        if not isinstance(result, dict):
            raise RuntimeError("JSON FFI result body must be one object")
        return result

    def call_value(self, function_name: str, payload: JsonMap) -> JsonValue:
        """
        Call one JSON FFI function with one payload and return the decoded result value.
        使用一个载荷调用单个 JSON FFI 函数并返回已解码的结果值。
        """

        ffi_function = getattr(self.library, function_name)
        ffi_function.argtypes = [FfiBorrowedBuffer]
        ffi_function.restype = FfiOwnedBuffer
        input_bytes = json.dumps(payload).encode("utf-8")
        input_array = (ctypes.c_uint8 * len(input_bytes)).from_buffer_copy(input_bytes)
        input_buffer = FfiBorrowedBuffer(
            ptr=ctypes.cast(input_array, ctypes.POINTER(ctypes.c_uint8)),
            len=len(input_bytes),
        )
        return self._decode_owned_json_buffer(ffi_function(input_buffer))

    def call_without_input_value(self, function_name: str) -> JsonValue:
        """
        Call one no-argument JSON FFI function and return its decoded result value.
        调用一个无参数 JSON FFI 函数并返回其解码后的结果值。
        """

        # Exact no-argument prototype required by descriptor-style exports.
        # 描述符类导出所要求的精确无参数原型。
        ffi_function = getattr(self.library, function_name)
        ffi_function.argtypes = []
        ffi_function.restype = FfiOwnedBuffer
        return self._decode_owned_json_buffer(ffi_function())

    def _decode_owned_json_buffer(self, buffer: FfiOwnedBuffer) -> JsonValue:
        """
        Decode one owned JSON buffer returned by one `_json` FFI call and free it.
        解码一个由 `_json` FFI 调用返回的拥有型 JSON 缓冲并释放。
        """

        if not buffer.ptr and buffer.len != 0:
            raise RuntimeError("JSON FFI returned one null pointer with non-zero len")
        payload_text = (
            ctypes.string_at(buffer.ptr, buffer.len).decode("utf-8")
            if buffer.ptr and buffer.len
            else ""
        )
        if buffer.ptr:
            self.library.luaskills_ffi_buffer_free(buffer)
        envelope = json.loads(payload_text or "{}")
        if envelope.get("ok") is not True:
            raise RuntimeError(envelope.get("error") or "Unknown JSON FFI error")
        return envelope.get("result")

    def describe(self) -> JsonMap:
        """
        Read and cache the exported JSON FFI descriptor payload for diagnostics.
        读取并缓存已导出 JSON FFI 描述载荷，供诊断使用。
        """

        if self._describe_cache is None:
            # Descriptor export has a `void` parameter list, unlike request-taking `_json` calls.
            # 描述符导出使用 `void` 参数列表，不同于接收请求的 `_json` 调用。
            described = self.call_without_input_value("luaskills_ffi_describe_json")
            if not isinstance(described, dict):
                raise RuntimeError("JSON FFI descriptor body must be one object")
            self._describe_cache = described
        return self._describe_cache


class StandardFixtureRuntimeClient:
    """
    Shared host wrapper around the repository standard-runtime fixture root.
    面向仓库 standard-runtime 夹具根目录的共享宿主包装器。
    """

    def __init__(self, client: JsonFfiClient, runtime_root: Path) -> None:
        """
        Bind one JSON client to one runtime root and ensure the fixture layout exists.
        将一个 JSON 客户端绑定到一个运行时根目录，并确保夹具目录结构存在。
        """

        self.client = client
        self.runtime_root = runtime_root
        ensure_standard_fixture_layout(runtime_root)
        # Canonical System Plugin package root used by system lease demos.
        # system 租约示例使用的规范 System Plugin 包根目录。
        package_root = (
            runtime_root / "system_lua_lib" / STANDARD_FIXTURE_SYSTEM_PACKAGE_ID
        )
        package_root.mkdir(parents=True, exist_ok=True)
        # Required package-relative manifest kept deliberately empty for the generic fixture.
        # 通用夹具所需的包相对清单，内容刻意保持为空依赖。
        dependencies_file = package_root / "dependencies.yaml"
        if not dependencies_file.exists():
            dependencies_file.write_text("{}\n", encoding="utf-8")

    def create_engine(
        self,
        default_text_encoding: str | None = "utf-8",
        enable_managed_io_compat: bool = True,
    ) -> int:
        """
        Create one runtime engine configured for the shared fixture root.
        创建一个面向共享夹具根目录配置好的运行时引擎。
        """

        payload = self.client.call(
            "luaskills_ffi_engine_new_json",
            self._build_engine_request(
                default_text_encoding=default_text_encoding,
                enable_managed_io_compat=enable_managed_io_compat,
            ),
        )
        engine_id = payload.get("engine_id")
        if not isinstance(engine_id, int):
            raise RuntimeError("engine_new_json did not return one integer engine_id")
        return engine_id

    def load_root(self, engine_id: int, root_name: str = "ROOT") -> None:
        """
        Load the shared skill root into one existing engine.
        把共享技能根加载进一个已有引擎。
        """

        self.client.call(
            "luaskills_ffi_load_from_roots_json",
            {
                "engine_id": engine_id,
                "skill_roots": [
                    {
                        "name": root_name,
                        "skills_dir": normalized_path(self.runtime_root / "skills"),
                    }
                ],
            },
        )

    def free_engine(self, engine_id: int) -> None:
        """
        Free one previously created runtime engine.
        释放一个先前创建的运行时引擎。
        """

        self.client.call(
            "luaskills_ffi_engine_free_json",
            {
                "engine_id": engine_id,
            },
        )

    def runtime_leases(self, engine_id: int) -> "RuntimeLeaseClient":
        """
        Build one plain runtime-lease client that targets the public JSON FFI endpoints.
        构造一个指向公共 JSON FFI 入口的普通运行时租约客户端。
        """

        return RuntimeLeaseClient(self.client, engine_id)

    def system_runtime_leases(
        self,
        engine_id: int,
        authority: str = SKILL_AUTHORITY_DELEGATED_TOOL,
    ) -> "RuntimeLeaseClient":
        """
        Build one authority-bound runtime-lease client that targets the system JSON FFI endpoints.
        构造一个指向 system JSON FFI 入口并绑定 authority 的运行时租约客户端。
        """

        return RuntimeLeaseClient(
            self.client,
            engine_id,
            system_tool_authority=authority,
            system_package=self.fixture_system_package(),
        )

    def managed_session_events(
        self,
        engine_id: int,
        authority: str = SKILL_AUTHORITY_DELEGATED_TOOL,
    ) -> "ManagedSessionEventsClient":
        """
        Build one authority-bound managed-session event client for the target engine.
        为目标引擎构造一个绑定 authority 的受管会话事件客户端。

        Args:
            engine_id: Stable numeric FFI engine handle.
            engine_id：稳定数值 FFI 引擎句柄。
            authority: Host-injected authority forwarded to every event request.
            authority：转发到每个事件请求的宿主注入权限。

        Returns:
            One authority-bound managed-session event client.
            一个绑定权限的受管会话事件客户端。
        """

        return ManagedSessionEventsClient(self.client, engine_id, authority)

    def fixture_skill_roots(self, root_name: str = "ROOT") -> list[JsonMap]:
        """
        Build the ordered fixture skill-root chain shared by the standard-runtime demos.
        构造 standard-runtime 示例共用的有序夹具技能根链。
        """

        return [
            build_runtime_skill_root(
                root_name,
                normalized_path(self.runtime_root / "skills"),
            )
        ]

    def system_client(
        self,
        engine_id: int,
        authority: str = SKILL_AUTHORITY_DELEGATED_TOOL,
        root_name: str = "ROOT",
    ) -> "SystemEngineJsonClient":
        """
        Build one authority-bound engine helper that wraps system JSON FFI entrypoints.
        构造一个封装 system JSON FFI 入口并绑定 authority 的引擎辅助器。
        """

        return SystemEngineJsonClient(
            self.client,
            engine_id,
            authority,
            default_skill_roots=self.fixture_skill_roots(root_name=root_name),
            default_system_package=self.fixture_system_package(),
        )

    def fixture_system_package(self) -> SystemRuntimePackageDescriptor:
        """
        Build the strict System Plugin descriptor used by repository fixture leases.
        构造仓库夹具租约使用的严格 System Plugin 描述符。
        """

        # Canonical fixture package directory created during wrapper initialization.
        # 在包装器初始化期间创建的规范夹具包目录。
        package_root = (
            self.runtime_root
            / "system_lua_lib"
            / STANDARD_FIXTURE_SYSTEM_PACKAGE_ID
        )
        return {
            "id": STANDARD_FIXTURE_SYSTEM_PACKAGE_ID,
            "root": normalized_path(package_root),
            "dependencies_file": "dependencies.yaml",
        }

    def _build_engine_request(
        self,
        default_text_encoding: str | None,
        enable_managed_io_compat: bool,
    ) -> JsonMap:
        """
        Build one JSON engine creation request for the fixture runtime root.
        为夹具运行时根构造一个 JSON 引擎创建请求。
        """

        return {
            "options": {
                "pool_config": {
                    "min_size": 1,
                    "max_size": 1,
                    "idle_ttl_secs": 30,
                },
                "host_options": {
                    "temp_dir": normalized_path(self.runtime_root / "temp"),
                    "resources_dir": normalized_path(self.runtime_root / "resources"),
                    "lua_packages_dir": normalized_path(self.runtime_root / "lua_packages"),
                    "host_provided_tool_root": normalized_path(
                        self.runtime_root / "bin" / "tools"
                    ),
                    "host_provided_lua_root": normalized_path(
                        self.runtime_root / "lua_packages"
                    ),
                    "host_provided_ffi_root": normalized_path(
                        self.runtime_root / "libs"
                    ),
                    "system_lua_lib_dir": normalized_path(
                        self.runtime_root / "system_lua_lib"
                    ),
                    "download_cache_root": normalized_path(
                        self.runtime_root / "temp" / "downloads"
                    ),
                    "dependency_dir_name": "dependencies",
                    "state_dir_name": "state",
                    "database_dir_name": "databases",
                    "skill_config_file_path": None,
                    "allow_network_download": False,
                    "github_base_url": None,
                    "github_api_base_url": None,
                    "default_text_encoding": default_text_encoding,
                    "sqlite_library_path": None,
                    "sqlite_provider_mode": "dynamic_library",
                    "sqlite_callback_mode": "standard",
                    "lancedb_library_path": None,
                    "lancedb_provider_mode": "dynamic_library",
                    "lancedb_callback_mode": "standard",
                    "cache_config": None,
                    "runlua_pool_config": None,
                    "reserved_entry_names": [],
                    "ignored_skill_ids": [],
                    "capabilities": {
                        "enable_skill_management_bridge": False,
                        "enable_managed_io_compat": enable_managed_io_compat,
                    },
                },
            }
        }


class RuntimeLeaseClient:
    """
    Stateful host helper that wraps one engine's runtime-lease JSON API.
    包装单个引擎 runtime-lease JSON API 的有状态宿主辅助器。
    """

    def __init__(
        self,
        client: JsonFfiClient,
        engine_id: int,
        system_tool_authority: str | None = None,
        system_package: SystemRuntimePackageDescriptor | None = None,
    ) -> None:
        """
        Bind one JSON client to one existing engine id.
        将一个 JSON 客户端绑定到一个已有引擎标识。
        """

        self.client = client
        self.engine_id = engine_id
        self.system_tool_authority = system_tool_authority
        self.system_package = system_package

    def call_raw(self, action: str, payload: JsonMap) -> JsonMap:
        """
        Dispatch one raw runtime-lease JSON request without applying success checks.
        分发单个原始运行时租约 JSON 请求而不附加成功校验。
        """

        request_payload: JsonMap = {
            **payload,
            "engine_id": self.engine_id,
        }
        if self.system_tool_authority is not None:
            request_payload["authority"] = self.system_tool_authority
        return self.client.call(
            self._runtime_lease_function_name(action),
            request_payload,
        )

    def create(
        self,
        sid: str,
        ttl_sec: int = 600,
        replace: bool = False,
        *,
        cwd: str | None = None,
        workspace_root: str | None = None,
        mounts: JsonMap | None = None,
    ) -> JsonMap:
        """
        Create or replace one persistent runtime lease.
        创建或替换一个持久运行时租约。
        """

        # Base fields shared by public and System lease creation.
        # 公共与 System 租约创建共享的基础字段。
        request: JsonMap = {
            "sid": sid,
            "ttl_sec": ttl_sec,
            "replace": replace,
        }
        if self.system_tool_authority is not None:
            if self.system_package is None:
                raise RuntimeError(
                    "system runtime lease creation requires system_package"
                )
            request["system_package"] = self.system_package
        if mounts is not None:
            request["mounts"] = mounts
        elif self.system_tool_authority is not None:
            request["mounts"] = {}
        if cwd is not None:
            request["cwd"] = cwd
        if workspace_root is not None:
            request["workspace_root"] = workspace_root
        return require_runtime_lease_ok(
            self.call_raw("create", request),
            "runtime lease create",
        )

    def create_handle(
        self,
        sid: str,
        ttl_sec: int = 600,
        replace: bool = False,
        *,
        cwd: str | None = None,
        workspace_root: str | None = None,
        mounts: JsonMap | None = None,
    ) -> "RuntimeLeaseHandle":
        """
        Create one runtime-lease handle object from a fresh create response.
        基于新的 create 响应创建一个运行时租约句柄对象。
        """

        return RuntimeLeaseHandle.from_payload(
            self,
            self.create(
                sid,
                ttl_sec=ttl_sec,
                replace=replace,
                cwd=cwd,
                workspace_root=workspace_root,
                mounts=mounts,
            ),
        )

    def bind_handle(self, payload: JsonMap) -> "RuntimeLeaseHandle":
        """
        Rebuild one runtime-lease handle object from one persisted payload.
        基于一份已持久化载荷重建一个运行时租约句柄对象。
        """

        return RuntimeLeaseHandle.from_payload(self, payload)

    def eval(
        self,
        lease_id: str,
        code: str,
        args: JsonMap | None = None,
        timeout_ms: int = 60_000,
        sid: str | None = None,
        generation: int | None = None,
    ) -> JsonMap:
        """
        Evaluate one Lua chunk inside one persistent runtime lease with optional identity guards.
        在一个持久运行时租约中执行单个 Lua 代码块，并可附带可选身份护栏。
        """

        payload: JsonMap = {
            "lease_id": lease_id,
            "timeout_ms": timeout_ms,
            "args": args or {},
            "code": code,
        }
        if sid is not None:
            payload["sid"] = sid
        if generation is not None:
            payload["generation"] = generation
        return require_runtime_lease_ok(
            self.call_raw("eval", payload),
            "runtime lease eval",
        )

    def status(
        self,
        lease_id: str,
        sid: str | None = None,
        generation: int | None = None,
    ) -> JsonMap:
        """
        Read one runtime lease status payload with optional identity guards.
        读取单个运行时租约状态载荷，并可附带可选身份护栏。
        """

        payload: JsonMap = {
            "lease_id": lease_id,
        }
        if sid is not None:
            payload["sid"] = sid
        if generation is not None:
            payload["generation"] = generation
        return self.call_raw("status", payload)

    def list(self, sid: str | None = None) -> JsonMap:
        """
        List active runtime leases and optionally filter by one SID.
        列出活跃运行时租约，并可按单个 SID 过滤。
        """

        payload: JsonMap = {}
        if sid is not None:
            payload["sid"] = sid
        return self.call_raw("list", payload)

    def list_handles(self, sid: str | None = None) -> "list[RuntimeLeaseHandle]":
        """
        List active runtime-lease handles rebuilt from the current lease listing payload.
        基于当前租约列表载荷重建活跃运行时租约句柄列表。
        """

        result = self.list(sid=sid)
        leases = result.get("leases")
        if not isinstance(leases, list):
            raise RuntimeError("runtime lease list payload is missing the leases array")
        return [
            self.bind_handle(lease)
            for lease in leases
            if isinstance(lease, dict)
        ]

    def find_handle(self, sid: str) -> "RuntimeLeaseHandle | None":
        """
        Return the first active runtime-lease handle for one SID when present.
        返回某个 SID 的第一个活跃运行时租约句柄（如果存在）。
        """

        handles = self.list_handles(sid=sid)
        return handles[0] if handles else None

    def close(
        self,
        lease_id: str,
        sid: str | None = None,
        generation: int | None = None,
    ) -> JsonMap:
        """
        Close one runtime lease and return its final status payload with optional identity guards.
        关闭单个运行时租约并返回其最终状态载荷，并可附带可选身份护栏。
        """

        payload: JsonMap = {
            "lease_id": lease_id,
        }
        if sid is not None:
            payload["sid"] = sid
        if generation is not None:
            payload["generation"] = generation
        return self.call_raw("close", payload)

    def _runtime_lease_function_name(self, action: str) -> str:
        """
        Resolve the concrete runtime-lease JSON FFI entrypoint name for one logical action.
        为单个逻辑动作解析具体的运行时租约 JSON FFI 入口名称。
        """

        if self.system_tool_authority is None:
            return f"luaskills_ffi_runtime_lease_{action}_json"
        return f"luaskills_ffi_system_runtime_lease_{action}_json"

    def uses_system_runtime_lease_endpoints(self) -> bool:
        """
        Return whether this helper will dispatch runtime-lease requests to dedicated system entrypoints.
        返回当前辅助器是否会把运行时租约请求分发到专用 system 入口。
        """

        return self.system_tool_authority is not None


class ManagedSessionEventsClient:
    """
    Authority-bound JSON client for engine-level System managed-session events.
    面向引擎级 System 受管会话事件、绑定 authority 的 JSON 客户端。
    """

    def __init__(self, client: JsonFfiClient, engine_id: int, authority: str) -> None:
        """
        Bind one JSON client, engine id, and host-injected authority.
        绑定一个 JSON 客户端、引擎标识与宿主注入的 authority。

        Args:
            client: Low-level JSON FFI transport.
            client：底层 JSON FFI 传输器。
            engine_id: Stable numeric FFI engine handle.
            engine_id：稳定数值 FFI 引擎句柄。
            authority: Host-injected authority required by the event surface.
            authority：事件接口要求的宿主注入权限。

        Returns:
            None.
            无返回值。
        """

        self.client = client
        self.engine_id = engine_id
        self.authority = authority

    def poll(self, max_events: int = 64) -> ManagedSessionEventBatch:
        """
        Destructively poll at most max_events ready events without waiting.
        无等待地破坏性轮询最多 max_events 个就绪事件。

        Args:
            max_events: Positive maximum number of events removed from the queue.
            max_events：从队列移除的正数事件数量上限。

        Returns:
            One validated immediate event batch.
            一个经过校验的即时事件批次。
        """

        return require_managed_session_event_batch(
            self.client.call(
                "luaskills_ffi_managed_session_events_poll_json",
                {
                    "engine_id": self.engine_id,
                    "max_events": max_events,
                    "authority": self.authority,
                },
            )
        )

    def wait(
        self,
        max_events: int = 64,
        timeout_ms: int = 1_000,
    ) -> ManagedSessionEventBatch:
        """
        Destructively wait for at most max_events events until timeout_ms expires.
        破坏性等待最多 max_events 个事件，直至 timeout_ms 到期。

        Args:
            max_events: Positive maximum number of events removed from the queue.
            max_events：从队列移除的正数事件数量上限。
            timeout_ms: Finite wait duration in milliseconds; zero is nonblocking.
            timeout_ms：有限等待毫秒数；零表示非阻塞。

        Returns:
            One validated event or timeout batch.
            一个经过校验的事件批次或超时批次。
        """

        return require_managed_session_event_batch(
            self.client.call(
                "luaskills_ffi_managed_session_events_wait_json",
                {
                    "engine_id": self.engine_id,
                    "max_events": max_events,
                    "timeout_ms": timeout_ms,
                    "authority": self.authority,
                },
            )
        )


class SystemEngineJsonClient:
    """
    Authority-bound helper that wraps one engine's system JSON FFI entrypoints.
    封装单个引擎 system JSON FFI 入口并绑定 authority 的辅助器。
    """

    def __init__(
        self,
        client: JsonFfiClient,
        engine_id: int,
        authority: str,
        default_skill_roots: list[JsonMap] | None = None,
        default_system_package: SystemRuntimePackageDescriptor | None = None,
    ) -> None:
        """
        Bind one JSON client, engine id, authority, and optional default skill-root chain.
        绑定一个 JSON 客户端、引擎标识、authority 与可选默认技能根链。
        """

        self.client = client
        self.engine_id = engine_id
        self.authority = authority
        self.default_skill_roots = list(default_skill_roots or [])
        self.default_system_package = default_system_package

    def call(self, function_name: str, payload: JsonMap | None = None) -> JsonMap:
        """
        Call one system JSON FFI function and require an object-shaped result payload.
        调用单个 system JSON FFI 函数并要求返回对象形状的结果载荷。
        """

        return self.client.call(
            function_name,
            self._with_engine_authority(payload or {}),
        )

    def call_value(self, function_name: str, payload: JsonMap | None = None) -> JsonValue:
        """
        Call one system JSON FFI function and return any decoded JSON result shape.
        调用单个 system JSON FFI 函数并返回任意已解码 JSON 结果形状。
        """

        return self.client.call_value(
            function_name,
            self._with_engine_authority(payload or {}),
        )

    def runtime_leases(self) -> RuntimeLeaseClient:
        """
        Build one authority-bound runtime-lease helper under the current engine wrapper.
        在当前引擎包装器下构造一个绑定 authority 的运行时租约辅助器。
        """

        if self.default_system_package is None:
            raise RuntimeError("system runtime lease helper requires system_package")
        return RuntimeLeaseClient(
            self.client,
            self.engine_id,
            system_tool_authority=self.authority,
            system_package=self.default_system_package,
        )

    def managed_session_events(self) -> ManagedSessionEventsClient:
        """
        Build one event helper whose poll and wait requests reuse the bound authority.
        构造一个让 poll 与 wait 请求复用当前 authority 的事件辅助器。

        Returns:
            One event client bound to this engine and authority.
            一个绑定当前引擎与权限的事件客户端。
        """

        return ManagedSessionEventsClient(
            self.client,
            self.engine_id,
            self.authority,
        )

    def list_entries(self) -> list[JsonMap]:
        """
        List runtime entries visible to the bound authority.
        列出当前绑定 authority 可见的运行时入口。
        """

        result = self.call_value("luaskills_ffi_list_entries_json")
        if not isinstance(result, list):
            raise RuntimeError("list_entries_json did not return one array result")
        return [entry for entry in result if isinstance(entry, dict)]

    def list_skill_help(self) -> list[JsonMap]:
        """
        List skill help trees visible to the bound authority.
        列出当前绑定 authority 可见的技能帮助树。
        """

        result = self.call_value("luaskills_ffi_list_skill_help_json")
        if not isinstance(result, list):
            raise RuntimeError("list_skill_help_json did not return one array result")
        return [entry for entry in result if isinstance(entry, dict)]

    def render_skill_help_detail(
        self,
        skill_id: str,
        flow_name: str,
        request_context: JsonMap | None = None,
    ) -> JsonMap | None:
        """
        Render one help-detail payload visible to the bound authority.
        渲染当前绑定 authority 可见的一份帮助详情载荷。
        """

        payload: JsonMap = {
            "skill_id": skill_id,
            "flow_name": flow_name,
        }
        if request_context is not None:
            payload["request_context"] = request_context
        result = self.call_value("luaskills_ffi_render_skill_help_detail_json", payload)
        if result is None:
            return None
        if not isinstance(result, dict):
            raise RuntimeError("render_skill_help_detail_json did not return one object result")
        return result

    def prompt_argument_completions(
        self,
        prompt_name: str,
        argument_name: str,
    ) -> list[str] | None:
        """
        Read prompt-argument completion candidates visible to the bound authority.
        读取当前绑定 authority 可见的提示词参数补全候选项。
        """

        result = self.call_value(
            "luaskills_ffi_prompt_argument_completions_json",
            {
                "prompt_name": prompt_name,
                "argument_name": argument_name,
            },
        )
        if result is None:
            return None
        if not isinstance(result, list):
            raise RuntimeError(
                "prompt_argument_completions_json did not return one array result"
            )
        return [value for value in result if isinstance(value, str)]

    def is_skill(self, tool_name: str) -> bool:
        """
        Return whether one tool name resolves to one visible Lua skill entry.
        返回某个工具名是否解析为一个可见 Lua 技能入口。
        """

        result = self.call(
            "luaskills_ffi_is_skill_json",
            {
                "tool_name": tool_name,
            },
        )
        value = result.get("value")
        if isinstance(value, bool):
            return value
        raise RuntimeError("is_skill_json did not return one boolean value field")

    def skill_name_for_tool(self, tool_name: str) -> str | None:
        """
        Resolve the visible owning skill id for one tool name when available.
        在可见时解析某个工具名所属的技能标识。
        """

        result = self.call(
            "luaskills_ffi_skill_name_for_tool_json",
            {
                "tool_name": tool_name,
            },
        )
        skill_id = result.get("skill_id")
        if skill_id is None or isinstance(skill_id, str):
            return skill_id
        raise RuntimeError("skill_name_for_tool_json did not return a nullable string field")

    def disable_skill(
        self,
        skill_id: str,
        reason: str | None = None,
        skill_roots: list[JsonMap] | None = None,
    ) -> JsonMap:
        """
        Disable one skill through the system JSON FFI surface.
        通过 system JSON FFI 入口停用单个技能。
        """

        payload: JsonMap = {
            "skill_roots": self._resolve_skill_roots(skill_roots),
            "skill_id": skill_id,
        }
        if reason is not None:
            payload["reason"] = reason
        return self.call("luaskills_ffi_system_disable_skill_json", payload)

    def enable_skill(
        self,
        skill_id: str,
        skill_roots: list[JsonMap] | None = None,
    ) -> JsonMap:
        """
        Enable one skill through the system JSON FFI surface.
        通过 system JSON FFI 入口启用单个技能。
        """

        return self.call(
            "luaskills_ffi_system_enable_skill_json",
            {
                "skill_roots": self._resolve_skill_roots(skill_roots),
                "skill_id": skill_id,
            },
        )

    def uninstall_skill(
        self,
        skill_id: str,
        skill_roots: list[JsonMap] | None = None,
        target_root: JsonMap | None = None,
        options: JsonMap | None = None,
    ) -> JsonMap:
        """
        Uninstall one skill through the system JSON FFI surface.
        通过 system JSON FFI 入口卸载单个技能。
        """

        payload: JsonMap = {
            "skill_roots": self._resolve_skill_roots(skill_roots),
            "skill_id": skill_id,
            "options": options or {},
        }
        if target_root is not None:
            payload["target_root"] = target_root
        return self.call("luaskills_ffi_system_uninstall_skill_json", payload)

    def install_skill(
        self,
        request: JsonMap,
        skill_roots: list[JsonMap] | None = None,
        target_root: JsonMap | None = None,
    ) -> JsonMap:
        """
        Install one managed skill through the system JSON FFI surface.
        通过 system JSON FFI 入口安装单个受管技能。
        """

        payload: JsonMap = {
            "skill_roots": self._resolve_skill_roots(skill_roots),
            "request": request,
        }
        if target_root is not None:
            payload["target_root"] = target_root
        return self.call("luaskills_ffi_system_install_skill_json", payload)

    def update_skill(
        self,
        request: JsonMap,
        skill_roots: list[JsonMap] | None = None,
        target_root: JsonMap | None = None,
    ) -> JsonMap:
        """
        Update one managed skill through the system JSON FFI surface.
        通过 system JSON FFI 入口更新单个受管技能。
        """

        payload: JsonMap = {
            "skill_roots": self._resolve_skill_roots(skill_roots),
            "request": request,
        }
        if target_root is not None:
            payload["target_root"] = target_root
        return self.call("luaskills_ffi_system_update_skill_json", payload)

    def _with_engine_authority(self, payload: JsonMap) -> JsonMap:
        """
        Attach the bound engine id and authority to one outgoing system JSON payload.
        为单个发出的 system JSON 载荷附加已绑定的引擎标识与 authority。
        """

        return {
            **payload,
            "engine_id": self.engine_id,
            "authority": self.authority,
        }

    def _resolve_skill_roots(
        self,
        skill_roots: list[JsonMap] | None,
    ) -> list[JsonMap]:
        """
        Resolve the skill-root chain for one system lifecycle request.
        解析单个 system 生命周期请求使用的技能根链。
        """

        resolved = skill_roots if skill_roots is not None else self.default_skill_roots
        if not resolved:
            raise RuntimeError("system helper requires one explicit or default skill_roots chain")
        return list(resolved)


class RuntimeLeaseHandle:
    """
    Stable host-side runtime-lease handle that carries lease identity guards automatically.
    自动携带租约身份护栏的稳定宿主侧运行时租约句柄。
    """

    def __init__(
        self,
        sessions: RuntimeLeaseClient,
        lease_id: str,
        sid: str,
        generation: int,
    ) -> None:
        """
        Bind one session client to one concrete lease identity triplet.
        将一个会话客户端绑定到一个具体的租约身份三元组。
        """

        self.sessions = sessions
        self.lease_id = lease_id
        self.sid = sid
        self.generation = generation

    @classmethod
    def from_payload(
        cls,
        sessions: RuntimeLeaseClient,
        payload: JsonMap,
    ) -> "RuntimeLeaseHandle":
        """
        Construct one runtime-lease handle from one JSON payload that contains identity fields.
        从包含身份字段的一份 JSON 载荷中构造一个运行时租约句柄。
        """

        return cls(
            sessions=sessions,
            lease_id=require_runtime_lease_string_field(payload, "lease_id"),
            sid=require_runtime_lease_string_field(payload, "sid"),
            generation=require_runtime_lease_int_field(payload, "generation"),
        )

    def identity_payload(self) -> JsonMap:
        """
        Export the stable lease identity fields for persistence or raw FFI calls.
        导出稳定租约身份字段，供持久化或原始 FFI 调用使用。
        """

        return {
            "lease_id": self.lease_id,
            "sid": self.sid,
            "generation": self.generation,
        }

    def eval(
        self,
        code: str,
        args: JsonMap | None = None,
        timeout_ms: int = 60_000,
    ) -> JsonMap:
        """
        Evaluate Lua code while automatically attaching the stored lease identity guards.
        执行 Lua 代码时自动附带已保存的租约身份护栏。
        """

        return self.sessions.eval(
            self.lease_id,
            code,
            args=args,
            timeout_ms=timeout_ms,
            sid=self.sid,
            generation=self.generation,
        )

    def status(self) -> JsonMap:
        """
        Read the current lease status while automatically attaching the stored identity guards.
        读取当前租约状态时自动附带已保存的身份护栏。
        """

        return self.sessions.status(
            self.lease_id,
            sid=self.sid,
            generation=self.generation,
        )

    def close(self) -> JsonMap:
        """
        Close the current lease while automatically attaching the stored identity guards.
        关闭当前租约时自动附带已保存的身份护栏。
        """

        return self.sessions.close(
            self.lease_id,
            sid=self.sid,
            generation=self.generation,
        )


def normalized_path(path: Path) -> str:
    """
    Convert one local path into one normalized POSIX-like string for JSON FFI payloads.
    将一个本地路径转换为供 JSON FFI 载荷使用的规范 POSIX 风格字符串。
    """

    return str(path.resolve()).replace("\\", "/")


def build_runtime_skill_root(name: str, skills_dir: str) -> JsonMap:
    """
    Build one JSON runtime skill-root object for lifecycle and load helpers.
    为生命周期与加载辅助函数构造一个 JSON 运行时技能根对象。
    """

    return {
        "name": name,
        "skills_dir": skills_dir,
    }


def require_runtime_lease_ok(payload: JsonMap, action: str) -> JsonMap:
    """
    Require one runtime-lease payload to report success.
    要求单个运行时租约载荷报告成功。
    """

    if payload.get("ok") is True:
        return payload
    raise RuntimeError(
        f"{action} failed: {payload.get('error_code') or 'unknown'}: {payload.get('message') or 'Unknown runtime lease error'}"
    )


def require_runtime_lease_string_field(payload: JsonMap, field_name: str) -> str:
    """
    Read one required runtime-lease string field from one JSON payload.
    从一份 JSON 载荷中读取一个必填的运行时租约字符串字段。
    """

    value = payload.get(field_name)
    if isinstance(value, str) and value:
        return value
    raise RuntimeError(
        f"runtime lease payload is missing required string field: {field_name}"
    )


def require_runtime_lease_int_field(payload: JsonMap, field_name: str) -> int:
    """
    Read one required runtime-lease integer field from one JSON payload.
    从一份 JSON 载荷中读取一个必填的运行时租约整数字段。
    """

    value = payload.get(field_name)
    if isinstance(value, int):
        return value
    raise RuntimeError(
        f"runtime lease payload is missing required integer field: {field_name}"
    )


def require_managed_session_event_batch(payload: JsonMap) -> ManagedSessionEventBatch:
    """
    Validate and return one managed-session event batch from the JSON FFI result.
    校验并返回 JSON FFI 结果中的单个受管会话事件批次。

    Args:
        payload: Decoded `_json` result object.
        payload：已解码的 `_json` 结果对象。

    Returns:
        One fully shape-validated managed-session event batch.
        一个已完成全部结构校验的受管会话事件批次。
    """

    # Raw event list validated before exposure through the typed wrapper.
    # 在通过类型化包装层暴露前完成校验的原始事件列表。
    events = payload.get("events")
    # Queue depth remaining after the destructive drain.
    # 破坏性排空后剩余的队列深度。
    remaining = payload.get("remaining")
    # Explicit wait-timeout marker distinguished from an ordinary empty poll.
    # 与普通空轮询区分的显式等待超时标记。
    timed_out = payload.get("timed_out")
    if (
        not isinstance(events, list)
        or not isinstance(remaining, int)
        or isinstance(remaining, bool)
        or remaining < 0
        or not isinstance(timed_out, bool)
    ):
        raise RuntimeError("managed-session event result has an invalid batch shape")
    for event in events:
        if not isinstance(event, dict) or not is_managed_session_event(event):
            raise RuntimeError(
                "managed-session event result contains an invalid event"
            )
    return {
        "events": cast(list[ManagedSessionEvent], events),
        "remaining": remaining,
        "timed_out": timed_out,
    }


def is_managed_session_event(event: JsonMap) -> bool:
    """
    Return whether one JSON object satisfies the stable managed-session event shape.
    返回一个 JSON 对象是否满足稳定的受管会话事件结构。

    Args:
        event: Decoded candidate event object.
        event：已解码的候选事件对象。

    Returns:
        True only when every identity, kind, and sequence field is valid.
        仅当全部身份、类型与序号字段均有效时返回 True。
    """

    # Stable event kinds admitted by the Rust event protocol.
    # Rust 事件协议允许的稳定事件类型。
    kinds = {"stdout_readable", "stderr_readable", "exited", "failed"}
    # Numeric protocol fields that must be non-negative integers rather than booleans.
    # 必须是非负整数而非布尔值的数值协议字段。
    numeric_fields = ("generation", "managed_session_id", "sequence")
    return (
        isinstance(event.get("system_lease_id"), str)
        and isinstance(event.get("sid"), str)
        and all(
            isinstance(event.get(field), int)
            and not isinstance(event.get(field), bool)
            and cast(int, event.get(field)) >= 0
            for field in numeric_fields
        )
        and event.get("kind") in kinds
    )
