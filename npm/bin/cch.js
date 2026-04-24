#!/usr/bin/env node
const path = require("path");
const { spawnSync } = require("child_process");

const ext = process.platform === "win32" ? ".exe" : "";
const bin = path.join(__dirname, "..", "vendor", `cch${ext}`);

const r = spawnSync(bin, process.argv.slice(2), { stdio: "inherit" });
if (r.error) {
  if (r.error.code === "ENOENT") {
    console.error(
      `cch binary not found at ${bin}. Reinstall with: npm i -g cch-tool`,
    );
  } else {
    console.error(`cch failed to start: ${r.error.message}`);
  }
  process.exit(1);
}
process.exit(r.status ?? 0);
