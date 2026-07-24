const { test, expect } = require('@playwright/test');

async function resetWorkspace(page) {
  page.__browserErrors = [];
  page.on('pageerror', error => page.__browserErrors.push(error.message));
  page.on('console', message => {
    if (message.type() === 'error') page.__browserErrors.push(message.text());
  });
  await page.goto('/');
  await page.evaluate(async () => {
    const databases = (await indexedDB.databases()).map(db => db.name).filter(Boolean);
    await Promise.all(databases.map(name => new Promise((resolve, reject) => {
      const request = indexedDB.deleteDatabase(name);
      request.onsuccess = resolve;
      request.onerror = () => reject(request.error);
      request.onblocked = resolve;
    })));
  });
  await page.reload();
  await expect(page.locator('.explorer-new-btn')).toBeAttached();
}

async function ensureWorkspaceOpen(page) {
  const toggle = page.locator('.sidebar-toggle-btn');
  if (await toggle.isVisible() && !/sidebar-open/.test(await page.locator('.app-container').getAttribute('class'))) {
    await toggle.click();
    await expect(page.locator('.app-container')).toHaveClass(/sidebar-open/);
  }
}

async function ensureInspectorOpen(page) {
  const inspector = page.locator('.inspector');
  const toggle = page.locator('.mode-bar .toggle-inspector-btn');
  if (await toggle.isVisible() && !/open/.test(await inspector.getAttribute('class'))) {
    await page.locator('.mode-bar .toggle-inspector-btn').click();
    await expect(inspector).toHaveClass(/open/);
  }
}

async function persisted(page, needle) {
  await page.waitForFunction(async expected => {
    const database = await new Promise((resolve, reject) => {
      const request = indexedDB.open('inkstone_local_db', 1);
      request.onsuccess = () => resolve(request.result);
      request.onerror = () => reject(request.error);
    });
    const values = await new Promise((resolve, reject) => {
      const request = database.transaction('documents').objectStore('documents').getAll();
      request.onsuccess = () => resolve(request.result);
      request.onerror = () => reject(request.error);
    });
    database.close();
    return values.some(value => String(value).includes(expected));
  }, needle);
}

async function latestBlockContent(page, type) {
  return page.evaluate(async blockType => {
    const database = await new Promise((resolve, reject) => {
      const request = indexedDB.open('inkstone_local_db', 1);
      request.onsuccess = () => resolve(request.result);
      request.onerror = () => reject(request.error);
    });
    const values = await new Promise((resolve, reject) => {
      const request = database.transaction('documents').objectStore('documents').getAll();
      request.onsuccess = () => resolve(request.result);
      request.onerror = () => reject(request.error);
    });
    database.close();

    let latest = null;
    for (const value of values) {
      const record = JSON.parse(String(value));
      for (const block of record.confirmed_doc.blocks) {
        if (block.content.type === blockType) latest = block.content;
      }
      for (const update of record.pending_updates) {
        if (update.op.type === 'insert_block' && update.op.block.content.type === blockType) {
          latest = update.op.block.content;
        }
        if (update.op.type === 'replace_block_content' && update.op.content.type === blockType) {
          latest = update.op.content;
        }
      }
    }
    return latest;
  }, type);
}

async function addBlock(page, type) {
  await page.locator(`.add-block-btn[data-block-type="${type}"]`).click();
  await expect(page.locator(`.block-wrapper[data-block-type="${type}"]`).last()).toBeVisible();
}

async function createNote(page, path, title, markdown) {
  await ensureWorkspaceOpen(page);
  await page.locator('.explorer-new-btn').click();
  await page.locator('.explorer-create-input').fill(path);
  await page.locator('.explorer-create-input').press('Enter');
  await expect(page.locator('.breadcrumb')).toHaveText(path);
  await expect(page.locator('.block-textarea')).toBeVisible();
  await page.locator('.document-title-input').fill(title);
  await persisted(page, title);
  await page.locator('.block-textarea').fill(markdown);
  await persisted(page, markdown.split('\n').at(-1));
}

async function drawInkStroke(page, canvas = page.locator('.block-wrapper[data-block-type="ink"] canvas')) {
  await expect(canvas).toBeVisible();
  await canvas.scrollIntoViewIfNeeded();
  const box = await canvas.boundingBox();
  await page.mouse.move(box.x + 55, box.y + 85);
  await page.mouse.down();
  await page.mouse.move(box.x + 125, box.y + 118, { steps: 6 });
  await page.mouse.move(box.x + 210, box.y + 72, { steps: 8 });
  await page.mouse.up();
}

