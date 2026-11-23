import { nodeResolve } from "@rollup/plugin-node-resolve";
import sucrase from "@rollup/plugin-sucrase";
import reylo from "reylo/rollup";

export default {
  input: "src/server.ts",
  plugins: [
    reylo(), // turns each .sql import into a module that runs the query
    nodeResolve({ extensions: [".ts", ".js"] }),
    sucrase({ transforms: ["typescript"], exclude: ["node_modules/**"] }),
  ],
  output: { file: "dist/server.mjs", format: "esm", inlineDynamicImports: true, sourcemap: true },
};
