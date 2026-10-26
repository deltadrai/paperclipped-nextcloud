// Builds dist/manifest.js and dist/worker.js for Node (dependencies stay external and are
// installed with the package) and dist/ui/index.js for the browser (React and the SDK's UI
// runtime are provided by the host).
import esbuild from "esbuild";

const common = { bundle: true, format: "esm", sourcemap: true, logLevel: "info" };
await esbuild.build({
  ...common,
  entryPoints: { manifest: "src/manifest.ts", worker: "src/worker.ts", index: "src/index.ts" },
  outdir: "dist",
  platform: "node",
  target: "node24",
  packages: "external",
});
await esbuild.build({
  ...common,
  entryPoints: ["src/ui/index.tsx"],
  outfile: "dist/ui/index.js",
  platform: "browser",
  target: ["es2022"],
  jsx: "automatic",
  external: ["react", "react-dom", "react/jsx-runtime", "@paperclipai/plugin-sdk/ui"],
});