async function preparePreviewNotebook(page) {
  await createNote(
    page,
    'Research/Bayes Rule Derivation.md',
    'Bayes Rule Derivation',
    '# Posterior update\n\nBayes rule connects a [prior reference](https://example.com/prior) to observed evidence. #bayes #probability\n\n## Components\n\n- Prior probability\n- Likelihood function\n- Normalizing evidence\n\n> Update beliefs only after accounting for the marginal likelihood.\n\n| Quantity | Meaning |\n| --- | --- |\n| Prior | Initial belief |\n| Posterior | Updated belief |\n\nUse `P(A|B)` for the posterior.'
  );
  await addBlock(page, 'math');
  await page.locator('.math-source-input').fill('P(A|B) = \\frac{P(B|A)P(A)}{P(B)}');
  await persisted(page, '\\frac');
  await addBlock(page, 'ink');
  await expect.poll(async () => (await latestBlockContent(page, 'ink'))?.stonepen_json.length ?? 0).toBeGreaterThan(100);
  await addBlock(page, 'divider');
}

async function openNote(page, path, title) {
  await ensureWorkspaceOpen(page);
  await page.locator(`.file-entry[data-path="${path}"]`).click();
  await expect(page.locator('.breadcrumb')).toHaveText(path);
  await expect(page.locator('.document-title-input')).toHaveValue(title);
}

function failOnBrowserErrors(page) {
  const errors = [];
  page.on('pageerror', error => errors.push(error.message));
  page.on('console', message => { if (message.type() === 'error') errors.push(message.text()); });
  return () => expect(errors, errors.join('\n')).toEqual([]);
}

test.beforeEach(async ({ page }) => resetWorkspace(page));
test.afterEach(async ({ page }) => {
  expect(page.__browserErrors, page.__browserErrors.join('\n')).toEqual([]);
});

test('creates, switches, and reopens notes', async ({ page }) => {
  const assertClean = failOnBrowserErrors(page);
  await createNote(page, 'A.md', 'Alpha', '# Alpha\n\nFirst note');
  await createNote(page, 'B.md', 'Beta', '# Beta\n\nSecond note');
  await openNote(page, 'A.md', 'Alpha');
  await expect(page.locator('.block-textarea')).toHaveValue('# Alpha\n\nFirst note');
  assertClean();
});

test('persists edits through switching and reload', async ({ page }) => {
  await createNote(page, 'A.md', 'Alpha', '# Alpha');
  await createNote(page, 'B.md', 'Beta', '# Beta');
  await openNote(page, 'A.md', 'Alpha');
  await page.locator('.document-title-input').fill('Alpha persisted');
  await persisted(page, 'Alpha persisted');
  await page.locator('.block-textarea').fill('# Durable body');
  await persisted(page, 'Durable body');
  await openNote(page, 'B.md', 'Beta');
  await page.reload();
  await openNote(page, 'A.md', 'Alpha persisted');
  await expect(page.locator('.block-textarea')).toHaveValue('# Durable body');
});

test('focused textarea switches to Preview on the first click', async ({ page }) => {
  await createNote(page, 'A.md', 'Alpha', '# Current content');
  await page.locator('.block-textarea').focus();
  await page.locator('.block-textarea').fill('# Latest content');
  await persisted(page, 'Latest content');
  await page.getByRole('button', { name: 'Preview', exact: true }).click();
  await expect(page.locator('.preview-content')).toContainText('Latest content');
});

test('focused title switches to Preview and Edit preserves content', async ({ page }) => {
  await createNote(page, 'A.md', 'Alpha', '# Body');
  await page.locator('.document-title-input').focus();
  await page.locator('.document-title-input').fill('Latest title');
  await persisted(page, 'Latest title');
  await page.getByRole('button', { name: 'Preview', exact: true }).click();
  await expect(page.locator('.document-title')).toContainText('Latest title');
  await page.getByRole('button', { name: 'Edit', exact: true }).click();
  await expect(page.locator('.document-title-input')).toHaveValue('Latest title');
  await expect(page.locator('.block-textarea')).toHaveValue('# Body');
});

