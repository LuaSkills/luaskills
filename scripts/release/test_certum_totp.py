"""Offline checks for the release signing one-time code generator.
发行签名一次性动态码生成器的离线检查。
"""

import base64
import unittest

import certum_totp


class CertumTotpTest(unittest.TestCase):
    """The generator matches RFC 6238 SHA-256 and refuses anything but a raw Base32 seed.
    生成器符合 RFC 6238 SHA-256，并拒绝原始 Base32 种子以外的输入。
    """

    def test_rfc6238_sha256_vectors(self):
        seed = base64.b32encode(b"12345678901234567890123456789012").decode("ascii")
        vectors = [(59, "46119246"), (1111111109, "68084774"),
                   (1111111111, "67062674"), (1234567890, "91819424"),
                   (2000000000, "90698825"), (20000000000, "77737706")]
        for timestamp, expected in vectors:
            self.assertEqual(certum_totp.totp(seed, timestamp, 8), expected)

    def test_invalid_seeds_are_refused(self):
        for invalid in ["", "not-a-seed", "0123456789", "otpauth://totp/test"]:
            with self.assertRaises(ValueError):
                certum_totp.totp(invalid, 59)


if __name__ == "__main__":
    unittest.main()
