interface Props {
  inkId: string;
}

export function InkBlock({ inkId }: Props) {
  return (
    <div className="ink-block-placeholder">
      <div>Ink Block</div>
      <div style={{ fontSize: 11, color: '#999', marginTop: 4 }}>
        id: {inkId}
      </div>
      <div style={{ fontSize: 11, color: '#999', marginTop: 2 }}>
        (handwriting rendering coming in v0.2)
      </div>
    </div>
  );
}
