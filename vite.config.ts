import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Tauri serves the built files itself; the dev server is only used by `tauri dev`.
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: { port: 1420, strictPort: true, host: "127.0.0.1" },
  envPrefix: ["VITE_", "TAURI_ENV_"],
  build: {
    target: "es2021",
    outDir: "dist",
    sourcemap: false,
  },
});
