import type { ServerWsMessage, ClientWsMessage } from '../types';
import { DEV_USER_ID, DEV_DEVICE_ID } from '../api/client';

export type SyncCallback = (msg: ServerWsMessage) => void;

export class DocSyncClient {
  private ws: WebSocket | null = null;
  private docId: string;
  private onMessage: SyncCallback;
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private connected = false;

  // Track outgoing client_update_ids so echo from broadcast is suppressed.
  private pendingOutgoingIds: Set<string> = new Set();
  private maxPendingIds = 64;

  constructor(docId: string, onMessage: SyncCallback) {
    this.docId = docId;
    this.onMessage = onMessage;
  }

  connect(): void {
    const protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
    const host = window.location.host;

    const params = new URLSearchParams({
      dev_user_id: DEV_USER_ID,
      dev_device_id: DEV_DEVICE_ID,
    });
    const url = `${protocol}//${host}/api/sync/docs/${this.docId}/ws?${params}`;

    this.ws = new WebSocket(url);

    this.ws.onopen = () => {
      this.connected = true;
      console.log('[sync] connected to', this.docId);
    };

    this.ws.onmessage = (event) => {
      try {
        const parsed: ServerWsMessage = JSON.parse(event.data);

        // Suppress own echo: if this is our own encrypted_update broadcast,
        // matched by client_update_id, drop it silently.
        if (parsed.type === 'encrypted_update') {
          if (this.pendingOutgoingIds.has(parsed.client_update_id)) {
            this.pendingOutgoingIds.delete(parsed.client_update_id);
            return;
          }
        }

        this.onMessage(parsed);
      } catch (err) {
        console.error('[sync] failed to parse message', err);
      }
    };

    this.ws.onclose = () => {
      this.connected = false;
      console.log('[sync] disconnected, reconnecting in 3s...');
      this.reconnectTimer = setTimeout(() => this.connect(), 3000);
    };

    this.ws.onerror = (err) => {
      console.error('[sync] error', err);
      this.ws?.close();
    };
  }

  send(msg: ClientWsMessage): void {
    if (this.ws && this.connected) {
      if (msg.type === 'encrypted_update') {
        this.pendingOutgoingIds.add(msg.client_update_id);
        if (this.pendingOutgoingIds.size > this.maxPendingIds) {
          const first = this.pendingOutgoingIds.values().next().value;
          if (first) this.pendingOutgoingIds.delete(first);
        }
      }
      this.ws.send(JSON.stringify(msg));
    }
  }

  disconnect(): void {
    if (this.reconnectTimer) {
      clearTimeout(this.reconnectTimer);
    }
    this.ws?.close();
    this.ws = null;
    this.connected = false;
  }

  isConnected(): boolean {
    return this.connected;
  }
}
