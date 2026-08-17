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

const releaseTag = process.env.GITHUB_REF_NAME;
if (
  releaseTag?.startsWith("desktop-v") &&
  releaseTag !== `desktop-v${packageJson.version}`
) {
  throw new Error(
    `Release tag ${releaseTag} does not match desktop-v${packageJson.version}`,
  );
}
