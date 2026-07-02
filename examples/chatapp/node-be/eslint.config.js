import eslint from "@eslint/js";
import tseslint from "typescript-eslint";

export default tseslint.config(
  { ignores: ["src/generated/**", "dist/**", "node_modules/**"] },
  eslint.configs.recommended,
  tseslint.configs.recommended,
  {
    rules: {
      // `_`-prefixed bindings are intentional discards (e.g. the `_c` rest-omit in context.ts).
      "@typescript-eslint/no-unused-vars": [
        "error",
        { argsIgnorePattern: "^_", varsIgnorePattern: "^_", ignoreRestSiblings: true },
      ],
    },
  },
);
