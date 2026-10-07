// app/src/screens/PairEditor.test.tsx
import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { I18nProvider } from '../i18n';

vi.mock('../api', () => ({
  api: { savePair: vi.fn(), deletePair: vi.fn(), builtinRules: vi.fn(), getSettings: vi.fn() },
  pickFolder: vi.fn(),
}));
import { api, pickFolder } from '../api';
import PairEditor from './PairEditor';

describe('PairEditor', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    vi.mocked(api.builtinRules).mockResolvedValue(['Thumbs.db']);
    vi.mocked(api.getSettings).mockResolvedValue({ settings: { language: 'auto', defaultTrashDays: 21 }, resolvedLanguage: 'en' });
  });

  it('shows a translated inline error and the same-drive tick when the engine asks for it', async () => {
    vi.mocked(pickFolder).mockResolvedValueOnce('/a').mockResolvedValueOnce('/b');
    vi.mocked(api.savePair)
      .mockRejectedValueOnce({ code: 'pair.sameVolume', params: {} })
      .mockResolvedValueOnce({} as never);
    const navigate = vi.fn();
    render(<I18nProvider lang="en"><PairEditor pair={null} navigate={navigate} /></I18nProvider>);
    await userEvent.type(screen.getByLabelText('Name'), 'Photos');
    const browse = screen.getAllByRole('button', { name: 'Browse…' });
    await userEvent.click(browse[0]);
    await userEvent.click(browse[1]);
    expect(pickFolder).toHaveBeenNthCalledWith(1, 'Folder to back up (source)');
    expect(pickFolder).toHaveBeenNthCalledWith(2, 'Backup folder');
    expect(await screen.findByDisplayValue('21')).toBeInTheDocument(); // default trash age from settings
    await userEvent.click(screen.getByRole('button', { name: 'Save' }));
    expect(await screen.findByRole('alert')).toHaveTextContent(/same drive/);
    await userEvent.click(screen.getByLabelText(/I understand/));
    await userEvent.click(screen.getByRole('button', { name: 'Save' }));
    expect(api.savePair).toHaveBeenLastCalledWith(
      expect.objectContaining({ name: 'Photos', source: '/a', replica: '/b', allowSameVolume: true, trashDays: 21 }),
    );
    expect(navigate).toHaveBeenCalledWith({ name: 'pairs' });
  });

  it('never sends a trash age of 0 or an empty one', async () => {
    vi.mocked(pickFolder).mockResolvedValueOnce('/a').mockResolvedValueOnce('/b');
    render(<I18nProvider lang="en"><PairEditor pair={null} navigate={vi.fn()} /></I18nProvider>);
    await userEvent.type(screen.getByLabelText('Name'), 'Photos');
    const browse = screen.getAllByRole('button', { name: 'Browse…' });
    await userEvent.click(browse[0]);
    await userEvent.click(browse[1]);
    const days = await screen.findByDisplayValue('21');
    await userEvent.clear(days);
    expect(screen.getByRole('button', { name: 'Save' })).toBeDisabled();
    await userEvent.click(screen.getByRole('button', { name: 'Save' }));
    await userEvent.type(days, '0');
    expect(screen.getByRole('button', { name: 'Save' })).toBeDisabled();
    await userEvent.click(screen.getByRole('button', { name: 'Save' }));
    expect(api.savePair).not.toHaveBeenCalled();
    await userEvent.clear(days);
    await userEvent.type(days, '7');
    await userEvent.click(screen.getByRole('button', { name: 'Save' }));
    expect(api.savePair).toHaveBeenCalledWith(expect.objectContaining({ trashDays: 7 }));
  });
});
