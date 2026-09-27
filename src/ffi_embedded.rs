//! Versioned, bounded transports independent of legacy engine-wide execution locks.
//! 独立于旧引擎级执行锁的版本化有界传输。

mod commands;
mod compatibility;
mod control;
mod json;
mod protocol;
mod responses;
mod runtime;
mod transport;
mod types;
mod wire;

#[cfg(feature = "contract-generation")]
pub mod contract;

#[cfg(test)]
mod tests;

pub use compatibility::{
    EMBEDDED_CAPABILITIES, EMBEDDED_DESCRIPTION_MAX_BYTES, EMBEDDED_DESCRIPTION_VERSION,
    EmbeddedBuildIdentity, EmbeddedCoreDescription, embedded_core_description,
};
pub use types::{
    EMBEDDED_FFI_PROTOCOL_VERSION, EmbeddedFfiStatus, FfiEmbeddedResultV1,
    FfiEmbeddedTransportConfigV1,
};

use crate::ffi_standard::FfiBorrowedBuffer;
use std::panic::{AssertUnwindSafe, catch_unwind};

/// Execute one native boundary `action`; return its status without unwinding into foreign frames.
/// 执行一个原生边界 `action`；返回状态，不向外部栈帧展开。
fn boundary(action: impl FnOnce() -> Result<(), EmbeddedFfiStatus>) -> i32 {
    match catch_unwind(AssertUnwindSafe(action)) {
        Ok(Ok(())) => EmbeddedFfiStatus::Ok as i32,
        Ok(Err(status)) => status as i32,
        Err(_) => EmbeddedFfiStatus::Internal as i32,
    }
}

/// Write the independent read-only core descriptor into `description_out`; return a stable native status.
/// 将独立只读核心描述写入 `description_out`；返回稳定原生状态。
/// Success borrows immutable bytes until library unload; never pass them to any buffer-free function.
/// 成功时借用直到动态库卸载前有效的不可变字节；绝不传给任何缓冲释放函数。
/// # Safety
/// `description_out` must be writable and exclusively borrowed for one complete FfiBorrowedBuffer until return.
/// `description_out` 必须在返回前对一个完整 FfiBorrowedBuffer 可写并被独占借用。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_embedded_describe_v1(
    description_out: *mut FfiBorrowedBuffer,
) -> i32 {
    boundary(|| {
        if description_out.is_null() {
            return Err(EmbeddedFfiStatus::InvalidArgument);
        }
        // Clear valid output before any fallible serialization; failure never creates cleanup ownership.
        // 在可能失败的序列化前清空有效输出；失败绝不创建清理所有权。
        unsafe {
            description_out.write_unaligned(FfiBorrowedBuffer {
                ptr: std::ptr::null(),
                len: 0,
            })
        };
        let bytes = compatibility::description_bytes()?;
        unsafe {
            description_out.write_unaligned(FfiBorrowedBuffer {
                ptr: bytes.as_ptr(),
                len: bytes.len(),
            })
        };
        Ok(())
    })
}

/// Create an independent transport from `config`; write its exact identity to `transport_out` on success.
/// 从 `config` 创建独立传输；成功时向 `transport_out` 写入其精确身份。
/// Return a stable status; every failure leaves a valid output location zeroed.
/// 返回稳定状态；所有失败均将有效输出位置保持为零。
/// # Safety
/// `config` must expose a readable size prefix and, when the size matches, the complete structure.
/// `config` 必须提供可读大小前缀，并在大小匹配时提供完整结构。
/// `transport_out` must be writable, exclusively borrowed, and disjoint from `config` until return.
/// `transport_out` 必须可写、独占借用，并在返回前与 `config` 不重叠。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_embedded_transport_new_v1(
    config: *const FfiEmbeddedTransportConfigV1,
    transport_out: *mut u64,
) -> i32 {
    boundary(|| {
        if transport_out.is_null() {
            return Err(EmbeddedFfiStatus::InvalidArgument);
        }
        // Reset before any fallible ownership mutation so failure never leaves a stale handle.
        // 在任何可能失败的所有权变更前重置，确保失败不会留下陈旧句柄。
        unsafe { transport_out.write_unaligned(0) };
        if config.is_null() {
            return Err(EmbeddedFfiStatus::InvalidArgument);
        }
        // Read only the prefix until the caller proves the full ABI structure's extent.
        // 在调用方证明完整 ABI 结构范围前，仅读取前缀。
        let size = unsafe { config.cast::<u32>().read_unaligned() };
        if size as usize != std::mem::size_of::<FfiEmbeddedTransportConfigV1>() {
            return Err(EmbeddedFfiStatus::InvalidArgument);
        }
        let config = types::TransportConfig::try_from(unsafe { config.read_unaligned() })?;
        let id = transport::create(config)?;
        unsafe { transport_out.write_unaligned(id) };
        Ok(())
    })
}

