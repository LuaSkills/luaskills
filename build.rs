//! Generate provenance for the actual local package and Cargo-provided compilation inputs.
//! 为实际本地包及 Cargo 提供的编译输入生成来源元数据。

#[path = "build_support/identity.rs"]
mod identity;

use std::{collections::BTreeMap, env, fs, path::PathBuf, process::Command};

/// Read a required Cargo `name`; return its Unicode value or fail instead of inventing a build identity.
/// 读取必需 Cargo `name`；返回其 Unicode 值，或失败而不编造构建身份。
fn required(name: &str) -> Result<String, Box<dyn std::error::Error>> {
    Ok(env::var(name)?)
}

/// Generate Rust constants and a machine-readable input report into OUT_DIR; propagate all missing evidence.
/// 向 OUT_DIR 生成 Rust 常量及机器可读输入报告；传递全部缺失证据错误。
fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Only declared source inputs trigger source rehashing; OUT_DIR never becomes an input to itself.
    // 仅声明的源码输入触发重新计算；OUT_DIR 绝不成为其自身输入。
    let package = PathBuf::from(required("CARGO_MANIFEST_DIR")?);
    for input in identity::INPUT_ROOTS {
        println!("cargo::rerun-if-changed={input}");
    }
    let files = identity::source_files(&package)?;
    let mut fields = BTreeMap::new();
    fields.insert("source_sha256", identity::source_digest(&files));
    fields.insert(
        "contract_sha256",
        identity::digest(&fs::read(
            package.join("contracts/embedded/v1/contract.json"),
        )?),
    );
    fields.insert(
        "package_lock_sha256",
        identity::digest(&fs::read(package.join("Cargo.lock"))?),
    );
    // Cargo owns these values for the target crate, including cross-compilation and custom optimization settings.
    // Cargo 为目标 crate 拥有这些值，包括交叉编译及自定义优化设置。
    for (field, variable) in [
        ("target", "TARGET"),
        ("target_os", "CARGO_CFG_TARGET_OS"),
        ("target_arch", "CARGO_CFG_TARGET_ARCH"),
        ("pointer_width", "CARGO_CFG_TARGET_POINTER_WIDTH"),
        ("opt_level", "OPT_LEVEL"),
        ("debug_info", "DEBUG"),
    ] {
        fields.insert(field, required(variable)?);
    }
    // Hash extra arguments rather than exposing possible absolute paths or sensitive --cfg contents at runtime.
    // 对额外参数计算摘要，避免在运行时暴露可能的绝对路径或敏感 --cfg 内容。
    fields.insert(
        "rustflags_sha256",
        identity::digest(required("CARGO_ENCODED_RUSTFLAGS")?.as_bytes()),
    );
    let compiler = Command::new(required("RUSTC")?)
        .arg("--version")
        .arg("--verbose")
        .output()?;
    if !compiler.status.success() {
        return Err("cannot read the compiler's actual version".into());
    }
    fields.insert(
        "rustc",
        String::from_utf8(compiler.stdout)?.trim_end().to_owned(),
    );
    // Preserve Cargo's actual feature environment names; do not reverse its lossy underscore normalization.
    // 保留 Cargo 实际功能环境名称；不逆转其有损下划线归一化。
    let mut features: Vec<_> = env::vars_os()
        .filter_map(|(name, _)| {
            name.to_str()
                .and_then(|name| name.strip_prefix("CARGO_FEATURE_"))
                .map(str::to_owned)
        })
        .collect();
    features.sort();
    let report = serde_json::json!({"format_version": identity::INPUTS_FORMAT_VERSION, "fields": fields, "cargo_features": features, "source_files": files});
    let encoded = serde_json::to_vec_pretty(&report)?;
    let report_digest = identity::digest(&encoded);
    let mut generated = String::from(
        "// Generated from actual package and compiler inputs; do not edit.\n// 从实际包及编译器输入生成；请勿修改。\nstatic BUILD_IDENTITY: EmbeddedBuildIdentity = EmbeddedBuildIdentity {\n",
    );
    generated.push_str(&format!("    inputs_sha256: {report_digest:?},\n"));
    for (name, value) in &fields {
        generated.push_str(&format!("    {name}: {value:?},\n"));
    }
    generated.push_str(&format!("    cargo_features: &{features:?},\n}};\n"));
    let output = PathBuf::from(required("OUT_DIR")?);
    fs::write(output.join("embedded_build_identity.rs"), generated)?;
    fs::write(output.join("embedded-build-inputs.json"), encoded)?;
    Ok(())
}
