import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';
import { I18nProvider } from '../i18n';
import type { RunView } from '../api';

vi.mock('../api', () => ({ api: { emptyRun: vi.fn().mockResolvedValue(undefined), openPath: vi.fn() } }));
import { api } from '../api';
import Result from './Result';

const run: RunView = {
  pairId: 'p1', trashDays: 30, logPath: '/logs/x.log', warnings: [], notDone: 0,
  oldTrashRuns: ['2026-08-01_090000'],
  report: {
    trash_run: null,
    stopped: null,
    results: [
      { id: 0, path: 'a.txt', outcome: { kind: 'applied' } },
      { id: 1, path: 'b.txt', outcome: { kind: 'failed', reason: { code: 'inUse' } } },
      { id: 2, path: 'c', outcome: { kind: 'skipped', reason: { code: 'folderNotEmpty' } } },
    ],
  },
};

describe('Result', () => {
  it('summarises, lists failures in the user language and offers retry', async () => {
    const navigate = vi.fn();
    render(<I18nProvider lang="pt-PT"><Result run={run} pairName="Photos" navigate={navigate} /></I18nProvider>);
    expect(screen.getByText('1 feitas · 1 ignoradas · 1 falharam')).toBeInTheDocument();
    expect(screen.getByText(/está a ser usado por outro programa/)).toBeInTheDocument();
    await userEvent.click(screen.getByRole('button', { name: 'Tentar de novo as que falharam' }));
    expect(navigate).toHaveBeenCalledWith({ name: 'applying', mode: 'retry', pairName: 'Photos' });
  });

  it('a stopped run says what was not done and offers Sync again instead of Retry failed', async () => {
    const navigate = vi.fn();
    const stopped: RunView = { ...run, notDone: 4, report: { ...run.report, stopped: { code: 'replicaDisconnected' } } };
    render(<I18nProvider lang="pt-PT"><Result run={stopped} pairName="Photos" navigate={navigate} /></I18nProvider>);
    expect(screen.getByText('Parou: o disco da cópia foi desligado')).toBeInTheDocument();
    expect(screen.getByText('4 alterações não foram feitas. Sincronize de novo para terminar a cópia.')).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Tentar de novo as que falharam' })).not.toBeInTheDocument();
    await userEvent.click(screen.getByRole('button', { name: 'Sincronizar de novo' }));
    expect(navigate).toHaveBeenCalledWith({ name: 'scanning', pairId: 'p1', pairName: 'Photos' });
  });

  it('empties old trash runs only after confirming', async () => {
    render(<I18nProvider lang="en"><Result run={run} pairName="Photos" navigate={vi.fn()} /></I18nProvider>);
    await userEvent.click(screen.getByRole('button', { name: 'Empty them' }));
    expect(screen.getByText("Permanently delete the old trash runs (1)? This can't be undone.")).toBeInTheDocument();
    expect(api.emptyRun).not.toHaveBeenCalled();
    await userEvent.click(screen.getAllByRole('button', { name: 'Empty them' }).at(-1)!); // the modal's button
    expect(api.emptyRun).toHaveBeenCalledWith('p1', '2026-08-01_090000');
  });

  it('a_partial_empty_failure_keeps_only_the_runs_not_yet_emptied', async () => {
    const two = { ...run, oldTrashRuns: ['2026-08-01_090000', '2026-08-02_090000'] };
    vi.mocked(api.emptyRun).mockResolvedValueOnce(undefined).mockRejectedValueOnce({ code: 'trash.unsafe', params: {} });
    render(<I18nProvider lang="en"><Result run={two} pairName="Photos" navigate={vi.fn()} /></I18nProvider>);
    await userEvent.click(screen.getByRole('button', { name: 'Empty them' }));
    await userEvent.click(screen.getAllByRole('button', { name: 'Empty them' }).at(-1)!);
    expect(await screen.findByRole('alert')).toBeInTheDocument();
    expect(screen.getByText('Trash runs older than 30 days: 1.')).toBeInTheDocument();
  });
});