test('search and recent results navigate to the selected note', async ({ page }) => {
  await createNote(page, 'A.md', 'Alpha', '# Searchable phrase');
  await createNote(page, 'B.md', 'Beta', '# Other');
  await ensureWorkspaceOpen(page);
  await page.locator('.search-input').fill('Searchable phrase');
  await expect(page.locator('.search-result-item[data-path="A.md"]')).toBeVisible();
  await page.locator('.search-result-item[data-path="A.md"]').click();
  await expect(page.locator('.breadcrumb')).toHaveText('A.md');
  await ensureWorkspaceOpen(page);
  await page.locator('.recent-item[data-path="B.md"]').click();
  await expect(page.locator('.breadcrumb')).toHaveText('B.md');
});

test('recent notes are newest-first without duplicates', async ({ page }) => {
  await createNote(page, 'A.md', 'Alpha', '# A');
  await createNote(page, 'B.md', 'Beta', '# B');
  await openNote(page, 'A.md', 'Alpha');
  await expect(page.locator('.recent-item')).toHaveCount(2);
  await expect(page.locator('.recent-item').first()).toContainText('A.md');
  await openNote(page, 'B.md', 'Beta');
  await expect(page.locator('.recent-item').first()).toContainText('B.md');
});

test('deletion survives reload', async ({ page }) => {
  await createNote(page, 'Keep.md', 'Keep', '# Keep');
  await createNote(page, 'Delete.md', 'Delete', '# Delete');
  await ensureWorkspaceOpen(page);
  page.once('dialog', dialog => dialog.accept());
  await page.locator('.file-entry[data-path="Delete.md"]').hover();
  await page.locator('.file-entry[data-path="Delete.md"] .delete-btn').click();
  await expect(page.locator('.file-entry[data-path="Delete.md"]')).toHaveCount(0);
  await page.reload();
  await expect(page.locator('.file-entry[data-path="Delete.md"]')).toHaveCount(0);
  await openNote(page, 'Keep.md', 'Keep');
});

test('core workflow has a clean browser console', async ({ page }) => {
  const assertClean = failOnBrowserErrors(page);
  await createNote(page, 'A.md', 'Alpha', '# Alpha');
  await page.getByRole('button', { name: 'Preview', exact: true }).click();
  await expect(page.locator('.preview-content')).toBeVisible();
  await page.getByRole('button', { name: 'Edit', exact: true }).click();
  assertClean();
});

test('workspace fits the configured viewport', async ({ page }, testInfo) => {
  await createNote(page, 'A.md', 'Alpha', '# Alpha');
  await expect(page.locator('.app-container')).toBeVisible();
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);
  expect(overflow, `${testInfo.project.name} horizontal overflow`).toBeLessThanOrEqual(0);
  if (testInfo.project.name === 'chromium-mobile') {
    await page.locator('.mode-bar .toggle-inspector-btn').click();
    await expect(page.locator('.inspector')).toHaveClass(/open/);
    await page.locator('.inspector-tab[data-tab="Tags"]').click();
    await page.locator('.inspector-close-btn').click();
    await expect(page.locator('.inspector')).not.toHaveClass(/open/);
  } else {
    await expect(page.locator('.inspector')).toBeVisible();
  }

  await page.setViewportSize({ width: 900, height: 800 });
  await page.getByRole('button', { name: 'Search pages' }).click();
  await expect(page.locator('.search-input')).toBeFocused();
  await expect(page.locator('.sidebar')).toBeVisible();
  await expect(page.locator('.drawer-backdrop')).toHaveCount(0);

  await page.setViewportSize({ width: 420, height: 800 });
  await page.keyboard.press('Control+K');
  await expect(page.locator('.app-container')).toHaveClass(/sidebar-open/);
  await expect(page.locator('.search-input')).toBeFocused();
});

