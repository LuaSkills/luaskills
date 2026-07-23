-- Return this package's declaration and completeness status.
-- 返回当前技能包的声明与完整性状态。
--
-- Return a JSON document suitable for host or AI inspection.
-- 返回适合宿主或 AI 检查的 JSON 文档。
return function(args)
    -- Value-free declaration visible inside this package.
    -- 当前技能包内部可见且不包含值的声明。
    local declaration = vulcan.config.describe()
    -- Package completeness and validity status.
    -- 技能包完整性与合法性状态。
    local status = vulcan.config.status()

    return vulcan.json.encode({
        declaration = declaration,
        status = status,
    })
end
