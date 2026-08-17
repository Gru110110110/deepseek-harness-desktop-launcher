import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";

const expectedPlatforms = ["darwin-aarch64", "darwin-x86_64", "windows-x86_64"];

function encodedReleasePath(repository, tag) {
  const repositoryPath = repository
    .split("/")
    .map((part) => encodeURIComponent(part))
    .join("/");
  return `/${repositoryPath}/releases/download/${encodeURIComponent(tag)}/`;
}

export function validateUpdaterManifest(
  manifest,
  { expectedVersion, assetNames, repository, tag } = {},
) {
  if (
    typeof manifest.version !== "string" ||
    !manifest.version ||
    (expectedVersion && manifest.version !== expectedVersion)
  ) {
    throw new Error(
      `Updater manifest version ${String(manifest.version)} does not match ${expectedVersion}`,
    );
  }
  if (typeof manifest.notes !== "string") {
    throw new Error("Updater manifest notes must be a string");
  }
  if (
    typeof manifest.pub_date !== "string" ||
    Number.isNaN(Date.parse(manifest.pub_date))
  ) {
    throw new Error("Updater manifest has an invalid pub_date");
  }

  for (const platform of expectedPlatforms) {
    const entry = manifest.platforms?.[platform];
    let url;
    try {
      url = new URL(entry?.url);
    } catch {
      throw new Error(`Updater manifest has an invalid URL for ${platform}`);
    }
    if (url.protocol !== "https:") {
      throw new Error(
        `Updater manifest is missing a secure URL for ${platform}`,
      );
    }
    if (
      repository &&
      tag &&
      (url.origin !== "https://github.com" ||
        !url.pathname.startsWith(encodedReleasePath(repository, tag)))
    ) {
      throw new Error(
        `Updater URL does not target this release for ${platform}`,
      );
    }
    if (url.search || url.hash) {
      throw new Error(
        `Updater URL contains unexpected parameters for ${platform}`,
      );
    }
    const encodedName = url.pathname.split("/").at(-1);
    let assetName;
    try {
      assetName = decodeURIComponent(encodedName ?? "");
    } catch {
      throw new Error(`Updater URL has an invalid asset name for ${platform}`);
    }
    if (!assetName || (assetNames && !assetNames.has(assetName))) {
      throw new Error(`Release asset is missing for ${platform}: ${assetName}`);
    }
    if (
      typeof entry.signature !== "string" ||
      entry.signature.trim().length < 32
    ) {
      throw new Error(
        `Updater manifest is missing a signature for ${platform}`,
      );
    }
  }
}

async function main() {
  const [manifestPath, releaseAssetsPath] = process.argv.slice(2);
  if (!manifestPath) {
    throw new Error(
      "Usage: node scripts/check-updater-manifest.mjs <latest.json> [release-assets.json]",
    );
  }
  const packageJson = JSON.parse(
    await readFile(new URL("../package.json", import.meta.url), "utf8"),
  );
  const manifest = JSON.parse(await readFile(manifestPath, "utf8"));
  let assetNames;
  if (releaseAssetsPath) {
    const release = JSON.parse(await readFile(releaseAssetsPath, "utf8"));
    if (!Array.isArray(release.assets)) {
      throw new Error("Release assets response is invalid");
    }
    assetNames = new Set(release.assets.map((asset) => asset.name));
  }
  validateUpdaterManifest(manifest, {
    expectedVersion: packageJson.version,
    assetNames,
    repository: process.env.GITHUB_REPOSITORY,
    tag: process.env.GITHUB_REF_NAME,
  });
  console.log(
    `Updater manifest passed for ${expectedPlatforms.join(", ")}${assetNames ? " with release assets" : ""}`,
  );
}

if (
  process.argv[1] &&
  resolve(process.argv[1]) === fileURLToPath(import.meta.url)
) {
  await main();
}