/// Permanently close creation admission for `transport_id`; return without discarding owned results.
/// 永久关闭 `transport_id` 的创建入场；返回时不丢弃所拥有结果。
#[unsafe(no_mangle)]
pub extern "C" fn luaskills_ffi_embedded_transport_close_v1(transport_id: u64) -> i32 {
    boundary(|| transport::get(transport_id)?.request_close())
}

/// Release closed, fully drained `transport_id`; return Busy while native ownership remains.
/// 释放已关闭且完全排空的 `transport_id`；原生所有权仍存在时返回 Busy。
/// Hosts must join all calls and release results before unloading the dynamic library.
/// 宿主必须汇合全部调用并释放结果后才能卸载动态库。
#[unsafe(no_mangle)]
pub extern "C" fn luaskills_ffi_embedded_transport_free_v1(transport_id: u64) -> i32 {
    boundary(|| transport::release(transport_id))
}

/// Release `result` only from its exact `transport_id`; return a status without dereferencing its pointer.
/// 仅从其精确 `transport_id` 释放 `result`；返回状态，不解引用其指针。
/// The caller must finish every reader before release; repeated or altered descriptors are rejected.
/// 调用方必须在释放前结束所有读取者；重复或被修改的描述符被拒绝。
#[unsafe(no_mangle)]
pub extern "C" fn luaskills_ffi_embedded_result_free_v1(
    transport_id: u64,
    result: FfiEmbeddedResultV1,
) -> i32 {
    boundary(|| transport::get(transport_id)?.free_result(result))
}

/// Dispatch bounded UTF-8 `request_json` and publish one result owned by `transport_id` into `result_out`.
/// 分发有界 UTF-8 `request_json`，并向 `result_out` 发布由 `transport_id` 拥有的一个结果。
/// Return a transport status; failures leave a valid output location empty and own no returned allocation.
/// 返回传输状态；失败时有效输出位置保持为空，不拥有返回分配。
/// # Safety
/// A nonempty request must reference readable immutable bytes for its exact declared length until return.
/// 非空请求必须在返回前为其精确声明长度提供可读且不可变的字节。
/// `result_out` must be writable, exclusively borrowed, and disjoint from the request bytes until return.
/// `result_out` 必须可写、独占借用，并在返回前与请求字节不重叠。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn luaskills_ffi_embedded_request_v1(
    transport_id: u64,
    request_json: FfiBorrowedBuffer,
    result_out: *mut FfiEmbeddedResultV1,
) -> i32 {
    boundary(|| {
        if result_out.is_null() {
            return Err(EmbeddedFfiStatus::InvalidArgument);
        }
        unsafe { result_out.write_unaligned(FfiEmbeddedResultV1::default()) };
        let transport = transport::get(transport_id)?;
        if request_json.ptr.is_null()
            || request_json.len == 0
            || request_json.len > transport.config.max_request_bytes
        {
            return Err(EmbeddedFfiStatus::InvalidArgument);
        }
        // Configuration already caps readable lengths at isize::MAX before a slice can be formed.
        // 配置已在形成切片前将可读长度限制为 isize::MAX。
        let request = unsafe { std::slice::from_raw_parts(request_json.ptr, request_json.len) };
        let request = protocol::parse(request)?;
        let reservation = transport.reserve()?;
        let response = protocol::execute(&transport, request)?;
        let result = reservation.publish(response)?;
        unsafe { result_out.write_unaligned(result) };
        Ok(())
    })
}
