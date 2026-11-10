#!/usr/bin/env python3
"""Build the updater manifest ``latest.json`` for a desktop release.

The desktop app checks
``https://github.com/JinsongRoh/Voice-Command-AI/releases/latest/download/latest.json``
(``plugins.updater`` in ``apps/desktop/src-tauri/tauri.conf.json``). This
script writes that file from the signed updater artifacts that
``tauri build`` produced (``bundle.createUpdaterArtifacts``): each artifact
next to its ``.sig`` file. Nothing is uploaded; see docs/releasing.md.

Artifacts found (searched recursively in the given files and folders):

=====================================  ======================================
Artifact                               ``latest.json`` platforms
=====================================  ======================================
``*_x64-setup.exe`` (NSIS)             ``windows-x86_64``, ``windows-x86_64-nsis``
``*_arm64-setup.exe`` (NSIS)           ``windows-aarch64``, ``windows-aarch64-nsis``
``*aarch64*.app.tar.gz``               ``darwin-aarch64``, ``darwin-aarch64-app``
``*x64*.app.tar.gz``                   ``darwin-x86_64``, ``darwin-x86_64-app``
``*_amd64.AppImage``                   ``linux-x86_64-appimage``
``*_aarch64.AppImage``                 ``linux-aarch64-appimage``
``*_amd64.deb`` / ``*_arm64.deb``      ``linux-x86_64-deb`` / ``linux-aarch64-deb``
``*.x86_64.rpm`` / ``*.aarch64.rpm``   ``linux-x86_64-rpm`` / ``linux-aarch64-rpm``
=====================================  ======================================

Linux entries always name the package type: the app looks up
``linux-<arch>-<appimage|deb|rpm>`` for its own kind of install, so a
``.deb`` install is never handed an AppImage. (A Linux install whose
package type has no entry is shown the release page instead.)

Tauri names both macOS archives ``<Product>.app.tar.gz``. Give them the
architecture with ``--artifact darwin-aarch64-app=PATH`` (and
``darwin-x86_64-app``), or rename them first; renaming does not affect the
signature, which covers the content only.

GitHub turns spaces in uploaded asset names into dots, so the URLs use
"safe" names (anything but letters, digits, ``.``, ``_`` and ``-`` becomes
``.``). ``--stage DIR`` copies every artifact, its ``.sig`` and
``latest.json`` into DIR under exactly those names, ready to upload.

Usage::

    python tools/release/make-latest-json.py --version 0.2.0 \\
        --notes-file notes.md --stage dist/release \\
        apps/target/release/bundle \\
        --artifact darwin-aarch64-app=mac/arm64/Voice\\ Command\\ AI.app.tar.gz
"""

from __future__ import annotations

import argparse
import base64
import binascii
import json
import re
import shutil
import sys
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from urllib.parse import quote

DEFAULT_REPO = "JinsongRoh/Voice-Command-AI"

INSTALLERS = ("nsis", "app", "appimage", "deb", "rpm")
OSES = {"nsis": "windows", "app": "darwin", "appimage": "linux", "deb": "linux", "rpm": "linux"}
ARCHES = ("x86_64", "aarch64")

SEMVER = re.compile(
    r"^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)"
    r"(?:-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$"
)


class ReleaseError(Exception):
    """A problem with the inputs; the message says what to fix."""


@dataclass(frozen=True)
class Artifact:
    installer: str  # one of INSTALLERS
    arch: str  # one of ARCHES
    path: Path

    @property
    def os(self) -> str:
        return OSES[self.installer]

    @property
    def keys(self) -> list[str]:
        """The ``latest.json`` platforms this artifact serves."""
        full = f"{self.os}-{self.arch}-{self.installer}"
        if self.os == "linux":
            return [full]
        return [f"{self.os}-{self.arch}", full]

    @property
    def sig_path(self) -> Path:
        return self.path.with_name(self.path.name + ".sig")


def _arch_token(name: str, tokens: dict[str, str]) -> str | None:
    """The architecture named in a file name, from separator-delimited tokens."""
    alternatives = "|".join(sorted(map(re.escape, tokens), key=len, reverse=True))
    pattern = rf"(?<![a-z0-9])({alternatives})(?![a-z0-9])"
    found = {tokens[m] for m in re.findall(pattern, name.lower())}
    if len(found) > 1:
        raise ReleaseError(f"{name}: names more than one architecture")
    return found.pop() if found else None


