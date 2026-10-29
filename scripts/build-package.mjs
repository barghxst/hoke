import { execFileSync } from "node:child_process";
import { rmSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { build } from "esbuild";

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const directory = join(root, "packages/hoke");
rmSync(join(directory, "dist"), { recursive: true, force: true });
execFileSync("pnpm", ["exec", "tsc", "-p", "tsconfig.build.json"], {
  cwd: directory,
  stdio: "inherit",
});
await build({
  entryPoints: [join(directory, "src/kv/native/index.ts")],
  outfile: join(directory, "dist/kv/native/index.js"),
  bundle: true,
  format: "esm",
  platform: "neutral",
  target: "es2022",
  packages: "external",
  sourcemap: true,
  legalComments: "none",
  minifyWhitespace: true,
  plugins: [
    {
      name: "native-kv-backend",
      setup(builder) {
        builder.onResolve({ filter: /^\.\/web\/resolve-backend\.js$/ }, () => ({
          path: join(directory, "src/kv/native/resolve-backend.ts"),
        }));
      },
    },
  ],
});
