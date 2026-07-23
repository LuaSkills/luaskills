# LuaSkills 技能包配置生产化完善与三 SDK 同步方案

## 计划状态

- 状态：已完成
- 计划日期：2026-07-23
- 计划编号：03
- 目标版本：LuaSkills core、TypeScript SDK、Python SDK、Go SDK 统一由 `0.5.4` 升级为 `0.5.5`
- 执行原则：当前方案定义的格式和接口是唯一受支持版本，不保留此前未发布实现的兼容入口、兼容读取、迁移脚本或字段别名
- 计划完成条件：实现、测试、文档、三个 SDK、发布链路全部闭环，并连续五轮代码审核未发现任何问题

## 一、任务背景

当前工作区已经具备第一版技能包级配置声明和管理能力，但该实现仍属于未正式发布的中间状态，主要存在以下生产化缺口：

1. 宿主虽然能通过 Rust、C ABI 和 JSON FFI 使用配置能力，但缺少一份同时覆盖“暴露给模型”和“仅宿主内部使用”的共同工具契约。
2. 配置文件写入只有进程内互斥，无法防止多个 CLI、TUI 或宿主进程同时读改写造成更新丢失。
3. 查询路径每次读取磁盘，没有稳定缓存快照，也没有外部修改自动重载。
4. 设置接口仍以单键为主，无法把同一技能包的多个配置作为一个原子事务提交。
5. `sensitive`、默认值、宿主授权和模型披露之间的边界需要固定。
6. Rust、Lua、JSON、TypeScript、Python、Go 的数字能力不同，缺少共同可无损表达的数值契约。
7. 持久化文件没有格式版本、修订号和并发更新依据。
8. 配置结构发现依赖已启用并加载的技能，宿主无法只读检查已安装但禁用或被遮蔽的技能包。
9. 第一版未发布格式包含不再需要的兼容思路，应全部删除。
10. 卸载时是否删除长期配置尚未形成明确规则。
11. 静态类型约束无法表达跨字段和业务条件校验。
12. 配置结构缺少宿主构建表单所需的通用 UI 提示元数据。
13. 声明、值、文件、响应和错误文本缺少统一安全上限。
14. 配置 key 只检查非空和首尾空白，缺少可跨语言稳定使用的命名规范。
15. 三个 SDK 的配置测试仍偏向模拟客户端，缺少从 SDK 到真实动态库、真实技能包和真实配置文件的端到端验证。
16. 核心与三个 SDK 的请求、响应、错误码和枚举容易由手写产生漂移。
17. 当前 `valid`、`configured`、`source` 等字段组合容易产生互相矛盾的状态。
18. 系统技能的依赖应保持系统级，但配置必须写入用户级专用目录，不能写回系统安装目录。
19. 配置变化没有事件，宿主只能轮询。

本方案在不保留旧兼容层的前提下，把以上能力收敛成唯一协议、唯一存储格式和统一生成的跨语言契约。

## 二、已确认的产品决策

| 编号 | 最终决策 |
|---|---|
| 1 | 定义统一宿主配置工具契约，分别说明模型可见模式和宿主私有模式 |
| 2 | 使用跨进程伴随锁文件保护完整读改写事务 |
| 3 | 默认读取内存缓存快照，监听配置文件并自动重载 |
| 4 | 核心写入以批量事务为唯一实现；Lua `set` 直接支持单键和 table 两种形式；外部 SDK按语言能力提供重载或两个方法 |
| 5 | 默认值属于公开声明元数据，不受 `sensitive` 披露限制 |
| 6 | 定义所有语言都能无损处理的数值最大公约数 |
| 7 | 持久化文件增加严格格式版本和单调修订号 |
| 8 | 支持只读发现已安装、禁用和被遮蔽技能包的配置声明 |
| 9 | 删除所有旧配置格式、旧字段、旧调用形态和迁移兼容，不提供兼容脚本 |
| 10 | 卸载不删除配置文件和技能包配置值，长期保留；清理由上级产品负责 |
| 11 | 增加独立的跨字段业务校验入口 |
| 12 | 增加单语言 UI 提示元数据 |
| 13 | 设置足够宽松的硬上限；业务数据超限拒绝，诊断和展示预览安全截断 |
| 14 | 使用严格、跨语言稳定的配置 key 规范 |
| 15 | 三个 SDK 增加真实动态库端到端测试 |
| 16 | 核心生成公共契约，SDK 不再各自手写同一批线协议类型 |
| 17 | 使用单一状态枚举和 `satisfied` 消除组合状态歧义 |
| 18 | 系统技能依赖仍在系统级，配置进入显式用户配置根下的系统技能专用目录 |
| 19 | 通过现有引擎事件队列发布配置变更和重载失败事件 |

## 三、范围与非目标

### 3.1 本次范围

1. 技能包 `skill.yaml` 配置声明模型。
2. Rust 配置存储、缓存、监听、跨进程事务、声明注册表和配置服务。
3. Lua 包内配置 API。
4. Rust 宿主 API、标准 C ABI、公共 JSON FFI。
5. 统一宿主工具协议和授权接入说明。
6. TypeScript、Python、Go 三个 SDK 的类型、方法、文档和真实端到端测试。
7. 示例技能包、帮助文档、架构文档和生成契约。
8. `0.5.5` 版本升级、提交、推送、标签和对应发布。

### 3.2 明确非目标

1. 不为配置声明实现 i18n，不增加语言标签、语言列表、回退规则或 `*_i18n` 字段。
2. 不自动加密配置文件。
3. 不由 LuaSkills 判断当前用户、模型、租户或角色是否有权读取或修改值。
4. 不在卸载技能包时删除配置。
5. 不修改 `luaskills-packages` 或其他非必要仓库；只有运行时包资产确实发生变化时才另行立项。
6. 不改变系统技能依赖安装位置、系统技能根优先级或依赖生命周期。
7. 不支持网络文件系统上的强一致并发写入；首版只保证受支持本地文件系统上的进程间一致性。
8. 不保留旧无版本配置文件、旧单键 FFI、旧状态字段或任何自动迁移。

## 四、总体架构

配置系统拆分为六个边界清晰的组件：

1. `ConfigDeclarationCatalog`
   - 从技能根只读扫描技能包清单。
   - 能发现启用、禁用、被遮蔽和无效清单。
   - 只解析声明，不执行技能 Lua。

2. `ConfigStoreRouter`
   - 根据有效技能实例所属层选择普通技能配置文件或系统技能用户级配置文件。
   - 不改变依赖和技能根归属。

3. `ConfigSnapshotStore`
   - 每个配置文件维护一个只读缓存快照。
   - 快照包含严格文档、修订号、内容摘要、加载时间和文件路径。

4. `ConfigTransactionWriter`
   - 使用伴随锁文件完成跨进程原子读改写。
   - 所有单键写入先转换为批量 patch，再进入同一事务。

5. `ConfigReloadWatcher`
   - 监听配置文件父目录。
   - 合并短时间事件，严格解析完整候选文件。
   - 只有合法且修订号单调前进的文件才能替换缓存快照。

6. `SkillPackageConfigService`
   - 统一完成声明解析、静态校验、业务校验、状态计算、值披露、事件发布和 Lua 包隔离。
   - Rust、Lua、C ABI、JSON FFI 和三个 SDK 均通过该服务，不允许绕过。

