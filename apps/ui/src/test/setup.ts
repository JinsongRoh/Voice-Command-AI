import '@testing-library/jest-dom/vitest';

import { cleanup } from '@testing-library/react';
import { afterEach } from 'vitest';

import { clearAllDrafts } from '../components/composerDrafts';

afterEach(() => {
  cleanup();
  // Composer drafts live in a module-level map; never carry one into the next test.
  clearAllDrafts();
});
