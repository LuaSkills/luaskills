-- Execute one configuration-aware example action.
-- 执行一个配置感知示例操作。
--
-- args.action optionally selects valid or intentionally invalid mutation demos.
-- args.action 可选选择合法或故意非法的修改示例。
--
-- Return a JSON string or a plain-text missing-configuration instruction.
-- 返回 JSON 字符串或纯文本缺配置指引。
return function(args)
    -- Completeness status shared by every entry in this package.
    -- 当前技能包全部入口共享的完整性状态。
    local status = vulcan.config.status()
    if not status.complete then
        return [[This skill package configuration is incomplete.

Ask the AI to call the host runtime-config tool:
1. Use action=describe and skill_id=example-config-skill to inspect names, types, constraints, and descriptions.
2. After host or user authorization, use action=set to configure api_token.
3. If the current environment cannot modify configuration, ask the user to provide api_token.]]
    end

    if args.action == "set_valid_demo" then
        vulcan.config.set({
            retry_count = 5,
            telemetry_enabled = true,
        })
    elseif args.action == "set_invalid_demo" then
        vulcan.config.set("retry_count", "99")
    end

    -- Required sensitive token stored by the host after authorization.
    -- 宿主在授权后存储的必填敏感令牌。
    local api_token = vulcan.config.get("api_token")
    -- Canonical integer string converted for example business use.
    -- 转换为示例业务用途的规范整数字符串。
    local retry_count = tonumber(vulcan.config.get("retry_count"))
    -- Canonical floating-point string converted for example business use.
    -- 转换为示例业务用途的规范浮点字符串。
    local temperature = tonumber(vulcan.config.get("temperature"))
    -- Stable enumeration machine value.
    -- 稳定枚举机器值。
    local provider = vulcan.config.get("provider")
    -- Canonical boolean string converted explicitly.
    -- 显式转换的规范布尔字符串。
    local telemetry_enabled = vulcan.config.get("telemetry_enabled") == "true"

    return vulcan.json.encode({
        configured = api_token ~= nil,
        retry_count = retry_count,
        temperature = temperature,
        provider = provider,
        telemetry_enabled = telemetry_enabled,
    })
end
