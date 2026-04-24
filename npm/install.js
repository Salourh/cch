#!/usr/bin/env node
// Downloads the platform-specific `cch` binary from the matching GitHub Release
// and places it at vendor/cch (or vendor/cch.exe on Windows). The wrapper in
// bin/cch.js then execs it.
//
// Security:
//   - HTTPS only; redirects restricted to a hostname allowlist.
//   - Archive SHA-256 verified against `checksums` in package.json before
//     extraction. Missing checksum = hard fail (no unverified fallback).

const fs = require("fs");
const path = require("path");
const os = require("os");
const https = require("https");
const crypto = require("crypto");
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

// Hosts we accept for the release download and any redirects along the way.
// GitHub serves /releases/download/... as a 302 to objects.githubusercontent.com
// (signed URL). Anything else = refuse.
const ALLOWED_HOSTS = new Set([
  "github.com",
  "objects.githubusercontent.com",
  "release-assets.githubusercontent.com",
]);

const MAX_REDIRECTS = 5;
const MAX_BYTES = 50 * 1024 * 1024; // 50MB ceiling, archives are ~2MB

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

function assertSafeUrl(urlStr, baseStr) {
  let u;
  try {
    u = baseStr ? new URL(urlStr, baseStr) : new URL(urlStr);
  } catch (_) {
    throw new Error(`Refusing malformed URL`);
  }
  if (u.protocol !== "https:") {
    throw new Error(`Refusing non-https redirect to scheme "${u.protocol}"`);
  }
  if (!ALLOWED_HOSTS.has(u.hostname.toLowerCase())) {
    throw new Error(`Refusing redirect to disallowed host "${u.hostname}"`);
  }
  return u;
}

function get(urlStr, hops = 0) {
  return new Promise((resolve, reject) => {
    let u;
    try {
      u = assertSafeUrl(urlStr);
    } catch (e) {
      return reject(e);
    }
    https
      .get(
        u,
        { headers: { "user-agent": "cch-tool-installer" } },
        (res) => {
          if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
            res.resume();
            if (hops >= MAX_REDIRECTS) {
              return reject(new Error(`Too many redirects (>${MAX_REDIRECTS})`));
            }
            let nextUrl;
            try {
              // Resolve relative Location headers against the current URL,
              // then re-validate scheme + host. Don't log the resolved URL —
              // GitHub signed URLs carry short-lived tokens in the query string.
              nextUrl = assertSafeUrl(res.headers.location, u.href);
            } catch (e) {
              return reject(e);
            }
            return get(nextUrl.href, hops + 1).then(resolve, reject);
          }
          if (res.statusCode !== 200) {
            res.resume();
            return reject(new Error(`HTTP ${res.statusCode} for ${u.origin}${u.pathname}`));
          }
          const chunks = [];
          let total = 0;
          res.on("data", (c) => {
            total += c.length;
            if (total > MAX_BYTES) {
              res.destroy(new Error(`Response exceeds ${MAX_BYTES} bytes`));
              return;
            }
            chunks.push(c);
          });
          res.on("end", () => resolve(Buffer.concat(chunks)));
          res.on("error", reject);
        },
      )
      .on("error", reject);
  });
}

function verifyChecksum(buf, archiveName) {
  const checksums = pkg.checksums || {};
  const expected = checksums[archiveName];
  if (!expected || typeof expected !== "string" || !/^[a-f0-9]{64}$/i.test(expected)) {
    throw new Error(
      `Missing or malformed SHA-256 for "${archiveName}" in package.json. ` +
      `Refusing to install unverified binary.`,
    );
  }
  const actual = crypto.createHash("sha256").update(buf).digest("hex");
  const a = Buffer.from(actual, "hex");
  const b = Buffer.from(expected.toLowerCase(), "hex");
  if (a.length !== b.length || !crypto.timingSafeEqual(a, b)) {
    throw new Error(
      `SHA-256 mismatch for ${archiveName}\n` +
      `  expected: ${expected.toLowerCase()}\n` +
      `  actual:   ${actual}`,
    );
  }
}

function tmpFile(suffix) {
  const rand = crypto.randomBytes(8).toString("hex");
  return path.join(os.tmpdir(), `cch-${process.pid}-${rand}${suffix}`);
}

function extract(buf, destDir, entryName, isZip) {
  const tmp = tmpFile(isZip ? ".zip" : ".tar.gz");
  fs.writeFileSync(tmp, buf, { mode: 0o600 });
  try {
    // System tar is available on Linux, macOS, and Windows 10+ (tar.exe also
    // handles zip). shell: false + fully-literal args = no injection surface;
    // entryName is a hardcoded string constant.
    const args = isZip
      ? ["-xf", tmp, "-C", destDir, entryName]
      : ["-xzf", tmp, "-C", destDir, entryName];
    const r = spawnSync("tar", args, { stdio: "inherit", shell: false });
    if (r.status !== 0) throw new Error(`tar extract failed (exit ${r.status})`);
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

  verifyChecksum(buf, archive);
  console.log(`[cch-tool] Verified SHA-256 for ${archive}`);

  extract(buf, vendorDir, `cch${ext}`, isWin);

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