## 五、唯一技能包配置声明格式

### 5.1 顶层归属

配置声明只允许出现在技能包根目录 `skill.yaml` 顶层：

```yaml
config:
  - key: retry_count
    type: integer
    title: Retry count
    description: Maximum number of retries for one request
    required: false
    default: 3
    group: network
    order: 10
    advanced: false
    restart_required: false
    constraints:
      minimum: 0
      maximum: 10

config_validator: runtime/config-validator.lua
```

包内所有 entries 共享同一份配置。entry 内出现 `config` 或 `config_validator` 必须作为未知字段拒绝。

### 5.2 公共字段

| 字段 | 类型 | 必填 | 规则 |
|---|---|---:|---|
| `key` | string | 是 | 必须符合严格 key 规范，同包唯一 |
| `type` | enum | 是 | `integer`、`string`、`float`、`enum`、`boolean` |
| `description` | string | 是 | 单语言非空说明 |
| `required` | boolean | 否 | 默认 `false` |
| `default` | 类型化标量 | 否 | 与运行时值走同一校验路径；始终属于可披露声明 |
| `sensitive` | boolean | 否 | 默认 `false`；只向宿主表达值处理提示 |
| `constraints` | object | 否 | 只允许当前类型支持的约束 |
| `options` | array | enum 必填 | 枚举稳定值及单语言说明 |
| `title` | string | 否 | 表单短标题 |
| `group` | string | 否 | 宿主分组提示，不参与权限 |
| `order` | integer | 否 | 组内排序，默认按声明顺序 |
| `advanced` | boolean | 否 | 高级选项提示，默认 `false` |
| `placeholder` | string | 否 | 输入占位提示 |
| `example` | 类型化标量 | 否 | 示例值，必须通过类型和静态约束 |
| `format` | enum | 否 | `text`、`password`、`uri`、`path`、`file`、`directory`、`multiline` |
| `restart_required` | boolean | 否 | 修改后是否建议宿主重启或重建相关运行时，默认 `false` |
| `deprecated` | boolean | 否 | 是否已弃用，默认 `false` |
| `deprecation_message` | string | 条件必填 | `deprecated=true` 时必填 |

UI 元数据只作为宿主提示，不能替代类型、范围、长度、枚举和业务校验。`format=password` 不会自动加密、遮罩或授权。

### 5.3 key 规范

配置 key 必须满足：

```text
^[a-z][a-z0-9_.-]{0,127}$
```

补充规则：

1. 使用 ASCII 小写字母开头。
2. 总长度最多 128 字节。
3. 禁止连续点、首尾点、首尾横线和首尾下划线。
4. 保留 `luaskills.` 前缀供核心内部使用，第三方技能不得声明。
5. 同一技能包内大小写不做折叠，因为大写本身非法。
6. 所有 SDK、JSON Schema、Lua API 和帮助文档使用同一规则。

### 5.4 类型、输入和持久化

| 类型 | 公共输入 | 持久化字符串 | 约束 |
|---|---|---|---|
| `integer` | 安全整数 JSON number、Lua number 或十进制 string | 无前导零十进制字符串 | `minimum`、`maximum` |
| `string` | string | 原样 UTF-8 字符串 | `min_length`、`max_length` |
| `float` | 有限 JSON number、Lua number 或十进制 string | 最短可往返十进制字符串，负零保存为 `0` | `minimum`、`maximum` |
| `enum` | string | 精确枚举 `value` | `options` |
| `boolean` | boolean 或严格小写 `true`/`false` string | `true` 或 `false` | 无 |

禁止隐式转换：

1. 不接受 `yes/no`、`1/0` 作为 boolean。
2. 不接受浮点数作为 integer。
3. 不对 string 自动 trim。
4. 不接受 `NaN`、正负无穷。
5. enum 必须精确匹配机器值。

### 5.5 枚举选项

```yaml
options:
  - value: openai
    label: OpenAI
    description: Use the OpenAI-compatible provider
```

`value`、`label`、`description` 都是单语言字符串。LuaSkills 不解析或匹配语言；公共生态建议使用英文，具体技能包可由开发者选择目标用户语言。

### 5.6 默认值与敏感值

1. `default` 是配置结构的一部分，无论 `sensitive` 是否为 `true`，`describe` 都返回默认值。
2. 技能开发者不得把密码、令牌或其他秘密写入默认值。
3. `sensitive=true` 只标记已保存值和有效值需要由宿主谨慎处理。
4. LuaSkills 不自动遮罩、不自动加密、不决定授权。
5. `include_values=false` 时省略已保存值和运行时有效值，但不省略默认值。

## 六、跨语言数值最大公约数

### 6.1 integer

公共 integer 范围固定为 JavaScript 安全整数范围：

```text
-9007199254740991 到 9007199254740991
```

原因是 TypeScript/JavaScript、JSON、Lua number、Python 和 Go 都必须无损表达同一值。Rust 内部即使能处理 `i64`，声明、默认值、输入和约束也不得超出该范围。

### 6.2 float

1. 使用有限 IEEE-754 binary64。
2. 禁止 `NaN`、`Infinity` 和 `-Infinity`。
3. 所有范围比较在 binary64 上完成。
4. 持久化采用最短可往返十进制形式。
5. `-0.0` 规范化为 `0`。

### 6.3 revision

修订号不使用 JSON number，而使用十进制字符串表示无符号 64 位整数，避免各语言整数边界差异：

```json
{
  "revision": "12"
}
```

SDK 对外也把修订号表示为 string。修订号耗尽时拒绝写入，不回绕。

## 七、唯一持久化文件格式

### 7.1 文档结构

唯一合法格式如下：

```json
{
  "format_version": 1,
  "revision": "12",
  "skills": {
    "example-skill": {
      "provider": "openai",
      "retry_count": "3"
    }
  }
}
```

规则：

1. 顶层只允许 `format_version`、`revision`、`skills`。
2. `format_version` 必须严格等于整数 `1`。
3. `revision` 必须是无前导零的十进制字符串；初始空状态为 `"0"`。
4. `skills`、技能包对象和所有值都必须是 JSON object/string。
5. 未知字段、重复 JSON key、错误类型、错误版本和非法 revision 全部拒绝。
6. 写出时按 `skill_id` 和配置 key 排序，使用稳定格式和文件末尾换行。

### 7.2 不兼容策略

以下内容直接删除，不提供读取和迁移：

1. 没有 `format_version` 和 `revision` 的旧文件。
2. 旧状态字段、旧请求字段和旧单键 FFI 形态。
3. 字段别名和宽松未知字段处理。
4. 自动备份后迁移、启动迁移和单独迁移脚本。

如果开发环境仍有旧文件，开发者必须显式删除或按新格式重新创建。错误信息只说明新格式要求，不尝试修复。

### 7.3 路径

宿主新增必需的用户级 `skill_config_root`。配置功能首次使用前该路径必须可解析为绝对路径。

```text
<skill_config_root>/skills/config.json
<skill_config_root>/system-skills/config.json
```

