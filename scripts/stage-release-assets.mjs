import { constants } from "node:fs";
import { copyFile, mkdir, readdir, readFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import {
  RELEASE_PLATFORMS,
  expectedReleaseAssetNames,
  releaseAssetName,
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

function exactlyOne(files, extension, artifact) {
  const matches = files.filter(
    (path) => path.endsWith(extension) && !path.endsWith(".sig"),
  );
  if (matches.length !== 1) {
    throw new Error(
      `Expected exactly one ${extension} in ${artifact}, found ${matches.length}`,
    );
  }
  return matches[0];
}

export async function stageReleaseAssets({
  artifactRoot,
  destination,
  productName,
  version,
}) {
  await mkdir(destination, { recursive: true });
  if ((await readdir(destination)).length !== 0) {
    throw new Error("Release staging destination must be empty");
  }

  for (const platform of RELEASE_PLATFORMS) {
    const files = await filesBelow(resolve(artifactRoot, platform.artifact));
    for (const extension of new Set([
      platform.installerExt,
      platform.updaterExt,
    ])) {
      const source = exactlyOne(files, extension, platform.artifact);
      const name = releaseAssetName({
        productName,
        version,
        platform,
        ext: extension,
      });
      await copyFile(source, join(destination, name), constants.COPYFILE_EXCL);
    }

    const updater = exactlyOne(files, platform.updaterExt, platform.artifact);
    const signature = `${updater}.sig`;
    if (!files.includes(signature)) {
      throw new Error(`Missing updater signature in ${platform.artifact}`);
    }
    const updaterName = releaseAssetName({
      productName,
      version,
      platform,
      ext: platform.updaterExt,
    });
    await copyFile(
      signature,
      join(destination, `${updaterName}.sig`),
      constants.COPYFILE_EXCL,
    );
  }

  const actual = new Set(await readdir(destination));
  const expected = expectedReleaseAssetNames({ productName, version });
  if (
    actual.size !== expected.size ||
    [...expected].some((name) => !actual.has(name))
  ) {
    throw new Error("Staged release assets do not match the release contract");
  }
  return actual;
}

async function main() {
  const [artifactRoot, destination] = process.argv.slice(2);
  if (!artifactRoot || !destination) {
    throw new Error(
      "Usage: node scripts/stage-release-assets.mjs <artifact-root> <destination>",
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
  const names = await stageReleaseAssets({
    artifactRoot,
    destination,
    productName: tauriConfig.productName,
    version: packageJson.version,
  });
  console.log(`Staged ${names.size} canonical release assets`);
}

if (
  process.argv[1] &&
  resolve(process.argv[1]) === fileURLToPath(import.meta.url)
) {
  await main();
}
