//! Original caller preservation and rejection of missing or contradictory durable authority.
//! 原始调用方保留以及缺失或矛盾持久权威的拒绝。

use super::*;
use crate::runtime::embedded::capabilities::CapabilityCaller;

/// Find the exact fixture effect in `snapshot`; return its immutable historical evidence.
/// 在 `snapshot` 中查找精确夹具副作用；返回其不可变历史证据。
fn effect(snapshot: &OperationSnapshot) -> &HostEffectRecord {
    snapshot
        .host_effects
        .iter()
        .find(|record| record.effect_id == "original-effect")
        .expect("original fixture effect is missing; fixture identities may have changed")
}

/// Find the exact fixture effect in `snapshot`; return its mutable evidence for deliberate fault injection.
/// 在 `snapshot` 中查找精确夹具副作用；返回用于有意故障注入的可变证据。
fn effect_mut(snapshot: &mut OperationSnapshot) -> &mut HostEffectRecord {
    snapshot
        .host_effects
        .iter_mut()
        .find(|record| record.effect_id == "original-effect")
        .expect("original fixture effect is missing; fixture identities may have changed")
}

/// Build a committed effect with explicit historical identity for mutation and reopen checks.
/// 构造带明确历史身份的已提交副作用，用于变更及重新打开检查。
/// Return an unfinished operation so retained evidence cannot be mistaken for permission to replay it.
/// 返回未结束操作，避免将保留证据误解为重放许可。
fn original() -> OperationSnapshot {
    // Keep identity separate from the operation's business value and any current registration.
    // 将身份与操作业务值及任何当前注册分开。
    let mut record = snapshot("operation");
    record.host_effects.push(HostEffectRecord {
        caller: CapabilityCaller {
            runtime_id: "runtime".into(),
            operation_id: "operation".into(),
            plugin_id: "original-plugin".into(),
            package_generation: "original-package".into(),
            execution_revision: "original-policy".into(),
            security_partition: "original-partition".into(),
            session_id: Some("original-session".into()),
            workspace_root: Some("D:/用户/workspace".into()),
        },
        effect_id: "original-effect".into(),
        registration_id: "original-registration".into(),
        capability_name: "test.write".into(),
        capability_version: "1.0.0".into(),
        request_id: Some("original-request".into()),
        phase: HostEffectPhase::Completed,
        effects: EffectState::Committed,
    });
    record
}

/// Reopening keeps every old authority field even when the caller's mutable input is later changed.
/// 即使调用方稍后修改可变输入，重新打开仍保留全部旧权威字段。
#[test]
fn embedded_journal_caller_survives_reopen_without_current_plugin() {
    // No capability registry or module exists when this history is reopened.
    // 重新打开此历史时不存在能力注册表或模块。
    let directory = Directory::new();
    // Preserve an independent expected snapshot before any host-side mutation.
    // 在任何宿主侧变更前保留独立预期快照。
    let expected = original();
    // The supplied value remains host-owned after insertion and must not alias durable state.
    // 插入后提供值仍归宿主所有，不得与持久状态共享可变引用。
    let mut supplied = expected.clone();
    {
        // Hold the sole live storage owner only for the initial insert.
        // 仅为初始插入持有唯一活动存储所有者。
        let journal = OperationJournal::open(&directory.database(), config()).unwrap();
        journal.insert("runtime", &supplied).unwrap();
        effect_mut(&mut supplied).caller.package_generation = "replacement-package".into();
    }
    // A newly opened connection has no access to the old caller's mutable input.
    // 新打开连接无法访问旧调用方的可变输入。
    let journal = OperationJournal::open(&directory.database(), config()).unwrap();
    // Read the exact original identity from disk without any active plugin lookup.
    // 从磁盘读取精确原始身份，不查询任何活动插件。
    let recovered = journal.get("runtime", "operation").unwrap().unwrap();
    assert_eq!(effect(&recovered.snapshot).caller, effect(&expected).caller);
    assert_ne!(effect(&recovered.snapshot).caller, effect(&supplied).caller);
    assert_eq!(recovered.snapshot.phase, OperationPhase::Running);
    assert_eq!(effect(&recovered.snapshot).effects, EffectState::Committed);
}