1. 普通用户级、项目级和其他非 `ROOT` 有效技能实例使用 `skills/config.json`。
2. `ROOT` 系统技能使用 `system-skills/config.json`。
3. 两份文件各自拥有独立 revision、锁、缓存和监听器。
4. 系统技能的依赖、资源和安装目录仍保持系统级。
5. 核心不得从当前工作目录、`HOME` 或其他隐式环境猜测用户配置根。
6. 多用户服务必须为每个用户创建独立引擎或显式注入对应用户配置根。
7. 原 `skill_config_file_path` 在本次未发布实现中删除，不保留别名。

### 7.4 卸载

1. 普通卸载和系统卸载都不删除配置文件。
2. 不删除对应 `skill_id` 的配置对象。
3. 不增加 `purge_config` 参数。
4. 上级产品如需清理，必须基于自己的用户、租户、保留期和审计规则显式调用配置删除或管理文件。
5. LuaSkills 文档提供目录和格式说明，但不承担清理生命周期。

## 八、跨进程事务与原子写入

### 8.1 伴随锁文件

每个配置文件使用固定伴随锁文件：

```text
config.json.lock
```

不得锁定 `config.json` 本身，因为原子替换会更换目标文件对象，无法为所有进程提供稳定锁身份。

### 8.2 写事务

每次写入必须按以下顺序执行：

1. 解析目标存储作用域和绝对路径。
2. 打开或创建固定 `.lock` 文件。
3. 使用 Rust 标准文件锁进行跨进程独占锁尝试。
4. 在默认 5 秒、最大 60 秒的可配置超时内重试；超时返回稳定冲突错误。
5. 持锁重新读取磁盘上的最新完整文档，不能直接基于调用前缓存写回。
6. 校验格式版本、修订号、大小和严格 JSON 结构。
7. 可选检查 `expected_revision`；不匹配时返回冲突，不修改文件。
8. 在最新文档上应用同一技能包的批量 patch。
9. 对全部候选值执行静态校验和业务校验。
10. 任一项失败则整个事务回滚，不写临时文件，不更新缓存。
11. revision 加一。
12. 在同目录使用 `create_new` 创建唯一临时文件，名称包含进程号、进程内原子计数和启动随机因子。
13. 写入完整 UTF-8 JSON，刷新用户态缓冲并同步文件。
14. 使用平台原子替换覆盖目标文件。
15. Unix 上同步父目录；Windows 上使用具备替换语义的受支持系统调用。
16. 立即把已提交文档安装为当前进程缓存快照。
17. 发布一个逻辑配置变更事件。
18. 删除本次失败遗留的自有临时文件并释放锁。

### 8.3 并发语义

1. 不传 `expected_revision` 时，批量 patch 会在持锁后合并进磁盘最新版本，因此不会覆盖其他技能包或其他 key 的并发更新。
2. 传入 `expected_revision` 时启用比较并交换语义，适合 UI 的“读取、编辑、保存”流程。
3. 同一事务只允许修改一个技能包，但可以修改该包任意多个已声明 key。
4. 写事务要么全部成功，要么全部不生效。
5. 本地文件系统是首版强一致支持范围；网络共享路径在文档中标记为不受保证。

## 九、缓存快照与文件监听

### 9.1 缓存模型

每份配置文件持有：

```text
ConfigSnapshot {
  document,
  revision,
  content_digest,
  loaded_at,
  file_path
}
```

所有常规 `get/list/describe/validate` 默认只读取不可变快照，不执行磁盘 I/O 和文件锁。

### 9.2 初始化

1. 文件不存在时，内存初始化为 `format_version=1`、`revision="0"`、空 `skills`，但不主动创建文件。
2. 文件存在时必须在引擎配置能力可用前完成严格加载。
3. 初始文件非法时，配置子系统返回明确初始化错误，不使用空配置掩盖。

### 9.3 监听

1. 使用跨平台文件监听库监听两个配置文件的父目录。
2. 过滤目标文件的创建、修改、删除和重命名事件，兼容原子替换。
3. 默认 200 毫秒防抖并合并事件。
4. 每次重载都读取完整候选文件并检查大小、格式、revision 和摘要。
5. 合法且 revision 大于当前值时，原子替换缓存。
6. revision 相同但内容摘要不同、revision 回退、文件删除或文件非法时，保留最后一个合法快照并发布 `reload_failed`。
7. 自己写入后监听到同一 revision 和摘要时只去重，不发布第二个变更事件。
8. 监听线程只更新快照并入队事件，绝不直接调用技能 Lua。
9. 提供显式 `refresh` 管理动作供测试、监听后端恢复和宿主诊断使用。
10. 监听后端不可用时继续提供最后合法快照和本进程写入能力，同时发布明确故障，不静默退化。

## 十、批量设置与删除

### 10.1 唯一内部写入模型

内部只保留：

```text
set_many(package_identity, values, expected_revision?)
```

单键设置必须先转换为只包含一个键的 map，再进入相同事务。不得维护两套写入逻辑。

### 10.2 公共工具 set 多态

统一宿主工具的 `set` 支持两种互斥输入：

```json
{
  "action": "set",
  "skill_id": "example-skill",
  "key": "retry_count",
  "value": 3
}
```

```json
{
  "action": "set",
  "skill_id": "example-skill",
  "values": {
    "retry_count": 3,
    "provider": "openai"
  },
  "expected_revision": "12"
}
```

规则：

1. 必须提供 `values`，或同时提供 `key` 和 `value`。
2. 两种形态不得同时出现。
3. 空 `values` 拒绝。
4. 所有 key 必须属于同一目标技能包并已声明。
5. 响应始终返回规范化后的 map、提交后的 revision 和变更 key。

### 10.3 Lua API

```lua
vulcan.config.set("retry_count", 3)

vulcan.config.set({
    retry_count = 3,
    provider = "openai",
})
```

Lua 实现要求：

1. 第一个参数为 table 时，第二个参数必须不存在。
2. 第一个参数非 table 时必须同时有 key 和 value。
3. table 只能使用 string key 和标量 value。
4. 批量值先全部校验，再一次提交。
5. 两种调用都返回规范化字符串 map。
6. Lua 不接受 `skill_id`，包身份来自 Rust 持有的当前执行上下文。
7. 包 A 调用包 B 时，B 只能修改 B；返回 A 后恢复 A 身份。
8. 未声明 key、跨包尝试和非法值直接失败。

### 10.4 外部 SDK

1. TypeScript 使用 overload：
   - `setSkillConfig(skillId, key, value, options?)`
   - `setSkillConfig(skillId, values, options?)`
2. Python 使用两个明确方法：
   - `set_skill_config(skill_id, key, value, ...)`
   - `set_skill_config_many(skill_id, values, ...)`
3. Go 使用两个明确方法：
   - `SetSkillConfig(...)`
   - `SetSkillConfigMany(...)`
4. 三种 SDK 最终都调用同一个批量 JSON FFI 请求。

删除仍保持单键语义，不把 `null` 当作删除，不在 `set` 中混合删除操作。

## 十一、声明发现、有效实例与禁用包

### 11.1 发现模式

`describe` 支持：

1. `effective`
   - 默认模式。
   - 每个 `skill_id` 只返回按技能根优先级解析后的有效声明实例。
   - 适合模型工具和普通宿主 UI。

2. `installed`
   - 返回每个物理安装实例。
   - 每项包含 `root_name`、绝对路径归属、`enabled`、`shadowed` 和清单状态。
   - 适合宿主管理界面和诊断。

