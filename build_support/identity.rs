//! Hash declared package inputs without Git, timestamps, network access or machine-specific source paths.
//! 不使用 Git、时间戳、网络或机器相关源码路径，对声明的包输入计算摘要。

use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fs, io, path::Path};

/// Version shared by the source-hash domain and the machine-readable input report.
/// 源码摘要域及机器可读输入报告共享的版本。
pub const INPUTS_FORMAT_VERSION: u32 = 1;

/// Exact package-relative input roots; build tracking and fingerprinting share this one declaration.
/// 精确包相对输入根；构建追踪与指纹共用此唯一声明。
pub const INPUT_ROOTS: &[&str] = &[
    "Cargo.toml",
    "Cargo.lock",
    "build.rs",
    "build_support",
    "src",
    "include",
    "contracts",
];

/// Hash `bytes` and return the lowercase SHA-256 identity of their exact contents.
/// 对 `bytes` 计算摘要，并返回精确内容的小写 SHA-256 身份。
pub fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Read regular files below declared roots in `package`; return sorted relative paths and exact content hashes.
/// 读取 `package` 声明根下的普通文件；返回排序后的相对路径和精确内容摘要。
/// Missing inputs, symlinks and paths escaping the package are errors, never omitted build evidence.
/// 缺失输入、符号链接及逃出包的路径均为错误，绝不省略构建证据。
pub fn source_files(package: &Path) -> io::Result<BTreeMap<String, String>> {
    // Canonical containment is checked independently from the readable relative path stored in the report.
    // 规范路径包含关系与报告中存储的可读相对路径分别校验。
    let canonical_package = package.canonicalize()?;
    let mut pending: Vec<_> = INPUT_ROOTS.iter().map(|name| package.join(name)).collect();
    let mut directories = std::collections::BTreeSet::new();
    let mut files = BTreeMap::new();
    while let Some(path) = pending.pop() {
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink()
            || !path.canonicalize()?.starts_with(&canonical_package)
        {
            return Err(io::Error::other(
                "build input must remain inside the package without symlinks",
            ));
        }
        if metadata.is_dir() {
            if !directories.insert(path.canonicalize()?) {
                return Err(io::Error::other(
                    "duplicate or cyclic build input directory",
                ));
            }
            for entry in fs::read_dir(path)? {
                pending.push(entry?.path());
            }
        } else if metadata.is_file() {
            let relative = path
                .strip_prefix(package)
                .map_err(io::Error::other)?
                .to_str()
                .ok_or_else(|| io::Error::other("non-UTF-8 build input path"))?
                .replace('\\', "/");
            if files.insert(relative, digest(&fs::read(path)?)).is_some() {
                return Err(io::Error::other("duplicate build input path"));
            }
        } else {
            return Err(io::Error::other(
                "build input is not a regular file or directory",
            ));
        }
    }
    Ok(files)
}

/// Hash ordered `files` using a versioned domain and length-delimited names and hashes.
/// 使用版本化域及带长度边界的名称和摘要，对有序 `files` 计算摘要。
/// Return a deterministic source identity without conflating path or content boundaries.
/// 返回确定性源码身份，不混淆路径或内容边界。
pub fn source_digest(files: &BTreeMap<String, String>) -> String {
    let mut state = Sha256::new();
    state.update(format!("luaskills-build-inputs-v{INPUTS_FORMAT_VERSION}\0").as_bytes());
    for (name, hash) in files {
        state.update((name.len() as u64).to_be_bytes());
        state.update(name.as_bytes());
        state.update((hash.len() as u64).to_be_bytes());
        state.update(hash.as_bytes());
    }
    format!("{:x}", state.finalize())
}
