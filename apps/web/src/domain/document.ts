export const SCHEMA_VERSION = 1;

export type BlockContent =
  | { type: 'markdown'; text: string }
  | { type: 'math'; source: string }
  | { type: 'ink'; stonepen_json: string }
  | { type: 'divider' };

export interface DocumentBlock {
  id: string;
  created_at_ms: number;
  updated_at_ms: number;
  content: BlockContent;
}

export interface InkstoneDocument {
  id: string;
  schema_version: number;
  title: string;
  blocks: DocumentBlock[];
  metadata: Record<string, string>;
}

export function newDocument(id: string): InkstoneDocument {
  return {
    id,
    schema_version: SCHEMA_VERSION,
    title: '',
    blocks: [],
    metadata: {},
  };
}

export function newBlock(id: string, content: BlockContent): DocumentBlock {
  const now = Date.now();
  return { id, created_at_ms: now, updated_at_ms: now, content };
}