### 11.2 安全边界

1. 发现禁用、被遮蔽或无效清单时只读取 YAML，不执行 Lua。
2. 无效清单返回结构化清单问题，不能伪装成无配置。
3. 写入默认只针对有效解析实例。
4. 宿主私有 API 可用 `root_name` 精确查看安装实例，但模型可见工具默认不暴露物理路径。
5. 禁用包如声明业务校验器，显式配置写入可以运行该专用校验器；这属于宿主已授权的配置事务，不会调用普通技能 entry。

## 十二、跨字段业务校验

### 12.1 声明

技能包可选声明一个专用校验脚本：

```yaml
config_validator: runtime/config-validator.lua
```

脚本返回一个校验函数：

```lua
return function(values)
    if values.provider == "remote" and not values.endpoint then
        return {
            {
                key = "endpoint",
                code = "endpoint_required_for_remote",
                message = "endpoint is required when provider is remote",
            },
        }
    end
    return {}
end
```

### 12.2 执行约束

1. 静态类型和约束校验先执行。
2. 校验器收到的是应用整个批量 patch 后的完整有效配置，默认值已展开，类型已恢复为 integer/string/float/enum/boolean。
3. 校验器在专用受限 Lua 状态中执行，不复用技能业务 VM。
4. 不提供文件、网络、进程、工具调用、技能调用或配置写入能力。
5. 设置指令数、执行时间和内存上限。
6. 校验器只能返回问题，不能修改候选配置。
7. 问题格式固定为 `key?`、`code`、`message`。
8. 技能自定义 code 必须符合 key 风格并由响应加上 `skill.` 命名空间。
9. 校验器加载失败、超时、超限、抛错或返回非法结构都会拒绝整个事务。
10. `validate` 也运行同一业务校验器，但不写文件。

## 十三、状态模型

### 13.1 配置项状态

每个声明项只使用一个 `state`：

| state | 含义 | satisfied |
|---|---|---:|
| `unset` | 可选项无显式值且无默认值 | `true` |
| `missing` | 必填项无显式值且无默认值 | `false` |
| `default` | 使用合法默认值 | `true` |
| `configured` | 使用合法显式持久化值 | `true` |
| `invalid` | 已保存值不满足当前声明或业务规则 | `false` |

不再返回可互相冲突的 `valid`、`configured` 和 `source` 组合。orphaned 值在独立 `orphaned` 列表中表达。

### 13.2 包状态

包级响应包含：

```text
complete
revision
store_scope
missing_count
invalid_count
orphaned_count
business_issue_count
items
orphaned
```

`complete=true` 的条件是所有声明项 `satisfied=true` 且没有业务校验问题。orphaned 不使包不完整，但必须计数和报告。

## 十四、统一宿主工具契约

### 14.1 唯一动作集合

统一工具名建议为 `runtime-config`，机器协议 action 为：

| action | 主要参数 | 行为 |
|---|---|---|
| `describe` | `skill_id?`、`mode?`、`root_name?`、`include_values?` | 查询声明、UI 元数据、类型、约束和状态 |
| `validate` | `skill_id` | 对当前有效技能包执行静态和业务只读校验 |
| `list` | `skill_id?` | 查询当前有效技能包的原始持久化记录 |
| `get` | `skill_id`、`key` | 查询当前有效技能包的一个原始持久化值 |
| `set` | 单键或 `values`、`expected_revision?` | 原子设置一个技能包 |
| `delete` | `skill_id`、`key`、`expected_revision?` | 删除一个显式值或 orphaned 值 |
| `refresh` | `store_scope?` | 显式重新读取磁盘并返回结果 |

所有请求严格拒绝未知字段。所有响应使用稳定 envelope：

```json
{
  "ok": true,
  "action": "set",
  "result": {},
  "error": null
}
```

失败时：

```json
{
  "ok": false,
  "action": "set",
  "result": null,
  "error": {
    "code": "CONFIG_REVISION_CONFLICT",
    "message": "Configuration revision does not match",
    "details": {}
  }
}
```

### 14.2 暴露给模型

宿主要把该工具暴露给模型时：

1. 注册一个工具，不把每个 action 拆成多个模型工具。
2. 工具 schema 可完整展示 action 和参数结构。
3. 默认允许 `describe(include_values=false)` 和 `validate`。
4. 宿主自行决定是否向当前模型开放 `get/list/include_values=true/set/delete/refresh`。
5. 宿主可以直接拒绝、强制覆写参数、移除参数、请求用户确认或在确认后重放调用。
6. LuaSkills 不读取“用户已授权”标记，也不伪造授权结论。
7. 宿主必须避免把敏感值写入普通日志、遥测或无关模型上下文。
8. 物理绝对路径和 `installed` 诊断模式默认不向模型暴露。

### 14.3 不暴露给模型

宿主不注册模型工具时：

1. UI、CLI、TUI 或后台服务直接调用相同 dispatcher、Rust API 或语言 SDK。
2. 请求、响应、校验和错误码与模型模式完全相同。
3. 权限由宿主自己的用户、租户、角色和交互层处理。
4. 不创建第二套“内部配置协议”。

### 14.4 技能缺配置帮助

技能在 `status.complete=false` 时应：

1. 列出缺失 key，不输出敏感值。
2. 建议 AI 或宿主先调用 `describe` 获取类型、约束和说明。
3. 如果宿主开放配置工具，建议使用 `set`，多个值一次批量提交。
4. 如果宿主未开放修改能力，明确要求用户提供哪些参数，或引导用户在产品配置界面完成。
5. 提示语言由技能开发者决定；公共生态建议使用英文。

## 十五、安全上限与截断

### 15.1 硬上限

| 对象 | 上限 |
|---|---:|
| 单包声明配置项 | 1024 |
| 配置 key | 128 ASCII 字节 |
| `description`、`deprecation_message` | 16384 UTF-8 字节 |
| `title` | 1024 UTF-8 字节 |
| `group` | 256 UTF-8 字节 |
| `placeholder`、`example` 展示文本 | 8192 UTF-8 字节 |
| enum 选项数 | 1024 |
| enum `value`、`label` | 各 1024 UTF-8 字节 |
| enum `description` | 16384 UTF-8 字节 |
| 单个持久化值 | 1 MiB UTF-8 字节 |
| string 声明 `max_length` | 1048576 个 Unicode 标量 |
| 单次批量 key 数 | 1024 |
| 单次批量请求编码后大小 | 16 MiB |
| 单配置文件技能包数 | 10000 |
| 单配置文件大小 | 64 MiB |
| 单个工具响应 | 64 MiB |
| 错误、日志、事件中的非敏感值预览 | 4096 UTF-8 字节 |

### 15.2 处理原则

1. 配置值、key、声明和配置文件超限时直接拒绝，绝不静默截断后保存。
2. 只有诊断、日志和 UI 预览允许截断。
3. 截断必须在合法 UTF-8 边界完成，并追加明确的 `[truncated]` 标记。
4. `sensitive=true` 的值不生成预览，不能先截断再输出。
5. 约束中的 `max_length` 可以不填写，但仍受系统硬上限约束。

## 十六、配置事件

### 16.1 事件结构

