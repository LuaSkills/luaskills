"""Sign and verify the Linux debug tool with the committed OpenPGP trust root.
使用仓库固定的 OpenPGP 信任根签名和验证 Linux 调试工具。
"""

import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile


# Public trust comes from reviewed repository files, never from the release being verified.
# 公开信任来自已审核的仓库文件，绝不从待验证发行包中读取信任根。
POLICY = json.loads(Path(__file__).with_name("signing-policy.json").read_text(encoding="utf-8"))
PUBLIC_KEY = Path(__file__).with_name("release-gpg-public.asc")


def gpg(home, *arguments, stdin=None, failure="GPG operation failed"):
    """Run bounded, non-interactive GnuPG and keep native diagnostics out of CI logs.
    限时、非交互运行 GnuPG，避免原生日志泄露凭据。
    home selects the isolated keyring, arguments selects the operation, stdin supplies optional secret bytes, and failure names
    the failed stage without native output; returns stdout bytes.
    home 指定隔离密钥环，arguments 指定操作，stdin 传入可选机密字节，failure 在不含原生输出的前提下说明失败阶段；返回标准输出字节。
    """
    result = subprocess.run(["gpg", "--homedir", str(home), "--batch", "--no-tty", *arguments],
                            input=stdin, capture_output=True, timeout=120)
    if result.returncode:
        raise ValueError(failure)
    return result.stdout


def secret_fingerprints(home):
    """List primary fingerprints of the secret keys in an isolated keyring; fingerprints are public identifiers.
    列出隔离密钥环中私钥的主密钥指纹；指纹是公开标识。
    """
    listing = gpg(home, "--with-colons", "--list-secret-keys", failure="Listing imported secret keys failed").decode("utf-8")
    fingerprints, primary = [], False
    for line in listing.splitlines():
        fields = line.split(":")
        if fields[0] in {"sec", "ssb"}:
            primary = fields[0] == "sec"
        elif fields[0] == "fpr" and primary:
            fingerprints.append(fields[9])
            primary = False
    return fingerprints


def check_signature(home, asset):
    """Verify one detached signature and require the pinned primary signing identity.
    验证一份分离式签名，并要求签名主密钥身份与固定指纹一致。
    home is a prepared keyring and asset is the signed file; returns None or raises ValueError on missing or invalid proof.
    home 是已准备的密钥环，asset 是被签名文件；通过时返回空值，证明缺失或无效时抛出 ValueError。
    """
    signature = Path(str(asset) + ".asc")
    if signature.is_symlink() or not signature.is_file():
        raise ValueError("Missing or non-regular Linux signature")
    status = gpg(home, "--status-fd", "1", "--verify", str(signature), str(asset),
                 failure="Linux signature verification failed").decode("utf-8")
    valid = [line.split() for line in status.splitlines() if line.startswith("[GNUPG:] VALIDSIG ")]
    if len(valid) != 1 or POLICY["gpg_primary_fingerprint"] not in {valid[0][2], valid[0][-1]}:
        raise ValueError("Linux signing identity does not match the committed trust root")


def process_assets(assets, sign=False):
    """Sign or verify explicit final assets; private material exists only in an isolated temporary keyring.
    签名或验证明确指定的最终资产；私钥仅存在于隔离的临时密钥环。
    assets names exact final files; sign enables private-key signing. Returns None only after all signatures verify.
    assets 指定精确的最终文件，sign 启用私钥签名；仅在全部签名验证成功后返回空值。
    """
    # Native children must never inherit signing secrets from the environment.
    # 原生子进程不得从环境变量继承签名机密。
    private_key = os.environ.pop("GPG_PRIVATE_KEY", "")
    passphrase = os.environ.pop("GPG_PASSPHRASE", "")
    if any(Path(asset).is_symlink() for asset in assets):
        raise ValueError("Linux signing targets cannot be symbolic links")
    assets = [Path(asset).resolve(strict=True) for asset in assets]
    if not assets or len(set(assets)) != len(assets):
        raise ValueError("Linux signing targets must be nonempty and unique")
    for asset in assets:
        if not asset.is_file():
            raise ValueError("Only explicit regular files can be signed")
    if sign and not private_key.lstrip().startswith("-----BEGIN PGP PRIVATE KEY BLOCK-----"):
        raise ValueError("GPG_PRIVATE_KEY must contain an armored private key")
    with tempfile.TemporaryDirectory(prefix="release-gpg-") as temporary:
        home = Path(temporary) / "keys"
        home.mkdir(mode=0o700)
        try:
            gpg(home, "--import", stdin=PUBLIC_KEY.read_bytes(), failure="Importing the committed public key failed")
            # Signing names the trusted key explicitly; an unrelated imported key cannot replace it.
            # 签名显式指定可信密钥，无关导入密钥无法替代它。
            if sign:
                gpg(home, "--import", stdin=private_key.encode("utf-8"),
                    failure="GPG_PRIVATE_KEY could not be imported as an OpenPGP secret key")
                private_key = ""
                found = secret_fingerprints(home)
                if POLICY["gpg_primary_fingerprint"] not in found:
                    raise ValueError(f"GPG_PRIVATE_KEY holds secret keys {found or 'none'}, "
                                     f"not the pinned signing key {POLICY['gpg_primary_fingerprint']}")
                for asset in assets:
                    signature = Path(str(asset) + ".asc")
                    if signature.exists() or signature.is_symlink():
                        raise ValueError("Refusing to overwrite an existing Linux signature")
                    gpg(home, "--pinentry-mode", "loopback", "--passphrase-fd", "0",
                        "--local-user", POLICY["gpg_primary_fingerprint"], "--armor", "--detach-sign",
                        "--output", str(signature), str(asset), stdin=(passphrase + "\n").encode("utf-8"),
                        failure="Signing with the pinned key failed; a protected key needs the GPG_PASSPHRASE secret")
            for asset in assets:
                check_signature(home, asset)
        finally:
            subprocess.run(["gpgconf", "--homedir", str(home), "--kill", "all"],
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=15, check=False)
    if sign:
        # Recheck without secret material, using a fresh public-only keyring.
        # 使用全新的纯公钥密钥环复验，不再依赖私钥材料。
        process_assets(assets)


def main():
    """Accept a closed operation name and explicit asset paths from the release workflow.
    从发行工作流接收封闭操作名和明确的资产路径。
    Parses process arguments without function parameters; successful completion returns None.
    无函数参数，从进程参数解析操作；成功结束时返回空值。
    """
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("operation", choices=("sign", "verify"))
    parser.add_argument("assets", nargs="+")
    args = parser.parse_args()
    process_assets(args.assets, sign=args.operation == "sign")
    print(f"Linux signatures {args.operation} passed: {len(args.assets)}")


if __name__ == "__main__":
    main()
