// Verifies that the development mock can only ever activate against a local
// dev server — never inside the packaged app.
//
//   node app/ui/test/dev-mock.test.mjs
//
// This matters more than it looks. If the mock activated in the shipped app it
// would render invented statistics and a settings window whose controls change
// nothing, while looking completely normal.

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import vm from "node:vm";
import assert from "node:assert/strict";

const here = dirname(fileURLToPath(import.meta.url));
const source = readFileSync(join(here, "../dev-mock.js"), "utf8");

/** Run dev-mock.js against a fake window and report whether it took over. */
function activates({ protocol, hostname, tauriPresent }) {
  const window = tauriPresent ? { __TAURI__: { core: { invoke: () => {} } } } : {};
  const context = {
    window,
    location: { protocol, hostname },
    console: { info() {}, warn() {} },
    navigator: { userAgent: "test" },
    structuredClone: (v) => JSON.parse(JSON.stringify(v)),
  };
  context.globalThis = context;
  vm.createContext(context);
  vm.runInContext(source, context);
  // The mock announces itself by installing the bridge where there was none.
  return !tauriPresent && Boolean(window.__TAURI__);
}

const cases = [
  // The only case that should activate.
  { name: "dev server on localhost", protocol: "http:", hostname: "localhost",
    tauriPresent: false, expect: true },
  { name: "dev server on 127.0.0.1", protocol: "http:", hostname: "127.0.0.1",
    tauriPresent: false, expect: true },

  // Packaged app, macOS: tauri://localhost
  { name: "packaged app (macOS)", protocol: "tauri:", hostname: "localhost",
    tauriPresent: false, expect: false },
  // Packaged app, Windows: http://tauri.localhost
  { name: "packaged app (Windows)", protocol: "http:", hostname: "tauri.localhost",
    tauriPresent: false, expect: false },
  // A real remote host must never get invented data either.
  { name: "remote host", protocol: "https:", hostname: "example.com",
    tauriPresent: false, expect: false },
  // With the real bridge present the mock must stand aside everywhere.
  { name: "dev server with real bridge", protocol: "http:", hostname: "localhost",
    tauriPresent: true, expect: false },
];

let failures = 0;
for (const c of cases) {
  const got = activates(c);
  const ok = got === c.expect;
  if (!ok) failures++;
  console.log(`  ${ok ? "ok  " : "FAIL"} ${c.name.padEnd(30)} activates=${got} expected=${c.expect}`);
}

console.log(`\n${cases.length} cases, ${failures} failure(s)`);
assert.equal(failures, 0, "dev mock activated where it must not");
