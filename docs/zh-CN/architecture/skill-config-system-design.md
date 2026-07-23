# Skill 包级配置契约与宿主接入

LuaSkills 的配置归属单位始终是技能包目录，不是 entry。一个包内所有 entry 共享同一份声明、持久化值、revision 和业务校验器；Lua 只能访问当前正在执行的包，不能指定其他 `skill_id`。

配置声明采用技能包作者选择的单一语言文本。生态发布建议使用英文，但运行时不强制，也不支持 `locale`、`*_i18n` 或语言回退。

## 唯一声明格式

声明只能位于 `skill.yaml` 顶层：

```yaml
name: example-config-skill
version: 1.0.0
enable: true
debug: false
config_validator: runtime/config-validator.lua

config:
  - key: api_token
    type: string
    required: true
    sensitive: true
    description: Service access token
    title: API token
    group: Connection
    order: 10
    placeholder: sk-...
    format: password
    restart_required: true
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
    advanced: true
    constraints:
      minimum: 0.0
      maximum: 2.0

  - key: provider
    type: enum
    default: openai
    description: Service provider
    options:
      - value: openai
        label: OpenAI
        description: OpenAI service
      - value: local
        label: Local
        description: User-managed local service

  - key: telemetry_enabled
    type: boolean
    default: false
    description: Whether telemetry is enabled

entries: []
```

公共字段为：

- `key`：包内唯一稳定键，只允许小写 ASCII 字母开头，后续可使用小写字母、数字、`_`、`-`、`.`。
- `type`：`integer`、`string`、`float`、`enum` 或 `boolean`。
- `description`：必填单语言说明。
- `required`、`sensitive`：默认 `false`。
- `default`、`example`：必须与声明类型一致；默认值即使敏感也属于公开声明元数据。
- `title`、`group`、`order`、`advanced`、`placeholder`：可选 UI 提示。
- `format`：可选 `text`、`password`、`uri`、`path`、`file`、`directory`、`multiline`。
- `restart_required`：提示宿主修改后可能需要执行重启流程。
- `deprecated`、`deprecation_message`：弃用状态与迁移说明。
- `constraints`：整数/浮点使用包含式 `minimum`、`maximum`；字符串使用 Unicode 标量数量 `min_length`、`max_length`。
- `options`：仅 `enum` 使用，每项必须有 `value`、`label`、`description`。

未知字段、重复 key、错误类型、非有限浮点、超限文本和超过 JavaScript 安全整数范围的整数都会被拒绝。entry 内声明 `config` 或 `config_validator` 也会作为未知字段失败。

## 数值与存储公约

跨 Rust、Lua、TypeScript、Python、Go 和 JSON 的公共整数范围是：

```text
-9007199254740991 .. 9007199254740991
```

浮点必须是有限 IEEE-754 双精度值；负零规范化为 `0`。持久化值统一保存为规范字符串：

- integer：十进制，无前导零；
- float：规范有限十进制表示；
- boolean：`true` 或 `false`；
- string/enum：UTF-8 原文。

唯一持久化文档格式为：

```json
{
  "format_version": 1,
  "revision": "12",
  "skills": {
    "example-config-skill": {
      "api_token": "secret",
      "retry_count": "3"
    }
  }
}
```

没有旧格式迁移或兼容读取。`format_version`、`revision`、`skills` 之外的字段以及任意重复对象 key 都会失败。revision 使用规范无符号十进制字符串，避免跨语言精度损失。

宿主必须显式提供绝对 `skill_config_root`。LuaSkills 不从当前目录或运行时根推导配置位置：

```text
<skill_config_root>/skills/config.json
<skill_config_root>/system-skills/config.json
```

`ROOT` 中的系统技能写入第二个用户级文件；其他根写入第一个文件。依赖仍保持系统级。卸载技能不会删除配置，清理由上级产品按自身生命周期策略负责。

## 一致性、缓存与监听

每个文件使用稳定伴随锁 `<config.json>.lock`。写入流程为：获取跨进程锁、重新读取磁盘最新合法版本、校验可选 expected revision、完成整批校验、写入同目录唯一临时文件、同步并原子替换、更新内存快照。默认锁等待 5 秒，可由宿主在 1–60000 毫秒内配置。

普通读取只访问最后一个合法不可变缓存快照。父目录文件监听默认执行 200 毫秒防抖；外部合法且 revision 更大的原子替换会自动重载。相同 revision 不同内容、revision 回退、删除已有文件、非法 JSON 或超限文件都不会污染快照，并产生结构化失败事件。宿主也可以显式 `refresh`。

本契约只保证同一台机器、同一文件系统上的强一致事务，不提供网络文件系统或分布式一致性承诺。

## 批量修改与 CAS

批量写入是唯一底层写实现。任意一项的声明、类型、范围或业务校验失败时，文件、revision、缓存和事件都不变。成功事务只增加一次 revision 并产生一个逻辑事件。

Lua 侧支持：

```lua
vulcan.config.set("retry_count", 5)
vulcan.config.set({
    retry_count = 5,
    telemetry_enabled = true,
})
```

