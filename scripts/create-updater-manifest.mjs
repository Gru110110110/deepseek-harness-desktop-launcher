import { readdir, readFile, writeFile } from "node:fs/promises";
import { basename, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import {
  RELEASE_PLATFORMS,
  releaseAssetName,
  releaseDownloadUrl,
} from "./release-assets.mjs";

async function filesBelow(directory) {
  const files = [];
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) files.push(...(await filesBelow(path)));
    else if (entry.isFile()) files.push(path);
  }
  return files;
}

export async function createUpdaterManifest({
  artifactRoot,
  repository,
  tag,
  version,
  productName,
  pubDate = new Date().toISOString(),
  notes = "See the repository changelog and installation guide for this release.",
}) {
  if (!/^[^/\s]+\/[^/\s]+$/u.test(repository)) {
    throw new Error(`Invalid GitHub repository: ${repository}`);
  }
  if (!tag || !version) throw new Error("Release tag and version are required");

  const entries = {};
  const releaseNames = new Set();
  for (const platform of RELEASE_PLATFORMS) {
    const directory = resolve(artifactRoot, platform.artifact);
    const files = await filesBelow(directory);
    const candidates = files.filter(
      (path) => path.endsWith(platform.updaterExt) && !path.endsWith(".sig"),
    );
    if (candidates.length !== 1) {
      throw new Error(
        `Expected exactly one ${platform.updaterExt} updater in ${platform.artifact}, found ${candidates.length}`,
      );
    }
    const updater = candidates[0];
    const signaturePath = `${updater}.sig`;
    if (!files.includes(signaturePath)) {
      throw new Error(`Missing updater signature for ${basename(updater)}`);
    }
    const signature = (await readFile(signaturePath, "utf8")).trim();
    if (signature.length < 32) {
      throw new Error(`Invalid updater signature for ${basename(updater)}`);
    }
    const name = releaseAssetName({
      productName,
      version,
      platform,
      ext: platform.updaterExt,
    });
    if (releaseNames.has(name)) {
      throw new Error(`Duplicate release asset name: ${name}`);
    }
    releaseNames.add(name);
    entries[platform.key] = {
      signature,
      url: releaseDownloadUrl(repository, tag, name),
    };
  }

  return { version, notes, pub_date: pubDate, platforms: entries };
}

async function main() {
  const [artifactRoot, destination] = process.argv.slice(2);
  const repository = process.env.GITHUB_REPOSITORY;
  const tag = process.env.GITHUB_REF_NAME;
  if (!artifactRoot || !destination || !repository || !tag) {
    throw new Error(
      "Usage: GITHUB_REPOSITORY=<owner/repo> GITHUB_REF_NAME=<tag> node scripts/create-updater-manifest.mjs <artifact-root> <latest.json>",
    );
  }
  const packageJson = JSON.parse(
    await readFile(new URL("../package.json", import.meta.url), "utf8"),
  );
  const tauriConfig = JSON.parse(
    await readFile(
      new URL("../src-tauri/tauri.conf.json", import.meta.url),
      "utf8",
    ),
  );
  if (tag !== `desktop-v${packageJson.version}`) {
    throw new Error(
      `Release tag ${tag} does not match desktop-v${packageJson.version}`,
    );
  }
  const manifest = await createUpdaterManifest({
    artifactRoot,
    repository,
    tag,
    version: packageJson.version,
    productName: tauriConfig.productName,
  });
  await writeFile(destination, `${JSON.stringify(manifest, null, 2)}\n`, {
    mode: 0o600,
  });
  console.log(`Wrote complete updater manifest to ${destination}`);
}

if (
  process.argv[1] &&
  resolve(process.argv[1]) === fileURLToPath(import.meta.url)
) {
  await main();
}
