use std::env;
use std::fs;
use std::path::PathBuf;

use luaskills::{
    SKILL_CONFIG_DEFAULT_LOCK_TIMEOUT_MS, SKILL_CONFIG_DEFAULT_WATCH_DEBOUNCE_MS,
    SKILL_CONFIG_EVENT_QUEUE_CAPACITY, SKILL_CONFIG_FORMAT_VERSION, SKILL_CONFIG_MAX_BATCH_KEYS,
    SKILL_CONFIG_MAX_DIAGNOSTIC_VALUE_PREVIEW_BYTES, SKILL_CONFIG_MAX_DOCUMENT_BYTES,
    SKILL_CONFIG_MAX_ENUM_OPTIONS, SKILL_CONFIG_MAX_EVENT_POLL_LIMIT, SKILL_CONFIG_MAX_GROUP_BYTES,
    SKILL_CONFIG_MAX_HINT_BYTES, SKILL_CONFIG_MAX_ITEMS_PER_PACKAGE,
    SKILL_CONFIG_MAX_LOCK_TIMEOUT_MS, SKILL_CONFIG_MAX_LONG_TEXT_BYTES,
    SKILL_CONFIG_MAX_PACKAGES_PER_DOCUMENT, SKILL_CONFIG_MAX_SAFE_INTEGER,
    SKILL_CONFIG_MAX_SHORT_TEXT_BYTES, SKILL_CONFIG_MAX_STRING_CHARS,
    SKILL_CONFIG_MAX_TOOL_RESPONSE_BYTES, SKILL_CONFIG_MAX_VALUE_BYTES,
    SKILL_CONFIG_MAX_WATCH_DEBOUNCE_MS, SKILL_CONFIG_MIN_SAFE_INTEGER,
    SKILL_CONFIG_RESERVED_KEY_PREFIX,
};
use serde_json::json;

/// Generate the canonical machine-readable package configuration contract.
/// 生成规范的机器可读技能包配置契约。
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let output_path = env::args_os().nth(1).map(PathBuf::from).unwrap_or_else(|| {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("contracts")
            .join("skill-config")
            .join("v1")
            .join("contract.json")
    });
    let contract = json!({
        "contract_version": 1,
        "storage": {
            "format_version": SKILL_CONFIG_FORMAT_VERSION,
            "revision": "canonical unsigned decimal string",
            "normal_relative_path": "skills/config.json",
            "system_relative_path": "system-skills/config.json",
            "companion_lock_suffix": ".lock"
        },
        "limits": {
            "minimum_safe_integer": SKILL_CONFIG_MIN_SAFE_INTEGER,
            "maximum_safe_integer": SKILL_CONFIG_MAX_SAFE_INTEGER,
            "maximum_items_per_package": SKILL_CONFIG_MAX_ITEMS_PER_PACKAGE,
            "maximum_batch_keys": SKILL_CONFIG_MAX_BATCH_KEYS,
            "maximum_value_bytes": SKILL_CONFIG_MAX_VALUE_BYTES,
            "maximum_diagnostic_value_preview_bytes": SKILL_CONFIG_MAX_DIAGNOSTIC_VALUE_PREVIEW_BYTES,
            "maximum_string_characters": SKILL_CONFIG_MAX_STRING_CHARS,
            "maximum_enum_options": SKILL_CONFIG_MAX_ENUM_OPTIONS,
            "maximum_document_bytes": SKILL_CONFIG_MAX_DOCUMENT_BYTES,
            "maximum_tool_response_bytes": SKILL_CONFIG_MAX_TOOL_RESPONSE_BYTES,
            "maximum_packages_per_document": SKILL_CONFIG_MAX_PACKAGES_PER_DOCUMENT,
            "maximum_long_text_bytes": SKILL_CONFIG_MAX_LONG_TEXT_BYTES,
            "maximum_short_text_bytes": SKILL_CONFIG_MAX_SHORT_TEXT_BYTES,
            "maximum_group_bytes": SKILL_CONFIG_MAX_GROUP_BYTES,
            "maximum_hint_bytes": SKILL_CONFIG_MAX_HINT_BYTES,
            "reserved_key_prefix": SKILL_CONFIG_RESERVED_KEY_PREFIX,
            "event_queue_capacity": SKILL_CONFIG_EVENT_QUEUE_CAPACITY,
            "maximum_event_poll_limit": SKILL_CONFIG_MAX_EVENT_POLL_LIMIT,
            "default_lock_timeout_ms": SKILL_CONFIG_DEFAULT_LOCK_TIMEOUT_MS,
            "maximum_lock_timeout_ms": SKILL_CONFIG_MAX_LOCK_TIMEOUT_MS,
            "default_watch_debounce_ms": SKILL_CONFIG_DEFAULT_WATCH_DEBOUNCE_MS,
            "maximum_watch_debounce_ms": SKILL_CONFIG_MAX_WATCH_DEBOUNCE_MS
        },
        "declaration": {
            "types": ["integer", "string", "float", "enum", "boolean"],
            "formats": ["text", "password", "uri", "path", "file", "directory", "multiline"],
            "states": ["unset", "missing", "default", "configured", "invalid"],
            "describe_modes": ["effective", "installed"],
            "store_scopes": ["skills", "system-skills"]
        },
        "tool": {
            "name": "runtime-config",
            "actions": ["describe", "validate", "list", "get", "set", "delete", "refresh"],
            "set_forms": ["values", "key/value"],
            "event_sources": ["local_write", "external_reload"]
        },
        "errors": [
            "CONFIG_ATOMIC_REPLACE_FAILED",
            "CONFIG_BATCH_ARGUMENT_CONFLICT",
            "CONFIG_BATCH_EMPTY",
            "CONFIG_BATCH_TOO_LARGE",
            "CONFIG_DECLARATION_INVALID",
            "CONFIG_ENUM_VALUE_INVALID",
            "CONFIG_EVENT_CURSOR_EXPIRED",
            "CONFIG_EVENT_CURSOR_INVALID",
            "CONFIG_FILE_TOO_LARGE",
            "CONFIG_FORMAT_INVALID",
            "CONFIG_FORMAT_VERSION_UNSUPPORTED",
            "CONFIG_KEY_INVALID",
            "CONFIG_KEY_UNDECLARED",
            "CONFIG_LOCK_FAILED",
            "CONFIG_LOCK_TIMEOUT",
            "CONFIG_PACKAGE_NOT_FOUND",
            "CONFIG_PATH_INVALID",
            "CONFIG_PATH_UNAVAILABLE",
            "CONFIG_RELOAD_FAILED",
            "CONFIG_RESPONSE_TOO_LARGE",
            "CONFIG_REVISION_CONFLICT",
            "CONFIG_REVISION_EXHAUSTED",
            "CONFIG_REVISION_INVALID",
            "CONFIG_REVISION_REGRESSION",
            "CONFIG_SNAPSHOT_UNAVAILABLE",
            "CONFIG_VALIDATOR_FAILED",
            "CONFIG_VALIDATOR_LIMIT_EXCEEDED",
            "CONFIG_VALIDATOR_TIMEOUT",
            "CONFIG_VALIDATOR_UNAVAILABLE",
            "CONFIG_VALUE_OUT_OF_RANGE",
            "CONFIG_VALUE_TOO_LONG",
            "CONFIG_VALUE_TYPE_INVALID",
            "CONFIG_WATCHER_FAILED"
        ]
    });
    let serialized = serde_json::to_string_pretty(&contract)? + "\n";
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(output_path, serialized)?;
    Ok(())
}
