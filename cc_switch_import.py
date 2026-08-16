# SPDX-License-Identifier: MIT
"""Read-only, first-run import of compatible CC Switch Claude providers.

CC Switch remains the owner of its SQLite database.  This module opens that
database read-only, translates only self-contained API-key providers, and
publishes new Harness settings/credential documents only when neither target
already exists.  OAuth, local-routing, and otherwise proxy-dependent providers
are deliberately left behind.
"""
from __future__ import annotations

import hashlib
import ipaddress
import json
import os
import re
import shutil
import sqlite3
import tempfile
from dataclasses import dataclass
from pathlib import Path
from typing import Any
from urllib.parse import urlsplit

_DATABASE_FILENAME = "cc-switch.db"
_MARKER_CONTENT = "1\n"
_MAX_PROVIDERS = 1_000
_MAX_MODELS_PER_PROVIDER = 64
_MODEL_ENV_KEYS = (
    "ANTHROPIC_MODEL",
    "ANTHROPIC_DEFAULT_SONNET_MODEL",
    "ANTHROPIC_DEFAULT_OPUS_MODEL",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL",
    "ANTHROPIC_REASONING_MODEL",
    "CLAUDE_CODE_SUBAGENT_MODEL",
)
_KEY_ENV_KEYS = (
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_API_KEY",
    "OPENROUTER_API_KEY",
    "GOOGLE_API_KEY",
)
_API_FORMATS = {
    "anthropic": "anthropic-messages",
    "anthropic_messages": "anthropic-messages",
    "openai_chat": "openai-completions",
    "openai_completions": "openai-completions",
    "openai_responses": "openai-responses",
}
_MANAGED_PROVIDER_TYPES = frozenset({"codex_oauth", "github_copilot", "xai_oauth"})
_CONTROL_CHARACTERS = re.compile(r"[\x00-\x1f\x7f]")
_SLUG_CHARACTERS = re.compile(r"[^a-z0-9]+")
_YAML_PLAIN_KEY = re.compile(r"^([A-Za-z_][A-Za-z0-9_.-]*)\s*:")


@dataclass(frozen=True)
class CcSwitchImportResult:
    """One content-free import outcome for the launcher diagnostic log."""

    imported: bool
    message: str


@dataclass(frozen=True)
class _Provider:
    route: str
    display_name: str
    credential_ref: str
    credential: str
    api: str
    base_url: str
    models: tuple[str, ...]


def _marker_is_complete(marker: Path) -> bool:
    try:
        return marker.read_text(encoding="utf-8") == _MARKER_CONTENT
    except OSError:
        return False


def _json_object(value: Any) -> dict[str, Any] | None:
    if isinstance(value, dict):
        return value
    if not isinstance(value, str):
        return None
    try:
        parsed = json.loads(value)
    except json.JSONDecodeError:
        return None
    return parsed if isinstance(parsed, dict) else None


def _clean_string(value: Any, *, maximum: int = 2_048) -> str | None:
    if not isinstance(value, str):
        return None
    cleaned = value.strip()
    if not cleaned or len(cleaned) > maximum or _CONTROL_CHARACTERS.search(cleaned):
        return None
    return cleaned


def _api_format(settings: dict[str, Any], meta: dict[str, Any]) -> str | None:
    raw = meta.get("apiFormat") or settings.get("api_format") or settings.get("apiFormat")
    if raw is None and settings.get("openrouter_compat_mode") in (True, 1, "1", "true"):
        raw = "openai_chat"
    if raw is None:
        raw = "anthropic"
    return _API_FORMATS.get(str(raw).strip().lower())


def _is_managed(meta: dict[str, Any]) -> bool:
    provider_type = str(meta.get("providerType") or "").strip().lower()
    if provider_type in _MANAGED_PROVIDER_TYPES:
        return True
    auth_binding = meta.get("authBinding")
    return (
        isinstance(auth_binding, dict)
        and str(auth_binding.get("source") or "").strip().lower() == "managed_account"
    )


