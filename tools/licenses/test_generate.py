"""Tests for the third-party notices generator (run: python -m pytest tools/licenses)."""

from __future__ import annotations

import importlib.util
import shutil
import sys
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parent
_spec = importlib.util.spec_from_file_location("vca_license_generate", HERE / "generate.py")
gen = importlib.util.module_from_spec(_spec)
sys.modules[_spec.name] = gen
_spec.loader.exec_module(gen)


@pytest.mark.parametrize(
    ("declared", "elected"),
    [
        ("MIT", "MIT"),
        ("MIT/Apache-2.0", "MIT"),
        ("Apache-2.0 OR MIT", "MIT"),
        ("Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT", "MIT"),
        ("0BSD OR Apache-2.0", "Apache-2.0"),
        ("(MIT OR Apache-2.0) AND Unicode-3.0", "MIT AND Unicode-3.0"),
        ("Apache-2.0 AND ISC", "Apache-2.0 AND ISC"),
        ("Apache-2.0 WITH LLVM-exception", "Apache-2.0 WITH LLVM-exception"),
        ("MIT OR Apache-2.0 OR LGPL-2.1-or-later", "MIT"),
        ("AFL-2.1 OR GPL-2.0-or-later", "AFL-2.1"),
        (None, "UNKNOWN"),
    ],
)
def test_elects_the_preferred_license(declared, elected):
    assert gen.elect(gen.normalize_expression(declared)) == elected


def test_license_ids_split_exceptions():
    assert gen.license_ids("Apache-2.0 WITH LLVM-exception") == ["Apache-2.0", "LLVM-exception"]
    assert gen.license_ids("MIT AND Unicode-3.0") == ["MIT", "Unicode-3.0"]


def test_extracts_copyright_lines_and_drops_templates_and_emails():
    text = "\n".join(
        [
            "MIT License",
            "",
            "Copyright (c) 2019 Jane Doe <jane@example.com>",
            "Copyright (c) <year> <copyright holders>",
            "   Copyright [yyyy] [name of copyright owner]",
            "copyright notice that is included in or attached to the work",
            "THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE",
            "(c) You must retain all notices",
            "Copyright (c) 2021, Some Font Author (https://example.org/font),",
            "with Reserved Font Name 'Example'.",
            "",
            "Copyright (c) 2019 Jane Doe",
        ]
    )
    assert gen.extract_copyrights(text) == [
        "Copyright (c) 2019 Jane Doe",
        "Copyright (c) 2021, Some Font Author (https://example.org/font), with Reserved Font Name 'Example'.",
    ]


def test_parses_production_deps_from_the_pnpm_lockfile():
    lock = """lockfileVersion: '9.0'

importers:

  .:
    dependencies:
      react-dom:
        specifier: ^19.0.0
        version: 19.3.1(react@19.3.1)
      '@scope/pkg':
        specifier: ^1.0.0
        version: 1.0.0
    devDependencies:
      vitest:
        specifier: ^5.0.0
        version: 5.0.5

snapshots:

  '@scope/pkg@1.0.0': {}

  react-dom@19.3.1(react@19.3.1):
    dependencies:
      react: 19.3.1
      scheduler: 0.28.0

  react@19.3.1: {}

  scheduler@0.28.0: {}

  vitest@5.0.5:
    dependencies:
      chai: 5.0.0
"""
    root, snapshots = gen.parse_pnpm_lock(lock)
    assert root == {"react-dom": "19.3.1(react@19.3.1)", "@scope/pkg": "1.0.0"}
    assert gen.npm_closure(root, snapshots) == [
        ("@scope/pkg", "1.0.0"),
        ("react", "19.3.1"),
        ("react-dom", "19.3.1"),
        ("scheduler", "0.28.0"),
    ]


def test_reads_gradle_dependencies_with_variables():
    a = 'val camerax_version = "1.5.1"\ndependencies {\n  implementation("androidx.camera:camera-core:${camerax_version}")\n  implementation("androidx.core:core-ktx:1.7.0")\n  testImplementation("junit:junit:4.13.2")\n  implementation(project(":tauri-android"))\n}\n'
    b = 'classpath("org.jetbrains.kotlin:kotlin-gradle-plugin:2.2.10")\nimplementation("androidx.core:core-ktx:1.9.0")\n'
    assert gen.gradle_coordinates([a, b]) == {
        "androidx.camera:camera-core": "1.5.1",
        "androidx.core:core-ktx": "1.9.0",
        "org.jetbrains.kotlin:kotlin-stdlib": "2.2.10",
    }


def test_normalizes_repository_urls():
    assert gen.normalize_repo("git+https://github.com/a/b.git", "x") == "https://github.com/a/b"
    assert gen.normalize_repo("a/b", "x") == "https://github.com/a/b"
    assert gen.normalize_repo(None, "fallback") == "fallback"


def test_every_reviewed_license_has_a_text():
    for spdx_id in gen.PREFERENCE + ["LLVM-exception"]:
        assert (gen.TEXTS_DIR / f"{spdx_id}.txt").is_file(), spdx_id


@pytest.mark.skipif(shutil.which("cargo") is None, reason="cargo is not installed")
@pytest.mark.skipif(
    not (gen.UI_DIR / "node_modules").is_dir(), reason="apps/ui dependencies are not installed"
)
def test_generated_files_are_up_to_date():
    # Deterministic output: rendering twice gives the same bytes, and the
    # committed files match (`--check`).
    components = gen.collect()
    assert gen.render_markdown(components) == gen.render_markdown(components)
    assert gen.main(["--check"]) == 0
