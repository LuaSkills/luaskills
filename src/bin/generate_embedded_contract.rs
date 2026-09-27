use luaskills::ffi_embedded::contract;
use sha2::{Digest, Sha256};
use std::{env, fs, path::PathBuf};

/// Generate or verify the bundled embedded contract and its exact SHA-256 sidecar.
/// 生成或验证随包嵌入式契约及其精确 SHA-256 伴随文件。
/// Accept only an optional `--check`; return failure without changing stale files in check mode.
/// 仅接受可选 `--check`；检查模式下发现陈旧文件直接失败且不修改文件。
fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Keep generation rooted in this crate, independent of the invoking shell's directory.
    // 使生成根固定为本 crate，独立于调用 Shell 的目录。
    let arguments: Vec<_> = env::args_os().skip(1).collect();
    let check = match arguments.as_slice() {
        [] => false,
        [argument] if argument == "--check" => true,
        _ => return Err("usage: generate_embedded_contract [--check]".into()),
    };
    let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("contracts/embedded/v1");
    let bytes = contract::bytes()?;
    let checksum = format!("{:x}  contract.json\n", Sha256::digest(&bytes));
    // Compare exact bytes; a matching package version cannot substitute for matching contracts.
    // 比较精确字节；包版本相同不能替代契约匹配。
    for (name, expected) in [
        ("contract.json", bytes.as_slice()),
        ("contract.sha256", checksum.as_bytes()),
    ] {
        let path = directory.join(name);
        if check {
            if fs::read(&path)? != expected {
                return Err(format!(
                    "{} is stale; regenerate the embedded contract",
                    path.display()
                )
                .into());
            }
        } else {
            fs::create_dir_all(&directory)?;
            fs::write(&path, expected)?;
        }
    }
    Ok(())
}
