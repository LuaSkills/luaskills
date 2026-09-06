use luaskills::runtime::cache::{
    ToolCacheConfig, configure_global_tool_cache, try_configure_global_tool_cache,
};

/// Return the exact source fragment between two required protocol markers.
/// 返回两个必需协议标记之间的精确源码片段。
///
/// `source` is the complete checked-in binding source, while `start` and `end` delimit one structure definition.
/// `source` 是完整的已签入绑定源码，`start` 与 `end` 用于限定一个结构定义。
///
/// The returned slice excludes both markers and panics when the checked-in protocol copy is malformed.
/// 返回切片不包含两个标记；已签入协议副本格式错误时触发 panic。
fn required_source_fragment<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
    // FragmentStart is the first byte immediately after the required opening marker.
    // FragmentStart 是必需起始标记之后的第一个字节位置。
    let fragment_start = source
        .find(start)
        .map(|offset| offset + start.len())
        .unwrap_or_else(|| panic!("missing protocol marker: {start}"));
    // RemainingSource limits the closing-marker search to the selected definition.
    // RemainingSource 将结束标记搜索限制在已选定义内。
    let remaining_source = &source[fragment_start..];
    // FragmentEnd is the relative byte offset of the required closing marker.
    // FragmentEnd 是必需结束标记的相对字节偏移。
    let fragment_end = remaining_source
        .find(end)
        .unwrap_or_else(|| panic!("missing protocol marker: {end}"));
    &remaining_source[..fragment_end]
}

/// Parse ordered field names from one public C header structure.
/// 从一个公共 C 头文件结构中解析有序字段名。
///
/// `header` is the complete public header and `struct_name` is the exact typedef structure name.
/// `header` 是完整公共头文件，`struct_name` 是精确的 typedef 结构名。
///
/// Returns field names in ABI order so language bindings can be compared byte-for-byte by position.
/// 按 ABI 顺序返回字段名，以便按位置逐项比较语言绑定。
fn c_struct_fields(header: &str, struct_name: &str) -> Vec<String> {
    // StartMarker identifies the authoritative public typedef opening.
    // StartMarker 标识权威公共 typedef 起始位置。
    let start_marker = format!("typedef struct {struct_name} {{");
    // EndMarker identifies the matching typedef closing without accepting another structure.
    // EndMarker 标识匹配的 typedef 结束位置，不接受其他结构。
    let end_marker = format!("}} {struct_name};");
    // Body is the exact authoritative field list.
    // Body 是精确的权威字段列表。
    let body = required_source_fragment(header, &start_marker, &end_marker);
    body.lines()
        .map(str::trim)
        .filter(|line| line.ends_with(';') && !line.starts_with("/*"))
        .map(|line| {
            line.trim_end_matches(';')
                .split_whitespace()
                .last()
                .expect("C structure field must have a name")
                .trim_start_matches('*')
                .to_string()
        })
        .collect()
}

/// Parse ordered ctypes field names from one Python structure class.
/// 从一个 Python ctypes 结构类中解析有序字段名。
///
/// `source` is the complete Python binding and `struct_name` is the exact ctypes class name.
/// `source` 是完整 Python 绑定源码，`struct_name` 是精确的 ctypes 类名。
///
/// Returns the `_fields_` names in declaration order.
/// 按声明顺序返回 `_fields_` 字段名。
fn python_struct_fields(source: &str, struct_name: &str) -> Vec<String> {
    // ClassMarker selects the exact ctypes structure instead of later object assignments.
    // ClassMarker 选择精确 ctypes 结构，而不是后续对象赋值。
    let class_marker = format!("class {struct_name}(ctypes.Structure):");
    // ClassBody ends at the next class declaration; a sentinel supports the final class in a file.
    // ClassBody 在下一个类声明处结束；哨兵支持文件中的最后一个类。
    let class_tail = source
        .split_once(&class_marker)
        .map(|(_, tail)| tail)
        .unwrap_or_else(|| panic!("missing Python structure: {struct_name}"));
    // ClassBody is restricted before parsing `_fields_` entries.
    // ClassBody 在解析 `_fields_` 条目前被限制到当前类。
    let class_body = class_tail.split("\nclass ").next().unwrap_or(class_tail);
    // FieldsBody excludes methods and unrelated class text after the ctypes field list.
    // FieldsBody 排除 ctypes 字段列表后的方法和无关类文本。
    let fields_tail = class_body
        .split_once("_fields_ = [")
        .map(|(_, tail)| tail)
        .unwrap_or_else(|| panic!("missing Python _fields_ list: {struct_name}"));
    // FieldsBody terminates at the first list close owned by `_fields_`.
    // FieldsBody 在 `_fields_` 所属的第一个列表结束符处终止。
    let fields_body = fields_tail
        .split_once(']')
        .map(|(body, _)| body)
        .expect("Python _fields_ list must close");
    // Fields collects every quoted field name from the isolated ctypes `_fields_` list.
    // Fields 从已隔离的 ctypes `_fields_` 列表中收集每个带引号的字段名。
    let mut fields = Vec::new();
    // RemainingFields advances past one quoted field name per iteration, independent of tuple line wrapping.
    // RemainingFields 每次迭代越过一个带引号的字段名，不受元组换行方式影响。
    let mut remaining_fields = fields_body;
    while let Some((_, field_tail)) = remaining_fields.split_once('\"') {
        // FieldName is the exact quoted token in the current ctypes tuple.
        // FieldName 是当前 ctypes 元组中的精确引号标记。
        let (field_name, next_fields) = field_tail
            .split_once('\"')
            .expect("Python ctypes field name must close");
        fields.push(field_name.to_string());
        remaining_fields = next_fields;
    }
    fields
}

