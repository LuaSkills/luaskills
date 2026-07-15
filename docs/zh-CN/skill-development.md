# Lua Skill 开发手册

## 1. 文档目标

本文档面向 **Lua Skill 作者**，整理当前仓库里已经实际暴露给 Lua 的 `vulcan.*` 能力面，并说明：

- 当前有哪些 `vulcan.*` 能力可用
- 每项能力怎么调用
- 哪些能力默认可用，哪些依赖宿主注入
- 哪些字段属于内部实现细节，不建议 skill 直接依赖

本文档以当前代码实现为准，主要对应 `src/runtime/engine.rs` 中的 Lua 注入逻辑。

## 2. 快速结论

当前 Skill 运行时会暴露这些顶级能力：

- `vulcan.call`
- `vulcan.runtime.*`
- `vulcan.fs.*`
- `vulcan.path.*`
- `vulcan.process.*`
- `vulcan.os.*`
- `vulcan.json.*`
- `vulcan.cache.*`
- `vulcan.host.*`
- `vulcan.context.*`
- `vulcan.deps.*`
- `vulcan.sqlite.*`
- `vulcan.lancedb.*`

同时还有两个重要事实：

1. 当前运行时默认把 skill 当作**受信代码**执行，不提供沙箱安全承诺。
2. `vulcan.context.*`、`vulcan.deps.*`、`vulcan.sqlite.*`、`vulcan.lancedb.*` 中有一部分内容依赖宿主或当前 skill 绑定状态，不能假设始终存在有效值。
3. 宿主可以通过 `LuaRuntimeHostOptions.ignored_skill_ids` 强制忽略某些 skill，被忽略的 skill 不会进入依赖准备、数据库绑定或 entry 注册阶段。

## 2.1 Skill 命名与发布包规则

`skill_id` 是 LuaSkills 运行时、生命周期管理、配置命名空间、依赖目录、数据库绑定和 canonical entry 名称共同使用的稳定主键。当前规则是“目录名即 `skill_id`”，而不是由 `skill.yaml` 里的字段声明。

`skill_id` 和每个 `entry.name` 必须使用同一套标识符格式：

```text
^[a-z]([a-z0-9-]*[a-z0-9])?$
```

规则含义：

- 必须以小写 ASCII 字母开头。
- 后续只能包含小写 ASCII 字母、数字和连字符 `-`。
- 不能以下划线、数字、大写字母或连字符结尾。
- 合法示例：`vulcan-codekit`、`codekit2`、`vulcan-runtime-tools`。
- 非法示例：`2codekit`、`Vulcan-codekit`、`vulcan_codekit`、`vulcan-codekit-`。

Skill 包结构必须遵守：

- 物理目录名就是最终 `skill_id`，例如 `skills/vulcan-codekit/` 的 `skill_id` 是 `vulcan-codekit`。
- `skill.yaml` 不允许声明 `skill_id` 字段；如果出现该字段，运行时会拒绝加载。
- `skill.yaml` 的 `name` 是人类可读元数据，不参与 `skill_id` 匹配。
- 每个 `entries[].name` 是当前 skill 内部的局部入口名，也必须满足同一标识符规则。
- 运行时对外暴露的 canonical entry 名称为 `{skill_id}-{entry_name}`；若与宿主保留名称或其他入口冲突，会追加稳定数字后缀，形成 `{skill_id}-{entry_name}-{N}`。

GitHub 托管 skill 的安装与发布资产必须保持同一个 `skill_id`：

- 安装请求若未显式传入 `skill_id`，运行时会从 GitHub 仓库名派生 `skill_id`，不会自动剥离 `luaskills-` 前缀。
- release zip 文件名必须是 `{skill_id}-v{version}-skill.zip`。
- checksum 文件名必须是 `{skill_id}-v{version}-checksums.txt`。
- zip 内部只能包含与 `skill_id` 同名的顶层目录，并且必须包含 `{skill_id}/skill.yaml`。
- 仓库名、release 资产名前缀、checksum 文件名前缀、zip 顶层目录和最终安装目录应全部使用同一个 `skill_id`。

## 2.2 入口输入 Schema

LuaSkills 现在支持为每个入口声明一份完整的、面向 AI 的对象输入 schema。

推荐规则：

- `skill.yaml` 继续承担 `name`、`version`、`lua_entry`、help 链接等人类友好的元数据。
- 复杂工具输入 schema 建议放到 `schemas/` 目录下的外部 JSON 文件中。
- 当 schema 包含嵌套对象、带 `items` 的数组、`oneOf` / `anyOf`、严格 `additionalProperties` 规则时，应优先使用 `input_schema_file`。
- 旧版 `parameters` 仅作为向后兼容或简单扁平入口的声明方式保留。

当前入口 schema 字段：

- `parameters`：旧版扁平参数列表。当 `input_schema` 与 `input_schema_file` 都缺失时，运行时会自动把 `parameters` 投影成对象 schema。
- `input_schema`：可选的内联对象 schema，直接写在 `skill.yaml` 中。
- `input_schema_file`：位于 `schemas/` 目录下的可选相对 JSON 文件路径。对于非简单 schema，这是推荐格式。

约束规则：

- 最终入口输入 schema 必须是一个 JSON 对象 schema，并且根节点 `type` 必须是 `object`。
- 同一个入口不能同时声明 `input_schema` 和 `input_schema_file`。
- `input_schema_file` 必须是位于 `schemas/` 下的相对路径。
- 当 `parameters` 为空但存在完整 schema 时，LuaSkills 会从根 `properties` 自动推导一份旧版顶层参数预览，以兼容旧导出接口。

示例：

```yaml
entries:
  - name: node_source
    description: 读取指定节点。
    lua_entry: runtime/node_source.lua
    lua_module: demo.node_source
    input_schema_file: schemas/node_source.input.schema.json
```

```json
{
  "type": "object",
  "additionalProperties": false,
  "properties": {
    "nodes": {
      "type": "array",
      "items": {
        "type": "object",
        "properties": {
          "file": { "type": "string" },
          "structural_path": { "type": "string" }
        },
        "required": ["file", "structural_path"]
      }
    }
  },
  "required": ["nodes"]
}
```

## 2.3 托管身份字段契约

有些 skill 需要把多次 entry 调用绑定到同一个会话、任务或上下文状态。LuaSkills 保留 `LUASKILL_SID` 作为 skill entry 参数中的标准托管身份字段。

这是 LuaSkills 生态契约，不是 MCP 私有约定。运行时不会把 `LUASKILL_SID` 当成魔法字段处理；skill 代码会把它作为普通字符串参数接收。把 LuaSkills entry 投影成用户可见或模型可见工具的宿主和 adapter，负责决定该字段是可见、隐藏，还是自动注入。

Skill 作者规则：

- 新 skill 如果需要稳定会话、任务或上下文身份，应使用 entry 参数名 `LUASKILL_SID`。
- 该字段只用于状态连续性的身份句柄，不能作为鉴权 token、密钥、数据库凭证或权限边界。
- skill 不能假设所有宿主都能提供托管身份；help 文本应说明托管和非托管两种行为。
- 有状态 skill 如果能够建立或恢复状态，建议提供 create、start、open 或 bootstrap 类 entry。
- 如果调用方显式传入 `LUASKILL_SID`，skill 应复用该值，而不是生成新身份。
- 如果调用方未传 `LUASKILL_SID`，并且 skill 支持非托管 fallback，create/start/bootstrap entry 可以生成新的公开身份。
- 当 skill 生成公开身份时，结果必须显式返回该身份，并说明后续调用应继续传入同一个值。
- skill 可以建议把生成的公开身份保存到宿主或项目认可的规则位置，但不能在没有用户或宿主明确授权时主动写入。
- 必须依赖 `LUASKILL_SID` 的非 create 类 entry，在缺少该字段时应返回明确错误，并提示对应的 create/start/bootstrap 恢复路径。

宿主和 adapter 规则：

- 暴露 entry schema 时，应扫描输入参数中是否存在名为 `LUASKILL_SID` 的 property。
- 如果宿主能为当前对话、任务、工作区或等价上下文提供稳定托管身份，可以从模型/用户可见 schema 中隐藏 `LUASKILL_SID`，并从可见 `required` 列表中移除。
- 托管模式调用 entry 前，宿主必须把隐藏的 `LUASKILL_SID` 注入参数对象。
- 如果宿主希望 skill 在结果里识别并隐藏宿主管理身份，应把注入值包装为保留前缀 `LUASKILLS-SID-` 开头的形式。
- 注入值必须在目标宿主作用域内保持稳定，不能每次调用都重新随机生成。
- 如果宿主无法提供稳定托管身份，应保持 `LUASKILL_SID` 可见，让调用方或 skill fallback 流程处理。
- 托管模式的 help 应告诉模型或用户：身份由宿主注入，不应询问、打印或保存原始值。
- 如果工具结果包含被注入的原始托管身份，宿主应在返回给模型/用户前脱敏，或改写成托管状态说明。

这些规则适用于所有投影层，包括 MCP、gRPC、FFI/SDK 宿主、IDE 集成和嵌入式产品宿主。

## 2.4 托管项目路径字段契约

有些 skill 需要一个面向调用方可见的项目路径或工作区路径，而且这个路径应跟随宿主当前激活的项目作用域，而不是简单等于运行时进程的原始工作目录。LuaSkills 把 `PWD` 保留为这一用途的约定入口参数名。

这是 LuaSkills 生态层的兼容性公约，不是携带授权能力的魔法字段。运行时只识别 `PWD`，以便在 Lua 边界归一化其路径写法。把 LuaSkills entry 投影成用户可见或模型可见工具的宿主和 adapter，负责决定该字段是可见、隐藏，还是自动注入。

Skill 作者规则：

- 新 skill 如果需要一个由宿主或调用方提供的项目根路径或工作区根路径，应使用入口参数名 `PWD`。
- `PWD` 的语义应视为面向跨宿主兼容的项目/工作区路径契约，而不是权限证明、沙箱边界，也不是“运行时进程当前一定就在这个目录里”的保证。
- 如果 skill 的行为应跟随宿主当前激活的项目/工作区作用域，而不是运行时进程目录，应优先使用显式传入的 `PWD`，不要直接把 `vulcan.runtime.cwd()` 当作同义替代。
- skill 不能假设所有宿主都能注入 `PWD`；help 文本应说明托管和非托管两种行为。
- 如果调用方显式传入 `PWD`，skill 应复用该值，而不是自己覆盖成猜测路径。
- 如果调用方未传 `PWD`，并且 skill 有安全的 fallback，可以显式回退到 `vulcan.runtime.cwd()` 或其他已文档化的项目解析路径；但这种 fallback 必须在 help 和行为上说清楚。

宿主和 adapter 规则：

- 暴露 entry schema 时，应扫描输入参数中是否存在名为 `PWD` 的 property。
- 如果宿主能为当前项目、工作区或等价上下文提供稳定路径，应从模型/用户可见 schema 中隐藏 `PWD`，并从可见 `required` 列表中移除。
- 托管模式调用 entry 前，宿主应把当前项目路径或工作区路径注入到 `PWD`。
- 托管模式下，宿主应直接提供真实项目/工作区路径，而不是要求模型或用户再手工输入。
- 如果宿主无法提供稳定的当前项目路径或工作区路径，应保持 `PWD` 可见，让调用方自行提供。
- 这条规则的定位是“为不同宿主提供更好兼容性”的公约，而不是 LuaSkills 运行时内的硬限制；宿主可以渐进式采纳。

这些规则适用于所有投影层，包括 MCP、gRPC、FFI/SDK 宿主、IDE 集成和嵌入式产品宿主。

Windows 宿主取得的规范路径可能采用 `\\?\C:\...` 或 `\\?\UNC\server\share\...` 形式。LuaSkills 会在保留的顶层 `PWD` 参数进入 Lua 前移除该逐字路径前缀；其他已声明的运行时路径字段也遵循同一边界规则。任意 JSON 字符串不会仅因“看起来像路径”而被改写。只有绝对盘符与 UNC verbatim 形式具备经过证明的普通等价路径；卷 GUID、设备、管道、混合分隔符及其他 verbatim 命名空间会被显式拒绝，不会重新解释为相对路径。前缀清理只属于兼容转换，不代表授权或规范化。

