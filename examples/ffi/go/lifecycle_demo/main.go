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

// makeBorrowedBuffer allocates one temporary borrowed UTF-8 buffer for one standard FFI request.
// makeBorrowedBuffer 为一次标准 FFI 请求分配一个临时借用型 UTF-8 缓冲。
func makeBorrowedBuffer(text string) (C.FfiBorrowedBuffer, unsafe.Pointer) {
	payload := []byte(text)
	if len(payload) == 0 {
		return C.FfiBorrowedBuffer{}, nil
	}
	storage := C.CBytes(payload)
	return C.FfiBorrowedBuffer{
		ptr: (*C.uint8_t)(storage),
		len: C.size_t(len(payload)),
	}, storage
}

// standardFixtureRuntimeRoot resolves the dedicated standard-ABI fixture runtime root.
// standardFixtureRuntimeRoot 解析标准 ABI 专用夹具运行时根目录。
func standardFixtureRuntimeRoot() string {
	_, currentFile, _, ok := runtime.Caller(0)
	if !ok {
		panic("failed to resolve lifecycle main.go path")
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

// printEntryCount loads the current entry list and returns its length.
// printEntryCount 读取当前入口列表并返回其长度。
func printEntryCount(engineID C.uint64_t) int {
	var entryList *C.FfiRuntimeEntryDescriptorList
	var errorOut C.FfiOwnedBuffer
	mustOK(C.luaskills_ffi_list_entries(engineID, C.LUASKILLS_SKILL_AUTHORITY_SYSTEM, &entryList, &errorOut), errorOut)
	if entryList == nil {
		fmt.Println("Current entry count: 0")
		return 0
	}
	defer C.luaskills_ffi_entry_list_free(entryList)
	entryCount := int(entryList.len)
	fmt.Println("Current entry count:", entryCount)
	return entryCount
}

// callFixtureSkill invokes the shared fixture entry and returns its textual content.
// callFixtureSkill 调用共享夹具入口并返回其文本内容。
func callFixtureSkill(engineID C.uint64_t, note string) string {
	argsBuffer, argsStorage := makeBorrowedBuffer(fmt.Sprintf(`{"note":"%s"}`, note))
	defer C.free(argsStorage)
	toolName := C.CString("demo-standard-ffi-skill-ping")
	defer C.free(unsafe.Pointer(toolName))

	var invocationResult *C.FfiRuntimeInvocationResult
	var errorOut C.FfiOwnedBuffer
	mustOK(
		C.luaskills_ffi_call_skill(
			engineID,
			toolName,
			argsBuffer,
			nil,
			&invocationResult,
			&errorOut,
		),
		errorOut,
	)
	if invocationResult == nil {
		panic("invocation result pointer is nil")
	}
	defer C.luaskills_ffi_invocation_result_free(invocationResult)
	return readOwnedBufferText(invocationResult.content)
}

// main demonstrates disable and enable lifecycle transitions through the standard ABI.
// main 演示通过标准 ABI 执行 disable 与 enable 生命周期切换。
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
		skill_config_lock_timeout_ms:     0,
		skill_config_watch_debounce_ms:   0,
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

	printEntryCount(engineID)
	fmt.Println("Call before disable:", callFixtureSkill(engineID, "before-disable"))

	skillID := C.CString("demo-standard-ffi-skill")
	disableReason := C.CString("maintenance window")
	defer C.free(unsafe.Pointer(skillID))
	defer C.free(unsafe.Pointer(disableReason))
	errorOut = C.FfiOwnedBuffer{}
	mustOK(
		C.luaskills_ffi_system_disable_skill(
			engineID,
			(*C.FfiRuntimeSkillRoot)(unsafe.Pointer(&skillRoots[0])),
			C.size_t(len(skillRoots)),
			C.LUASKILLS_SKILL_AUTHORITY_SYSTEM,
			skillID,
			disableReason,
			&errorOut,
		),
		errorOut,
	)
	fmt.Println("Skill disabled: demo-standard-ffi-skill")
	printEntryCount(engineID)

	disabledArgsBuffer, disabledArgsStorage := makeBorrowedBuffer(`{"note":"after-disable"}`)
	defer C.free(disabledArgsStorage)
	toolName := C.CString("demo-standard-ffi-skill-ping")
	defer C.free(unsafe.Pointer(toolName))
	var disabledInvocationResult *C.FfiRuntimeInvocationResult
	errorOut = C.FfiOwnedBuffer{}
	disabledStatus := C.luaskills_ffi_call_skill(
		engineID,
		toolName,
		disabledArgsBuffer,
		nil,
		&disabledInvocationResult,
		&errorOut,
	)
	if disabledStatus == 0 {
		panic("call_skill unexpectedly succeeded while the skill was disabled")
	}
	fmt.Println("Call after disable failed as expected:", readOwnedBufferText(errorOut))
	C.luaskills_ffi_buffer_free(errorOut)

	errorOut = C.FfiOwnedBuffer{}
	mustOK(
		C.luaskills_ffi_system_enable_skill(
			engineID,
			(*C.FfiRuntimeSkillRoot)(unsafe.Pointer(&skillRoots[0])),
			C.size_t(len(skillRoots)),
			C.LUASKILLS_SKILL_AUTHORITY_SYSTEM,
			skillID,
			&errorOut,
		),
		errorOut,
	)
	fmt.Println("Skill enabled: demo-standard-ffi-skill")
	printEntryCount(engineID)
	fmt.Println("Call after enable:", callFixtureSkill(engineID, "after-enable"))

	errorOut = C.FfiOwnedBuffer{}
	mustOK(C.luaskills_ffi_engine_free(engineID, &errorOut), errorOut)
	fmt.Println("Engine freed")
}
