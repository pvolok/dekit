"""Builds the PyPI wheels from the release archives: one wheel per platform,
with the dekit binary installed as a script.

Usage: python3 packaging/pypi/build.py --archives <dir> --out <dir> --version <version>
"""

import argparse
import base64
import hashlib
import io
import json
import re
import tarfile
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
CONFIG = json.loads((ROOT / "packaging/platforms.json").read_text())

# Zip entries get a fixed time so the same archives give the same wheels.
DATE = (1980, 1, 1, 0, 0, 0)


def pep440(version):
    """1.2.3 stays as it is; 1.2.3-rc.1 becomes 1.2.3rc1."""
    m = re.fullmatch(r"(\d+\.\d+\.\d+)(?:-(alpha|beta|rc)\.(\d+))?", version)
    if not m:
        raise SystemExit(f"cannot turn {version} into a PyPI version")
    base, pre, n = m.groups()
    if not pre:
        return base
    pre = {"alpha": "a", "beta": "b", "rc": "rc"}[pre]
    return f"{base}{pre}{n}"


def read_binary(archives, platform):
    archive = archives / f"{CONFIG['package']}-{platform['target']}.{platform['archive']}"
    if platform["archive"] == "zip":
        with zipfile.ZipFile(archive) as z:
            return z.read(platform["binary"])
    with tarfile.open(archive, "r:gz") as t:
        return t.extractfile(platform["binary"]).read()


def metadata(version):
    readme = (ROOT / "packaging/pypi/README.md").read_text()
    lines = [
        "Metadata-Version: 2.4",
        f"Name: {CONFIG['pypiPackage']}",
        f"Version: {version}",
        "Summary: Process manager for dev and prod",
        "Keywords: process-manager,supervisor,task-runner,tui,cli,mprocs",
        "Author-email: Pavel Volokitin <pavelvolokitin@gmail.com>",
        "License-Expression: MIT",
        "License-File: LICENSE",
        "Project-URL: Homepage, https://dekit.run",
        "Project-URL: Documentation, https://dekit.run/docs",
        "Project-URL: Repository, https://github.com/pvolok/dekit",
        "Project-URL: Changelog, https://github.com/pvolok/dekit/blob/master/CHANGELOG.md",
        "Project-URL: Issues, https://github.com/pvolok/dekit/issues",
        "Description-Content-Type: text/markdown",
    ]
    return "\n".join(lines) + "\n\n" + readme


def wheel(platform, binary, out, version):
    name = CONFIG["pypiPackage"].replace("-", "_")
    dist_info = f"{name}-{version}.dist-info"
    tags = [f"py3-none-{tag}" for tag in platform["pypiPlatform"].split(".")]
    wheel_meta = "".join(
        ["Wheel-Version: 1.0\n", "Generator: dekit packaging/pypi/build.py\n", "Root-Is-Purelib: false\n"]
        + [f"Tag: {tag}\n" for tag in tags]
    )
    files = [
        (f"{name}-{version}.data/scripts/{platform['binary']}", binary, 0o755),
        (f"{dist_info}/METADATA", metadata(version).encode(), 0o644),
        (f"{dist_info}/WHEEL", wheel_meta.encode(), 0o644),
        (f"{dist_info}/licenses/LICENSE", (ROOT / "LICENSE").read_bytes(), 0o644),
    ]

    record = io.StringIO()
    for path, data, _ in files:
        digest = base64.urlsafe_b64encode(hashlib.sha256(data).digest()).rstrip(b"=").decode()
        record.write(f"{path},sha256={digest},{len(data)}\n")
    record.write(f"{dist_info}/RECORD,,\n")
    files.append((f"{dist_info}/RECORD", record.getvalue().encode(), 0o644))

    path = out / f"{name}-{version}-py3-none-{platform['pypiPlatform']}.whl"
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as z:
        for entry, data, mode in files:
            info = zipfile.ZipInfo(entry, DATE)
            info.external_attr = (0o100000 | mode) << 16
            info.compress_type = zipfile.ZIP_DEFLATED
            z.writestr(info, data)
    print(path)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--archives", required=True, type=Path)
    parser.add_argument("--out", required=True, type=Path)
    parser.add_argument("--version", required=True)
    args = parser.parse_args()

    version = pep440(args.version)
    args.out.mkdir(parents=True, exist_ok=True)
    for platform in CONFIG["platforms"]:
        wheel(platform, read_binary(args.archives, platform), args.out, version)


if __name__ == "__main__":
    main()
