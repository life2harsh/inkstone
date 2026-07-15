import type { Doc, PaginatedResponse, PostUpdateRequest, SnapshotResponse, UpdateResponse, Workspace } from '../types';

const DEV_USER_ID = '11111111-1111-4111-8111-111111111111';

function headers(): Record<string, string> {
  return {
    'Content-Type': 'application/json',
    'x-dev-user-id': DEV_USER_ID,
    'x-dev-device-id': crypto.randomUUID(),
  };
}

async function request<T>(url: string, options?: RequestInit): Promise<T> {
  const res = await fetch(url, {
    ...options,
    headers: { ...headers(), ...options?.headers },
  });
  if (!res.ok) {
    const body = await res.text();
    throw new Error(`API error ${res.status}: ${body}`);
  }
  return res.json();
}

// Workspaces

export async function createWorkspace(name: string, description?: string): Promise<Workspace> {
  return request<Workspace>('/api/workspaces', {
    method: 'POST',
    body: JSON.stringify({ name, description }),
  });
}

export async function listWorkspaces(): Promise<PaginatedResponse<Workspace>> {
  return request<PaginatedResponse<Workspace>>('/api/workspaces');
}

// Docs

export async function createDoc(
  workspaceId: string,
  encryptedTitleB64: string,
  titleNonceB64: string,
): Promise<Doc> {
  return request<Doc>(`/api/workspaces/${workspaceId}/docs`, {
    method: 'POST',
    body: JSON.stringify({
      encrypted_title_b64: encryptedTitleB64,
      title_nonce_b64: titleNonceB64,
    }),
  });
}

export async function listDocs(workspaceId: string): Promise<PaginatedResponse<Doc>> {
  return request<PaginatedResponse<Doc>>(`/api/workspaces/${workspaceId}/docs`);
}

export async function getDoc(docId: string): Promise<Doc> {
  return request<Doc>(`/api/docs/${docId}`);
}

// Updates

export async function postUpdate(docId: string, req: PostUpdateRequest): Promise<UpdateResponse> {
  return request<UpdateResponse>(`/api/docs/${docId}/updates`, {
    method: 'POST',
    body: JSON.stringify(req),
  });
}

export async function listUpdates(docId: string): Promise<PaginatedResponse<UpdateResponse>> {
  return request<PaginatedResponse<UpdateResponse>>(`/api/docs/${docId}/updates`);
}

// Snapshots

export async function getSnapshot(docId: string): Promise<SnapshotResponse> {
  return request<SnapshotResponse>(`/api/docs/${docId}/snapshot`);
}

export async function postSnapshot(
  docId: string,
  encryptedSnapshotB64: string,
  nonceB64: string,
  snapshotVersion: number,
): Promise<SnapshotResponse> {
  return request<SnapshotResponse>(`/api/docs/${docId}/snapshot`, {
    method: 'POST',
    body: JSON.stringify({
      encrypted_snapshot_b64: encryptedSnapshotB64,
      nonce_b64: nonceB64,
      snapshot_version: snapshotVersion,
    }),
  });
}
