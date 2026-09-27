"""Assemble only current addon binaries into an unpacked development bundle.

This produces a ZIP for UXP Developer Tool, not an installable/signed CCX.
"""
import argparse
import json
from pathlib import Path
import shutil

parser = argparse.ArgumentParser()
parser.add_argument("--platform", choices=["win/x64", "mac/x64", "mac/arm64", "all"], required=True)
parser.add_argument("--output", default="dist/plugin")
args = parser.parse_args()
root = Path(__file__).resolve().parents[1]
output = (root / args.output).resolve()
output.mkdir(parents=True, exist_ok=True)
manifest = json.loads((root / "plugin/manifest.json").read_text(encoding="utf-8-sig"))
for name in ["manifest.json", "index.html", "style.css", "main.js", "workflow.js", "compact-stack.js", "native-client.js"]:
    shutil.copy2(root / "plugin" / name, output / name)
for name in ["LICENSE-MIT", "LICENSE-APACHE", "UPSTREAM.md"]:
    shutil.copy2(root / name, output / name)
shutil.copytree(root / "plugin/icons", output / "icons", dirs_exist_ok=True)
platforms = ["win/x64", "mac/x64", "mac/arm64"] if args.platform == "all" else [args.platform]
for platform in platforms:
    destination = output / platform
    destination.mkdir(parents=True, exist_ok=True)
    shutil.copy2(root / "plugin" / platform / manifest["addon"]["name"], destination)
archive = shutil.make_archive(str(root / "dist" / f"negative-band-correction-{manifest['version']}-{args.platform.replace('/', '-')}-development"), "zip", output)
print(archive)
