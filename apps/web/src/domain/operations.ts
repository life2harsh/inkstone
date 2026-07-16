export const UPDATE_VERSION = 1;

import type { BlockContent, DocumentBlock } from './document';

export interface DocumentUpdate {
  version: number;
  update_id: string;
  doc_id: string;
  applied_at_ms: number;
  op: DocumentOp;
}

export type DocumentOp =
  | { type: 'insert_block'; idx: number; block: DocumentBlock }
  | { type: 'delete_block'; block_id: string }
  | { type: 'move_block'; block_id: string; new_idx: number }
  | { type: 'replace_block_content'; block_id: string; content: BlockContent }
  | { type: 'set_title'; title: string };
