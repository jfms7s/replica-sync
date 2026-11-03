import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';
import { I18nProvider } from '../i18n';

vi.mock('../api', () => ({ api: { openDataFolder: vi.fn() } }));
import { api } from '../api';
import Broken from './Broken';

describe('Broken', () => {
  it('shows an error when the data folder cannot be opened', async () => {
    vi.mocked(api.openDataFolder).mockRejectedValue({ code: 'io', params: { detail: 'no opener' } });
    render(<I18nProvider lang="en"><Broken error={{ code: 'store.unreadable', params: { path: '/d', detail: 'bad' } }} /></I18nProvider>);
    await userEvent.click(screen.getByRole('button', { name: 'Open folder' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('no opener');
  });
});
