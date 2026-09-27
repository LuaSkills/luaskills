use super::*;
use serde_json::{Value, json};

/// Return the shared core-owned corpus; IDs remain stable across SDK copies.
/// 返回核心拥有的共享语料；身份在 SDK 副本之间保持稳定。
fn vectors() -> Value {
    serde_json::from_str(include_str!(
        "../../../contracts/embedded/v1/json-vectors.json"
    ))
    .unwrap()
}

/// Embed exact `value` bytes in a real completion request without normalizing their contents.
/// 将精确 `value` 字节嵌入真实完成请求，不归一化其内容。
fn completion(value: &[u8]) -> Vec<u8> {
    // The absent runtime proves parsing precedes object lookup; this helper performs no dispatch.
    // 缺失运行时用于证明解析先于对象查找；此辅助函数不执行分发。
    let mut request = br#"{"protocol_version":1,"command":{"type":"runtime","runtime_id":"absent","operation":{"type":"host_request_complete","request_id":"absent","outcome":{"ok":true,"value":"#.to_vec();
    request.extend_from_slice(value);
    request.extend_from_slice(br#", "effects":"committed"}}}}"#);
    request
}

/// Describe `value` without losing integer intent, floating-point bits or explicit collection shapes.
/// 描述 `value`，保留整数意图、浮点位及显式集合形状。
fn fingerprint(value: &Value) -> Value {
    match value {
        Value::Null => json!(["null"]),
        Value::Bool(value) => json!(["boolean", value]),
        Value::String(value) => json!(["string", value]),
        Value::Number(value) if value.is_f64() => {
            json!([
                "float",
                format!("{:016x}", value.as_f64().unwrap().to_bits())
            ])
        }
        Value::Number(value) => json!(["integer", value.to_string()]),
        Value::Array(values) => {
            json!(["array", values.iter().map(fingerprint).collect::<Vec<_>>()])
        }
        Value::Object(values) => json!([
            "object",
            values
                .iter()
                .map(|(key, value)| (key.clone(), fingerprint(value)))
                .collect::<serde_json::Map<_, _>>()
        ]),
    }
}

/// Check shared valid values against the real typed request parser and independent semantic fingerprints.
/// 对照真实类型化请求解析器及独立语义指纹检查共享有效值。
#[test]
fn ffi_embedded_shared_json_values() {
    let corpus = vectors();
    assert_eq!(corpus["version"], 1);
    for case in corpus["valid"].as_array().unwrap() {
        let raw = case["json"].as_str().unwrap().as_bytes();
        assert!(protocol::parse(&completion(raw)).is_ok(), "{}", case["id"]);
        let decoded: Value = serde_json::from_slice(raw).unwrap();
        assert_eq!(fingerprint(&decoded), case["expected"], "{}", case["id"]);
    }
}

/// Reject every shared invalid value before arbitrary completion payloads can hide duplicate members.
/// 在任意完成载荷能够隐藏重复成员前，拒绝全部共享无效值。
#[test]
fn ffi_embedded_shared_json_rejections() {
    let corpus = vectors();
    // Collect all failures so the first regression run records the complete disagreement set.
    // 收集全部失败，使首次回归运行记录完整不一致集合。
    let accepted: Vec<_> = corpus["invalid"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|case| {
            protocol::parse(&completion(case["json"].as_str().unwrap().as_bytes())).is_ok()
        })
        .map(|case| case["id"].as_str().unwrap())
        .collect();
    assert!(
        accepted.is_empty(),
        "accepted shared invalid values: {accepted:?}"
    );
    // The real C boundary must reject the same values without publishing any response allocation.
    // 真实 C 边界必须拒绝相同值，且不发布任何响应分配。
    let id = create(config());
    for case in corpus["invalid"].as_array().unwrap() {
        let bytes = completion(case["json"].as_str().unwrap().as_bytes());
        let (status, result) = request(id, &bytes);
        assert_eq!(
            status,
            EmbeddedFfiStatus::InvalidArgument as i32,
            "{}",
            case["id"]
        );
        assert_eq!(result.allocation_id, 0);
        assert!(result.ptr.is_null());
        assert_eq!(result.len, 0);
    }
    finish(id);
}

/// Reject shared invalid UTF-8 bytes at the same pre-dispatch validation boundary.
/// 在同一分发前校验边界拒绝共享无效 UTF-8 字节。
#[test]
fn ffi_embedded_shared_json_bytes() {
    let corpus = vectors();
    for case in corpus["invalid_bytes"].as_array().unwrap() {
        let bytes: Vec<u8> = case["hex"]
            .as_str()
            .unwrap()
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
            .collect();
        assert!(
            protocol::parse(&completion(&bytes)).is_err(),
            "{}",
            case["id"]
        );
    }
}
