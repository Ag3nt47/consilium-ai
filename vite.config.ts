import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { readFileSync } from "node:fs";

// Tauri serves the built files itself; the dev server is only used by `tauri dev`.
export default defineConfig({
  plugins: [
    react(),
    {
      name: "consilium-typescript-notices",
      apply: "build",
      generateBundle() {
        for (const file of ["LICENSE.txt", "ThirdPartyNoticeText.txt"]) {
          this.emitFile({
            type: "asset",
            fileName: `licenses/TypeScript-${file}`,
            source: readFileSync(new URL(`./node_modules/typescript-ast/${file}`, import.meta.url)),
          });
        }
      },
    },
  ],
  clearScreen: false,
  server: { port: 1420, strictPort: true, host: "127.0.0.1" },
  envPrefix: ["VITE_", "TAURI_ENV_"],
  build: {
    target: "es2021",
    outDir: "dist",
    sourcemap: false,
  },
});