## 3. 顶级能力总览

| 顶级项 | 作用 | 默认可用 | 备注 |
| --- | --- | --- | --- |
| `vulcan.call` | 调用其他 skill 入口 | 是 | 要求第二个参数必须是 Lua table |
| `vulcan.runtime` | 运行时辅助能力 | 是 | 包含日志、cwd、luaexec、skill 管理桥接等 |
| `vulcan.fs` | 文件系统读写 | 是 | 不做沙箱限制 |
| `vulcan.io` | Rust 托管 IO | 是 | 支持编码可控的文件读写、托管 `popen` 与 luaexec `io` 劫持 |
| `vulcan.path` | 路径拼接 | 是 | 返回对 Lua 友好的系统路径 |
| `vulcan.process` | 启动子进程 | 是 | 包含一次性 `exec` 与交互式 `session` |
| `vulcan.os` | 宿主 OS/架构信息 | 是 | `os`、`arch` |
| `vulcan.json` | JSON 编解码 | 是 | JSON/Lua 类型保真转换 |
| `vulcan.cache` | 运行时缓存 | 是 | 在 `vulcan.runtime.lua.exec` 中会被禁用 |
| `vulcan.models` | 标准模型能力 | 是 | 只有宿主注册对应 callback 后能力才会开启 |
| `vulcan.host` | 宿主注册工具桥接 | 是 | 宿主未注册 callback 时为空能力面 |
| `vulcan.context` | 请求与当前入口上下文 | 是 | 多数值由宿主注入 |
| `vulcan.deps` | 当前 skill 依赖根路径 | 是 | 未解析到当前 skill 时可能为 `nil` |
| `vulcan.sqlite` | 当前 skill 的 SQLite 绑定 | 条件可用 | 未启用时仍有 `enabled/status/info` |
| `vulcan.lancedb` | 当前 skill 的 LanceDB 绑定 | 条件可用 | 未启用时仍有 `enabled/status/info` |

## 3.1 托管 IO 与进程编码

`vulcan.io` 是 Rust 宿主管理的 IO 接口，优先用于 AI 生成代码、跨平台文件读写，以及 `luaexec` 隔离执行环境。对于 Windows 中文路径、编码控制、`io.popen` 输出解码这类场景，**应优先使用 `vulcan.io`，不要回退到原生 `io` / `os`。**

### 3.1.1 `vulcan.io.*` API 参考

| API | 参数与类型 | 返回值 | 说明 |
| --- | --- | --- | --- |
| `vulcan.io.open(path, mode?, options?)` | `path: string`；`mode?: string`，默认 `r`；`options?: string \| { encoding?: string }` | 托管文件句柄 | 支持 `r`、`w`、`a`、`rb`、`wb`、`ab`、`r+`、`w+`、`a+` 及其二进制组合 |
| `vulcan.io.read_text(path, options?)` | `path: string`；`options?: string \| { encoding?: string }` | `string` | 一次性读取整个文本文件 |
| `vulcan.io.write_text(path, content, options?)` | `path: string`；`content: string`；`options?: string \| { encoding?: string }` | `true` | 覆盖写入整个文本文件；不会自动创建父目录 |
| `vulcan.io.append_text(path, content, options?)` | 同上 | `true` | 以追加模式写入文本 |
| `vulcan.io.lines(path, options?)` | `path: string`；`options?: string \| { encoding?: string }` | 迭代器函数 | 逐行返回文本内容，EOF 时返回 `nil` |
| `vulcan.io.popen(command, mode?, options?)` | `command: string`；`mode?: string`，默认 `r`；`options?: string \| { encoding?: string, timeout_ms?: integer }` | 托管文件句柄 | 当前仅支持读取模式；写入模式尚未实现 |
| `vulcan.io.tmpfile()` | 无 | 托管文件句柄 | 返回临时文件句柄；关闭时自动删除底层临时文件 |

### 3.1.2 托管文件句柄方法

`vulcan.io.open(...)`、`vulcan.io.popen(...)`、`vulcan.io.tmpfile()` 返回的都是同一类托管句柄，支持以下方法：

| 方法 | 参数与类型 | 返回值 | 说明 |
| --- | --- | --- | --- |
| `file:read(...)` | 无参数时等价于按行读取；支持 `"*a"` / `"a"`、`"*l"` / `"l"`、正整数长度 | `string`、`nil`，或多返回值 | 文本模式下返回解码后的 UTF-8 字符串；二进制模式下返回原始 Lua 字节串 |
| `file:write(...)` | 一个或多个 Lua 标量值 | `true` | 文本模式会按编码写入；二进制模式按原始字节写入 |
| `file:flush()` | 无 | `true` | 刷新当前缓冲写入 |
| `file:close()` | 无 | `boolean` | 普通文件一般返回 `true`；`popen` 句柄会返回子进程是否成功退出 |
| `file:seek(whence?, offset?)` | `whence?: "set" \| "cur" \| "end"`；`offset?: integer` | `integer` | 返回新的游标位置；默认 `whence = "cur"`、`offset = 0` |
| `file:lines()` | 无 | 迭代器函数 | 从当前游标开始逐行迭代，直到 EOF 返回 `nil` |
| `file:setvbuf(...)` | 任意参数 | `true` | 仅做兼容占位；当前是 no-op |

补充规则：

- `file:read()` 在无参数时读取一行，不带末尾换行。
- `file:read(0)` 会返回空字符串，不会返回 `nil`。
- `file:read(n)` 在到达 EOF 后返回 `nil`。
- `file:seek(...)` 超过文件末尾会被夹到当前缓冲末尾；向前越界会直接报错。
- 文本模式下，传给 `file:write(...)` 的字符串必须是合法 UTF-8；如果你需要保留原始字节，应使用二进制模式，或改走 `vulcan.fs.read_bytes/write_bytes`。

### 3.1.3 编码选项

`options.encoding` 或 `encoding` 字段当前支持：

| 值 | 说明 |
| --- | --- |
| `utf-8` | 标准 UTF-8 文本 |
| `system` | 宿主系统默认文本编码；Windows 下对应 ANSI 代码页 |
| `oem` | Windows 控制台 OEM 代码页 |
| `gbk` | GBK 编码 |
| `gb18030` | GB18030 编码 |
| `latin1` | Latin-1 / ISO-8859-1 |
| `base64` | 以 Base64 文本保真传输原始字节 |

默认编码选择规则：

- 优先使用宿主配置的 `LuaRuntimeHostOptions.default_text_encoding`
- 若宿主未配置：
  - Windows 默认 `system`
  - 其他平台默认 `utf-8`

### 3.1.4 托管 `io.*` 兼容层

在 `vulcan.runtime.lua.exec(...)` 隔离环境中，常见 `io.*` 调用会被托管兼容层接管，包括：

- `io.open`
- `io.input`
- `io.output`
- `io.read`
- `io.write`
- `io.flush`
- `io.close`
- `io.lines`
- `io.popen`
- `io.tmpfile`
- `io.type`

兼容层的重要行为：

- `io.input(path_or_file)` / `io.output(path_or_file)` 可以设置默认输入输出句柄。
- 如果没有显式 `io.output(...)`，`io.write(...)` 会把内容写到运行时日志，而不是直接丢失。
- `io.close()` 在无参数时会关闭当前默认输出句柄。
- `io.type(value)` 返回 `"file"`、`"closed file"` 或 `"nil"`。
- 宿主可通过 `LuaRuntimeHostOptions.capabilities.enable_managed_io_compat = false` 关闭这层全局替换；即使关闭，`vulcan.io` 本身仍然可用。

### 3.1.5 示例

文本读取：

```lua
local text = vulcan.io.read_text("D:/data/example.txt", {
    encoding = "utf-8",
})

return text
```

二进制句柄读取：

```lua
local file = vulcan.io.open("D:/data/archive.bin", "rb")
local bytes = file:read("*a")
file:close()

-- `bytes` 是 Lua 字节串，不保证是 UTF-8。
return #bytes
```

托管 `popen`：

```lua
local file = vulcan.io.popen("cmd /C dir", "r", {
    encoding = "oem",
    timeout_ms = 3000,
})

local output = file:read("*a")
local ok = file:close()

return vulcan.json.encode({
    ok = ok,
    output = output,
})
```

## 3.2 宿主强制忽略 skill

`ignored_skill_ids` 是宿主运行时级策略，用来在加载早期跳过某些 skill。

典型场景：

- 宿主已经提供了更强的原生、gRPC 或 VMM 能力实现
- 默认 skill 包与宿主现有功能重复
- 某个数据库型 skill 的能力已被宿主侧服务替代，不希望继续启动 SQLite / LanceDB 绑定

匹配规则：

- 匹配对象是 skill 目录派生出的 `skill_id`
- 不匹配 `skill.yaml` 的 `name`
- 不由 skill 自己声明，也不属于依赖判定

运行时效果：

- 命中忽略列表后，整个 skill 会被跳过
- 不准备 `dependencies.yaml`
- 不绑定 SQLite / LanceDB
- 不注册任何 entry
- 不会出现在 `list_entries` 或 `vulcan.call` 可调用目标中

这项能力保留宿主和用户的最终选择权。  
如果用户希望继续使用某个 skill，宿主不应在策略层把它加入忽略列表。

## 4. `vulcan.call`

### 4.1 用途

`vulcan.call(name, args)` 用来在一个 skill 内部调用另一个已加载的 skill 入口。

- `name`：目标入口的 canonical 名称
- `args`：必须是 Lua table
- 返回值：直接透传被调 skill 的返回值，支持多返回值

### 4.2 最小示例

```lua
local ok, result = pcall(vulcan.call, "demo-skill-run", {
    query = "hello",
    limit = 5,
})

if not ok then
    vulcan.runtime.log("warn", "call failed: " .. tostring(result))
    return nil
end

return result
```

### 4.3 注意事项

- 不存在的入口会直接报错。
- `args` 必须是 table，不能传字符串或其他标量。
- `vulcan.call` 会继承当前请求上下文、预算快照、tool config，并切换到目标 skill 的文件上下文与数据库绑定。
- 在 `luaexec` 场景下，存在额外重入保护，不能无限递归调回当前运行时调用方。

## 4.5 `vulcan.models.*`

`vulcan.models.*` 是 Lua skill 使用模型能力的固定标准接口。
它不是通用 host tool 调用，也不允许 Lua 选择 provider 配置。

### 4.5.1 API 参考

| API | 参数与类型 | 返回值 | 说明 |
| --- | --- | --- | --- |
| `vulcan.models.status()` | 无 | `{ ok = true, capabilities = { embed = boolean, llm = boolean } }` | 固定存在；只反映宿主是否注册了能力 |
| `vulcan.models.has(capability)` | `capability: string` | `boolean` | 仅识别 `embed` 与 `llm` |
| `vulcan.models.embed(text)` | `text: string`，必须非空 | 成功或失败包络 | 执行单文本 embedding |
| `vulcan.models.llm(system, user)` | `system: string`，`user: string`，两者都必须非空 | 成功或失败包络 | 执行一轮非流式 LLM 调用 |

最小示例：

```lua
if not vulcan.models.has("embed") then
    return {
        ok = false,
        reason = "model-embed-unavailable",
    }
end

local result = vulcan.models.embed("hello")
if not result.ok then
    return result
end

return result.vector
```

### 4.5.2 返回结构

`embed(...)` 成功包络：

```lua
{
    ok = true,
    vector = { 0.1, 0.2, 0.3 },
    dimensions = 1536,
    usage = {
        input_tokens = 123,
    },
}
```

字段说明：

| 字段 | 类型 | 说明 |
| --- | --- | --- |
| `ok` | `boolean` | 成功时固定为 `true` |
| `vector` | `number[]` | embedding 向量 |
| `dimensions` | `integer` | 向量维度 |
| `usage` | `table?` | 可选；宿主若提供则包含 token 用量 |
| `usage.input_tokens` | `integer?` | 可选输入 token 数 |
| `usage.output_tokens` | `integer?` | 可选输出 token 数，embedding 常见为无 |

`llm(...)` 成功包络：

```lua
{
    ok = true,
    assistant = "...",
    usage = {
        input_tokens = 123,
        output_tokens = 456,
    },
}
```

