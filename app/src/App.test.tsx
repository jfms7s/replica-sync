// app/src/App.test.tsx
import { render, screen } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';

vi.mock('./api', () => ({
  api: {
    startupStatus: vi.fn().mockRejectedValue({ code: 'store.unreadable', params: { path: '/data/pairs.json', detail: 'bad json' } }),
    getSettings: vi.fn().mockResolvedValue({ settings: { language: 'auto', defaultTrashDays: 30 }, resolvedLanguage: 'en' }),
    listPairs: vi.fn().mockResolvedValue([]),
    openDataFolder: vi.fn(),
  },
  onEvent: vi.fn().mockResolvedValue(() => {}),
}));
import { api } from './api';
import App from './App';

describe('App', () => {
  it('shows_the_blocking_screen_when_the_store_is_unreadable', async () => {
    render(<App />);
    expect(await screen.findByText("replica-sync can't read its saved pairs")).toBeInTheDocument();
    expect(screen.getByText(/\/data\/pairs\.json is damaged/)).toBeInTheDocument();
    expect(screen.queryByText('Sync pairs')).not.toBeInTheDocument();
  });

  it('shows_a_generic_message_when_startup_fails_with_something_else', async () => {
    vi.mocked(api.startupStatus).mockRejectedValueOnce('ipc broke');
    render(<App />);
    expect(await screen.findByText("replica-sync can't read its saved pairs")).toBeInTheDocument();
    expect(screen.getByText('Something went wrong: ipc broke')).toBeInTheDocument();
  });

  it('a_null_startup_rejection_still_blocks', async () => {
    vi.mocked(api.startupStatus).mockRejectedValueOnce(null);
    render(<App />);
    expect(await screen.findByText("replica-sync can't read its saved pairs")).toBeInTheDocument();
    expect(screen.queryByText('Sync pairs')).not.toBeInTheDocument();
  });
});
