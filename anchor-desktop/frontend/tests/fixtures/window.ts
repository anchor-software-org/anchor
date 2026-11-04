import { mount } from 'svelte';
import { mockIPC, mockWindows } from '@tauri-apps/api/mocks';
import { emit } from '@tauri-apps/api/event';
import WindowChrome from '../../src/lib/WindowChrome.svelte';
import type { DesktopState } from '../../src/lib/anchor';
import '../../src/app.css';

const state = {
  commands: [] as Array<{ command: string; args: unknown }>,
  maximized: false,
  failCommand: '',
  showWindowControls: localStorage.getItem('show-window-controls') !== 'false',
  async resize(maximized: boolean) {
    state.maximized = maximized;
    await emit('tauri://resize', { width: 1180, height: 780 });
  },
};
export type MockWindow = typeof state;
Object.assign(window, { testWindow: state });
mockWindows('main');
mockIPC((command, args) => {
  state.commands.push({ command, args });
  if (command === state.failCommand) throw new Error('Window action denied');
  if (command === 'set_window_controls') {
    state.showWindowControls = (args as { enabled: boolean }).enabled;
    localStorage.setItem('show-window-controls', String(state.showWindowControls));
  }
  if (command === 'desktop_state') return {
    showWindowControls: state.showWindowControls,
    appName: 'Anchor', version: '1.0.0', build: 'test', compositor: 'test',
    environment: { screenSharing: false, pointerInput: false, keyboardInput: false, virtualDisplays: false },
    capture: { status: 'idle', outputs: [], selectedOutputIndex: 0 },
    virtualDisplays: [], devices: [], notifications: [], smsThreads: [], smsMessages: [], smsAvailable: false,
    clipboard: [], transfers: [], sharedFolder: '/tmp', commands: [], logs: [],
    camera: { status: 'unavailable', autoLoad: false, fps: 30, bitrateKbps: 2000, zoomRatio: 1, exposure: 0, torchOn: false },
  } satisfies DesktopState;
  if (command === 'plugin:window|is_maximized') return state.maximized;
  if (command === 'plugin:window|toggle_maximize') state.maximized = !state.maximized;
}, { shouldMockEvents: true });

if (new URLSearchParams(location.search).has('app')) {
  const { default: App } = await import('../../src/App.svelte');
  mount(App, { target: document.getElementById('app')! });
} else mount(WindowChrome, {
  target: document.getElementById('app')!,
  props: {
    title: 'Sideboat',
    onerror(error: unknown) {
      const message = document.createElement('p');
      message.setAttribute('role', 'alert');
      message.textContent = String(error);
      document.body.append(message);
    },
  },
});
