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

  it('forgets the same-drive answer when a folder changes', async () => {
    vi.mocked(pickFolder).mockResolvedValueOnce('/a').mockResolvedValueOnce('/b').mockResolvedValueOnce('/c');
    vi.mocked(api.savePair).mockRejectedValueOnce({ code: 'pair.sameVolume', params: {} }).mockResolvedValue({} as never);
    render(<I18nProvider lang="en"><PairEditor pair={null} navigate={vi.fn()} /></I18nProvider>);
    await userEvent.type(screen.getByLabelText('Name'), 'Photos');
    const browse = screen.getAllByRole('button', { name: 'Browse…' });
    await userEvent.click(browse[0]);
    await userEvent.click(browse[1]);
    await userEvent.click(screen.getByRole('button', { name: 'Save' }));
    await userEvent.click(await screen.findByLabelText(/same drive/));
    await userEvent.click(browse[1]);
    await userEvent.click(screen.getByRole('button', { name: 'Save' }));
    expect(api.savePair).toHaveBeenLastCalledWith(expect.objectContaining({ replica: '/c', allowSameVolume: false }));
  });

  it('shows a folder-picker failure', async () => {
    vi.mocked(pickFolder).mockRejectedValue({ code: 'io', params: { detail: 'no dialog' } });
    render(<I18nProvider lang="en"><PairEditor pair={null} navigate={vi.fn()} /></I18nProvider>);
    await userEvent.click(screen.getAllByRole('button', { name: 'Browse…' })[0]);
    expect(await screen.findByRole('alert')).toHaveTextContent('no dialog');
  });

  it('says when the default trash age could not be read', async () => {
    vi.mocked(api.getSettings).mockRejectedValue({ code: 'io', params: { detail: 'x' } });
    vi.spyOn(console, 'error').mockImplementation(() => {});
    render(<I18nProvider lang="en"><PairEditor pair={null} navigate={vi.fn()} /></I18nProvider>);
    expect(await screen.findByText(/default could not be read/)).toBeInTheDocument();
  });

  const existing = {
    pair: {
      id: 'p1', name: 'Photos',
      source: { volume_id: 'a', rel_path: 'Photos', label: 'Internal' },
      replica: { volume_id: 'b', rel_path: 'Backup', label: 'USB' },
      user_rules: ['*.tmp'], trash_days: 14, last_sync: null, last_scan_files: null,
    },
    sourceConnected: true, replicaConnected: true,
  };

  it('deletes a pair only after confirming', async () => {
    vi.mocked(api.deletePair).mockResolvedValue(undefined as never);
    const navigate = vi.fn();
    render(<I18nProvider lang="en"><PairEditor pair={existing} navigate={navigate} /></I18nProvider>);
    await userEvent.click(screen.getByRole('button', { name: 'Delete pair' }));
    expect(api.deletePair).not.toHaveBeenCalled();
    const dialog = screen.getByRole('dialog', { name: 'Delete pair' });
    expect(dialog).toHaveTextContent('Delete the pair Photos?');
    await userEvent.click(screen.getAllByRole('button', { name: 'Delete pair' }).at(-1)!);
    expect(api.deletePair).toHaveBeenCalledWith('p1');
    expect(navigate).toHaveBeenCalledWith({ name: 'pairs' });
  });

  it('editing without changing folders sends both as null', async () => {
    vi.mocked(api.savePair).mockResolvedValue({} as never);
    render(<I18nProvider lang="en"><PairEditor pair={existing} navigate={vi.fn()} /></I18nProvider>);
    const name = screen.getByLabelText('Name');
    await userEvent.clear(name);
    await userEvent.type(name, 'Pictures');
    await userEvent.click(screen.getByRole('button', { name: 'Save' }));
    expect(api.savePair).toHaveBeenCalledWith(expect.objectContaining({
      id: 'p1', name: 'Pictures', source: null, replica: null, userRules: ['*.tmp'], trashDays: 14,
    }));
  });
});
