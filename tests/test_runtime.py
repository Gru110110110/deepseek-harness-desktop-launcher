# SPDX-License-Identifier: MIT
"""Deployment tests with temporary homes, local transports, and owned processes."""
from __future__ import annotations

import functools
import hashlib
import http.server
import io
import json
import os
import shutil
import sys
import tarfile
import tempfile
import threading
import time
import unittest
import urllib.error
import zipfile
from contextlib import contextmanager
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

import app_paths
import runtime
from app_paths import ApplicationPaths
from localization import LocalizedError

TEST_HARNESS_VERSION = "0.1.0-rc.6"


class _MemoryResponse:
    def __init__(self, payload: bytes, *, status: int = 200, headers: dict[str, str] | None = None):
        self._stream = io.BytesIO(payload)
        self.status = status
        self.headers = headers or {"Content-Length": str(len(payload))}

    def __enter__(self) -> "_MemoryResponse":
        return self

    def __exit__(self, _type: object, _value: object, _traceback: object) -> None:
        return None

    def getcode(self) -> int:
        return self.status

    def read(self, size: int = -1) -> bytes:
        return self._stream.read(size)


class _QuietHandler(http.server.SimpleHTTPRequestHandler):
    def log_message(self, _format: str, *_args: object) -> None:
        return None


@contextmanager
def _serve_directory(directory: Path):
    handler = functools.partial(_QuietHandler, directory=str(directory))
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield f"http://127.0.0.1:{server.server_port}"
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)


def _write_dsh_tree(directory: Path, version: str) -> None:
    package = directory / "node_modules" / "@deepseek-ai" / "dsh"
    (package / "lib").mkdir(parents=True)
    (package / "package.json").write_text(json.dumps({"version": version}), encoding="utf-8")
    (package / "lib" / "bin.js").write_text(f"print({version!r})\n", encoding="utf-8")


def _build_synthetic_node_archive(root: Path, filename: str, version: str) -> Path:
    top = root / f"node-v{version}-{app_paths.node_platform()}-{app_paths.arch_tag()}"
    node = top / ("node.exe" if os.name == "nt" else "bin/node")
    node.parent.mkdir(parents=True)
    shutil.copy2(sys.executable, node)
    npm_cli = (
        top / "node_modules" / "npm" / "bin" / "npm-cli.js"
        if os.name == "nt"
        else top / "lib" / "node_modules" / "npm" / "bin" / "npm-cli.js"
    )
    npm_cli.parent.mkdir(parents=True)
    npm_cli.write_text(
        """import json
import pathlib
import sys
spec = next(value for value in sys.argv if value.startswith('@deepseek-ai/dsh@'))
version = spec.rsplit('@', 1)[1]
package = pathlib.Path.cwd() / 'node_modules' / '@deepseek-ai' / 'dsh'
(package / 'lib').mkdir(parents=True)
(package / 'package.json').write_text(json.dumps({'version': version}), encoding='utf-8')
(package / 'lib' / 'bin.js').write_text(f'print({version!r})\\n', encoding='utf-8')
""",
        encoding="utf-8",
    )
    archive = root / filename
    if filename.endswith(".zip"):
        with zipfile.ZipFile(archive, "w") as bundle:
            for path in top.rglob("*"):
                if path.is_file():
                    bundle.write(path, path.relative_to(root))
    else:
        with tarfile.open(archive, "w:gz") as bundle:
            bundle.add(top, arcname=top.name)
    return archive


