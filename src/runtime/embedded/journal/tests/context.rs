//! Journal-level rejection of retargeted module contexts and contradictory effect authority.
//! 日志层拒绝重新指向的模块上下文及矛盾副作用权威。

use super::*;
use crate::runtime::embedded::{ModuleOperationContext, capabilities::CapabilityCaller};

/// Return a valid no-effect checkpoint with a bound module; authority exists before any callback.
/// 返回绑定模块的有效无副作用检查点；权威在任何回调前存在。
fn bound() -> OperationSnapshot {
    // The factory uses explicit host-owned identities rather than inferring them from a callback record.
    // 构造器使用明确的宿主自有身份，不从回调记录推断。
    let mut snapshot = snapshot("operation");
    snapshot.context = OperationContext::Module(Box::new(ModuleOperationContext {
        finalization_instance_id: None,
        pool_id: "original-pool".into(),
        capability_revision: "original-snapshot".into(),
        export: Some("call".into()),
        caller: CapabilityCaller {
            request_id: None,
            runtime_id: "runtime".into(),
            operation_id: "operation".into(),
            plugin_id: "plugin".into(),
            package_generation: "original-generation".into(),
            execution_revision: "original-revision".into(),
            security_partition: "original-partition".into(),
            session_id: None,
            workspace_root: None,
        },
    }));
    snapshot
}

/// A later checkpoint cannot redirect a stored operation to a different pool, revision or unbound origin.
/// 后续检查点不能将存储操作指向不同池、修订或未绑定来源。
#[test]
fn embedded_operation_context_journal_rejects_retargeting() {
    // All variants are individually valid documents; refusal must come from immutable context ownership.
    // 每个分支单独都是有效文档；拒绝必须来自不可变上下文归属。
    for field in [
        "pool",
        "capabilities",
        "export",
        "generation",
        "request",
        "unbound",
    ] {
        // Isolate each storage mutation and preserve its original exact revision.
        // 隔离每个存储变更，并保留其原始精确修订。
        let directory = Directory::new();
        // Own one real database for this attempted retargeting.
        // 为本次重新指向尝试拥有一个真实数据库。
        let journal = OperationJournal::open(&directory.database(), config()).unwrap();
        // Preserve the immutable admitted authority separately from the mutation candidate.
        // 与变更候选分开保留不可变入场权威。
        let original = bound();
        // Alter one individually valid field while retaining the original operation key.
        // 保留原操作键，同时改变一个单独有效的字段。
        let mut changed = original.clone();
        if field == "unbound" {
            changed.context = OperationContext::Unbound;
        } else if let OperationContext::Module(context) = &mut changed.context {
            match field {
                "pool" => context.pool_id = "replacement-pool".into(),
                "capabilities" => context.capability_revision = "replacement-snapshot".into(),
                "export" => context.export = Some("other".into()),
                "generation" => context.caller.package_generation = "replacement-generation".into(),
                "request" => context.caller.request_id = Some("replacement-request".into()),
                _ => unreachable!(),
            }
        }
        journal.insert("runtime", &original).unwrap();
        assert_eq!(
            journal.replace("runtime", 1, &changed).unwrap_err().code,
            EmbeddedErrorCode::InvalidArgument,
            "{field}"
        );
        // Failed replacement must leave both stored context and revision unchanged.
        // 替换失败必须使存储上下文及修订均保持不变。
        let retained = journal.get("runtime", "operation").unwrap().unwrap();
        assert_eq!(retained.revision, 1);
        assert_eq!(retained.snapshot.context, original.context);
        changed.context = original.context;
        assert_eq!(journal.replace("runtime", 1, &changed).unwrap().revision, 2);
    }
}

/// Host effects with valid runtime/operation IDs still cannot contradict the admitted module identity.
/// 即使宿主副作用具有有效运行时及操作 ID，仍不能与入场模块身份矛盾。
#[test]
fn embedded_operation_context_journal_rejects_contradictory_effects() {
    // Keep a valid bound checkpoint and inject only a different plugin generation into one callback.
    // 保持有效绑定检查点，仅向单次回调注入不同插件代次。
    let directory = Directory::new();
    // Verify insertion and replacement against the same real storage boundary.
    // 对照同一个真实存储边界验证插入及替换。
    let journal = OperationJournal::open(&directory.database(), config()).unwrap();
    // This admitted operation supplies the only accepted module authority.
    // 此入场操作提供唯一可接纳的模块权威。
    let original = bound();
    // Add contradictory callback evidence without changing the admitted context.
    // 增加矛盾回调证据，不更改入场上下文。
    let mut snapshot = original.clone();
    // Keep runtime and operation IDs valid to isolate the generation mismatch.
    // 保持运行时及操作 ID 有效，以隔离代次错配。
    let mut caller = snapshot.context.caller().unwrap().clone();
    caller.package_generation = "foreign-generation".into();
    snapshot.host_effects.push(HostEffectRecord {
        caller,
        effect_id: "effect".into(),
        registration_id: "registration".into(),
        capability_name: "test.write".into(),
        capability_version: "1.0.0".into(),
        request_id: None,
        phase: HostEffectPhase::Completed,
        effects: EffectState::Committed,
    });
    assert_eq!(
        journal.insert("runtime", &snapshot).unwrap_err().code,
        EmbeddedErrorCode::InvalidArgument
    );
    journal.insert("runtime", &original).unwrap();
    assert_eq!(
        journal.replace("runtime", 1, &snapshot).unwrap_err().code,
        EmbeddedErrorCode::InvalidArgument
    );
    assert!(
        journal
            .get("runtime", "operation")
            .unwrap()
            .unwrap()
            .snapshot
            .host_effects
            .is_empty()
    );
}
