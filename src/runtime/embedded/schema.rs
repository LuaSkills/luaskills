use super::{EmbeddedError, EmbeddedResult};
use serde_json::Value;
use std::sync::Arc;

/// Single authoritative dialect for new embedded-runtime function contracts.
/// 新嵌入式运行时函数契约的唯一权威方言。
pub const EMBEDDED_SCHEMA_DIALECT: &str = "https://json-schema.org/draft/2020-12/schema";

/// Block ambient retrieval even if a consuming crate enables extra dependency features.
/// 即使消费 crate 启用额外依赖功能，也阻止环境检索。
struct NoExternalSchemas;

impl jsonschema::Retrieve for NoExternalSchemas {
    /// Reject external `uri`; all contract references must be bundled locally in the schema.
    /// 拒绝外部 `uri`；所有契约引用必须在 Schema 内本地打包。
    fn retrieve(
        &self,
        _uri: &jsonschema::Uri<String>,
    ) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
        Err(std::io::Error::other(
            "external schema retrieval is disabled; bundle references inside the contract",
        )
        .into())
    }
}

/// Immutable compiled JSON contract shared by VMs in an activation generation.
/// 由同一激活代次的 VM 共享的不可变已编译 JSON 契约。
#[derive(Clone)]
pub struct JsonContract {
    /// Compiled validator; source schemas remain in the module declaration.
    /// 已编译校验器；源 Schema 保留在模块声明内。
    validator: Arc<jsonschema::Validator>,
}

impl JsonContract {
    /// Compile `schema` with the declared dialect and offline reference resolution.
    /// 使用声明的方言与离线引用解析编译 `schema`。
    /// Return an error for unsupported dialects or invalid contracts before execution.
    /// 在执行前，对不支持的方言或无效契约返回错误。
    pub fn compile(schema: &Value) -> EmbeddedResult<Self> {
        if let Some(dialect) = schema.get("$schema")
            && dialect.as_str() != Some(EMBEDDED_SCHEMA_DIALECT)
        {
            return Err(EmbeddedError::invalid(
                "embedded contracts require JSON Schema Draft 2020-12",
            ));
        }
        // Linear-time patterns avoid an uninterruptible backtracking validation stage.
        // 线性时间模式避免出现不可中断的回溯校验阶段。
        let validator = jsonschema::options()
            .with_draft(jsonschema::Draft::Draft202012)
            .with_retriever(NoExternalSchemas)
            .with_pattern_options(jsonschema::PatternOptions::regex())
            .build(schema)
            .map_err(|error| {
                EmbeddedError::invalid(format!(
                    "invalid JSON contract at schema path {}",
                    error.schema_path()
                ))
            })?;
        Ok(Self {
            validator: Arc::new(validator),
        })
    }

    /// Validate `value`, returning paths so rejected secrets cannot enter error text.
    /// 校验 `value`，返回路径以防被拒绝的秘密进入错误文案。
    pub fn validate(&self, value: &Value) -> EmbeddedResult<()> {
        self.validator.validate(value).map_err(|error| {
            EmbeddedError::invalid(format!(
                "contract violation at instance path {} and schema path {}",
                error.instance_path(),
                error.schema_path(),
            ))
        })
    }
}
