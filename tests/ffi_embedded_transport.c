/* Exercise the installed headers against an explicitly selected dynamic library. */
/* 针对显式选择的动态库验证安装头文件。 */
#include "luaskills_json_ffi.h"
#include <stdio.h>
#include <string.h>

#ifdef _WIN32
#include <windows.h>
/* Native loader owners and symbols retain the platform-defined representations. */
/* 原生加载器所有者与符号保留平台定义的表示。 */
typedef HMODULE Library;
typedef FARPROC Symbol;
#else
#include <dlfcn.h>
/* POSIX loader owners and symbols remain opaque until copied into typed function pointers. */
/* POSIX 加载器所有者与符号在复制到类型化函数指针前保持不透明。 */
typedef void *Library;
typedef void *Symbol;
#endif

/* Match the public declarations exactly; argument/result layout is tested by actual calls. */
/* 精确匹配公开声明；通过实际调用检验参数／结果布局。 */
typedef int32_t (*Create)(const FfiEmbeddedTransportConfigV1 *, uint64_t *);
typedef int32_t (*Close)(uint64_t);
typedef int32_t (*FreeTransport)(uint64_t);
typedef int32_t (*FreeResult)(uint64_t, FfiEmbeddedResultV1);
typedef int32_t (*Request)(uint64_t, FfiBorrowedBuffer, FfiEmbeddedResultV1 *);

/* Load the exact path without searching for alternate builds; return NULL on failure. */
/* 加载精确路径，不搜索其他构建；失败时返回 NULL。 */
static Library open_library(const char *path) {
#ifdef _WIN32
    return LoadLibraryA(path);
#else
    return dlopen(path, RTLD_NOW | RTLD_LOCAL);
#endif
}

/* Resolve name only from library; return NULL if the declared ABI is unavailable. */
/* 仅从 library 解析 name；声明 ABI 不可用时返回 NULL。 */
static Symbol find_symbol(Library library, const char *name) {
#ifdef _WIN32
    return GetProcAddress(library, name);
#else
    return dlsym(library, name);
#endif
}

/* Unload library only after every synchronous call and owned result has completed. */
/* 仅在每个同步调用与拥有的结果均完成后卸载 library。 */
static void close_library(Library library) {
#ifdef _WIN32
    FreeLibrary(library);
#else
    dlclose(library);
#endif
}

/* Fail with the exact expression; the test process exits rather than unloading live ownership. */
/* 失败时报告精确表达式；测试进程退出，不卸载仍有活动所有权的动态库。 */
#define CHECK(expression) do { \
    if (!(expression)) { \
        fprintf(stderr, "failed: %s\n", #expression); \
        return 1; \
    } \
} while (0)

/* Preserve the native symbol representation while requiring the function-pointer width to match. */
/* 保留原生符号表示，同时要求函数指针宽度匹配。 */
#define LOAD(variable, name) do { \
    Symbol symbol = find_symbol(library, name); \
    _Static_assert(sizeof(variable) == sizeof(symbol), "loader symbol width differs from function pointer"); \
    CHECK(symbol != NULL); \
    memcpy(&(variable), &symbol, sizeof(variable)); \
} while (0)

/* Validate argv[1]'s real C ABI, result ownership, and close protocol; return zero only after unload. */
/* 验证 argv[1] 的真实 C ABI、结果所有权与关闭协议；仅在卸载后返回零。 */
int main(int argc, char **argv) {
    /* The caller provides an exact path to the artifact being accepted. */
    /* 调用方提供被验收产物的精确路径。 */
    CHECK(argc == 2);
    Library library = open_library(argv[1]);
    CHECK(library != NULL);
    /* Typed native entrypoints are resolved from that one artifact. */
    /* 类型化原生入口从这一个产物解析。 */
    Create create;
    Close close;
    FreeTransport free_transport;
    FreeResult free_result;
    Request request;
    LOAD(create, "luaskills_ffi_embedded_transport_new_v1");
    LOAD(close, "luaskills_ffi_embedded_transport_close_v1");
    LOAD(free_transport, "luaskills_ffi_embedded_transport_free_v1");
    LOAD(free_result, "luaskills_ffi_embedded_result_free_v1");
    LOAD(request, "luaskills_ffi_embedded_request_v1");
    /* Explicit native layout and positive budgets are consumed by Rust without a shim. */
    /* 显式原生布局与正数预算直接由 Rust 消费，无中间适配层。 */
    FfiEmbeddedTransportConfigV1 config = {
        sizeof(FfiEmbeddedTransportConfigV1), 1, 2, 2, 4096, 2048, 1024
    };
    uint64_t transport = 0;
    CHECK(create(&config, &transport) == LUASKILLS_EMBEDDED_OK);
    CHECK(transport != 0);
    /* Exact-length UTF-8 excludes the trailing C string terminator. */
    /* 精确长度 UTF-8 不包含末尾 C 字符串终止符。 */
    const uint8_t json[] = "{\"protocol_version\":1,\"command\":{\"type\":\"describe\"}}";
    FfiBorrowedBuffer input = {json, sizeof(json) - 1};
    FfiEmbeddedResultV1 result = {0};
    CHECK(request(transport, input, &result) == LUASKILLS_EMBEDDED_OK);
    CHECK(result.ptr != NULL && result.len > 0 && result.allocation_id != 0);
    CHECK(result.len < config.max_response_bytes);
    CHECK(result.ptr[0] == '{' && result.ptr[result.len - 1] == '}');
    CHECK(close(transport) == LUASKILLS_EMBEDDED_OK);
    CHECK(free_transport(transport) == LUASKILLS_EMBEDDED_BUSY);
    /* Shape tampering must not consume the original result's release authority. */
    /* 形状篡改不能消费原结果的释放权威。 */
    FfiEmbeddedResultV1 altered = result;
    altered.len++;
    CHECK(free_result(transport, altered) == LUASKILLS_EMBEDDED_INVALID_ARGUMENT);
    CHECK(free_result(transport, result) == LUASKILLS_EMBEDDED_OK);
    CHECK(free_result(transport, result) == LUASKILLS_EMBEDDED_NOT_FOUND);
    CHECK(free_transport(transport) == LUASKILLS_EMBEDDED_OK);
    CHECK(close(transport) == LUASKILLS_EMBEDDED_NOT_FOUND);
    close_library(library);
    puts("embedded transport C ABI: passed");
    return 0;
}
