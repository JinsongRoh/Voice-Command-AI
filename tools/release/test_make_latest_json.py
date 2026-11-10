"""Tests for the updater manifest builder (run: python -m pytest tools/release)."""

from __future__ import annotations

import base64
import importlib.util
import json
import sys
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parent
_spec = importlib.util.spec_from_file_location("vca_make_latest_json", HERE / "make-latest-json.py")
mlj = importlib.util.module_from_spec(_spec)
sys.modules[_spec.name] = mlj
_spec.loader.exec_module(mlj)

VERSION = "0.2.0"
BASE = "https://github.com/JinsongRoh/Voice-Command-AI/releases/download/v0.2.0"


def sig_text(label: str) -> str:
    """A `.sig` file as Tauri writes it: base64 of the minisign text."""
    minisign = (
        "untrusted comment: signature from tauri secret key\n"
        f"RUT{label}\n"
        f"trusted comment: timestamp:1\tfile:{label}\n"
        "abc\n"
    )
    return base64.b64encode(minisign.encode()).decode()


def make(dir_: Path, rel: str, signed: bool = True) -> Path:
    path = dir_ / rel
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(b"payload " + rel.encode())
    if signed:
        path.with_name(path.name + ".sig").write_text(sig_text(path.name) + "\n")
    return path


@pytest.fixture
def bundle(tmp_path: Path) -> Path:
    """Artifacts of every platform, in Tauri's own bundle layout and names."""
    b = tmp_path / "bundle"
    make(b, "nsis/Voice Command AI_0.2.0_x64-setup.exe")
    make(b, "msi/Voice Command AI_0.2.0_x64_en-US.msi", signed=False)
    make(b, "macos/Voice Command AI_0.2.0_aarch64.app.tar.gz")
    make(b, "macos/Voice Command AI_0.2.0_x64.app.tar.gz")
    make(b, "dmg/Voice Command AI_0.2.0_aarch64.dmg", signed=False)
    make(b, "appimage/Voice Command AI_0.2.0_amd64.AppImage")
    make(b, "appimage/Voice Command AI_0.2.0_aarch64.AppImage")
    make(b, "deb/Voice Command AI_0.2.0_amd64.deb")
    make(b, "deb/Voice Command AI_0.2.0_arm64.deb")
    make(b, "rpm/Voice Command AI-0.2.0-1.x86_64.rpm")
    make(b, "rpm/Voice Command AI-0.2.0-1.aarch64.rpm")
    return b


def run(*args: str) -> int:
    return mlj.main(list(args))


def test_every_platform_gets_its_entry(bundle: Path, tmp_path: Path) -> None:
    out = tmp_path / "latest.json"
    assert run("--version", "v0.2.0", "--notes", "Fixes", "--pub-date", "2026-11-01T00:00:00Z",
               "--out", str(out), str(bundle)) == 0
    manifest = json.loads(out.read_text(encoding="utf-8"))
    assert manifest["version"] == VERSION
    assert manifest["notes"] == "Fixes"
    assert manifest["pub_date"] == "2026-11-01T00:00:00Z"
    platforms = manifest["platforms"]
    assert sorted(platforms) == [
        "darwin-aarch64",
        "darwin-aarch64-app",
        "darwin-x86_64",
        "darwin-x86_64-app",
        "linux-aarch64-appimage",
        "linux-aarch64-deb",
        "linux-aarch64-rpm",
        "linux-x86_64-appimage",
        "linux-x86_64-deb",
        "linux-x86_64-rpm",
        "windows-x86_64",
        "windows-x86_64-nsis",
    ]
    assert platforms["windows-x86_64"]["url"] == (
        f"{BASE}/Voice.Command.AI_0.2.0_x64-setup.exe"
    )
    assert platforms["windows-x86_64"] == platforms["windows-x86_64-nsis"]
    assert platforms["darwin-aarch64"]["url"] == f"{BASE}/Voice.Command.AI_0.2.0_aarch64.app.tar.gz"
    assert platforms["darwin-x86_64"]["url"] == f"{BASE}/Voice.Command.AI_0.2.0_x64.app.tar.gz"
    assert platforms["linux-x86_64-appimage"]["url"] == (
        f"{BASE}/Voice.Command.AI_0.2.0_amd64.AppImage"
    )
    assert platforms["linux-aarch64-appimage"]["url"] == (
        f"{BASE}/Voice.Command.AI_0.2.0_aarch64.AppImage"
    )
    assert platforms["linux-aarch64-deb"]["url"] == f"{BASE}/Voice.Command.AI_0.2.0_arm64.deb"
    assert platforms["linux-x86_64-rpm"]["url"] == f"{BASE}/Voice.Command.AI-0.2.0-1.x86_64.rpm"
    assert platforms["linux-aarch64-rpm"]["url"] == f"{BASE}/Voice.Command.AI-0.2.0-1.aarch64.rpm"
    # The signature is the .sig content itself.
    assert platforms["linux-aarch64-appimage"]["signature"] == sig_text(
        "Voice Command AI_0.2.0_aarch64.AppImage"
    )


