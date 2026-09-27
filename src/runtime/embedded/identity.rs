/// One authority for opaque core identities whose numeric suffixes are never reused.
/// 数字后缀绝不复用的不透明核心身份的唯一权威。
#[derive(Clone, Copy)]
pub(crate) enum IdentityKind {
    /// Registered immutable pool domain.
    /// 已注册不可变池域。
    Pool,
    /// Fixed-instance session.
    /// 固定实例会话。
    Session,
    /// Retained asynchronous operation.
    /// 保留的异步操作。
    Operation,
    /// Exact host capability registration.
    /// 精确宿主能力注册。
    Capability,
}

impl IdentityKind {
    /// Render exact `runtime_id` and checked `sequence`; consumers must not parse the resulting opaque string.
    /// 渲染精确 `runtime_id` 与已检查 `sequence`；消费者不得解析所得不透明字符串。
    pub(crate) fn render(self, runtime_id: &str, sequence: u64) -> String {
        let kind = match self {
            Self::Pool => "pool",
            Self::Session => "session",
            Self::Operation => "op",
            Self::Capability => "cap",
        };
        format!("{runtime_id}:{kind}:{sequence}")
    }

    /// Render the longest possible identity for this exact namespace using the same formation authority.
    /// 使用相同生成权威，为此精确命名空间渲染最长可能身份。
    /// FFI serializers use it to reserve success output before a mutation can issue an identity.
    /// FFI 序列化器使用它，在变更能够签发身份前预留成功输出。
    pub(crate) fn longest(self, runtime_id: &str) -> String {
        self.render(runtime_id, u64::MAX)
    }
}
