# Skill Root 层级与管理边界

## 1. 目标

LuaSkills 对外正式暴露三个逻辑层级：

```text
ROOT -> PROJECT -> USER
```

这里的箭头表示加载优先级从高到低。`ROOT` 是系统控制级，`PROJECT` 是当前项目级，`USER` 是用户全局级。

## 2. 三层定义

| 层级 | 控制方 | 是否建议普通用户直接操作 | 典型用途 |
| --- | --- | --- | --- |
| `ROOT` | 宿主 system tools | 否 | 核心工具、内置 skill、宿主托管能力 |
| `PROJECT` | 宿主授权的项目管理面 | 是 | 当前项目专属 skill、项目覆盖 |
| `USER` | 宿主授权的用户管理面 | 是 | 用户全局安装、个人偏好 skill |

对外产品语义应固定为这三个标签。即使底层 FFI 仍使用 `RuntimeSkillRoot[]` 承载路径，正式宿主也不应把任意 root 链暴露成用户可选择的层级模型。

运行时启动或加载时必须传入 `ROOT` root。`ROOT` 可以暂时没有任何已安装 skill，但 root 链里不能缺失该层；缺失时应直接报错，而不是把 system 操作回退到 `PROJECT` 或 `USER`。

## 3. 覆盖与加载规则

加载顺序固定为：

```text
ROOT -> PROJECT -> USER
```

同名判断以目录派生出的 `skill_id` 为准。

规则如下：

1. `ROOT` 中存在某个 `skill_id` 时，该 `skill_id` 由系统层全局占用，`PROJECT` 和 `USER` 中的同名 skill 不应被加载。
2. `PROJECT` 中存在某个 `skill_id` 且 `ROOT` 中不存在同名 skill 时，`PROJECT` 覆盖 `USER`。
3. `USER` 是最低优先级用户可写层。
4. `ROOT` 级 skill 一旦通过 system tools 安装，就表示宿主声明该 skill 属于系统控制级，低层同名实现必须让位。
5. 无论调用方是 `System` 还是 `DelegatedTool`，只要 `ROOT` 已存在同名 `skill_id`，都禁止向 `PROJECT` / `USER` 主动 install 或 update 同名 skill。
6. `PROJECT` / `USER` 中已经存在的同名残留允许通过显式目标 uninstall 清理；清理残留不会影响 `ROOT` 中的系统 skill。

LuaSkills 不再使用 `protected_skills` 或按 `skill_id` 配置的默认保护清单。保护不是配置出来的，而是由 `ROOT` 层级归属天然决定。

这套模型故意不支持多层 `PROJECT_A / PROJECT_B / ORG / WORKSPACE` 的用户可见层级。若宿主内部确实有更复杂的组织结构，应在宿主侧折叠成单个对外 `PROJECT` 标签。

## 4. 普通 skills 管理面

`vulcan.runtime.skills.*` 是面向 skill 的普通运行时管理桥接。它只能请求宿主操作 `PROJECT` / `USER` 层级。

普通桥接不允许：

1. 安装 `ROOT` 级 skill。
2. 更新 `ROOT` 级 skill。
3. 卸载 `ROOT` 级 skill。
4. 启用或停用 `ROOT` 级 skill。
5. 暴露把目标层级设为 `ROOT` 的选项。

如果宿主允许普通桥接指定目标层级，目标层级只能来自宿主声明的可操作层级列表。未指定目标层级时，普通安装默认优先落到 `USER`，没有 `USER` 时才落到 `PROJECT`；如果当前只有 `ROOT`，普通安装必须失败。

如果 `ROOT` 中已有同名 `skill_id`，普通 install / update 必须失败并提示该 skill 由系统层管理；普通 uninstall 可以用于删除 `PROJECT` / `USER` 中的同名残留。

## 5. 层级列表函数

普通 skills 管理面应提供一个层级列表函数，用于让调用方获取当前宿主支持的可操作层级标签。

推荐 Lua 侧名称：

```lua
vulcan.runtime.skills.layers()
```

推荐返回结构：

