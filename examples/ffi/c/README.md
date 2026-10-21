# C FFI Demo

## 1. 目录作用

这个目录提供一个最小 `C` 标准 ABI 示例，用于演示：

- 通过 [luaskills_ffi.h](../../../include/luaskills_ffi.h) 接入底层标准 C ABI
- 查询 `version`
- 创建 `engine`
- `load_from_roots`
- `list_entries`
- `call_skill`
- `run_lua`
- 释放 `engine`

对应源码：

- [demo.c](demo.c)

## 2. 运行前提

需要先在仓库根目录构建 `luaskills`：

```powershell
cargo build
```

如果您希望改用自定义运行时根目录，可以设置：

```powershell
$env:LUASKILLS_DEMO_ROOT = "D:\custom\runtime_root"
```

否则示例默认使用：

```text
examples/ffi/standard_runtime/runtime_root
```

## 3. 编译方式

当前仓库没有绑定某一个固定 C 工具链，因此这里给出的是**典型命令形态**。  
请根据本机工具链与 Rust 产物类型调整最终命令。

### MSVC 典型命令

```powershell
cl /utf-8 /std:c11 /I include examples\ffi\c\demo.c /link /LIBPATH:target\debug luaskills.dll.lib
```

### MinGW GCC 典型命令

```powershell
gcc -std=c11 -Iinclude examples/ffi/c/demo.c -Ltarget/debug -lluaskills -o examples/ffi/c/demo.exe
```

## 4. 运行方式

从仓库根目录执行：

```powershell
.\examples\ffi\c\demo.exe
```

运行成功后应看到：

1. `Version: ...`
2. `Engine created: ...`
3. `Loaded roots from: ...`
4. `Entry count: ...`
5. 若当前夹具根目录返回了入口，还会继续输出首个入口和首个参数预览
6. `Call content: standard-ffi-demo:c`
7. `Run Lua result JSON: ...`
8. `Engine freed`

## 5. 示例定位

这个示例当前聚焦标准 ABI 下的：

- 引擎生命周期
- 根链加载
- 结构化入口读取
- `FfiBorrowedBuffer` 请求输入
- `call_skill` 结构化结果读取
- `run_lua` JSON 结果读取

它仍然不负责演示：

- `install/update/uninstall`

如果需要看高层动态语言入口，请参考：

- [Python demo](../python/demo.py)
- [Go demo](../go/demo.go)
- [TypeScript demo](../typescript/demo.ts)
- [Standard Runtime Fixture](../standard_runtime/README.md)
- [FFI Demo Runtime](../demo_runtime/README.md)
