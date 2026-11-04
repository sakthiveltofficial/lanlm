import { defineConfig } from "vitest/config";

// Tauri expects a fixed dev port and does not want Vite to clear the terminal.
export default defineConfig({
  clearScreen: false,
  server: { port: 1420, strictPort: true, host: "127.0.0.1" },
  build: { target: "es2021", outDir: "dist", emptyOutDir: true },
  test: {
    include: ["src/**/*.test.ts"],
    environment: "node",
    coverage: { provider: "v8", include: ["src/lib/**"], reporter: ["text", "lcov"] },
  },
});