Lua 直接从当前执行上下文取得包标识，因此无法跨包读写。宿主管理面必须显式传入 `skill_id`，并可使用 `expected_revision` 实现 CAS。删除也支持 CAS，并允许清理 orphaned key。

## 受限业务校验器

`config_validator` 是技能包内相对路径，目标必须是普通 `.lua` 文件。脚本返回一个函数，该函数接收包含默认值与显式值的完整类型化配置：

```lua
return function(values)
    if values.provider == "local" and values.temperature > 1.0 then
        return {
            {
                key = "temperature",
                code = "local_temperature_too_high",
                message = "Local provider requires temperature at or below 1.0",
            },
        }
    end
    return {}
end
```

问题的 `key` 可省略；存在时必须引用已声明键。`code` 会以 `skill.` 前缀进入宿主协议。校验器运行在独立 Lua 状态中，没有文件、网络、工具、技能调用、配置写入、动态加载或调试能力，并受源码大小、内存、指令、时间、问题数量和消息长度上限约束。

校验器错误不得把敏感值写入错误消息。运行时会对 issue 消息中的已声明敏感有效值做替换，并把脚本异常收敛为不包含原始异常文本的稳定错误。

## `runtime-config` 共同工具契约

建议上级统一封装工具名 `runtime-config`，支持：

| action | 参数 | 结果 |
|---|---|---|
| `describe` | `skill_id?`、`mode?`、`root_name?`、`include_values?` | 声明、类型、说明、约束、默认值、UI 元数据、状态 |
| `validate` | `skill_id` | 完整性、静态问题、业务问题、orphaned |
| `list` | `skill_id?` | 原始持久化条目；每条都携带 `store_scope`，可区分普通与系统存储中的同名历史记录 |
| `get` | `skill_id`、`key` | 单个原始值 |
| `set` | `skill_id`、`values` 或 `key/value`、`expected_revision?` | 原子写入结果 |
| `delete` | `skill_id`、`key`、`expected_revision?` | 删除结果 |
| `refresh` | `store_scope?` | 一个或两个存储的刷新结果 |

请求拒绝未知字段，也拒绝“字段已知但与当前 action 无关”的组合。`set` 的 `values` 与 `key/value` 互斥，空批次失败。

完整工具响应按流式 JSON 编码计数，超过 64 MiB 时返回 `CONFIG_RESPONSE_TOO_LARGE`，不会先构造第二份超大编码缓冲。非敏感值出现在诊断中时，单个预览最多 4096 个 UTF-8 字节，并在合法字符边界追加 `[truncated]`；敏感值不生成预览。

### 模型可见模式

LuaSkills 不做用户授权、敏感值披露判断或加密。上级宿主应：

1. 默认只向模型开放 `describe(mode=effective, include_values=false)` 与 `validate`。
2. 默认禁止模型选择 `mode=installed` 或 `root_name`，避免把物理安装路径作为普通模型上下文披露；确有诊断需要时由宿主授权并裁剪 `absolute_path`。
3. 自行决定是否开放 `get`、`list`、`include_values=true`、`set`、`delete`、`refresh`。
4. 对读取原始值或修改配置执行产品自己的显式允许、强制策略或用户确认。
5. 不把敏感值写入普通日志、工具摘要或错误。

`include_values=false` 时响应完全省略运行时 `value`，但始终保留声明的 `default`。`include_values=true` 返回未遮罩值，调用前必须由宿主完成授权。

### 宿主私有模式

同一个 dispatcher 可以不注册为模型工具，由宿主设置页、CLI、管理 API 或后台服务直接调用。权限仍由宿主决定；LuaSkills 只负责声明约束、包路由、事务、快照和事件。

## 缺配置时的技能帮助

技能发现配置不完整时，应先返回可执行的英文提示，避免猜测参数或要求用户编辑内部文件：

```text
This skill package configuration is incomplete.
Ask the AI to call runtime-config with action=describe and this package id.
After host or user authorization, set the missing declared keys.
If mutation is unavailable, ask the user to provide the listed parameters.
```

技能可以通过 `vulcan.config.status()` 获得缺失项和问题，通过 `vulcan.config.describe()` 获得声明结构。不得把敏感当前值拼进帮助文本。

## 事件

配置事件使用引擎内单调十进制 `sequence`，来源只有 `local_write` 或 `external_reload`。事件包含存储作用域、revision、变更键、需要重启的键和可选完整性；失败事件包含稳定 code/message，不含配置值。

事件队列有界。分页返回的 `next_sequence` 只前进到本页实际返回的最后一个事件，不能跳过尚未读取的事件；过期或超前游标会显式失败。SDK 提供 poll、wait 与 callback 封装。

## installed 与 effective 发现

`describe(mode=effective)` 返回当前根优先级下真正生效的包。`describe(mode=installed)` 只解析每个物理目录的 `skill.yaml`，不执行 Lua，可诊断被遮蔽、禁用、空目录停用标记和非法清单。installed 模式不返回配置值，并可用 `root_name` 过滤。

## 相关入口

- [技能开发指南](../../skill-development.md)
- [FFI 对接指南](../ffi/integration-guide.md)
- [完整示例](../../../examples/skill-package-config/README.md)