class AppPathsTest(unittest.TestCase):
    def test_source_import_markers_and_deployment_paths_are_independent(self) -> None:
        paths = ApplicationPaths.from_home(Path("desktop-home"))
        self.assertEqual(paths.home_import_marker.name, ".source-home-import-v1")
        self.assertEqual(paths.workspace_import_marker.name, ".source-workspace-import-v1")
        self.assertEqual(paths.cc_switch_import_marker.name, ".cc-switch-import-v2")
        self.assertEqual(paths.language_file.name, "language")
        self.assertEqual(paths.cache_dir.name, "cache")
        self.assertEqual(paths.deployment_lock.name, ".deployment.lock")

    def test_node_platform(self) -> None:
        self.assertIn(app_paths.node_platform(), ("darwin", "win", "linux"))

    def test_arch_tag(self) -> None:
        self.assertIn(app_paths.arch_tag(), ("arm64", "x64"))

    def test_explicit_node_bases_suppress_public_fallbacks(self) -> None:
        with patch.dict(os.environ, {"DSH_DESKTOP_NODE_BASES": "https://one, https://two/"}):
            self.assertEqual(app_paths.node_dist_bases(), ("https://one", "https://two"))


class VersionTest(unittest.TestCase):
    def test_semver_orders_prereleases_and_rejects_downgrades(self) -> None:
        self.assertTrue(runtime.is_newer_version("0.1.0-rc.10", "0.1.0-rc.9"))
        self.assertTrue(runtime.is_newer_version("0.1.0", "0.1.0-rc.10"))
        self.assertFalse(runtime.is_newer_version("0.1.0-rc.4", "0.1.0-rc.5"))
        self.assertFalse(runtime.is_newer_version("not-semver", "0.1.0"))

    def test_latest_version_uses_the_highest_reachable_registry_value(self) -> None:
        versions = {
            runtime.NPM_REGISTRY_DEFAULT: "0.1.0-rc.10",
            runtime.NPM_REGISTRY_FALLBACK: "0.1.0-rc.9",
        }
        with patch.object(runtime, "_query_registry_version", side_effect=lambda source, _owner: versions[source]):
            self.assertEqual(runtime.latest_harness_version(), "0.1.0-rc.10")

    def test_explicit_registry_is_authoritative(self) -> None:
        with patch.dict(os.environ, {"DSH_DESKTOP_NPM_REGISTRY": "https://registry.internal"}):
            self.assertEqual(runtime.npm_registries(), ("https://registry.internal",))

    def test_install_prefers_a_registry_that_has_the_exact_version(self) -> None:
        with patch.object(
            runtime,
            "_probe_registry_version",
            side_effect=lambda source, _version, _owner: source == runtime.NPM_REGISTRY_FALLBACK,
        ):
            ordered = runtime._ordered_install_registries("0.1.0-rc.5", runtime.DeploymentController())
        self.assertEqual(ordered[0], runtime.NPM_REGISTRY_FALLBACK)

    def test_release_sources_require_matching_node_hashes_and_valid_latest_metadata(self) -> None:
        expected = runtime._node_archive_sha256(
            app_paths.NODE_VERSION,
            runtime.node_dist_filename(app_paths.NODE_VERSION),
        )
        with (
            patch.object(runtime, "_node_manifest_checksum", return_value=expected) as node_check,
            patch.object(
                runtime,
                "_query_registry_version",
                side_effect=("0.1.0-rc.6", "0.1.0-rc.5"),
            ) as npm_check,
        ):
            checked = runtime.verify_release_sources()
        self.assertEqual(node_check.call_count, 2)
        self.assertEqual(npm_check.call_count, 2)
        self.assertEqual(len(checked), 4)

    def test_release_sources_reject_unavailable_mirror_latest_metadata(self) -> None:
        expected = runtime._node_archive_sha256(
            app_paths.NODE_VERSION,
            runtime.node_dist_filename(app_paths.NODE_VERSION),
        )
        with (
            patch.object(runtime, "_node_manifest_checksum", return_value=expected),
            patch.object(
                runtime,
                "_query_registry_version",
                side_effect=("0.1.0-rc.6", OSError("mirror unavailable")),
            ),
        ):
            with self.assertRaises(OSError):
                runtime.verify_release_sources()


