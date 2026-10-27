"""Generate the Certum SimplySign one-time code for release signing; never log the seed or the code.
为发行签名生成 Certum SimplySign 一次性动态码；绝不记录种子或动态码。
"""

import base64
import hashlib
import hmac
import os
import re
import struct
import sys
import time


def totp(seed: str, timestamp: int, digits: int = 6) -> str:
    """Return the SimplySign SHA-256 TOTP for a raw Base32 seed and Unix time.
    根据原始 Base32 种子和 Unix 时间返回 SimplySign 使用的 SHA-256 动态码。
    """
    canonical = seed.strip().upper().rstrip("=")
    if not re.fullmatch(r"[A-Z2-7]{16,128}", canonical):
        raise ValueError("totp_seed_must_be_raw_base32")
    key = base64.b32decode(canonical + "=" * (-len(canonical) % 8))
    digest = hmac.new(key, struct.pack(">Q", timestamp // 30), hashlib.sha256).digest()
    offset = digest[-1] & 15
    number = struct.unpack(">I", digest[offset:offset + 4])[0] & 0x7FFFFFFF
    return str(number % (10 ** digits)).zfill(digits)


def main() -> None:
    """Print one code from CERTUM_TOTP_SECRET, waiting out a window that would expire during login.
    根据 CERTUM_TOTP_SECRET 输出一个动态码；当前时间窗即将在登录期间过期时等待下一个窗口。
    """
    if sys.argv[1:] != ["totp"]:
        raise RuntimeError("usage: certum_totp.py totp")
    seed = os.environ.pop("CERTUM_TOTP_SECRET", "")
    remaining = 30 - int(time.time()) % 30
    if remaining < 10:
        time.sleep(remaining + 1)
    print(totp(seed, int(time.time())))


if __name__ == "__main__":
    main()
