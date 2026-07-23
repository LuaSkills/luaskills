-- Validate cross-field package configuration in the isolated configuration sandbox.
-- 在隔离配置沙箱中校验跨字段技能包配置。
--
-- values contains typed effective values from declarations and defaults.
-- values 包含来自声明与默认值的类型化有效值。
--
-- Return an array of optional-key issues without performing I/O or mutation.
-- 返回可选关联键的问题数组，不执行 I/O 或修改。
return function(values)
    -- Local mode is intentionally conservative for the example.
    -- 示例中的本地模式有意采用更保守的参数。
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
