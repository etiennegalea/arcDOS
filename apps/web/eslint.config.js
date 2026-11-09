import js from '@eslint/js';
import { defineConfig, globalIgnores } from 'eslint/config';
import svelte from 'eslint-plugin-svelte';
import globals from 'globals';
import ts from 'typescript-eslint';

export default defineConfig(
  globalIgnores(['.svelte-kit', 'build', 'src/lib/gen', 'playwright-report', 'test-results']),
  js.configs.recommended,
  ts.configs.recommended,
  svelte.configs.recommended,
  {
    files: ['src/**/*.{svelte,ts}'],
    languageOptions: { globals: globals.browser }
  },
  {
    files: ['*.config.{js,ts}', 'tests/**/*.ts'],
    languageOptions: { globals: globals.node }
  },
  {
    files: ['**/*.svelte', '**/*.svelte.ts'],
    languageOptions: {
      parserOptions: { parser: ts.parser }
    }
  }
);