```json
{
  "type": "skill_config_changed",
  "store_scope": "system-skills",
  "skill_id": "example-skill",
  "revision": "13",
  "changed_keys": ["provider", "retry_count"],
  "source": "local_write",
  "restart_required_keys": [],
  "complete": true
}
```

重载失败事件：

```json
{
  "type": "skill_config_reload_failed",
  "store_scope": "skills",
  "revision": "12",
  "source": "external_reload",
  "error": {
    "code": "CONFIG_REVISION_REGRESSION",
    "message": "External configuration revision moved backwards"
  }
}
```

### 16.2 语义

1. `source` 只允许 `local_write` 或 `external_reload`。
2. 本进程批量事务只产生一个事件。
3. 外部重载通过新旧快照比较产生每包一个变更事件。
4. 短时间重复文件事件合并。
5. 监听线程只向现有引擎事件队列入队。
6. SDK 提供轮询、等待和语言惯用回调封装。
7. `restart_required_keys` 只提供提示，LuaSkills 不自动重启宿主或任意技能。

## 十七、稳定错误码

至少固定以下错误码，并生成到三个 SDK：

```text
CONFIG_ATOMIC_REPLACE_FAILED
CONFIG_BATCH_ARGUMENT_CONFLICT
CONFIG_BATCH_EMPTY
CONFIG_BATCH_TOO_LARGE
CONFIG_DECLARATION_INVALID
CONFIG_ENUM_VALUE_INVALID
CONFIG_EVENT_CURSOR_EXPIRED
CONFIG_EVENT_CURSOR_INVALID
CONFIG_FILE_TOO_LARGE
CONFIG_FORMAT_INVALID
CONFIG_FORMAT_VERSION_UNSUPPORTED
CONFIG_KEY_INVALID
CONFIG_KEY_UNDECLARED
CONFIG_LOCK_FAILED
CONFIG_LOCK_TIMEOUT
CONFIG_PACKAGE_NOT_FOUND
CONFIG_PATH_INVALID
CONFIG_PATH_UNAVAILABLE
CONFIG_RELOAD_FAILED
CONFIG_RESPONSE_TOO_LARGE
CONFIG_REVISION_CONFLICT
CONFIG_REVISION_EXHAUSTED
CONFIG_REVISION_INVALID
CONFIG_REVISION_REGRESSION
CONFIG_SNAPSHOT_UNAVAILABLE
CONFIG_VALIDATOR_FAILED
CONFIG_VALIDATOR_LIMIT_EXCEEDED
CONFIG_VALIDATOR_TIMEOUT
CONFIG_VALIDATOR_UNAVAILABLE
CONFIG_VALUE_OUT_OF_RANGE
CONFIG_VALUE_TOO_LONG
CONFIG_VALUE_TYPE_INVALID
CONFIG_WATCHER_FAILED
```

错误消息用于人类阅读，程序只能依赖 error code 和结构化 details。错误详情不得包含敏感原值。

## 十八、公共契约生成

### 18.1 核心为唯一事实源

Rust 核心定义：

1. 工具请求和响应 DTO。
2. 配置声明 DTO。
3. 配置文档 DTO。
4. 状态、事件、错误码和枚举。
5. 所有硬上限常量。

核心使用已有 `serde`、`serde_json` 和确定性生成器输出一份规范化公共契约：

```text
contracts/skill-config/v1/contract.json
```

该契约包含协议版本、声明类型、渲染格式、值状态、发现模式、存储作用域、错误码、硬上限、存储约定与工具动作。Rust 的强类型 DTO 与真实动态库端到端测试负责保证请求和响应字段一致性，避免同时维护多份容易分叉的 JSON Schema。生成器输出必须稳定排序，不为生成契约引入非必要运行时依赖。

### 18.2 SDK 生成

1. TypeScript 从公共契约生成类型、状态、发现模式、存储作用域、错误码和上限常量。
2. Python 从公共契约生成对应的 `Literal` 类型、元组常量和上限常量。
3. Go 从公共契约生成强类型字符串、常量切片和上限常量。
4. 语言惯用的高层客户端与 wire DTO 手写，使用生成类型约束枚举字段，并由真实动态库端到端测试验证字段和行为。
5. 三个 SDK 的生成器检查模式逐字节拒绝过期生成结果。
6. 三个 SDK 保存完全一致的 `skill-config/v1` 契约副本，并在发布验证中逐字节比较。

## 十九、核心接口调整

### 19.1 Rust Host Options

1. 删除 `skill_config_file_path`。
2. 新增显式 `skill_config_root`。
3. 新增锁超时和监听防抖配置，均有安全默认值和上限。
4. 路径在引擎初始化时规范化并保存，不在每次调用重新猜测。

### 19.2 Rust API

核心 API 至少提供：

```text
describe_skill_package_config
validate_skill_package_config
list_skill_config_entries
get_skill_config_value
set_skill_config_values
delete_skill_config_value
refresh_skill_config
```

删除旧单值写入作为底层实现；如 Rust 高层保留单键便利方法，也只能构造单元素 map 调用 `set_skill_config_values`。

### 19.3 C ABI 与 JSON FFI

1. 标准 C ABI 的写入使用一个批量 JSON values 参数，删除旧单键写函数。
2. JSON FFI 的 `set` 请求改为唯一批量 wire 请求。
3. 工具多态只存在统一 dispatcher 层，进入 FFI 前已经规范化成 `values`。
4. 所有新结构和函数同步更新两个头文件。
5. ABI 测试编译 C 头文件并链接真实动态库。
6. 不保留旧 symbol 作为兼容入口。

## 二十、三个 SDK 调整

### 20.1 TypeScript

1. 增加生成 wire 类型和手写 overload。
2. integer 输入限制为 `Number.isSafeInteger`。
3. revision 全程使用 string。
4. 支持 describe 模式、批量 set、refresh 和配置事件。
5. 使用真实动态库完成端到端配置测试。
6. 版本、锁文件、`VERSION`、运行时默认核心 tag 和文档统一改为 `0.5.5` / `v0.5.5`。

### 20.2 Python

1. 增加生成 wire 类型。
2. Python 大整数在出站前强制检查公共安全整数范围。
3. 提供单键和批量两个方法，共用同一 FFI。
4. 支持 describe 模式、refresh 和配置事件。
5. 使用真实动态库完成端到端配置测试。
6. `VERSION`、`pyproject.toml`、运行时默认核心 tag 和文档统一升级。

### 20.3 Go

1. 增加生成 wire struct、typed enum 和限制常量。
2. 即使 Go 使用 `int64`，也必须检查公共安全整数范围。
3. 提供单键和批量两个方法，共用同一 FFI。
4. 支持 describe 模式、refresh 和配置事件。
5. 增加 `CGO_ENABLED=1` 的真实动态库端到端测试，同时保留 `CGO_ENABLED=0` 纯 Go 检查。
6. `VERSION`、运行时默认核心 tag 和文档统一升级；发布使用 `v0.5.5` 模块标签。

## 二十一、帮助文档交付

必须更新或新增：

1. 技能开发手册
   - 完整声明字段、类型、key 规范、UI 元数据和硬上限。
   - 单语言规则和公共生态建议英文。
   - 缺配置时如何告诉 AI 调用 `runtime-config`，或告诉用户应提供哪些参数。
   - 批量 set 和业务校验器示例。

