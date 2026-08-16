# Agent Note: Desktop runtime deployment reliability

Status: implemented

English | [中文](2026-08-16-desktop-runtime-deployment-reliability.zh.md)

## Problem

The desktop launcher must prepare Node.js and `@deepseek-ai/dsh` before the local service can start, including on networks where the official Node or npm endpoint is slow or unreachable. The original path passed a floating npm target into installation, trusted downloaded archives without an independent digest, used the POSIX npm path on Windows, allowed network and npm work to outlive the window, and deleted the active dependency tree before an update had succeeded. File-existence checks, unequal-version update prompts, and a build-only release workflow could report readiness or publish installers without exercising those failure modes.

## Decision

Each desktop release pins one exact Node version and the SHA-256 of every supported Node archive. A first Harness installation concurrently queries the configured registries for valid `latest` metadata, selects the highest SemVer, and freezes that value as the exact target for the deployment transaction. Node transport defaults to the official distribution and npmmirror; npm defaults to the official registry and npmmirror. Explicit singular or ordered plural source configuration is authoritative and suppresses those public defaults. HTTP operations have an idle bound, the complete Node transfer and npm installation have separate total bounds, partial Node bytes and the npm content cache survive retry, and registry probes prefer a source that proves the selected exact package version is reachable. Proxy and certificate variables cross into npm, while ambient API keys, passwords, tokens, the user home, and user npm configuration do not.

The launcher verifies the Node archive against the pinned SHA-256 before validating every archive member path and extracting it. Platform-specific npm discovery uses `lib/node_modules/npm` on POSIX and `node_modules/npm` on Windows. Node and Harness install into unique sibling staging directories. Node `--version`, the Harness package manifest, and installed `dsh --version` must agree with the pinned or selected exact versions before publication. A cross-process lock serializes writers; the version marker is an atomic file replacement.

Deployment reports named activities for writer-lock acquisition, installed-runtime validation, version resolution, Node transfer and verification, registry selection, Harness installation and validation, activation, and service startup. The UI renders byte-derived percentage only for Node transfer. Activities without a truthful total retain an indeterminate bar while showing the exact target, selected source when relevant, and an elapsed timer that updates every second; the event queue keeps Tk mutations on the main thread.

An update never mutates the active Harness directory. Publication retains the former Node and Harness directories, switches only after staging validation, and restores the complete former pair and marker if the Harness phase, publication, or post-publication validation fails. Startup recovers interrupted renames, repairs a missing marker only after executable validation, and selects a valid pairing from the active and previous directories when a process stopped between component publications. One `DeploymentController` owns each deployment subprocess and its process group; window close cancels it, waits for termination, escalates when needed, and prevents another worker from starting concurrently in the same launcher.

Update discovery queries configured registries concurrently, selects the highest valid SemVer result, and offers it only when it is strictly newer than the installed version. The update action carries that exact version into deployment rather than resolving `latest` again.

## Alternatives considered

**Bundle the complete runtime in every desktop installer.** This removes first-run network dependence, but duplicates a large native dependency tree across three platform packages and couples every Harness update to a new desktop release. The thin launcher keeps bounded multi-source installation and persistent caches; an offline bundle remains an independent distribution option if measured regional failure rates justify its release and storage cost.

**Pin Harness to the desktop release.** A fixed package is rehearsable, but it can lag the registry or become undeployable when desktop and npm publication order diverge. Resolving `latest` first and freezing the returned exact version for one transaction gives a current installation without letting npm re-resolve during probe, retry, validation, or publication. Node remains pinned because native platform archives require release-time hashes.

**Retry installation in the active directory.** Reusing one tree uses less disk, but a timeout, cancellation, or mirror failure destroys the last working version. Staging plus one retained previous directory spends disk to preserve rollback.

**Trust HTTPS transport without a release digest.** TLS authenticates the selected mirror, not equality with the Node release reviewed by this repository. The pinned SHA-256 allows several transports to serve the same admitted bytes.

## Testing

- The runtime unit suite covers SemVer ordering, authoritative source configuration, concurrent registry preference, bounded cancellation, secret scrubbing, deployment locking, checksum rejection, mirror fallback, partial-download resume, archive traversal rejection, executable readiness, interrupted single- and two-component publication, and failed-update pair rollback.
- Activity tests cover every blocking deployment phase and require matching Chinese and English copy, including the selected Harness version, sanitized source, and elapsed time.
- A local HTTP fixture serves a synthetic platform-shaped Node archive through the real downloader. The real deployment controller executes the archive's platform-specific npm entry, installs a synthetic exact Harness package, publishes it, and validates its CLI without touching public networks or user data.
- The desktop workflow runs the complete suite on macOS arm64, macOS x64, and Windows x64 before packaging, then executes each frozen launcher through `--check` against an isolated temporary desktop home.
- Before packaging, each platform job requires both default Node manifests to list the pinned platform archive and SHA-256, and both npm registries to expose valid Harness `latest` metadata.

## Consequences

First installation still needs at least one configured Node transport and npm registry unless a verified cache is already present. It also requires valid `latest` metadata before Node download begins; failure is explicit instead of silently deploying an obsolete fallback. npm does not expose a stable completion total for dependency resolution and extraction, so its activity remains indeterminate while the changing phase and elapsed timer distinguish work from a frozen window. Maintainers must update the Node version and all platform hashes together, while Harness publication can advance independently of desktop releases. The cache and one previous runtime consume additional disk, while exact per-transaction selection, resumable transfers, bounded shutdown, Windows parity, and rollback keep transient network or process failure from replacing the last valid runtime.
