import { mkdtemp, mkdir, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { afterEach, describe, expect, it } from "vitest";
import { validateUpdaterManifest } from "./check-updater-manifest.mjs";
import { createUpdaterManifest } from "./create-updater-manifest.mjs";

const temporaryDirectories = [];

async function fixture({ omitSignature = false } = {}) {
  const root = await mkdtemp(join(tmpdir(), "dsh-updater-manifest-"));
  temporaryDirectories.push(root);
  const artifacts = [
    ["dsh-launcher-macos-arm64", "DSH_0.2.0_aarch64.app.tar.gz"],
    ["dsh-launcher-macos-x64", "DSH_0.2.0_x64.app.tar.gz"],
    ["dsh-launcher-windows-x64", "DSH_0.2.0_x64-setup.exe"],
  ];
  for (const [directory, filename] of artifacts) {
    const target = join(root, directory, "bundle");
    await mkdir(target, { recursive: true });
    await writeFile(join(target, filename), "updater");
    if (!(omitSignature && directory === "dsh-launcher-windows-x64")) {
      await writeFile(join(target, `${filename}.sig`), "s".repeat(64));
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
    const manifest = await createUpdaterManifest({
      artifactRoot: await fixture(),
      repository: "owner/repository",
      tag: "desktop-v0.2.0",
      version: "0.2.0",
      pubDate: "2026-08-17T00:00:00.000Z",
    });

    expect(Object.keys(manifest.platforms).sort()).toEqual([
      "darwin-aarch64",
      "darwin-x86_64",
      "windows-x86_64",
    ]);
    expect(manifest.platforms["darwin-aarch64"].url).toBe(
      "https://github.com/owner/repository/releases/download/desktop-v0.2.0/DSH_0.2.0_aarch64.app.tar.gz",
    );
    expect(manifest.platforms["windows-x86_64"].signature).toHaveLength(64);
    expect(() =>
      validateUpdaterManifest(manifest, {
        expectedVersion: "0.2.0",
        repository: "owner/repository",
        tag: "desktop-v0.2.0",
        assetNames: new Set([
          "DSH_0.2.0_aarch64.app.tar.gz",
          "DSH_0.2.0_x64.app.tar.gz",
          "DSH_0.2.0_x64-setup.exe",
        ]),
      }),
    ).not.toThrow();
  });

  it("refuses to publish an incomplete platform set", async () => {
    await expect(
      createUpdaterManifest({
        artifactRoot: await fixture({ omitSignature: true }),
        repository: "owner/repository",
        tag: "desktop-v0.2.0",
        version: "0.2.0",
      }),
    ).rejects.toThrow("Missing updater signature");
  });

  it("rejects a wrong version or a release with a missing asset", async () => {
    const manifest = await createUpdaterManifest({
      artifactRoot: await fixture(),
      repository: "owner/repository",
      tag: "desktop-v0.2.0",
      version: "0.2.0",
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
  });
});
