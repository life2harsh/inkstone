// Stonepen integration boundary.
//
// Integration gap:
// StonepenHandle provides is_dirty() but no push-based change notification.
// Inkstone needs to know when an Ink block becomes dirty without polling.
// Future contract: Stonepen should provide a host-visible change/dirty callback,
// or Inkstone must define an explicit flush lifecycle (e.g. on block blur, save).
// Do not poll is_dirty() in a loop. Do not use global variables.
