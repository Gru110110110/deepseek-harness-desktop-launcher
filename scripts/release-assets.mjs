export const RELEASE_PLATFORMS = [
  {
    key: "darwin-aarch64",
    artifact: "dsh-launcher-macos-arm64",
    websitePlatform: "mac-arm64",
    arch: "aarch64",
    setup: "",
    updaterExt: ".app.tar.gz",
    installerExt: ".dmg",
  },
  {
    key: "darwin-x86_64",
    artifact: "dsh-launcher-macos-x64",
    websitePlatform: "mac-x64",
    arch: "x64",
    setup: "",
    updaterExt: ".app.tar.gz",
    installerExt: ".dmg",
  },
  {
    key: "windows-x86_64",
    artifact: "dsh-launcher-windows-x64",
    websitePlatform: "win-x64",
    arch: "x64",
    setup: "-setup",
    updaterExt: ".exe",
    installerExt: ".exe",
  },
];

function requireReleaseValue(label, value) {
  if (typeof value !== "string" || !value.trim()) {
    throw new Error(`Release ${label} is required`);
  }
  return value.trim();
}

export function githubAssetName(label) {
  return requireReleaseValue("asset label", label)
    .replace(/[^a-zA-Z0-9_-]/gu, ".")
    .replace(/\.\./gu, ".");
}

export function releaseAssetName({ productName, version, platform, ext }) {
  const name = requireReleaseValue("product name", productName);
  const releaseVersion = requireReleaseValue("version", version);
  if (!platform || !RELEASE_PLATFORMS.includes(platform)) {
    throw new Error("Release platform is invalid");
  }
  if (typeof ext !== "string" || !ext.startsWith(".")) {
    throw new Error("Release asset extension is invalid");
  }
  return githubAssetName(
    `${name}_${releaseVersion}_${platform.arch}${platform.setup}${ext}`,
  );
}

export function releaseDownloadUrl(repository, tag, filename) {
  const repositoryPath = requireReleaseValue("repository", repository)
    .split("/")
    .map((part) => encodeURIComponent(part))
    .join("/");
  return `https://github.com/${repositoryPath}/releases/download/${encodeURIComponent(
    requireReleaseValue("tag", tag),
  )}/${encodeURIComponent(requireReleaseValue("asset filename", filename))}`;
}

export function expectedReleaseAssetNames({ productName, version }) {
  const names = new Set();
  for (const platform of RELEASE_PLATFORMS) {
    const updater = releaseAssetName({
      productName,
      version,
      platform,
      ext: platform.updaterExt,
    });
    names.add(updater);
    names.add(`${updater}.sig`);
    names.add(
      releaseAssetName({
        productName,
        version,
        platform,
        ext: platform.installerExt,
      }),
    );
  }
  return names;
}
