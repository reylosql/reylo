import { defineConfig } from "rolldown";
import reylo from "reylo/rolldown";

export default defineConfig({
  input: "src/server.ts",
  platform: "node",
  plugins: [reylo()], // turns each .sql import into a module that runs the query
  output: { file: "dist/server.mjs", format: "esm", codeSplitting: false, sourcemap: true },
});
