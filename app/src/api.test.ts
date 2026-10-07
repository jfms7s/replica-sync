import { beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn() }));
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn(), save: vi.fn() }));
import { open } from '@tauri-apps/plugin-dialog';
import { pickFolder } from './api';

describe('pickFolder', () => {
  beforeEach(() => vi.clearAllMocks());

  it('passes the title to the folder dialog', async () => {
    vi.mocked(open).mockResolvedValue('/media/usb/Backup');
    expect(await pickFolder('Choose the backup folder on USB')).toBe('/media/usb/Backup');
    expect(open).toHaveBeenCalledWith({ directory: true, multiple: false, title: 'Choose the backup folder on USB' });
  });
});
