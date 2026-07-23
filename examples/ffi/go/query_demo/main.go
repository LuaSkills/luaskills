package main

/*
#cgo CFLAGS: -I../../../../include
#include <stdlib.h>
#include "luaskills_ffi.h"
*/
import "C"

import (
	"fmt"
	"os"
	"path/filepath"
	"runtime"
	"unsafe"
)

// mustOK raises one panic when the standard FFI call reports failure.
// mustOK 在标准 FFI 调用报告失败时抛出一个 panic。
func mustOK(status C.int32_t, errorOut C.FfiOwnedBuffer) {
	if status == 0 {
		return
	}
	var message string
	if errorOut.ptr != nil && errorOut.len > 0 {
		message = readOwnedBufferText(errorOut)
	}
	C.luaskills_ffi_buffer_free(errorOut)
	if message == "" {
		message = "Unknown FFI error"
	}
	panic(message)
}

// readOwnedBufferText reads one nested UTF-8 owned buffer without freeing it immediately.
// readOwnedBufferText 读取一个嵌套 UTF-8 拥有型缓冲但不立即释放。
func readOwnedBufferText(buffer C.FfiOwnedBuffer) string {
	if buffer.ptr == nil || buffer.len == 0 {
		return ""
	}
	return string(C.GoBytes(unsafe.Pointer(buffer.ptr), C.int(buffer.len)))
}

// readStringArray copies one returned string-array structure into Go strings.
// readStringArray 将一个返回的字符串数组结构复制为 Go 字符串切片。
func readStringArray(values *C.FfiStringArray) []string {
	if values == nil || values.items == nil || values.len == 0 {
		return []string{}
	}
	valueSlice := unsafe.Slice(values.items, int(values.len))
	results := make([]string, 0, len(valueSlice))
	for _, item := range valueSlice {
		results = append(results, readOwnedBufferText(item))
	}
	return results
}

// standardFixtureRuntimeRoot resolves the dedicated standard-ABI fixture runtime root.
// standardFixtureRuntimeRoot 解析标准 ABI 专用夹具运行时根目录。
func standardFixtureRuntimeRoot() string {
	_, currentFile, _, ok := runtime.Caller(0)
	if !ok {
		panic("failed to resolve query main.go path")
	}
	return filepath.Clean(
		filepath.Join(filepath.Dir(currentFile), "..", "..", "standard_runtime", "runtime_root"),
	)
}

// ensureStandardFixtureLayout creates the shared fixture runtime layout when it is missing.
// ensureStandardFixtureLayout 在缺失时创建共享夹具运行时目录结构。
func ensureStandardFixtureLayout(root string) {
	for _, relativePath := range []string{
		"skills",
		"dependencies",
		"state",
		"databases",
		"temp",
		"resources",
		"lua_packages",
		filepath.Join("bin", "tools"),
		"libs",
	} {
		if err := os.MkdirAll(filepath.Join(root, relativePath), 0o755); err != nil {
			panic(err)
		}
	}
}