def _usable_base_url(value: Any) -> str | None:
    candidate = _clean_string(value)
    if candidate is None:
        return None
    try:
        parsed = urlsplit(candidate)
        port = parsed.port
    except ValueError:
        return None
    if (
        parsed.scheme not in {"http", "https"}
        or parsed.hostname is None
        or parsed.username is not None
        or parsed.password is not None
        or parsed.query
        or parsed.fragment
    ):
        return None
    hostname = parsed.hostname.rstrip(".").lower()
    if hostname == "localhost":
        return None
    try:
        if ipaddress.ip_address(hostname).is_loopback:
            return None
    except ValueError:
        pass
    if port is not None and not (1 <= port <= 65_535):
        return None
    return candidate.rstrip("/")


def _provider_models(settings: dict[str, Any]) -> tuple[str, ...]:
    values: list[Any] = []
    environment = settings.get("env")
    if isinstance(environment, dict):
        values.extend(environment.get(key) for key in _MODEL_ENV_KEYS)
    models = settings.get("models")
    if isinstance(models, dict):
        values.extend(models.keys())
    elif isinstance(models, list):
        for model in models:
            values.append(model.get("id") if isinstance(model, dict) else model)

    result: list[str] = []
    seen: set[str] = set()
    for value in values:
        model = _clean_string(value, maximum=256)
        if model is not None and model not in seen:
            seen.add(model)
            result.append(model)
            if len(result) >= _MAX_MODELS_PER_PROVIDER:
                break
    return tuple(result)


def _provider_key(settings: dict[str, Any]) -> str | None:
    environment = settings.get("env")
    if not isinstance(environment, dict):
        return None
    for key in _KEY_ENV_KEYS:
        candidate = _clean_string(environment.get(key), maximum=16_384)
        if candidate is not None and candidate != "PROXY_MANAGED":
            return candidate
    return None


def _provider_identity(provider_id: str, name: str) -> tuple[str, str]:
    digest = hashlib.sha256(provider_id.encode("utf-8")).hexdigest()
    slug = _SLUG_CHARACTERS.sub("-", name.lower()).strip("-")[:32] or "provider"
    return f"ccswitch-{slug}-{digest[:8]}", f"CCSWITCH_{digest[:12].upper()}_API_KEY"


def _translate_row(row: sqlite3.Row) -> _Provider | None:
    provider_id = _clean_string(row["id"], maximum=512)
    name = _clean_string(row["name"], maximum=120)
    settings = _json_object(row["settings_config"])
    meta = _json_object(row["meta"]) if "meta" in row.keys() else {}
    if provider_id is None or name is None or settings is None or meta is None:
        return None
    if _is_managed(meta) or meta.get("isFullUrl") is True:
        return None
    if meta.get("localProxyRequestOverrides") or settings.get("localProxyRequestOverrides"):
        return None
    if str(settings.get("auth_mode") or "").strip().lower() == "bearer_only":
        return None

    environment = settings.get("env")
    if not isinstance(environment, dict):
        return None
    base_url = _usable_base_url(environment.get("ANTHROPIC_BASE_URL"))
    credential = _provider_key(settings)
    api = _api_format(settings, meta)
    models = _provider_models(settings)
    if base_url is None or credential is None or api is None or not models:
        return None

    route, credential_ref = _provider_identity(provider_id, name)
    return _Provider(
        route=route,
        display_name=f"{name} (CC Switch)",
        credential_ref=credential_ref,
        credential=credential,
        api=api,
        base_url=base_url,
        models=models,
    )


