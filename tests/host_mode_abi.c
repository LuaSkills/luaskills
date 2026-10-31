/* Compile the additive host-mode ABI as C11 without requiring a linked runtime.
   将增量宿主模式 ABI 作为 C11 编译，无需链接运行库。 */
#include "../include/luaskills_ffi.h"
#include "../include/luaskills_json_ffi.h"

/* SQLite registration has one context pointer and two callback transports.
   SQLite 注册包含一个上下文指针及两种回调传输。 */
_Static_assert(sizeof(FfiDatabaseProviderCallbacksV1) == 3 * sizeof(void *), "SQLite-only callback descriptor");
_Static_assert(offsetof(FfiDatabaseProviderCallbacksV1, sqlite) == sizeof(void *), "structured callback offset");
_Static_assert(offsetof(FfiDatabaseProviderCallbacksV1, sqlite_json) == 2 * sizeof(void *), "JSON callback offset");

_Static_assert(offsetof(FfiLuaEngineOptionsV4, base) == 0, "v3 prefix offset");
_Static_assert(offsetof(FfiLuaEngineOptionsV4, host_mode) == sizeof(FfiLuaEngineOptionsV3), "host mode follows the intact v3 input");
_Static_assert(sizeof(((FfiLuaEngineOptionsV4 *)0)->host_mode) == sizeof(int32_t), "mode is a fixed-width integer");
_Static_assert(LUASKILLS_HOST_MODE_CONSOLE == 0 && LUASKILLS_HOST_MODE_DESKTOP == 1, "host-mode wire values");

/* Verify the new function signature while retaining all prior creation declarations; returns no value.
   保留所有先前创建声明的同时验证新增函数签名；无返回值。 */
void verify_host_mode_abi(void) {
    /* The pointer verifies v4 calling convention and exact output ownership types at compilation.
       此指针在编译时验证 v4 调用约定及精确输出所有权类型。 */
    int32_t (*create_v4)(const FfiLuaEngineOptionsV4 *, uint64_t *, FfiOwnedBuffer *) = luaskills_ffi_engine_new_v4;
    (void)create_v4;
    /* Compile the instance-registration signature against the same public header.
       按同一公开头文件编译实例注册签名。 */
    int32_t (*create_with_database)(const FfiLuaEngineOptionsV4 *, const FfiDatabaseProviderCallbacksV1 *, uint64_t *, FfiOwnedBuffer *) = luaskills_ffi_engine_new_with_database_providers_v1;
    (void)create_with_database;
    (void)&luaskills_ffi_engine_new;
    (void)&luaskills_ffi_engine_new_v2;
    (void)&luaskills_ffi_engine_new_v3;
}
