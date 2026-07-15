import { useEffect, useRef, useCallback } from 'react';
import { DocSyncClient } from './sync';

interface Props {
  docId: string;
  initialContent: string;
  onChange: (content: string) => void;
  syncClient: DocSyncClient | null;
}

export function MarkdownEditor({ docId, initialContent, onChange, syncClient }: Props) {
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  useEffect(() => {
    if (textareaRef.current) {
      textareaRef.current.value = initialContent;
    }
  }, [docId, initialContent]);

  const handleChange = useCallback(() => {
    const value = textareaRef.current?.value ?? '';
    onChange(value);

    // Placeholder: encrypt and send update via sync client
    if (syncClient?.isConnected()) {
      const encoder = new TextEncoder();
      const plaintext = encoder.encode(value);

      // TODO: Replace with actual XChaCha20-Poly1305 encryption
      // For now, send plaintext as a placeholder (not secure!)
      const fakeCiphertext = btoa(String.fromCharCode(...plaintext));

      syncClient.send({
        type: 'encrypted_update',
        doc_id: docId,
        client_update_id: crypto.randomUUID(),
        encrypted_update_b64: fakeCiphertext,
        nonce_b64: btoa('0'.repeat(24)),
        aad_version: 1,
      });
    }
  }, [docId, onChange, syncClient]);

  return (
    <textarea
      ref={textareaRef}
      className="markdown-editor"
      placeholder="Start writing in Markdown..."
      onChange={handleChange}
      spellCheck={false}
    />
  );
}
