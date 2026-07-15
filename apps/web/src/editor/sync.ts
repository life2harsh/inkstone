import type { ServerWsMessage, ClientWsMessage } from '../types';

export type SyncCallback = (msg: ServerWsMessage) => void;

export class DocSyncClient {
  private ws: WebSocket | null = null;
  private docId: string;
  private onMessage: SyncCallback;
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private connected = false;

  constructor(docId: string, onMessage: SyncCallback) {
    this.docId = docId;
    this.onMessage = onMessage;
  }

  connect(): void {
    const protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
    const host = window.location.host;
    const url = `${protocol}//${host}/api/sync/docs/${this.docId}/ws`;

    this.ws = new WebSocket(url);

    this.ws.onopen = () => {
      this.connected = true;
      console.log('[sync] connected to', this.docId);
    };

    this.ws.onmessage = (event) => {
      try {
        const msg: ServerWsMessage = JSON.parse(event.data);
        this.onMessage(msg);
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