```lua
{
  default = "USER",
  writable = true,
  labels = { "PROJECT", "USER" },
  layers = {
    {
      label = "PROJECT",
      writable = true,
      description = "当前项目级 skill 层"
    },
    {
      label = "USER",
      writable = true,
      description = "用户全局 skill 层"
    }
  }
}
```

约束：

1. `labels` 只列出普通桥接可操作的层级。
2. `ROOT` 不应出现在普通桥接的可操作层级列表中。
3. `default` 必须是 `labels` 中的一个值。
4. 如果当前没有项目上下文，运行时只返回实际存在的 `USER`；如果只有 `ROOT`，则返回空 `labels` 和空 `layers`，且顶层 `writable=false`。
5. 当 `enable_skill_management_bridge` 关闭时，可以返回实际存在的普通层用于展示，但顶层 `writable` 与每个 layer 的 `writable` 都必须为 `false`。
6. `layers()` 只是能力发现接口，不替代宿主在 install / update / uninstall 等操作中的最终权限校验。

## 6. System Tools 管理面

system tools 是宿主控制面，可以安装、更新、删除 `ROOT` 级 skill。未显式传入目标 root 时，system install 默认只允许写入已配置的 `ROOT`；如果 root 链缺少 `ROOT`，必须失败，不能回退到普通层。

system 管理入口必须区分宿主注入的权限上下文：

```text
System = 可以写 ROOT
DelegatedTool = 普通 tools 委托权限，不能写 ROOT，也看不到 ROOT skills
```

可见性查询与 prompt completion 属于 authority 边界：`list_entries`、`list_skill_help`、`render_skill_help_detail`、`prompt_argument_completions`、`is_skill`、`skill_name_for_tool` 等入口在 `DelegatedTool` 下不得返回 `ROOT` skill 信息。`call_skill` 与 `run_lua` 是运行时执行面，面向当前已激活的 skill，不作为 `ROOT` 可见性或技能管理权限边界；如果宿主不希望普通用户执行任意 Lua，应在暴露 `run_lua` 前自行封装权限。

`System` 不是“跳过规则”。它与 `DelegatedTool` 的唯一区别是是否允许管理 `ROOT` 层 skill；ROOT 同名占用、普通层 install/update 冲突、普通层残留清理等规则完全一致。

建议宿主将面向用户或 AI 的技能管理能力组合成一个统一工具，例如：

```text
luaskills-manager(action, layer?, skill_id?, source?, options?)
```

建议：

1. `action` 覆盖增删改查，例如 `list / install / update / uninstall / enable / disable`。
2. `layer` 对普通用户默认只允许 `PROJECT` / `USER`。
3. 默认安装到 `USER` 还是 `PROJECT` 由宿主策略决定。
4. 不建议向普通用户开放 `ROOT` 级 skill 的调整能力。
5. 若确需开放 system tools 给普通 tools，应由宿主 wrapper 固定注入 `DelegatedTool` authority，而不是让调用方自由声明权限。
6. 若确需开放 `ROOT` 操作，应只放在宿主内部维护、管理员模式、修复流程或受控 system updater 中，并固定注入 `System` authority。

如果宿主只配置 `ROOT`，却仍希望让用户增删改 skill，本质上是在让用户操作系统层。LuaSkills 不会把这种场景伪装成普通 skills plane；宿主必须自行完成权限、白名单、确认、审计和合法性判断。

## 7. ROOT 手工修改与修复建议

LuaSkills 不试图防止用户手工修改 `ROOT` 目录。

原因：

1. 本地文件系统上的手工复制、删除、替换无法靠运行时完全阻止。
2. 真正有意修改 `ROOT` 的用户通常有明确目的，也可以绕过运行时限制。
3. 过度校验会增加复杂度，却不能形成可靠安全边界。

建议宿主提供 ROOT 修复能力：

1. 维护一份 `ROOT` 级 skill 安装表或期望清单。
2. 修复时可以清空 root 级 skill 目录。
3. 通过 system install 指令重新安装清单内的 root skill。
4. 修复完成后重新加载运行时。

这让 `ROOT` 层保持“系统基线”语义，同时避免把本地文件防篡改误做成运行时职责。
