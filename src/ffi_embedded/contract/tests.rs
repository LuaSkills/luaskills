use super::*;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::OnceLock,
};

/// Build the exact generated document once for real native exchange validation.
/// 为实际原生交换校验一次性构建精确生成文档。
/// Return a process-lifetime immutable document with no runtime engine or external resources.
/// 返回进程寿命内不可变文档，不包含运行时引擎或外部资源。
fn contract() -> &'static Value {
    static DOCUMENT: OnceLock<Value> = OnceLock::new();
    DOCUMENT.get_or_init(document)
}

/// Cache independent validators for the request, failure and each command's successful response.
/// 缓存请求、失败及各命令成功响应的独立验证器。
/// Return a read-only map; reject duplicate names rather than replacing another command's contract.
/// 返回只读映射；拒绝重复名称，不替换其他命令的契约。
fn validators() -> &'static BTreeMap<String, jsonschema::Validator> {
    static VALIDATORS: OnceLock<BTreeMap<String, jsonschema::Validator>> = OnceLock::new();
    VALIDATORS.get_or_init(|| {
        let document = contract();
        let mut validators = BTreeMap::new();
        for (name, schema) in [
            ("request", &document["request"]),
            ("error_response", &document["error_response"]),
            ("core_description", &document["core_description"]),
        ]
        .into_iter()
        .chain(
            document["root_responses"]
                .as_object()
                .unwrap()
                .iter()
                .map(|(name, schema)| (name.as_str(), schema)),
        )
        .chain(
            document["runtime_responses"]
                .as_object()
                .unwrap()
                .iter()
                .map(|(name, schema)| (name.as_str(), schema)),
        ) {
            assert!(
                validators
                    .insert(name.to_owned(), jsonschema::validator_for(schema).unwrap())
                    .is_none(),
                "duplicate generated contract name: {name}"
            );
        }
        validators
    })
}

/// Validate an actual native response against its exact command's generated schema.
/// 对照精确命令的生成 Schema 校验实际原生响应。
/// `request` and `response` are decoded wire frames; assertion failures identify contract drift.
/// `request` 和 `response` 是已解码线帧；断言失败标识契约漂移。
pub(crate) fn assert_exchange(request: &Value, response: &Value) {
    let validators = validators();
    let request_validator = &validators["request"];
    assert!(
        request_validator.is_valid(request),
        "native accepted a request outside its generated schema: {request}"
    );
    let name = if response["status"] == "error" {
        "error_response"
    } else {
        let command = &request["command"];
        let name = command["type"].as_str().unwrap();
        if name == "runtime" {
            command["operation"]["type"].as_str().unwrap()
        } else {
            name
        }
    };
    let validator = &validators[name];
    let failures: Vec<_> = validator
        .iter_errors(response)
        .map(|error| error.to_string())
        .collect();
    assert!(
        failures.is_empty(),
        "generated response contract drift: {failures:?}; request={request}; response={response}"
    );
}

/// Ensure the committed offline artifact and digest exactly match this source and locked generator.
/// 确保提交的离线产物及摘要与当前源码和锁定生成器精确匹配。
#[test]
fn embedded_contract_artifact_is_current() {
    let generated = bytes().unwrap();
    assert_eq!(
        generated,
        include_bytes!("../../../contracts/embedded/v1/contract.json"),
        "embedded contract is stale; run generate_embedded_contract"
    );
    assert_eq!(
        format!("{:x}  contract.json\n", Sha256::digest(&generated)),
        include_str!("../../../contracts/embedded/v1/contract.sha256").replace("\r\n", "\n")
    );
    assert_eq!(
        source_digest("first\r\nsecond\r\n"),
        source_digest("first\nsecond\n")
    );
}

/// Compile each independently rooted schema without network or neighbouring SDK repositories.
/// 不使用网络或相邻 SDK 仓库，编译每个独立根 Schema。
#[test]
fn embedded_contract_all_schemas_resolve_offline() {
    let document = contract();
    for schema in [&document["request"], &document["error_response"]]
        .into_iter()
        .chain(document["root_responses"].as_object().unwrap().values())
        .chain(document["runtime_responses"].as_object().unwrap().values())
    {
        jsonschema::validator_for(schema)
            .expect("generated standalone schema must compile offline");
    }
}

