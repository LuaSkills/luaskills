//! Verify native allocator addressing and external-state ownership across real destruction.
//! 验证原生分配器寻址及外部状态所有权的真实销毁行为。

use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

/// Count native userdata destruction without keeping a strong Lua handle.
/// 统计原生用户数据销毁次数，不持有强 Lua 句柄。
struct DropProbe(Arc<AtomicUsize>);

impl mlua::UserData for DropProbe {}

impl Drop for DropProbe {
    /// Increment the shared counter once when Lua closes or collects this value; return unit.
    /// Lua 关闭或收集此值时将共享计数器增加一次；返回空值。
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

/// Native closure releases retained userdata and invalidates weak handles exactly once.
/// 原生关闭释放保留的用户数据，并恰好使弱句柄失效一次。
#[test]
fn native_state_closes_retained_userdata_and_weak_handles() {
    // The counter remains outside the state to observe actual finalization.
    // 计数器位于状态之外，以观察真实终结。
    let drops = Arc::new(AtomicUsize::new(0));
    // This owner is the sole authority allowed to close the native state.
    // 此所有者是唯一允许关闭原生状态的权威。
    let lua = LuaState::new(StdLib::ALL_SAFE).unwrap();
    lua.globals()
        .set("probe", DropProbe(Arc::clone(&drops)))
        .unwrap();
    // Weak value references must not keep the state alive after the exclusive owner closes it.
    // 独占所有者关闭状态后，弱值引用不能延长其存活。
    let weak = lua.weak();
    // Value handles are weak, so even an accidentally retained value must become inert after close.
    // 值句柄为弱引用，因此即使意外保留一个值，关闭后也必须失效。
    let retained = lua.create_table().unwrap();
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    drop(lua);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    assert!(weak.try_upgrade().is_none());
    drop(retained);
}

/// LuaJIT allocations stay representable even after worker memory is freed on the caller.
/// 即使工作线程内存在调用线程释放，LuaJIT 分配仍保持在可表示范围内。
#[test]
fn native_state_survives_cross_thread_allocator_reuse() {
    // Repeat fresh states because the original failure depends on prior runtime teardown.
    // 重复创建全新状态，因为原始失败依赖此前运行时的销毁。
    for _ in 0..16 {
        // Transfers cover the small-object size classes used by Lua strings and parser tables.
        // 转移覆盖 Lua 字符串及解析器表使用的小对象大小类别。
        let allocations = std::thread::spawn(|| {
            // Every allocation is freed on the caller rather than its allocating worker.
            // 每个分配均由调用线程释放，而不是由分配它的工作线程释放。
            let mut allocations = Vec::new();
            for size in (16..=1024).step_by(16) {
                for _ in 0..16 {
                    allocations.push(vec![0x5au8; size].into_boxed_slice());
                }
            }
            allocations
        })
        .join()
        .unwrap();
        drop(allocations);
        // The native allocator is selected before the first Lua GC object exists.
        // 在第一个 Lua GC 对象存在之前选择原生分配器。
        let lua = LuaState::new(StdLib::ALL_SAFE).unwrap();
        assert!((lua.state.as_ptr() as u64) < (1u64 << 47));
        assert_eq!(
            lua.load("local t = {}; for i=1,256 do t[i] = 'address-test-' .. i end; return #t")
                .eval::<usize>()
                .unwrap(),
            256
        );
        // Retain many collectable strings and tables, checking actual live object addresses.
        // 保留大量可收集字符串和表，检查真实存活对象地址。
        for index in 0..256 {
            // Each retained table has an independently allocated GC header.
            // 每个保留的表都有独立分配的 GC 头部。
            let table = lua.create_table().unwrap();
            table
                .set("text", format!("allocation-{index}-{}", "x".repeat(index)))
                .unwrap();
            assert!((table.to_pointer() as u64) < (1u64 << 47));
            lua.globals()
                .set(format!("retained_{index}"), table)
                .unwrap();
        }
    }
}
