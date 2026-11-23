import reylo from "reylo/rolldown";
export default { input: "main.ts", external: ["reylo", "postgres"], plugins: [reylo()], output: { dir: "dist/rolldown", format: "es", entryFileNames: "main.js" } };