def _read_providers(database: Path) -> tuple[list[_Provider], int]:
    if database.is_symlink() or not database.is_file():
        return [], 0
    uri = f"{database.resolve(strict=True).as_uri()}?mode=ro"
    with sqlite3.connect(uri, uri=True, timeout=1.0) as connection:
        connection.row_factory = sqlite3.Row
        connection.execute("PRAGMA query_only = ON")
        columns = {
            row[1]
            for row in connection.execute("PRAGMA table_info(providers)")
            if isinstance(row[1], str)
        }
        required = {"id", "app_type", "name", "settings_config"}
        if not required.issubset(columns):
            return [], 0
        selected = ["id", "name", "settings_config"]
        if "meta" in columns:
            selected.append("meta")
        order = "is_current DESC, " if "is_current" in columns else ""
        if "sort_index" in columns:
            order += "CASE WHEN sort_index IS NULL THEN 1 ELSE 0 END, sort_index, "
        order += "name, id"
        rows = connection.execute(
            f"SELECT {', '.join(selected)} FROM providers "
            f"WHERE app_type = ? ORDER BY {order} LIMIT ?",
            ("claude", _MAX_PROVIDERS),
        ).fetchall()

    providers: list[_Provider] = []
    routes: set[str] = set()
    credential_refs: set[str] = set()
    for row in rows:
        provider = _translate_row(row)
        if (
            provider is not None
            and provider.route not in routes
            and provider.credential_ref not in credential_refs
        ):
            routes.add(provider.route)
            credential_refs.add(provider.credential_ref)
            providers.append(provider)
    return providers, len(rows) - len(providers)


def _provider_document(provider: _Provider) -> dict[str, Any]:
    return {
        "displayName": provider.display_name,
        "apiKeyEnv": provider.credential_ref,
        "api": provider.api,
        "baseURL": provider.base_url,
        "models": [{"id": model} for model in provider.models],
    }


def _json_mapping(content: bytes) -> dict[str, Any] | None:
    try:
        parsed = json.loads(content)
    except (UnicodeDecodeError, json.JSONDecodeError):
        return None
    return parsed if isinstance(parsed, dict) else None


def _safe_yaml_top_level_keys(content: bytes) -> set[str] | None:
    """Recognize a conservative single-document block mapping without values."""
    try:
        text = content.decode("utf-8")
    except UnicodeDecodeError:
        return None
    keys: set[str] = set()
    saw_content = False
    for line in text.splitlines():
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        if not saw_content and line.strip() == "---":
            saw_content = True
            continue
        saw_content = True
        if line[0].isspace():
            continue
        if line.strip() == "..." or line.startswith(("%", "-", "{", "[", "?")):
            return None
        match = _YAML_PLAIN_KEY.match(line)
        if match is None or match.group(1) in keys:
            return None
        keys.add(match.group(1))
    return keys


def _yaml_append(content: bytes, lines: list[str]) -> bytes:
    if not lines:
        return content
    separator = b"" if not content or content.endswith((b"\n", b"\r")) else b"\n"
    return content + separator + ("\n".join(lines) + "\n").encode("utf-8")


