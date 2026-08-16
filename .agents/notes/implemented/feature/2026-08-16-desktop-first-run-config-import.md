# Agent Note: Desktop first-run configuration and history import

Status: implemented

English | [中文](2026-08-16-desktop-first-run-config-import.zh.md)

## Problem

The desktop launcher needs a Harness home isolated from source launches so their service versions and later mutations cannot interfere. A completely separate home also makes a first desktop installation forget the user's existing providers, preferences, instructions, profiles, presets, skills, conversation history, and workspace grouping even when no desktop configuration exists to preserve. Session logs do not contain the workspace registry's ordered ownership account, so copying history alone renders every imported session as Ungrouped after an empty desktop registry has initialized.

## Decision

Before starting the service with the launcher-owned `DSH_HOME`, the desktop launcher checks the versioned `.source-home-import-v1` marker. A complete marker suppresses the import while `dsh-home` contains configuration; an empty configuration remains eligible so clearing it restores first-run behavior. An absent or incomplete marker imports from the default source home `~/.dsh`. Existing destination files always win, while existing configuration directories receive only source descendants whose complete destination paths are absent. This lets a launcher-generated `profiles` or `settings.yaml` coexist with missing source credentials, profiles, presets, and skills instead of suppressing the whole import.

`sessions` and `attachments` form one history unit. The launcher copies the available source directories only when neither destination directory exists; either existing destination directory preserves the complete desktop history unit and suppresses both source directories. After a successful import attempt, including one with nothing missing, the launcher atomically writes the marker so later source additions do not become implicit synchronization. An explicit `DSH_HOME` bypasses both the launcher-owned home and the import.

A separate `.source-workspace-import-v1` marker controls one workspace-grouping snapshot independently of configuration and history. The launcher accepts only a validated workspace v2 `storages/workspace.json` with complete registry fields and no pending mutation. A missing desktop ledger receives the source ledger. A validated initialized desktop ledger with no workspaces and no archived sessions is replaceable because it carries no user grouping or archive state; this repairs a registry initialized before source sessions arrived. A populated, incompatible, symbolic-link-backed, or otherwise unrecognized desktop ledger always wins. An incompatible or empty source ledger completes the snapshot without copying, and the marker prevents later grouping changes from becoming synchronization.

The snapshots exclude every other `storages` file, the home-scoped anonymous user id, every `node_modules` directory, transient `.lock` and `.tmp` writer file, and symbolic links. Dependency installations cannot couple different Harness versions, projection caches cannot cross versions, and copied links cannot make the desktop service read or write through to the source home. Profiles remain configuration and are copied without their installed dependencies; external profile plugins must be installed separately for the desktop home. Session attachments travel with the session log so historical messages do not lose their referenced local files.

The launcher builds each snapshot in a temporary sibling directory. Configuration and history publication identifies maximal missing destination subtrees and publishes only those roots. Workspace publication creates a missing ledger exclusively or atomically replaces only a revalidated empty ledger; failure to write its marker restores the exact empty ledger. Concurrently created, non-empty, or unrecognized destination state is preserved. A failed import rolls back its published entries, reports the migration failure, and does not start the service with a partial snapshot. The source home remains read-only throughout the operation. `server.log` records each completion or skip reason and aggregate copy counts without naming or rendering configuration values.

## Alternatives considered

**Copy the complete source home.** This gives the closest clone, but it also duplicates mutable storage state, anonymous identity, transient writer residue, and version-specific dependency installations that the desktop home exists to isolate.

**Continuously synchronize both homes.** Synchronization makes later edits convenient, but destroys independent configuration ownership and introduces conflict and downgrade behavior between installations that may run different versions.

**Copy only a fixed filename list.** A small allowlist is easy to audit, but silently drops user-authored profiles, presets, skills, and future configuration types. Excluding the known non-history runtime and installation entries preserves configuration and conversations without treating every mutable home entry as portable.

**Rebuild workspaces from imported session headers.** Header cwd values can reconstruct directory buckets, but not workspace ids, display titles, workspace order, manual session order, or archived-session state. Importing the compatible ledger preserves the complete grouping account while keeping unrelated storage state isolated.

## Consequences

First desktop use inherits missing source configuration, conversation history, and compatible workspace grouping without creating an ongoing relationship between the two homes. The configuration/history import repairs a desktop home that contains generated configuration but lacks its versioned marker or history; the independent workspace import repairs an initialized empty grouping ledger even when the earlier import is already complete. The desktop Harness version must accept the copied on-disk formats at import time; after those snapshots, the homes can diverge onto different versions. Clearing all desktop configuration makes its next launch eligible to import again. Existing desktop paths, history, populated grouping, archive state, and unrelated storage remain preserved. A source configuration that depends on symbolic links or already-installed external profile packages needs an explicit desktop-local replacement.

## Testing

The desktop unit suite verifies configuration, session, and attachment copying; recursive missing-only directory merge; excluded entry classes; history import beside launcher-generated configuration; preservation of existing paths and history; rollback; marker-controlled one-time behavior and empty-configuration re-entry; diagnostics; and the explicit `DSH_HOME` bypass. Workspace cases cover a missing ledger, repair of a validated empty ledger after the home import completed, populated-ledger preservation, version rejection, exclusion of projection caches, no later synchronization, and exact restoration when marker publication fails. All fixtures use temporary homes and never inspect or modify the user's real `~/.dsh` or desktop data.
