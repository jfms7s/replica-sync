// app/src/screens/Settings.test.tsx
import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';
import { I18nProvider } from '../i18n';

vi.mock('../api', () => ({ api: { getSettings: vi.fn(), setSettings: vi.fn(), openLogsFolder: vi.fn() } }));
import { api } from '../api';
import Settings from './Settings';

describe('Settings', () => {
  it('saves the language and reports the resolved one', async () => {
    vi.mocked(api.getSettings).mockResolvedValue({ settings: { language: 'auto', defaultTrashDays: 30 }, resolvedLanguage: 'en' });
    vi.mocked(api.setSettings).mockResolvedValue({ settings: { language: 'pt-PT', defaultTrashDays: 30 }, resolvedLanguage: 'pt-PT' });
    const onLanguage = vi.fn();
    render(<I18nProvider lang="en"><Settings navigate={vi.fn()} onLanguage={onLanguage} /></I18nProvider>);
    await userEvent.selectOptions(await screen.findByLabelText('Language'), 'pt-PT');
    await userEvent.click(screen.getByRole('button', { name: 'Save' }));
    expect(api.setSettings).toHaveBeenCalledWith({ language: 'pt-PT', defaultTrashDays: 30 });
    expect(onLanguage).toHaveBeenCalledWith('pt-PT');
  });
});