def _merge_documents(
    providers: list[_Provider],
    existing_settings: bytes | None,
    existing_credentials: bytes | None,
) -> tuple[bytes, bytes, list[_Provider]] | None:
    """Add only missing routes/references while preserving every existing value."""
    settings_json = _json_mapping(existing_settings) if existing_settings is not None else {}
    settings_yaml_keys: set[str] | None = None
    existing_routes: set[str] = set()
    if existing_settings is not None and settings_json is None:
        settings_yaml_keys = _safe_yaml_top_level_keys(existing_settings)
        if settings_yaml_keys is None or "llm-pi-ai" in settings_yaml_keys:
            return None
    elif settings_json is not None:
        namespace = settings_json.get("llm-pi-ai")
        if namespace is not None:
            if not isinstance(namespace, dict):
                return None
            routes = namespace.get("providers")
            if routes is not None:
                if not isinstance(routes, dict):
                    return None
                existing_routes = {key for key in routes if isinstance(key, str)}

    credentials_json = (
        _json_mapping(existing_credentials) if existing_credentials is not None else {}
    )
    credentials_yaml_keys: set[str] | None = None
    if existing_credentials is not None and credentials_json is None:
        credentials_yaml_keys = _safe_yaml_top_level_keys(existing_credentials)
        if credentials_yaml_keys is None:
            return None
        existing_credential_refs = credentials_yaml_keys
    elif credentials_json is not None:
        if not all(isinstance(key, str) and isinstance(value, str) for key, value in credentials_json.items()):
            return None
        existing_credential_refs = set(credentials_json)
    else:
        return None

    additions = [
        provider
        for provider in providers
        if provider.route not in existing_routes
        and provider.credential_ref not in existing_credential_refs
    ]
    if not additions:
        return None

    provider_values = {
        provider.route: _provider_document(provider)
        for provider in additions
    }
    credential_values = {
        provider.credential_ref: provider.credential
        for provider in additions
    }
    if existing_settings is None:
        settings_content = (
            json.dumps(
                {"llm-pi-ai": {"providers": provider_values}},
                ensure_ascii=False,
                indent=2,
                sort_keys=True,
            )
            + "\n"
        ).encode("utf-8")
    elif settings_json is not None:
        namespace = settings_json.setdefault("llm-pi-ai", {})
        routes = namespace.setdefault("providers", {})
        routes.update(provider_values)
        settings_content = (
            json.dumps(settings_json, ensure_ascii=False, indent=2, sort_keys=True) + "\n"
        ).encode("utf-8")
    else:
        settings_content = _yaml_append(
            existing_settings,
            [
                "llm-pi-ai: "
                + json.dumps(
                    {"providers": provider_values},
                    ensure_ascii=False,
                    separators=(",", ":"),
                    sort_keys=True,
                )
            ],
        )

    if existing_credentials is None:
        credentials_content = (
            json.dumps(credential_values, ensure_ascii=False, indent=2, sort_keys=True) + "\n"
        ).encode("utf-8")
    elif credentials_json is not None:
        credentials_json.update(credential_values)
        credentials_content = (
            json.dumps(credentials_json, ensure_ascii=False, indent=2, sort_keys=True) + "\n"
        ).encode("utf-8")
    else:
        credentials_content = _yaml_append(
            existing_credentials,
            [
                f"{reference}: {json.dumps(value, ensure_ascii=False)}"
                for reference, value in sorted(credential_values.items())
            ],
        )
    return settings_content, credentials_content, additions


def _stage_file(path: Path, content: bytes) -> None:
    path.write_bytes(content)
    if os.name != "nt":
        path.chmod(0o600)


def _publish_new_file(source: Path, target: Path) -> None:
    descriptor = os.open(target, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    try:
        with os.fdopen(descriptor, "wb") as target_file:
            descriptor = -1
            with source.open("rb") as source_file:
                shutil.copyfileobj(source_file, target_file)
            target_file.flush()
        if os.name != "nt":
            target.chmod(0o600)
    except OSError:
        target.unlink(missing_ok=True)
        raise
    finally:
        if descriptor >= 0:
            os.close(descriptor)


def _record_completion(marker: Path) -> None:
    """Publish the one-shot decision marker without replacing any path."""
    marker.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=".cc-switch-marker-", dir=marker.parent) as temporary:
        staged_marker = Path(temporary) / "marker"
        _stage_file(staged_marker, _MARKER_CONTENT.encode("utf-8"))
        try:
            _publish_new_file(staged_marker, marker)
        except FileExistsError:
            if not _marker_is_complete(marker):
                raise


def _publish_candidate(source: Path, target: Path, expected: bytes | None) -> None:
    if expected is None:
        _publish_new_file(source, target)
        return
    if target.is_symlink() or not target.is_file() or target.read_bytes() != expected:
        raise OSError(f"configuration changed while preparing {target.name}")
    os.replace(source, target)
    if os.name != "nt":
        target.chmod(0o600)


def _restore_candidate(target: Path, original: bytes | None) -> None:
    if original is None:
        target.unlink(missing_ok=True)
        return
    descriptor, temporary_name = tempfile.mkstemp(
        prefix=f".{target.name}.cc-switch-restore-",
        dir=target.parent,
    )
    temporary = Path(temporary_name)
    try:
        with os.fdopen(descriptor, "wb") as restored:
            restored.write(original)
            restored.flush()
            os.fsync(restored.fileno())
        if os.name != "nt":
            temporary.chmod(0o600)
        os.replace(temporary, target)
    finally:
        temporary.unlink(missing_ok=True)


