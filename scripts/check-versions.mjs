import { readFile } from "node:fs/promises";

const packageJson = JSON.parse(
  await readFile(new URL("../package.json", import.meta.url), "utf8"),
);
const tauriConfig = JSON.parse(
  await readFile(
    new URL("../src-tauri/tauri.conf.json", import.meta.url),
    "utf8",
  ),
);
const cargo = await readFile(new URL("../Cargo.toml", import.meta.url), "utf8");
const appCargo = await readFile(
  new URL("../src-tauri/Cargo.toml", import.meta.url),
  "utf8",
);
const workflow = await readFile(
  new URL("../.github/workflows/desktop.yml", import.meta.url),
  "utf8",
);
const cargoVersion = cargo.match(
  /\[workspace\.package\][\s\S]*?\nversion\s*=\s*"([^"]+)"/,
)?.[1];
const versions = new Set([
  packageJson.version,
  tauriConfig.version,
  cargoVersion,
]);

if (versions.size !== 1 || versions.has(undefined)) {
  throw new Error(
    `Version mismatch: package=${packageJson.version}, tauri=${tauriConfig.version}, cargo=${cargoVersion}`,
  );
}

const appPackageName = appCargo.match(
  /\[package\][\s\S]*?\nname\s*=\s*"([^"]+)"/u,
)?.[1];
if (
  !appPackageName ||
  packageJson.name !== appPackageName ||
  (tauriConfig.mainBinaryName && tauriConfig.mainBinaryName !== appPackageName)
) {
  throw new Error(
    `Release binary name mismatch: package=${packageJson.name}, cargo=${appPackageName}, tauri=${tauriConfig.mainBinaryName}`,
  );
}

if (
  !workflow.includes("node scripts/stage-release-assets.mjs") ||
  workflow.includes("releaseAssetNamePattern:") ||
  workflow.includes("tagName:")
) {
  throw new Error(
    "Release workflow must stage assets once and must not publish from matrix jobs",
  );
}

const releaseTag = process.env.GITHUB_REF_NAME;
if (
  releaseTag?.startsWith("desktop-v") &&
  releaseTag !== `desktop-v${packageJson.version}`
) {
  throw new Error(
    `Release tag ${releaseTag} does not match desktop-v${packageJson.version}`,
  );
}
