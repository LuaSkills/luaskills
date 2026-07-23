# Skill 包级配置声明、运行时控制与宿主对接

## 1. 适用范围

LuaSkills 的配置对象是**技能包**，不是包内的独立 entry。

- 声明写在技能包根目录 `skill.yaml` 的顶层 `config` 字段。
- 同一技能包的全部 entries 共享同一份声明和值。
- 配置命名空间使用物理目录绑定后的稳定 `skill_id`。
- entry 名称变化不会改变配置命名空间。
- `config` 写在某个 entry 内会被视为未知字段并拒绝加载。
- 不同技能包可以声明完全不同的配置项。

配置缺失或旧值非法不会阻止技能包加载。技能包应在真正需要配置的操作前调用 `vulcan.config.status()`，并给出可执行提示。

## 2. 完整声明示例

```yaml
name: example-skill
version: 1.0.0
enable: true
debug: false

config:
  - key: api_token
    type: string
    required: true
    sensitive: true
    description: Service access token
    constraints:
      min_length: 1
      max_length: 4096

  - key: retry_count
    type: integer
    default: 3
    description: Request retry count
    constraints:
      minimum: 0
      maximum: 10

  - key: temperature
    type: float
    default: 0.7
    description: Model sampling temperature
    constraints:
      minimum: 0.0
      maximum: 2.0

  - key: provider
    type: enum
    required: true
    description: Service provider
    options:
      - value: openai
        label: OpenAI
        description: OpenAI service
      - value: local
        label: Local service
        description: User-managed local service

  - key: telemetry_enabled
    type: boolean
    default: false
    description: Whether telemetry is enabled

entries:
  - name: query
    description: Execute a query
    lua_entry: runtime/query.lua
    lua_module: example-skill.query
  - name: status
    description: Inspect configuration status
    lua_entry: runtime/status.lua
    lua_module: example-skill.status
```

`query` 与 `status` 共享上述五个配置项。

## 3. 公共字段

| 字段 | 必填 | 说明 |
|---|---:|---|
| `key` | 是 | 包内稳定参数名；不得为空或包含首尾空白；同一包内不得重复 |
| `type` | 是 | `integer`、`string`、`float`、`enum`、`boolean` |
| `required` | 否 | 默认 `false`；没有显式值和默认值时是否计为缺失 |
| `default` | 否 | 类型化 YAML 值；加载时按与写入相同的规则校验；不会自动写入配置文件 |
| `sensitive` | 否 | 默认 `false`；只提供给宿主作为权限和展示提示 |
| `description` | 是 | 技能包作者提供的人类可读说明，不得为空 |
| `constraints` | 否 | 类型专属约束 |
| `options` | enum 必填 | 枚举选项，机器值不得重复 |

清单结构采用严格未知字段检查。拼错字段名，或写入未支持的国际化字段，都不会被静默忽略。

## 4. 文本语言规则

LuaSkills 不为配置声明单独实现国际化，也不解析语言标记。`description`、枚举 `label` 和枚举 `description` 均为技能包作者直接提供的单一文本。

- 技能包作者可以选择适合目标用户的任意语言。
- 面向广泛分发、跨地区宿主或公共生态的技能包建议统一使用英文。
- LuaSkills 不强制英文，也不执行翻译、语言匹配或回退。
- 宿主收到的文本与技能包声明一致，不包含语言标记或解析后的语言字段。
- 如果开发者需要多语言体验，应由技能包自身或上层产品作为完整能力设计，不应在配置声明中加入私有 `*_i18n` 字段。

## 5. 类型、约束与持久化格式

配置文件只保存字符串。类型声明负责把宿主或 Lua 写入的字符串校验并规范化。

| 类型 | YAML 默认值 | 可用约束 | 写入输入 | 持久化字符串 |
|---|---|---|---|---|
| `integer` | YAML 整数 | `minimum`、`maximum`，包含边界，必须为 i64 | 可含首尾空白的十进制整数 | i64 十进制规范形式，如 `003` 保存为 `3` |
| `string` | YAML 字符串 | `min_length`、`max_length`，按 Unicode 标量数量计算 | 任意 UTF-8 字符串 | 原样保存，不自动 trim |
| `float` | YAML 数字 | `minimum`、`maximum`，包含边界 | 可解析为 f64 的字符串 | 有限 f64 的规范十进制形式，如 `0.500` 保存为 `0.5` |
| `enum` | 选项中的字符串 `value` | 不允许 `constraints` | 必须精确匹配某个 `value` | 对应稳定机器值 |
| `boolean` | YAML 布尔值 | 不允许 `constraints` | 严格为小写 `true` 或 `false`，可有首尾空白 | `true` 或 `false` |

