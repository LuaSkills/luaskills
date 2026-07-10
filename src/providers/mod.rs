use std::ffi::{CStr, c_char};

/// Decode one non-null FFI C string pointer into owned Rust text.
/// 将单个非空 FFI C 字符串指针解码为 Rust 拥有的文本。
///
/// The ptr parameter must point to a valid NUL-terminated C string for the duration of this call.
/// ptr 参数必须在本次调用期间指向有效的 NUL 结尾 C 字符串。
///
/// Return the decoded Rust string, or an explicit UTF-8 decoding error.
/// 返回解码后的 Rust 字符串，或明确的 UTF-8 解码错误。
///
/// # Safety
/// # 安全性
///
/// The caller must guarantee that ptr is non-null, valid, and not freed until decoding finishes.
/// 调用方必须保证 ptr 非空、有效，并且在解码完成前不会被释放。
pub(crate) unsafe fn decode_non_null_ffi_c_string(ptr: *const c_char) -> Result<String, String> {
    unsafe { CStr::from_ptr(ptr) }
        .to_str()
        .map(str::to_string)
        .map_err(|error| format!("provider FFI string is not valid UTF-8: {error}"))
}

/// Decode one optional provider-owned last-error C string for diagnostics.
/// 解码单个可选 provider 拥有的 last-error C 字符串，用于诊断。
///
/// The ptr parameter is the provider-returned pointer, or null when no error text is available.
/// ptr 参数是 provider 返回的指针；没有错误文本时可为 null。
///
/// The provider_name parameter identifies the provider in UTF-8 decoding diagnostics.
/// provider_name 参数用于在 UTF-8 解码诊断中标识 provider。
///
/// The unknown_message parameter is returned when the provider supplies a null pointer.
/// unknown_message 参数会在 provider 返回空指针时作为结果返回。
///
/// Return the provider error text, or an explicit diagnostic when the provider text is not UTF-8.
/// 返回 provider 错误文本；当 provider 文本不是 UTF-8 时返回显式诊断。
pub(crate) unsafe fn decode_provider_last_error_message(
    ptr: *const c_char,
    provider_name: &str,
    unknown_message: &str,
) -> String {
    if ptr.is_null() {
        return unknown_message.to_string();
    }
    unsafe { decode_non_null_ffi_c_string(ptr) }.unwrap_or_else(|error| {
        format!("{provider_name} provider error message is not valid UTF-8: {error}")
    })
}

pub(crate) mod lancedb;
pub(crate) mod sqlite;

#[cfg(test)]
mod tests {
    use super::{decode_non_null_ffi_c_string, decode_provider_last_error_message};
    use std::ffi::CString;

    /// Verify provider FFI C strings decode exact UTF-8 text.
    /// 验证 provider FFI C 字符串会按精确 UTF-8 文本解码。
    #[test]
    fn provider_ffi_c_string_decodes_valid_utf8() {
        // Valid C string used to prove ordinary provider text still decodes.
        // 用于证明普通 provider 文本仍可解码的有效 C 字符串。
        let value = CString::new("provider-ok").expect("build valid provider string");
        // Decoded text returned by the strict provider FFI string helper.
        // 由严格 provider FFI 字符串辅助函数返回的解码文本。
        let decoded = unsafe { decode_non_null_ffi_c_string(value.as_ptr()) }
            .expect("valid provider string should decode");

        assert_eq!(decoded, "provider-ok");
    }

    /// Verify provider FFI C strings reject invalid UTF-8 instead of lossy replacement.
    /// 验证 provider FFI C 字符串会拒绝非法 UTF-8，而不是执行有损替换。
    #[test]
    fn provider_ffi_c_string_rejects_invalid_utf8() {
        // Invalid UTF-8 C string used to prove lossy replacement is not accepted.
        // 用于证明不会接受有损替换的非法 UTF-8 C 字符串。
        let value = CString::new(vec![0xff]).expect("build invalid provider string");
        // Decode error returned by the strict provider FFI string helper.
        // 由严格 provider FFI 字符串辅助函数返回的解码错误。
        let error = unsafe { decode_non_null_ffi_c_string(value.as_ptr()) }
            .expect_err("invalid provider string should fail");

        assert!(error.contains("not valid UTF-8"));
    }

    /// Verify provider last-error decoding labels invalid UTF-8 as a diagnostic failure.
    /// 验证 provider last-error 解码会把非法 UTF-8 标记为诊断失败。
    #[test]
    fn provider_last_error_message_marks_invalid_utf8() {
        // Invalid UTF-8 provider error text returned from one dynamic library boundary.
        // 从动态库边界返回的非法 UTF-8 provider 错误文本。
        let value = CString::new(vec![0xff]).expect("build invalid provider error string");
        // Decoded diagnostic returned for a malformed provider error message.
        // 格式错误的 provider 错误消息对应的解码诊断。
        let decoded = unsafe {
            decode_provider_last_error_message(value.as_ptr(), "SQLite", "unknown provider error")
        };

        assert!(decoded.contains("SQLite provider error message is not valid UTF-8"));
    }

    /// Verify provider last-error decoding uses the unknown message for null pointers.
    /// 验证 provider last-error 解码会对空指针使用 unknown 消息。
    #[test]
    fn provider_last_error_message_uses_unknown_message_for_null() {
        // Decoded diagnostic returned when the provider exposes no error string pointer.
        // provider 未暴露错误字符串指针时返回的解码诊断。
        let decoded = unsafe {
            decode_provider_last_error_message(
                std::ptr::null(),
                "SQLite",
                "unknown SQLite host error",
            )
        };

        assert_eq!(decoded, "unknown SQLite host error");
    }
}
