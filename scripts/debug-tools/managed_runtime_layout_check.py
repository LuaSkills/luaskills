#!/usr/bin/env python3
"""
Validate one prepared LuaSkills managed runtime root layout.
校验一个已准备好的 LuaSkills 受管运行时根目录布局。
"""

from __future__ import annotations

import argparse
import json
import os
import platform
import stat
import sys
from pathlib import Path


PYTHON_VERSION = "3.12.7"
UV_VERSION = "0.11.17"
NODE_VERSION = "22.11.0"
PNPM_VERSION = "9.15.0"


def current_platform_key() -> str:
    """
    Return the managed runtime platform key for the current host.
    返回当前宿主的受管运行时平台键。
    """
    system = platform.system().lower()
    machine = platform.machine().lower()
    if system == "windows":
        os_key = "windows"
    elif system == "linux":
        os_key = "linux"
    elif system == "darwin":
        os_key = "macos"
    else:
        raise RuntimeError(f"unsupported operating system: {platform.system()}")

    if machine in {"amd64", "x86_64"}:
        arch_key = "x64"
    elif machine in {"arm64", "aarch64"}:
        arch_key = "arm64"
    else:
        raise RuntimeError(f"unsupported architecture: {platform.machine()}")

    return f"{os_key}-{arch_key}"


def read_manifest(path: Path) -> dict:
    """
    Read one runtime-manifest.json file as UTF-8 while accepting a BOM.
    以 UTF-8 读取单个 runtime-manifest.json 文件，同时兼容 BOM。
    """
    text = path.read_text(encoding="utf-8-sig")
    return json.loads(text)


def executable_exists(path: Path) -> bool:
    """
    Return whether one executable path exists and is runnable enough for the host platform.
    返回单个可执行路径是否存在，并且对当前宿主而言具备足够的可运行属性。
    """
    if not path.is_file():
        return False
    if os.name == "nt":
        return True
    return bool(path.stat().st_mode & stat.S_IXUSR)


def validate_install(
    runtime_root: Path,
    family: str,
    directory_name: str,
    runtime: str,
    version: str,
    platform_key: str,
) -> list[str]:
    """
    Validate one managed runtime installation directory and manifest.
    校验单个受管运行时安装目录与清单。
    """
    errors: list[str] = []
    install_dir = runtime_root / "dependencies" / "runtimes" / family / directory_name
    manifest_path = install_dir / "runtime-manifest.json"
    if not manifest_path.is_file():
        return [f"missing manifest: {manifest_path}"]

    try:
        manifest = read_manifest(manifest_path)
    except Exception as error:  # noqa: BLE001
        return [f"failed to parse manifest {manifest_path}: {error}"]

    expected = {
        "schema_version": 1,
        "runtime": runtime,
        "version": version,
        "platform": platform_key,
    }
    for key, value in expected.items():
        if manifest.get(key) != value:
            errors.append(
                f"{manifest_path}: expected {key}={value!r}, got {manifest.get(key)!r}"
            )

    executable = manifest.get("executable")
    if not isinstance(executable, str) or not executable:
        errors.append(f"{manifest_path}: executable must be a non-empty string")
    elif Path(executable).is_absolute() or ".." in Path(executable).parts:
        errors.append(f"{manifest_path}: executable must stay under install directory")
    else:
        executable_path = install_dir / executable
        if not executable_exists(executable_path):
            errors.append(f"{manifest_path}: executable not found or not runnable: {executable_path}")

    source = manifest.get("source")
    if not isinstance(source, str) or not source:
        errors.append(f"{manifest_path}: source must be a non-empty string")

    return errors


def validate_env_markers(runtime_root: Path) -> list[str]:
    """
    Validate managed runtime environment marker files when environments exist.
    在环境存在时校验受管运行时环境 marker 文件。
    """
    errors: list[str] = []
    env_root = runtime_root / "dependencies" / "envs"
    if not env_root.exists():
        return errors

    for marker_path in env_root.rglob(".luaskills-env.json"):
        try:
            marker = read_manifest(marker_path)
        except Exception as error:  # noqa: BLE001
            errors.append(f"failed to parse env marker {marker_path}: {error}")
            continue

        for key in ("schema_version", "runtime", "runtime_version", "platform", "env_hash"):
            if key not in marker:
                errors.append(f"{marker_path}: missing marker field {key}")
        if marker.get("schema_version") != 1:
            errors.append(f"{marker_path}: schema_version must be 1")
        if marker.get("runtime") not in {"python", "node"}:
            errors.append(f"{marker_path}: runtime must be python or node")

    return errors


def validate_layout(runtime_root: Path) -> list[str]:
    """
    Validate all first-class managed runtime layout entries under one runtime root.
    校验单个运行时根目录下所有一等受管运行时布局项。
    """
    platform_key = current_platform_key()
    errors: list[str] = []
    errors.extend(
        validate_install(
            runtime_root,
            "python",
            f"uv-{UV_VERSION}-{platform_key}",
            "uv",
            UV_VERSION,
            platform_key,
        )
    )
    errors.extend(
        validate_install(
            runtime_root,
            "python",
            f"cpython-{PYTHON_VERSION}-{platform_key}",
            "python",
            PYTHON_VERSION,
            platform_key,
        )
    )
    errors.extend(
        validate_install(
            runtime_root,
            "node",
            f"node-{NODE_VERSION}-{platform_key}",
            "node",
            NODE_VERSION,
            platform_key,
        )
    )
    errors.extend(
        validate_install(
            runtime_root,
            "node",
            f"pnpm-{PNPM_VERSION}",
            "pnpm",
            PNPM_VERSION,
            "any",
        )
    )
    errors.extend(validate_env_markers(runtime_root))
    return errors


def main() -> int:
    """
    Parse CLI arguments and validate one managed runtime layout.
    解析命令行参数并校验单个受管运行时布局。
    """
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("runtime_root", help="Prepared LuaSkills runtime root to validate.")
    args = parser.parse_args()

    runtime_root = Path(args.runtime_root).resolve()
    errors = validate_layout(runtime_root)
    if errors:
        for error in errors:
            print(error, file=sys.stderr)
        return 1

    print(f"Managed runtime layout ok: {runtime_root}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
