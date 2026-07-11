# TypeScript 标准 FFI 示例

当前目录提供基于 `koffi` 的 TypeScript 标准 C ABI 示例：

- `demo.ts`
- `lifecycle_demo.ts`
- `query_demo.ts`
- `runtime_lease_demo.ts`

## 1. 安装依赖

示例要求 Node.js `24.18.0` 或更高版本，并使用 Node 原生 TypeScript 类型剥离执行，不依赖额外的开发服务器或转译运行器。从当前目录执行：

```powershell
npm install
```

提交前执行独立类型检查：

```powershell
npm run typecheck
```

## 2. 运行前准备

需要先设置动态库路径环境变量：

```powershell
$env:LUASKILLS_LIB="D:\projects\luaskills\target\debug\luaskills.dll"
```

如果您要运行标准运行时夹具相关示例，还需要保证：

- 仓库已经完成 `cargo build`
- [../standard_runtime/README.md](../standard_runtime/README.md) 中的夹具目录可用

## 3. 运行方式

主调用链示例：

```powershell
npm run demo
```

生命周期示例：

```powershell
npm run lifecycle
```

查询辅助示例：

```powershell
npm run query
```

持久运行时租约示例：

```powershell
npm run runtime-lease
```
