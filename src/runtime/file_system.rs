use std::fs;
use std::path::Path;

#[cfg(windows)]
use windows_sys::Win32::Storage::FileSystem::ReplaceFileW;

/// Atomically publish one prepared file at a destination, replacing an existing file when needed.
/// 将一个已准备文件原子发布到目标位置，并在需要时替换已有文件。
///
/// The temp_path parameter must identify a completed file in the destination directory.
/// temp_path 参数必须指向目标目录内一个已经写完的文件。
///
/// The destination_path parameter identifies the final file name visible to readers.
/// destination_path 参数标识读取方可见的最终文件名。
///
/// Returns success only after the prepared file becomes the destination, or the exact filesystem error.
/// 仅在准备文件成为目标文件后返回成功，否则返回精确文件系统错误。
pub(crate) fn replace_file_atomically(
    temp_path: &Path,
    destination_path: &Path,
) -> Result<(), std::io::Error> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;

        // Destination existence remains explicit because ReplaceFileW requires an existing target.
        // 显式检查目标是否存在，因为 ReplaceFileW 要求目标文件已经存在。
        let destination_exists = destination_path.try_exists()?;
        if !destination_exists {
            return fs::rename(temp_path, destination_path);
        }

        // DestinationWide is the zero-terminated native path consumed by ReplaceFileW.
        // DestinationWide 是 ReplaceFileW 使用的零结尾原生路径。
        let destination_wide: Vec<u16> = destination_path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        // TempWide is the zero-terminated prepared-file path consumed by ReplaceFileW.
        // TempWide 是 ReplaceFileW 使用的零结尾准备文件路径。
        let temp_wide: Vec<u16> = temp_path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();

        // Replaced reports whether Windows atomically exchanged the destination contents.
        // Replaced 表示 Windows 是否已原子替换目标内容。
        let replaced = unsafe {
            ReplaceFileW(
                destination_wide.as_ptr(),
                temp_wide.as_ptr(),
                std::ptr::null(),
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if replaced == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        fs::rename(temp_path, destination_path)
    }
}

#[cfg(test)]
mod tests {
    use super::replace_file_atomically;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Monotonic suffix isolating atomic-replacement test directories in one process.
    /// 在单个进程内隔离原子替换测试目录的单调后缀。
    static ATOMIC_REPLACE_TEST_SEQUENCE: AtomicU64 = AtomicU64::new(1);

    /// Verify atomic publication creates a missing destination and replaces an existing one.
    /// 验证原子发布既能创建缺失目标，也能替换已有目标。
    #[test]
    fn atomic_file_replacement_handles_missing_and_existing_destinations() {
        // Sequence prevents parallel test invocations from sharing one temporary directory.
        // Sequence 防止并行测试调用共享同一个临时目录。
        let sequence = ATOMIC_REPLACE_TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        // TempRoot isolates both publication branches from repository files.
        // TempRoot 将两个发布分支与仓库文件隔离。
        let temp_root = std::env::temp_dir().join(format!(
            "luaskills_atomic_replace_{}_{}",
            std::process::id(),
            sequence
        ));
        fs::create_dir_all(&temp_root).expect("create atomic replacement test root");
        // Destination is the stable path observed before and after replacement.
        // Destination 是替换前后读取方观察到的稳定路径。
        let destination = temp_root.join("destination.bin");
        // FirstTemp supplies the initial destination contents.
        // FirstTemp 提供目标文件的初始内容。
        let first_temp = temp_root.join("first.tmp");
        fs::write(&first_temp, b"first").expect("write first prepared file");
        replace_file_atomically(&first_temp, &destination).expect("publish missing destination");
        assert_eq!(
            fs::read(&destination).expect("read first destination"),
            b"first"
        );

        // SecondTemp supplies replacement contents while the destination already exists.
        // SecondTemp 在目标已经存在时提供替换内容。
        let second_temp = temp_root.join("second.tmp");
        fs::write(&second_temp, b"second").expect("write second prepared file");
        replace_file_atomically(&second_temp, &destination).expect("replace existing destination");
        assert_eq!(
            fs::read(&destination).expect("read replaced destination"),
            b"second"
        );

        // Cleanup is best-effort because assertion diagnostics are more important than temp removal.
        // 清理按最佳努力执行，因为断言诊断比删除临时目录更重要。
        let _ = fs::remove_dir_all(&temp_root);
    }
}
