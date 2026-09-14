import { invoke } from '@tauri-apps/api/core';

export type CaptureMetrics = { fps: number; avgFrameMs: number; frameSizeBytes: number };
export type ScreenPreview = { sequence: number; dataUrl: string };
export type Output = {
  index: number;
  name: string;
  description: string;
  width: number;
  height: number;
  refreshRateMhz: number;
};
export type CaptureState = {
  status: 'initializing' | 'idle' | 'starting' | 'streaming' | 'switching' | 'stopping' | 'error' | 'unavailable';
  unavailableReason?: string;
  outputs: Output[];
  selectedOutputIndex: number;
  metrics?: CaptureMetrics;
};
export type PairedDevice = { id: string; name: string; online: boolean; batteryLevel?: number | null; batteryCharging: boolean; fingerprint: string; cameraCapable: boolean; displayWidth?: number; displayHeight?: number };
export type Environment = { screenSharing: boolean | null; pointerInput: boolean | null; keyboardInput: boolean | null; virtualDisplays: boolean };
export type VirtualDisplay = { name: string; width: number; height: number };
export type PairingRequest = { deviceId: string; deviceName: string; deviceType: string; fingerprint: string; safetyNumber: string };
export type Notification = { source: string; appName: string; appPackage: string; iconKey: string; title: string; body: string; timestamp: number };
export type SmsThread = { threadId: number; name: string; snippet: string; lastUpdated: number; hasPhoto: boolean };
export type SmsMessage = { uid: number; body: string; date: number; sent: boolean };
export type ClipboardEntry = { hash: string; contentType: 'text' | 'image'; preview: string; timestamp: number; sizeBytes: number };
export type Transfer = { name: string; direction: 'sent' | 'received'; sizeBytes: number; timestamp: number; path: string; exists: boolean };
export type DroppedFileImport = { imported: number; rejected: number; names: string[]; errors: string[] };
export type SavedCommand = {
  id: string;
  name: string;
  command: string;
  description: string;
  detach: boolean;
  runStatus?: 'started' | 'done' | 'failed';
  runDetail?: string;
};
export type Media = {
  sessionId: string; state: string; title: string; artist: string; album: string; app: string;
  artUrl: string;
  positionMs: number; durationMs: number; canPlay: boolean; canPause: boolean; canNext: boolean; canPrev: boolean;
};
export type LogEntry = { timestamp: number; source: string; level: string; message: string };
export type Camera = {
  status: string;
  device?: string;
  selectedDeviceId?: string;
  autoLoad: boolean;
  fps: number;
  bitrateKbps: number;
  zoomRatio: number;
  exposure: number;
  torchOn: boolean;
};

export type DesktopState = {
  appName: string;
  version: string;
  build: string;
  compositor: string;
  environment: Environment;
  capture: CaptureState;
  virtualDisplays: VirtualDisplay[];
  devices: PairedDevice[];
  pairingRequest?: PairingRequest;
  notifications: Notification[];
  smsThreads: SmsThread[];
  selectedSmsThread?: number;
  smsMessages: SmsMessage[];
  smsAvailable: boolean;
  clipboard: ClipboardEntry[];
  transfers: Transfer[];
  sharedFolder: string;
  commands: SavedCommand[];
  phoneMedia?: Media;
  desktopMedia?: Media;
  logs: LogEntry[];
  camera: Camera;
};

export const getDesktopState = (section?: string) => invoke<DesktopState>('desktop_state', { section });
export const setStreaming = (enabled: boolean) => invoke<void>('set_streaming', { enabled });
export const selectOutput = (index: number) => invoke<void>('select_output', { index });
export const createVirtualDisplay = (width: number, height: number) => invoke<void>('create_virtual_display', { width, height });
export const destroyVirtualDisplay = (name: string) => invoke<void>('destroy_virtual_display', { name });
export const screenPreview = (afterSequence: number) => invoke<ScreenPreview | null>('screen_preview', { afterSequence });
export const setSideboatVisible = (visible: boolean) => invoke<void>('set_sideboat_visible', { visible });
export const unpairDevice = (deviceId: string) => invoke<void>('unpair_device', { deviceId });
export const respondToPairing = (accepted: boolean) => invoke<void>('respond_to_pairing', { accepted });
export const dismissNotification = (index: number) => invoke<void>('dismiss_notification', { index });
export const clearNotifications = () => invoke<void>('clear_notifications');
export const notificationIcon = (iconKey: string) => invoke<string | null>('notification_icon', { iconKey });
export const smsContactPhoto = (threadId: number) => invoke<string | null>('sms_contact_photo', { threadId });
export const selectSmsThread = (threadId: number) => invoke<void>('select_sms_thread', { threadId });
export const sendSms = (threadId: number, body: string) => invoke<void>('send_sms', { threadId, body });
export const clearSmsCache = () => invoke<void>('clear_sms_cache');
export const clearClipboard = () => invoke<void>('clear_clipboard');
export const sendClipboard = (hash: string) => invoke<void>('send_clipboard', { hash });
export const copyClipboardLocal = (hash: string) => invoke<void>('copy_clipboard_local', { hash });
export const clearTransfers = () => invoke<void>('clear_transfers');
export const importDroppedFiles = (paths: string[]) => invoke<DroppedFileImport>('import_dropped_files', { paths });
export const openSharedFolder = () => invoke<void>('open_shared_folder');
export const chooseSharedFolder = () => invoke<void>('choose_shared_folder');
export const openTransfer = (path: string) => invoke<void>('open_transfer', { path });
export const createCommand = (name: string, command: string, description: string, detach: boolean) =>
  invoke<void>('create_command', { name, command, description, detach });
export const deleteCommand = (id: string) => invoke<void>('delete_command', { id });
export const updateCommand = (id: string, name: string, command: string, description: string, detach: boolean) =>
  invoke<void>('update_command', { id, name, command, description, detach });
export const runCommand = (id: string) => invoke<void>('run_command', { id });
export const mediaCommand = (source: 'phone' | 'desktop', command: string) =>
  invoke<void>('media_command', { source, command });
export const clearLogs = () => invoke<void>('clear_logs');
export const setupCamera = (retry: boolean) => invoke<void>('setup_camera', { retry });
export const setCameraDevice = (deviceId?: string) => invoke<void>('set_camera_device', { deviceId });
export const setCameraAutoLoad = (enabled: boolean) => invoke<void>('set_camera_auto_load', { enabled });
export const setCameraStreamParams = (fps: number, bitrateKbps: number) =>
  invoke<void>('set_camera_stream_params', { fps, bitrateKbps });
export const setCameraZoom = (value: number) => invoke<void>('set_camera_zoom', { value });
export const setCameraExposure = (value: number) => invoke<void>('set_camera_exposure', { value });
export const setCameraTorch = (enabled: boolean) => invoke<void>('set_camera_torch', { enabled });
export const switchCamera = () => invoke<void>('switch_camera');