2. 宿主对接手册
   - 模型可见和宿主私有两种接法。
   - `include_values`、敏感值、授权和强制改参责任。
   - 用户级配置根和系统技能专用目录。
   - 锁、revision、CAS、监听和事件处理。

3. 配置存储规范
   - 唯一 JSON 格式。
   - 严格版本规则。
   - 本地文件系统范围。
   - 外部编辑必须递增 revision。
   - 旧格式不兼容。

4. 公共契约文档
   - 数值最大公约数。
   - wire 输入类型。
   - 错误码、状态和限制。
   - SDK 一致性要求。

5. 示例技能包
   - 五种类型。
   - UI 元数据。
   - 单键和批量设置。
   - 跨字段校验。
   - 缺配置帮助。
   - 配置变化事件。

不恢复任何配置 i18n 文档或语言标记列表。

## 二十二、预计文件变更

### 22.1 LuaSkills core

重点修改：

```text
Cargo.toml
Cargo.lock
src/skill/config.rs
src/skill/manifest.rs
src/runtime/config.rs
src/runtime/config_service.rs
src/runtime/engine.rs
src/runtime/engine/runlua.rs
src/host/options.rs
src/ffi.rs
src/ffi/requests.rs
src/ffi_standard.rs
src/lib.rs
include/luaskills_ffi.h
include/luaskills_json_ffi.h
examples/skill-package-config/**
README.md
docs/**
```

预计新增：

```text
src/runtime/config_contract.rs
src/runtime/config_watcher.rs
src/runtime/config_validator.rs
schemas/skill-config/v1/**
scripts/generate_skill_config_contracts.*
tests/fixtures/skill-config-contract/**
```

只在文件监听确实需要时增加跨平台监听依赖；文件锁、摘要、序列化和生成优先复用 Rust 标准库及现有依赖。

### 22.2 SDK

三个 SDK 分别修改：

1. FFI 声明与调用封装。
2. 生成 wire 类型和生成脚本。
3. 高层配置 API。
4. 真实动态库测试和共享 fixture。
5. README、中文 README、版本文件、运行时资产默认版本。
6. TypeScript 必要的 `package.json`、`package-lock.json` 版本字段。
7. Python 必要的 `pyproject.toml` 版本字段。
8. Go 必要的 `VERSION` 和发布标签。

不修改与本能力无关的依赖版本、package 元数据、运行时包版本或示例发布结构。

## 二十三、分阶段执行步骤

### 第一阶段：冻结协议与 schema

1. 把本方案中的声明、存储、工具、状态、事件、错误和限制落实为 Rust DTO。
2. 建立确定性 schema 生成器。
3. 生成 `skill-config/v1` 契约。
4. 增加 schema 快照测试。
5. 删除旧兼容 DTO、旧请求和旧存储格式。

验收：

- 旧无版本文件必定失败。
- 未知字段必定失败。
- 核心生成文件重复执行无差异。

### 第二阶段：存储路由、跨进程锁和批量事务

1. 用 `skill_config_root` 替换旧单文件选项。
2. 实现普通技能和系统技能双存储路由。
3. 实现伴随锁文件、超时、持锁重读、唯一临时文件和原子替换。
4. 实现 revision 和可选 CAS。
5. 把所有写入统一到批量事务。
6. 增加多进程压力测试。

验收：

- 多进程同时更新不同 key 不丢失。
- 同 revision CAS 只允许一个提交成功。
- 事务中任一值非法时文件、revision 和缓存都不变。

### 第三阶段：缓存和监听

1. 建立双存储快照。
2. 所有读接口改读快照。
3. 添加父目录监听、防抖、严格重载和最后合法快照策略。
4. 实现显式 refresh。
5. 接入事件队列。

验收：

- 外部合法更新自动可见。
- 外部非法更新不污染缓存。
- 自写只产生一个逻辑事件。
- 文件删除和 revision 回退产生明确失败事件。

### 第四阶段：声明、状态和业务校验

1. 收紧 key 规则和数值范围。
2. 增加 UI 元数据。
3. 增加统一硬上限。
4. 替换状态模型。
5. 增加受限业务校验器。
6. 建立禁用和已安装包只读声明目录。

验收：

- 五类配置、默认值、UI 元数据和枚举都严格校验。
- 禁用包 describe 不执行 Lua。
- 业务校验器不能访问文件、网络、工具、技能或配置写入。

### 第五阶段：Lua、Rust、C ABI、JSON FFI

1. 实现 Lua `set(key, value)` / `set(table)` 多态。
2. 落实 Rust 批量 API。
3. 更新 C ABI 和 JSON FFI。
4. 更新头文件和导出。
5. 删除旧 symbol 和兼容代码。

验收：

- 所有入口都进入同一 service 和同一批量事务。
- Lua 不能跨包。
- C 头文件可编译并链接真实库。

### 第六阶段：统一工具和帮助

1. 实现统一 dispatcher。
2. 落实模型可见和宿主私有说明。
3. 更新技能缺配置帮助模板。
4. 更新架构、开发、FFI 和存储文档。
5. 更新示例技能。

验收：

- `describe` 能返回配置列表、类型、说明、约束、默认值和状态。
- `include_values=false` 完全省略持久化值。
- 默认值不因 `sensitive` 被省略。

### 第七阶段：三个 SDK

1. 同步生成契约。
2. 分别实现语言惯用批量 API。
3. 同步事件、错误、状态和 revision。
4. 增加真实动态库端到端测试。
5. 更新文档和示例。

验收：

- 三个 SDK 对同一 fixture 产生一致请求和响应。
- TypeScript 拒绝非安全整数。
- Python 和 Go 即使能表达更大整数也拒绝越界。

### 第八阶段：版本、全量验证和五轮审核

1. 核心和三个 SDK 从 `0.5.4` 升级为 `0.5.5`。
2. 只修改必要版本字段和核心 tag 默认值。
3. 运行各仓库格式化、静态检查、单元测试、集成测试、端到端测试、打包检查。
4. 执行代码审核。
5. 发现任一问题后立即修复、补回归测试，并把连续无问题计数归零。
6. 直到连续五轮审核没有发现任何问题。
7. 对照本计划逐项验收。
8. 在本文件末尾追加执行变更总结。
9. 将计划迁移为：

```text
docs/completed/20260723/03-SKILL_CONFIG_PRODUCTION_HARDENING.md
```

## 二十四、测试矩阵

### 24.1 声明

- 五种类型及合法边界。
- 所有 UI 元数据。
- 严格未知字段。
- key 正则、保留前缀、重复 key。
- 默认值、示例值和 enum 选项。
- 数量和文本硬上限。
- 不允许任何 i18n 字段。

### 24.2 数值

- 安全整数上下边界成功。
- 边界外一位失败。
- TypeScript、Lua、Python、Go 与 Rust 结果一致。
- 浮点有限值、负零、指数形式、边界和非有限值。

### 24.3 存储

- 初始无文件。
- 唯一 v1 格式。
- 旧格式拒绝。
- 未知字段、重复 key、错误 revision 和超大文件拒绝。
- 稳定排序和末尾换行。
- 系统技能与普通技能写入不同用户级文件。

### 24.4 并发

