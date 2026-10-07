"""Puts Windows Terminal's ConPTY next to afar.exe (docs/16, "Свой ConPTY").

portable-pty loads `conpty.dll` from the program's folder before the
system one; that DLL starts `x64\\OpenConsole.exe` beside it. The newer
ConPTY passes programs' output through as is and in order (curly
underlines, underline colors, OSC in place), the system one re-renders it.

Usage: python tools/conpty.py [folder ...]
Without folders: target/debug and target/release (those that exist).
The package (Microsoft.Windows.Console.ConPTY, MIT) is downloaded once into
target/conpty/ and checked against a pinned SHA-256.
"""

import hashlib
import io
import os
import platform
import subprocess
import sys
import urllib.request
import zipfile

VERSION = "1.25.260930003"
SHA256 = "02b07b349af66d801159bdf9e440d4a1ce78bb951f37fc8609731665afdae7ee"
URL = f"https://www.nuget.org/api/v2/package/Microsoft.Windows.Console.ConPTY/{VERSION}"

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
CACHE = os.path.join(ROOT, "target", "conpty", f"Microsoft.Windows.Console.ConPTY.{VERSION}.nupkg")


def package() -> bytes:
    if os.path.isfile(CACHE):
        data = open(CACHE, "rb").read()
    else:
        print(f"downloading {URL}")
        try:
            with urllib.request.urlopen(URL, timeout=30) as r:
                data = r.read()
        except OSError:
            # Python's own connection may be blocked where curl (the
            # system's proxy settings, IPv4) gets through.
            out = subprocess.run(["curl", "-sfL", URL], capture_output=True, check=True)
            data = out.stdout
    digest = hashlib.sha256(data).hexdigest()
    if digest != SHA256:
        sys.exit(f"package checksum mismatch: {digest}")
    os.makedirs(os.path.dirname(CACHE), exist_ok=True)
    if not os.path.isfile(CACHE):
        open(CACHE, "wb").write(data)
    return data


def main() -> None:
    arch = "arm64" if platform.machine().lower() in ("arm64", "aarch64") else "x64"
    folders = sys.argv[1:] or [
        d for d in (os.path.join(ROOT, "target", p) for p in ("debug", "release")) if os.path.isdir(d)
    ]
    if not folders:
        sys.exit("no target folder: build afar first (cargo build)")
    z = zipfile.ZipFile(io.BytesIO(package()))
    files = {
        "conpty.dll": z.read(f"runtimes/win-{arch}/native/conpty.dll"),
        os.path.join(arch, "OpenConsole.exe"): z.read(f"build/native/runtimes/{arch}/OpenConsole.exe"),
    }
    for folder in folders:
        for name, data in files.items():
            path = os.path.join(folder, name)
            os.makedirs(os.path.dirname(path), exist_ok=True)
            try:
                if os.path.isfile(path) and open(path, "rb").read() == data:
                    continue
                open(path, "wb").write(data)
            except PermissionError:
                # A running afar holds them; they are the same version anyway.
                print(f"in use, kept: {path}")
                continue
            print(f"wrote {path}")


if __name__ == "__main__":
    main()