_COMMON_TOKENS = {
    "x64": "x86_64",
    "x86_64": "x86_64",
    "amd64": "x86_64",
    "aarch64": "aarch64",
    "arm64": "aarch64",
}


def classify(path: Path) -> Artifact | None:
    """The artifact a file is, or ``None`` when it is not an updater artifact."""
    name = path.name
    lower = name.lower()
    if lower.endswith("-setup.exe"):
        installer = "nsis"
        stem = name[: -len("-setup.exe")]
    elif lower.endswith(".app.tar.gz"):
        installer = "app"
        stem = name[: -len(".app.tar.gz")]
    elif lower.endswith(".appimage"):
        installer = "appimage"
        stem = name[: -len(".appimage")]
    elif lower.endswith(".deb"):
        installer = "deb"
        stem = name[: -len(".deb")]
    elif lower.endswith(".rpm"):
        installer = "rpm"
        stem = name[: -len(".rpm")]
    else:
        return None
    # The version may contain dots ("0.2.0"), so only whole tokens count.
    arch = _arch_token(stem, _COMMON_TOKENS)
    if arch is None:
        if installer == "app":
            raise ReleaseError(
                f"{path}: the macOS archive does not name its architecture; pass "
                "--artifact darwin-aarch64-app=PATH or darwin-x86_64-app=PATH, or rename it "
                "(for example to Voice.Command.AI_0.2.0_aarch64.app.tar.gz)"
            )
        raise ReleaseError(
            f"{path}: cannot tell the architecture; pass --artifact "
            f"<os>-<x86_64|aarch64>-{installer}=PATH"
        )
    return Artifact(installer, arch, path)


def parse_override(spec: str) -> Artifact:
    """``linux-aarch64-deb=PATH`` style ``--artifact`` values."""
    key, sep, value = spec.partition("=")
    if not sep or not value:
        raise ReleaseError(f"--artifact {spec!r}: expected <os>-<arch>-<installer>=PATH")
    parts = key.strip().split("-")
    if len(parts) != 3:
        raise ReleaseError(f"--artifact {spec!r}: expected <os>-<arch>-<installer>=PATH")
    os_, arch, installer = parts
    if installer not in INSTALLERS or arch not in ARCHES or OSES[installer] != os_:
        allowed = ", ".join(f"{OSES[i]}-<{'|'.join(ARCHES)}>-{i}" for i in INSTALLERS)
        raise ReleaseError(f"--artifact {spec!r}: unknown platform; use one of {allowed}")
    return Artifact(installer, arch, Path(value))


def find_artifacts(inputs: list[Path], skip: frozenset[Path] = frozenset()) -> list[Artifact]:
    """Updater artifacts in the given files and folders (only those with a ``.sig``).

    ``skip``: resolved paths given with ``--artifact`` instead.
    """
    found: list[Artifact] = []
    for root in inputs:
        if not root.exists():
            raise ReleaseError(f"{root}: no such file or folder")
        files = [root] if root.is_file() else sorted(p for p in root.rglob("*") if p.is_file())
        for path in files:
            if path.resolve() in skip:
                continue
            artifact = classify(path)
            if artifact is None:
                continue
            if not artifact.sig_path.is_file():
                print(f"note: skipping {path} (no {artifact.sig_path.name})", file=sys.stderr)
                continue
            found.append(artifact)
    return found


def read_signature(artifact: Artifact) -> str:
    """The ``.sig`` content (base64 of a minisign signature), checked for shape."""
    sig_path = artifact.sig_path
    if not sig_path.is_file():
        raise ReleaseError(f"{artifact.path}: missing signature {sig_path.name}")
    text = sig_path.read_text(encoding="utf-8").strip()
    try:
        decoded = base64.b64decode(text, validate=True).decode("utf-8")
    except (binascii.Error, UnicodeDecodeError) as e:
        raise ReleaseError(f"{sig_path}: not a Tauri signature file ({e})") from e
    if not decoded.startswith("untrusted comment:"):
        raise ReleaseError(f"{sig_path}: not a minisign signature")
    return text


def asset_name(artifact: Artifact, version: str) -> str:
    """The name the file has on the GitHub release."""
    name = artifact.path.name
    if artifact.installer == "app" and _arch_token(
        name[: -len(".app.tar.gz")], _COMMON_TOKENS
    ) is None:
        product = name[: -len(".app.tar.gz")]
        arch = "aarch64" if artifact.arch == "aarch64" else "x64"
        name = f"{product}_{version}_{arch}.app.tar.gz"
    return re.sub(r"[^A-Za-z0-9._-]", ".", name)


