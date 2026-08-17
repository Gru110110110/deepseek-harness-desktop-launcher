# SPDX-License-Identifier: MIT
from __future__ import annotations

import re
import unittest
from pathlib import Path

from app_paths import APP_VERSION


ROOT = Path(__file__).resolve().parents[1]


class PackagingTests(unittest.TestCase):
    def test_windows_build_avoids_one_file_self_extraction(self) -> None:
        spec = (ROOT / "build" / "windows.spec").read_text(encoding="utf-8")

        self.assertIn("exclude_binaries=True", spec)
        self.assertIn("COLLECT(", spec)
        self.assertNotRegex(spec, r"a\.scripts,\s*a\.binaries,\s*a\.datas")

    def test_windows_release_and_website_use_directory_zip(self) -> None:
        workflow = (ROOT / ".github" / "workflows" / "desktop.yml").read_text(
            encoding="utf-8"
        )
        website = (ROOT / "public" / "index.html").read_text(encoding="utf-8")

        artifact = "DSHLauncher-Windows-x64.zip"
        self.assertIn(artifact, workflow)
        self.assertIn(artifact, website)
        self.assertNotIn("releases/latest/download/DSHLauncher.exe", website)

    def test_windows_version_resource_matches_application_version(self) -> None:
        version_info = (ROOT / "build" / "windows-version-info.txt").read_text(
            encoding="utf-8"
        )
        string_versions = set(
            re.findall(
                r'StringStruct\("(?:FileVersion|ProductVersion)", "([^"]+)"\)',
                version_info,
            )
        )
        fixed_version = tuple(int(part) for part in APP_VERSION.split(".")) + (0,)
        normalized = re.sub(r"\s+", "", version_info)

        self.assertEqual(string_versions, {APP_VERSION})
        self.assertIn(f"filevers={fixed_version}".replace(" ", ""), normalized)
        self.assertIn(f"prodvers={fixed_version}".replace(" ", ""), normalized)


if __name__ == "__main__":
    unittest.main()
