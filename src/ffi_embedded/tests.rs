use super::*;
use std::sync::{Arc, Barrier};

mod commands;
pub(super) mod runtimes;

/// Build explicit small budgets for native transport ownership tests.
/// 构造显式小预算，用于原生传输所有权测试。
fn config() -> FfiEmbeddedTransportConfigV1 {
    FfiEmbeddedTransportConfigV1 {
        struct_size: std::mem::size_of::<FfiEmbeddedTransportConfigV1>() as u32,
        protocol_version: EMBEDDED_FFI_PROTOCOL_VERSION,
        max_runtimes: 2,
        max_result_buffers: 2,
        max_result_bytes: 4096,
        max_response_bytes: 2048,
        max_request_bytes: 1024,
    }
}

/// Create one native transport from `config`; require an exact nonzero identity.
/// 从 `config` 创建一个原生传输；要求获得精确非零身份。
fn create(config: FfiEmbeddedTransportConfigV1) -> u64 {
    let mut id = 0;
    assert_eq!(
        unsafe { luaskills_ffi_embedded_transport_new_v1(&config, &mut id) },
        0
    );
    assert_ne!(id, 0);
    id
}

/// Send borrowed `bytes` to `id` and return the native status and output without reading failed output.
/// 向 `id` 发送借用 `bytes`，返回原生状态与输出，不读取失败输出。
fn request(id: u64, bytes: &[u8]) -> (i32, FfiEmbeddedResultV1) {
    let mut result = FfiEmbeddedResultV1::default();
    let status = unsafe {
        luaskills_ffi_embedded_request_v1(
            id,
            FfiBorrowedBuffer {
                ptr: bytes.as_ptr(),
                len: bytes.len(),
            },
            &mut result,
        )
    };
    (status, result)
}

/// Obtain one describe result from `id`; the caller owns its native allocation until release.
/// 从 `id` 获取一个描述结果；调用方在释放前拥有其原生分配。
fn describe(id: u64) -> FfiEmbeddedResultV1 {
    let (status, result) = request(
        id,
        br#"{"protocol_version":1,"command":{"type":"describe"}}"#,
    );
    assert_eq!(status, 0);
    assert!(!result.ptr.is_null());
    assert_ne!(result.allocation_id, 0);
    result
}

/// Close and release `id`, requiring actual ownership drainage.
/// 关闭并释放 `id`，要求实际所有权排空。
fn finish(id: u64) {
    assert_eq!(luaskills_ffi_embedded_transport_close_v1(id), 0);
    assert_eq!(luaskills_ffi_embedded_transport_free_v1(id), 0);
}