字段说明：

| 字段 | 类型 | 说明 |
| --- | --- | --- |
| `ok` | `boolean` | 成功时固定为 `true` |
| `assistant` | `string` | 模型回复文本 |
| `usage` | `table?` | 可选 token 用量对象 |

失败包络：

```lua
{
    ok = false,
    error = {
        code = "provider_error",
        message = "model provider failed",
        provider_message = "raw provider error after host redaction",
        provider_code = "model_not_found",
        provider_status = 400,
    },
}
```

错误字段说明：

| 字段 | 类型 | 说明 |
| --- | --- | --- |
| `error.code` | `string` | 稳定错误码 |
| `error.message` | `string` | 面向 skill 的错误摘要 |
| `error.provider_message` | `string?` | 宿主愿意透出的 provider 侧消息 |
| `error.provider_code` | `string?` | provider 原始错误码 |
| `error.provider_status` | `integer?` | 例如 HTTP 状态码 |

当前稳定错误码包括：

- `model_unavailable`
- `invalid_argument`
- `provider_error`
- `timeout`
- `budget_exceeded`
- `internal_error`

### 4.5.3 行为规则

- `status()` 永远存在，并根据 callback 注册状态生成能力表。
- `has()` 只识别 `embed` 与 `llm`；未知能力、错误参数个数、错误类型都会返回 `false`，不会抛出 Lua 异常。
- `embed()` 只接受一个非空字符串，不支持批量输入。
- `llm()` 只接受两个非空字符串，不支持 messages、tool call、stream 或 thinking 控制。
- `embed()` / `llm()` 的参数错误、能力未注册、provider 调用失败，都会返回失败包络，而不是抛 Lua 运行时异常。
- Lua 不能传 `model`、`temperature`、`max_tokens`、`base_url`、`api_key`、`dimensions` 或 provider-specific 参数。
- LuaSkills 会把 caller context 传给宿主 callback，用于审计与成本归因，但不会通过模型 API 暴露给 Lua。
- 模型配置、API key、provider 路由、超时、预算和脱敏都由宿主负责。

宿主对接参考：