class DownloadTest(unittest.TestCase):
    def test_download_falls_back_and_verifies_sha256(self) -> None:
        payload = b"verified runtime bytes"
        requested: list[str] = []

        def open_request(request, timeout):
            self.assertGreater(timeout, 0)
            requested.append(request.full_url)
            if request.full_url.startswith("https://primary"):
                raise urllib.error.URLError("offline")
            return _MemoryResponse(payload)

        with tempfile.TemporaryDirectory() as tmp:
            destination = Path(tmp) / "node.zip"
            with patch.object(runtime.urllib.request, "urlopen", side_effect=open_request):
                runtime._download(
                    ("https://primary/node.zip", "https://mirror/node.zip"),
                    destination,
                    hashlib.sha256(payload).hexdigest(),
                    lambda _done, _total: None,
                    runtime.DeploymentController(),
                )
            self.assertEqual(destination.read_bytes(), payload)
            self.assertEqual(
                requested,
                ["https://primary/node.zip", "https://mirror/node.zip"],
            )

    def test_download_resumes_a_partial_response(self) -> None:
        payload = b"0123456789"

        def open_request(request, timeout):
            self.assertGreater(timeout, 0)
            self.assertEqual(request.get_header("Range"), "bytes=4-")
            return _MemoryResponse(
                payload[4:],
                status=206,
                headers={"Content-Range": "bytes 4-9/10", "Content-Length": "6"},
            )

        with tempfile.TemporaryDirectory() as tmp:
            destination = Path(tmp) / "node.zip"
            destination.with_name("node.zip.part").write_bytes(payload[:4])
            with patch.object(runtime.urllib.request, "urlopen", side_effect=open_request):
                runtime._download(
                    ("https://mirror/node.zip",),
                    destination,
                    hashlib.sha256(payload).hexdigest(),
                    lambda _done, _total: None,
                    runtime.DeploymentController(),
                )
            self.assertEqual(destination.read_bytes(), payload)

    def test_download_rejects_untrusted_bytes(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            destination = Path(tmp) / "node.zip"
            with (
                patch.object(runtime.urllib.request, "urlopen", return_value=_MemoryResponse(b"bad")),
                patch.object(runtime, "_DOWNLOAD_ATTEMPTS_PER_SOURCE", 1),
            ):
                with self.assertRaises(LocalizedError):
                    runtime._download(
                        ("https://mirror/node.zip",),
                        destination,
                        hashlib.sha256(b"good").hexdigest(),
                        lambda _done, _total: None,
                        runtime.DeploymentController(),
                    )
            self.assertFalse(destination.exists())


class ArchiveTest(unittest.TestCase):
    def test_extract_node_strips_top_level(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp)
            archive = base / "node.tar.gz"
            payload = base / "node-v24.19.0-darwin-arm64" / "bin" / "node"
            payload.parent.mkdir(parents=True)
            payload.write_text("x")
            with tarfile.open(archive, "w:gz") as bundle:
                bundle.add(payload, arcname="node-v24.19.0-darwin-arm64/bin/node")
            node_dir = base / "out" / "node"
            runtime._extract_node(archive, node_dir)
            self.assertTrue((node_dir / "bin" / "node").is_file())

    def test_extract_node_rejects_multi_top_level(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp)
            archive = base / "node.tar.gz"
            with tarfile.open(archive, "w:gz") as bundle:
                for name in ("a/file.txt", "b/file.txt"):
                    member = base / name
                    member.parent.mkdir(parents=True, exist_ok=True)
                    member.write_text("x")
                    bundle.add(member, arcname=name)
            with self.assertRaises(RuntimeError):
                runtime._extract_node(archive, base / "out" / "node")

    def test_extract_node_rejects_parent_traversal(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp)
            archive = base / "node.tar.gz"
            with tarfile.open(archive, "w:gz") as bundle:
                member = tarfile.TarInfo("../outside")
                member.size = 1
                bundle.addfile(member, io.BytesIO(b"x"))
            with self.assertRaises(LocalizedError):
                runtime._extract_node(archive, base / "node")
            self.assertFalse((base / "outside").exists())

    def test_extract_node_rejects_hard_link_outside_top_level(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp)
            archive = base / "node.tar.gz"
            with tarfile.open(archive, "w:gz") as bundle:
                member = tarfile.TarInfo("node/bin/node")
                member.type = tarfile.LNKTYPE
                member.linkname = "other/bin/node"
                bundle.addfile(member)
            with self.assertRaises(LocalizedError):
                runtime._extract_node(archive, base / "node")


class RuntimeStateTest(unittest.TestCase):
    def test_installed_version_requires_matching_manifest_and_marker(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            paths = ApplicationPaths.from_home(Path(tmp) / "home")
            paths.ensure_dirs()
            _write_dsh_tree(paths.dsh_dir, "0.1.0-rc.6")
            paths.version_file.write_text("0.1.0-rc.6\n", encoding="utf-8")
            self.assertEqual(runtime.installed_version(paths), "0.1.0-rc.6")
            paths.version_file.write_text("0.1.0-rc.5\n", encoding="utf-8")
            self.assertIsNone(runtime.installed_version(paths))

    def test_runtime_ready_executes_node_and_cli_smokes(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            paths = ApplicationPaths.from_home(Path(tmp) / "home")
            paths.ensure_dirs()
            _write_dsh_tree(paths.dsh_dir, "0.1.0-rc.6")
            paths.version_file.write_text("0.1.0-rc.6\n", encoding="utf-8")
            with (
                patch.object(runtime, "_node_is_valid", return_value=True) as node_smoke,
                patch.object(runtime, "_dsh_is_valid", return_value=True) as dsh_smoke,
            ):
                self.assertTrue(runtime.is_runtime_ready(paths))
            node_smoke.assert_called_once()
            dsh_smoke.assert_called_once()

    def test_failed_update_restores_the_previous_runtime(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            paths = ApplicationPaths.from_home(Path(tmp) / "home")
            paths.ensure_dirs()
            _write_dsh_tree(paths.dsh_dir, "0.1.0-rc.5")
            (paths.dsh_dir / "old.txt").write_text("preserved", encoding="utf-8")
            paths.version_file.write_text("0.1.0-rc.5\n", encoding="utf-8")

            def staged_install(_paths, version, _controller, _activity):
                staging = paths.runtime_dir / "dsh.staging-test"
                _write_dsh_tree(staging, version)
                return staging

            with (
                patch.object(runtime, "_ensure_node"),
                patch.object(runtime, "_install_with_fallback", side_effect=staged_install),
                patch.object(runtime, "_dsh_is_valid", return_value=False),
            ):
                with self.assertRaises(LocalizedError):
                    runtime.deploy_runtime(
                        paths,
                        lambda _step: None,
                        force=True,
                        target_version="0.1.0-rc.6",
                    )
            self.assertEqual((paths.dsh_dir / "old.txt").read_text(encoding="utf-8"), "preserved")
            self.assertEqual(paths.version_file.read_text(encoding="utf-8"), "0.1.0-rc.5\n")

    def test_failed_harness_install_restores_the_previous_node_pair(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            paths = ApplicationPaths.from_home(Path(tmp) / "home")
            paths.ensure_dirs()
            paths.node_dir.mkdir()
            (paths.node_dir / "old.txt").write_text("preserved", encoding="utf-8")
            _write_dsh_tree(paths.dsh_dir, "0.1.0-rc.5")
            paths.version_file.write_text("0.1.0-rc.5\n", encoding="utf-8")

            def switch_node(_paths, _progress, _controller, _activity):
                staging = paths.runtime_dir / "node.staging-test"
                staging.mkdir()
                (staging / "new.txt").write_text("candidate", encoding="utf-8")
                return runtime._publish_directory(staging, paths.node_dir)

            with (
                patch.object(runtime, "_recover_valid_previous"),
                patch.object(runtime, "_runtime_is_valid", return_value=False),
                patch.object(runtime, "_node_reported_version", return_value="23.0.0"),
                patch.object(runtime, "_dsh_is_valid", return_value=True),
                patch.object(runtime, "_ensure_node", side_effect=switch_node),
                patch.object(
                    runtime,
                    "_install_with_fallback",
                    side_effect=LocalizedError("install_failed", log=paths.install_log),
                ),
            ):
                with self.assertRaises(LocalizedError):
                    runtime.deploy_runtime(
                        paths,
                        lambda _step: None,
                        force=True,
                        target_version=TEST_HARNESS_VERSION,
                    )
            self.assertTrue((paths.node_dir / "old.txt").is_file())
            self.assertFalse((paths.node_dir / "new.txt").exists())

    def test_deployment_reports_each_blocking_activity(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            paths = ApplicationPaths.from_home(Path(tmp) / "home")
            paths.ensure_dirs()
            activities: list[tuple[str, dict[str, object]]] = []

            def prepare_node(_paths, _progress, _controller, activity):
                activity("downloading_node", {"version": app_paths.NODE_VERSION})
                activity("verifying_node", {"version": app_paths.NODE_VERSION})
                return None

            def install_harness(_paths, version, _controller, activity):
                activity("checking_sources", {"version": version})
                activity(
                    "installing_harness",
                    {"version": version, "source": runtime.NPM_REGISTRY_FALLBACK},
                )
                activity("validating_harness", {"version": version})
                staging = paths.runtime_dir / "dsh.staging-test"
                _write_dsh_tree(staging, version)
                return staging

            with (
                patch.object(runtime, "_recover_valid_previous"),
                patch.object(runtime, "_runtime_is_valid", return_value=False),
                patch.object(runtime, "_ensure_node", side_effect=prepare_node),
                patch.object(runtime, "_install_with_fallback", side_effect=install_harness),
                patch.object(runtime, "_dsh_is_valid", return_value=True),
            ):
                runtime.deploy_runtime(
                    paths,
                    lambda _step: None,
                    on_activity=lambda key, values: activities.append((key, values)),
                    force=True,
                    target_version="0.1.0-rc.6",
                )

            self.assertEqual(
                [key for key, _values in activities],
                [
                    "waiting_for_lock",
                    "checking_runtime",
                    "downloading_node",
                    "verifying_node",
                    "checking_sources",
                    "installing_harness",
                    "validating_harness",
                    "activating_harness",
                ],
            )

    def test_interrupted_publication_restores_previous_directory(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            paths = ApplicationPaths.from_home(Path(tmp) / "home")
            paths.ensure_dirs()
            previous = paths.runtime_dir / "dsh.previous"
            previous.mkdir()
            (previous / "old.txt").write_text("preserved", encoding="utf-8")
            runtime._recover_interrupted_publication(paths)
            self.assertEqual((paths.dsh_dir / "old.txt").read_text(encoding="utf-8"), "preserved")

    def test_invalid_published_runtime_rolls_back_to_a_valid_previous_version(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            paths = ApplicationPaths.from_home(Path(tmp) / "home")
            paths.ensure_dirs()
            paths.dsh_dir.mkdir()
            (paths.dsh_dir / "broken.txt").write_text("broken", encoding="utf-8")
            previous = paths.runtime_dir / "dsh.previous"
            _write_dsh_tree(previous, "0.1.0-rc.5")

            def validate_dsh(_paths, _node, dsh_dir, _version, _controller=None):
                return dsh_dir == previous

            with (
                patch.object(runtime, "_node_is_valid", return_value=True),
                patch.object(runtime, "_dsh_is_valid", side_effect=validate_dsh),
            ):
                runtime._recover_valid_previous(paths, runtime.DeploymentController())
            self.assertEqual(runtime._dsh_manifest_version(paths.dsh_dir), "0.1.0-rc.5")
            self.assertEqual(paths.version_file.read_text(encoding="utf-8"), "0.1.0-rc.5\n")

    def test_interrupted_node_switch_restores_the_node_that_runs_current_harness(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            paths = ApplicationPaths.from_home(Path(tmp) / "home")
            paths.ensure_dirs()
            paths.node_dir.mkdir()
            (paths.node_dir / "new.txt").write_text("candidate", encoding="utf-8")
            previous = paths.runtime_dir / "node.previous"
            previous.mkdir()
            (previous / "old.txt").write_text("preserved", encoding="utf-8")
            _write_dsh_tree(paths.dsh_dir, "0.1.0-rc.5")

            def validate_dsh(_paths, node_dir, _dsh_dir, _version, _controller=None):
                return (node_dir / "old.txt").is_file()

            with (
                patch.object(runtime, "_node_reported_version", return_value="23.0.0"),
                patch.object(runtime, "_node_is_valid", return_value=False),
                patch.object(runtime, "_dsh_is_valid", side_effect=validate_dsh),
            ):
                runtime._recover_valid_previous(paths, runtime.DeploymentController())
            self.assertTrue((paths.node_dir / "old.txt").is_file())
            self.assertFalse((paths.node_dir / "new.txt").exists())

    def test_interrupted_pair_switch_restores_both_previous_directories(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            paths = ApplicationPaths.from_home(Path(tmp) / "home")
            paths.ensure_dirs()
            paths.node_dir.mkdir()
            (paths.node_dir / "new.txt").write_text("candidate", encoding="utf-8")
            _write_dsh_tree(paths.dsh_dir, "0.1.0-rc.6")
            node_previous = paths.runtime_dir / "node.previous"
            node_previous.mkdir()
            (node_previous / "old.txt").write_text("preserved", encoding="utf-8")
            dsh_previous = paths.runtime_dir / "dsh.previous"
            _write_dsh_tree(dsh_previous, "0.1.0-rc.5")
            (dsh_previous / "old.txt").write_text("preserved", encoding="utf-8")

            def validate_dsh(_paths, node_dir, dsh_dir, _version, _controller=None):
                return (node_dir / "old.txt").is_file() and (dsh_dir / "old.txt").is_file()

            with (
                patch.object(runtime, "_node_reported_version", return_value="23.0.0"),
                patch.object(runtime, "_dsh_is_valid", side_effect=validate_dsh),
            ):
                runtime._recover_valid_previous(paths, runtime.DeploymentController())
            self.assertTrue((paths.node_dir / "old.txt").is_file())
            self.assertTrue((paths.dsh_dir / "old.txt").is_file())
            self.assertEqual(paths.version_file.read_text(encoding="utf-8"), "0.1.0-rc.5\n")

    def test_deployment_lock_rejects_a_second_writer_after_its_bound(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            paths = ApplicationPaths.from_home(Path(tmp) / "home")
            paths.ensure_dirs()
            with runtime._DeploymentLock(paths.deployment_lock, runtime.DeploymentController()):
                with patch.object(runtime, "_LOCK_TIMEOUT_SECONDS", 0.05):
                    with self.assertRaises(LocalizedError):
                        with runtime._DeploymentLock(paths.deployment_lock, runtime.DeploymentController()):
                            self.fail("the second writer must not acquire the active lock")

    def test_windows_process_probe_does_not_send_a_console_event(self) -> None:
        with (
            patch.object(runtime.os, "name", "nt"),
            patch.object(runtime, "_windows_process_exists", return_value=True) as query,
            patch.object(runtime.os, "kill", side_effect=AssertionError("must not signal")),
        ):
            self.assertTrue(runtime._process_exists(1234))
        query.assert_called_once_with(1234)

    @unittest.skipUnless(os.name == "nt", "requires the Windows process API")
    def test_windows_process_probe_finds_the_current_process(self) -> None:
        self.assertTrue(runtime._process_exists(os.getpid()))

    def test_subprocess_environment_drops_ambient_secrets_but_keeps_proxy_settings(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            paths = ApplicationPaths.from_home(Path(tmp) / "home")
            with patch.dict(
                os.environ,
                {"DEEPSEEK_API_KEY": "secret", "NPM_TOKEN": "secret", "HTTPS_PROXY": "http://proxy"},
            ):
                environment = runtime._subprocess_environment(paths)
            self.assertNotIn("DEEPSEEK_API_KEY", environment)
            self.assertNotIn("NPM_TOKEN", environment)
            self.assertEqual(environment["HTTPS_PROXY"], "http://proxy")
            self.assertEqual(environment["HOME"], str(paths.app_home))
            self.assertEqual(environment["USERPROFILE"], str(paths.app_home))
            self.assertEqual(
                environment["NPM_CONFIG_USERCONFIG"],
                str(paths.cache_dir / "isolated-npmrc"),
            )

    def test_transport_diagnostics_redact_credentials_and_queries(self) -> None:
        self.assertEqual(
            runtime._display_source("https://name:secret@example.test/path?token=secret#fragment"),
            "https://example.test/path",
        )


class PlatformInstallTest(unittest.TestCase):
    def test_npm_cli_uses_the_platform_distribution_layout(self) -> None:
        node_dir = Path("node")
        with patch.object(runtime.os, "name", "nt"):
            self.assertEqual(runtime._npm_cli(node_dir), node_dir / "node_modules/npm/bin/npm-cli.js")
        with patch.object(runtime.os, "name", "posix"):
            self.assertEqual(runtime._npm_cli(node_dir), node_dir / "lib/node_modules/npm/bin/npm-cli.js")

    def test_real_first_install_uses_local_verified_transport_and_owned_processes(self) -> None:
        if app_paths.node_platform() not in {"darwin", "win"}:
            self.skipTest("desktop packages are built only for macOS and Windows")
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            paths = ApplicationPaths.from_home(root / "home")
            version = app_paths.NODE_VERSION
            filename = runtime.node_dist_filename(version)
            release_dir = root / "transport" / f"v{version}"
            release_dir.mkdir(parents=True)
            built = _build_synthetic_node_archive(root, filename, version)
            archive = release_dir / filename
            built.replace(archive)
            checksum = hashlib.sha256(archive.read_bytes()).hexdigest()
            registry_metadata = root / "transport" / "registry" / "@deepseek-ai" / "dsh"
            registry_metadata.mkdir(parents=True)
            (registry_metadata / "latest").write_text(
                json.dumps({"version": TEST_HARNESS_VERSION}),
                encoding="utf-8",
            )
            (registry_metadata / TEST_HARNESS_VERSION).write_text(
                json.dumps({"version": TEST_HARNESS_VERSION}),
                encoding="utf-8",
            )
            with _serve_directory(root / "transport") as base_url:
                environment = {
                    "DSH_DESKTOP_NODE_BASES": base_url,
                    "DSH_DESKTOP_NODE_VERSION": version,
                    "DSH_DESKTOP_NODE_SHA256": checksum,
                    "DSH_DESKTOP_NPM_REGISTRIES": f"{base_url}/registry",
                }

                def node_exists(_paths, node_dir, _controller=None):
                    return runtime._node_executable(node_dir).is_file()

                with (
                    patch.dict(os.environ, environment),
                    patch.object(runtime, "_node_is_valid", side_effect=node_exists),
                ):
                    deployed = runtime.deploy_runtime(paths, lambda _step: None)
                    self.assertEqual(deployed, paths.node_bin)
                    self.assertTrue(runtime.is_runtime_ready(paths))
            self.assertEqual(runtime.installed_version(paths), TEST_HARNESS_VERSION)
            self.assertIn(f"registry={base_url}/registry", paths.install_log.read_text(encoding="utf-8"))


class CancellationTest(unittest.TestCase):
    def test_controller_cancels_and_joins_its_process(self) -> None:
        controller = runtime.DeploymentController()
        timer = threading.Timer(0.2, controller.cancel)
        timer.start()
        started = time.monotonic()
        try:
            with tempfile.TemporaryFile() as output:
                with self.assertRaises(runtime.DeploymentCancelled):
                    controller.run(
                        [sys.executable, "-c", "import time; time.sleep(120)"],
                        cwd=Path.cwd(),
                        stdout=output,
                        timeout=30,
                        environment=dict(os.environ),
                    )
        finally:
            timer.cancel()
        self.assertLess(time.monotonic() - started, 5)


if __name__ == "__main__":
    unittest.main()
