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
  await expect(page.locator('.explorer-new-btn')).toBeVisible();
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

async function createNote(page, path, title, markdown) {
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

async function openNote(page, path, title) {
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
  await expect(page.locator('.preview-content')).toContainText('Latest title');
  await page.getByRole('button', { name: 'Edit', exact: true }).click();
  await expect(page.locator('.document-title-input')).toHaveValue('Latest title');
  await expect(page.locator('.block-textarea')).toHaveValue('# Body');
});

test('search and recent results navigate to the selected note', async ({ page }) => {
  await createNote(page, 'A.md', 'Alpha', '# Searchable phrase');
  await createNote(page, 'B.md', 'Beta', '# Other');
  await page.locator('.search-input').fill('Searchable phrase');
  await expect(page.locator('.search-result-item[data-path="A.md"]')).toBeVisible();
  await page.locator('.search-result-item[data-path="A.md"]').click();
  await expect(page.locator('.breadcrumb')).toHaveText('A.md');
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
  await page.locator('.recent-item[data-path="Home.md"]').click();
  if (!await page.locator('.inspector-tab[data-tab="Links"]').isVisible()) {
    await page.locator('.mode-bar .toggle-inspector-btn').click();
  }
  await page.locator('.inspector-tab[data-tab="Links"]').click();
  await expect(page.locator('.inspector-section')).toContainText('Notes label');
  await expect(page.locator('.inspector-section')).toContainText('Home context');
  await page.locator('.inspector-tab[data-tab="Tags"]').click();
  await expect(page.locator('.inspector-section')).toContainText('Current Note Tags');
  await expect(page.locator('.tag-item[data-tag="rust"]').last()).toContainText('2');
});
