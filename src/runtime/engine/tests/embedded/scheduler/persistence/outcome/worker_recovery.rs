//! Actual writer replacement preserves original native and queued callback completions through shutdown.
//! 实际写入者替换在关闭期间保留原生及队列回调的原始完成。

use super::*;

/// Recover before-write and post-commit worker panics without rerunning host handlers or releasing their permits early.
/// 恢复写前及提交后工作线程 panic，不重跑宿主处理器，也不提前释放其许可。
#[test]
fn embedded_writer_recovery_preserves_original_callback_completion() {
    for queued in [false, true] {
        for after_storage in [false, true] {
            for closing in [false, true] {
                // Every transport, failure boundary and close branch owns a separate real Lua/SQLite runtime.
                // 每种传输、故障边界及关闭分支拥有独立真实 Lua／SQLite 运行时。
                let layout = SystemRuntimeTestLayout::new("embedded writer callback recovery");
                // Retain the original journal, writer object and runtime while only its failed thread is replaced.
                // 仅替换失败线程，同时保留原日志、写入者对象及运行时。
                let (journal, writer, runtime) =
                    durable_runtime(&layout, pool_config(), journal_config(16, 128 * 1024));
                // Actual handler entries are counted independently from every persistence attempt.
                // 独立于每次持久尝试统计实际处理器进入。
                let count = Arc::new(AtomicUsize::new(0));
                // Registration identity is captured from the chosen real transport's authoritative response.
                // 从所选真实传输的权威响应捕获注册身份。
                let (pool, mut registration) = if queued {
                    (queued_pool(&runtime, &layout), None)
                } else {
                    // The native handler arms failure only after its durable start intent already exists.
                    // 原生处理器仅在持久开始意图已经存在后启用故障。
                    let handler_writer = Arc::clone(&writer);
                    // Share the exact entry counter without exposing storage state to Lua.
                    // 共享精确进入计数器，不向 Lua 暴露存储状态。
                    let called = Arc::clone(&count);
                    // Reuse the established capability schema and permissions.
                    // 复用既有能力 Schema 及权限。
                    let mut descriptor = super::super::super::super::capabilities::descriptor(
                        "test.recover_writer",
                        CapabilityExecution::Native,
                    );
                    descriptor.effects = CapabilityEffects::Mutating;
                    descriptor.max_call_ms = OBSERVE.as_millis() as u64;
                    // This registration retains actual native permission through lost completion publication.
                    // 此注册在完成发布丢失期间保留实际原生许可。
                    let registration = runtime
                        .capabilities()
                        .register(vec![CapabilityRegistrationRequest {
                            descriptor,
                            native: Some(Arc::new(move |_| {
                                called.fetch_add(1, Ordering::SeqCst);
                                handler_writer.panic_next_write_for_test(after_storage);
                                CapabilityOutcome {
                                    result: Ok(json!({"original":true})),
                                    effects: EffectState::Committed,
                                }
                            })),
                        }])
                        .unwrap()
                        .into_iter()
                        .next()
                        .unwrap();
                    // This export unwraps only the original acknowledged native result.
                    // 此导出仅展开原已确认原生结果。
                    let pool = runtime.register_pool(definition(&layout, "return {call=function() local r=vulcan.capabilities.call('test.recover_writer',{}); assert(r.ok); return r.value end}"), pool_policy(InstanceReuse::Reusable), permissions(), "r1".into()).unwrap();
                    (pool, Some(registration))
                };
                // Preserve one exact operation throughout failure, reconstruction and optional cancellation.
                // 跨故障、重建及可选取消保留单个精确操作。
                let operation = runtime.submit(call(&pool, Value::Null), OBSERVE).unwrap();
                if queued {
                    // Extraction proves the original callback intent reached storage before completion is submitted.
                    // 提取证明完成提交前，原回调意图已到达存储。
                    let request = host_request(&runtime);
                    registration = Some(request.registration_id);
                    count.fetch_add(1, Ordering::SeqCst);
                    writer.panic_next_write_for_test(after_storage);
                    runtime
                        .capabilities()
                        .host_requests()
                        .complete(
                            &request.request_id,
                            CapabilityOutcome {
                                result: Ok(json!({"original":true})),
                                effects: EffectState::Committed,
                            },
                        )
                        .unwrap();
                }
                // Both transport branches have now obtained an exact authoritative registration identity.
                // 两种传输分支此时均已取得精确权威注册身份。
                let registration = registration.unwrap();
                // The retained business result is independent of the writer's infrastructure failure.
                // 保留业务结果独立于写入者基础设施故障。
                let failed = failure(&runtime, &operation);
                assert_eq!(failed.error.code, EmbeddedErrorCode::Internal);
                assert_eq!(failed.retry, CheckpointRetryState::Waiting);
                until(
                    || writer.status().unwrap().worker_exited,
                    "failed writer did not actually exit",
                );
                assert!(writer.status().unwrap().failure.is_some());
                // The journal itself is healthy; only the actual completion publication may have been lost.
                // 日志自身健康；可能丢失的仅是实际完成发布。
                let stored = journal.get(runtime.id(), operation.id()).unwrap().unwrap();
                // Locate the original effect by registration instead of an evolving vector position.
                // 按注册身份定位原副作用，而非不断演进的向量位置。
                let effect = stored
                    .snapshot
                    .host_effects
                    .iter()
                    .find(|effect| effect.registration_id == registration)
                    .unwrap();
                assert_eq!(
                    effect.effects,
                    if after_storage {
                        EffectState::Committed
                    } else {
                        EffectState::Unknown
                    }
                );
                // Live callback ownership survives unregister and an optional runtime-wide close request.
                // 活动回调所有权跨注销及可选运行时全局关闭请求保留。
                let retiring = runtime.capabilities().unregister(&registration).unwrap();
                assert_eq!(retiring.in_flight, 1);
                assert!(!retiring.drained);
                if closing {
                    runtime.request_close().unwrap();
                    assert!(!runtime.poll_closed().unwrap());
                }
                assert!(writer.recover_worker().unwrap());
                assert_eq!(
                    failure(&runtime, &operation).retry,
                    CheckpointRetryState::Waiting
                );
                assert_eq!(
                    runtime
                        .capabilities()
                        .status(&registration)
                        .unwrap()
                        .in_flight,
                    1
                );
                assert_eq!(count.load(Ordering::SeqCst), 1);
                assert!(retry(&runtime, &operation));
                // The original owner resumes only checkpoint confirmation, preserving late committed effects.
                // 原所有者仅恢复检查点确认，保留迟到已提交副作用。
                let completed = operation.wait(OBSERVE).unwrap();
                assert_eq!(completed.context, stored.snapshot.context);
                assert_eq!(
                    completed.phase,
                    if closing {
                        OperationPhase::Cancelled
                    } else {
                        OperationPhase::Succeeded
                    }
                );
                assert!(
                    completed
                        .host_effects
                        .iter()
                        .any(|effect| effect.registration_id == registration
                            && effect.effects == EffectState::Committed
                            && effect.phase == HostEffectPhase::Completed)
                );
                if !closing {
                    assert_eq!(
                        completed.value,
                        Some(if queued {
                            json!({"ok":true,"effects":"committed","value":{"original":true}})
                        } else {
                            json!({"original":true})
                        })
                    );
                }
                assert_eq!(count.load(Ordering::SeqCst), 1);
                assert!(
                    runtime
                        .capabilities()
                        .status(&registration)
                        .unwrap()
                        .drained
                );
                shutdown_durable(&runtime, &writer);
            }
        }
    }
}
