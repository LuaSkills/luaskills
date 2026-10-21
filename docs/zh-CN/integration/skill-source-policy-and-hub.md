# Skill 来源策略、官方 Hub 与进度事件

## 1. 来源边界

LuaSkills 的公开用户安装面只建议开放两类来源：

1. `github`：通过 GitHub Release 解析 `{skill_id}-v{version}-skill.zip` 与 checksums。
2. `official_hub`：通过官方 Skills Hub resolve 接口获取标准 manifest 后安装。

`url` 不再作为用户可见安装来源。历史 `url` source type 保留用于兼容解析，但运行时会拒绝公开 URL 安装。

宿主如确实需要从自己的根技能服务、内网发布服务或受控 CDN 安装，可以使用 `private_url_manifest`。该来源必须满足：

1. 调用 system 入口且 authority 为 `system`。
2. `enable_private_url_skill_install = true`。
3. manifest URL 与 archive URL 均命中 `private_skill_source_allowlist`。
4. manifest 提供归档 SHA-256。

## 2. Host Options

新增来源策略字段：

```json
{
  "github_base_url": "https://gh-proxy.example.com/github",
  "github_api_base_url": "https://gh-proxy.example.com/api.github.com",
  "official_skill_hub_base_url": "https://skills.example.com",
  "enable_private_url_skill_install": false,
  "private_skill_source_allowlist": [
    "https://internal.example.com/luaskills/manifests"
  ]
}
```

`github_base_url` 与 `github_api_base_url` 是全局 GitHub 源替换。只要安装或依赖解析使用 GitHub Release，就会统一使用这组配置，不需要在每个 skill 依赖里单独封装。

## 3. 官方 Hub 接口

宿主或官方 Hub 推荐提供以下接口：

```text
GET /api/v1/health
GET /api/v1/skills/search?q={keyword}&platform={platform}&runtime_version={version}
GET /api/v1/skills/{skill_id}
GET /api/v1/skills/{skill_id}/versions
GET /api/v1/skills/{skill_id}/resolve?version=latest
GET /api/v1/sources
```

搜索响应建议格式：

```json
{
  "items": [
    {
      "skill_id": "skill-search",
      "display_name": "Skill Search",
      "description": "Search official LuaSkills Hub skills.",
      "latest_version": "0.1.0",
      "verified": true,
      "source_type": "official_hub",
      "source": "skill-search",
      "tags": ["official", "search"]
    }
  ],
  "next_cursor": null
}
```

resolve 响应同时也是运行时可消费的标准 manifest：

```json
{
  "schema_version": 1,
  "skill_id": "skill-search",
  "version": "0.1.0",
  "archive": {
    "type": "zip",
    "url": "https://cdn.example.com/skills/skill-search-v0.1.0-skill.zip",
    "sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
  },
  "update": {
    "source_type": "official_hub",
    "locator": "skill-search",
    "tag": "v0.1.0"
  }
}
```

## 4. 私有 URL Manifest

私有 URL manifest 与官方 Hub resolve 响应使用同一格式。区别是它只能通过 `private_url_manifest` source type 进入，并受到 system authority、开关和 allowlist 约束。

示例安装请求：

```json
{
  "request": {
    "skill_id": "internal-tool",
    "source_type": "private_url_manifest",
    "source": "https://internal.example.com/luaskills/manifests/internal-tool.json"
  },
  "authority": "system"
}
```

普通 SDK/demo 不应暴露该来源；如果宿主要开放，应把它放在管理员工具、修复工具或系统更新器中。

JSON FFI 提供了独立私有入口，SDK 可以只在宿主私有工具层绑定它们：

```text
luaskills_ffi_system_private_install_skill_from_url_manifest_json
luaskills_ffi_system_private_update_skill_from_url_manifest_json
```

这两个入口会强制要求 `authority = "system"`，并在内部固定构造 `source_type = "private_url_manifest"`，普通用户安装工具不需要也不应该复用它们。

## 5. 安装进度事件

安装与更新会通过进程级 progress callback 发出事件。JSON FFI 可注册：

```text
luaskills_ffi_set_skill_operation_progress_json_callback
```

事件示例：

```json
{
  "operation_id": "skill-install-skill-search-1780000000000",
  "sequence": 4,
  "plane": "System",
  "action": "Install",
  "phase": "downloading_archive",
  "status": "progress",
  "skill_id": "skill-search",
  "root_name": "ROOT",
  "source_type": "official_hub",
  "source_locator": "https://cdn.example.com/skills/skill-search-v0.1.0-skill.zip",
  "bytes_done": 1048576,
  "bytes_total": 5242880,
  "percent": 20.0,
  "message": null
}
```

常见 phase：

1. `validating_request`
2. `resolving_source`
3. `fetching_manifest`
4. `source_resolved`
5. `downloading_archive`
6. `extracting_archive`
7. `validating_skill_manifest`
8. `staging_install` / `staging_update`
9. `reloading_runtime`
10. `committing`
11. `completed`
12. `failed`

同步 API 仍保持兼容。宿主 UI 不应在 progress callback 中重入同一个 engine 的 install/update；建议把事件投递到 UI 队列，由 SDK 在后台线程或异步任务中执行安装。

## 6. `skill-search`

仓库提供 `skills/skill-search` 示例 skill。它通过以下宿主工具访问 Hub：

1. `luaskills.skill_hub.search`
2. `luaskills.skill_hub.detail`
3. `luaskills.skill_hub.resolve`
4. `luaskills.skill_hub.sources`

该 skill 只负责搜索、详情和解析，不直接调用安装接口。宿主应在用户确认后再把 `source_type=official_hub` 或 `source_type=github` 的请求转发到安装 API。