// main demonstrates query-helper APIs through the standard ABI surface.
// main 演示通过标准 ABI 接口调用查询辅助能力。
func main() {
	root := standardFixtureRuntimeRoot()
	ensureStandardFixtureLayout(root)

	host := C.FfiLuaRuntimeHostOptions{
		temp_dir:                         C.CString(filepath.ToSlash(filepath.Join(root, "temp"))),
		resources_dir:                    C.CString(filepath.ToSlash(filepath.Join(root, "resources"))),
		lua_packages_dir:                 C.CString(filepath.ToSlash(filepath.Join(root, "lua_packages"))),
		host_provided_tool_root:          C.CString(filepath.ToSlash(filepath.Join(root, "bin", "tools"))),
		host_provided_lua_root:           C.CString(filepath.ToSlash(filepath.Join(root, "lua_packages"))),
		host_provided_ffi_root:           C.CString(filepath.ToSlash(filepath.Join(root, "libs"))),
		download_cache_root:              C.CString(filepath.ToSlash(filepath.Join(root, "temp", "downloads"))),
		dependency_dir_name:              C.CString("dependencies"),
		state_dir_name:                   C.CString("state"),
		database_dir_name:                C.CString("databases"),
		skill_config_root:                nil,
		skill_config_lock_timeout_ms:      0,
		skill_config_watch_debounce_ms:    0,
		allow_network_download:           0,
		github_base_url:                  nil,
		github_api_base_url:              nil,
		sqlite_library_path:              nil,
		sqlite_provider_mode:             0,
		sqlite_callback_mode:             0,
		lancedb_library_path:             nil,
		lancedb_provider_mode:            0,
		lancedb_callback_mode:            0,
		space_controller_endpoint:        nil,
		space_controller_auto_spawn:      0,
		space_controller_executable_path: nil,
		space_controller_process_mode:    0,
		cache_config:                     nil,
		runlua_pool_config:               nil,
		reserved_entry_names:             nil,
		reserved_entry_names_len:         0,
		ignored_skill_ids:                nil,
		ignored_skill_ids_len:            0,
		enable_skill_management_bridge:   0,
		default_text_encoding:            nil,
		disable_managed_io_compat:        0,
	}
	defer C.free(unsafe.Pointer(host.temp_dir))
	defer C.free(unsafe.Pointer(host.resources_dir))
	defer C.free(unsafe.Pointer(host.lua_packages_dir))
	defer C.free(unsafe.Pointer(host.host_provided_tool_root))
	defer C.free(unsafe.Pointer(host.host_provided_lua_root))
	defer C.free(unsafe.Pointer(host.host_provided_ffi_root))
	defer C.free(unsafe.Pointer(host.download_cache_root))
	defer C.free(unsafe.Pointer(host.dependency_dir_name))
	defer C.free(unsafe.Pointer(host.state_dir_name))
	defer C.free(unsafe.Pointer(host.database_dir_name))

	options := C.FfiLuaEngineOptions{
		pool: C.FfiLuaVmPoolConfig{
			min_size:      1,
			max_size:      1,
			idle_ttl_secs: 30,
		},
		host: host,
	}

	var engineID C.uint64_t
	var errorOut C.FfiOwnedBuffer
	mustOK(C.luaskills_ffi_engine_new(&options, &engineID, &errorOut), errorOut)
	fmt.Println("Engine created:", uint64(engineID))

	rootName := C.CString("ROOT")
	skillsDir := C.CString(filepath.ToSlash(filepath.Join(root, "skills")))
	defer C.free(unsafe.Pointer(rootName))
	defer C.free(unsafe.Pointer(skillsDir))
	skillRoots := []C.FfiRuntimeSkillRoot{
		{
			name:       rootName,
			skills_dir: skillsDir,
		},
	}
	errorOut = C.FfiOwnedBuffer{}
	mustOK(
		C.luaskills_ffi_load_from_roots(
			engineID,
			(*C.FfiRuntimeSkillRoot)(unsafe.Pointer(&skillRoots[0])),
			C.size_t(len(skillRoots)),
			&errorOut,
		),
		errorOut,
	)
	fmt.Println("Loaded roots from:", filepath.ToSlash(filepath.Join(root, "skills")))

	toolName := C.CString("demo-standard-ffi-skill-ping")
	argumentName := C.CString("note")
	defer C.free(unsafe.Pointer(toolName))
	defer C.free(unsafe.Pointer(argumentName))

	var isSkillValue C.uint8_t
	errorOut = C.FfiOwnedBuffer{}
	mustOK(
		C.luaskills_ffi_is_skill(
			engineID,
			C.LUASKILLS_SKILL_AUTHORITY_SYSTEM,
			toolName,
			&isSkillValue,
			&errorOut,
		),
		errorOut,
	)
	fmt.Println("Is skill tool:", isSkillValue != 0)

	var skillIDOut C.FfiOwnedBuffer
	errorOut = C.FfiOwnedBuffer{}
	mustOK(
		C.luaskills_ffi_skill_name_for_tool(
			engineID,
			C.LUASKILLS_SKILL_AUTHORITY_SYSTEM,
			toolName,
			&skillIDOut,
			&errorOut,
		),
		errorOut,
	)
	fmt.Println("Owning skill id:", readOwnedBufferText(skillIDOut))
	C.luaskills_ffi_buffer_free(skillIDOut)

	var valuesOut *C.FfiStringArray
	errorOut = C.FfiOwnedBuffer{}
	mustOK(
		C.luaskills_ffi_prompt_argument_completions(
			engineID,
			C.LUASKILLS_SKILL_AUTHORITY_SYSTEM,
			toolName,
			argumentName,
			&valuesOut,
			&errorOut,
		),
		errorOut,
	)
	if valuesOut != nil {
		defer C.luaskills_ffi_string_array_free(valuesOut)
	}
	values := readStringArray(valuesOut)
	fmt.Println("Prompt completion count:", len(values))
	fmt.Println("Prompt completions:", values)

	errorOut = C.FfiOwnedBuffer{}
	mustOK(C.luaskills_ffi_engine_free(engineID, &errorOut), errorOut)
	fmt.Println("Engine freed")
}
