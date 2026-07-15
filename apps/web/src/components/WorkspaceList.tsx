import { useEffect, useState } from 'react';
import { listWorkspaces } from '../api/client';
import type { Workspace } from '../types';

interface Props {
  selectedId: string | null;
  onSelect: (ws: Workspace) => void;
  onCreate: () => void;
}

export function WorkspaceList({ selectedId, onSelect, onCreate }: Props) {
  const [workspaces, setWorkspaces] = useState<Workspace[]>([]);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    listWorkspaces()
      .then((res) => setWorkspaces(res.items))
      .catch(console.error)
      .finally(() => setLoading(false));
  }, []);

  if (loading) return <div className="sidebar-item">Loading...</div>;

  return (
    <div className="sidebar-section">
      {workspaces.map((ws) => (
        <div
          key={ws.id}
          className={`sidebar-item ${selectedId === ws.id ? 'active' : ''}`}
          onClick={() => onSelect(ws)}
        >
          {ws.name}
        </div>
      ))}
      <div className="sidebar-item" onClick={onCreate}>
        + New Workspace
      </div>
    </div>
  );
}
