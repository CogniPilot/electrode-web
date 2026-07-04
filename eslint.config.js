import js from '@eslint/js';
import globals from 'globals';
import svelte from 'eslint-plugin-svelte';
import tseslint from 'typescript-eslint';

export default tseslint.config(
  {
    ignores: [
      'apps/web/.svelte-kit/**',
      'apps/web/build/**',
      'apps/web/static/wasm/**',
      'node_modules/**',
      'packages/electrode-flatbuffers/src/schema-assets.ts',
      'packages/electrode-sdk/src/generated/**',
      'target/**',
      'vendor/**'
    ]
  },
  js.configs.recommended,
  ...tseslint.configs.recommended,
  ...svelte.configs['flat/recommended'],
  {
    files: ['**/*.{js,mjs,ts}'],
    rules: {
      'max-lines': [
        'error',
        {
          max: 2000,
          skipBlankLines: true,
          skipComments: true
        }
      ],
      'max-lines-per-function': [
        'error',
        {
          max: 100,
          skipBlankLines: true,
          skipComments: true,
          IIFEs: true
        }
      ]
    }
  },
  {
    files: ['**/*.{js,mjs,ts,svelte}'],
    languageOptions: {
      ecmaVersion: 2022,
      globals: {
        ...globals.browser,
        ...globals.worker,
        ...globals.node
      },
      sourceType: 'module'
    },
    rules: {
      '@typescript-eslint/no-explicit-any': 'off',
      '@typescript-eslint/no-unused-vars': [
        'error',
        {
          argsIgnorePattern: '^_',
          caughtErrorsIgnorePattern: '^_',
          varsIgnorePattern: '^_'
        }
      ],
      'no-console': ['warn', { allow: ['log', 'warn', 'error'] }],
      'svelte/no-immutable-reactive-statements': 'off',
      'svelte/no-reactive-literals': 'off',
      'svelte/prefer-svelte-reactivity': 'off',
      'svelte/require-each-key': 'off'
    }
  },
  {
    files: ['**/*.svelte'],
    languageOptions: {
      parserOptions: {
        parser: tseslint.parser,
        extraFileExtensions: ['.svelte']
      }
    },
    rules: {
      '@typescript-eslint/no-unused-vars': 'off',
      'no-unused-vars': 'off'
    }
  }
);
