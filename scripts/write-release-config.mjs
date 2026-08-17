import { writeFile } from "node:fs/promises";

const destination = process.argv[2];
const publicKey = process.env.TAURI_UPDATER_PUBLIC_KEY?.trim();
if (!destination || !publicKey) {
  throw new Error(
    "Usage: TAURI_UPDATER_PUBLIC_KEY=<key> node scripts/write-release-config.mjs <output>",
  );
}

const normalized = publicKey.replace(/\s/gu, "");
if (!/^[A-Za-z0-9+/]+={0,2}$/u.test(normalized)) {
  throw new Error("TAURI_UPDATER_PUBLIC_KEY is not valid base64");
}
const decoded = Buffer.from(normalized, "base64").toString("utf8");
const lines = decoded.trimEnd().split(/\r?\n/u);
if (
  lines.length !== 2 ||
  !lines[0].startsWith("untrusted comment: minisign public key:") ||
  !lines[1].startsWith("RW")
) {
  throw new Error(
    "TAURI_UPDATER_PUBLIC_KEY is not a Tauri minisign public key",
  );
}

await writeFile(
  destination,
  `${JSON.stringify({ plugins: { updater: { pubkey: normalized } } })}\n`,
  { mode: 0o600 },
);
console.log(`Wrote validated Tauri release config to ${destination}`);
