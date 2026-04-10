import { copyFileSync, mkdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { execFileSync } from "node:child_process";

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
