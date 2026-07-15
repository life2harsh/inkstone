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

    if (syncClient?.isConnected()) {
      // DEV MODE PLACEHOLDER: base64-encode plaintext.
      // This is NOT encrypted. Real clients must encrypt with XChaCha20-Poly1305
      // before sending. The server never sees plaintext in production.
      const encoder = new TextEncoder();
      const devPlaintext = encoder.encode(value);
      const devPlaintextB64 = btoa(String.fromCharCode(...devPlaintext));

      syncClient.send({
        type: 'encrypted_update',
        doc_id: docId,
        client_update_id: crypto.randomUUID(),
        encrypted_update_b64: devPlaintextB64,
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
