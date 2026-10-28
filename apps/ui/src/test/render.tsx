import { render } from '@testing-library/react';
import type { ReactElement } from 'react';

import { I18nProvider } from '../i18n/context';
import { SpeechProvider, type SpeechServices } from '../speech/context';
import { defaultSettings } from '../state/settings';
import { AppProvider } from '../state/store';
import { FakeTransport } from './fakeTransport';

export function renderWithApp(
  ui: ReactElement,
  {
    transport = new FakeTransport(),
    speech,
  }: { transport?: FakeTransport; speech?: Partial<SpeechServices> } = {},
) {
  const services: SpeechServices = {
    transcribe: async () => ({ text: '', corrections: [] }),
    createRecorder: () => ({
      start: async () => {},
      stop: async () => new Blob(),
      cancel: () => {},
    }),
    fallbackWithoutMic: false,
    ...speech,
  };
  const result = render(
    <AppProvider
      machines={[{ id: 'm', name: 'test machine', connectivity: 'LAN', transport }]}
      settings={defaultSettings()}
      persistSettings={false}
    >
      <I18nProvider locale="en">
        <SpeechProvider value={services}>{ui}</SpeechProvider>
      </I18nProvider>
    </AppProvider>,
  );
  return { ...result, transport };
}