/// Parse ordered Koffi field names from one TypeScript structure declaration.
/// 从一个 TypeScript Koffi 结构声明中解析有序字段名。
///
/// `source` is the complete TypeScript binding and `struct_name` is the exact Koffi structure name.
/// `source` 是完整 TypeScript 绑定源码，`struct_name` 是精确的 Koffi 结构名。
///
/// Returns object keys in declaration order without reading later payload objects.
/// 按声明顺序返回对象键，并且不会读取后续载荷对象。
fn typescript_struct_fields(source: &str, struct_name: &str) -> Vec<String> {
    // StartMarker selects the exact Koffi declaration.
    // StartMarker 选择精确的 Koffi 声明。
    let start_marker = format!("const {struct_name} = koffi.struct(\"{struct_name}\", {{");
    // Body ends at the matching flat structure declaration close.
    // Body 在匹配的平坦结构声明结束处终止。
    let body = required_source_fragment(source, &start_marker, "});");
    body.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with("//"))
        .filter_map(|line| {
            line.split_once(':')
                .map(|(name, _)| name.trim().to_string())
        })
        .collect()
}

/// Verify one checked-in binding structure matches the authoritative C ABI field order.
/// 验证一个已签入绑定结构与权威 C ABI 字段顺序一致。
///
/// `expected` is the header-derived field list, `actual` is one binding-derived list, and `label` identifies failures.
/// `expected` 是从头文件派生的字段列表，`actual` 是绑定派生列表，`label` 用于标识失败来源。
///
/// Returns unit after asserting exact field-name and order equality.
/// 精确断言字段名与顺序相等后返回 unit。
fn assert_binding_layout(expected: &[String], actual: Vec<String>, label: &str) {
    assert_eq!(actual, expected, "{label} drifted from the public C ABI");
}

/// Verify the unit-returning cache configurator remains source-compatible while a checked API is also available.
/// 验证返回 unit 的缓存配置函数保持源码兼容，同时提供可检查错误的新 API。
#[test]
fn legacy_cache_configuration_surface_remains_source_compatible() {
    // LegacyConfigurator preserves the original function type without mutating global test state.
    // LegacyConfigurator 在不修改全局测试状态的情况下保留原始函数类型。
    std::hint::black_box(configure_global_tool_cache as fn(ToolCacheConfig));
    // CheckedConfigurator preserves the explicit conflict-reporting entrypoint used by engine construction.
    // CheckedConfigurator 保留引擎构造使用的显式冲突报告入口。
    std::hint::black_box(
        try_configure_global_tool_cache as fn(ToolCacheConfig) -> Result<(), String>,
    );
}