- 同进程多线程。
- 多进程同 key。
- 多进程不同 key。
- 多进程不同技能包。
- 多进程不同存储作用域。
- 锁超时。
- CAS 冲突。
- 写入中断和原子替换失败。
- Windows、Linux、macOS 至少由 CI 各运行一组。

### 24.5 缓存和监听

- 首次加载。
- 自写同步缓存。
- 外部合法替换。
- 原子 rename 事件。
- 防抖合并。
- 相同 revision 同摘要去重。
- 相同 revision 不同摘要失败。
- revision 回退。
- 删除文件。
- 非法 JSON 和超大文件。
- 监听器后端错误。
- refresh 恢复。

### 24.6 批量事务

- 单键转批量。
- 多键全部成功。
- 中间项失败全部回滚。
- 空 map。
- 超大 map。
- `key/value` 与 `values` 冲突。
- 规范化返回值。
- 单次 revision 增加和单次事件。

### 24.7 包隔离

- 同包多 entry 共享。
- 嵌套调用切换和恢复。
- Lua 伪造内部字段不能跨包。
- 禁用包 describe 不执行 Lua。
- 被遮蔽包 installed 诊断。
- 普通技能不能写系统技能配置。

### 24.8 业务校验

- 合法跨字段组合。
- 多问题返回。
- 自定义 code 命名空间。
- 脚本错误、超时、内存和指令超限。
- 非法返回结构。
- 无文件、网络、工具、技能和配置写入能力。

### 24.9 安全披露

- `include_values=false` 不含持久化值。
- 默认值始终存在。
- sensitive 值不进入错误、事件预览和普通日志。
- 非敏感超长预览按 UTF-8 安全截断。
- 宿主参数强改和授权流程不会被核心绕过。

### 24.10 SDK 真实端到端

每个 SDK 都必须：

1. 加载本次构建的真实动态库。
2. 创建独立临时用户配置根。
3. 加载真实示例技能包。
4. describe 五种类型和 UI 元数据。
5. 执行单键和批量 set。
6. 验证规范化、revision、状态和事件。
7. 外部替换配置文件并等待监听事件。
8. 验证系统技能用户级路由。
9. 验证非法批量回滚。
10. 验证旧格式拒绝。

## 二十五、验证命令基线

实际执行前应先读取各仓库现有脚本，以仓库真实命令为准，不臆造脚本。至少覆盖：

### 25.1 核心

```text
rtk cargo fmt --check
rtk cargo check --all-targets
rtk cargo test --all-targets
rtk cargo clippy --all-targets -- -D warnings
rtk cargo package
```

### 25.2 TypeScript SDK

```text
rtk npm install
rtk npm run check
rtk npm run build
rtk npm pack --dry-run
```

### 25.3 Python SDK

```text
rtk python -m pytest
rtk python -m build
rtk twine check dist/*
```

### 25.4 Go SDK

```text
rtk go test ./...
```

另行在已配置 cgo 编译器的环境中执行真实 FFI 测试。

## 二十六、连续五轮代码审核规则

每轮审核都检查完整变更，不只检查上一轮修复：

1. 第一轮：协议、数据模型、包作用域、系统技能路由和兼容代码删除。
2. 第二轮：跨进程锁、原子性、revision、缓存、监听和故障恢复。
3. 第三轮：安全边界、敏感值、业务校验沙箱、硬上限和错误泄露。
4. 第四轮：C ABI、JSON FFI、生成契约和三个 SDK 一致性。
5. 第五轮：测试完整性、文档、版本、打包和发布准备。

若任何一轮发现问题：

1. 立即修复。
2. 增加能够复现该问题的测试。
3. 运行受影响测试和全量验证。
4. 将“连续无问题轮数”归零。
5. 从新的第一轮重新审核，直到连续五轮均无问题。

## 二十七、Git、标签和发布

### 27.1 提交规则

1. 每个仓库独立检查工作区，保留用户已有修改，不 reset、不覆盖。
2. 提交信息严格使用中文。
3. 提交采用“中文首行 + 空行 + 多行列表”。
4. 核心和三个 SDK 分别提交。
5. 提交前确认没有混入无关 package、依赖或运行时资产变更。

### 27.2 版本

| 仓库 | 旧版本 | 新版本 | 标签 |
|---|---:|---:|---|
| LuaSkills core | 0.5.4 | 0.5.5 | `v0.5.5` |
| TypeScript SDK | 0.5.4 | 0.5.5 | `v0.5.5` |
| Python SDK | 0.5.4 | 0.5.5 | `v0.5.5` |
| Go SDK | 0.5.4 | 0.5.5 | `v0.5.5` |

### 27.3 发布顺序

本次不修改 `luaskills-packages`。在确认现有 Lua runtime/deps 资产与 `0.5.5` 仍兼容后，顺序为：

1. 提交并推送 LuaSkills core。
2. 创建并推送 core `v0.5.5` 标签。
3. 构建并发布 core crate、FFI SDK 资产和主仓库 release 资产。
4. 提交并推送 TypeScript SDK，创建并推送 `v0.5.5`，发布 `@luaskills/sdk@0.5.5`。
5. 提交并推送 Python SDK，创建并推送 `v0.5.5`，发布 `luaskills-sdk==0.5.5`。
6. 提交并推送 Go SDK，创建并推送 `v0.5.5`，等待 Go module proxy 可解析。
7. 按各 SDK 现有文档触发 Examples Release 工作流。
8. 从全新临时目录分别安装 npm、PyPI 和 Go module 发布物做发布后冒烟测试。

发布前置检查：

1. 远端不存在同名标签和不可覆盖版本。
2. 当前分支已推送且标签指向已验证提交。
3. npm、PyPI、crates.io 和 GitHub 发布凭据可用。
4. core release 资产在 SDK 发布前可下载。
5. 三个 SDK 的默认 core tag 都已经指向 `v0.5.5`。

若外部注册表鉴权、远端保护规则或跨平台 release 资产阻塞，停止发布阶段并报告明确阻塞；不得伪造发布成功。

## 二十八、最终验收标准

### 28.1 功能

- 配置对象始终是技能包，不是独立 entry。
- 不同技能包可以声明不同配置。
- Lua 单键和 table 批量设置均可用且原子。
- Lua 不能跨包读写。
- 宿主可获取配置列表、说明、类型、约束、默认值、状态和值。
- 宿主可选择模型暴露或私有使用同一工具协议。
- 禁用和已安装包可以只读发现。
- 系统技能配置写入用户级专用目录。
- 外部合法修改自动重载。
- 配置变化通过事件通知。

### 28.2 一致性

- 核心、Lua、C ABI、JSON FFI 和三个 SDK 使用同一类型、状态、错误码、限制和 revision 语义。
- 所有写入进入同一批量事务。
- 所有读取默认来自最后合法快照。
- 数值在全部语言中无损。

### 28.3 安全

- 敏感值不自动披露或进入诊断。
- 默认值不因 sensitive 被隐藏。
- 权限和用户授权明确由上级宿主负责。
- 超限业务数据拒绝，诊断安全截断。
- 业务校验器不能访问宿主能力。

### 28.4 工程

