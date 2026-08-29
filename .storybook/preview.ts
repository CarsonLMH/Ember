import type { Preview } from '@storybook/react-vite';
import { sb } from 'storybook/test';
import '../src/App.css';
import './storybook.css';

sb.mock(import('../src/lib/ipc.ts'), { spy: true });

const preview = {
  parameters: {
    layout: 'fullscreen',
    a11y: {
      test: 'error',
      options: {
        runOnly: ['wcag2a', 'wcag2aa', 'wcag21a', 'wcag21aa', 'best-practice'],
      },
    },
  },
} satisfies Preview;

export default preview;
