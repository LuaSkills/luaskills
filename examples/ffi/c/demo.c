#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#if defined(_WIN32)
#include <direct.h>
#else
#include <sys/stat.h>
#include <sys/types.h>
#endif

#include "luaskills_ffi.h"

/*
Print one fatal message and terminate the process.
打印一条致命错误信息并终止进程。
*/
static void exit_with_message(const char *message) {
    fprintf(stderr, "C FFI demo failed: %s\n", message);
    exit(EXIT_FAILURE);
}

/*
Print one owned UTF-8 error buffer, free it, and terminate the process.
打印一个拥有型 UTF-8 错误缓冲、释放它并终止进程。
*/
static void exit_with_owned_error(const char *fallback_message, FfiOwnedBuffer error_buffer) {
    if (error_buffer.ptr != NULL && error_buffer.len > 0) {
        fprintf(
            stderr,
            "C FFI demo failed: %.*s\n",
            (int)error_buffer.len,
            (const char *)error_buffer.ptr
        );
        luaskills_ffi_buffer_free(error_buffer);
        exit(EXIT_FAILURE);
    }
    exit_with_message(fallback_message);
}

/*
Create one directory when it does not already exist.
在目录不存在时创建该目录。
*/
static void ensure_directory(const char *path) {
#if defined(_WIN32)
    int status = _mkdir(path);
#else
    int status = mkdir(path, 0755);
#endif
    if (status == 0 || errno == EEXIST) {
        return;
    }
    exit_with_message("failed to create demo runtime directory");
}

/*
Build one child path below the shared demo runtime root.
在共享演示运行时根目录下拼接一个子路径。
*/
static void build_runtime_subpath(
    char *buffer,
    size_t buffer_len,
    const char *runtime_root,
    const char *suffix
) {
    int written = snprintf(buffer, buffer_len, "%s/%s", runtime_root, suffix);
    if (written < 0 || (size_t)written >= buffer_len) {
        exit_with_message("demo runtime path is too long");
    }
}

/*
Return the shared runtime root used by the repository FFI demos.
返回仓库 FFI 演示共用的运行时根目录。
*/
static const char *resolve_demo_runtime_root(void) {
    const char *runtime_root = getenv("LUASKILLS_DEMO_ROOT");
    if (runtime_root != NULL && runtime_root[0] != '\0') {
        return runtime_root;
    }
    return "examples/ffi/standard_runtime/runtime_root";
}

/*
Create the shared demo runtime directory layout before engine creation.
在创建引擎前创建共享演示运行时目录结构。
*/
static void ensure_demo_runtime_layout(const char *runtime_root) {
    static const char *const relative_paths[] = {
        "",
        "skills",
        "dependencies",
        "state",
        "databases",
        "temp",
        "resources",
        "lua_packages",
        "bin",
        "bin/tools",
        "libs"
    };
    size_t index = 0;
    for (index = 0; index < sizeof(relative_paths) / sizeof(relative_paths[0]); index += 1) {
        char directory_path[1024];
        if (relative_paths[index][0] == '\0') {
            snprintf(directory_path, sizeof(directory_path), "%s", runtime_root);
        } else {
            build_runtime_subpath(
                directory_path,
                sizeof(directory_path),
                runtime_root,
                relative_paths[index]
            );
        }
        ensure_directory(directory_path);
    }
}

/*
Print one owned UTF-8 buffer with one label and free it.
输出一个带标签的拥有型 UTF-8 缓冲并释放它。
*/
static void print_owned_text(const char *label, FfiOwnedBuffer text_buffer) {
    if (text_buffer.ptr == NULL) {
        printf("%s\n", label);
        return;
    }
    printf("%s%.*s\n", label, (int)text_buffer.len, (const char *)text_buffer.ptr);
    luaskills_ffi_buffer_free(text_buffer);
}

