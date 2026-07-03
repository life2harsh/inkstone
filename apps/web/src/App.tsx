import { useState, useCallback, useEffect } from 'react';
import './App.css';
import { WorkspaceList } from './components/WorkspaceList';
import { DocList } from './components/DocList';
import { GraphPanel } from './components/GraphPanel';
import { MarkdownEditor } from './editor/MarkdownEditor';
import { DocSyncClient } from './editor/sync';
import { createWorkspace, createDoc } from './api/client';
import type { Workspace, Doc } from './types';

function decodeDevBase64Title(b64: string): string {
  try {
    const binary = atob(b64);
    return new TextDecoder().decode(
      new Uint8Array(binary.split('').map((c) => c.charCodeAt(0))),
    );
  } catch {
    return 'Untitled';
  }
}

export default function App() {
  const [selectedWorkspace, setSelectedWorkspace] = useState<Workspace | null>(null);
  const [selectedDoc, setSelectedDoc] = useState<Doc | null>(null);
  const [docContent, setDocContent] = useState('');
  const [syncClient, setSyncClient] = useState<DocSyncClient | null>(null);

  useEffect(() => {
    if (!selectedDoc) return;

    const client = new DocSyncClient(selectedDoc.id, (msg) => {
      console.log('[sync] received:', msg);
    });

    client.connect();
    setSyncClient(client);

    return () => {
      client.disconnect();
      setSyncClient(null);
    };
  }, [selectedDoc?.id]);

  const handleSelectWorkspace = useCallback((ws: Workspace) => {
    setSelectedWorkspace(ws);
    setSelectedDoc(null);
  }, []);

  const handleSelectDoc = useCallback((doc: Doc) => {
    setSelectedDoc(doc);
    setDocContent(`# Document ${doc.id.slice(0, 8)}\n\nNo saved content replay is loaded yet.`);
  }, []);

  const handleCreateWorkspace = useCallback(async () => {
    const name = prompt('Workspace name:');
    if (!name) return;
    const ws = await createWorkspace(name);
    setSelectedWorkspace(ws);
  }, []);

  const handleCreateDoc = useCallback(async () => {
    if (!selectedWorkspace) return;
    const encoder = new TextEncoder();
    const titleBytes = encoder.encode('New Document');
    const encryptedTitleB64 = btoa(String.fromCharCode(...titleBytes));
    const nonceB64 = btoa('0'.repeat(24));

    const doc = await createDoc(selectedWorkspace.id, encryptedTitleB64, nonceB64);
    setSelectedDoc(doc);
    setDocContent('# New Document\n\nStart writing...');
  }, [selectedWorkspace]);

  const handleContentChange = useCallback((content: string) => {
    setDocContent(content);
  }, []);

  const title = selectedDoc
    ? decodeDevBase64Title(selectedDoc.encrypted_title_b64)
    : '';

  return (
    <div className="app">
      <div className="sidebar">
        <div className="sidebar-header">Inkstone</div>
        <WorkspaceList
          selectedId={selectedWorkspace?.id ?? null}
          onSelect={handleSelectWorkspace}
          onCreate={handleCreateWorkspace}
        />
        <DocList
          workspaceId={selectedWorkspace?.id ?? null}
          selectedId={selectedDoc?.id ?? null}
          onSelect={handleSelectDoc}
          onCreate={handleCreateDoc}
        />
      </div>

      <div className="main-area">
        <div className="toolbar">
          {selectedDoc ? (
            <span>{title}</span>
          ) : (
            <span style={{ color: '#999' }}>Select a document</span>
          )}
        </div>
        <div className="editor-container">
          <div className="editor-pane">
            {selectedDoc ? (
              <MarkdownEditor
                docId={selectedDoc.id}
                initialContent={docContent}
                onChange={handleContentChange}
                syncClient={syncClient}
              />
            ) : (
              <p style={{ color: '#999', padding: 24 }}>
                Select a workspace and document to start editing.
              </p>
            )}
          </div>
          <GraphPanel docId={selectedDoc?.id ?? null} />
        </div>
      </div>
    </div>
  );
}
