export interface Workspace {
  id: string;
  name: string;
  description: string | null;
  owner_id: string;
  created_at: string;
  updated_at: string;
}

export interface Doc {
  id: string;
  workspace_id: string;
  encrypted_title_b64: string;
  title_nonce_b64: string;
  created_by: string;
  created_at: string;
  updated_at: string;
}

export interface PaginatedResponse<T> {
  items: T[];
  total: number;
}

export interface PostUpdateRequest {
  encrypted_update_b64: string;
  nonce_b64: string;
  aad_version: number;
  client_update_id: string;
  sender_device_id: string;
}

export interface UpdateResponse {
  seq: number;
  created_at: string;
}

export interface SnapshotResponse {
  snapshot_version: number;
  encrypted_snapshot_b64: string;
  nonce_b64: string;
  created_at: string;
}

// WebSocket protocol types (mirrors core/protocol.rs)
export type ClientWsMessage =
  | {
      type: 'encrypted_update';
      doc_id: string;
      client_update_id: string;
      encrypted_update_b64: string;
      nonce_b64: string;
      aad_version: number;
    }
  | { type: 'ping' }
  | {
      type: 'presence';
      doc_id: string;
      encrypted_presence_b64: string;
    };

export type ServerWsMessage =
  | {
      type: 'encrypted_update';
      doc_id: string;
      sender_device_id: string;
      seq: number;
      encrypted_update_b64: string;
      nonce_b64: string;
      aad_version: number;
    }
  | { type: 'pong' }
  | {
      type: 'presence';
      doc_id: string;
      sender_device_id: string;
      encrypted_presence_b64: string;
    }
  | { type: 'error'; message: string };