/*
Print one nested UTF-8 buffer field without freeing it immediately.
输出一个嵌套 UTF-8 缓冲字段但不立即释放。
*/
static void print_nested_text(const char *label, FfiOwnedBuffer text_buffer) {
    if (text_buffer.ptr == NULL) {
        printf("%s\n", label);
        return;
    }
    printf("%s%.*s\n", label, (int)text_buffer.len, (const char *)text_buffer.ptr);
}

/*
Print one compact preview of the first loaded entry and its first parameter.
输出第一个已加载入口及其首个参数的紧凑预览。
*/
static void print_entry_preview(const FfiRuntimeEntryDescriptorList *entry_list) {
    const FfiRuntimeEntryDescriptor *first_entry = NULL;
    const FfiRuntimeEntryParameterDescriptor *first_parameter = NULL;
    printf("Entry count: %zu\n", entry_list->len);
    if (entry_list->len == 0 || entry_list->items == NULL) {
        printf("No entries were returned by the current fixture root.\n");
        return;
    }

    first_entry = &entry_list->items[0];
    print_nested_text("First canonical entry: ", first_entry->canonical_name);
    print_nested_text("First entry skill id: ", first_entry->skill_id);
    print_nested_text("First entry description: ", first_entry->description);
    print_nested_text("First entry input schema: ", first_entry->input_schema_json);
    printf("First entry parameter count: %zu\n", first_entry->parameters_len);

    if (first_entry->parameters_len == 0 || first_entry->parameters == NULL) {
        return;
    }

    first_parameter = &first_entry->parameters[0];
    print_nested_text("First parameter name: ", first_parameter->name);
    print_nested_text("First parameter type: ", first_parameter->param_type);
    printf("First parameter required: %s\n", first_parameter->required != 0 ? "true" : "false");
}

/*
Build one borrowed UTF-8 buffer from one existing C string.
从一个现有 C 字符串构造借用型 UTF-8 缓冲。
*/
static FfiBorrowedBuffer borrowed_buffer_from_text(const char *text) {
    FfiBorrowedBuffer buffer;
    if (text == NULL) {
        buffer.ptr = NULL;
        buffer.len = 0;
        return buffer;
    }
    buffer.ptr = (const uint8_t *)text;
    buffer.len = strlen(text);
    return buffer;
}

/*
Print one compact preview of one standard invocation result.
输出一个标准调用结果的紧凑预览。
*/
static void print_invocation_preview(const FfiRuntimeInvocationResult *result) {
    if (result == NULL) {
        printf("Invocation result pointer is null.\n");
        return;
    }
    print_nested_text("Call content: ", result->content);
    printf("Call content bytes: %zu\n", result->content_bytes);
    printf("Call content lines: %zu\n", result->content_lines);
    print_nested_text("Call template hint: ", result->template_hint);
}