额外规则：

- `integer` 不允许长度约束。
- `float` 不允许长度约束，也不允许 `NaN`、`inf`、`-inf`。
- `string` 不允许数值范围。
- `enum` 至少有一个选项；每项必须包含非空 `value`、`label`、`description`。
- `boolean` 和 `enum` 不允许任何 `constraints`。
- 下界不得大于上界，最小长度不得大于最大长度。
- 默认值必须使用声明类型，并满足全部约束。

## 6. 有效值、完整性与升级

有效值解析顺序：

1. 已持久化的显式值；
2. 清单中的默认值；
3. 未设置。

`required=true` 只影响完整性判定，不阻止包加载。一个包在以下条件同时满足时 `complete=true`：

- 每个必填项都有显式值或合法默认值；
- 每个已持久化且仍然声明的值都满足当前声明。

升级后可能出现两类状态：

- **invalid**：key 仍被声明，但旧值不满足新类型、范围、长度或枚举。
- **orphaned**：配置文件中存在，但当前包不再声明该 key。

orphaned 不影响 `complete`，但会通过 `orphaned_count` 报告。技能包内部看不到 orphaned 的 key 和 value；宿主仍可通过原始 `list/get/delete` 管理它们。宿主 `set` 永远不能绕过声明。

## 7. Lua API 与包隔离

所有 Lua API 隐式使用当前正在执行的技能包，不接受 `skill_id` 参数。授权身份由 Rust 侧执行上下文持有；修改 Lua 可见的 `vulcan.runtime.internal.skill_name` 不会改变配置归属，也不能用于跨包读写。

| API | 语义 |
|---|---|
| `vulcan.config.get(key)` | key 必须声明；返回显式值、默认值或 `nil` |
| `vulcan.config.has(key)` | key 必须声明；显式值或默认值存在时返回 `true` |
| `vulcan.config.set(key, value)` | key 必须声明；校验、规范化、持久化后返回 `true` |
| `vulcan.config.delete(key)` | key 必须声明；只删除显式值，之后可回退默认值 |
| `vulcan.config.list()` | 只列当前包声明项的有效值，不包含 orphaned |
| `vulcan.config.describe()` | 返回当前包结构；不返回 `value` |
| `vulcan.config.status()` | 返回 `complete`、`missing`、`invalid`、`orphaned_count` |

嵌套调用时，包 A 调用包 B 的 entry，包 B 内只能访问包 B 配置；包 B 返回后恢复包 A 配置上下文。包内 API 禁止跨包读写。

```lua
local status = vulcan.config.status()
if not status.complete then
    return [[This skill package configuration is incomplete.

Ask the AI to call the host runtime-config tool:
1. Use action=describe to inspect names, types, constraints, and descriptions.
2. After host or user authorization, use action=set for required values.

If configuration mutation is unavailable, ask the user to provide the missing values.]]
end

local token = vulcan.config.get("api_token")
```

推荐技能在缺配置时返回清晰帮助，而不是抛出难以理解的内部错误。

## 8. Rust 宿主 API

结构与状态类型从 crate 根导出，包括声明类型、运行时描述符、状态、问题和校验错误。

主要方法：

```rust
engine.describe_skill_package_config(skill_id, include_values)
engine.validate_skill_package_config(skill_id)
engine.list_skill_config_entries(skill_id)
engine.get_skill_config_value(skill_id, key)
engine.set_skill_config_value(skill_id, key, value)
engine.delete_skill_config_value(skill_id, key)
```

管理面语义：