test('creates and reloads Markdown, Math, Ink, and Divider blocks', async ({ page }) => {
  await createNote(page, 'Mixed.md', 'Mixed notebook', '# Markdown block');
  await addBlock(page, 'markdown');
  await page.locator('.markdown-source-input').last().fill('Second Markdown block');
  await persisted(page, 'Second Markdown block');
  await addBlock(page, 'math');
  await page.locator('.math-source-input').fill('P(A|B) = P(B|A)P(A) / P(B)');
  await persisted(page, 'P(A|B)');
  await addBlock(page, 'ink');
  await expect.poll(async () => (await latestBlockContent(page, 'ink'))?.stonepen_json.length ?? 0).toBeGreaterThan(100);
  await addBlock(page, 'divider');

  await expect(page.locator('.block-wrapper[data-block-type="markdown"]')).toHaveCount(2);
  await expect(page.locator('.block-wrapper[data-block-type="math"]')).toHaveCount(1);
  await expect(page.locator('.block-wrapper[data-block-type="ink"]')).toHaveCount(1);
  await expect(page.locator('.block-wrapper[data-block-type="divider"]')).toHaveCount(1);

  await page.reload();
  await openNote(page, 'Mixed.md', 'Mixed notebook');
  await expect(page.locator('.math-source-input')).toHaveValue('P(A|B) = P(B|A)P(A) / P(B)');
  await expect(page.locator('.ink-canvas-host canvas')).toBeVisible();
  await expect(page.locator('.document-divider')).toBeVisible();
  await page.getByRole('button', { name: 'Preview', exact: true }).click();
  const previewBlocks = page.locator('.preview-block');
  await expect(previewBlocks).toHaveCount(5);
  await expect(previewBlocks.nth(0)).toHaveClass(/preview-markdown/);
  await expect(previewBlocks.nth(1)).toHaveClass(/preview-markdown/);
  await expect(previewBlocks.nth(2)).toHaveClass(/preview-math/);
  await expect(previewBlocks.nth(3)).toHaveClass(/preview-ink/);
  await expect(previewBlocks.nth(4)).toHaveClass(/preview-divider/);
  await page.getByRole('button', { name: 'Edit', exact: true }).click();
});

test('Preview is a semantic reading page without authoring controls', async ({ page }) => {
  await preparePreviewNotebook(page);
  await page.getByRole('button', { name: 'Preview', exact: true }).click();

  for (const selector of [
    '.document-title-input',
    '.block-textarea',
    '.math-source-input',
    '.block-insert-control',
    '.block-action-btn',
    '.ink-tool-btn',
    '.ink-width-input',
    '.ink-color-btn'
  ]) {
    await expect(page.locator(selector), `${selector} is absent in Preview`).toHaveCount(0);
  }

  await expect(page.getByRole('heading', { name: 'Bayes Rule Derivation', exact: true })).toBeVisible();
  await expect(page.getByRole('heading', { name: 'Posterior update', exact: true })).toBeVisible();
  await expect(page.locator('.preview-markdown ul li')).toHaveCount(3);
  await expect(page.getByRole('link', { name: 'prior reference' })).toHaveAttribute('href', 'https://example.com/prior');
  await expect(page.locator('.preview-markdown table')).toBeVisible();
  await expect(page.locator('.preview-markdown blockquote')).toBeVisible();
  await expect(page.locator('.preview-content')).not.toContainText('# Posterior update');
  await expect(page.locator('.preview-content')).not.toContainText('- Prior probability');
  await expect(page.locator('.preview-math .math-display math')).toBeVisible();
  await expect(page.locator('.preview-math mfrac')).toBeVisible();
  await expect(page.locator('.preview-divider .document-divider')).toBeVisible();
  await expect(page.locator('.preview-ink canvas[role="img"]')).toBeVisible();
  await expect(page.getByRole('button', { name: 'Expand Ink drawing' })).toBeVisible();
});

test('Preview Ink is read-only and returns to editable Ink without loss', async ({ page }) => {
  await preparePreviewNotebook(page);
  const editCanvas = page.locator('.block-wrapper[data-block-type="ink"] canvas');
  const emptyPixels = await editCanvas.evaluate(canvas => canvas.toDataURL());
  const initialJson = (await latestBlockContent(page, 'ink')).stonepen_json;
  await drawInkStroke(page, editCanvas);
  await expect.poll(async () => (await latestBlockContent(page, 'ink'))?.stonepen_json).not.toBe(initialJson);
  const drawnJson = (await latestBlockContent(page, 'ink')).stonepen_json;
  await expect.poll(async () => editCanvas.evaluate(canvas => canvas.toDataURL())).not.toBe(emptyPixels);

  await page.getByRole('button', { name: 'Preview', exact: true }).click();
  const previewCanvas = page.locator('.preview-ink canvas');
  await expect(previewCanvas).toBeVisible();
  const beforeInputPixels = await previewCanvas.evaluate(canvas => canvas.toDataURL());
  await previewCanvas.evaluate(canvas => {
    const rect = canvas.getBoundingClientRect();
    const init = {
      bubbles: true,
      pointerId: 41,
      pointerType: 'mouse',
      buttons: 1,
      clientX: rect.left + 95,
      clientY: rect.top + 95
    };
    canvas.dispatchEvent(new PointerEvent('pointerdown', init));
    canvas.dispatchEvent(new PointerEvent('pointerup', { ...init, buttons: 0 }));
  });
  await expect.poll(async () => (await latestBlockContent(page, 'ink')).stonepen_json).toBe(drawnJson);
  expect(await previewCanvas.evaluate(canvas => canvas.toDataURL())).toBe(beforeInputPixels);

  await previewCanvas.evaluate(canvas => {
    canvas.tabIndex = 0;
    canvas.focus();
  });
  await page.keyboard.press('Control+Z');
  await page.keyboard.press('Delete');
  await expect.poll(async () => (await latestBlockContent(page, 'ink')).stonepen_json).toBe(drawnJson);
  expect(await previewCanvas.evaluate(canvas => canvas.toDataURL())).toBe(beforeInputPixels);

  await page.getByRole('button', { name: 'Edit', exact: true }).click();
  await expect(page.locator('.document-title-input')).toHaveValue('Bayes Rule Derivation');
  await expect(page.locator('.math-source-input')).toHaveValue('P(A|B) = \\frac{P(B|A)P(A)}{P(B)}');
  await expect(page.locator('.ink-tool-btn')).toHaveCount(4);
  await expect.poll(async () => (await latestBlockContent(page, 'ink')).stonepen_json).toBe(drawnJson);
  const returnedCanvas = page.locator('.block-wrapper[data-block-type="ink"] canvas');
  await drawInkStroke(page, returnedCanvas);
  await expect.poll(async () => (await latestBlockContent(page, 'ink')).stonepen_json).not.toBe(drawnJson);
});