/*
Run one version query, one root load, one structured entry-list read, one standard call_skill roundtrip, and one standard run_lua roundtrip.
通过标准 C ABI 执行一次版本查询、一次根链加载、一次结构化入口列表读取、一次标准 call_skill 往返调用以及一次标准 run_lua 往返调用。
*/
static void run_standard_ffi_demo(void) {
    const char *runtime_root = resolve_demo_runtime_root();

    /*
    Allocate all runtime directory strings required by the host options.
    为宿主选项准备所有运行时目录字符串。
    */
    char temp_dir[1024];
    char resources_dir[1024];
    char lua_packages_dir[1024];
    char tool_root_dir[1024];
    char ffi_root_dir[1024];
    char skills_root_dir[1024];
    char download_cache_root[1024];

    /*
    Hold transient version and error buffers returned by the standard ABI.
    保存标准 ABI 返回的临时版本缓冲和错误缓冲。
    */
    FfiOwnedBuffer version_buffer = {0};
    FfiOwnedBuffer error_buffer = {0};

    /*
    Hold the created engine id for the create/free roundtrip.
    保存创建后的引擎标识以完成创建与释放往返调用。
    */
    uint64_t engine_id = 0;
    FfiRuntimeSkillRoot skill_roots[1];
    FfiRuntimeEntryDescriptorList *entry_list = NULL;
    FfiRuntimeInvocationResult *invocation_result = NULL;
    FfiOwnedBuffer run_lua_result_json = {0};
    const char *args_json_text = "{\"note\":\"c\"}";
    const char *run_lua_args_json_text = "{\"note\":\"c-lua\"}";
    const char *request_context_json_text = "{\"transport_name\":\"c-demo\"}";
    const char *client_budget_json_text = "{\"budget\":1}";
    const char *tool_config_json_text = "{\"mode\":\"standard-demo\"}";
    const char *run_lua_code =
        "return { note = args.note, transport = vulcan.context.request.transport_name, budget = vulcan.context.client_budget.budget, mode = vulcan.context.tool_config.mode }";

    ensure_demo_runtime_layout(runtime_root);
    build_runtime_subpath(temp_dir, sizeof(temp_dir), runtime_root, "temp");
    build_runtime_subpath(resources_dir, sizeof(resources_dir), runtime_root, "resources");
    build_runtime_subpath(lua_packages_dir, sizeof(lua_packages_dir), runtime_root, "lua_packages");
    build_runtime_subpath(tool_root_dir, sizeof(tool_root_dir), runtime_root, "bin/tools");
    build_runtime_subpath(ffi_root_dir, sizeof(ffi_root_dir), runtime_root, "libs");
    build_runtime_subpath(skills_root_dir, sizeof(skills_root_dir), runtime_root, "skills");
    build_runtime_subpath(download_cache_root, sizeof(download_cache_root), runtime_root, "temp/downloads");

    if (luaskills_ffi_version(&version_buffer, &error_buffer) != 0) {
        exit_with_owned_error("failed to query FFI version", error_buffer);
    }
    print_owned_text("Version: ", version_buffer);

    /*
    Build one minimal host option set aligned with the repository smoke demos.
    构造一份与仓库烟测示例对齐的最小宿主选项集合。
    */
    FfiLuaRuntimeHostOptions host_options = {
        .temp_dir = temp_dir,
        .resources_dir = resources_dir,
        .lua_packages_dir = lua_packages_dir,
        .host_provided_tool_root = tool_root_dir,
        .host_provided_lua_root = lua_packages_dir,
        .host_provided_ffi_root = ffi_root_dir,
        .download_cache_root = download_cache_root,
        .dependency_dir_name = "dependencies",
        .state_dir_name = "state",
        .database_dir_name = "databases",
        .skill_config_file_path = NULL,
        .allow_network_download = 0,
        .github_base_url = NULL,
        .github_api_base_url = NULL,
        .sqlite_library_path = NULL,
        .sqlite_provider_mode = FFI_PROVIDER_MODE_DYNAMIC_LIBRARY,
        .sqlite_callback_mode = FFI_CALLBACK_MODE_STANDARD,
        .lancedb_library_path = NULL,
        .lancedb_provider_mode = FFI_PROVIDER_MODE_DYNAMIC_LIBRARY,
        .lancedb_callback_mode = FFI_CALLBACK_MODE_STANDARD,
        .space_controller_endpoint = NULL,
        .space_controller_auto_spawn = 0,
        .space_controller_executable_path = NULL,
        .space_controller_process_mode = FFI_SPACE_CONTROLLER_PROCESS_MODE_SERVICE,
        .cache_config = NULL,
        .runlua_pool_config = NULL,
        .reserved_entry_names = NULL,
        .reserved_entry_names_len = 0,
        .ignored_skill_ids = NULL,
        .ignored_skill_ids_len = 0,
        .enable_skill_management_bridge = 0,
        .default_text_encoding = NULL,
        .disable_managed_io_compat = 0
    };
    FfiLuaEngineOptions engine_options = {
        .pool = {
            .min_size = 1,
            .max_size = 1,
            .idle_ttl_secs = 30
        },
        .host = host_options
    };

    error_buffer = (FfiOwnedBuffer){0};
    if (luaskills_ffi_engine_new(&engine_options, &engine_id, &error_buffer) != 0) {
        exit_with_owned_error("failed to create engine", error_buffer);
    }
    printf("Engine created: %llu\n", (unsigned long long)engine_id);

    /*
    Load one physical ROOT skill directory before listing runtime entries.
    在列出运行时入口前加载一个物理 ROOT 技能目录。
    */
    skill_roots[0].name = "ROOT";
    skill_roots[0].skills_dir = skills_root_dir;
    error_buffer = (FfiOwnedBuffer){0};
    if (
        luaskills_ffi_load_from_roots(
            engine_id,
            skill_roots,
            1,
            &error_buffer
        ) != 0
    ) {
        exit_with_owned_error("failed to load skill roots", error_buffer);
    }
    printf("Loaded roots from: %s\n", skills_root_dir);

    /*
    Request one structured entry list and print the first entry preview.
    请求一个结构化入口列表并输出首个入口预览。
    */
    error_buffer = (FfiOwnedBuffer){0};
    if (luaskills_ffi_list_entries(engine_id, LUASKILLS_SKILL_AUTHORITY_SYSTEM, &entry_list, &error_buffer) != 0) {
        exit_with_owned_error("failed to list runtime entries", error_buffer);
    }
    if (entry_list == NULL) {
        exit_with_message("entry list pointer is null");
    }
    print_entry_preview(entry_list);
    luaskills_ffi_entry_list_free(entry_list);

    /*
    Call one real fixture skill entry through borrowed JSON buffers and one structured invocation result.
    通过借用型 JSON 缓冲和一个结构化调用结果来调用真实夹具技能入口。
    */
    error_buffer = (FfiOwnedBuffer){0};
    {
        FfiLuaInvocationContext invocation_context = {
            .request_context_json = borrowed_buffer_from_text(request_context_json_text),
            .client_budget_json = borrowed_buffer_from_text(client_budget_json_text),
            .tool_config_json = borrowed_buffer_from_text(tool_config_json_text)
        };
        if (
            luaskills_ffi_call_skill(
                engine_id,
                "demo-standard-ffi-skill-ping",
                borrowed_buffer_from_text(args_json_text),
                &invocation_context,
                &invocation_result,
                &error_buffer
            ) != 0
        ) {
            exit_with_owned_error("failed to call fixture skill", error_buffer);
        }
    }
    if (invocation_result == NULL) {
        exit_with_message("invocation result pointer is null");
    }
    print_invocation_preview(invocation_result);
    luaskills_ffi_invocation_result_free(invocation_result);

    /*
    Run one Lua snippet through borrowed JSON buffers and print the JSON result payload.
    通过借用型 JSON 缓冲执行一段 Lua 代码并输出 JSON 结果载荷。
    */
    error_buffer = (FfiOwnedBuffer){0};
    {
        FfiLuaInvocationContext invocation_context = {
            .request_context_json = borrowed_buffer_from_text(request_context_json_text),
            .client_budget_json = borrowed_buffer_from_text(client_budget_json_text),
            .tool_config_json = borrowed_buffer_from_text(tool_config_json_text)
        };
        if (
            luaskills_ffi_run_lua(
                engine_id,
                run_lua_code,
                borrowed_buffer_from_text(run_lua_args_json_text),
                &invocation_context,
                &run_lua_result_json,
                &error_buffer
            ) != 0
        ) {
            exit_with_owned_error("failed to run fixture lua snippet", error_buffer);
        }
    }
    print_owned_text("Run Lua result JSON: ", run_lua_result_json);

    error_buffer = (FfiOwnedBuffer){0};
    if (luaskills_ffi_engine_free(engine_id, &error_buffer) != 0) {
        exit_with_owned_error("failed to free engine", error_buffer);
    }
    printf("Engine freed\n");
}

/*
Run the minimal standard C ABI demo entrypoint.
运行标准 C ABI 演示入口。
*/
int main(void) {
    run_standard_ffi_demo();
    return EXIT_SUCCESS;
}
