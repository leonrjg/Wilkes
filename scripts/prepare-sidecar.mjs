import { createHash } from "node:crypto";
import {
  chmodSync,
  copyFileSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  readFileSync,
  rmSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { execFileSync } from "node:child_process";

// The `uv` that builds Python environments. Pinned with its checksums because
// it decides what gets installed on every machine. Dockerfile pins it too.
const UV_VERSION = "0.11.28";
const UV_SHA256 = {
  "aarch64-apple-darwin": "33540eb7c883ab857eff79bd5ac2aa31fe27b595abecb4a9c003a2c998447232",
  "x86_64-unknown-linux-gnu": "e490a6464492183c5d4534a5527fb4440f7f2bb2f228162ad7e4afe076dc0224",
  "x86_64-pc-windows-msvc": "0a23463216d09c6a72ff80ef5dc5a795f07dc1575cb84d24596c2f124a441b7b",
};

const args = process.argv.slice(2);
const isRelease = args.includes("--release");

const featuresIndex = args.indexOf("--features");
const features =
  featuresIndex >= 0 && featuresIndex + 1 < args.length
    ? args[featuresIndex + 1]
    : null;

const scriptDir = dirname(fileURLToPath(import.meta.url));
const repoRoot = dirname(scriptDir);
const desktopDir = join(repoRoot, "crates", "desktop");
const sidecarsDir = join(desktopDir, "sidecars");

function resolveTargetTriple() {
  if (process.env.TAURI_ENV_TARGET_TRIPLE) {
    return process.env.TAURI_ENV_TARGET_TRIPLE;
  }

  const output = execFileSync("rustc", ["-vV"], {
    cwd: repoRoot,
    encoding: "utf8",
  });
  const hostLine = output
    .split("\n")
    .find((line) => line.startsWith("host: "));

  if (!hostLine) {
    throw new Error("Could not determine Rust host target triple");
  }

  return hostLine.slice("host: ".length).trim();
}

const targetTriple = resolveTargetTriple();
const isWindows = targetTriple.includes("windows");
const extension = isWindows ? ".exe" : "";
const profile = isRelease ? "release" : "debug";

const cargoArgs = [
  "build",
  "--locked",
  "--package",
  "wilkes-rust-worker",
  "--bin",
  "wilkes-rust-worker",
  "--target",
  targetTriple,
];

if (isRelease) {
  cargoArgs.push("--release");
}

if (features) {
  cargoArgs.push("--features", features);
}

execFileSync("cargo", cargoArgs, {
  cwd: repoRoot,
  stdio: "inherit",
});

const builtBinary = join(
  repoRoot,
  "target",
  targetTriple,
  profile,
  `wilkes-rust-worker${extension}`,
);
const bundledBinary = join(
  sidecarsDir,
  `wilkes-rust-worker-${targetTriple}${extension}`,
);

mkdirSync(sidecarsDir, { recursive: true });
copyFileSync(builtBinary, bundledBinary);

console.log(`Prepared sidecar: ${bundledBinary}`);

function findFile(dir, name) {
  for (const entry of readdirSync(dir)) {
    const path = join(dir, entry);
    if (statSync(path).isDirectory()) {
      const found = findFile(path, name);
      if (found) return found;
    } else if (entry === name) {
      return path;
    }
  }
  return null;
}

async function prepareUv() {
  const expected = UV_SHA256[targetTriple];
  if (!expected) {
    throw new Error(`No uv build is pinned for ${targetTriple}`);
  }
  const bundled = join(sidecarsDir, `uv-${targetTriple}${extension}`);
  const stamp = `${bundled}.version`;
  if (existsSync(bundled) && existsSync(stamp) && readFileSync(stamp, "utf8") === UV_VERSION) {
    return;
  }

  const archiveName = `uv-${targetTriple}${isWindows ? ".zip" : ".tar.gz"}`;
  const url = `https://github.com/astral-sh/uv/releases/download/${UV_VERSION}/${archiveName}`;
  const response = await fetch(url);
  if (!response.ok) {
    throw new Error(`Downloading ${url} failed: ${response.status}`);
  }
  const archive = Buffer.from(await response.arrayBuffer());
  const actual = createHash("sha256").update(archive).digest("hex");
  if (actual !== expected) {
    throw new Error(`Checksum mismatch for ${archiveName}: ${actual}`);
  }

  const workspace = mkdtempSync(join(tmpdir(), "wilkes-uv-"));
  try {
    const archivePath = join(workspace, archiveName);
    writeFileSync(archivePath, archive);
    // bsdtar, which is what Windows ships as tar, reads zip as well.
    execFileSync("tar", ["-xf", archivePath, "-C", workspace], { stdio: "inherit" });
    const extracted = findFile(workspace, `uv${extension}`);
    if (!extracted) {
      throw new Error(`${archiveName} holds no uv${extension}`);
    }
    copyFileSync(extracted, bundled);
    chmodSync(bundled, 0o755);
    writeFileSync(stamp, UV_VERSION);
  } finally {
    rmSync(workspace, { recursive: true, force: true });
  }
  console.log(`Prepared sidecar: ${bundled}`);
}

await prepareUv();
