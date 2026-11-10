import { screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { mockTask } from '../protocol/mockTasks';
import type { TaskSettings } from '../protocol/tasks';
import { NativeError } from '../native/bridge';
import { saveTextFile, type SaveTextResult } from '../native/saveFile';
import { useAppState } from '../state/store';
import { defaultResponse, FakeTransport } from '../test/fakeTransport';
import { localSession } from '../test/fixtures';
import { renderWithApp } from '../test/render';
import { TasksView } from './TasksView';

vi.mock('../native/saveFile', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../native/saveFile')>()),
  saveTextFile: vi.fn(),
}));

const SETTINGS: TaskSettings = {
  extract: true,
  extract_llm: false,
  stale_hours: 8,
  remind_blocking_minutes: 30,
  digest_hour: null,
};

function server() {
  return new FakeTransport((msg) => {
    switch (msg.type) {
      case 'hello':
        return {
          type: 'welcome',
          protocol_version: 1,
          server_version: 'test',
          server_name: 'test',
          features: ['human_tasks'],
        };
      case 'list_sessions':
        return { type: 'sessions', sessions: [localSession()] };
      case 'task_list':
        return {
          type: 'tasks',
          tasks: [mockTask({ id: 'a', title: 'Buy toner' })],
          settings: SETTINGS,
        };
      case 'task_export':
        return { type: 'task_exported', format: msg.format, text: 'title\nBuy toner\n' };
      default:
        return defaultResponse(msg);
    }
  });
}

/** The export toast as "title / body". */
function ToastProbe() {
  const toast = useAppState((s) => s.toasts.find((t) => t.id.startsWith('task-export:')));
  return (
    <output data-testid="export-toast">{toast ? `${toast.title} / ${toast.body}` : ''}</output>
  );
}

let writeText: ReturnType<typeof vi.fn>;
beforeEach(() => {
  writeText = vi.fn(async () => {});
  Object.defineProperty(navigator, 'clipboard', { configurable: true, value: { writeText } });
});
afterEach(() => {
  Reflect.deleteProperty(navigator, 'clipboard');
  vi.mocked(saveTextFile).mockReset();
});

async function exportCsv(result: SaveTextResult): Promise<string> {
  vi.mocked(saveTextFile).mockResolvedValue(result);
  renderWithApp(
    <>
      <TasksView />
      <ToastProbe />
    </>,
    { transport: server() },
  );
  await userEvent.click(await screen.findByRole('button', { name: 'Export CSV' }));
  const probe = screen.getByTestId('export-toast');
  await waitFor(() => expect(probe).not.toBeEmptyDOMElement());
  expect(writeText).toHaveBeenCalledWith('title\nBuy toner\n');
  expect(saveTextFile).toHaveBeenCalledWith('vca-tasks.csv', 'title\nBuy toner\n');
  return probe.textContent ?? '';
}

describe('task export toast', () => {
  it('says the text was copied and names the shared file', async () => {
    expect(await exportCsv({ status: 'shared', fileName: 'vca-tasks.csv' })).toBe(
      'Copied to clipboard / Shared vca-tasks.csv.',
    );
  });

  it('adds nothing when the share sheet was dismissed', async () => {
    expect(await exportCsv({ status: 'cancelled' })).toBe('Copied to clipboard / ');
  });

  it('names the saved file on desktop', async () => {
    expect(await exportCsv({ status: 'saved', fileName: 'tasks.csv' })).toBe(
      'Copied to clipboard / Saved to tasks.csv.',
    );
  });

  it('does not tell the user to copy what is already copied', async () => {
    const error = new NativeError('save_unsupported', 'sharing is not available here');
    expect(await exportCsv({ status: 'error', error })).toBe('Copied to clipboard / ');
  });

  it('shows a failed share as the second line', async () => {
    const error = new NativeError('share_failed', 'no window');
    expect(await exportCsv({ status: 'error', error })).toBe(
      'Copied to clipboard / Could not save the file: no window',
    );
  });

  it('says so when the clipboard refused the text', async () => {
    writeText.mockRejectedValue(new Error('denied'));
    // jsdom has no execCommand fallback that succeeds.
    document.execCommand = vi.fn(() => false);
    try {
      expect(await exportCsv({ status: 'shared', fileName: 'vca-tasks.csv' })).toBe(
        "Couldn't copy to the clipboard / Shared vca-tasks.csv.",
      );
    } finally {
      Reflect.deleteProperty(document, 'execCommand');
    }
  });
});