test('Preview Ink expands and collapses read-only without changing JSON', async ({ page }) => {
  await preparePreviewNotebook(page);
  const initialJson = (await latestBlockContent(page, 'ink')).stonepen_json;
  await drawInkStroke(page);
  await expect.poll(async () => (await latestBlockContent(page, 'ink'))?.stonepen_json).not.toBe(initialJson);
  const drawnJson = (await latestBlockContent(page, 'ink')).stonepen_json;
  await page.getByRole('button', { name: 'Preview', exact: true }).click();

  const ink = page.locator('.preview-ink');
  const mainContent = page.locator('.main-content');
  const expandButton = page.getByRole('button', { name: 'Expand Ink drawing' });
  await expandButton.scrollIntoViewIfNeeded();
  const scrollPosition = await mainContent.evaluate(element => element.scrollTop);
  await expandButton.click();
  await expect(ink).toHaveClass(/expanded/);
  await expect(mainContent).toHaveClass(/ink-expanded/);
  await expect(page.locator('.preview-ink canvas')).toBeVisible();
  await expect(page.locator('.ink-tool-btn')).toHaveCount(0);
  await expect(page.locator('.ink-toolbar')).toHaveCount(0);
  const expandedBox = await ink.boundingBox();
  expect(expandedBox.width).toBeGreaterThan(380);
  expect(expandedBox.height).toBeGreaterThan(650);
  await page.getByRole('button', { name: 'Collapse Ink drawing' }).click();
  await expect(ink).not.toHaveClass(/expanded/);
  await expect(mainContent).not.toHaveClass(/ink-expanded/);
  expect(await mainContent.evaluate(element => element.scrollTop)).toBe(scrollPosition);

  await page.getByRole('button', { name: 'Expand Ink drawing' }).click();
  await expect(ink).toHaveClass(/expanded/);
  await page.keyboard.press('Escape');
  await expect(ink).not.toHaveClass(/expanded/);
  await expect(mainContent).not.toHaveClass(/ink-expanded/);
  expect(await mainContent.evaluate(element => element.scrollTop)).toBe(scrollPosition);
  await expect.poll(async () => (await latestBlockContent(page, 'ink')).stonepen_json).toBe(drawnJson);
});

