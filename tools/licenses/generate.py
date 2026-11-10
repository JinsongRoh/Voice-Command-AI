#!/usr/bin/env python3
"""Generate THIRD_PARTY_NOTICES.md and the in-app notices JSON.

Sources (nothing is typed by hand except the small tables marked STATIC):

* Rust: ``cargo metadata`` and ``cargo tree`` for the root workspace (server,
  CLI) and the ``apps`` workspace (desktop and mobile apps). ``cargo tree``
  with ``--target all`` gives the crates that are really built for any
  platform (``cargo metadata`` alone over-counts weak optional features);
  ``cargo metadata`` gives each crate's license, repository and directory,
  where the license files are read for copyright lines.
* UI: production dependencies from ``apps/ui/pnpm-lock.yaml``, with license,
  repository and license files from ``apps/ui/node_modules``.
* Android: the Gradle files of the Tauri runtime and of each Tauri plugin in
  the app's dependency tree (plus the generated app project when present).
* License texts: SPDX standard texts in ``tools/licenses/texts``.

Usage::

    python tools/licenses/generate.py          # write the files
    python tools/licenses/generate.py --check  # exit 1 when they are stale

Standard library only. The output is deterministic: no dates, no local paths.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
from dataclasses import dataclass, field
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
TEXTS_DIR = Path(__file__).resolve().parent / "texts"
NOTICES_MD = ROOT / "THIRD_PARTY_NOTICES.md"
NOTICES_JSON = ROOT / "apps" / "ui" / "src" / "licenses" / "notices.json"
UI_DIR = ROOT / "apps" / "ui"
ANDROID_APP_DIR = ROOT / "apps" / "mobile" / "src-tauri" / "gen" / "android"
REPO_URL = "https://github.com/JinsongRoh/Voice-Command-AI"
NOTICES_URL = f"{REPO_URL}/blob/main/THIRD_PARTY_NOTICES.md"

# Packages of the root workspace that are never shipped (test tools).
ROOT_EXCLUDE = ["vca-fake-agent"]
# The shipped apps of the apps workspace.
APP_PACKAGES = ["vca-desktop", "vca-mobile"]

# Component groups (also used by the in-app screen).
GROUP_SERVER = "rust-server"
GROUP_APP = "rust-app"
GROUP_NPM = "npm"
GROUP_FONTS = "fonts"
GROUP_ANDROID = "android"
GROUP_NATIVE = "native"
GROUP_OTHER = "other"
GROUP_TITLES = {
    GROUP_SERVER: "Rust crates: server and CLI (vcad, vca)",
    GROUP_APP: "Rust crates: desktop and mobile apps",
    GROUP_NPM: "UI: npm packages",
    GROUP_FONTS: "Fonts",
    GROUP_ANDROID: "Android libraries (Gradle)",
    GROUP_NATIVE: "Native libraries bundled by Rust crates",
    GROUP_OTHER: "Other credits",
}

# Which of several offered licenses is used, most preferred first. The texts
# of the elected licenses are reproduced; for "A OR B" only one is needed.
PREFERENCE = [
    "MIT",
    "Apache-2.0",
    "ISC",
    "BSD-3-Clause",
    "BSD-2-Clause",
    "Zlib",
    "0BSD",
    "MIT-0",
    "Unlicense",
    "CC0-1.0",
    "BSL-1.0",
    "Unicode-3.0",
    "CDLA-Permissive-2.0",
    "OFL-1.1",
    "MPL-2.0",
    "AFL-2.1",
]
# Licenses whose text names no copyright holder: their notices are the
# copyright lines listed with each component.
NOTICE_LICENSES = {"MIT", "BSD-2-Clause", "BSD-3-Clause", "ISC", "Zlib", "0BSD", "MIT-0"}

UNKNOWN = "UNKNOWN"


# --------------------------------------------------------------------------
# STATIC: facts that cannot be read from a manifest.
# --------------------------------------------------------------------------

# Maven coordinates (group or group:artifact prefix) ->
# (license, repository, copyright holder as stated by the project).
MAVEN_LICENSES: list[tuple[str, str, str, str]] = [
    (
        "androidx.",
        "Apache-2.0",
        "https://android.googlesource.com/platform/frameworks/support",
        "Copyright (C) The Android Open Source Project",
    ),
    (
        "com.google.android.material:",
        "Apache-2.0",
        "https://github.com/material-components/material-components-android",
        "Copyright (C) The Android Open Source Project",
    ),
    (
        "com.fasterxml.jackson.",
        "Apache-2.0",
        "https://github.com/FasterXML/jackson",
        "Copyright (c) FasterXML, LLC and the Jackson project contributors",
    ),
    (
        "org.jetbrains.kotlin:",
        "Apache-2.0",
        "https://github.com/JetBrains/kotlin",
        "Copyright (c) JetBrains s.r.o. and Kotlin Programming Language contributors",
    ),
    (
        "org.jetbrains.kotlinx:",
        "Apache-2.0",
        "https://github.com/Kotlin/kotlinx.coroutines",
        "Copyright (c) JetBrains s.r.o. and Kotlin Programming Language contributors",
    ),
    (
        "com.google.firebase:",
        "Apache-2.0",
        "https://github.com/firebase/firebase-android-sdk",
        "Copyright (c) Google LLC",
    ),
    (
        "com.google.android.gms:play-services-mlkit-",
        "LicenseRef-Google-ML-Kit-Terms",
        "https://developers.google.com/ml-kit/terms",
        "Copyright (c) Google LLC",
    ),
]
# License ids that are not open source; listed with a link instead of a text.
PROPRIETARY = {
    "LicenseRef-Google-ML-Kit-Terms": (
        "Google ML Kit Terms of Service (proprietary, free to redistribute in apps)",
        "https://developers.google.com/ml-kit/terms",
    ),
}

# Native code that a crate compiles or links into the binary. Each entry
# applies only when the crate (and, if given, the feature) is in the tree.
NATIVE_COMPONENTS = [
    {
        "crate": "webview2-com-sys",
        "feature": None,
        "name": "Microsoft Edge WebView2 Loader",
        "license": "BSD-3-Clause",
        "repository": "https://www.nuget.org/packages/Microsoft.Web.WebView2",
        "copyright": ["Copyright (C) Microsoft Corporation. All rights reserved."],
        "note": "Statically linked into the Windows desktop app by webview2-com-sys.",
    },
    {
        "crate": "libdbus-sys",
        "feature": "vendored",
        "name": "libdbus (D-Bus reference library)",
        "license": "AFL-2.1 OR GPL-2.0-or-later",
        "repository": "https://gitlab.freedesktop.org/dbus/dbus",
        "copyright": ["Copyright (C) the D-Bus authors (see the AUTHORS file of the dbus sources)"],
        "note": (
            "Compiled from source into the Linux builds by libdbus-sys (feature "
            "`vendored`); used under the Academic Free License 2.1."
        ),
    },
]

OTHER_CREDITS = [
    {
        "name": "Feather Icons",
        "version": "-",
        "license": "MIT",
        "repository": "https://github.com/feathericons/feather",
        "copyright": ["Copyright (c) 2013-2017 Cole Bemis"],
        "note": "A few icon outlines in apps/ui/src/components/Icon.tsx (such as the settings gear) are adapted from Feather.",
    },
]


# --------------------------------------------------------------------------
# Model
# --------------------------------------------------------------------------


@dataclass
class Component:
    name: str
    version: str
    declared: str  # declared license expression (normalized)
    repository: str
    groups: set[str] = field(default_factory=set)
    copyright: list[str] = field(default_factory=list)
    authors: list[str] = field(default_factory=list)
    notice: str = ""  # Apache-2.0 NOTICE file text
    note: str = ""
    source: str = ""  # MPL-2.0: where the source code is

    @property
    def elected(self) -> str:
        return elect(self.declared)

    def key(self) -> tuple[str, str]:
        return (self.name.lower(), self.version)


# --------------------------------------------------------------------------
# SPDX expressions
# --------------------------------------------------------------------------


def normalize_expression(expr: str | None) -> str:
    """Normalizes legacy separators (``MIT/Apache-2.0``) and spacing."""
    if not expr or not expr.strip():
        return UNKNOWN
    expr = re.sub(r"\s*/\s*", " OR ", expr.strip())
    return re.sub(r"\s+", " ", expr)


def _tokenize(expr: str) -> list[str]:
    return re.findall(r"\(|\)|[^\s()]+", expr)


def parse_expression(expr: str):
    """Parses an SPDX expression into nested ("OR"|"AND", [..]) tuples."""
    tokens = _tokenize(expr)
    pos = 0

    def peek():
        return tokens[pos] if pos < len(tokens) else None

    def take():
        nonlocal pos
        pos += 1
        return tokens[pos - 1]

    def atom():
        tok = take()
        if tok == "(":
            node = or_expr()
            if take() != ")":
                raise ValueError(f"unbalanced expression: {expr}")
            return node
        if peek() == "WITH":
            take()
            return f"{tok} WITH {take()}"
        return tok

    def and_expr():
        items = [atom()]
        while peek() == "AND":
            take()
            items.append(atom())
        return items[0] if len(items) == 1 else ("AND", items)

    def or_expr():
        items = [and_expr()]
        while peek() == "OR":
            take()
            items.append(and_expr())
        return items[0] if len(items) == 1 else ("OR", items)

    node = or_expr()
    if pos != len(tokens):
        raise ValueError(f"cannot parse license expression: {expr}")
    return node


def _rank(lic: str) -> float:
    base, _, exc = lic.partition(" WITH ")
    rank = PREFERENCE.index(base) if base in PREFERENCE else len(PREFERENCE) + 10
    return rank + (0.5 if exc else 0)


def _elect(node) -> list[str]:
    if isinstance(node, str):
        return [node]
    op, items = node
    if op == "AND":
        out: list[str] = []
        for item in items:
            for lic in _elect(item):
                if lic not in out:
                    out.append(lic)
        return out
    options = [_elect(item) for item in items]
    return min(options, key=lambda opt: (sum(_rank(x) for x in opt), len(opt)))


def elect(expr: str) -> str:
    """The licenses used from a declared expression, e.g. ``MIT`` from
    ``MIT OR Apache-2.0``; conjunctions stay (``Apache-2.0 AND ISC``)."""
    if expr == UNKNOWN:
        return UNKNOWN
    chosen = _elect(parse_expression(expr))
    return " AND ".join(sorted(chosen, key=lambda x: (_rank(x), x)))


def license_ids(expr: str) -> list[str]:
    """SPDX ids (licenses and exceptions) needed to reproduce ``expr``."""
    ids: list[str] = []
    for part in expr.split(" AND "):
        for lic in part.split(" WITH "):
            if lic not in ids:
                ids.append(lic)
    return ids


# --------------------------------------------------------------------------
# Copyright lines
# --------------------------------------------------------------------------

_PLACEHOLDER = re.compile(
    r"<year>|<owner>|<copyright holders>|\[yyyy\]|\{yyyy\}|\[year\]|\{year\}|"
    r"\[fullname\]|name of copyright owner|Free Software Foundation",
    re.IGNORECASE,
)
_EMAIL = re.compile(r"<?\b[\w.+-]+@[\w-]+(?:\.[\w-]+)+\b>?")


def clean_line(line: str) -> str:
    """Removes e-mail addresses and tidies whitespace."""
    line = _EMAIL.sub("", line)
    line = re.sub(r"\(\s*\)|<\s*>", "", line)
    line = re.sub(r"\s+", " ", line).strip()
    return re.sub(r"\s+([,.;])", r"\1", line).rstrip(" ,;")


def extract_copyrights(text: str) -> list[str]:
    """Copyright lines of a license or notice file, in file order."""
    lines = text.splitlines()
    out: list[str] = []
    i = 0
    while i < len(lines):
        s = re.sub(r"^[\s#*/;!>-]+", "", lines[i]).strip()
        starts_word = re.match(r"(?i)(copyright\b|©)", s)
        starts_c = re.match(r"(?i)\(c\)", s)
        has_year = re.search(r"\b(1[89]|20)\d\d\b", s)
        has_mark = re.search(r"(?i)\(c\)|©", s)
        j = i + 1
        if ((starts_word and (has_year or has_mark)) or (starts_c and has_year)) and not _PLACEHOLDER.search(s):
            # A notice may wrap after a comma: "..., <newline> with Reserved ...".
            while s.endswith(",") and j < len(lines) and j <= i + 2:
                nxt = lines[j].strip()
                if not nxt or re.match(r"(?i)copyright\b", nxt):
                    break
                s = f"{s} {nxt}"
                j += 1
            cleaned = clean_line(s)
            if cleaned and cleaned not in out:
                out.append(cleaned[:300])
        i = j
    return out


LICENSE_FILE_RE = re.compile(r"(?i)^(licen[cs]e|copying|copyright|notice|unlicense)")


def license_files(directory: Path, extra: list[str] | None = None) -> list[Path]:
    """License-like files at the top of a package directory (and ``dist/``)."""
    found: list[Path] = []
    for sub in (directory, directory / "dist"):
        if not sub.is_dir():
            continue
        for entry in sorted(sub.iterdir(), key=lambda p: p.name):
            if entry.is_file() and LICENSE_FILE_RE.match(entry.name):
                found.append(entry)
        if found:
            break
    for rel in extra or []:
        path = directory / rel
        if path.is_file() and path not in found:
            found.append(path)
    return found


def read_text(path: Path) -> str:
    return path.read_bytes().decode("utf-8", errors="replace").replace("\r\n", "\n")


def collect_file_notices(comp: Component, directory: Path, extra: list[str] | None = None) -> None:
    for path in license_files(directory, extra):
        text = read_text(path)
        for line in extract_copyrights(text):
            if line not in comp.copyright:
                comp.copyright.append(line)
        if path.name.upper().startswith("NOTICE") and "Apache-2.0" in license_ids(comp.elected):
            comp.notice = (comp.notice + "\n\n" + text.strip()).strip()


# --------------------------------------------------------------------------
# Rust
# --------------------------------------------------------------------------


def run(cmd: list[str], cwd: Path) -> str:
    result = subprocess.run(cmd, cwd=cwd, capture_output=True, check=False)
    if result.returncode != 0:
        sys.stderr.write(result.stderr.decode("utf-8", errors="replace"))
        raise SystemExit(f"command failed: {' '.join(cmd)} (in {cwd.name or cwd})")
    return result.stdout.decode("utf-8")


def cargo_tree_set(workspace: Path, selection: list[str]) -> set[tuple[str, str]]:
    out = run(
        [
            "cargo",
            "tree",
            "--locked",
            "-e",
            "normal,build",
            "--target",
            "all",
            "--prefix",
            "none",
            "--format",
            "{p}",
            *selection,
        ],
        workspace,
    )
    found = set()
    for line in out.splitlines():
        m = re.match(r"^(\S+) v(\S+)", line.strip())
        if m:
            found.add((m.group(1), m.group(2)))
    return found


def normalize_repo(url: str | None, fallback: str) -> str:
    if not url:
        return fallback
    url = url.strip()
    url = re.sub(r"^git\+", "", url)
    url = re.sub(r"^git://", "https://", url)
    url = re.sub(r"^ssh://git@", "https://", url)
    url = re.sub(r"^git@([^:]+):", r"https://\1/", url)
    if re.match(r"^[\w.-]+/[\w.-]+$", url):
        url = f"https://github.com/{url}"
    if url.startswith("github:"):
        url = "https://github.com/" + url[len("github:") :]
    url = re.sub(r"\.git$", "", url.rstrip("/"))
    return url


def rust_components() -> tuple[list[Component], dict]:
    comps: dict[tuple[str, str], Component] = {}
    native_flags: dict[str, set[str]] = {}
    workspaces = [
        (ROOT, GROUP_SERVER, ["--workspace", *[a for x in ROOT_EXCLUDE for a in ("--exclude", x)]]),
        (ROOT / "apps", GROUP_APP, [a for x in APP_PACKAGES for a in ("-p", x)]),
    ]
    android_dirs: list[Path] = []
    for workspace, group, selection in workspaces:
        meta = json.loads(run(["cargo", "metadata", "--locked", "--format-version", "1"], workspace))
        packages = {(p["name"], p["version"]): p for p in meta["packages"]}
        features = {n["id"]: set(n.get("features", [])) for n in meta["resolve"]["nodes"]}
        used = cargo_tree_set(workspace, selection)
        for name_version in sorted(used):
            pkg = packages.get(name_version)
            if pkg is None:
                raise SystemExit(f"cargo tree lists {name_version} but cargo metadata does not")
            directory = Path(pkg["manifest_path"]).parent
            if group == GROUP_APP:
                gradle = directory / "android" / "build.gradle.kts"
                if gradle.is_file():
                    android_dirs.append(gradle)
                tauri_android = directory / "mobile" / "android" / "build.gradle.kts"
                if pkg["name"] == "tauri" and tauri_android.is_file():
                    android_dirs.append(tauri_android)
            if pkg["source"] is None:
                continue  # this repository's own crates
            native_flags.setdefault(pkg["name"], set()).update(features.get(pkg["id"], set()))
            comp = comps.get(name_version)
            if comp is None:
                name, version = name_version
                comp = Component(
                    name=name,
                    version=version,
                    declared=normalize_expression(pkg.get("license")),
                    repository=normalize_repo(pkg.get("repository"), f"https://crates.io/crates/{name}"),
                    authors=[a for a in (clean_line(x) for x in pkg.get("authors") or []) if a],
                )
                extra = [pkg["license_file"]] if pkg.get("license_file") else []
                collect_file_notices(comp, directory, extra)
                if "MPL-2.0" in license_ids(comp.elected):
                    comp.source = f"https://crates.io/crates/{name}/{version}"
                comps[name_version] = comp
            comp.groups.add(group)
    return list(comps.values()), {"native_flags": native_flags, "android_gradle": android_dirs}


def native_components(native_flags: dict[str, set[str]]) -> list[Component]:
    out = []
    for item in NATIVE_COMPONENTS:
        flags = native_flags.get(item["crate"])
        if flags is None or (item["feature"] and item["feature"] not in flags):
            continue
        out.append(
            Component(
                name=item["name"],
                version=f"via {item['crate']}",
                declared=item["license"],
                repository=item["repository"],
                groups={GROUP_NATIVE},
                copyright=list(item["copyright"]),
                note=item["note"],
            )
        )
    return out


# --------------------------------------------------------------------------
# npm (pnpm lockfile + node_modules)
# --------------------------------------------------------------------------


def _unquote(s: str) -> str:
    s = s.strip()
    if len(s) >= 2 and s[0] == s[-1] and s[0] in "'\"":
        return s[1:-1]
    return s


def parse_pnpm_lock(text: str) -> tuple[dict[str, str], dict[str, dict[str, str]]]:
    """Returns (root production deps name->version, snapshot key->deps)."""
    root: dict[str, str] = {}
    snapshots: dict[str, dict[str, str]] = {}
    section = None
    importer = None
    dep_kind = None
    current_dep = None
    snap_key = None
    for raw in text.splitlines():
        if not raw.strip() or raw.lstrip().startswith("#"):
            continue
        indent = len(raw) - len(raw.lstrip(" "))
        line = raw.strip()
        if indent == 0:
            section = line.rstrip(":")
            continue
        if section == "importers":
            if indent == 2:
                importer = _unquote(line.rstrip(":"))
                dep_kind = None
            elif indent == 4:
                dep_kind = line.rstrip(":")
            elif indent == 6 and importer == "." and dep_kind in ("dependencies", "optionalDependencies"):
                current_dep = _unquote(line.rstrip(":"))
            elif indent == 8 and importer == "." and dep_kind in ("dependencies", "optionalDependencies"):
                key, _, value = line.partition(":")
                if key.strip() == "version" and current_dep:
                    root[current_dep] = _unquote(value)
        elif section == "snapshots":
            if indent == 2:
                if line.endswith(": {}"):
                    body = line[: -len(": {}")]
                else:
                    body = line[:-1] if line.endswith(":") else line
                snap_key = _unquote(body)
                snapshots[snap_key] = {}
                dep_kind = None
            elif indent == 4:
                dep_kind = line.rstrip(":")
            elif indent == 6 and dep_kind in ("dependencies", "optionalDependencies") and snap_key:
                key, _, value = line.partition(": ")
                snapshots[snap_key][_unquote(key)] = _unquote(value)
    return root, snapshots


def split_name_version(key: str) -> tuple[str, str]:
    at = key.index("@", 1)
    name, version = key[:at], key[at + 1 :]
    return name, version.split("(", 1)[0]


def npm_closure(root: dict[str, str], snapshots: dict[str, dict[str, str]]) -> list[tuple[str, str]]:
    seen: set[str] = set()
    stack = [f"{n}@{v}" for n, v in root.items() if not v.startswith("link:")]
    while stack:
        key = stack.pop()
        if key in seen:
            continue
        seen.add(key)
        for name, version in snapshots.get(key, {}).items():
            if not version.startswith("link:"):
                stack.append(f"{name}@{version}")
    return sorted({split_name_version(k) for k in seen})


def npm_package_dir(name: str, version: str) -> Path:
    store = UI_DIR / "node_modules" / ".pnpm"
    base = f"{name.replace('/', '+')}@{version}"
    if store.is_dir():
        for entry in sorted(os.listdir(store)):
            if entry == base or entry.startswith(base + "_") or entry.startswith(base + "("):
                path = store / entry / "node_modules" / name
                if (path / "package.json").is_file():
                    return path
    direct = UI_DIR / "node_modules" / name
    if (direct / "package.json").is_file():
        meta = json.loads(read_text(direct / "package.json"))
        if meta.get("version") == version:
            return direct
    raise SystemExit(
        f"{name}@{version} is not installed; run `pnpm install` in apps/ui first"
    )


def npm_components() -> list[Component]:
    lock = read_text(UI_DIR / "pnpm-lock.yaml")
    root, snapshots = parse_pnpm_lock(lock)
    out = []
    for name, version in npm_closure(root, snapshots):
        directory = npm_package_dir(name, version)
        meta = json.loads(read_text(directory / "package.json"))
        lic = meta.get("license")
        if isinstance(lic, dict):
            lic = lic.get("type")
        repo = meta.get("repository")
        if isinstance(repo, dict):
            repo = repo.get("url")
        author = meta.get("author")
        if isinstance(author, dict):
            author = author.get("name")
        declared = normalize_expression(lic)
        comp = Component(
            name=name,
            version=version,
            declared=declared,
            repository=normalize_repo(repo, f"https://www.npmjs.com/package/{name}"),
            groups={GROUP_FONTS if "OFL-1.1" in license_ids(elect(declared)) else GROUP_NPM},
            authors=[clean_line(author)] if author else [],
        )
        collect_file_notices(comp, directory)
        out.append(comp)
    return out


# --------------------------------------------------------------------------
# Android (Gradle)
# --------------------------------------------------------------------------

_GRADLE_DEP = re.compile(r'^\s*(?:implementation|api)\(\s*"([^"]+)"\s*\)', re.MULTILINE)
_GRADLE_VAL = re.compile(r'^\s*val\s+(\w+)\s*=\s*"([^"]+)"', re.MULTILINE)
_KOTLIN_PLUGIN = re.compile(r'"org\.jetbrains\.kotlin:kotlin-gradle-plugin:([^"]+)"')


def _version_key(v: str) -> tuple:
    return tuple(int(x) if x.isdigit() else x for x in re.split(r"[.\-]", v))


def gradle_coordinates(texts: list[str]) -> dict[str, str]:
    """group:artifact -> highest declared version (Gradle picks the highest)."""
    coords: dict[str, str] = {}
    for text in texts:
        values = dict(_GRADLE_VAL.findall(text))
        for dep in _GRADLE_DEP.findall(text):
            dep = re.sub(r"\$\{(\w+)\}|\$(\w+)", lambda m: values.get(m.group(1) or m.group(2), m.group(0)), dep)
            parts = dep.split(":")
            if len(parts) < 3:
                continue
            ga, version = f"{parts[0]}:{parts[1]}", parts[2]
            if ga not in coords or _version_key(version) > _version_key(coords[ga]):
                coords[ga] = version
        for version in _KOTLIN_PLUGIN.findall(text):
            ga = "org.jetbrains.kotlin:kotlin-stdlib"
            if ga not in coords or _version_key(version) > _version_key(coords[ga]):
                coords[ga] = version
    return coords


def maven_license(ga: str) -> tuple[str, str, list[str]]:
    for prefix, lic, repo, holder in MAVEN_LICENSES:
        if ga.startswith(prefix):
            return lic, repo, [holder]
    return UNKNOWN, f"https://mvnrepository.com/artifact/{ga.replace(':', '/')}", []


def android_components(gradle_files: list[Path]) -> list[Component]:
    files = list(gradle_files)
    for rel in ("build.gradle.kts", "app/build.gradle.kts"):
        if (ANDROID_APP_DIR / rel).is_file():
            files.append(ANDROID_APP_DIR / rel)
    coords = gradle_coordinates([read_text(f) for f in sorted(set(files), key=str)])
    out = []
    for ga, version in sorted(coords.items()):
        lic, repo, holders = maven_license(ga)
        out.append(
            Component(
                name=ga,
                version=version,
                declared=lic,
                repository=repo,
                groups={GROUP_ANDROID},
                copyright=holders,
            )
        )
    return out


def other_components() -> list[Component]:
    return [
        Component(
            name=item["name"],
            version=item["version"],
            declared=item["license"],
            repository=item["repository"],
            groups={GROUP_OTHER},
            copyright=list(item["copyright"]),
            note=item["note"],
        )
        for item in OTHER_CREDITS
    ]


# --------------------------------------------------------------------------
# License texts
# --------------------------------------------------------------------------


def license_text(spdx_id: str) -> str:
    path = TEXTS_DIR / f"{spdx_id}.txt"
    if not path.is_file():
        raise SystemExit(f"missing license text tools/licenses/texts/{spdx_id}.txt")
    return read_text(path).strip() + "\n"


def needed_texts(components: list[Component]) -> list[str]:
    ids: set[str] = set()
    for comp in components:
        for lic in license_ids(comp.elected):
            if lic != UNKNOWN and lic not in PROPRIETARY:
                ids.add(lic)
    return sorted(ids, key=lambda x: (_rank(x), x))


# --------------------------------------------------------------------------
# Rendering
# --------------------------------------------------------------------------


def _sorted(components: list[Component]) -> list[Component]:
    return sorted(components, key=lambda c: (c.name.lower(), _version_key(c.version), c.version))


def count_by_license(components: list[Component]) -> list[tuple[str, int]]:
    counts: dict[str, int] = {}
    for comp in components:
        counts[comp.elected] = counts.get(comp.elected, 0) + 1
    return sorted(counts.items(), key=lambda kv: (-kv[1], kv[0]))


def flagged(components: list[Component]) -> list[tuple[Component, str]]:
    out = []
    for comp in _sorted(components):
        ids = license_ids(comp.elected)
        if UNKNOWN in ids:
            out.append((comp, "no license declared"))
        elif any(i in PROPRIETARY for i in ids):
            out.append((comp, "proprietary terms (not open source)"))
        elif any(i not in PREFERENCE and i != "LLVM-exception" for i in ids):
            out.append((comp, "license outside the reviewed list"))
    return out


def _anchor(text: str) -> str:
    return re.sub(r"[^a-z0-9 -]", "", text.lower()).replace(" ", "-")


def _attribution(comp: Component) -> list[str]:
    if comp.copyright:
        return comp.copyright
    if comp.authors:
        return [f"Copyright (c) {', '.join(comp.authors)} (no copyright line in the package; authors from its manifest)"]
    return [f"Copyright (c) the {comp.name} authors (no copyright line in the package)"]


def _entry(comp: Component, with_groups: bool = False) -> list[str]:
    head = f"- `{comp.name}` {comp.version}"
    if comp.declared != comp.elected:
        head += f" (declared `{comp.declared}`)"
    head += f" - <{comp.repository}>"
    if with_groups:
        scopes = []
        if GROUP_SERVER in comp.groups:
            scopes.append("server/CLI")
        if GROUP_APP in comp.groups:
            scopes.append("apps")
        head += f" - {', '.join(scopes)}"
    lines = [head]
    if comp.note:
        lines.append(f"  - {comp.note}")
    for line in _attribution(comp):
        lines.append(f"  - {line}")
    return lines


def render_markdown(components: list[Component]) -> str:
    rust = [c for c in components if c.groups & {GROUP_SERVER, GROUP_APP}]
    sections = [
        ("Rust crates", rust, True),
        (GROUP_TITLES[GROUP_NPM], [c for c in components if GROUP_NPM in c.groups], False),
        (GROUP_TITLES[GROUP_FONTS], [c for c in components if GROUP_FONTS in c.groups], False),
        (GROUP_TITLES[GROUP_ANDROID], [c for c in components if GROUP_ANDROID in c.groups], False),
        (GROUP_TITLES[GROUP_NATIVE], [c for c in components if GROUP_NATIVE in c.groups], False),
        (GROUP_TITLES[GROUP_OTHER], [c for c in components if GROUP_OTHER in c.groups], False),
    ]
    texts = needed_texts(components)
    out: list[str] = []
    w = out.append
    w("<!-- Generated by tools/licenses/generate.py. Do not edit by hand; run the script. -->")
    w("")
    w("# Third-party notices")
    w("")
    w(
        "Voice Command AI is MIT-licensed (see [LICENSE](LICENSE)). Its desktop and mobile "
        "apps, server (`vcad`) and CLI (`vca`) include the open-source components listed "
        "below, each under its own license. Where a component offers a choice of licenses "
        "(for example `MIT OR Apache-2.0`), it is used under the license shown as its group; "
        "the declared expression is given next to it."
    )
    w("")
    w(
        "Each component is listed with its version, repository and the copyright notices "
        "found in its license files. The full text of every license used follows once at "
        "the end; for MIT, BSD, ISC and Zlib the copyright notices listed with each "
        "component complete the license text. A readable summary is in "
        "[docs/open-source.md](docs/open-source.md)."
    )
    w("")
    w("## Summary")
    w("")
    w("| License | Components |")
    w("| --- | ---: |")
    for lic, n in count_by_license(components):
        w(f"| {lic} | {n} |")
    w(f"| **Total** | **{len(components)}** |")
    w("")
    w("| Group | Components |")
    w("| --- | ---: |")
    for group in (GROUP_SERVER, GROUP_APP, GROUP_NPM, GROUP_FONTS, GROUP_ANDROID, GROUP_NATIVE, GROUP_OTHER):
        n = sum(1 for c in components if group in c.groups)
        w(f"| {GROUP_TITLES[group]} | {n} |")
    w("")
    w("Rust crates used by both the server and the apps are listed once.")
    w("")
    w("## Contents")
    w("")
    for title, items, _ in sections:
        if items:
            w(f"- [{title}](#{_anchor(title)})")
    for title in (
        "MPL-2.0 components and their source code",
        "Not bundled: reference tools, engines and model weights",
        "License texts",
    ):
        w(f"- [{title}](#{_anchor(title)})")
    w("")
    for title, items, with_groups in sections:
        if not items:
            continue
        w(f"## {title}")
        w("")
        if title == GROUP_TITLES[GROUP_FONTS]:
            w(
                "The fonts are embedded in the app UI under the SIL Open Font License 1.1. "
                "They are not sold on their own and keep their names and copyright notices."
            )
            w("")
        if title == GROUP_TITLES[GROUP_ANDROID]:
            w(
                "Direct Gradle dependencies of the Android app, the Tauri runtime and its "
                "plugins. Gradle adds their transitive dependencies (more AndroidX, Kotlin "
                "and Material libraries under Apache-2.0, and Google Play services libraries "
                "under Google's terms for ML Kit)."
            )
            w("")
        for lic, _ in count_by_license(items):
            group_items = [c for c in _sorted(items) if c.elected == lic]
            w(f"### {lic} ({len(group_items)})")
            w("")
            if lic in PROPRIETARY:
                label, url = PROPRIETARY[lic]
                w(f"{label}: <{url}>")
                w("")
            for comp in group_items:
                out.extend(_entry(comp, with_groups))
            w("")
    w("## MPL-2.0 components and their source code")
    w("")
    w(
        "These components are under the Mozilla Public License 2.0, a file-level copyleft "
        "license. They are used unmodified, as published on crates.io. Their source code "
        "is available at the links below; the license text is in [License texts](#license-texts)."
    )
    w("")
    for comp in _sorted([c for c in components if "MPL-2.0" in license_ids(c.elected)]):
        w(f"- `{comp.name}` {comp.version}: source <{comp.source or comp.repository}>, repository <{comp.repository}>")
    w("")
    w("## Not bundled: reference tools, engines and model weights")
    w("")
    w(
        "- The reference servers in `tools/stt-server` (on top of faster-whisper and "
        "CTranslate2, MIT) and `tools/tts-server` (for MeloTTS, MIT, or Supertonic: code "
        "MIT, model weights OpenRAIL-M) are optional helpers that you install yourself. "
        "They are not part of the apps or their installers."
    )
    w(
        "- No speech-to-text, text-to-speech or language model weights are shipped. "
        "Engines and models (local or cloud), and connectivity services such as "
        "DNSPilot, are provided and configured by the user."
    )
    w(
        "- You are responsible for complying with the licenses and terms of the engines, "
        "models and services you install or connect, including any use restrictions or "
        "disclosure requirements."
    )
    w("")
    notices = [c for c in _sorted(components) if c.notice]
    if notices:
        w("## Apache-2.0 NOTICE files")
        w("")
        for comp in notices:
            w(f"### {comp.name} {comp.version}")
            w("")
            w("```text")
            w(comp.notice)
            w("```")
            w("")
    w("## License texts")
    w("")
    for lic in texts:
        w(f"### {lic}")
        w("")
        if lic in NOTICE_LICENSES:
            w("The copyright notices for this license are listed with each component above.")
            w("")
        w("```text")
        out.append(license_text(lic).rstrip("\n"))
        w("```")
        w("")
    return "\n".join(out).rstrip("\n") + "\n"


def render_json(components: list[Component]) -> str:
    order = (GROUP_SERVER, GROUP_APP, GROUP_NPM, GROUP_FONTS, GROUP_ANDROID, GROUP_NATIVE, GROUP_OTHER)
    data = {
        "generator": "tools/licenses/generate.py",
        "notices": NOTICES_URL,
        "groups": list(order),
        "counts": [[lic, n] for lic, n in count_by_license(components)],
        "packages": [
            {
                "n": c.name,
                "v": c.version,
                "l": c.elected,
                "d": c.declared,
                "r": c.repository,
                "g": [g for g in order if g in c.groups],
                "c": _attribution(c),
                **({"s": c.source} if c.source else {}),
                **({"x": c.note} if c.note else {}),
            }
            for c in _sorted(components)
        ],
        "texts": {lic: license_text(lic) for lic in needed_texts(components)},
        "proprietary": {k: {"label": v[0], "url": v[1]} for k, v in sorted(PROPRIETARY.items())},
    }
    return json.dumps(data, ensure_ascii=False, separators=(",", ":"), sort_keys=False) + "\n"


def collect() -> list[Component]:
    rust, extra = rust_components()
    components = rust + npm_components() + android_components(extra["android_gradle"])
    components += native_components(extra["native_flags"]) + other_components()
    return _sorted(components)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--check", action="store_true", help="exit 1 when the generated files are stale")
    args = parser.parse_args(argv)

    components = collect()
    outputs = {NOTICES_MD: render_markdown(components), NOTICES_JSON: render_json(components)}

    for comp, reason in flagged(components):
        sys.stderr.write(f"review: {comp.name} {comp.version} ({comp.declared}): {reason}\n")

    stale = []
    for path, content in outputs.items():
        # Compare without line-ending differences (Git may check out CRLF).
        current = read_text(path) if path.is_file() else None
        if current != content:
            stale.append(path)
            if not args.check:
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(content.encode("utf-8"))
    rel = [p.relative_to(ROOT).as_posix() for p in stale]
    if args.check:
        if stale:
            sys.stderr.write(f"stale: {', '.join(rel)}; run python tools/licenses/generate.py\n")
            return 1
        print(f"third-party notices are up to date ({len(components)} components)")
        return 0
    print(f"wrote {', '.join(rel) or 'nothing (up to date)'} ({len(components)} components)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