/// Verify advertised and response-mapped commands cover every Rust-derived discriminated variant.
/// 校验公布命令及响应映射覆盖全部 Rust 派生判别变体。
#[test]
fn embedded_contract_command_coverage_matches_rust() {
    let document = contract();
    for (definition, advertised, responses) in [
        ("Command", "commands", "root_responses"),
        ("RuntimeCommand", "runtime_commands", "runtime_responses"),
    ] {
        let schema = &document["request"]["$defs"][definition];
        let names: BTreeSet<_> = schema["oneOf"]
            .as_array()
            .unwrap()
            .iter()
            .map(|variant| variant["properties"]["type"]["const"].as_str().unwrap())
            .collect();
        let advertised: BTreeSet<_> = document[advertised]
            .as_array()
            .unwrap()
            .iter()
            .map(|name| name.as_str().unwrap())
            .collect();
        assert_eq!(
            names, advertised,
            "advertised commands drifted from {definition}"
        );
        let mapped: BTreeSet<_> = document[responses]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        let expected: BTreeSet<_> = names
            .into_iter()
            .filter(|name| *name != "runtime")
            .collect();
        assert_eq!(
            expected, mapped,
            "response coverage drifted from {definition}"
        );
    }
}

/// Compare strict callback completion shapes with the actual Rust parser, retaining successful null.
/// 对照实际 Rust 解析器检查严格回调完成形状，保留成功空值。
#[test]
fn embedded_contract_completion_presence_matches_parser() {
    let validator = jsonschema::validator_for(&contract()["request"]).unwrap();
    for (outcome, accepted) in [
        (json!({"ok":true,"value":null,"effects":"committed"}), true),
        (
            json!({"ok":true,"value":{"text":"中文\u{0000}","list":[false,{},[]]},"effects":"unknown"}),
            true,
        ),
        (
            json!({"ok":false,"error":{"code":"execution_failed","message":"failure"},"effects":"rolled_back"}),
            true,
        ),
        (json!({"ok":true,"effects":"committed"}), false),
        (
            json!({"ok":true,"value":null,"effects":"committed","unknown":true}),
            false,
        ),
        (
            json!({"ok":true,"value":null,"effects":"committed","error":{"code":"internal","message":"mixed"}}),
            false,
        ),
    ] {
        let request = json!({"protocol_version":EMBEDDED_FFI_PROTOCOL_VERSION,
            "command":{"type":"runtime","runtime_id":"test","operation":{
                "type":"host_request_complete","request_id":"test","outcome":outcome}}});
        assert_eq!(
            validator.is_valid(&request),
            accepted,
            "generated request shape mismatch: {request}"
        );
        assert_eq!(
            super::super::protocol::parse(&serde_json::to_vec(&request).unwrap()).is_ok(),
            accepted,
            "Rust request shape mismatch: {request}"
        );
    }
}

/// Keep result omission distinct from an explicit null success and reject invalid status discriminators.
/// 区分结果省略和显式空值成功，并拒绝无效状态判别。
#[test]
fn embedded_contract_response_presence_and_error_discriminators() {
    let success = &contract()["runtime_responses"]["operation_status"];
    let validator = jsonschema::validator_for(success).unwrap();
    let operation = json!({"host_effects":[],"operation_id":"operation","phase":"succeeded",
        "cancellation_requested":false,"effects":"not_applicable","value":null});
    let mut response =
        json!({"protocol_version":EMBEDDED_FFI_PROTOCOL_VERSION,"status":"ok","result":operation});
    assert!(validator.is_valid(&response));
    response["result"].as_object_mut().unwrap().remove("value");
    assert!(validator.is_valid(&response));
    response.as_object_mut().unwrap().remove("result");
    assert!(!validator.is_valid(&response));
    let error = jsonschema::validator_for(&contract()["error_response"]).unwrap();
    assert!(!error.is_valid(
        &json!({"protocol_version":EMBEDDED_FFI_PROTOCOL_VERSION,"status":"ok",
        "error":{"code":"internal","message":"wrong discriminator"}})
    ));
}
