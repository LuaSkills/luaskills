use super::*;
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::Path};

#[path = "../../../build_support/identity.rs"]
mod build_inputs;

/// Read the actual borrowed C descriptor; return its immutable address, length and parsed document.
/// 读取实际借用型 C 描述；返回其不可变地址、长度及已解析文档。
fn borrowed_description() -> (usize, usize, Value) {
    // Deliberately nonempty initial output must be replaced without reading or freeing its bogus contents.
    // 刻意非空的初始输出必须被替换，不能读取或释放其无效内容。
    let mut output = FfiBorrowedBuffer {
        ptr: std::ptr::dangling(),
        len: usize::MAX,
    };
    assert_eq!(
        unsafe { luaskills_ffi_embedded_describe_v1(&mut output) },
        EmbeddedFfiStatus::Ok as i32
    );
    assert!(!output.ptr.is_null());
    assert!(output.len > 0 && output.len <= EMBEDDED_DESCRIPTION_MAX_BYTES);
    let bytes = unsafe { std::slice::from_raw_parts(output.ptr, output.len) };
    (
        output.ptr as usize,
        output.len,
        serde_json::from_slice(bytes).unwrap(),
    )
}

/// Prove read-only discovery works concurrently without a transport and does not advertise reserved backends.
/// 证明只读发现可在没有传输时并发工作，且不宣称预留后端可用。
#[test]
fn ffi_embedded_description_is_borrowed_stable_and_versioned() {
    assert_eq!(
        unsafe { luaskills_ffi_embedded_describe_v1(std::ptr::null_mut()) },
        EmbeddedFfiStatus::InvalidArgument as i32
    );
    let first = borrowed_description();
    let copies = std::thread::scope(|scope| {
        let workers: Vec<_> = (0..8).map(|_| scope.spawn(borrowed_description)).collect();
        workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>()
    });
    for copy in copies {
        assert_eq!(copy, first);
    }
    let description = first.2;
    assert_eq!(
        description["description_version"],
        EMBEDDED_DESCRIPTION_VERSION
    );
    assert_eq!(
        description["protocol_version"],
        EMBEDDED_FFI_PROTOCOL_VERSION
    );
    assert_eq!(description["core_version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(description["capabilities"], json!(EMBEDDED_CAPABILITIES));
    assert_eq!(description["execution_backends"], json!(["in_process"]));
    assert_eq!(
        description["build"]["contract_sha256"],
        build_inputs::digest(include_bytes!(
            "../../../contracts/embedded/v1/contract.json"
        ))
    );
    assert_eq!(
        description["build"]["pointer_width"],
        usize::BITS.to_string()
    );
    assert!(
        description["build"]["rustc"]
            .as_str()
            .unwrap()
            .starts_with("rustc ")
    );
    #[cfg(feature = "contract-generation")]
    {
        // The independent descriptor has its own schema root and does not masquerade as a transport response.
        // 独立描述拥有自己的 Schema 根，不冒充传输响应。
        let schema = &contract::document()["core_description"];
        assert!(
            jsonschema::validator_for(schema)
                .unwrap()
                .is_valid(&description)
        );
    }
}

/// Keep existing transport description fields consistent with the new pre-ownership descriptor.
/// 保持既有传输描述字段与新的所有权创建前描述一致。
#[test]
fn ffi_embedded_description_preserves_transport_contract() {
    let core = borrowed_description().2;
    let id = create(config());
    let response = describe(id);
    let value: Value =
        serde_json::from_slice(unsafe { std::slice::from_raw_parts(response.ptr, response.len) })
            .unwrap();
    assert_eq!(luaskills_ffi_embedded_result_free_v1(id, response), 0);
    finish(id);
    for field in [
        "core_version",
        "protocol_version",
        "abi_structure_version",
        "commands",
        "runtime_commands",
    ] {
        assert_eq!(value["result"][field], core[field], "{field}");
    }
}

/// Compare compiled provenance with current exact source inputs and the generated compiler-input report.
/// 将编译来源与当前精确源码输入及生成的编译器输入报告比较。
#[test]
fn embedded_build_inputs_match_compiled_report() {
    let encoded = include_bytes!(concat!(env!("OUT_DIR"), "/embedded-build-inputs.json"));
    let report: Value = serde_json::from_slice(encoded).unwrap();
    let actual = build_inputs::source_files(Path::new(env!("CARGO_MANIFEST_DIR"))).unwrap();
    let recorded: BTreeMap<String, String> =
        serde_json::from_value(report["source_files"].clone()).unwrap();
    assert_eq!(
        recorded, actual,
        "build metadata is stale relative to source inputs"
    );
    assert_eq!(
        report["format_version"],
        build_inputs::INPUTS_FORMAT_VERSION
    );
    let identity = embedded_core_description().build;
    assert_eq!(identity.inputs_sha256, build_inputs::digest(encoded));
    assert_eq!(identity.source_sha256, build_inputs::source_digest(&actual));
    assert_eq!(
        identity.package_lock_sha256,
        build_inputs::digest(include_bytes!("../../../Cargo.lock"))
    );
    assert_eq!(report["fields"]["target"], identity.target);
    assert_eq!(report["cargo_features"], json!(identity.cargo_features));
    assert!(
        actual
            .keys()
            .all(|name| !name.contains('\\') && !Path::new(name).is_absolute())
    );
}

/// Input identities must change for renamed or edited sources but ignore map insertion order.
/// 输入身份必须随源码改名或编辑而变化，同时忽略映射插入顺序。
#[test]
fn embedded_build_digest_preserves_name_and_content_boundaries() {
    let first = BTreeMap::from([
        ("ab".to_owned(), "c".to_owned()),
        ("z".to_owned(), "tail".to_owned()),
    ]);
    let reordered = BTreeMap::from([
        ("z".to_owned(), "tail".to_owned()),
        ("ab".to_owned(), "c".to_owned()),
    ]);
    let renamed = BTreeMap::from([
        ("a".to_owned(), "bc".to_owned()),
        ("z".to_owned(), "tail".to_owned()),
    ]);
    let mut changed = first.clone();
    changed.insert("ab".to_owned(), "different".to_owned());
    assert_eq!(
        build_inputs::source_digest(&first),
        build_inputs::source_digest(&reordered)
    );
    assert_ne!(
        build_inputs::source_digest(&first),
        build_inputs::source_digest(&renamed)
    );
    assert_ne!(
        build_inputs::source_digest(&first),
        build_inputs::source_digest(&changed)
    );
    assert!(
        build_inputs::source_files(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("missing-build-input-fixture")
        )
        .is_err()
    );
}
