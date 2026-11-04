import { mount } from 'svelte';
import { mockIPC, mockWindows } from '@tauri-apps/api/mocks';
import { emit } from '@tauri-apps/api/event';
import WindowChrome from '../../src/lib/WindowChrome.svelte';
import '../../src/app.css';

const state = {
  commands: [] as Array<{ command: string; args: unknown }>,
  maximized: false,
  failCommand: '',
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
  if (command === 'plugin:window|is_maximized') return state.maximized;
  if (command === 'plugin:window|toggle_maximize') state.maximized = !state.maximized;
}, { shouldMockEvents: true });

mount(WindowChrome, {
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