/// Mismatched operation/runtime identity and invalid authority fail before either insertion or replacement commits.
/// 操作或运行时身份错配及无效权威在插入或替换提交前失败。
#[test]
fn embedded_journal_caller_mismatch_rejects_mutation() {
    // Exercise each independent identity rejection through the actual storage entrypoints.
    // 通过真实存储入口分别检验每个独立身份拒绝条件。
    for field in ["runtime", "operation", "generation"] {
        // Isolate each rejected mutation from the others and retain the exact initial revision.
        // 将每次被拒变更与其他变更隔离，并保留精确初始修订。
        let directory = Directory::new();
        // The database starts empty for the insertion rejection check.
        // 数据库在插入拒绝检查开始时为空。
        let journal = OperationJournal::open(&directory.database(), config()).unwrap();
        // Retain a valid original for the subsequent replacement rejection check.
        // 保留有效原始值，用于随后的替换拒绝检查。
        let original = original();
        // Mutate exactly one authority field while preserving the other evidence.
        // 仅变更一个权威字段，同时保留其他证据。
        let mut invalid = original.clone();
        match field {
            "runtime" => effect_mut(&mut invalid).caller.runtime_id = "different".into(),
            "operation" => effect_mut(&mut invalid).caller.operation_id = "different".into(),
            "generation" => effect_mut(&mut invalid).caller.package_generation.clear(),
            _ => unreachable!(),
        }
        assert_eq!(
            journal.insert("runtime", &invalid).unwrap_err().code,
            EmbeddedErrorCode::InvalidArgument,
            "{field}"
        );
        assert!(journal.get("runtime", "operation").unwrap().is_none());
        journal.insert("runtime", &original).unwrap();
        assert_eq!(
            journal.replace("runtime", 1, &invalid).unwrap_err().code,
            EmbeddedErrorCode::InvalidArgument,
            "{field}"
        );
        // A rejected replacement must preserve both the original revision and caller.
        // 被拒替换必须同时保留原始修订及调用方。
        let retained = journal.get("runtime", "operation").unwrap().unwrap();
        assert_eq!(retained.revision, 1);
        assert_eq!(effect(&retained.snapshot).caller, effect(&original).caller);
    }
}

/// A valid checksum cannot make missing or mismatched caller identity acceptable; schema-one files remain untouched.
/// 有效摘要不能使缺失或错配的调用身份可被接纳；第一版文件保持原字节。
#[test]
fn embedded_journal_caller_corruption_and_legacy_schema_are_preserved() {
    // Cover malformed current documents separately from the exact unsupported historical format.
    // 分别覆盖当前畸形文档及精确不支持历史格式。
    for damage in ["missing", "runtime", "operation", "legacy"] {
        // Change persisted bytes deliberately after releasing the actual journal owner.
        // 释放真实日志所有者后，有意修改持久字节。
        let directory = Directory::new();
        // Begin with a real committed record before faulting its serialized document.
        // 从真实已提交记录开始，再对其序列化文档注入故障。
        let mut encoded = {
            // Release the exclusive owner before using an independent corruption connection.
            // 使用独立损坏注入连接前释放独占所有者。
            let journal = OperationJournal::open(&directory.database(), config()).unwrap();
            serde_json::to_value(journal.insert("runtime", &original()).unwrap()).unwrap()
        };
        // Select the sole fixture effect by identity, not a production array position.
        // 按身份选取唯一夹具副作用，不依赖生产数组位置。
        let effect = encoded["snapshot"]["host_effects"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|effect| effect["effect_id"] == "original-effect")
            .unwrap();
        match damage {
            "missing" | "legacy" => {
                effect.as_object_mut().unwrap().remove("caller");
            }
            "runtime" => effect["caller"]["runtime_id"] = json!("different"),
            "operation" => effect["caller"]["operation_id"] = json!("different"),
            _ => unreachable!(),
        }
        {
            // This test-only connection deliberately bypasses the production identity checks.
            // 此测试专用连接有意绕过生产身份检查。
            let connection = Connection::open(directory.database()).unwrap();
            // Recompute the checksum so rejection proves identity validation, not checksum failure.
            // 重算摘要，使拒绝证明身份校验而非摘要失败。
            let bytes = serde_json::to_vec(&encoded).unwrap();
            connection
                .execute(
                    "UPDATE operations SET document=?1,digest=?2",
                    params![bytes, Sha256::digest(&bytes).as_slice()],
                )
                .unwrap();
            if damage == "legacy" {
                // Version one is the exact historical format without caller identity, not the current version constant.
                // 第一版是缺少调用身份的精确历史格式，不是当前版本常量。
                connection.pragma_update(None, "user_version", 1).unwrap();
            }
        }
        // Save exact bytes to prove refusal does not rewrite or repair historical evidence.
        // 保存精确字节，证明拒绝不会重写或修补历史证据。
        let before = std::fs::read(directory.database()).unwrap();
        // Reopening must fail before returning a usable history owner.
        // 重新打开必须在返回可用历史所有者前失败。
        let failure = OperationJournal::open(&directory.database(), config())
            .err()
            .unwrap();
        assert_eq!(
            failure.code,
            if damage == "legacy" {
                EmbeddedErrorCode::Unsupported
            } else {
                EmbeddedErrorCode::Internal
            },
            "{damage}"
        );
        assert_eq!(
            std::fs::read(directory.database()).unwrap(),
            before,
            "{damage}"
        );
    }
}