def normalize_version(version: str) -> str:
    v = version.strip()
    if v.startswith("v"):
        v = v[1:]
    if not SEMVER.match(v):
        raise ReleaseError(f"version {version!r} is not a semantic version like 0.2.0")
    return v


def build_manifest(
    artifacts: list[Artifact],
    version: str,
    notes: str,
    pub_date: str,
    repo: str = DEFAULT_REPO,
) -> tuple[dict, dict[Path, str]]:
    """``latest.json`` and the asset name of every artifact."""
    if not artifacts:
        raise ReleaseError("no signed updater artifacts found (each needs its .sig next to it)")
    version = normalize_version(version)
    base = f"https://github.com/{repo}/releases/download/v{quote(version)}"
    platforms: dict[str, dict[str, str]] = {}
    names: dict[Path, str] = {}
    owners: dict[str, Path] = {}
    for artifact in artifacts:
        name = asset_name(artifact, version)
        if name in owners and owners[name] != artifact.path:
            raise ReleaseError(
                f"{artifact.path} and {owners[name]} would both be uploaded as {name}"
            )
        owners[name] = artifact.path
        names[artifact.path] = name
        entry = {"signature": read_signature(artifact), "url": f"{base}/{quote(name)}"}
        for key in artifact.keys:
            if key in platforms:
                raise ReleaseError(f"more than one artifact for {key} (one is {artifact.path})")
            platforms[key] = entry
    manifest = {
        "version": version,
        "notes": notes,
        "pub_date": pub_date,
        "platforms": dict(sorted(platforms.items())),
    }
    return manifest, names


def stage(artifacts: list[Artifact], names: dict[Path, str], manifest: dict, out: Path) -> None:
    """Copies the artifacts, their signatures and ``latest.json`` for upload."""
    out.mkdir(parents=True, exist_ok=True)
    for artifact in artifacts:
        name = names[artifact.path]
        shutil.copy2(artifact.path, out / name)
        shutil.copy2(artifact.sig_path, out / f"{name}.sig")
    write_manifest(manifest, out / "latest.json")


def write_manifest(manifest: dict, path: Path) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(manifest, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")


def utc_now() -> str:
    return datetime.now(timezone.utc).replace(microsecond=0).isoformat().replace("+00:00", "Z")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Build latest.json for the desktop app's updater from signed artifacts."
    )
    parser.add_argument("inputs", nargs="*", type=Path, help="artifact files or folders to search")
    parser.add_argument("--version", required=True, help="the release version, e.g. 0.2.0")
    notes = parser.add_mutually_exclusive_group()
    notes.add_argument("--notes", default="", help="release notes (plain text)")
    notes.add_argument("--notes-file", type=Path, help="read the release notes from this file")
    parser.add_argument("--pub-date", help="RFC 3339 date (default: now, UTC)")
    parser.add_argument("--repo", default=DEFAULT_REPO, help="GitHub owner/name")
    parser.add_argument(
        "--artifact",
        action="append",
        default=[],
        metavar="OS-ARCH-INSTALLER=PATH",
        help="add an artifact explicitly, e.g. darwin-aarch64-app=PATH (repeatable)",
    )
    parser.add_argument("--out", type=Path, help="where to write latest.json")
    parser.add_argument("--stage", type=Path, help="copy artifacts, .sig files and latest.json here")
    args = parser.parse_args(argv)

    try:
        overrides = [parse_override(s) for s in args.artifact]
        skip = frozenset(a.path.resolve() for a in overrides)
        artifacts = find_artifacts(args.inputs, skip) + overrides
        for a in artifacts:
            if not a.path.is_file():
                raise ReleaseError(f"{a.path}: no such file")
        text = args.notes_file.read_text(encoding="utf-8").strip() if args.notes_file else args.notes
        manifest, names = build_manifest(
            artifacts, args.version, text, args.pub_date or utc_now(), args.repo
        )
        if args.stage:
            stage(artifacts, names, manifest, args.stage)
        out = args.out or (None if args.stage else Path("latest.json"))
        if out:
            write_manifest(manifest, out)
    except ReleaseError as e:
        print(f"error: {e}", file=sys.stderr)
        return 1
    for key, entry in manifest["platforms"].items():
        print(f"{key}: {entry['url']}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