def test_linux_entries_always_name_the_package_type(bundle: Path) -> None:
    """A .deb install must never be pointed at an AppImage (or the reverse)."""
    manifest, _ = mlj.build_manifest(mlj.find_artifacts([bundle]), VERSION, "", "d")
    assert "linux-x86_64" not in manifest["platforms"]
    assert "linux-aarch64" not in manifest["platforms"]


def test_only_present_platforms_are_listed(tmp_path: Path) -> None:
    make(tmp_path, "Voice Command AI_0.2.0_x64-setup.exe")
    make(tmp_path, "Voice Command AI_0.2.0_aarch64.AppImage")
    manifest, _ = mlj.build_manifest(mlj.find_artifacts([tmp_path]), VERSION, "", "d")
    assert sorted(manifest["platforms"]) == [
        "linux-aarch64-appimage",
        "windows-x86_64",
        "windows-x86_64-nsis",
    ]


def test_unsigned_installers_are_skipped(tmp_path: Path) -> None:
    make(tmp_path, "Voice Command AI_0.2.0_x64-setup.exe", signed=False)
    make(tmp_path, "Voice Command AI_0.2.0_amd64.AppImage")
    artifacts = mlj.find_artifacts([tmp_path])
    assert [a.installer for a in artifacts] == ["appimage"]


def test_mac_archives_without_an_architecture_need_it_given(tmp_path: Path) -> None:
    arm = make(tmp_path / "arm", "Voice Command AI.app.tar.gz")
    intel = make(tmp_path / "intel", "Voice Command AI.app.tar.gz")
    with pytest.raises(mlj.ReleaseError, match="does not name its architecture"):
        mlj.find_artifacts([tmp_path])
    stage = tmp_path / "stage"
    assert run("--version", VERSION, "--stage", str(stage),
               "--artifact", f"darwin-aarch64-app={arm}",
               "--artifact", f"darwin-x86_64-app={intel}",
               str(tmp_path / "arm"), str(tmp_path / "intel")) == 0
    manifest = json.loads((stage / "latest.json").read_text(encoding="utf-8"))
    assert manifest["platforms"]["darwin-aarch64"]["url"] == (
        f"{BASE}/Voice.Command.AI_0.2.0_aarch64.app.tar.gz"
    )
    assert manifest["platforms"]["darwin-x86_64-app"]["url"] == (
        f"{BASE}/Voice.Command.AI_0.2.0_x64.app.tar.gz"
    )
    # Staged under the URL names, each with its signature.
    for name in ("Voice.Command.AI_0.2.0_aarch64.app.tar.gz", "Voice.Command.AI_0.2.0_x64.app.tar.gz"):
        assert (stage / name).is_file()
        assert (stage / f"{name}.sig").is_file()
    assert (stage / "Voice.Command.AI_0.2.0_aarch64.app.tar.gz").read_bytes() == arm.read_bytes()


def test_stage_copies_everything_under_the_url_names(bundle: Path, tmp_path: Path) -> None:
    stage = tmp_path / "stage"
    assert run("--version", VERSION, "--stage", str(stage), str(bundle)) == 0
    manifest = json.loads((stage / "latest.json").read_text(encoding="utf-8"))
    for entry in manifest["platforms"].values():
        name = entry["url"].rsplit("/", 1)[1]
        assert (stage / name).is_file(), name
        assert (stage / f"{name}.sig").read_text().strip() == entry["signature"]


