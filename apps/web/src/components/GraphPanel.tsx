interface Props {
  docId: string | null;
}

export function GraphPanel({ docId }: Props) {
  if (!docId) {
    return (
      <div className="side-panel">
        <h3>Graph</h3>
        <p className="panel-placeholder">Select a document to view graph</p>
      </div>
    );
  }

  return (
    <div className="side-panel">
      <h3>Graph</h3>
      <p className="panel-placeholder">
        Graph view will be computed client-side from parsed Markdown wikilinks and tags.
      </p>
      <div style={{ marginTop: 16 }}>
        <h3>Backlinks</h3>
        <p className="panel-placeholder">No backlinks yet.</p>
      </div>
      <div style={{ marginTop: 16 }}>
        <h3>Tags</h3>
        <p className="panel-placeholder">No tags found.</p>
      </div>
    </div>
  );
}