- [运行时架构中的模型能力边界](../architecture/runtime-model.md#standard-model-capability-boundary)
- [FFI 与 SDK 模型能力速查](../ffi/overview.md#model-capability-quick-path)
- [中文 FFI 模型 callback 对接说明](ffi/integration-guide.md#98-模型能力-callback)

## 4.6 `vulcan.host.*`

`vulcan.host.*` 是固定的宿主注册工具桥接。
它刻意比任意 `vulcan.xxx` 注入更窄：Lua 可以列出、探测和调用宿主工具，但不能自己创建新的顶级命名空间，也不能注册宿主工具。

### 4.6.1 API 参考

| API | 参数与类型 | 返回值 | 说明 |
| --- | --- | --- | --- |
| `vulcan.host.list()` | 无 | `table` | 宿主开放的工具元数据；结构由宿主定义 |
| `vulcan.host.has(tool_name)` | `tool_name: string` | `boolean` | 判断工具是否存在 |
| `vulcan.host.has_tool(tool_name)` | `tool_name: string` | `boolean` | `has` 的别名 |
| `vulcan.host.call(tool_name, args)` | `tool_name: string`；`args: table` | `table` | 调用宿主工具并返回结果 |

最小示例：

```lua
if not vulcan.host.has("vault.lookup") then
    return {
        ok = false,
        reason = "host-tool-unavailable",
    }
end

local result = vulcan.host.call("vault.lookup", {
    key = "demo-secret",
})

if not result.ok then
    return result
end

return result.value
```

### 4.6.2 推荐返回包络

推荐的成功返回包络：

```lua
{
    ok = true,
    value = {
        text = "resolved value",
    },
    meta = {
        elapsed_ms = 120,
    },
}
```

推荐的错误返回包络：

```lua
{
    ok = false,
    error = {
        code = "tool_not_found",
        message = "host tool not found: vault.lookup",
    },
}
```

### 4.6.3 运行时归一化规则

- `list()` 在宿主未注册 host-tool callback 时返回空 table `{}`。
- `has()` / `has_tool()` 在宿主未注册 callback 时返回 `false`。
- `has()` 的宿主回调返回值允许两种形式：
  - 直接返回 `boolean`
  - 返回对象，并在 `exists`、`has`、`available` 之一上放 `boolean`
- `call()` 要求 `args` 必须是 Lua table；空 table 会被保持为 JSON 空对象 `{}`，而不是空数组 `[]`。
- 如果宿主 `call` 回调返回的是对象，会原样透传给 Lua。
- 如果宿主 `call` 回调返回的是标量或数组，运行时会自动包装成：

```lua
{
    ok = true,
    value = <宿主原始返回值>,
}
```

- 当宿主未注册 callback 时，`call()` 返回：

```lua
{
    ok = false,
    error = {
        code = "host_tool_callback_missing",
        message = "...",
    },
}
```

- 当宿主 callback 在分发阶段失败时，`call()` 返回：

```lua
{
    ok = false,
    error = {
        code = "host_tool_callback_error",
        message = "...",
    },
}
```

### 4.6.4 行为规则

- `args` 必须是 Lua table；对象型入参建议使用显式 key。
- 该桥接不支持 stream，宿主工具应返回完整 table 结果。
- 权限、超时、审计和 secret 管理仍由宿主负责。
- 标准模型能力应使用 `vulcan.models.*`，不要长期依赖通用 host-tool 协议。

## 5. `vulcan.runtime.*`

### 5.1 `vulcan.runtime.log(level, message)`

用于向宿主日志输出一条运行时日志。

```lua
vulcan.runtime.log("info", "skill started")
vulcan.runtime.log("warn", "budget is low")
vulcan.runtime.log("error", "query failed")
```

API 契约：

| 项 | 类型 | 说明 |
| --- | --- | --- |
| `level` | `string` | 建议使用 `info`、`warn`、`error`；运行时会按文本内容粗分级 |
| `message` | `string` | 要写入宿主日志的文本 |
| 返回值 | `nil` | 成功时不返回业务值 |

说明：

- `level` 文本中包含 `error` 或 `fatal` 时按错误日志处理。
- `level` 文本中包含 `warn` 时按警告日志处理。
- 其他情况按普通信息日志处理。
- 该能力只在普通 skill VM 中可用。
- 在 `vulcan.runtime.lua.exec(...)` 的隔离执行环境中，该函数会被禁用。

### 5.2 `vulcan.runtime.cwd()`

返回当前进程工作目录。

```lua
local cwd = vulcan.runtime.cwd()
```

| 返回值 | 类型 | 说明 |
| --- | --- | --- |
| `cwd` | `string` | 宿主进程当前工作目录 |

补充说明：

- 这里返回的是运行时进程的原始工作目录。
- 不要把它和约定入口参数 `PWD` 混为一谈。
- 如果 skill 需要跟随宿主管理的项目/工作区作用域，应优先使用显式传入的 `PWD`；两者可能不同。

### 5.3 `vulcan.runtime.temp_dir`

宿主注入的临时目录路径，可能为 `nil`。

```lua
local temp_dir = vulcan.runtime.temp_dir
```

| 字段 | 类型 | 说明 |
| --- | --- | --- |
| `vulcan.runtime.temp_dir` | `string \| nil` | 宿主配置的临时目录；未配置时为 `nil` |

### 5.4 `vulcan.runtime.resources_dir`

宿主注入的资源目录路径，可能为 `nil`。

```lua
local resources_dir = vulcan.runtime.resources_dir
```

| 字段 | 类型 | 说明 |
| --- | --- | --- |
| `vulcan.runtime.resources_dir` | `string \| nil` | 宿主配置的资源目录；未配置时为 `nil` |

### 5.5 `vulcan.runtime.overflow_type`

当前暴露两个固定常量：

- `vulcan.runtime.overflow_type.truncate`
- `vulcan.runtime.overflow_type.page`

它们主要供宿主侧预算/溢出策略相关逻辑使用。

### 5.6 `vulcan.runtime.internal`

当前会暴露这些字段：

- `tool_name`
- `skill_name`
- `entry_name`
- `root_name`
- `luaexec_active`
- `luaexec_caller_tool_name`

字段说明：

| 字段 | 类型 | 说明 |
| --- | --- | --- |
| `tool_name` | `string \| nil` | 当前对外工具名，即宿主可见的 canonical tool name |
| `skill_name` | `string \| nil` | 当前 skill 标识 |
| `entry_name` | `string \| nil` | 当前 skill 内部 entry 名 |
| `root_name` | `string \| nil` | 当前 skill 所属运行时根层级，如 `ROOT` / `PROJECT` / `USER` |
| `luaexec_active` | `boolean` | 当前调用是否处于 `vulcan.runtime.lua.exec(...)` 隔离链路中 |
| `luaexec_caller_tool_name` | `string \| nil` | 若当前是 `luaexec` 内部执行，则表示外层触发它的工具名 |

这组字段属于**内部执行上下文**，建议只用于调试、日志、审计定位，不建议作为长期公共协议依赖。

### 5.7 `vulcan.runtime.lua.exec(input)`

执行一次隔离的内联 Lua 运行时调用，返回 **Markdown 字符串**，不是普通 Lua table。

当前输入结构：

| 字段 | 类型 | 必填 | 说明 |
| --- | --- | --- | --- |
| `task` | `string` | 否 | 人类可读任务摘要，仅用于结果头部展示 |
| `code` | `string` | 与 `file` 二选一 | 要执行的内联 Lua 代码 |
| `file` | `string` | 与 `code` 二选一 | 要执行的 Lua 文件路径 |
| `args` | `table` | 否 | 注入到执行上下文中的结构化参数；默认空对象 |
| `timeout_ms` | `integer` | 否 | 超时时间（毫秒），默认 `60000` |

最小示例：

```lua
local rendered = vulcan.runtime.lua.exec({
    task = "inspect args",
    code = [[
        print("hello", args.name)
        return { ok = true, name = args.name }
    ]],
    args = {
        name = "codex",
    },
})

return rendered
```

返回值与行为：

| 项 | 类型 | 说明 |
| --- | --- | --- |
| 返回值 | `string` | 已渲染 Markdown 文本 |
| `print(...)` 输出 | 文本 | 会被捕获并写入最终渲染结果 |
| 代码返回值 | 任意 JSON 可序列化值 | 不直接作为 Lua table 返回，而是被渲染进结果文本 |

重要限制：

- 返回值是 Markdown 渲染结果，不是结构化 Lua table。
- 隔离环境里会覆盖全局 `print`，把输出收集进结果文本。
- 隔离环境里会禁用：
  - `vulcan.runtime.log`
  - `vulcan.cache.put`
  - `vulcan.cache.get`
  - `vulcan.cache.delete`
  - `vulcan.runtime.lua.exec`（禁止递归再调）
- 该环境使用内部模拟请求上下文，因此你在 `luaexec` 中看到的：
  - `vulcan.context.client_info.name`
  - `vulcan.context.request.transport_name`
  默认会是 `luaexec_call` 一类内部标识，而不是外部真实客户端。
- 当前隔离执行链已经拥有独立 VM 池：
  - 默认 `min_size=1 / max_size=4 / idle_ttl_secs=60`
  - 可由宿主通过 `LuaRuntimeHostOptions.runlua_pool_config` 覆盖
  - 这只影响 `vulcan.runtime.lua.exec(...)`，不改变普通 skill VM 池
  - 当前不再支持为 `vulcan.runtime.lua.exec(...)` 单独配置外部执行器路径
- 该调用会重置请求级上下文，不会继承外层 VM 中临时写进全局表的任意自定义状态。

### 5.8 `vulcan.runtime.skills.*`

这组能力用于让 skill 请求宿主执行安装、更新、启停、卸载等动作。

正式层级模型为：

```text
ROOT -> PROJECT -> USER
```

其中 `ROOT` 是系统控制级，运行时启动或加载时必须存在该层。普通 skill 不能通过 `vulcan.runtime.skills.*` 请求安装、更新、卸载、启用或停用 `ROOT` 级 skill。普通桥接只面向宿主开放且当前实际存在的 `PROJECT` / `USER` 层级。

`vulcan.runtime.skills.*` 固定等价于 `DelegatedTool` 权限：它看不到 `ROOT` skills，也不能写入 `ROOT`。FFI 查询与 prompt completion 入口在 `DelegatedTool` 下同样不会返回 `ROOT` entries、help 或 ROOT tool name 归属。`call_skill` 与 `run_lua` 是运行时执行面，允许调用当前已激活的 skill，不作为技能管理权限边界。如果 `ROOT` 已经存在同名 `skill_id`，普通层 install / update 会被拒绝；普通层 uninstall 仍可用于清理 `PROJECT` / `USER` 中的同名残留。

正式桥接建议包含：

- `vulcan.runtime.skills.enabled`
- `vulcan.runtime.skills.status()`
- `vulcan.runtime.skills.layers()`
- `vulcan.runtime.skills.install(input)`
- `vulcan.runtime.skills.update(input)`
- `vulcan.runtime.skills.uninstall(input)`
- `vulcan.runtime.skills.enable(input)`
- `vulcan.runtime.skills.disable(input)`

稳定字段：

| API / 字段 | 类型 | 说明 |
| --- | --- | --- |
| `vulcan.runtime.skills.enabled` | `boolean` | 宿主是否在策略层开启了 skill 管理桥接 |
| `status().enabled` | `boolean` | 与上面一致 |
| `status().callback_registered` | `boolean` | 宿主是否实际注册了 skill 管理 callback |
| `status().mode` | `string` | 当前固定为 `host_callback` |
| `status().message` | `string` | 当前桥接状态说明 |

`layers()` 用于返回当前宿主允许普通桥接操作的层级标签。推荐返回内容包含：

- `default`
- `writable`
- `labels`
- `layers`

其中 `labels` 只应包含 `PROJECT` / `USER` 中当前实际存在的层级，不应包含 `ROOT`。如果当前没有项目上下文，运行时只返回 `USER`；如果只有 `ROOT`，则返回空列表且顶层 `writable=false`。bridge 关闭时仍可发现层级，但顶层 `writable` 与每个 layer 的 `writable` 都必须为 `false`。如果后续安装、更新、卸载输入允许指定层级，也只能使用 `layers()` 返回的标签。

完整层级与管理边界见 [Skill Root 层级与管理边界](architecture/skill-root-layer-policy.md)。

注意事项：

- 这组能力只有在宿主显式打开 `enable_skill_management_bridge` 时才允许执行。
- 即使宿主策略打开了，如果没有注册对应回调，也会返回明确错误。
- `install/update/uninstall/enable/disable` 的 `input` / 返回值结构是宿主桥接契约的一部分，运行时只负责分发，不负责替你定义业务字段。
- 如果 payload 显式试图操作 `ROOT` 层，运行时会直接拒绝调用。

### 5.9 受管 Python 与 Node 子运行时

Lua 仍然是一等调度层。受管 Python 与 Node.js 只是由 Lua 通过 `vulcan.runtime.python.*` 和 `vulcan.runtime.node.*` 调用的子执行面，不替代 skill entry 模型、不替代 Lua VM 池，也不改变宿主 SDK 契约。

普通 Skill 调用与专用 System Plugin 租约都会携带一份可信包上下文。System 租约必须使用严格的 `system_package = { id, root, dependencies_file }` 描述符创建；`root` 必须是引擎 `system_lua_lib` 目录下的绝对严格子目录，不能包含 Lua 搜索路径元字符，并会在每次 eval 前重新校验。`dependencies_file` 必须解析为包内相对路径指向的普通文件，不能通过路径或符号链接逃逸。可选 `workspace_root` 必须是既有绝对目录，`cwd` 必须解析到包根或该授权工作区内。包根是该 System Plugin 唯一的 Lua 模块根，因此兄弟插件不会共享 `require(...)` 作用域或受管依赖身份。

在 System Plugin 租约内，运行时还会暴露：

| 字段 | 类型 | 说明 |
| --- | --- | --- |
| `vulcan.runtime.system_plugin` | 递归只读 userdata 视图 | 绑定包租约的 `id`、规范 `root`、`lease_id`、`sid` 与 `generation` |
| `vulcan.runtime.workspace_root` | `string \| nil` | 宿主显式授权的规范工作区根 |
| `vulcan.runtime.mounts` | 递归只读 userdata 视图 | 宿主拥有的结构化挂载元数据 |

两个 userdata 视图支持索引、`pairs` / `ipairs`、长度与普通 JSON 结果序列化，三个宿主所有的根字段也不能被赋值或替换。专用 System VM 会移除全局 `rawset` 与 Lua `debug` 库，阻止绕过元表边界。这取代了“System 运行时上下文全部缺失或恒为 `nil`”的旧假设；入口专属的 `vulcan.context.skill_dir / entry_dir / entry_file` 仍是另一组语义，在租约 eval 不是 Skill 入口调用时仍可能为 `nil`。

正常调用链为：

1. skill 在 `dependencies.yaml` 声明 Python 或 Node.js 需求。
2. 运行时根目录先通过受管运行时拉取脚本准备好主程序。
3. Lua 通过 `vulcan.runtime.python.invoke(...)` 或 `vulcan.runtime.node.invoke(...)` 调用 Python / Node.js 文件。
4. 运行时按需创建带版本的依赖环境，复用常驻 worker，并把结构化结果返回给 Lua。

调用声明了受管子运行时的 skill 前，需要先准备运行时：

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/deps/fetch_managed_runtimes.ps1 -RuntimeRoot <runtime_root> -Target all
```

```bash
RUNTIME_ROOT=<runtime_root> scripts/deps/fetch_managed_runtimes.sh all
```

拉取脚本会把可携带的运行时放入 `<runtime_root>/dependencies/runtimes/...`。运行时创建的包环境位于 `<runtime_root>/dependencies/envs/...`，并按运行时版本、平台、包管理器版本与 lockfile 内容生成稳定 hash。

最小 `dependencies.yaml`：

```yaml
python_runtime:
version: "3.14.6"
  package_manager: uv
  package_manager_version: "0.11.28"
  lockfile: python/requirements.lock
node_runtime:
  version: "24.18.0"
  package_manager: pnpm
  package_manager_version: "11.11.0"
  package_json: node/package.json
  lockfile: node/pnpm-lock.yaml
```

`node_runtime.version` 必须是一个精确语义版本，不能使用版本范围、标签或不完整版本。受管异步 ESM Worker 协议要求 Node.js 不低于 `22.0.0`；无效 SemVer 或更低版本会在解析环境或启动 Worker 前被拒绝。

Lua 调用示例：

```lua
local python_result = vulcan.runtime.python.invoke({
    file = "python/handler.py",
    handler = "main",
    timeout_ms = 30000,
    args = {
        text = "hello",
    },
})

if not python_result.ok then
    error("python failed: " .. tostring(python_result.error))
end

local node_result = vulcan.runtime.node.invoke({
    file = "node/handler.mjs",
    handler = "main",
    timeout_ms = 30000,
    args = python_result.value,
})

if not node_result.ok then
    error("node failed: " .. tostring(node_result.error))
end
```

Python handler 接收 JSON 兼容参数与上下文：

```python
def main(args, ctx):
    return {
        "text": args.get("text", ""),
        "ctx_is_dict": isinstance(ctx, dict),
    }
```

Node.js handler 使用 ESM 模块。未传 `handler` 时默认查找 `default`，并以 `main` 作为 fallback；显式 `handler = "main"` 会调用命名导出：

```javascript
export async function main(args, ctx) {
  return {
    text: args.text || "",
    ctxIsObject: ctx && typeof ctx === "object",
  };
}
```

状态 API：

| API | 返回值 | 说明 |
| --- | --- | --- |
| `vulcan.runtime.python.status()` | table | 当前 skill 的 `python_runtime` 声明状态 |
| `vulcan.runtime.node.status()` | table | 当前 skill 的 `node_runtime` 声明状态 |

稳定状态字段：

| 字段 | 类型 | 说明 |
| --- | --- | --- |
| `available` | `boolean` | 声明是否能解析到已准备好的运行时 |
| `configured` | `boolean` | skill 是否在 `dependencies.yaml` 声明了该运行时 |
| `ready` | `boolean` | 包环境是否已经创建 |
| `runtime` | `"python" \| "node"` | 子运行时类型 |
| `runtime_version` | `string?` | 解析成功后的运行时版本 |
| `runtime_executable` | `string?` | 解析成功后的子运行时可执行文件路径 |
| `package_manager` | `string?` | `uv` 或 `pnpm` |
| `package_manager_version` | `string?` | 声明的包管理器版本 |
| `package_manager_executable` | `string?` | 解析成功后的包管理器可执行文件路径 |
| `env_hash` | `string?` | 稳定包环境 hash |
| `env_dir` | `string?` | 解析成功后的包环境目录 |
| `message` | `string?` | 可读状态说明 |
| `error` | `string?` | 无法从 available/configured 进入 ready 时的解析错误 |

调用输入：

| 字段 | 类型 | 是否必填 | 说明 |
| --- | --- | --- | --- |
| `file` | `string` | 是 | 当前 skill 目录内的安全相对路径 |
| `handler` | `string` | 否 | Python 默认 `main`；Node 默认 `default`，并 fallback 到 `main` |
| `args` | JSON 兼容 Lua 值 | 否 | 传给子 handler |
| `timeout_ms` | 正整数 | 否 | 单次调用超时；优先于引擎 `managed_runtime_config.invoke_default_timeout_ms`；两处都省略时无限制；超时 Worker 会被丢弃 |

调用返回：

| 字段 | 类型 | 说明 |
| --- | --- | --- |
| `ok` | `boolean` | 子 handler 是否成功完成 |
| `value` | JSON 兼容值 | handler 返回值；失败时为 `nil` / `null` |
| `stdout` | `string` | 捕获到的 Python stdout 或 Node `console.log/info` 输出 |
| `stderr` | `string` | 捕获到的 Python stderr 或 Node `console.warn/error` 输出 |
| `error` | `string?` | 子运行时错误信息 |
| `trace` | `string?` | 可用时返回 Python traceback 或 Node stack trace |
| `status` | `string?` | worker 层失败状态，例如 timeout 或输出损坏 |
| `timed_out` | `boolean?` | 调用超过 `timeout_ms` 时为 true |
| `worker_reused` | `boolean` | 本次调用是否复用了已有常驻 worker |
| `env_hash` | `string` | 本次调用使用的包环境 hash |
| `env_dir` | `string` | 本次调用使用的包环境目录 |

行为注意事项：

- 新环境 hash 的第一次 invoke 会创建包环境；后续调用复用环境 marker，并通常复用常驻 worker。
- 每个存活 Worker 或持久会话快照都会持有跨进程共享环境生命周期租约。环境最终发布或替换使用非阻塞独占租约；只要仍有 Worker 或会话存活，操作就会立即返回稳定的 `managed runtime environment is busy with active lifecycle leases` 错误，而不会等待或替换使用方脚下的文件。
- Python 模块按文件路径加载，并在 Python Worker 内缓存。每个 Worker 会把唯一快照根固定在 `sys.path` 首位，移除由 cwd 派生的空入口，并拒绝后续请求更换该根。
- Node.js 模块按原生 Node ESM 行为导入。只要依赖写进 `package.json` 并由 `pnpm-lock.yaml` 锁定，default import、named import、namespace import、relative import、side-effect import 与 dynamic import 都应遵守 Node 原生规则。
- 对 Node.js，运行时会先把当前 skill 目录复制进受管环境的 import root，再执行导入。这样可以让 bare dependency 解析停留在受管 `node_modules` 内，同时避免 Windows 符号链接权限问题。
- 在 Unix 上，池化 Worker 会在 `exec` 前进入已固定的快照目录；每个请求的相对入口都会通过描述符相对、逐组件且不跟随链接的 `openat` 校验。在 Windows 上，Worker 接收由共享锁保护的绝对快照源码路径，并使用快照所在的短驱动器根或 UNC 共享根作为中立 cwd，因此既不会继承宿主 cwd，也不会把长快照路径作为 `CreateProcessW.lpCurrentDirectory` 传入。
- 受管 `status`、池化 `invoke` 与 System Plugin 持久会话支持 Windows x86_64、Linux x86_64/aarch64、macOS x86_64/aarch64；Windows ARM/aarch64 是持久会话唯一明确例外。
- 子代码应返回 JSON 可序列化值。不可序列化结果会作为结构化子错误返回，而不是让 Lua skill VM 崩溃。
- `stdout` 与 `stderr` 会进入调用结果，由 Lua 决定是否展示、脱敏或记录。
- skill 不应依赖系统 Python、系统 Node.js、外部 `node_modules` 或未声明包。

#### 持久 `session.open(...)`

只有专用 System Plugin 租约可在子进程需要跨同一租约 VM 的多次执行持续存活时使用 `vulcan.runtime.python.session.open(...)` 或 `vulcan.runtime.node.session.open(...)`。持久受管会话在 Windows x86_64、Linux x86_64/aarch64、macOS x86_64/aarch64 上保持相同的包快照、依赖、事件与进程树清理规则。Windows ARM/aarch64 会在创建环境、创建快照或预留会话前返回 `persistent_session.supported=false` 与稳定原因 `windows_arm_is_not_supported`。普通 Skill 保留受支持的 `status` 与 `invoke` 操作，但不能打开持久受管会话。每个受管运行时 `status()` 响应都包含 `persistent_session.supported`、`target_os`、`target_arch`，不支持时还包含 `reason`。返回的 userdata 复用 `vulcan.process.session` 文档中的 `write`、`read`、`status`、`close` 与 `kill` 方法，但可执行文件与依赖环境只能来自当前可信包声明。

严格打开参数：

| 字段 | 类型 | 必填 | 说明 |
| --- | --- | --- | --- |
| `file` | `string` | 是 | 已存在的包相对 Python 或 Node 源文件；拒绝目录穿越与符号链接逃逸 |
| `args` | 稠密 `string[]` | 否 | 直接传给子进程的参数；不执行 shell 展开 |
| `cwd` | `string` | 否 | 位于包根或 System 宿主显式授权工作区根内的包相对/绝对目录；默认包根 |
| `stdout_encoding` | `string` | 否 | stdout 解码；默认使用宿主运行时编码 |
| `stderr_encoding` | `string` | 否 | stderr 解码；默认使用宿主运行时编码 |
| `stdin_encoding` | `string` | 否 | stdin 编码；默认使用宿主运行时编码 |
| `buffer_limit_bytes` | 正整数 | 否 | 每个输出流的保留字节上限；优先于引擎 `managed_runtime_config.persistent_session_default_buffer_limit_bytes_per_stream`，后者稳定默认值为 1 MiB |

未知字段会被拒绝。Python 与 Node 都从每会话唯一的不可变包快照执行；快照通过固定且不跟随链接的文件系统对象复制。Python 使用包声明对应的受管虚拟环境，并移除继承的 `PYTHONHOME`、`PYTHONPATH` 与用户 site-packages；Node 从精确受管 `node_modules` 解析裸依赖。复制的包目录树会拒绝符号链接与不支持的对象，进程创建期间还会固定已授权的 `cwd` 对象。子代码通过 `LUASKILLS_MANAGED_CONTEXT_JSON` 获取受控的包与租约元数据，而不是任意复制宿主请求状态。

最小跨 eval System Plugin 流程：

```lua
-- 第一次 eval：把 userdata 保存在持久租约 VM 中。
managed = vulcan.runtime.python.session.open({
    file = "python/sidecar.py",
    args = { "--stdio" },
    cwd = "python",
    stdout_encoding = "utf-8",
    stderr_encoding = "utf-8",
    stdin_encoding = "utf-8",
    buffer_limit_bytes = 1024 * 1024,
})
local status = managed:status()
return { managed_session_id = status.managed_session_id }
```

```lua
-- 同一 lease_id 的后续 eval：复用同一个 userdata 与进程。
managed:write(vulcan.json.encode(args.message) .. "\n")
local output = managed:read({ timeout_ms = 2000, max_bytes = 65536 })
return {
    managed_session_id = managed:status().managed_session_id,
    stdout = output.stdout,
    stderr = output.stderr,
    stdout_total_bytes = output.stdout_total_bytes,
    stdout_dropped_bytes = output.stdout_dropped_bytes,
}
```

只有受管 Python/Node 会话的状态 table 包含 `managed_session_id`；普通 `vulcan.process.session` 状态 table 不包含该字段。该标识用于把 Lua userdata 与宿主侧 `RuntimeManagedSessionEvent.managed_session_id` 精确关联。

生命周期规则是确定的：

- 租约 eval 失败时，会回滚并终止该次 eval 新开的全部受管会话，即使 Lua 在抛错前已经把 userdata 保存到全局变量。
- 显式关闭租约、同 SID 替换、租约过期、VM 销毁与引擎销毁都会终止完整子进程树，并退役包拥有的 worker。
- 普通 Skill 根重载不会替换专用 System 租约管理器，因此活跃 System Plugin session 会跨无关 Skill 重载继续存活，直至自身租约生命周期结束。
- userdata 被丢弃或回收也会终止进程树；Python/Node 包快照只会在进程清理完成后删除。
- 快照会在进程清理与自身删除完成前持续持有环境共享生命周期租约，因此环境替换无法与仍存活的会话或 Worker 竞态。
- `session:close(...)` 与 `session:kill()` 会同时终止直接子进程及其后代。
- 后台 stdout、stderr、退出与失败观察器绝不会调用 Lua。System Plugin 会话只向宿主发布有界引擎事件；宿主 wake callback 只能唤醒或调度宿主工作，不能同步进入 Lua。

完整 System 租约、持久会话、宿主事件与清理流程见 [System Plugin 受管运行时使用指南](system-plugin-managed-runtime.md)。[Managed Runtime Example](../../examples/managed_runtime/README.md) 是覆盖池化 `status` / `invoke` 路径的可运行普通 Skill 包。仓库开发阶段可以用布局检查器与隔离冒烟脚本做端到端验证：

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/debug-tools/managed_runtime_smoke.ps1
```

```bash
bash scripts/debug-tools/managed_runtime_smoke.sh
```

### 5.10 `vulcan.config.*`

这组能力用于读取和维护**当前 skill 自己的字符串配置**。

当前提供：

| API | 参数与类型 | 返回值 | 说明 |
| --- | --- | --- | --- |
| `vulcan.config.get(key)` | `key: string` | `string \| nil` | 读取当前 skill 配置值 |
| `vulcan.config.has(key)` | `key: string` | `boolean` | 判断键是否存在 |
| `vulcan.config.set(key, value)` | `key: string`；`value: string` | `true` | 设置当前 skill 配置值 |
| `vulcan.config.delete(key)` | `key: string` | `boolean` | 删除键；是否成功由底层 store 决定 |
| `vulcan.config.list()` | 无 | `table<string, string>` | 列出当前 skill 命名空间下的全部字符串配置 |

最小示例：

```lua
local api_token = vulcan.config.get("api_token")

if not api_token or api_token == "" then
    return "当前未配置 `api_token`。请使用宿主提供的 runtime-config 工具为当前 skill 设置 `api_token`。"
end

local endpoint = vulcan.config.get("endpoint") or "https://api.example.com"

vulcan.config.set("last_endpoint", endpoint)

return {
    ok = true,
    endpoint = endpoint,
}
```

`list()` 当前返回的是当前 skill 命名空间下的平面表：

```lua
local config = vulcan.config.list()
-- config.api_token
-- config.endpoint
```

注意事项：

- 当前配置值第一版统一为 `string`。
- 如果你确实需要复杂结构，建议把 JSON 文本作为字符串存入，再由 skill 自己 `vulcan.json.decode(...)`。
- 配置默认只作用于当前 skill，不能直接跨 skill 读写其他命名空间。
- 这组 API **要求当前存在活动 skill 上下文**；如果在没有当前 skill 身份的系统运行时里调用，会直接报错。
- 当前不做“未配置即不加载”的自动策略；更推荐 skill 在缺配置时返回明确提示，告知用户如何完成配置。
- 统一主配置文件默认位于 `<runtime_root>/config/skill_config.json`；宿主也可以显式覆盖路径。

## 6. `vulcan.fs.*`

### 6.1 API 参考

| API | 参数与类型 | 返回值 | 说明 |
| --- | --- | --- | --- |
| `vulcan.fs.list(dir)` | `dir: string` | `string[]` | 返回目录下的文件名列表，仅文件名，不含父路径 |
| `vulcan.fs.read(path)` | `path: string` | `string` | 以 UTF-8 文本方式读取整个文件；非 UTF-8 文件会报错 |
| `vulcan.fs.write(path, content)` | `path: string`；`content: string` | `true` | 覆盖写入文本；不会自动创建父目录 |
| `vulcan.fs.read_bytes(path)` | `path: string` | `string` | 读取原始字节并返回 Base64 文本 |
| `vulcan.fs.write_bytes(path, base64_text)` | `path: string`；`base64_text: string` | `true` | 解析 Base64 并写入原始字节 |
| `vulcan.fs.rename(old_path, new_path)` | `old_path: string`；`new_path: string` | `true` | 重命名或移动文件/目录 |
| `vulcan.fs.copy(src_path, dst_path, options?)` | `src_path: string`；`dst_path: string`；`options?: { overwrite?: boolean }` | `boolean` | 复制常规文件或整个目录树；若目标已存在且不允许覆盖则返回 `false` |
| `vulcan.fs.remove(path, options?)` | `path: string`；`options?: { recursive?: boolean }` | `boolean` | 删除文件或目录；目标不存在时返回 `false` |
| `vulcan.fs.mkdir(path, options?)` | `path: string`；`options?: { recursive?: boolean }` | `boolean` | 创建目录；目标目录已存在时返回 `false` |
| `vulcan.fs.stat(path)` | `path: string` | `table \| nil` | 目标不存在返回 `nil`；存在时返回元数据表 |
| `vulcan.fs.exists(path)` | `path: string` | `boolean` | 判断路径是否存在 |
| `vulcan.fs.is_dir(path)` | `path: string` | `boolean` | 判断路径是否为目录 |

### 6.2 `fs.stat(path)` 返回结构

`vulcan.fs.stat(path)` 在目标存在时返回：

| 字段 | 类型 | 说明 |
| --- | --- | --- |
| `kind` | `"file" \| "dir" \| "symlink" \| "other"` | 规范化目标类型 |
| `is_file` | `boolean` | 是否为常规文件 |
| `is_dir` | `boolean` | 是否为目录 |
| `is_symlink` | `boolean` | 是否为符号链接 |
| `readonly` | `boolean` | 当前元数据是否只读 |
| `size` | `integer?` | 常规文件大小（字节）；目录通常没有该字段 |
| `modified_unix_ms` | `integer?` | 最后修改时间的 Unix 毫秒时间戳；宿主无法获取时可能缺失 |

### 6.3 示例

```lua
local entries = vulcan.fs.list(vulcan.context.entry_dir)
local exists = vulcan.fs.exists(vulcan.context.entry_file)
local content = vulcan.fs.read(vulcan.context.entry_file)
local payload_base64 = vulcan.fs.read_bytes(vulcan.context.entry_file)
local info = vulcan.fs.stat(vulcan.context.entry_file)
local created = vulcan.fs.mkdir(vulcan.path.join(vulcan.context.entry_dir, "输出目录"), {
    recursive = true,
})
```

字节保真示例：

```lua
local payload = vulcan.fs.read_bytes("D:/data/image.bin")

vulcan.fs.write_bytes("D:/data/image.copy.bin", payload)
```

### 6.4 行为规则与注意事项

- 当前没有沙箱限制，skill 理论上可以访问宿主可访问的任意路径。
- `fs.list(dir)` 返回的是**子项名称**，不是绝对路径；如需完整路径，请配合 `vulcan.path.join(...)`。
- `fs.read` / `fs.write` 处理的是文本内容。`fs.read` 使用 UTF-8 读取；如果文件不是合法 UTF-8，请改用 `fs.read_bytes`。
- `fs.read_bytes(path)` 返回 base64 文本，`fs.write_bytes(path, base64_text)` 接收 base64 文本并写入原始字节，适合需要跨 Lua / 宿主边界保真传递非 UTF-8 内容的场景。
- `fs.rename`、`fs.remove`、`fs.mkdir` 优先用于需要兼容 Windows 中文路径的文件生命周期操作，不建议继续依赖原生 `os.rename` / `os.remove`。
- `fs.copy(src_path, dst_path)` 默认不会覆盖已有目标；只有显式传入 `{ overwrite = true }` 时才会覆盖。
- 目标是否“已存在”是按路径条目本身判断的，因此悬空符号链接也会被视为已存在目标。
- 当源路径是目录时，`fs.copy` 会递归复制整个目录树。
- 当 `overwrite = true` 且目标已存在时，运行时会先整体删除目标路径，再复制新内容；这意味着目标树会被“替换”，而不是“合并”。
- 当源路径是目录时，目标路径关系校验会先解析现有父级链接，再判断目标是否会实际落回源目录树内部。
- 当前目录树复制会拒绝符号链接子项，避免跨平台行为不一致。
- 当源路径是目录时，目标路径不能等于源目录，也不能位于源目录内部；否则会直接报错，避免把复制结果卷回源树。
- `fs.remove(path, { recursive = true })` 用于递归删除目录；目标不存在时返回 `false`。
- `fs.remove(path)` 删除目录时若未设置 `recursive = true`，会尝试按空目录删除；目录非空时会报错。
- 当路径本身是符号链接时，`fs.remove` 会删除链接条目本身，而不是删除它指向的目标。
- `fs.mkdir(path, { recursive = true })` 用于递归创建目录；目标目录已存在时返回 `false`。
- `fs.mkdir(path)` 如果目标路径已存在但不是目录，会直接报错。
- `fs.rename(old_path, new_path)` 成功时返回 `true`，其余错误直接抛出运行时异常。
- `fs.stat(path)` 在目标不存在时返回 `nil`；目标存在时返回包含 `kind`、`is_file`、`is_dir`、`is_symlink`、`readonly`、普通文件可选 `size` 以及可选 `modified_unix_ms` 的 table。
- 路径参数必须是字符串，且会经过基础路径语法校验。

## 7. `vulcan.path.*`

### 7.1 API 参考

| API | 参数与类型 | 返回值 | 说明 |
| --- | --- | --- | --- |
| `vulcan.path.join(...)` | 一个或多个 `string` 路径片段 | `string` | 按宿主平台规则拼接路径；至少要传一个片段 |
| `vulcan.path.dirname(path)` | `path: string` | `string` | 返回父目录 |
| `vulcan.path.basename(path)` | `path: string` | `string` | 返回末尾文件名部分 |
| `vulcan.path.stem(path)` | `path: string` | `string` | 返回不带扩展名的末尾文件名 |
| `vulcan.path.extname(path)` | `path: string` | `string` | 返回扩展名；包含前导点 |
| `vulcan.path.normalize(path)` | `path: string` | `string` | 做词法规范化，不访问文件系统 |
| `vulcan.path.is_abs(path)` | `path: string` | `boolean` | 判断是否为绝对路径 |

### 7.2 示例

```lua
local config_path = vulcan.path.join(
    vulcan.context.skill_dir,
    "runtime",
    "config.json"
)
local ext = vulcan.path.extname(config_path)
```

### 7.3 返回规则

- 会按宿主系统返回正常路径文本。
- Windows 下不会把 `\\?\` 或 `\\?\UNC\` verbatim 前缀直接泄漏给 Lua。
- Lua/运行时的路径型输入会在接收 API 寻址前归一化，包括保留的顶层 `PWD` 参数、`vulcan.fs` / `vulcan.io` 路径、进程 `program` / `cwd` 字段、Runtime Lease 路径字段、模块搜索根与受管运行时 FFI 路径。
- 只有 verbatim 绝对盘符路径与 verbatim UNC 路径会被转换；其他 Windows verbatim 命名空间会在寻址或创建进程前以 `unsupported Windows verbatim path namespace` 明确失败。
- `package.path` 与 `package.cpath` 只使用宿主可见路径形式构造，Windows 规范根不会因逐字路径前缀导致 Lua 模块寻址卡顿。
- Rust 内部用于包含关系、目录替换、符号链接与竞态校验的规范路径及原生文件对象身份保持不变；Lua/API 边界去前缀不会降低这些校验强度。
- 任意 JSON 字符串、进程普通参数和环境变量不会被猜测为路径，也不会被改写。
- `path.dirname(path)` 在相对路径没有父级片段时返回 `.`。
- `path.basename(path)`、`path.stem(path)`、`path.extname(path)` 在末尾组件缺失时返回空字符串。
- `path.extname(path)` 在存在扩展名时会包含前导点，例如 `.lua`。
- `path.normalize(path)` 只做词法规范化；它会折叠 `.` 与 `..`，不会触碰文件系统，结果为空时返回 `.`。

## 8. `vulcan.process.*`

### 8.1 API 参考

| API | 参数与类型 | 返回值 | 说明 |
| --- | --- | --- | --- |
| `vulcan.process.exec(spec)` | `spec: string \| table` | 结果 table | 启动一次性子进程并等待结束 |
| `vulcan.process.launchers()` | 无 | `table` | 返回当前宿主支持的 `shell` 参数值列表与默认值 |
| `vulcan.process.which(program)` | `program: string` | `string \| nil` | 搜索可执行文件 |
| `vulcan.process.session.open(spec)` | `spec: table` | 进程会话句柄 | 启动交互式子进程会话 |

`vulcan.process.which(program)` 会按宿主平台搜索可执行文件；找到时返回宿主可见绝对路径，找不到时返回 `nil`。Windows 下会结合 `PATHEXT` 解析无扩展名命令。

`vulcan.process.launchers()` 返回结构固定为：

| 字段 | 类型 | 说明 |
| --- | --- | --- |
| `default` | `string` | 当前宿主默认 shell 参数值；Windows 通常为 `cmd`，类 Unix 通常为 `sh` |
| `shells` | `array<string>` | 当前宿主支持的 `shell` 参数值列表；顺序稳定，默认值总在第一个 |

最小示例：

```lua
local launchers = vulcan.process.launchers()
local shell = launchers.default

for _, name in ipairs(launchers.shells or {}) do
    if name == "bash" then
        shell = "bash"
        break
    end
    if name == "pwsh" then
        shell = "pwsh"
        break
    end
end

local command = shell == "cmd" and "dir" or "ls"
local result = vulcan.process.exec({
    command = command,
    shell = shell,
})
```

### 8.2 `exec(spec)` 请求结构

`vulcan.process.exec(spec)` 支持两种输入形式：

1. 直接传字符串，等价于 shell/command 模式
2. 传 table，显式描述启动参数

最简单的字符串模式：

```lua
local result = vulcan.process.exec("echo hello")
```

table 模式字段：

| 字段 | 类型 | 必填 | 说明 |
| --- | --- | --- | --- |
| `command` | `string` | 与 `program` 二选一 | shell 命令文本 |
| `program` | `string` | 与 `command` 二选一 | 可执行程序路径或命令名 |
| `args` | `array` | 否 | 仅 `program` 模式可用；建议传字符串数组，数字/布尔值也会被转成字符串 |
| `cwd` | `string` | 否 | 子进程工作目录 |
| `env` | `table<string, scalar>` | 否 | 环境变量映射；值会转成字符串 |
| `stdin` | `string` | 否 | 启动后一次性写入 stdin 的文本 |
| `timeout_ms` | `integer` | 否 | 正整数毫秒超时；省略时表示不设置超时 |
| `shell` | `boolean \| string` | 否 | `command` 模式下可选；布尔值用于兼容旧逻辑，字符串必须来自 `vulcan.process.launchers().shells` |
| `encoding` | `string` | 否 | 同时作为 `stdout/stderr/stdin` 默认编码 |
| `stdout_encoding` | `string` | 否 | 单独覆盖 stdout 解码编码 |
| `stderr_encoding` | `string` | 否 | 单独覆盖 stderr 解码编码 |
| `stdin_encoding` | `string` | 否 | 单独覆盖 stdin 编码 |

`shell` 字段规则：

- 省略 `shell`：`command` 模式自动使用 `vulcan.process.launchers().default`
- `shell = true`：兼容旧语义，等价于默认 shell
- `shell = false`：只允许配合 `program` 模式；`command` 模式会报错
- `shell = "cmd" | "pwsh" | "powershell" | "bash" | "zsh" | "sh"`：显式选择命令承载器；必须出现在 `vulcan.process.launchers().shells` 中
- `program` 模式不能配 shell 名称字符串，否则会报错

`command` 模式示例：

```lua
local result = vulcan.process.exec({
    command = "echo hello",
    timeout_ms = 3000,
})
```

基于 `launchers()` 选择 shell 的示例：

```lua
local launchers = vulcan.process.launchers()
local command = launchers.default == "cmd" and "echo hello" or "printf hello"

local result = vulcan.process.exec({
    command = command,
    shell = launchers.default,
    encoding = "utf-8",
})
```

`program` 模式示例：

```lua
local result = vulcan.process.exec({
    program = "git",
    args = { "status", "--short" },
    cwd = vulcan.runtime.cwd(),
    env = {
        DEMO_MODE = "1",
    },
    timeout_ms = 5000,
    encoding = "utf-8",
})
```

### 8.3 `exec(spec)` 返回结构

返回 table 固定包含：

| 字段 | 类型 | 说明 |
| --- | --- | --- |
| `ok` | `boolean` | 进程执行链是否整体成功结束 |
| `success` | `boolean` | 进程是否未超时且以成功状态退出 |
| `code` | `integer \| nil` | 退出码；无法获取时为 `nil` |
| `stdout` | `string` | 按编码解码后的 stdout；若使用 `base64` 编码，则这里就是 Base64 文本 |
| `stderr` | `string` | 按编码解码后的 stderr |
| `stdout_encoding` | `string` | 实际 stdout 解码编码标签 |
| `stderr_encoding` | `string` | 实际 stderr 解码编码标签 |
| `stdout_lossy` | `boolean` | stdout 解码时是否发生替换或兜底 |
| `stderr_lossy` | `boolean` | stderr 解码时是否发生替换或兜底 |
| `stdout_base64` | `string \| nil` | 可用时的 stdout 原始字节 Base64 |
| `stderr_base64` | `string \| nil` | 可用时的 stderr 原始字节 Base64 |
| `timed_out` | `boolean` | 是否超时 |
| `error` | `string \| nil` | 失败时的人类可读错误摘要 |

### 8.4 `session.open(spec)` 与会话句柄

`vulcan.process.session.open(spec)` 只接受 table，字段如下：

| 字段 | 类型 | 必填 | 说明 |
| --- | --- | --- | --- |
| `program` | `string` | 是 | 可执行程序路径或命令名 |
| `args` | `string[]` | 否 | 参数数组，省略时默认为空数组 |
| `cwd` | `string` | 否 | 子进程工作目录 |
| `encoding` | `string` | 否 | 作为 stdout/stderr/stdin 默认编码 |
| `stdout_encoding` | `string` | 否 | 单独覆盖 stdout 编码 |
| `stderr_encoding` | `string` | 否 | 单独覆盖 stderr 编码 |
| `stdin_encoding` | `string` | 否 | 单独覆盖 stdin 编码 |
| `buffer_limit_bytes` | `integer` | 否 | stdout/stderr 内部缓冲上限 |

会话句柄方法：

| 方法 | 参数与类型 | 返回值 | 说明 |
| --- | --- | --- | --- |
| `session:write(...)` | 一个或多个 Lua 标量 | `true` | 按 `stdin_encoding` 编码后写入 stdin，并立即 flush |
| `session:read(options?)` | `options?: { timeout_ms?: integer, max_bytes?: integer, until_text?: string }` | 结果 table | 读取并**排空**当前已捕获输出 |
| `session:status()` | 无 | 状态 table | 查看当前运行状态，不会终止进程 |
| `session:close(options?)` | `options?: { timeout_ms?: integer }` | 状态 table | 关闭 stdin，等待退出；超时后会杀死进程树 |
| `session:kill()` | 无 | `true` | 直接终止进程树并关闭会话 |

`session:read(...)` 返回：

| 字段 | 类型 | 说明 |
| --- | --- | --- |
| `stdout` | `string` | 本次读取并排空的 stdout 文本 |
| `stderr` | `string` | 本次读取并排空的 stderr 文本 |
| `stdout_encoding` | `string` | 实际 stdout 解码编码 |
| `stderr_encoding` | `string` | 实际 stderr 解码编码 |
| `stdout_lossy` | `boolean` | stdout 是否发生 lossy 解码 |
| `stderr_lossy` | `boolean` | stderr 是否发生 lossy 解码 |
| `stdout_base64` | `string \| nil` | 可用时的原始 stdout 字节 |
| `stderr_base64` | `string \| nil` | 可用时的原始 stderr 字节 |
| `timed_out` | `boolean` | 是否在本次读取等待期内超时 |

`session:status()` / `session:close(...)` 返回：

| 字段 | 类型 | 说明 |
| --- | --- | --- |
| `running` | `boolean` | 进程是否仍在运行 |
| `exited` | `boolean` | 进程是否已经退出 |
| `success` | `boolean \| nil` | 已退出时表示退出是否成功；运行中通常为 `nil` |
| `code` | `integer \| nil` | 已退出时的退出码；无法获取时可能为 `nil` |

交互式示例：

```lua
local session = vulcan.process.session.open({
    program = "python",
    args = { "-i" },
    encoding = "utf-8",
})

session:write("print(1 + 1)\n")
local output = session:read({
    timeout_ms = 1000,
    until_text = "2",
})
local status = session:close({
    timeout_ms = 3000,
})

return vulcan.json.encode({
    stdout = output.stdout,
    exited = status.exited,
    success = status.success,
})
```

### 8.5 注意事项

- `exec(spec)` 与 `session.open(spec)` 都不会替你做 shell 转义；如果你自行拼接命令文本，需要自己处理安全性。
- `session:read(...)` 会排空缓冲；下一次读取只会拿到新增输出。
- `session:close(...)` 超时后会主动终止整个进程树，而不是仅终止直接子进程。
- `which(program)` 显式路径和 `PATH` 搜索都支持；Windows 下还会按 `PATHEXT` 自动补常见扩展名。

## 9. `vulcan.os.*`

当前提供：

- `vulcan.os.info()`

示例：

```lua
local info = vulcan.os.info()
-- info.os
-- info.arch
```

返回结构：

| 字段 | 类型 | 说明 |
| --- | --- | --- |
| `os` | `string` | 当前规范化平台名，常见为 `windows`、`linux`、`macos` |
| `arch` | `string` | 当前规范化架构名，常见为 `x86_64`、`i686`、`aarch64`、`armv7l` |

`vulcan.os.*` 当前故意只保留信息查询，不负责托管原生 `os.rename` / `os.remove` 等有平台历史包袱的接口。

## 10. `vulcan.json.*`

当前提供：

- `vulcan.json.encode(value)`
- `vulcan.json.decode(text)`
- `vulcan.json.object()` / `vulcan.json.object(table)`
- `vulcan.json.array()` / `vulcan.json.array(table)`
- `vulcan.json.null`

示例：

```lua
local payload = vulcan.json.object({
    environment = vulcan.json.object(),
    binary_path_requests = vulcan.json.array(),
    contributions = vulcan.json.array({
        "shell-hook",
    }),
    optional = vulcan.json.null,
})

local text = vulcan.json.encode(payload)
local decoded = vulcan.json.decode(text)
```

说明：

| API | 参数与类型 | 返回值 | 说明 |
| --- | --- | --- | --- |
| `vulcan.json.encode(value)` | 任意可 JSON 化 Lua 值 | `string` | 编码为 JSON 文本 |
| `vulcan.json.decode(text)` | `text: string` | 带类型的 Lua 值 | 递归保留 JSON Object、Array 与 Null |
| `vulcan.json.object(table?)` | 可选 `table` | 带标记的 `table` | 创建带 Object 标记的浅复制；无参数时创建空 Object |
| `vulcan.json.array(table?)` | 可选 `table` | 带标记的 `table` | 创建带 Array 标记的浅复制；无参数时创建空 Array |
| `vulcan.json.null` | 无 | JSON Null 哨兵 | 对象字段或数组元素需要 Null 时使用，避免 `nil` 删除字段或产生空洞 |

容器与 Null 规则：

- `decode` 产生的每个 Object 与 Array 都会递归携带原始 JSON 容器类型标记，因此再次编码时嵌套空 `{}` 与 `[]` 仍保持原类型。
- JSON Null 会解码为与 `vulcan.json.null` 相同的哨兵，可安全保留在对象字段与数组元素中。
- `object(table)` 与 `array(table)` 会浅复制源 table，不替换源 table 的元表或对象身份；嵌套值继续保留原身份。
- 容器类型标记使用受保护的共享元表；普通 Lua 代码无法通过 `getmetatable` 替换或修改运行时标记。
- Object 只允许 UTF-8 字符串键。
- Array 只允许从 1 开始、连续且为正数的整数键；字符串键、混合键、非正数索引与空洞都会返回明确编码错误。
- 构造器只接受零个参数，或恰好一个 table 参数；其他参数形式会明确报错。

标量编码规则：

- 单独作为编码值传入的 `nil` -> JSON Null
- `vulcan.json.null` -> JSON Null
- `boolean` -> JSON boolean
- `integer` / `number` -> JSON number
- `string` -> JSON string
- 带标记 `table` -> 其声明的 JSON Object 或 Array 类型

无标记 Lua table 的兼容规则：

- 若无标记 table 的 `raw_len() > 0`，运行时推断为 Array，并执行相同的连续索引严格校验
- 若 `raw_len() == 0` 且存在字符串 key，运行时推断为 Object，并要求全部键都是字符串
- 无标记空 `{}` 保留旧行为，仍编码为 `[]`；需要显式空 Object 时使用 `vulcan.json.object()`

不支持直接编码的 Lua 值：

- `function`
- `thread`
- `userdata`，但运行时已文档化的只读 JSON 上下文视图除外
- `vulcan.json.null` 以外的 `lightuserdata`

这些值传给 `vulcan.json.encode(...)` 会直接报运行时错误。

## 11. `vulcan.cache.*`

当前提供：

| API | 参数与类型 | 返回值 | 说明 |
| --- | --- | --- | --- |
| `vulcan.cache.put(value, ttl_sec?)` | `value: JSON 可序列化 Lua 值`；`ttl_sec?: integer` | `string` | 写入缓存并返回 `cache_id` |
| `vulcan.cache.get(cache_id)` | `cache_id: string` | Lua 值或 `nil` | 按当前 scope 读取缓存 |
| `vulcan.cache.delete(cache_id)` | `cache_id: string` | `boolean` | 删除缓存项 |

示例：

```lua
local cache_id = vulcan.cache.put({
    summary = "warm result",
}, 60)

local cached = vulcan.cache.get(cache_id)
local deleted = vulcan.cache.delete(cache_id)
```

注意事项：

- `value` 的可序列化规则与 `vulcan.json.encode(...)` 一致。
- 缓存作用域会优先落到当前 `tool_name`，否则落到当前 `skill_name`。
- 如果都取不到，会退化到内部 `__runtime` 作用域。
- 在 `vulcan.runtime.lua.exec(...)` 中，缓存接口会被主动清空，不可用。
- `cache_id` 是 scope 内部标识；不要假设它跨 skill 或跨宿主实例稳定。

## 12. `vulcan.context.*`

`vulcan.context` 用来读取当前请求和当前入口的运行时上下文。

### 12.1 顶层字段总览

| 字段 | 类型 | 默认值 | 说明 |
| --- | --- | --- | --- |
| `request` | `table` | 空 table | 原始请求上下文 |
| `client_info` | `table \| nil` | `nil` | 当前客户端元信息 |
| `client_capabilities` | `table` | 空 table | 宿主注入的客户端能力快照 |
| `client_budget` | `table` | 空 table | 宿主注入的预算快照 |
| `tool_config` | `table` | 空 table | 宿主注入的工具配置 |
| `host_result` | `table` | 空 table | 宿主结构化结果桥接能力视图 |
| `skill_dir` | `string \| nil` | `nil` | 当前 skill 根目录 |
| `entry_dir` | `string \| nil` | `nil` | 当前 entry 所在目录 |
| `entry_file` | `string \| nil` | `nil` | 当前 entry 文件绝对路径 |

### 12.2 `vulcan.context.request`

宿主传入的原始请求上下文对象，默认是空对象。

常见字段来自：

- `transport_name`
- `session_id`
- `request_id`
- `client_name`
- `client_info`
- `client_capabilities`

这部分结构由宿主定义，skill 不应假设一定存在某个字段。

### 12.3 `vulcan.context.client_info`

当前请求的客户端元信息，常见字段：

- `kind`
- `name`
- `version`

说明：

- 如果宿主没有注入 `client_info`，这里可能是 `nil`。
- 如果你在 `luaexec` 中看到 `name = "luaexec_call"`，那是内部隔离执行环境的模拟上下文，不是外部真实客户端。

### 12.4 `vulcan.context.client_capabilities`

宿主传入的客户端能力对象，默认是空对象。

### 12.5 `vulcan.context.client_budget`

宿主解析后的预算快照对象，默认是空对象。

该对象由宿主决定内容，但常见会包含：

- `client_name`
- `tool_name`
- `skill_name`
- `tool_result`
- `file_read`

### 12.6 `vulcan.context.tool_config`

宿主解析后的工具配置对象，默认是空对象。

### 12.7 `vulcan.context.skill_dir / entry_dir / entry_file`

当前执行 skill 的文件上下文：

- `skill_dir`：当前 skill 目录
- `entry_dir`：当前入口脚本所在目录
- `entry_file`：当前入口脚本完整路径

说明：

- 在普通 skill 调用中，这三个值通常都可用。
- 在某些 runlua / help / 非 skill 文件场景里，可能为 `nil`。
- 在 System Plugin 租约 eval 中，这三个**入口专属**字段仍为 `nil`，因为该 eval 不是 Skill 入口调用。这不表示包上下文缺失：包身份应读取递归只读的 `vulcan.runtime.system_plugin` 视图，宿主授权上下文应读取 `vulcan.runtime.workspace_root / mounts`。
- 当前实现会把 Windows verbatim 盘符/UNC 路径转换为普通等价形式，保证 Lua 侧拿到正常系统路径；不受支持的 verbatim 命名空间会被拒绝。

### 12.8 `vulcan.context.host_result`

宿主结构化结果桥接的标准化视图。

当前推荐字段包括：

- `enabled`
- `allowed_kinds`
- `max_payload_bytes`

说明：

- 当前运行时基线里 `host_result` 顶层对象始终存在，但宿主未显式开启时通常不会有 `enabled = true`。
- 支持结构化结果的 skill 不应只看 `client_capabilities.host_result` 原始对象，而应优先读取这份标准化视图。
- 当前推荐的标准结果种类是 `change_set`，用于把 IDE 每轮操作级结果独立返回给宿主。

### 12.9 结构化第四返回值

当宿主显式开启 `host_result` 时，skill 可以返回：

```lua
return content, overflow_mode, template_hint, host_result
```

其中：

- `content` 仍然是主文本结果
- `overflow_mode` 与 `template_hint` 继续按原有文本主链语义工作
- 第四返回值 `host_result` 仅作为宿主独立结构化结果源，不替代主文本结果

推荐形态：

```lua
return "Applied 1 edit.", nil, nil, {
    kind = "change_set",
    payload = {
        mode = "applied",
        summary = "Updated one file.",
        files = {
            {
                change = "modify",
                path = "D:/projects/demo/src/example.lua",
                hunks = {
                    {
                        before = "local a = 1\nlocal b = 2",
                        delete = {
                            { line = 10, content = "local x = 1" },
                            { line = 11, content = "return x" },
                        },
                        insert = {
                            { line = 10, content = "local x = 2" },
                            { line = 11, content = "local y = 3" },
                            { line = 12, content = "return x + y" },
                        },
                        after = "end\nreturn M",
                    },
                },
            },
        },
    },
}
```

注意事项：

- 宿主未开启 `host_result` 时，第四返回值会被忽略。
- `host_result` 应保持可 JSON 化。
- 对 skill 来说，`change_set` 的目标是提供操作级结果，不是替代 `git diff`。
- `change_set.payload.files` 现在应始终存在，并使用绝对路径。
- `change = "modify"` 时，必须提供非空 `hunks`；每个 `hunk` 都必须包含 `before`、`after`、`delete`、`insert`。
- `before` 与 `after` 应表示紧贴修改块前后的连续上下文字符串，不应误用为整文件快照。
- `delete[].line` 表示旧文件中的被删行号，`insert[].line` 表示新文件中的插入后行号，两者都应按升序排列。
- `change = "create"` 时，应直接提供完整文件 `content`。
- `change = "delete"` 时，支持两种内容模式：
  - `content_mode = "full"`，或完全省略 `content_mode`。这时应提供完整 `content`；`total_line_count` 在输入侧可省略，运行时会自动补齐 `content_mode = "full"` 与 `total_line_count`。
  - `content_mode = "truncated"`。这时必须提供 `total_line_count`、`content_head`、`content_tail`；`content_head` 表示前片段，`content_tail` 表示后片段，中间内容固定视为省略。
- 对 skill 开发者的强约束是：应优先在 skill 自身逻辑里主动控制超大删除结果，尽量在返回前就决定是否输出截断片段；运行时自动截断只是一层兜底保护，不应被当作常规主路径。
- `change = "delete"` 的运行时归一化规则是稳定的：
  - 如果 skill 直接返回完整 `content`，运行时会先计算 `total_line_count`。
  - 当删除内容总行数不超过 `500` 行时，运行时会保留全文，并自动规范成 `content_mode = "full"`。
  - 当删除内容总行数超过 `500` 行时，运行时会强制改写为 `content_mode = "truncated"`，即使 skill 原样返回了完整 `content` 也一样。
  - 强制截断后的固定输出格式为：前 `50` 行写入 `content_head`，后 `50` 行写入 `content_tail`，中间部分不再返回。
- `change = "rename"` 时，应提供 `old_path` 与 `new_path`，两者都必须是绝对路径。

删除记录示例：

```lua
{
    change = "delete",
    path = "D:/projects/demo/src/legacy.lua",
    content = "line 1\nline 2\nline 3\n"
}
```

上面的旧写法仍然兼容，但运行时会对宿主输出归一化为：

```json
{
  "change": "delete",
  "path": "D:/projects/demo/src/legacy.lua",
  "content_mode": "full",
  "total_line_count": 3,
  "content": "line 1\nline 2\nline 3\n"
}
```

当删除文件超过 `500` 行时，运行时会自动改写为：

```json
{
  "change": "delete",
  "path": "D:/projects/demo/src/legacy.lua",
  "content_mode": "truncated",
  "total_line_count": 1200,
  "content_head": "前 50 行拼接后的文本",
  "content_tail": "后 50 行拼接后的文本"
}
```

## 13. `vulcan.deps.*`

当前字段包括：

| 字段 | 类型 | 说明 |
| --- | --- | --- |
| `vulcan.deps.tools_path` | `string \| nil` | 当前 skill 的工具依赖目录 |
| `vulcan.deps.lua_path` | `string \| nil` | 当前 skill 的 Lua 依赖目录 |
| `vulcan.deps.ffi_path` | `string \| nil` | 当前 skill 的 FFI 依赖目录 |

这三项表示当前 skill 的依赖根路径：

- 工具依赖目录
- Lua 依赖目录
- FFI 依赖目录

示例：

```lua
local lua_lib_root = vulcan.deps.lua_path
local ffi_root = vulcan.deps.ffi_path
```

注意事项：

- 这三项依赖当前 skill 所在根目录和宿主依赖布局。
- 如果当前没有有效 skill 上下文，它们会是 `nil`。
- 在 System Plugin 租约 eval 中，这组旧 Skill 依赖根字段仍为 `nil`。受管 Python/Node 的 status、invoke 与 session API 会改为解析 `system_package` 绑定的精确 `dependencies_file`；不要用 `vulcan.deps.*` 判断 System 包是否声明了依赖。
- skill 应当只依赖这些协议暴露出的路径，不要自己猜宿主物理目录结构。

## 14. `vulcan.sqlite.*`

`vulcan.sqlite` 是**按当前 skill 作用域隔离**的 SQLite 绑定。

### 14.1 当前字段与方法

- `vulcan.sqlite.enabled`
- `vulcan.sqlite.info()`
- `vulcan.sqlite.status()`
- `vulcan.sqlite.tokenize_text(input)`
- `vulcan.sqlite.execute_script(input)`
- `vulcan.sqlite.execute_batch(input)`
- `vulcan.sqlite.query_json(input)`
- `vulcan.sqlite.query_stream(input)`
- `vulcan.sqlite.query_stream_wait_metrics(input)`
- `vulcan.sqlite.query_stream_chunk(input)`
- `vulcan.sqlite.query_stream_close(input)`
- `vulcan.sqlite.upsert_custom_word(input)`
- `vulcan.sqlite.remove_custom_word(input)`
- `vulcan.sqlite.list_custom_words()`
- `vulcan.sqlite.ensure_fts_index(input)`
- `vulcan.sqlite.rebuild_fts_index(input)`
- `vulcan.sqlite.upsert_fts_document(input)`
- `vulcan.sqlite.delete_fts_document(input)`
- `vulcan.sqlite.search_fts(input)`

除 `enabled` 之外，这些方法可分为两类：

| 类别 | API | 入参 | 返回值 | 说明 |
| --- | --- | --- | --- | --- |
| 固定探测 | `enabled` | 无 | `boolean` | 当前 skill 是否已绑定 SQLite |
| 固定探测 | `info()` | 无 | `table` | 绑定信息；至少可用于探测是否可用 |
| 固定探测 | `status()` | 无 | `table` | 当前状态；至少可用于探测是否可用 |
| provider 转发 | 其余全部方法 | 单个 `input: table` 或无参 | `table`、数组、标量，取决于 provider | LuaSkills 只负责把动作和当前绑定上下文转发给宿主/provider |

动作映射关系：

| Lua API | provider action |
| --- | --- |
| `execute_script(input)` | `execute_script` |
| `execute_batch(input)` | `execute_batch` |
| `query_json(input)` | `query_json` |
| `query_stream(input)` | `query_stream` |
| `query_stream_wait_metrics(input)` | `query_stream_wait_metrics` |
| `query_stream_chunk(input)` | `query_stream_chunk` |
| `query_stream_close(input)` | `query_stream_close` |
| `tokenize_text(input)` | `tokenize_text` |
| `upsert_custom_word(input)` | `upsert_custom_word` |
| `remove_custom_word(input)` | `remove_custom_word` |
| `list_custom_words()` | `list_custom_words` |
| `ensure_fts_index(input)` | `ensure_fts_index` |
| `rebuild_fts_index(input)` | `rebuild_fts_index` |
| `upsert_fts_document(input)` | `upsert_fts_document` |
| `delete_fts_document(input)` | `delete_fts_document` |
| `search_fts(input)` | `search_fts` |

### 14.2 行为规则

- `enabled = true` 表示当前 skill 已绑定 SQLite 能力。
- `info()` / `status()` 总是存在。
- 当 SQLite 未启用时：
  - `enabled = false`
  - `info()` / `status()` 会返回禁用状态描述
  - 其余方法会直接报错：`current skill has not enabled sqlite`

稳定开发约束：

- `input` 一律应传 Lua table，不要传裸字符串或裸数组。
- 返回值结构由当前 SQLite provider 决定；LuaSkills 不在运行时对业务字段做二次改写。
- 宿主/provider 侧总会额外拿到绑定上下文，例如 `space_label`、`skill_id`、`binding_tag`、`database_kind`、`default_database_path`。

### 14.3 开发建议

- 把 `info()` / `status()` 当成探测入口。
- 业务调用前先判断 `enabled`，避免把“能力未绑定”误当成查询失败。
- 具体输入输出字段请结合宿主的 SQLite provider 契约与：
  - [宿主数据库 Provider 对接说明](providers/host-database-provider-guide.md)
- 如果你是 skill 作者而不是宿主实现者，建议优先参考对应记忆类 skill 的现有调用示例，而不是猜测 SQL provider 的自定义返回字段。

## 15. `vulcan.lancedb.*`

`vulcan.lancedb` 是**按当前 skill 作用域隔离**的 LanceDB 绑定。

### 15.1 当前字段与方法

- `vulcan.lancedb.enabled`
- `vulcan.lancedb.info()`
- `vulcan.lancedb.status()`
- `vulcan.lancedb.create_table(input)`
- `vulcan.lancedb.vector_upsert(input)`
- `vulcan.lancedb.vector_search(input)`
- `vulcan.lancedb.delete(input)`
- `vulcan.lancedb.drop_table(input)`

| 类别 | API | 入参 | 返回值 | 说明 |
| --- | --- | --- | --- | --- |
| 固定探测 | `enabled` | 无 | `boolean` | 当前 skill 是否已绑定 LanceDB |
| 固定探测 | `info()` | 无 | `table` | 绑定信息；至少可用于探测是否可用 |
| 固定探测 | `status()` | 无 | `table` | 当前状态；至少可用于探测是否可用 |
| provider 转发 | `create_table(input)` | `input: table` | provider 定义 | 创建表 |
| provider 转发 | `vector_upsert(input)` | `input: table` | provider 定义 | 写入或更新向量数据 |
| provider 转发 | `vector_search(input)` | `input: table` | provider 定义 | 向量检索 |
| provider 转发 | `delete(input)` | `input: table` | provider 定义 | 删除数据 |
| provider 转发 | `drop_table(input)` | `input: table` | provider 定义 | 删除表 |

### 15.2 行为规则

- `enabled = true` 表示当前 skill 已绑定 LanceDB 能力。
- `info()` / `status()` 总是存在。
- 当 LanceDB 未启用时：
  - `enabled = false`
  - `info()` / `status()` 会返回禁用状态描述
  - 其余方法会直接报错：`current skill has not enabled lancedb`

稳定开发约束：

- `input` 应传单个 Lua table。
- 具体业务字段与返回值由 LanceDB provider 定义；LuaSkills 只做转发与能力守卫。
- 宿主/provider 同样会拿到稳定绑定上下文，例如 `space_label`、`binding_tag`、`database_kind` 等。

### 15.3 特别说明

`vector_search(input)` 的结果里可能出现两种载荷形态：

- `data_json`
- `data`

当返回格式是 JSON 时，结果表里会放 `data_json`；否则会放原始二进制字符串 `data`。

具体输入输出字段请结合宿主的 LanceDB provider 契约与：

- [宿主数据库 Provider 对接说明](providers/host-database-provider-guide.md)

## 16. 常见开发建议

### 16.1 先探测，再调用

对宿主条件注入的能力，建议先探测：

```lua
if vulcan.sqlite.enabled then
    return vulcan.sqlite.query_json({
        sql = "select 1 as ok",
    })
end

return {
    ok = false,
    reason = "sqlite-disabled",
}
```

### 16.2 不要依赖内部字段名

这些属于内部机制，不建议 skill 长期依赖：

- `vulcan.runtime.internal.*`
- `vulcan.__sqlite_skill_name`
- `vulcan.__lancedb_skill_name`

### 16.3 不要猜测宿主目录布局

优先使用：

- `vulcan.context.skill_dir`
- `vulcan.context.entry_dir`
- `vulcan.context.entry_file`
- `vulcan.deps.*`

不要自己反推：

- runtime 根目录
- 其他 skill 的依赖目录
- 宿主是否使用了某个固定目录名

### 16.4 区分“外部请求上下文”和“内部 luaexec 上下文”

如果你在普通 skill 执行时看到真实客户端名称，而在 `vulcan.runtime.lua.exec(...)` 里看到 `luaexec_call`，这是当前设计的正常行为，不是 Bug。

## 17. 推荐阅读顺序

如果你主要是写 skill，建议按下面顺序阅读：

1. 本文档：了解当前 `vulcan.*` 真实能力面
2. [README.zh-CN.md](../../README.zh-CN.md)：理解运行时定位与宿主边界
3. [宿主数据库 Provider 对接说明](providers/host-database-provider-guide.md)：了解 SQLite / LanceDB 与宿主的对接契约

如果后续你要做宿主集成而不是写 skill，请改看：

- [FFI 对接文档](ffi/integration-guide.md)
- [FFI 宿主接入检查清单](ffi/host-checklist.md)
