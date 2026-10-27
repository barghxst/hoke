import { execFileSync } from "node:child_process";
import { mkdtempSync, renameSync, rmSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const version = "0.2.129";
let installed;
try {
  installed = execFileSync("wasm-bindgen", ["--version"], { encoding: "utf8" }).trim();
} catch {
  throw new Error(
    `Install the WASM toolchain first: rustup target add wasm32-unknown-unknown; cargo install wasm-bindgen-cli --version ${version} --locked`,
  );
}
if (installed !== `wasm-bindgen ${version}`) {
  throw new Error(`Expected wasm-bindgen ${version}, found ${installed}`);
}
execFileSync(
  "cargo",
  [
    "build",
    "-p",
    "hoke-wasm",
    "--lib",
    "--target",
    "wasm32-unknown-unknown",
    "--release",
    "--locked",
  ],
  { cwd: root, stdio: "inherit" },
);
const metadata = JSON.parse(
  execFileSync("cargo", ["metadata", "--format-version", "1", "--no-deps", "--locked"], {
    cwd: root,
    encoding: "utf8",
  }),
);
const packageRoot = join(root, "packages/hoke");
const output = join(packageRoot, "wasm");
const temporary = mkdtempSync(join(packageRoot, ".wasm-build-"));
try {
  execFileSync(
    "wasm-bindgen",
    [
      "--target",
      "web",
      "--out-name",
      "hoke_wasm",
      "--out-dir",
      temporary,
      join(metadata.target_directory, "wasm32-unknown-unknown/release/hoke_wasm.wasm"),
    ],
    { cwd: root, stdio: "inherit" },
  );
  rmSync(output, { recursive: true, force: true });
  renameSync(temporary, output);
} finally {
  rmSync(temporary, { recursive: true, force: true });
}
