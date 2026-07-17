# Workspace Format v1

A workspace is a flat or nested directory of ordinary Markdown notes and
assets, with a small Inkstone-owned metadata directory.

## Layout

```
workspace-root/
├── Notes.md
├── Projects/
│   └── Inkstone.md
├── images/
│   └── diagram.png
└── .inkstone/
    └── workspace.toml
```

## Rules

1. **Ordinary user content lives outside `.inkstone/`.**
   Notes, images, PDFs, etc. are plain files the user can open, edit, and
   version with any tool.

2. **Markdown notes are ordinary `.md` files.**
   No opaque binary envelope. The file itself is the canonical content.

3. **Assets are ordinary files.**
   Images, attachments, etc. live alongside notes in the same tree.

4. **Relative links must remain portable.**
   `[link](Projects/Inkstone.md)` works between desktop filesystem and
   browser virtual store without rewriting.

5. **`.inkstone/` is reserved for Inkstone-owned metadata.**
   Users should not create or edit files inside `.inkstone/` manually.
   The directory may not exist at all in a newly-initialised workspace.

6. **The abstract format must not assume:**
   - IndexedDB
   - Postgres
   - a desktop filesystem
   - a sync server

7. **User content must not exist only inside an opaque database.**
   The user must always be able to access their notes as normal files,
   even if Inkstone disappears.

8. **The final user-facing name for a workspace is NOT decided here.**
   Internally the code uses "workspace". The product may call it
   "notebook", "vault", "project", etc.

9. **Note/document Markdown serialization is explicitly deferred.**
   This pass does not define block markers, frontmatter identity rules,
   or Stonepen sidecars. Those will be added in a later format version.

10. **Future planned internal files (not yet implemented):**
    - `.inkstone/config.toml`
    - `.inkstone/init.lua`
    - theme metadata
    - document identity metadata

## Manifest: `.inkstone/workspace.toml`

The only file defined in this pass.

```toml
format_version = 1
workspace_id = "550e8400-e29b-41d4-a716-446655440000"
```

- `format_version` — must be `1`. Future versions will increase this.
- `workspace_id` — a v4 UUID assigned at workspace creation.