/// Verify version discovery, immutable results, explicit close, and stale transport rejection through C entrypoints.
/// 通过 C 入口验证版本发现、不可变结果、显式关闭以及陈旧传输拒绝。
#[test]
fn ffi_embedded_describe_and_close_preserve_result_ownership() {
    let id = create(config());
    let result = describe(id);
    let json: serde_json::Value =
        serde_json::from_slice(unsafe { std::slice::from_raw_parts(result.ptr, result.len) })
            .unwrap();
    assert_eq!(json["result"]["core_version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(json["result"]["limits"]["max_result_buffers"], 2);
    assert_eq!(
        luaskills_ffi_embedded_transport_free_v1(id),
        EmbeddedFfiStatus::Busy as i32
    );
    assert_eq!(luaskills_ffi_embedded_transport_close_v1(id), 0);
    assert_eq!(
        luaskills_ffi_embedded_transport_free_v1(id),
        EmbeddedFfiStatus::Busy as i32
    );
    // Closing retains diagnostic admission and keeps prior bytes alive until their explicit release.
    // 正在关闭保留诊断入场，并在显式释放前保持此前字节有效。
    let second = describe(id);
    assert_eq!(
        unsafe { std::slice::from_raw_parts(result.ptr, result.len) }[0],
        b'{'
    );
    assert_eq!(luaskills_ffi_embedded_result_free_v1(id, second), 0);
    assert_eq!(luaskills_ffi_embedded_result_free_v1(id, result), 0);
    finish(id);
    assert_eq!(
        luaskills_ffi_embedded_transport_close_v1(id),
        EmbeddedFfiStatus::NotFound as i32
    );
}

/// Reject repeated, foreign and altered result descriptors without dereferencing untrusted addresses.
/// 拒绝重复、外来和被修改的结果描述符，且不解引用不可信地址。
#[test]
fn ffi_embedded_result_identity_prevents_double_and_foreign_free() {
    let id = create(config());
    let other = create(config());
    let result = describe(id);
    assert_eq!(
        luaskills_ffi_embedded_result_free_v1(other, result),
        EmbeddedFfiStatus::NotFound as i32
    );
    let mut altered = result;
    altered.ptr = std::ptr::dangling();
    assert_eq!(
        luaskills_ffi_embedded_result_free_v1(id, altered),
        EmbeddedFfiStatus::InvalidArgument as i32
    );
    altered = result;
    altered.len += 1;
    assert_eq!(
        luaskills_ffi_embedded_result_free_v1(id, altered),
        EmbeddedFfiStatus::InvalidArgument as i32
    );
    assert_eq!(luaskills_ffi_embedded_result_free_v1(id, result), 0);
    assert_eq!(
        luaskills_ffi_embedded_result_free_v1(id, result),
        EmbeddedFfiStatus::NotFound as i32
    );
    let replacement = describe(id);
    assert_ne!(replacement.allocation_id, result.allocation_id);
    assert_eq!(
        luaskills_ffi_embedded_result_free_v1(id, result),
        EmbeddedFfiStatus::NotFound as i32
    );
    assert_eq!(luaskills_ffi_embedded_result_free_v1(id, replacement), 0);
    finish(id);
    finish(other);
}

/// Verify both count and aggregate byte admission using actual C result owners.
/// 使用实际 C 结果所有者验证数量与聚合字节入场。
#[test]
fn ffi_embedded_result_limits_recover_only_after_release() {
    for count_limited in [true, false] {
        let mut limits = config();
        if count_limited {
            limits.max_result_buffers = 1;
        } else {
            limits.max_result_bytes = limits.max_response_bytes;
        }
        let id = create(limits);
        let result = describe(id);
        let (status, rejected) = request(
            id,
            br#"{"protocol_version":1,"command":{"type":"describe"}}"#,
        );
        assert_eq!(status, EmbeddedFfiStatus::CapacityExceeded as i32);
        assert!(rejected.ptr.is_null());
        assert_eq!(rejected.allocation_id, 0);
        assert_eq!(luaskills_ffi_embedded_result_free_v1(id, result), 0);
        let result = describe(id);
        assert_eq!(luaskills_ffi_embedded_result_free_v1(id, result), 0);
        finish(id);
    }
}

/// Reject malformed, ambiguous and undeclared requests without consuming result ownership.
/// 拒绝畸形、歧义及未声明请求，且不消费结果所有权。
#[test]
fn ffi_embedded_protocol_rejects_unknown_duplicate_and_invalid_utf8() {
    let id = create(config());
    let invalid: &[&[u8]] = &[
        b"",
        b"\xff",
        b"{}",
        br#"{"protocol_version":1,"command":{"type":"unknown"}}"#,
        br#"{"protocol_version":1,"command":{"type":"describe","unexpected":true}}"#,
        br#"{"protocol_version":1,"command":{"type":"describe"},"unexpected":true}"#,
        br#"{"protocol_version":1,"protocol_version":1,"command":{"type":"describe"}}"#,
        br#"{"protocol_version":1,"command":{"type":"describe","type":"describe"}}"#,
        br#"{"protocol_version":1,"command":{"type":"describe"}}{}"#,
    ];
    for bytes in invalid {
        let (status, result) = request(id, bytes);
        assert_eq!(
            status,
            EmbeddedFfiStatus::InvalidArgument as i32,
            "request: {bytes:?}"
        );
        assert!(result.ptr.is_null());
        assert_eq!(result.len, 0);
    }
    assert_eq!(
        request(
            id,
            br#"{"protocol_version":2,"command":{"type":"describe"}}"#
        )
        .0,
        EmbeddedFfiStatus::Unsupported as i32
    );
    let result = describe(id);
    assert_eq!(luaskills_ffi_embedded_result_free_v1(id, result), 0);
    finish(id);
}

/// Check null output/input shapes and excessive length before foreign memory can be sliced.
/// 在形成外部内存切片前检查空输出／输入形状与超长长度。
#[test]
fn ffi_embedded_pointer_shapes_fail_before_dereference() {
    let id = create(config());
    let mut result = FfiEmbeddedResultV1::default();
    for input in [
        FfiBorrowedBuffer {
            ptr: std::ptr::null(),
            len: 1,
        },
        FfiBorrowedBuffer {
            ptr: std::ptr::dangling(),
            len: usize::MAX,
        },
        FfiBorrowedBuffer {
            ptr: std::ptr::dangling(),
            len: 0,
        },
    ] {
        assert_eq!(
            unsafe { luaskills_ffi_embedded_request_v1(id, input, &mut result) },
            EmbeddedFfiStatus::InvalidArgument as i32
        );
        assert!(result.ptr.is_null());
    }
    assert_eq!(
        unsafe {
            luaskills_ffi_embedded_request_v1(
                id,
                FfiBorrowedBuffer {
                    ptr: std::ptr::null(),
                    len: 0,
                },
                std::ptr::null_mut(),
            )
        },
        EmbeddedFfiStatus::InvalidArgument as i32
    );
    finish(id);
}

/// Validate declared ABI extent before reading the full structure and reject unsupported or overflowing budgets.
/// 在读取完整结构前校验声明 ABI 范围，并拒绝不支持或溢出的预算。
#[test]
fn ffi_embedded_configuration_is_explicit_and_prefix_checked() {
    let mut id = 999;
    let short_prefix = 4_u32;
    assert_eq!(
        unsafe {
            luaskills_ffi_embedded_transport_new_v1((&short_prefix as *const u32).cast(), &mut id)
        },
        EmbeddedFfiStatus::InvalidArgument as i32
    );
    assert_eq!(id, 0);
    assert_eq!(
        unsafe { luaskills_ffi_embedded_transport_new_v1(std::ptr::null(), &mut id) },
        EmbeddedFfiStatus::InvalidArgument as i32
    );
    assert_eq!(
        unsafe { luaskills_ffi_embedded_transport_new_v1(&config(), std::ptr::null_mut()) },
        EmbeddedFfiStatus::InvalidArgument as i32
    );
    for selector in 0..7 {
        let mut value = config();
        match selector {
            0 => value.protocol_version = 2,
            1 => value.max_runtimes = 0,
            2 => value.max_result_buffers = 0,
            3 => value.max_result_bytes = 0,
            4 => value.max_response_bytes = value.max_result_bytes + 1,
            5 => value.max_request_bytes = u64::MAX,
            6 => value.max_response_bytes = 0,
            _ => unreachable!(),
        }
        let expected = if selector == 0 {
            EmbeddedFfiStatus::Unsupported
        } else {
            EmbeddedFfiStatus::InvalidArgument
        };
        assert_eq!(
            unsafe { luaskills_ffi_embedded_transport_new_v1(&value, &mut id) },
            expected as i32
        );
        assert_eq!(id, 0);
    }
}

/// Ensure serialization overflow relinquishes every reservation and permits actual transport release.
/// 确保序列化超限放弃全部预留，并允许实际释放传输。
#[test]
fn ffi_embedded_oversized_response_does_not_leak_admission() {
    let mut limits = config();
    limits.max_response_bytes = 1;
    let id = create(limits);
    for _ in 0..3 {
        let (status, result) = request(
            id,
            br#"{"protocol_version":1,"command":{"type":"describe"}}"#,
        );
        assert_eq!(status, EmbeddedFfiStatus::CapacityExceeded as i32);
        assert!(result.ptr.is_null());
    }
    finish(id);
}

/// Prove a panic across an admitted action returns Internal and relinquishes the pending reservation.
/// 证明已接纳操作中的 panic 返回 Internal，并放弃待完成预留。
#[test]
fn ffi_embedded_panic_boundary_releases_unpublished_reservation() {
    let id = create(config());
    let transport = transport::get(id).unwrap();
    assert_eq!(
        boundary(|| {
            let _reservation = transport.reserve()?;
            panic!("intentional boundary unwind");
        }),
        EmbeddedFfiStatus::Internal as i32
    );
    finish(id);
    assert!(matches!(
        transport.reserve(),
        Err(EmbeddedFfiStatus::Closed)
    ));
}

/// Hold a concurrent admitted request across close/free; publication still owns a live result until release.
/// 跨关闭／释放保持并发已接纳请求；发布后仍拥有有效结果直到释放。
#[test]
fn ffi_embedded_concurrent_close_cannot_release_pending_or_published_owner() {
    let id = create(config());
    let transport = transport::get(id).unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let other = Arc::clone(&barrier);
    let worker = std::thread::spawn(move || {
        let reservation = transport.reserve().unwrap();
        other.wait();
        other.wait();
        let result = reservation.publish(b"{}".to_vec()).unwrap();
        other.wait();
        other.wait();
        assert_eq!(luaskills_ffi_embedded_result_free_v1(id, result), 0);
    });
    barrier.wait();
    assert_eq!(luaskills_ffi_embedded_transport_close_v1(id), 0);
    assert_eq!(
        luaskills_ffi_embedded_transport_free_v1(id),
        EmbeddedFfiStatus::Busy as i32
    );
    barrier.wait();
    barrier.wait();
    assert_eq!(
        luaskills_ffi_embedded_transport_free_v1(id),
        EmbeddedFfiStatus::Busy as i32
    );
    barrier.wait();
    worker.join().unwrap();
    finish(id);
}
