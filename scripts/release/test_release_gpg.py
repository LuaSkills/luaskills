"""Exercise real OpenPGP rejection gates using temporary test credentials, never production secrets.
使用临时测试凭据验证真实 OpenPGP 拒绝门禁，绝不读取正式密钥。
"""

import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import release_gpg


@unittest.skipUnless(os.name == "posix" and shutil.which("gpg"), "requires native Unix GnuPG")
class ReleaseGPGTests(unittest.TestCase):
    """Check trust pinning, private/public separation, and byte-level tamper rejection.
    检查信任固定、私钥与公钥隔离以及字节级篡改拒绝。
    """

    def test_real_signatures_reject_tampering_missing_proof_and_wrong_identity(self):
        """Generate an ephemeral test key and exercise the actual release signer and verifier.
        生成一次性测试密钥，并运行真实的发行签名器和验证器。
        """
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            home = root / "source"
            home.mkdir(mode=0o700)
            try:
                release_gpg.gpg(home, "--pinentry-mode", "loopback", "--passphrase", "",
                                "--quick-generate-key", "Release Test <test@example.invalid>", "ed25519", "sign", "1d")
                listing = release_gpg.gpg(home, "--with-colons", "--list-secret-keys").decode()
                fingerprint = next(line.split(":")[9] for line in listing.splitlines() if line.startswith("fpr:"))
                private = release_gpg.gpg(home, "--armor", "--export-secret-keys", fingerprint).decode()
                public = root / "public.asc"
                public.write_bytes(release_gpg.gpg(home, "--armor", "--export", fingerprint))
                asset = root / "luaskills-debug"
                original = b"final release bytes"
                asset.write_bytes(original)
                with patch.object(release_gpg, "PUBLIC_KEY", public), patch.dict(
                    release_gpg.POLICY, {"gpg_primary_fingerprint": fingerprint}
                ), patch.dict(os.environ, {"GPG_PRIVATE_KEY": private, "GPG_PASSPHRASE": ""}):
                    release_gpg.process_assets([asset], sign=True)
                    self.assertNotIn("GPG_PRIVATE_KEY", os.environ)
                    release_gpg.process_assets([asset])
                    asset.write_bytes(b"changed release bytes")
                    with self.assertRaises(ValueError):
                        release_gpg.process_assets([asset])
                    asset.write_bytes(original)
                    with patch.dict(release_gpg.POLICY, {"gpg_primary_fingerprint": "A" * 40}):
                        with self.assertRaises(ValueError):
                            release_gpg.process_assets([asset])
                    Path(str(asset) + ".asc").unlink()
                    with self.assertRaises(ValueError):
                        release_gpg.process_assets([asset])
                # A secret key other than the pinned one is rejected before signing, naming what was supplied.
                # 非固定身份的私钥在签名前被拒绝，并指出实际提供的密钥。
                with patch.object(release_gpg, "PUBLIC_KEY", public), patch.dict(
                    release_gpg.POLICY, {"gpg_primary_fingerprint": "A" * 40}
                ), patch.dict(os.environ, {"GPG_PRIVATE_KEY": private, "GPG_PASSPHRASE": ""}):
                    with self.assertRaises(ValueError) as rejected:
                        release_gpg.process_assets([asset], sign=True)
                    self.assertIn(fingerprint, str(rejected.exception))
                    self.assertFalse(Path(str(asset) + ".asc").exists())
            finally:
                subprocess.run(["gpgconf", "--homedir", str(home), "--kill", "all"],
                               capture_output=True, timeout=15, check=True)


if __name__ == "__main__":
    unittest.main()
