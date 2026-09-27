"""Fetch a privately hosted SDK ZIP, verify its digest, extract safely for CI."""
import hashlib
import io
import os
from pathlib import Path
import sys
import urllib.request
import zipfile

def main():
    url = os.environ.get("UXP_SDK_URL", "")
    digest = os.environ.get("UXP_SDK_SHA256", "").lower()
    if not url.startswith("https://") or len(digest) != 64:
        raise ValueError("Configure UXP_SDK_URL (HTTPS secret) and UXP_SDK_SHA256 (repository variable)")
    try:
        request = urllib.request.Request(url, headers={"User-Agent": "Mozilla/5.0 (compatible; BandingBuild/1.0)"})
        with urllib.request.urlopen(request, timeout=120) as response:
            data = response.read()
    except Exception:
        # Do not expose a signed download URL in Actions logs.
        raise RuntimeError("SDK download failed; check the SDK URL secret") from None
    if hashlib.sha256(data).hexdigest() != digest:
        raise ValueError("SDK SHA-256 mismatch")
    destination = Path(sys.argv[1]).resolve()
    destination.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(io.BytesIO(data)) as archive:
        for entry in archive.infolist():
            target = (destination / entry.filename).resolve()
            if not target.is_relative_to(destination):
                raise ValueError("SDK archive contains an unsafe path")
        archive.extractall(destination)
    headers = list(destination.rglob("src/api/UxpAddonShared.h"))
    if len(headers) != 1:
        raise ValueError("SDK archive must contain one src/api/UxpAddonShared.h")
    sdk_root = headers[0].parents[2]
    with open(os.environ["GITHUB_ENV"], "a", encoding="utf8") as env:
        env.write(f"UXP_SDK_PATH={sdk_root}\n")

if __name__ == "__main__":
    main()