- `describe` 返回声明、约束、作者提供的说明、值来源和有效性。
- `skill_id=None` 时按技能包标识排序返回全部有效包。
- `include_values=false` 时 JSON 中完全省略 `value`。
- `include_values=true` 时返回未遮罩的有效值；非法旧值也会原样返回并附带结构化 `validation_error`。
- `validate` 只读，不修改持久化状态。
- 原始 `get` 只读取已保存值，不回退默认值。
- 原始 `list/get/delete` 可处理 orphaned。
- `set` 要求目标包当前有效、key 已声明，并返回规范化后的最终字符串。

## 9. 标准 C ABI

结构与状态接口：

```c
int32_t luaskills_ffi_skill_config_describe(
    uint64_t engine_id,
    const char *skill_id,       /* 可为 NULL */
    uint8_t include_values,     /* 只能是 0 或 1 */
    FfiOwnedBuffer *result_json_out,
    FfiOwnedBuffer *error_out
);

int32_t luaskills_ffi_skill_config_validate(
    uint64_t engine_id,
    const char *skill_id,
    FfiOwnedBuffer *result_json_out,
    FfiOwnedBuffer *error_out
);
```

原有 `list/get/set/delete` 继续作为宿主管理面。`set` 严格受当前有效包声明和类型约束限制；`delete` 仍允许清理 orphaned。`include_values` 传入除 `0`、`1` 以外的值会失败。

## 10. JSON FFI

结构查询：

```json
{
  "engine_id": 42,
  "skill_id": "example-skill",
  "include_values": false
}
```

调用 `luaskills_ffi_skill_config_describe_json`。状态查询：

```json
{
  "engine_id": 42,
  "skill_id": "example-skill"
}
```

调用 `luaskills_ffi_skill_config_validate_json`。请求采用严格未知字段检查，`include_values` 省略时为 `false`。

## 11. 宿主权限与安全边界

LuaSkills 不负责判断“当前用户是否有权读取或修改配置值”，也不根据 `sensitive` 自动遮罩。

宿主或上层封装必须自行决定：

1. 是否向当前调用方暴露原始 `list/get`。
2. 是否允许结构查询设置 `include_values=true`。
3. 是否直接允许 `set/delete`。
4. 是否在修改前通知用户并取得授权。
5. 是否对 `sensitive=true` 的值进行日志过滤、界面遮罩和模型上下文隔离。

推荐默认策略：

- 结构发现使用 `include_values=false`；
- 只有明确需要且已经授权时才使用 `include_values=true`；
- `set/delete` 由宿主执行用户、租户、角色或交互确认策略；
- 不把包含敏感值的响应写入普通日志；
- 不把 `sensitive` 当作 LuaSkills 已实施的安全机制。

## 12. 推荐统一宿主工具

建议上层提供一个 `runtime-config` 工具：

| action | 参数 | 用途 |
|---|---|---|
| `describe` | `skill_id?`、`include_values?` | 获取配置列表、说明、类型、约束、枚举和状态 |
| `validate` | `skill_id` | 获取缺失、非法与 orphaned 数量 |
| `list` | `skill_id?` | 获取原始持久化记录 |
| `get` | `skill_id`、`key` | 获取原始持久化值 |
| `set` | `skill_id`、`key`、`value` | 校验并设置声明项 |
| `delete` | `skill_id`、`key` | 删除显式值或清理 orphaned |

技能返回缺配置提示时，应指导 AI 先用 `describe` 发现参数要求，再由宿主决定直接设置、强制指定值、请求用户授权，或要求用户提供值。

## 13. 文件路径与格式

宿主可通过 `LuaRuntimeHostOptions.skill_config_file_path` 指定文件。未指定时使用：

```text
<runtime_root>/config/skill_config.json
```

文件按 `skill_id` 分组，值全部为字符串：

```json
{
  "skills": {
    "example-skill": {
      "api_token": "sk-xxx",
      "retry_count": "3",
      "temperature": "0.7",
      "provider": "openai",
      "telemetry_enabled": "false"
    }
  }
}
```

运行时使用进程内共享路径锁和临时文件原子替换。直接手工编辑可以产生不符合声明的旧值；运行时不会静默修复，而是通过 `status/validate/describe` 明确报告。

## 14. 可运行示例

仓库内的 [skill-package-config 示例](../../../examples/skill-package-config/README.md) 可以直接作为第三方包模板。它包含五种类型、两个共享配置的 entries、缺配置提示、合法规范化写入和非法范围写入。
