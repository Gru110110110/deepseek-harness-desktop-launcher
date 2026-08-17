import { mkdtemp, mkdir, readdir, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { afterEach, describe, expect, it } from "vitest";
import {
  validateReleaseAssets,
  validateUpdaterManifest,
} from "./check-updater-manifest.mjs";
import { createUpdaterManifest } from "./create-updater-manifest.mjs";
import { expectedReleaseAssetNames } from "./release-assets.mjs";
import { stageReleaseAssets } from "./stage-release-assets.mjs";

const temporaryDirectories = [];

async function fixture({ omitSignature = false } = {}) {
  const root = await mkdtemp(join(tmpdir(), "dsh-updater-manifest-"));
  temporaryDirectories.push(root);
  const artifacts = [
    [
      "dsh-launcher-macos-arm64",
      "DSH Launcher.app.tar.gz",
      "DSH Launcher_0.2.0_aarch64.dmg",
    ],
    [
      "dsh-launcher-macos-x64",
      "DSH Launcher.app.tar.gz",
      "DSH Launcher_0.2.0_x64.dmg",
    ],
    [
      "dsh-launcher-windows-x64",
      "DSH Launcher_0.2.0_x64-setup.exe",
      "DSH Launcher_0.2.0_x64-setup.exe",
    ],
  ];
  for (const [directory, updater, installer] of artifacts) {
    const target = join(root, directory, "bundle");
    await mkdir(target, { recursive: true });
    for (const filename of new Set([updater, installer])) {
      await writeFile(join(target, filename), "artifact");
    }
    if (!(omitSignature && directory === "dsh-launcher-windows-x64")) {
      await writeFile(join(target, `${updater}.sig`), "s".repeat(64));
    }
  }
  return root;
}

afterEach(async () => {
  await Promise.all(
    temporaryDirectories.splice(0).map((path) => rm(path, { recursive: true })),
  );
});

describe("updater manifest generation", () => {
  it("combines all signed platform artifacts once", async () => {
    const artifactRoot = await fixture();
    const manifest = await createUpdaterManifest({
      artifactRoot,
      repository: "owner/repository",
      tag: "desktop-v0.2.0",
      version: "0.2.0",
      productName: "DSH Launcher",
      pubDate: "2026-08-17T00:00:00.000Z",
    });

    expect(Object.keys(manifest.platforms).sort()).toEqual([
      "darwin-aarch64",
      "darwin-x86_64",
      "windows-x86_64",
    ]);
    expect(manifest.platforms["darwin-aarch64"].url).toBe(
      "https://github.com/owner/repository/releases/download/desktop-v0.2.0/DSH.Launcher_0.2.0_aarch64.app.tar.gz",
    );
    expect(manifest.platforms["darwin-x86_64"].url).toBe(
      "https://github.com/owner/repository/releases/download/desktop-v0.2.0/DSH.Launcher_0.2.0_x64.app.tar.gz",
    );
    expect(manifest.platforms["windows-x86_64"].signature).toHaveLength(64);
    const assetNames = expectedReleaseAssetNames({
      productName: "DSH Launcher",
      version: "0.2.0",
    });
    expect([...assetNames].sort()).toEqual([
      "DSH.Launcher_0.2.0_aarch64.app.tar.gz",
      "DSH.Launcher_0.2.0_aarch64.app.tar.gz.sig",
      "DSH.Launcher_0.2.0_aarch64.dmg",
      "DSH.Launcher_0.2.0_x64-setup.exe",
      "DSH.Launcher_0.2.0_x64-setup.exe.sig",
      "DSH.Launcher_0.2.0_x64.app.tar.gz",
      "DSH.Launcher_0.2.0_x64.app.tar.gz.sig",
      "DSH.Launcher_0.2.0_x64.dmg",
    ]);
    expect(() =>
      validateUpdaterManifest(manifest, {
        expectedVersion: "0.2.0",
        repository: "owner/repository",
        tag: "desktop-v0.2.0",
        assetNames,
      }),
    ).not.toThrow();
    expect(() =>
      validateReleaseAssets(assetNames, {
        productName: "DSH Launcher",
        version: "0.2.0",
      }),
    ).not.toThrow();

    const destination = join(artifactRoot, "staged");
    const staged = await stageReleaseAssets({
      artifactRoot,
      destination,
      productName: "DSH Launcher",
      version: "0.2.0",
    });
    expect([...staged].sort()).toEqual([...assetNames].sort());
    expect((await readdir(destination)).sort()).toEqual([...assetNames].sort());
  });

  it("refuses to publish an incomplete platform set", async () => {
    await expect(
      createUpdaterManifest({
        artifactRoot: await fixture({ omitSignature: true }),
        repository: "owner/repository",
        tag: "desktop-v0.2.0",
        version: "0.2.0",
        productName: "DSH Launcher",
      }),
    ).rejects.toThrow("Missing updater signature");
  });

  it("rejects a wrong version or a release with a missing asset", async () => {
    const manifest = await createUpdaterManifest({
      artifactRoot: await fixture(),
      repository: "owner/repository",
      tag: "desktop-v0.2.0",
      version: "0.2.0",
      productName: "DSH Launcher",
    });

    expect(() =>
      validateUpdaterManifest(manifest, { expectedVersion: "0.3.0" }),
    ).toThrow("does not match");
    expect(() =>
      validateUpdaterManifest(manifest, {
        expectedVersion: "0.2.0",
        assetNames: new Set(),
      }),
    ).toThrow("Release asset is missing");

    const releaseAssets = expectedReleaseAssetNames({
      productName: "DSH Launcher",
      version: "0.2.0",
    });
    releaseAssets.delete("DSH.Launcher_0.2.0_aarch64.dmg");
    expect(() =>
      validateReleaseAssets(releaseAssets, {
        productName: "DSH Launcher",
        version: "0.2.0",
      }),
    ).toThrow("DSH.Launcher_0.2.0_aarch64.dmg");
  });
});
