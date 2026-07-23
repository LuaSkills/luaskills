# LuaSkills 0.5.5 Package Configuration Upgrade Guide / 技能包配置升级说明

LuaSkills `0.5.5` establishes the first supported package-level configuration contract. Earlier configuration code in the repository was an unpublished draft. This release intentionally rejects its unversioned files, old host option, old single-value C symbol, ambiguous status fields, and any migration aliases.

LuaSkills `0.5.5` 建立首个正式支持的技能包级配置契约。仓库此前的配置代码属于未发布草案；本版本有意拒绝其无版本文件、旧宿主选项、旧单值 C 符号、歧义状态字段以及任何迁移别名。

## Version alignment / 版本对齐

- Rust crate: `luaskills = "0.5.5"`
- TypeScript SDK: `@luaskills/sdk@0.5.5`
- Python SDK: `luaskills-sdk==0.5.5`
- Go module: `github.com/LuaSkills/luaskills-sdk-go@v0.5.5`
- Core FFI and demo assets: GitHub tag `v0.5.5`

Use matching core and SDK releases. The canonical machine contract is `contracts/skill-config/v1/contract.json`; each SDK generates its shared enums and limits from the exact contract copied from core.

核心与 SDK 应使用匹配版本。规范机器契约位于 `contracts/skill-config/v1/contract.json`；每个 SDK 都从核心复制的精确契约生成公共枚举与限制。

## Required host changes / 宿主必改项

1. Set `host_options.skill_config_root` to one explicit absolute, writable, user-level directory.
2. Replace `skill_config_file_path`; it is not accepted.
3. Expect two fixed stores:
   - `<skill_config_root>/skills/config.json`
   - `<skill_config_root>/system-skills/config.json`
4. Send typed nonempty `values` objects for writes. Single-key SDK helpers wrap the same batch transaction.
5. Treat every returned `revision` and event `sequence` as a decimal string.
6. Use `expected_revision` for compare-and-swap writes or deletes whenever stale UI or concurrent processes are possible.
7. Do not pass an old unversioned configuration file. Create the new document explicitly or let LuaSkills create it on the first write.

1. 把 `host_options.skill_config_root` 设置为显式、绝对、可写的用户级目录。
2. 删除 `skill_config_file_path`；该字段不再接受。
3. 接受两个固定存储：
   - `<skill_config_root>/skills/config.json`
   - `<skill_config_root>/system-skills/config.json`
4. 写入时发送类型化非空 `values` 对象；SDK 单键便利方法仍会包装成同一批量事务。
5. 所有 `revision` 与事件 `sequence` 都按十进制字符串处理。
6. UI 状态可能过期或存在并发进程时，写入和删除必须使用 `expected_revision`。
7. 不要传入旧的无版本配置文件；显式创建新文档，或让 LuaSkills 在首次写入时创建。

The only persisted shape is:

唯一持久化格式为：

```json
{
  "format_version": 1,
  "revision": "1",
  "skills": {
    "example-skill": {
      "enabled": "true",
      "retry_count": "3"
    }
  }
}
```

External editors must acquire the companion `.lock`, read the latest document while holding it, increment `revision`, write a same-directory temporary file, flush it, and atomically replace the destination. Invalid or non-monotonic external files do not replace the last-good cached snapshot.

外部编辑器必须获取伴随 `.lock`，在持锁期间读取最新文档、递增 `revision`、写入同目录临时文件、刷新并原子替换目标。非法或非单调外部文件不会替换最后一个合法缓存快照。

## Required package changes / 技能包必改项

Declare configuration only at the top level of `skill.yaml`. All entries in one package share the declaration and persisted namespace.

配置只能声明在 `skill.yaml` 顶层，同一技能包内所有入口共享声明与持久化命名空间。

```yaml
config:
  - key: retry_count
    type: integer
    description: Maximum request retry count
    default: 3
    constraints:
      minimum: 0
      maximum: 10

config_validator: runtime/config-validator.lua
```

Supported types are `integer`, `string`, `float`, `enum`, and `boolean`. Human-readable metadata is single-language package content. LuaSkills does not provide configuration-specific i18n fields or language fallback.

支持 `integer`、`string`、`float`、`enum`、`boolean`。人类可读元数据属于技能包自行选择的单语言内容；LuaSkills 不提供配置专属 i18n 字段或语言回退。

Lua code uses `vulcan.config.get/has/list/status/delete` and polymorphic `set(key, value)` or `set(table)`. LuaSkills binds these calls to the current package identity, so package Lua cannot modify another package. Host SDK and FFI calls are cross-package management APIs and must be authorized by the embedding product.

Lua 代码使用 `vulcan.config.get/has/list/status/delete`，以及多态 `set(key, value)` 或 `set(table)`。LuaSkills 把这些调用绑定到当前技能包身份，因此技能包 Lua 不能修改其他包。宿主 SDK 与 FFI 属于跨包管理 API，必须由上级产品授权。

## Discovery, disclosure, and events / 发现、披露与事件

- Effective `describe` returns declared types, constraints, UI hints, defaults, states, completeness, and orphaned keys.
- Installed discovery scans enabled, disabled, shadowed, and invalid physical packages without executing Lua.
- Saved and effective values are omitted unless the host explicitly requests `include_values=true`.
- Defaults remain declaration metadata even for `sensitive=true`; package authors must never use a secret default.
- LuaSkills does not decide who may read or change values. The host may deny, force parameters, or request user approval.
- File-watch events are ordered by engine-local decimal `sequence`. Consumers must process a complete page before advancing to `next_sequence`.
- Uninstall does not delete configuration. Any explicit cleanup belongs to the host.

- 有效 `describe` 返回声明类型、约束、UI 提示、默认值、状态、完整性与 orphaned key。
- 已安装发现不执行 Lua，即可扫描启用、禁用、被遮蔽和清单非法的物理技能包。
- 除非宿主显式请求 `include_values=true`，否则省略已保存值与有效值。
- 即使 `sensitive=true`，默认值仍属于声明元数据；技能包作者绝不能设置秘密默认值。
- LuaSkills 不判断谁可以读取或修改值；宿主可以拒绝、强制参数或请求用户授权。
- 文件监听事件使用引擎内十进制 `sequence` 排序；消费者完整处理一页后才能前进到 `next_sequence`。
- 卸载不会删除配置；显式清理由宿主负责。

## Missing-configuration help / 缺配置帮助

When `status.complete` is false, a skill should tell the host or AI to call `runtime-config` with `action=describe` and its package id. If mutation is authorized, submit all available values in one `set` batch. Otherwise list the exact declared parameters the user must provide without echoing secrets.

当 `status.complete=false` 时，技能应告知宿主或 AI 使用自身技能包 id 调用 `runtime-config(action=describe)`。如果修改已获授权，应通过一次 `set` 批量提交全部可用值；否则应列出用户需要提供的精确声明参数，并且不得回显秘密。

For the complete declaration, storage, locking, authorization, event, and tool contracts, see [Skill package configuration declaration and host integration](zh-CN/architecture/skill-config-system-design.md).

完整声明、存储、锁、授权、事件与工具契约见[技能包配置声明与宿主对接](zh-CN/architecture/skill-config-system-design.md)。
