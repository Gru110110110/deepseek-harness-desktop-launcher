import { readdir, readFile, writeFile } from "node:fs/promises";
import { basename, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const platforms = [
  {
    key: "darwin-aarch64",
    artifact: "dsh-launcher-macos-arm64",
    suffix: ".app.tar.gz",
  },
  {
    key: "darwin-x86_64",
    artifact: "dsh-launcher-macos-x64",
    suffix: ".app.tar.gz",
  },
  {
    key: "windows-x86_64",
    artifact: "dsh-launcher-windows-x64",
    suffix: ".exe",
  },
];

async function filesBelow(directory) {
  const files = [];
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) files.push(...(await filesBelow(path)));
    else if (entry.isFile()) files.push(path);
  }
  return files;
}

function releaseUrl(repository, tag, filename) {
  const repositoryPath = repository
    .split("/")
    .map((part) => encodeURIComponent(part))
    .join("/");
  return `https://github.com/${repositoryPath}/releases/download/${encodeURIComponent(tag)}/${encodeURIComponent(filename)}`;
}

export async function createUpdaterManifest({
  artifactRoot,
  repository,
  tag,
  version,
  pubDate = new Date().toISOString(),
  notes = "See the repository changelog and installation guide for this release.",
}) {
  if (!/^[^/\s]+\/[^/\s]+$/u.test(repository)) {
    throw new Error(`Invalid GitHub repository: ${repository}`);
  }
  if (!tag || !version) throw new Error("Release tag and version are required");

  const entries = {};
  const releaseNames = new Set();
  for (const platform of platforms) {
    const directory = resolve(artifactRoot, platform.artifact);
    const files = await filesBelow(directory);
    const candidates = files.filter(
      (path) => path.endsWith(platform.suffix) && !path.endsWith(".sig"),
    );
    if (candidates.length !== 1) {
      throw new Error(
        `Expected exactly one ${platform.suffix} updater in ${platform.artifact}, found ${candidates.length}`,
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
    const name = basename(updater);
    if (releaseNames.has(name)) {
      throw new Error(`Duplicate release asset name: ${name}`);
    }
    releaseNames.add(name);
    entries[platform.key] = {
      signature,
      url: releaseUrl(repository, tag, name),
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
