import js from '@eslint/js'
import globals from 'globals'
import reactHooks from 'eslint-plugin-react-hooks'
import reactRefresh from 'eslint-plugin-react-refresh'
import tseslint from 'typescript-eslint'
import { defineConfig, globalIgnores } from 'eslint/config'

export default defineConfig([
  globalIgnores(['dist']),
  {
    files: ['**/*.{ts,tsx}'],
    extends: [
      js.configs.recommended,
      tseslint.configs.recommended,
      reactRefresh.configs.vite,
    ],
    plugins: {
      'react-hooks': reactHooks,
    },
    languageOptions: {
      ecmaVersion: 2020,
      globals: globals.browser,
    },
    rules: {
      'no-restricted-imports': ['error', {
        patterns: [{
          group: ['@radix-ui/*', '@langchain/*', 'class-variance-authority', 'class-variance-authority/*', 'date-fns', 'date-fns/*', 'marked', 'marked/*', 'react-grid-layout', 'react-grid-layout/*', 'react-resizable', 'react-resizable/*', 'framer-motion', 'framer-motion/*'],
          message: 'Use the antd 6 design system and dayjs; retired console dependencies must not return.',
        }],
      }],
      'no-restricted-syntax': ['error', {
        selector: 'ImportExpression[source.value=/^(@radix-ui|@langchain|class-variance-authority|date-fns|marked|react-grid-layout|react-resizable|framer-motion)/]',
        message: 'Load console UI from antd and dates from dayjs; retired dependencies must not return through dynamic imports.',
      }],
      'react-hooks/rules-of-hooks': 'error',
      'react-hooks/exhaustive-deps': 'warn',
    },
  },
])