- 旧格式、旧 API、兼容脚本和字段别名全部删除。
- 全量格式化、静态检查、测试、打包检查通过。
- 三个 SDK 真实动态库端到端测试通过。
- 连续五轮代码审核无问题。
- 计划文件补齐执行变更总结并迁移到 completed。
- 四个仓库分别完成中文结构化提交、推送和标签。
- core、npm、PyPI、Go module 及 SDK 示例发布验证成功。

## 二十九、执行前需要确认的重大决策

本方案涉及存储格式、宿主选项、公共 FFI、系统技能配置路径、跨进程事务、文件监听、受限 Lua 校验器和三个 SDK wire contract 的破坏式调整，属于核心架构变更。

执行前确认点只有一个：

> 是否批准以本文件为唯一实施依据，直接删除当前未发布配置实现的兼容形态，并按 `0.5.5` 完成核心与三个 SDK 的实现、五轮审核、提交、推送、打标和发布。

批准后严格按本计划推进；未批准前只保留本方案文件，不修改实现。

## 三十、执行变更总结

### 30.1 核心修复与调整概述

1. 完成唯一受支持的技能包级配置声明与严格解析，配置对象始终归属于技能包，不归属于独立技能入口。
2. 完成整数、字符串、浮点、枚举、布尔五类声明及约束、默认值、示例值和单语言 UI 元数据；未引入 i18n 字段或旧格式兼容层。
3. 完成普通技能与系统技能的用户级配置路由、严格 v1 存储格式、十进制字符串 revision、SHA-256 快照和 CAS 冲突检查。
4. 完成跨进程伴随锁文件、原子替换、父目录持久化、提交后快照一致性、缓存读取、配置文件监听和变更事件发布。
5. 完成包内 Lua 配置隔离、单键与 table 批量多态设置、受限 Lua 业务校验器及跨包读写禁止。
6. 完成 Rust、Lua、标准 C ABI、JSON FFI 与统一宿主工具协议；值是否向模型或调用方披露继续由上级宿主通过显式参数和授权流程决定。
7. 完成公共契约生成与四仓库逐字节同步，契约 SHA-256 为 `7cfbfe8632ecd025e263bce27c75da67e6cb136634a7e3ed4fdcd090bc478123`。
8. 完成 TypeScript、Python、Go 三个 SDK 的类型、客户端、命令行或语言接口、契约生成、文档和真实动态库端到端测试。
9. 核心与三个 SDK 全部升级至 `0.5.5`，未修改 `luaskills-packages`，未增加与本目标无关的依赖或包配置。
10. 完成五轮连续零问题审核；审核覆盖协议与作用域、存储与并发、安全与上限、FFI 与 SDK 一致性、打包与发布准备。

### 30.2 📂文件变更清单

#### 新增

- 核心公共配置契约、契约生成器和运行时配置工具模块。
- 技能包配置校验器示例及 `0.5.5` 升级说明。
- TypeScript、Python、Go SDK 的公共契约副本、契约生成器、配置类型和配置端到端测试。
- 本执行完成记录。

#### 修改

- 核心清单模型、配置存储、配置服务、运行时引擎、宿主选项、FFI、公共头文件、示例、同步脚本和中英文文档。
- TypeScript SDK 客户端、类型、命令行、运行时资产、测试、同步脚本、版本和中英文文档。
- Python SDK 客户端、类型、命令行、运行时资产、测试、打包清单、同步脚本、版本和中英文文档。
- Go SDK 客户端、FFI、配置接口、运行时资产、测试、同步脚本、版本和中英文文档。

#### 删除

- 删除未发布旧配置实现中的兼容字段、兼容调用形态和迁移思路；未保留兼容脚本。
- 未删除任何持久化技能包配置文件，卸载后的配置清理由上级宿主按目录规则负责。

### 30.3 💻关键代码调整详情

1. `SkillConfigStore` 使用严格文档、单调 revision、伴随锁文件、原子临时文件替换和提交后快照安装，保证多进程读改写不会丢失更新。
2. `SkillPackageConfigService` 统一声明发现、值校验、业务校验、状态计算、事件排序和普通/系统技能存储路由。
3. 文件监听只监控配置文件所在目录，合法且前进的外部变更替换缓存；非法、回退或同 revision 不同摘要的内容不会污染最后合法快照。
4. Lua `config.set` 直接判断参数是否为 table，将单键和批量写入统一到同一原子事务，并根据当前执行包绑定作用域。
5. 公共工具支持声明查询、值查询、批量设置、删除和刷新；`include_values` 是显式披露参数，核心不内置最终用户权限策略。
6. 配置值、文件、响应和诊断文本均设置硬上限；诊断预览采用 UTF-8 安全截断，工具响应使用流式计数写入避免二次超大分配。
7. `durability_error` 仅作为内部提交结果传递，确保已提交快照和 `local_write` 事件先落地，再向调用方报告父目录持久化异常，不改变公共 JSON/FFI 结构。
8. 四仓库从同一 `contract.json` 生成状态、枚举、错误码和限制常量，消除跨语言手工协议漂移。

### 30.4 验证、审核与发布结果

1. 核心 `cargo test --all-targets` 最终通过 644 个测试，零失败；`cargo fmt --check`、`cargo check --all-targets`、`cargo clippy --all-targets -- -D warnings` 和 `cargo package` 全部通过。
2. TypeScript `npm run check`、构建、打包检查和真实动态库端到端测试通过。
3. Python 56 个测试、构建、Twine 检查和真实动态库端到端测试通过。
4. Go 57 个测试、`go vet`、Zig cgo 真实动态库端到端测试和模块解析通过。
5. 连续五轮审核均未发现问题；任何前序发现均已修复、补充回归测试并重新开始五轮计数。
6. 核心提交 `604228e9af9a71b5fc7a553acb18e6db16768ada` 已推送并标记 `v0.5.5`；`luaskills 0.5.5` 已发布到 crates.io。
7. TypeScript 提交 `df906a7` 已推送并标记 `v0.5.5`；`@luaskills/sdk@0.5.5` 已发布到 npm。
8. Python 提交 `6e4b139` 已推送并标记 `v0.5.5`；`luaskills-sdk==0.5.5` 已发布到 PyPI。
9. Go 提交 `0636186` 已推送并标记 `v0.5.5`；Go module proxy 已能解析 `github.com/LuaSkills/luaskills-sdk-go@v0.5.5`。
10. 核心资产工作流 `30030562567` 成功，GitHub Release 包含五个平台共 40 个发布资产。
11. TypeScript、Python、Go 示例工作流重跑 `30030974732`、`30030974805`、`30030974965` 均成功，并生成 `examples-v0.5.5` Release。

### 30.5 ⚠️遗留问题与注意事项

1. Windows 链接阶段仍会输出既有的 `LNK4098` 默认库冲突警告，但不影响构建、测试、打包或发布；本任务未引入新的链接失败。
2. GitHub Actions 提示 `softprops/action-gh-release@v2` 的 Node.js 20 运行时已弃用并由平台强制使用 Node.js 24；当前工作流成功，后续应在独立维护任务中跟踪上游 action 更新。
3. 首次触发三个 SDK 示例工作流时，核心 Release 资产矩阵尚未完成，下载 `v0.5.5` 资产返回 404；核心资产完成后重跑均成功，未对代码加入发布顺序兼容或重试兜底。
4. 配置文件保留策略保持不变：技能卸载不删除用户配置；需要清理时由上级宿主根据公开目录规则实施。
