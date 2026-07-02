import { useEffect, useState } from 'react';
import { listDocs } from '../api/client';
import type { Doc } from '../types';

interface Props {
  workspaceId: string | null;
  selectedId: string | null;
  onSelect: (doc: Doc) => void;
  onCreate: () => void;
}

export function DocList({ workspaceId, selectedId, onSelect, onCreate }: Props) {
  const [docs, setDocs] = useState<Doc[]>([]);
  const [loading, setLoading] = useState(false);

  useEffect(() => {
    if (!workspaceId) return;
    setLoading(true);
    listDocs(workspaceId)
      .then((res) => setDocs(res.items))
      .catch(console.error)
      .finally(() => setLoading(false));
  }, [workspaceId]);

  if (!workspaceId) return null;

  return (
    <div className="sidebar-section">
      <div className="sidebar-header">Documents</div>
      {loading ? (
        <div className="sidebar-item">Loading...</div>
      ) : (
        docs.map((doc) => (
          <div
            key={doc.id}
            className={`sidebar-item ${selectedId === doc.id ? 'active' : ''}`}
            onClick={() => onSelect(doc)}
          >
            {doc.id.slice(0, 8)}...
          </div>
        ))
      )}
      <div className="sidebar-item" onClick={onCreate}>
        + New Document
      </div>
    </div>
  );
}