def _complete_without_import(marker: Path, message: str) -> CcSwitchImportResult:
    _record_completion(marker)
    return CcSwitchImportResult(False, message)


def import_cc_switch_configuration(
    cc_switch_home: Path,
    dsh_home: Path,
    marker: Path,
) -> CcSwitchImportResult:
    """Import compatible Claude providers without replacing any Harness file."""
    settings_path = dsh_home / "settings.yaml"
    credentials_path = dsh_home / ".credentials.yaml"
    if _marker_is_complete(marker):
        return CcSwitchImportResult(False, "skipped because CC Switch import v2 is already complete")
    if marker.exists() or marker.is_symlink():
        return CcSwitchImportResult(False, "skipped because an unrecognized CC Switch import marker exists")
    if any(path.is_symlink() or (path.exists() and not path.is_file()) for path in (settings_path, credentials_path)):
        return _complete_without_import(
            marker,
            "completed once with unsupported desktop model document paths preserved",
        )

    try:
        existing_settings = settings_path.read_bytes() if settings_path.is_file() else None
        existing_credentials = credentials_path.read_bytes() if credentials_path.is_file() else None
    except OSError:
        return _complete_without_import(
            marker,
            "completed once because existing desktop model documents could not be read safely",
        )

    database = cc_switch_home.expanduser() / _DATABASE_FILENAME
    if database.is_symlink() or not database.is_file():
        return _complete_without_import(
            marker,
            "completed once without a CC Switch database",
        )
    try:
        providers, skipped = _read_providers(database)
    except (OSError, sqlite3.Error):
        return _complete_without_import(
            marker,
            "completed once without importing because the CC Switch database could not be read safely",
        )
    if not providers:
        return _complete_without_import(
            marker,
            f"completed with no compatible standalone Claude providers; skipped={skipped}",
        )

    merged = _merge_documents(providers, existing_settings, existing_credentials)
    if merged is None:
        return _complete_without_import(
            marker,
            "completed once with existing desktop model values preserved and no safe additions",
        )
    settings_content, credentials_content, additions = merged
    if (
        _json_mapping(settings_content) is None
        and _safe_yaml_top_level_keys(settings_content) is None
    ) or (
        _json_mapping(credentials_content) is None
        and _safe_yaml_top_level_keys(credentials_content) is None
    ):
        return _complete_without_import(
            marker,
            "completed once because merged desktop model documents did not pass validation",
        )

    dsh_home.mkdir(parents=True, exist_ok=True)
    marker.parent.mkdir(parents=True, exist_ok=True)
    published: list[tuple[Path, bytes | None]] = []
    with tempfile.TemporaryDirectory(prefix=".cc-switch-import-", dir=dsh_home.parent) as temporary:
        staging = Path(temporary)
        staged_settings = staging / "settings.yaml"
        staged_credentials = staging / ".credentials.yaml"
        staged_marker = staging / "marker"
        _stage_file(staged_settings, settings_content)
        _stage_file(staged_credentials, credentials_content)
        _stage_file(staged_marker, _MARKER_CONTENT.encode("utf-8"))
        try:
            for source, target, original in (
                (staged_settings, settings_path, existing_settings),
                (staged_credentials, credentials_path, existing_credentials),
            ):
                _publish_candidate(source, target, original)
                published.append((target, original))
            _publish_new_file(staged_marker, marker)
        except OSError as publication_error:
            try:
                for target, original in reversed(published):
                    _restore_candidate(target, original)
            except OSError as rollback_error:
                raise rollback_error from publication_error
            _record_completion(marker)
            return CcSwitchImportResult(
                False,
                "completed once without importing after safe publication rollback",
            )

    return CcSwitchImportResult(
        True,
        f"completed: standalone Claude providers imported={len(additions)}; skipped={skipped + len(providers) - len(additions)}",
    )
