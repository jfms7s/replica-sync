import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { I18nProvider } from '../i18n';
import type { PreviewSummary } from '../api';

vi.mock('../api', () => ({
  api: { treeChildren: vi.fn().mockResolvedValue([]), toggle: vi.fn(), selectAll: vi.fn(), confirmWrongFolder: vi.fn(), savePreview: vi.fn() },
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
  beforeEach(() => vi.clearAllMocks());

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
});
