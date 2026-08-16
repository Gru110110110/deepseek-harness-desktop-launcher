# AGENTS.md

DSH Launcher is a Python/PyInstaller launcher for DeepSeek Harness that installs and runs the published `@deepseek-ai/dsh` package. Keep it independent from the Harness source workspace: integration happens through the published CLI and its documented output.

## Data safety

- Tests, builds, packaging, and install checks must use temporary `DSH_DESKTOP_HOME`, `DSH_HOME`, and source-home paths. They must never read, write, copy, migrate, delete, or overwrite real `~/.dsh-desktop`, `~/.dsh`, Keychain, credential stores, or production user data.
- Treat activation codes, encryption keys, tokens, configuration, sessions, attachments, and workspace ledgers as user data. Never mutate or migrate real instances without read-only discovery, impact analysis, a verified backup and restore rehearsal, and explicit user approval for the exact target and action.
- Preserve an active runtime and user data when deployment or import fails. Candidate runtime content is staged and validated before publication; import copies only supported entries and never replaces an existing destination.
- Keep test fixtures physically isolated from production paths. Local loopback servers and fake runtimes belong in temporary directories and must be stopped by the owning test.

## Commands

```sh
python3.11 -m unittest discover -s tests -v
python3.11 main.py --check
./build-local.sh
```

Run `main.py --check` only with an isolated temporary `DSH_DESKTOP_HOME`. `build-local.bat` is the Windows packaging entry point.

## Conventions

- Support Python 3.11 with no runtime dependencies beyond the standard library; build-only dependencies belong in `requirements-build.txt`.
- Keep subprocess calls shell-free and pass URLs and paths as separate arguments.
- Validate downloaded Node archives against the pinned SHA-256 before extraction, reject archive traversal and links outside the expected top-level directory, and install an exact Harness version.
- Keep English and Simplified Chinese UI dictionaries structurally identical. Update `README.md` and `README.zh.md` together.
- Do not commit `.build-venv/`, `dist/`, PyInstaller work directories, `__pycache__/`, or platform metadata.