/// Verify every hand-written Python and TypeScript V1 binding stays aligned with the public C ABI.
/// 验证每个手写 Python 与 TypeScript V1 绑定始终与公共 C ABI 对齐。
#[test]
fn handwritten_ffi_bindings_match_public_v1_layouts() {
    // Header is the authoritative source for standard C ABI field names and order.
    // Header 是标准 C ABI 字段名与顺序的权威来源。
    let header = include_str!("../include/luaskills_ffi.h");
    // HostFields is the complete current V1 host-options layout.
    // HostFields 是当前完整的 V1 宿主选项布局。
    let host_fields = c_struct_fields(header, "FfiLuaRuntimeHostOptions");
    // EngineFields is the stable pool-plus-host V1 engine wrapper layout.
    // EngineFields 是稳定的“池配置加宿主选项”V1 引擎包装布局。
    let engine_fields = c_struct_fields(header, "FfiLuaEngineOptions");

    // PythonBindings cover every checked-in file that duplicates the standard V1 structures.
    // PythonBindings 覆盖每个重复定义标准 V1 结构的已签入文件。
    let python_bindings = [
        (
            "python/demo.py",
            include_str!("../examples/ffi/python/demo.py"),
        ),
        (
            "demo_runtime/run_python_install_demo.py",
            include_str!("../examples/ffi/demo_runtime/run_python_install_demo.py"),
        ),
        (
            "host_provider_demo/run_python_host_provider_demo.py",
            include_str!("../examples/ffi/host_provider_demo/run_python_host_provider_demo.py"),
        ),
    ];
    for (label, source) in python_bindings {
        assert_binding_layout(
            &host_fields,
            python_struct_fields(source, "FfiLuaRuntimeHostOptions"),
            label,
        );
        assert_binding_layout(
            &engine_fields,
            python_struct_fields(source, "FfiLuaEngineOptions"),
            label,
        );
    }

    // TypeScriptBindings cover every Koffi example that declares the V1 engine surface directly.
    // TypeScriptBindings 覆盖每个直接声明 V1 引擎界面的 Koffi 示例。
    let typescript_bindings = [
        (
            "typescript/lifecycle_demo.ts",
            include_str!("../examples/ffi/typescript/lifecycle_demo.ts"),
        ),
        (
            "typescript/query_demo.ts",
            include_str!("../examples/ffi/typescript/query_demo.ts"),
        ),
    ];
    for (label, source) in typescript_bindings {
        assert_binding_layout(
            &host_fields,
            typescript_struct_fields(source, "FfiLuaRuntimeHostOptions"),
            label,
        );
        assert_binding_layout(
            &engine_fields,
            typescript_struct_fields(source, "FfiLuaEngineOptions"),
            label,
        );
    }

    // V3Fields cover the extended runtime roots and managed-runtime policy used by the primary demos.
    // V3Fields 覆盖主示例使用的扩展运行时根与受管运行时策略。
    let v2_host_fields = c_struct_fields(header, "FfiLuaRuntimeHostOptionsV2");
    let managed_runtime_config_fields =
        c_struct_fields(header, "FfiLuaRuntimeManagedRuntimeConfig");
    let v3_host_fields = c_struct_fields(header, "FfiLuaRuntimeHostOptionsV3");
    let v3_engine_fields = c_struct_fields(header, "FfiLuaEngineOptionsV3");
    // PrimaryPythonBinding is the V3 ctypes surface exercised by the Python release-DLL demo.
    // PrimaryPythonBinding 是 Python release DLL 示例执行的 V3 ctypes 界面。
    let primary_python_binding = include_str!("../examples/ffi/python/demo.py");
    assert_binding_layout(
        &v2_host_fields,
        python_struct_fields(primary_python_binding, "FfiLuaRuntimeHostOptionsV2"),
        "python/demo.py V2 host options",
    );
    assert_binding_layout(
        &managed_runtime_config_fields,
        python_struct_fields(primary_python_binding, "FfiLuaRuntimeManagedRuntimeConfig"),
        "python/demo.py managed runtime config",
    );
    assert_binding_layout(
        &v3_host_fields,
        python_struct_fields(primary_python_binding, "FfiLuaRuntimeHostOptionsV3"),
        "python/demo.py V3 host options",
    );
    assert_binding_layout(
        &v3_engine_fields,
        python_struct_fields(primary_python_binding, "FfiLuaEngineOptionsV3"),
        "python/demo.py V3 engine options",
    );

    // PrimaryTypeScriptBinding is the V3 Koffi surface exercised by the TypeScript release-DLL demo.
    // PrimaryTypeScriptBinding 是 TypeScript release DLL 示例执行的 V3 Koffi 界面。
    let primary_typescript_binding = include_str!("../examples/ffi/typescript/demo.ts");
    assert_binding_layout(
        &v2_host_fields,
        typescript_struct_fields(primary_typescript_binding, "FfiLuaRuntimeHostOptionsV2"),
        "typescript/demo.ts V2 host options",
    );
    assert_binding_layout(
        &managed_runtime_config_fields,
        typescript_struct_fields(
            primary_typescript_binding,
            "FfiLuaRuntimeManagedRuntimeConfig",
        ),
        "typescript/demo.ts managed runtime config",
    );
    assert_binding_layout(
        &v3_host_fields,
        typescript_struct_fields(primary_typescript_binding, "FfiLuaRuntimeHostOptionsV3"),
        "typescript/demo.ts V3 host options",
    );
    assert_binding_layout(
        &v3_engine_fields,
        typescript_struct_fields(primary_typescript_binding, "FfiLuaEngineOptionsV3"),
        "typescript/demo.ts V3 engine options",
    );
}
