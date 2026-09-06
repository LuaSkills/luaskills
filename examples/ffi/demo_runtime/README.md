# FFI Demo Runtime

## 1. 目录作用

这个目录提供一套最小可运行的 LuaSkills FFI 演示环境，用于：

- 创建空运行时目录
- 通过 FFI 创建引擎
- 通过 system install 动态安装 `LuaSkills/demo-skill`
- 调用 `demo-skill-demo-status`
- 输出 success 级别的烟测结果

注意：这个 smoke demo 使用单一 `ROOT` 演示 root 验证 system 安装链路，并显式注入 `authority = "system"`，不代表正式产品的用户可见层级设计。正式宿主启动或加载时必须传入 `ROOT` root；若要开放普通用户安装，应额外传入 `PROJECT` 或 `USER`，并让普通 install/update/uninstall 只落到这些层。若把 system tools 封装给普通 tools，应固定注入 `DelegatedTool` authority，不要把 `ROOT` 级调整能力暴露给普通用户。

## 2. 目录结构

- [D:\projects\vulcan-luaskills\examples\ffi\demo_runtime\runtime_root](/D:/projects/vulcan-luaskills/examples/ffi/demo_runtime/runtime_root)
  - 演示使用的空运行时根目录
- [D:\projects\vulcan-luaskills\examples\ffi\demo_runtime\run_python_install_demo.py](/D:/projects/vulcan-luaskills/examples/ffi/demo_runtime/run_python_install_demo.py)
  - 可直接运行的 Python FFI 安装与调用烟测脚本

## 3. 前置条件

运行前需要准备：

1. 已构建好 `luaskills` 动态库
2. 设置环境变量 `LUASKILLS_LIB`
3. 当前网络可访问 GitHub Release

## 4. 运行方式

```powershell
python .\examples\ffi\demo_runtime\run_python_install_demo.py
```

运行成功后，脚本会：

1. 清理 demo skill 在当前 runtime root 下的旧安装痕迹
2. 创建引擎
3. 加载包含 `ROOT` 的 root 链
4. 通过 `luaskills_ffi_system_install_skill_json` 动态安装 `LuaSkills/demo-skill` 到 `ROOT`
5. 调用 `demo-skill-demo-status`
6. 校验返回结果
7. 输出 success

## 5. 说明

当前 smoke demo 选择 Python 是因为：

- ctypes 接入成本最低
- JSON FFI 已切到 `FfiBorrowedBuffer + FfiOwnedBuffer`，调试体验最好
- 便于快速验证 install / call 主链

Go 和 TypeScript 示例仍保留在：

- [D:\projects\vulcan-luaskills\examples\ffi\go\demo.go](/D:/projects/vulcan-luaskills/examples/ffi/go/demo.go)
- [D:\projects\vulcan-luaskills\examples\ffi\typescript\demo.ts](/D:/projects/vulcan-luaskills/examples/ffi/typescript/demo.ts)

它们当前主要用于说明最小标准 FFI 对接方式。
