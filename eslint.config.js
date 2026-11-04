import js from "@eslint/js";
import { defineConfig } from "eslint/config";
import globals from "globals";
import tseslint from "typescript-eslint";

export default defineConfig([
  { ignores: ["dist/**", "node_modules/**", "src-tauri/target/**", "src-tauri/gen/**"] },
  {
    files: ["**/*.{ts,js,mjs}"],
    extends: [js.configs.recommended, tseslint.configs.recommended],
    rules: {
      // Unused values are almost always a mistake; an underscore prefix opts out.
      "@typescript-eslint/no-unused-vars": ["error", { argsIgnorePattern: "^_", varsIgnorePattern: "^_" }],
      eqeqeq: ["error", "always"],
      "no-var": "error",
      "prefer-const": "error",
    },
  },
  { files: ["src/**/*.ts"], languageOptions: { globals: globals.browser } },
  { files: ["scripts/**/*.mjs", "*.config.{js,ts}"], languageOptions: { globals: globals.node } },
]);
