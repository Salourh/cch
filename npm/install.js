#!/usr/bin/env node
// Downloads the platform-specific `cch` binary from the matching GitHub Release
// and places it at vendor/cch (or vendor/cch.exe on Windows). The wrapper in
// bin/cch.js then execs it.

const fs = require("fs");
const path = require("path");
const os = require("os");
const https = require("https");
const zlib = require("zlib");
const { spawnSync } = require("child_process");

const pkg = require("./package.json");
const REPO = "Salourh/cch";
const VERSION = `v${pkg.version}`;

const TARGETS = {
  "linux-x64": "x86_64-unknown-linux-gnu",
  "linux-arm64": "aarch64-unknown-linux-gnu",
  "darwin-x64": "x86_64-apple-darwin",
  "darwin-arm64": "aarch64-apple-darwin",
  "win32-x64": "x86_64-pc-windows-msvc",
};

function pickTarget() {
  const key = `${process.platform}-${process.arch}`;
  const target = TARGETS[key];
  if (!target) {
    console.error(`[cch-tool] Unsupported platform: ${key}`);
    console.error(`[cch-tool] Supported: ${Object.keys(TARGETS).join(", ")}`);
    process.exit(1);
  }
  return target;
}

function get(url) {
  return new Promise((resolve, reject) => {
    https
      .get(url, { headers: { "user-agent": "cch-tool-installer" } }, (res) => {
        if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
          res.resume();
          return get(res.headers.location).then(resolve, reject);
        }
        if (res.statusCode !== 200) {
          return reject(new Error(`HTTP ${res.statusCode} for ${url}`));
        }
        const chunks = [];
        res.on("data", (c) => chunks.push(c));
        res.on("end", () => resolve(Buffer.concat(chunks)));
        res.on("error", reject);
      })
      .on("error", reject);
  });
}

function extractTarGz(buf, destDir, entryName) {
  const tmp = path.join(os.tmpdir(), `cch-${Date.now()}.tar.gz`);
  fs.writeFileSync(tmp, buf);
  try {
    // Prefer system tar — it's available on Linux, macOS, Windows 10+.
    const r = spawnSync("tar", ["-xzf", tmp, "-C", destDir, entryName], {
      stdio: "inherit",
    });
    if (r.status !== 0) throw new Error(`tar extract failed (exit ${r.status})`);
  } finally {
    try { fs.unlinkSync(tmp); } catch (_) {}
  }
}

function extractZip(buf, destDir, entryName) {
  const tmp = path.join(os.tmpdir(), `cch-${Date.now()}.zip`);
  fs.writeFileSync(tmp, buf);
  try {
    // Windows 10+ has tar.exe which handles zip too.
    const r = spawnSync("tar", ["-xf", tmp, "-C", destDir, entryName], {
      stdio: "inherit",
    });
    if (r.status !== 0) throw new Error(`zip extract failed (exit ${r.status})`);
  } finally {
    try { fs.unlinkSync(tmp); } catch (_) {}
  }
}

async function main() {
  const target = pickTarget();
  const isWin = process.platform === "win32";
  const ext = isWin ? ".exe" : "";
  const archive = isWin ? `cch-${target}.zip` : `cch-${target}.tar.gz`;
  const url = `https://github.com/${REPO}/releases/download/${VERSION}/${archive}`;

  const vendorDir = path.join(__dirname, "vendor");
  fs.mkdirSync(vendorDir, { recursive: true });

  console.log(`[cch-tool] Downloading ${url}`);
  const buf = await get(url);

  if (isWin) extractZip(buf, vendorDir, `cch${ext}`);
  else extractTarGz(buf, vendorDir, `cch${ext}`);

  const binPath = path.join(vendorDir, `cch${ext}`);
  if (!fs.existsSync(binPath)) {
    throw new Error(`Binary not found at ${binPath} after extraction`);
  }
  if (!isWin) fs.chmodSync(binPath, 0o755);
  console.log(`[cch-tool] Installed ${binPath}`);
}

main().catch((err) => {
  console.error(`[cch-tool] Install failed: ${err.message}`);
  console.error(
    `[cch-tool] You can also install via cargo: cargo install cch`,
  );
  process.exit(1);
});
