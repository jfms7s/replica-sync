import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { I18nProvider } from '../i18n';
import type { PreviewSummary } from '../api';

vi.mock('../api', () => ({
  api: { previewSummary: vi.fn(), treeChildren: vi.fn().mockResolvedValue([]), toggle: vi.fn(), selectAll: vi.fn(), confirmWrongFolder: vi.fn(), savePreview: vi.fn() },
  pickSaveFile: vi.fn(),
}));
import { api } from '../api';
import Preview from './Preview';

const totals = { creates: 2, updates: 0, moves: 0, deletes: 3, folders: 0, skipped: 1, bytes_to_copy: 2048 };
const base: PreviewSummary = {
  pairId: 'p1', pairName: 'Photos', totals, selected: totals, selectedCount: 5, actionableCount: 5,
  isEmpty: false, guard: null, guardConfirmed: false, shortfall: null,
  source: { files: 10, bytes: 100, elapsed_ms: 1500 }, replica: { files: 8, bytes: 80, elapsed_ms: 1200 },
  deleteFolders: ['old'],
};

describe('Preview', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    vi.mocked(api.previewSummary).mockRejectedValue({ code: 'apply.noPlan', params: {} });
  });

  it('blocks on the wrong-folder dialog until confirmed', async () => {
    vi.mocked(api.confirmWrongFolder).mockResolvedValue({ ...base, guard: { affected: 3, replica_files: 4 }, guardConfirmed: true });
    render(<I18nProvider lang="en"><Preview summary={{ ...base, guard: { affected: 3, replica_files: 4 } }} navigate={vi.fn()} /></I18nProvider>);
    expect(screen.getByRole('dialog', { name: 'Is this the right backup folder?' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Apply selected' })).toBeDisabled();
    await userEvent.click(screen.getByRole('button', { name: 'Yes, this is the right folder' }));
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Apply selected' })).toBeEnabled();
  });

  it('disables Apply and shows the shortfall when space is short', () => {
    render(<I18nProvider lang="en"><Preview summary={{ ...base, shortfall: 1048576 }} navigate={vi.fn()} /></I18nProvider>);
    expect(screen.getByText(/1.0 MB more needed/)).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Apply selected' })).toBeDisabled();
  });

  it('says Already in sync for an empty plan', () => {
    render(<I18nProvider lang="en"><Preview summary={{ ...base, isEmpty: true, totals: { ...totals, creates: 0, deletes: 0 } }} navigate={vi.fn()} /></I18nProvider>);
    expect(screen.getByText('Already in sync. Nothing to do.')).toBeInTheDocument();
  });

  it('applies the selection', async () => {
    const navigate = vi.fn();
    render(<I18nProvider lang="en"><Preview summary={base} navigate={navigate} /></I18nProvider>);
    expect(screen.getByText(/3 files will be deleted/)).toBeInTheDocument();
    await userEvent.click(screen.getByRole('button', { name: 'Apply selected' }));
    expect(navigate).toHaveBeenCalledWith({ name: 'applying', mode: 'apply', pairName: 'Photos' });
  });

  it('approve_all_refreshes_the_tree', async () => {
    vi.mocked(api.selectAll).mockResolvedValue(base);
    render(<I18nProvider lang="en"><Preview summary={base} navigate={vi.fn()} /></I18nProvider>);
    await vi.waitFor(() => expect(api.treeChildren).toHaveBeenCalledTimes(1));
    await userEvent.click(screen.getByRole('button', { name: 'Approve all' }));
    expect(api.selectAll).toHaveBeenCalledWith(true);
    await vi.waitFor(() => expect(api.treeChildren).toHaveBeenCalledTimes(2));
    expect(api.treeChildren).toHaveBeenLastCalledWith('');
  });

  it('refreshes the summary on mount so space freed in the trash shows', async () => {
    vi.mocked(api.previewSummary).mockResolvedValue({ ...base, shortfall: null });
    render(<I18nProvider lang="en"><Preview summary={{ ...base, shortfall: 1048576 }} navigate={vi.fn()} /></I18nProvider>);
    expect(api.previewSummary).toHaveBeenCalledTimes(1);
    await vi.waitFor(() => expect(screen.queryByText(/more needed/)).not.toBeInTheDocument());
    expect(screen.getByRole('button', { name: 'Apply selected' })).toBeEnabled();
  });

  it('keeps the given summary when the refresh fails', async () => {
    render(<I18nProvider lang="en"><Preview summary={{ ...base, shortfall: 1048576 }} navigate={vi.fn()} /></I18nProvider>);
    await vi.waitFor(() => expect(api.previewSummary).toHaveBeenCalled());
    expect(screen.getByText(/1.0 MB more needed/)).toBeInTheDocument();
    expect(screen.queryByRole('alert')).not.toBeInTheDocument();
  });

  it('open trash comes back to this preview', async () => {
    const navigate = vi.fn();
    const short = { ...base, shortfall: 1048576 };
    render(<I18nProvider lang="en"><Preview summary={short} navigate={navigate} /></I18nProvider>);
    await userEvent.click(screen.getByRole('button', { name: 'Open trash' }));
    expect(navigate).toHaveBeenCalledWith({
      name: 'trash', pairId: 'p1', pairName: 'Photos', back: { name: 'preview', summary: short },
    });
  });
});