def test_notes_file(tmp_path: Path) -> None:
    make(tmp_path / "a", "Voice Command AI_0.2.0_amd64.AppImage")
    notes = tmp_path / "notes.md"
    notes.write_text("Line one\nLine two\n", encoding="utf-8")
    out = tmp_path / "latest.json"
    assert run("--version", VERSION, "--notes-file", str(notes), "--out", str(out),
               str(tmp_path / "a")) == 0
    assert json.loads(out.read_text(encoding="utf-8"))["notes"] == "Line one\nLine two"


def test_default_pub_date_is_utc_rfc3339(tmp_path: Path) -> None:
    make(tmp_path, "Voice Command AI_0.2.0_amd64.AppImage")
    manifest, _ = mlj.build_manifest(mlj.find_artifacts([tmp_path]), VERSION, "", mlj.utc_now())
    assert manifest["pub_date"].endswith("Z")
    assert "T" in manifest["pub_date"]


@pytest.mark.parametrize("version", ["0.2", "latest", "1.0.0.0", ""])
def test_rejects_versions_that_are_not_semver(tmp_path: Path, version: str) -> None:
    make(tmp_path, "Voice Command AI_0.2.0_amd64.AppImage")
    with pytest.raises(mlj.ReleaseError, match="semantic version"):
        mlj.build_manifest(mlj.find_artifacts([tmp_path]), version, "", "d")


def test_prerelease_versions_are_allowed() -> None:
    assert mlj.normalize_version("v1.2.3-beta.1") == "1.2.3-beta.1"


def test_rejects_duplicates(tmp_path: Path) -> None:
    make(tmp_path / "a", "Voice Command AI_0.2.0_x64-setup.exe")
    make(tmp_path / "b", "VCA_0.2.0_x64-setup.exe")
    with pytest.raises(mlj.ReleaseError, match="more than one artifact for windows-x86_64"):
        mlj.build_manifest(mlj.find_artifacts([tmp_path]), VERSION, "", "d")


def test_rejects_bad_signatures(tmp_path: Path) -> None:
    path = make(tmp_path, "Voice Command AI_0.2.0_amd64.AppImage")
    path.with_name(path.name + ".sig").write_text("not base64 !!!")
    with pytest.raises(mlj.ReleaseError, match="not a Tauri signature"):
        mlj.build_manifest(mlj.find_artifacts([tmp_path]), VERSION, "", "d")
    path.with_name(path.name + ".sig").write_text(base64.b64encode(b"hello").decode())
    with pytest.raises(mlj.ReleaseError, match="not a minisign signature"):
        mlj.build_manifest(mlj.find_artifacts([tmp_path]), VERSION, "", "d")


def test_requires_at_least_one_artifact(tmp_path: Path, capsys: pytest.CaptureFixture[str]) -> None:
    assert run("--version", VERSION, "--out", str(tmp_path / "latest.json"), str(tmp_path)) == 1
    assert "no signed updater artifacts" in capsys.readouterr().err
    assert not (tmp_path / "latest.json").exists()


@pytest.mark.parametrize(
    "spec",
    ["linux-aarch64=x", "linux-arm-deb=x", "darwin-x86_64-deb=x", "windows-x86_64-nsis", "a-b-c-d=x"],
)
def test_rejects_bad_artifact_options(spec: str) -> None:
    with pytest.raises(mlj.ReleaseError):
        mlj.parse_override(spec)


def test_override_for_a_linux_arm64_deb(tmp_path: Path) -> None:
    deb = make(tmp_path, "vca.deb")
    artifact = mlj.parse_override(f"linux-aarch64-deb={deb}")
    assert artifact.keys == ["linux-aarch64-deb"]
    manifest, _ = mlj.build_manifest([artifact], VERSION, "", "d")
    assert manifest["platforms"]["linux-aarch64-deb"]["url"] == f"{BASE}/vca.deb"


def test_the_manifest_shape_matches_the_updater(bundle: Path) -> None:
    """Fields the Tauri updater reads (`version`, `notes`, `pub_date`, `platforms.*.url/signature`)."""
    manifest, _ = mlj.build_manifest(mlj.find_artifacts([bundle]), VERSION, "n", "2026-11-01T00:00:00Z")
    assert set(manifest) == {"version", "notes", "pub_date", "platforms"}
    for entry in manifest["platforms"].values():
        assert set(entry) == {"url", "signature"}
        assert entry["url"].startswith(f"{BASE}/")
