#ifndef LUASKILLS_JSON_FFI_H
#define LUASKILLS_JSON_FFI_H

#include "luaskills_ffi.h"

/*
Public high-level JSON FFI exported by luaskills.
luaskills 导出的公共高层 JSON FFI 接口面。
*/

/*
Beta integration contract for v0.1.x:
- This header is the public high-level JSON FFI for dynamic languages and rapid integrations.
- Shared structs and free helpers come from luaskills_ffi.h.
- Returned buffers must be released only with the matching luaskills free function.
- JSON callbacks must be registered before engine creation when callback-based modes are used.
- Callbacks must not unwind across the C ABI boundary.
- Same-thread reentry into the same engine is not supported.
v0.1.x beta 集成契约：
- 当前头文件是面向动态语言与快速集成场景的公共高层 JSON FFI。
- 共享结构体与释放辅助函数来自 luaskills_ffi.h。
- 所有返回缓冲都只能使用匹配的 luaskills 释放函数处理。
- 使用 JSON callback 模式时，宿主必须先注册 callback，再创建 engine。
- callback 不允许把异常跨越 C ABI 边界传播。
- 不支持同一线程内对同一 engine 的重入调用。
*/

#ifdef __cplusplus
extern "C" {
#endif

/*
JSON callback must consume one borrowed UTF-8 request buffer and fill one owned response buffer.
JSON callback 必须消费一个借用 UTF-8 请求缓冲，并填充一个拥有型响应缓冲。
*/
typedef int32_t (*FfiJsonProviderCallback)(
    FfiBorrowedBuffer request_json,
    void *user_data,
    FfiOwnedBuffer *response_out,
    FfiOwnedBuffer *error_out
);

/*
Free one heap-allocated string returned by JSON/helper string-producing FFI functions.
Only pass pointers returned by luaskills FFI string-producing helper functions to string_free.
释放一段由 JSON 或辅助字符串型 FFI 函数返回的堆字符串。
只能将 luaskills FFI 字符串辅助函数产出的指针传给 string_free。
*/
void luaskills_ffi_string_free(char *value);
/*
Clone one host-owned string into one luaskills-owned heap string for helper returns.
The input must be null or valid UTF-8; invalid UTF-8 returns null.
将宿主拥有的字符串克隆为 luaskills 自主管理的堆字符串，供辅助返回值使用。
输入必须为空指针或有效 UTF-8；非法 UTF-8 会返回空指针。
*/
char *luaskills_ffi_string_clone(const char *value);

/*
Register or clear the SQLite JSON callback before engine creation.
在创建 engine 前注册或清理 SQLite JSON callback。
*/
int32_t luaskills_ffi_set_sqlite_provider_json_callback(
    FfiJsonProviderCallback callback,
    void *user_data,
    FfiOwnedBuffer *error_out
);

/*
Register or clear the LanceDB JSON callback before engine creation.
在创建 engine 前注册或清理 LanceDB JSON callback。
*/
int32_t luaskills_ffi_set_lancedb_provider_json_callback(
    FfiJsonProviderCallback callback,
    void *user_data,
    FfiOwnedBuffer *error_out
);

/*
Register or clear the host-tool JSON callback used by Lua vulcan.host.*.
注册或清理 Lua vulcan.host.* 使用的宿主工具 JSON callback。
*/
int32_t luaskills_ffi_set_host_tool_json_callback(
    FfiJsonProviderCallback callback,
    void *user_data,
    FfiOwnedBuffer *error_out
);

/*
Register or clear the skill-operation progress JSON callback used by install and update flows.
注册或清理安装与更新流程使用的技能操作进度 JSON callback。
*/
int32_t luaskills_ffi_set_skill_operation_progress_json_callback(
    FfiJsonProviderCallback callback,
    void *user_data,
    FfiOwnedBuffer *error_out
);

/*
Register or clear the model embedding JSON callback used by Lua vulcan.models.embed(text).
注册或清理 Lua vulcan.models.embed(text) 使用的模型 embedding JSON callback。
*/
int32_t luaskills_ffi_set_model_embed_json_callback(
    FfiJsonProviderCallback callback,
    void *user_data,
    FfiOwnedBuffer *error_out
);

/*
Register or clear the model LLM JSON callback used by Lua vulcan.models.llm(system, user).
注册或清理 Lua vulcan.models.llm(system, user) 使用的模型 LLM JSON callback。
*/
int32_t luaskills_ffi_set_model_llm_json_callback(
    FfiJsonProviderCallback callback,
    void *user_data,
    FfiOwnedBuffer *error_out
);

/*
Return one stable FFI version descriptor as JSON.
以 JSON 形式返回稳定的 FFI 版本描述。
*/
FfiOwnedBuffer luaskills_ffi_version_json(void);

/*
Return one JSON description of exported FFI entrypoints.
以 JSON 形式返回已导出 FFI 入口点说明。
*/
FfiOwnedBuffer luaskills_ffi_describe_json(void);

/*
Create one LuaSkills engine from one JSON request.
通过一段 JSON 请求创建一个 LuaSkills 引擎。
*/
FfiOwnedBuffer luaskills_ffi_engine_new_json(FfiBorrowedBuffer input_json);

/*
Free one previously created LuaSkills engine handle.
释放一个先前创建的 LuaSkills 引擎句柄。
*/
FfiOwnedBuffer luaskills_ffi_engine_free_json(FfiBorrowedBuffer input_json);

/*
Load skills from one ordered root chain.
从一条有序根链加载技能。
*/
FfiOwnedBuffer luaskills_ffi_load_from_roots_json(FfiBorrowedBuffer input_json);

/*
Reload skills from one ordered root chain.
从一条有序根链重载技能。
*/
FfiOwnedBuffer luaskills_ffi_reload_from_roots_json(FfiBorrowedBuffer input_json);

/*
List runtime entry descriptors as JSON with host-injected query authority.
通过宿主注入查询权限以 JSON 形式列出运行时入口描述。
*/
FfiOwnedBuffer luaskills_ffi_list_entries_json(FfiBorrowedBuffer input_json);

/*
List runtime help descriptors as JSON with host-injected query authority.
通过宿主注入查询权限以 JSON 形式列出运行时帮助描述。
*/
FfiOwnedBuffer luaskills_ffi_list_skill_help_json(FfiBorrowedBuffer input_json);

/*
Render one runtime help detail payload as JSON with host-injected query authority.
通过宿主注入查询权限以 JSON 形式渲染单个运行时帮助详情。
*/
FfiOwnedBuffer luaskills_ffi_render_skill_help_detail_json(FfiBorrowedBuffer input_json);

/*
Resolve prompt argument completions as JSON with host-injected authority.
通过宿主注入权限以 JSON 形式解析提示词参数补全项。
*/
FfiOwnedBuffer luaskills_ffi_prompt_argument_completions_json(FfiBorrowedBuffer input_json);

/*
Check whether one canonical tool name belongs to a visible Lua skill.
检查某个 canonical 工具名是否属于可见 Lua 技能。
*/
FfiOwnedBuffer luaskills_ffi_is_skill_json(FfiBorrowedBuffer input_json);

/*
Resolve the visible owning skill id of one canonical tool name.
解析某个 canonical 工具名可见的所属技能标识符。
*/
FfiOwnedBuffer luaskills_ffi_skill_name_for_tool_json(FfiBorrowedBuffer input_json);

/*
List flattened skill config records as JSON.
以 JSON 形式列出扁平化技能配置记录。
*/
FfiOwnedBuffer luaskills_ffi_skill_config_list_json(FfiBorrowedBuffer input_json);

/*
Read one optional skill config value as JSON.
以 JSON 形式读取单个可选技能配置值。
*/
FfiOwnedBuffer luaskills_ffi_skill_config_get_json(FfiBorrowedBuffer input_json);

/*
Insert or replace one skill config value as JSON.
以 JSON 形式插入或替换单个技能配置值。
*/
FfiOwnedBuffer luaskills_ffi_skill_config_set_json(FfiBorrowedBuffer input_json);

/*
Delete one skill config key as JSON.
以 JSON 形式删除单个技能配置键。
*/
FfiOwnedBuffer luaskills_ffi_skill_config_delete_json(FfiBorrowedBuffer input_json);

/*
Call one active loaded skill entry using one JSON request.
使用一段 JSON 请求调用单个已激活的已加载技能入口。
*/
FfiOwnedBuffer luaskills_ffi_call_skill_json(FfiBorrowedBuffer input_json);

/*
Execute arbitrary Lua code using one JSON request.
使用一段 JSON 请求执行任意 Lua 代码。
*/
FfiOwnedBuffer luaskills_ffi_run_lua_json(FfiBorrowedBuffer input_json);

/*
Create one persistent runtime lease using one JSON request.
使用一段 JSON 请求创建单个持久运行时租约。
*/
FfiOwnedBuffer luaskills_ffi_runtime_lease_create_json(FfiBorrowedBuffer input_json);

/*
Evaluate Lua code inside one persistent runtime lease using one JSON request.
使用一段 JSON 请求在单个持久运行时租约中执行 Lua 代码。
*/
FfiOwnedBuffer luaskills_ffi_runtime_lease_eval_json(FfiBorrowedBuffer input_json);

/*
Return one persistent runtime lease status using one JSON request.
使用一段 JSON 请求返回单个持久运行时租约状态。
*/
FfiOwnedBuffer luaskills_ffi_runtime_lease_status_json(FfiBorrowedBuffer input_json);

/*
List active persistent runtime leases using one JSON request.
使用一段 JSON 请求列出活跃持久运行时租约。
*/
FfiOwnedBuffer luaskills_ffi_runtime_lease_list_json(FfiBorrowedBuffer input_json);

/*
Close one persistent runtime lease using one JSON request.
使用一段 JSON 请求关闭单个持久运行时租约。
*/
FfiOwnedBuffer luaskills_ffi_runtime_lease_close_json(FfiBorrowedBuffer input_json);

/*
Create one persistent runtime lease using one system JSON request with host-injected authority.
使用一段带宿主注入 authority 的 system JSON 请求创建单个持久运行时租约。
*/
FfiOwnedBuffer luaskills_ffi_system_runtime_lease_create_json(FfiBorrowedBuffer input_json);

/*
Evaluate Lua code inside one persistent runtime lease using one system JSON request with host-injected authority.
使用一段带宿主注入 authority 的 system JSON 请求在单个持久运行时租约中执行 Lua 代码。
*/
FfiOwnedBuffer luaskills_ffi_system_runtime_lease_eval_json(FfiBorrowedBuffer input_json);

/*
Return one persistent runtime lease status using one system JSON request with host-injected authority.
使用一段带宿主注入 authority 的 system JSON 请求返回单个持久运行时租约状态。
*/
FfiOwnedBuffer luaskills_ffi_system_runtime_lease_status_json(FfiBorrowedBuffer input_json);

/*
List active persistent runtime leases using one system JSON request with host-injected authority.
使用一段带宿主注入 authority 的 system JSON 请求列出活跃持久运行时租约。
*/
FfiOwnedBuffer luaskills_ffi_system_runtime_lease_list_json(FfiBorrowedBuffer input_json);

/*
Close one persistent runtime lease using one system JSON request with host-injected authority.
使用一段带宿主注入 authority 的 system JSON 请求关闭单个持久运行时租约。
*/
FfiOwnedBuffer luaskills_ffi_system_runtime_lease_close_json(FfiBorrowedBuffer input_json);

/*
Poll one bounded managed-session event batch using a strict JSON request.
使用严格 JSON 请求轮询一批有界的受管会话事件。
The request requires engine_id, positive max_events, and host-injected authority; unknown fields fail.
请求要求 engine_id、正数 max_events 与宿主注入 authority；未知字段会导致失败。
The result envelope's result field contains events, remaining, and timed_out; closed and empty centers return an error.
结果包络的 result 字段包含 events、remaining 与 timed_out；关闭且空队列的事件中心返回错误。
*/
FfiOwnedBuffer luaskills_ffi_managed_session_events_poll_json(FfiBorrowedBuffer input_json);

/*
Wait for one bounded managed-session event batch using a strict JSON request.
使用严格 JSON 请求等待一批有界的受管会话事件。
The request additionally requires timeout_ms; zero performs a true nonblocking poll.
请求还要求 timeout_ms；零表示真正的非阻塞轮询。
Timeout returns a successful empty batch with timed_out=true.
超时返回 timed_out=true 的成功空批次。
Closed and empty centers return an explicit error instead of a timeout batch.
关闭且队列为空的事件中心返回显式错误，而不是超时批次。
*/
FfiOwnedBuffer luaskills_ffi_managed_session_events_wait_json(FfiBorrowedBuffer input_json);

/*
Disable one skill through one ordered root chain.
通过一条有序根链停用单个技能。
*/
FfiOwnedBuffer luaskills_ffi_disable_skill_json(FfiBorrowedBuffer input_json);

/*
Disable one skill through one ordered root chain with host-injected system authority.
通过一条有序根链和宿主注入的 system 权限停用单个技能。
*/
FfiOwnedBuffer luaskills_ffi_system_disable_skill_json(FfiBorrowedBuffer input_json);

/*
Enable one skill through one ordered root chain.
通过一条有序根链启用单个技能。
*/
FfiOwnedBuffer luaskills_ffi_enable_skill_json(FfiBorrowedBuffer input_json);

/*
Enable one skill through one ordered root chain with host-injected system authority.
通过一条有序根链和宿主注入的 system 权限启用单个技能。
*/
FfiOwnedBuffer luaskills_ffi_system_enable_skill_json(FfiBorrowedBuffer input_json);

/*
Uninstall one skill through one ordered root chain.
通过一条有序根链卸载单个技能。
*/
FfiOwnedBuffer luaskills_ffi_uninstall_skill_json(FfiBorrowedBuffer input_json);

/*
Uninstall one skill through one ordered root chain with host-injected system authority.
通过一条有序根链和宿主注入的 system 权限卸载单个技能。
*/
FfiOwnedBuffer luaskills_ffi_system_uninstall_skill_json(FfiBorrowedBuffer input_json);

/*
Install one managed skill through one ordered root chain.
通过一条有序根链安装单个受管技能。
*/
FfiOwnedBuffer luaskills_ffi_install_skill_json(FfiBorrowedBuffer input_json);

/*
Install one managed skill through one ordered root chain with host-injected system authority.
通过一条有序根链和宿主注入的 system 权限安装单个受管技能。
*/
FfiOwnedBuffer luaskills_ffi_system_install_skill_json(FfiBorrowedBuffer input_json);

/*
Install one private URL-manifest skill through a host-private system JSON entrypoint.
通过宿主私有 system JSON 入口安装单个私有 URL manifest 技能。
*/
FfiOwnedBuffer luaskills_ffi_system_private_install_skill_from_url_manifest_json(FfiBorrowedBuffer input_json);

/*
Update one managed skill through one ordered root chain.
通过一条有序根链更新单个受管技能。
*/
FfiOwnedBuffer luaskills_ffi_update_skill_json(FfiBorrowedBuffer input_json);

/*
Update one managed skill through one ordered root chain with host-injected system authority.
通过一条有序根链和宿主注入的 system 权限更新单个受管技能。
*/
FfiOwnedBuffer luaskills_ffi_system_update_skill_json(FfiBorrowedBuffer input_json);

/*
Update one private URL-manifest skill through a host-private system JSON entrypoint.
通过宿主私有 system JSON 入口更新单个私有 URL manifest 技能。
*/
FfiOwnedBuffer luaskills_ffi_system_private_update_skill_from_url_manifest_json(FfiBorrowedBuffer input_json);

#ifdef __cplusplus
}
#endif

#endif