test('Stonepen drawing, tools, undo, redo, navigation, and reload are durable', async ({ page }) => {
  await createNote(page, 'Ink.md', 'Ink notebook', '# Drawing proof');
  await addBlock(page, 'ink');
  const canvas = page.locator('.ink-canvas-host canvas');
  await expect(canvas).toBeVisible();
  await expect.poll(async () => {
    const box = await canvas.boundingBox();
    return box ? Math.min(box.width, box.height) : 0;
  }).toBeGreaterThan(200);

  await expect.poll(async () => latestBlockContent(page, 'ink')).not.toBeNull();
  const initialJson = (await latestBlockContent(page, 'ink')).stonepen_json;
  const initialPixels = await canvas.evaluate(element => element.toDataURL());
  const box = await canvas.boundingBox();
  await page.mouse.move(box.x + 60, box.y + 95);
  await page.mouse.down();
  await page.mouse.move(box.x + 120, box.y + 125, { steps: 6 });
  await page.mouse.move(box.x + 205, box.y + 80, { steps: 8 });
  await page.mouse.up();

  await expect.poll(async () => (await latestBlockContent(page, 'ink'))?.stonepen_json).not.toBe(initialJson);
  const drawnJson = (await latestBlockContent(page, 'ink')).stonepen_json;
  await expect.poll(async () => canvas.evaluate(element => element.toDataURL())).not.toBe(initialPixels);

  const inkBlock = page.locator('.block-wrapper[data-block-type="ink"]');
  await inkBlock.getByRole('button', { name: 'Eraser', exact: true }).click();
  await expect(inkBlock.getByRole('button', { name: 'Eraser', exact: true })).toHaveClass(/active/);
  await inkBlock.getByRole('button', { name: 'Select', exact: true }).click();
  await expect(inkBlock.getByRole('button', { name: 'Select', exact: true })).toHaveClass(/active/);
  await inkBlock.getByRole('button', { name: 'Pen', exact: true }).click();
  await inkBlock.locator('.ink-width-input').fill('8');
  await inkBlock.getByRole('button', { name: 'Blue', exact: true }).click();

  await inkBlock.getByRole('button', { name: 'Undo', exact: true }).click();
  await expect.poll(async () => (await latestBlockContent(page, 'ink'))?.stonepen_json).not.toBe(drawnJson);
  await inkBlock.getByRole('button', { name: 'Redo', exact: true }).click();
  await expect.poll(async () => (await latestBlockContent(page, 'ink'))?.stonepen_json).toBe(drawnJson);

  await createNote(page, 'Other.md', 'Other note', '# Other');
  await openNote(page, 'Ink.md', 'Ink notebook');
  await expect(page.locator('.ink-canvas-host canvas')).toBeVisible();
  await page.reload();
  await openNote(page, 'Ink.md', 'Ink notebook');
  await expect(page.locator('.ink-canvas-host canvas')).toBeVisible();
  await expect.poll(async () => (await latestBlockContent(page, 'ink'))?.stonepen_json).toBe(drawnJson);
  await expect.poll(async () => page.locator('.ink-canvas-host canvas').evaluate(element => element.toDataURL())).not.toBe(initialPixels);
});

test('knowledge navigation resolves outlines, links, backlinks, and tags', async ({ page }) => {
  await createNote(page, 'Projects/Target.md', 'Target', '# Target\n#Rust');
  await createNote(page, 'Notes.md', 'Notes', '# Notes\nHome context [[Home|Home link]]');
  await createNote(
    page,
    'Home.md',
    'Home',
    '# Overview\n## Details\n## Details\n[[Projects/Target|Readable Alias]]\n[Notes label](Notes.md)\n#rust #RUST'
  );
  await page.reload();
  await expect(page.locator('.search-status')).toBeHidden();
  await openNote(page, 'Home.md', 'Home');
  await expect(page.locator('.outline-item[data-anchor]')).toHaveCount(3);
  await page.getByRole('button', { name: 'Preview', exact: true }).click();
  await expect(page.locator('.preview-content h2#details')).toHaveCount(1);
  await expect(page.locator('.preview-content h2#details-2')).toHaveCount(1);
  await expect(page.locator('.wikilink')).toHaveText('Readable Alias');
  await page.locator('.wikilink').click();
  await expect(page.locator('.breadcrumb')).toHaveText('Projects/Target.md');
  await ensureWorkspaceOpen(page);
  await page.locator('.recent-item[data-path="Home.md"]').click();
  await ensureInspectorOpen(page);
  await page.locator('.inspector-tab[data-tab="Backlinks"]').click();
  await expect(page.locator('.inspector-section')).toContainText('Notes label');
  await expect(page.locator('.inspector-section')).toContainText('Home context');
  await page.locator('.inspector-tab[data-tab="Tags"]').click();
  await expect(page.locator('.inspector-section')).toContainText('Current Note Tags');
  await expect(page.locator('.tag-item[data-tag="rust"]').last()).toContainText('2');
  await page.locator('.inspector-tab[data-tab="Graph"]').click();
  await expect(page.locator('.local-graph-node[data-path="Projects/Target.md"]')).toBeVisible();
  await page.locator('.graph-node-hit[data-path="Projects/Target.md"]').click();
  await expect(page.locator('.breadcrumb')).toHaveText('Projects/Target.md');
});
