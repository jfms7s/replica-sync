// app/src/screens/Trash.test.tsx
import { act, render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { I18nProvider } from '../i18n';

vi.mock('../api', () => ({
  api: { trashRuns: vi.fn(), trashContents: vi.fn(), restore: vi.fn(), emptyRun: vi.fn(), openTrashRun: vi.fn() },
}));
import { api } from '../api';
import Trash from './Trash';

const runId = '2026-10-07_143200';

describe('Trash', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    vi.mocked(api.trashRuns).mockResolvedValue([{ id: runId, files: 2, bytes: 2048, unrecorded: 1 }]);
    vi.mocked(api.trashContents).mockResolvedValue({
      items: [{ path: 'a.txt', size: 1024, mtime_ns: 0, reason: 'deleted' }],
      unrecorded: ['lost.bin'],
      bytes: 2048,
    });
  });

  it('restores selected files and asks before replacing', async () => {
    vi.mocked(api.restore)
      .mockRejectedValueOnce({ code: 'trash.conflict', params: { path: 'a.txt' } })
      .mockResolvedValueOnce(1);
    render(<I18nProvider lang="en"><Trash pairId="p1" pairName="Photos" navigate={vi.fn()} /></I18nProvider>);
    await userEvent.click(await screen.findByRole('button', { name: /2 files/ }));
    expect(await screen.findByText(/Unrecorded/)).toBeInTheDocument();
    await userEvent.click(screen.getByRole('checkbox', { name: 'a.txt' }));
    await userEvent.click(screen.getByRole('button', { name: 'Restore selected' }));
    expect(api.restore).toHaveBeenLastCalledWith('p1', undefined, runId, ['a.txt'], false);
    expect(await screen.findByRole('dialog', { name: 'Something is in the way' })).toBeInTheDocument();
    await userEvent.click(screen.getByRole('button', { name: 'Replace' }));
    expect(api.restore).toHaveBeenLastCalledWith('p1', undefined, runId, ['a.txt'], true);
    expect(await screen.findByText('Restored: 1')).toBeInTheDocument();
  });

  it('empties a run only after confirming', async () => {
    vi.mocked(api.emptyRun).mockResolvedValue(undefined);
    render(<I18nProvider lang="en"><Trash pairId="p1" pairName="Photos" navigate={vi.fn()} /></I18nProvider>);
    await userEvent.click(await screen.findByRole('button', { name: 'Empty' }));
    expect(api.emptyRun).not.toHaveBeenCalled();
    await userEvent.click(screen.getAllByRole('button', { name: 'Empty' }).at(-1)!);
    expect(api.emptyRun).toHaveBeenCalledWith('p1', undefined, runId);
  });

  it('goes back to the pairs list by default', async () => {
    const navigate = vi.fn();
    render(<I18nProvider lang="en"><Trash pairId="p1" pairName="Photos" navigate={navigate} /></I18nProvider>);
    await userEvent.click(screen.getByRole('button', { name: 'Back' }));
    expect(navigate).toHaveBeenCalledWith({ name: 'pairs' });
  });

  it('reads, restores and empties the trash of the disk it was opened for, and names it', async () => {
    vi.mocked(api.restore).mockResolvedValue(1);
    vi.mocked(api.emptyRun).mockResolvedValue(undefined as never);
    render(<I18nProvider lang="en"><Trash pairId="p1" pairName="Photos" replica={1} diskName="Disk B (RELATIVE)" navigate={vi.fn()} /></I18nProvider>);
    expect(await screen.findByRole('heading', { level: 1 })).toHaveTextContent('Disk B (RELATIVE)');
    expect(api.trashRuns).toHaveBeenCalledWith('p1', 1);
    await userEvent.click(await screen.findByRole('button', { name: /2 files/ }));
    expect(api.trashContents).toHaveBeenCalledWith('p1', 1, runId);
    await userEvent.click(screen.getByRole('checkbox', { name: 'a.txt' }));
    await userEvent.click(screen.getByRole('button', { name: 'Restore selected' }));
    expect(api.restore).toHaveBeenCalledWith('p1', 1, runId, ['a.txt'], false);
  });

  it('goes back to the screen it was opened from', async () => {
    const navigate = vi.fn();
    const back = { name: 'settings' } as const;
    render(<I18nProvider lang="en"><Trash pairId="p1" pairName="Photos" back={back} navigate={navigate} /></I18nProvider>);
    await userEvent.click(screen.getByRole('button', { name: 'Back' }));
    expect(navigate).toHaveBeenCalledWith(back);
  });

  it('Restore all on a run with nothing recorded never calls restore', async () => {
    vi.mocked(api.trashContents).mockResolvedValue({ items: [], unrecorded: ['lost.bin'], bytes: 10 });
    render(<I18nProvider lang="en"><Trash pairId="p1" pairName="Photos" navigate={vi.fn()} /></I18nProvider>);
    await userEvent.click(await screen.findByRole('button', { name: 'Restore all' }));
    expect(await screen.findByText(/nothing to restore/)).toBeInTheDocument();
    expect(api.restore).not.toHaveBeenCalled();
  });

  it('Restore all sends only the recorded files', async () => {
    vi.mocked(api.restore).mockResolvedValue(1);
    render(<I18nProvider lang="en"><Trash pairId="p1" pairName="Photos" navigate={vi.fn()} /></I18nProvider>);
    await userEvent.click(await screen.findByRole('button', { name: 'Restore all' }));
    expect(api.restore).toHaveBeenCalledWith('p1', undefined, runId, ['a.txt'], false);
  });

  it('cancelling the replace dialog restores nothing more', async () => {
    vi.mocked(api.restore).mockRejectedValueOnce({ code: 'trash.conflict', params: { path: 'a.txt' } });
    render(<I18nProvider lang="en"><Trash pairId="p1" pairName="Photos" navigate={vi.fn()} /></I18nProvider>);
    await userEvent.click(await screen.findByRole('button', { name: 'Restore all' }));
    const dialog = await screen.findByRole('dialog', { name: 'Something is in the way' });
    await userEvent.click(within(dialog).getByRole('button', { name: 'Cancel' }));
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
    expect(api.restore).toHaveBeenCalledTimes(1);
  });

  it('cancelling the empty dialog empties nothing', async () => {
    render(<I18nProvider lang="en"><Trash pairId="p1" pairName="Photos" navigate={vi.fn()} /></I18nProvider>);
    await userEvent.click(await screen.findByRole('button', { name: 'Empty' }));
    await userEvent.click(within(screen.getByRole('dialog')).getByRole('button', { name: 'Cancel' }));
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
    expect(api.emptyRun).not.toHaveBeenCalled();
  });

  it('disables the run buttons while a restore is running', async () => {
    let finish: (n: number) => void = () => {};
    vi.mocked(api.restore).mockImplementation(() => new Promise((res) => { finish = res; }));
    render(<I18nProvider lang="en"><Trash pairId="p1" pairName="Photos" navigate={vi.fn()} /></I18nProvider>);
    await userEvent.click(await screen.findByRole('button', { name: 'Restore all' }));
    await vi.waitFor(() => expect(api.restore).toHaveBeenCalled());
    expect(screen.getByRole('button', { name: 'Restore all' })).toBeDisabled();
    expect(screen.getByRole('button', { name: 'Empty' })).toBeDisabled();
    await act(async () => finish(1));
    await vi.waitFor(() => expect(screen.getByRole('button', { name: 'Restore all' })).toBeEnabled());
  });

  it('offers to show the run folder only when it holds unrecorded files, by ids', async () => {
    vi.mocked(api.openTrashRun).mockResolvedValue(undefined);
    render(<I18nProvider lang="en"><Trash pairId="p1" pairName="Photos" navigate={vi.fn()} /></I18nProvider>);
    await userEvent.click(await screen.findByRole('button', { name: /2 files/ }));
    await userEvent.click(await screen.findByRole('button', { name: 'Show in the file manager' }));
    expect(api.openTrashRun).toHaveBeenCalledWith('p1', undefined, runId);
  });

  it('shows no folder button for a run with every file recorded', async () => {
    vi.mocked(api.trashContents).mockResolvedValue({ items: [{ path: 'a.txt', size: 1, mtime_ns: 0, reason: 'deleted' }], unrecorded: [], bytes: 1 });
    render(<I18nProvider lang="en"><Trash pairId="p1" pairName="Photos" navigate={vi.fn()} /></I18nProvider>);
    await userEvent.click(await screen.findByRole('button', { name: /2 files/ }));
    await screen.findByText(/a\.txt/);
    expect(screen.queryByRole('button', { name: 'Show in the file manager' })).not.toBeInTheDocument();
  });

  it('shows an error when the folder cannot be opened', async () => {
    vi.mocked(api.openTrashRun).mockRejectedValue({ code: 'io', params: { detail: 'no opener' } });
    render(<I18nProvider lang="en"><Trash pairId="p1" pairName="Photos" navigate={vi.fn()} /></I18nProvider>);
    await userEvent.click(await screen.findByRole('button', { name: /2 files/ }));
    await userEvent.click(await screen.findByRole('button', { name: 'Show in the file manager' }));
    expect(await screen.findByText(/no opener/)).toBeInTheDocument();
  });
});
