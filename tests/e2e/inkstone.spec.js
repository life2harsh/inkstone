const { test, expect } = require('@playwright/test');

test('sensitive account fragments are cleared before rendering token flows', async ({ page }) => {
  await page.route('**/api/auth/me', route => route.fulfill({
    status: 200,
    contentType: 'application/json',
    body: JSON.stringify({
      id: '00000000-0000-0000-0000-000000000001',
      email: 'token-test@example.test',
      device_id: '00000000-0000-0000-0000-000000000002'
    })
  }));
  await page.goto('/#password-reset=browser-secret-token&email=token-test%40example.test');

  await expect(page).toHaveURL(/\/$/);
  await expect(page.getByRole('heading', { name: 'Reset your password' })).toBeVisible();
  await expect(page.getByText(/you'll need your recovery key to unlock them/)).toBeVisible();
  await expect(page.locator('.reset-email')).toHaveValue('token-test@example.test');
  await expect(page.locator('body')).not.toContainText('browser-secret-token');
});

async function resetWorkspace(page) {
  page.__browserErrors = [];
  page.on('pageerror', error => page.__browserErrors.push(error.message));
  page.on('console', message => {
    if (message.type() === 'error') page.__browserErrors.push(message.text());
  });
  await page.goto('/?local=1');
  await expect(page.locator('.explorer-new-btn')).toBeAttached();
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
  await expect(page.locator('.explorer-new-btn')).toBeAttached();
  const toggle = page.locator('.sidebar-toggle-btn');
  if (await toggle.isVisible() && !/sidebar-open/.test(await page.locator('.app-container').getAttribute('class'))) {
    await toggle.click();
    await expect(page.locator('.app-container')).toHaveClass(/sidebar-open/);
  }
  await expect(page.locator('.sidebar')).toBeVisible();
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

// The note's Markdown, one element per block, joined like an export.
async function pageMarkdown(page) {
  return page.evaluate(async () => {
    const module = await import('/pkg/inkstone_web.js');
    const doc = JSON.parse(module.active_document_json_for_test?.() ?? '{"blocks":[]}');
    return doc.blocks
      .filter(block => block.content.type === 'markdown')
      .map(block => block.content.text)
      .join('\n\n');
  });
}

function pageBlocks(page, type) {
  return page.locator(type ? `.page-block[data-block-type="${type}"]` : '.page-block');
}

// Rendered blocks become source on click; returns that block's textarea.
async function openSource(page, type = 'markdown', index = 0) {
  const block = pageBlocks(page, type).nth(index);
  await expect(block).toBeVisible();
  const source = block.locator('.page-source');
  if (await source.count() === 0) {
    await block.evaluate(element => element.scrollIntoView({ block: 'start' }));
    await block.locator('.page-rendered').click({ position: { x: 2, y: 2 } });
  } else if (!(await source.evaluate(element => element === document.activeElement))) {
    await source.click();
  }
  await expect(source).toBeVisible();
  await expect(source).toBeFocused();
  return source;
}

async function closeSource(page) {
  await page.keyboard.press('Escape');
  await expect(page.locator('.page-source')).toHaveCount(0);
  await saved(page);
}

// Every edit has reached this device's storage.
async function saved(page) {
  await expect.poll(() => page.evaluate(async () => {
    const module = await import('/pkg/inkstone_web.js');
    return module.has_unsaved_edits_for_test();
  })).toBe(false);
}

async function addBlock(page, type) {
  const before = await pageBlocks(page, type).count();
  await page.locator(`.page-insert .add-block-btn[data-block-type="${type}"]`).click();
  await expect(pageBlocks(page, type)).toHaveCount(before + 1);
  await expect(pageBlocks(page, type).last()).toBeVisible();
}

async function createNote(page, path, title, markdown) {
  await ensureWorkspaceOpen(page);
  await page.locator('.explorer-new-btn').click();
  await page.locator('.explorer-create-input').fill(path);
  await page.locator('.explorer-create-input').press('Enter');
  await expect(page.locator('.breadcrumb')).toHaveText(path);
  await expect(page.locator('.page-surface')).toBeVisible();
  await page.locator('.document-title-input').fill(title);
  await persisted(page, title);
  const source = await openSource(page);
  await source.fill(markdown);
  await persisted(page, markdown.split('\n').at(-1));
  await closeSource(page);
}

async function pageInk(page) {
  return page.evaluate(async () => {
    const module = await import('/pkg/inkstone_web.js');
    const json = module.active_document_json_for_test?.();
    return json ? JSON.parse(json).ink ?? null : null;
  });
}

function strokeCount(ink) {
  return (ink?.stonepen_json.match(/"Stroke":/g) ?? []).length;
}

function anchorTargets(ink) {
  return Object.values(ink?.anchors ?? {}).map(anchor => anchor.target);
}

async function chooseInkTool(page, tool = 'pen') {
  const button = page.locator(`.ink-dock [data-page-ink="tool"][data-tool="${tool}"]`);
  await button.click();
  await expect(button).toHaveClass(/active/);
  await expect(page.locator('.page-surface')).toHaveClass(/ink-mode/);
}

async function chooseTyping(page) {
  await page.locator('.ink-dock [data-page-ink="type"]').click();
  await expect(page.locator('.page-surface')).not.toHaveClass(/ink-mode/);
}

// Draws one stroke over `target` (any page element) with the mouse.
async function drawOver(page, target, { x = 20, y = 0.5, dx = 160 } = {}) {
  // Renders replace page elements, so scroll and measure in separate,
  // re-resolving steps rather than holding a handle.
  await expect(target).toBeVisible();
  await target.evaluate(element => element.scrollIntoView({ block: 'center' }));
  const box = await target.boundingBox();
  const startY = box.y + box.height * y;
  await page.mouse.move(box.x + x, startY);
  await page.mouse.down();
  await page.mouse.move(box.x + x + dx / 2, startY + 14, { steps: 6 });
  await page.mouse.move(box.x + x + dx, startY - 6, { steps: 6 });
  await page.mouse.up();
}

async function canvasPixels(page) {
  return page.locator('#page-ink-canvas').evaluate(canvas => {
    const context = canvas.getContext('2d');
    const data = context.getImageData(0, 0, canvas.width, canvas.height).data;
    let painted = 0;
    for (let index = 3; index < data.length; index += 4) if (data[index] > 0) painted += 1;
    return painted;
  });
}

// Topmost canvas row with ink, in canvas pixels.
async function inkTop(page) {
  return page.locator('#page-ink-canvas').evaluate(canvas => {
    const data = canvas.getContext('2d').getImageData(0, 0, canvas.width, canvas.height).data;
    for (let index = 3; index < data.length; index += 4) {
      if (data[index] > 0) return Math.floor((index - 3) / 4 / canvas.width);
    }
    return -1;
  });
}

async function inkSpan(page) {
  return page.locator('#page-ink-canvas').evaluate(canvas => {
    const data = canvas.getContext('2d').getImageData(0, 0, canvas.width, canvas.height).data;
    let min = Infinity;
    let max = -Infinity;
    for (let index = 3; index < data.length; index += 4) {
      if (data[index] === 0) continue;
      const x = ((index - 3) / 4) % canvas.width;
      min = Math.min(min, x);
      max = Math.max(max, x);
    }
    const rect = canvas.getBoundingClientRect();
    const ratio = rect.width / canvas.width;
    return max < min ? null : { left: rect.left + min * ratio, right: rect.left + max * ratio };
  });
}

async function inkBox(page) {
  return page.locator('#page-ink-canvas').evaluate(canvas => {
    const data = canvas.getContext('2d').getImageData(0, 0, canvas.width, canvas.height).data;
    let left = Infinity;
    let right = -Infinity;
    let top = Infinity;
    let bottom = -Infinity;
    for (let index = 3; index < data.length; index += 4) {
      if (data[index] === 0) continue;
      const pixel = (index - 3) / 4;
      const x = pixel % canvas.width;
      const y = Math.floor(pixel / canvas.width);
      left = Math.min(left, x);
      right = Math.max(right, x);
      top = Math.min(top, y);
      bottom = Math.max(bottom, y);
    }
    if (right < left) return null;
    const rect = canvas.getBoundingClientRect();
    const ratio = rect.width / canvas.width;
    return {
      left: rect.left + left * ratio,
      right: rect.left + right * ratio,
      top: rect.top + top * ratio,
      bottom: rect.top + bottom * ratio
    };
  });
}

async function wordBox(page, word) {
  return page.evaluate(needle => {
    const walker = document.createTreeWalker(document.querySelector('.page-surface'), NodeFilter.SHOW_TEXT);
    for (let node = walker.nextNode(); node; node = walker.nextNode()) {
      const index = node.data.indexOf(needle);
      if (index < 0) continue;
      const range = document.createRange();
      range.setStart(node, index);
      range.setEnd(node, index + needle.length);
      const rect = range.getBoundingClientRect();
      return { left: rect.left, right: rect.right, top: rect.top, bottom: rect.bottom };
    }
    return null;
  }, word);
}

async function circleAround(page, box) {
  const cx = (box.left + box.right) / 2;
  const cy = (box.top + box.bottom) / 2;
  const rx = (box.right - box.left) / 2 + 10;
  const ry = (box.bottom - box.top) / 2 + 8;
  await page.mouse.move(cx + rx, cy);
  await page.mouse.down();
  for (let step = 1; step <= 28; step += 1) {
    const angle = (step / 28) * Math.PI * 2;
    await page.mouse.move(cx + rx * Math.cos(angle), cy + ry * Math.sin(angle));
  }
  await page.mouse.up();
}

async function createStressWorkspace(page, count = 100) {
  test.setTimeout(120_000);
  for (let index = 0; index < count; index += 1) {
    const padded = String(index).padStart(3, '0');
    const previous = index === 0 ? '' : ` [[Stress/Note ${String(index - 1).padStart(3, '0')}]]`;
    await createNote(
      page,
      `Stress/Note ${padded}.md`,
      `Stress Note ${padded}`,
      `# Stress Note ${padded}\n\nSearch corpus token-${padded}.${previous} #stress`
    );
  }
}

async function preparePageNotebook(page) {
  await createNote(
    page,
    'Research/Bayes Rule Derivation.md',
    'Bayes Rule Derivation',
    '# Posterior update\n\nBayes rule connects a [prior reference](https://example.com/prior) to observed evidence. #bayes #probability\n\n## Components\n\n- Prior probability\n- Likelihood function\n- Normalizing evidence\n\n> Update beliefs only after accounting for the marginal likelihood.\n\n| Quantity | Meaning |\n| --- | --- |\n| Prior | Initial belief |\n| Posterior | Updated belief |\n\nUse `P(A|B)` for the posterior.'
  );
  await addBlock(page, 'math');
  await expect(page.locator('.math-source-input')).toBeFocused();
  await page.locator('.math-source-input').fill('P(A|B) = \\frac{P(B|A)P(A)}{P(B)}');
  await persisted(page, '\\frac');
  await closeSource(page);
  await addBlock(page, 'divider');
}

async function prepareResearchWorkspace(page) {
  const primaryMarkdown = `# Posterior inference

Bayes' rule is the bookkeeping identity behind probabilistic learning. It combines a prior model with observed evidence and makes every assumption available for inspection. #probability #bayes #derivation

The derivation starts from the symmetry of the joint distribution. See this [conditional probability reference](https://en.wikipedia.org/wiki/Conditional_probability) and [[Conjugate Priors|closed-form conjugate updates]] for the surrounding theory.

## Derivation

1. Write the joint probability in both conditional directions.
2. Equate the two factorizations.
3. Divide by the evidence term.

The resulting terms have distinct roles:

- **Prior**: belief before the observation
- **Likelihood**: compatibility of data and hypothesis
- **Evidence**: the normalizing probability of the data

> The denominator is not optional: it ensures the posterior integrates to one.

| Quantity | Interpretation | Notebook |
| --- | --- | --- |
| \`P(H)\` | Prior belief | [[Conjugate Priors|Conjugate Priors]] |
| \`P(D\\|H)\` | Likelihood | [[Thompson Sampling|Sequential decisions]] |
| \`P(D)\` | Marginal evidence | [Marginal likelihood](Marginal Likelihood.md) |

## Marginalization

For a discrete hypothesis space, compute the evidence by summing over all candidate hypotheses. In implementation notes, \`log_evidence\` is preferable when products become numerically small.

This identity also motivates [[Variational Inference|variational approximations]]. The related [[Teaching/Bayesian Statistics|teaching note]] presents the same argument geometrically.`;

  await createNote(
    page,
    'Research/Bayes Rule Derivation.md',
    'Bayes’ Rule Derivation',
    primaryMarkdown
  );
  await addBlock(page, 'math');
  await page.locator('.math-source-input').fill('P(H|D) = \\frac{P(D|H)P(H)}{P(D)}');
  await persisted(page, '\\frac');
  await closeSource(page);
  await addBlock(page, 'math');
  await page.locator('.math-source-input').fill('P(D) = \\sum_{h=1}^{n} P(D|h)P(h)');
  await persisted(page, '\\sum');
  await closeSource(page);

  // Annotate the derivation steps and sketch in a writing space below.
  await chooseInkTool(page);
  await drawOver(page, page.locator('.page-rendered ol li').nth(1), { x: -12, dx: 220 });
  await expect.poll(async () => strokeCount(await pageInk(page))).toBe(1);
  await addBlock(page, 'space');
  const space = pageBlocks(page, 'space').last();
  for (const y of [0.25, 0.45, 0.65]) {
    await drawOver(page, space, { x: 60, y, dx: 240 });
  }
  await expect.poll(async () => strokeCount(await pageInk(page))).toBe(4);
  await chooseTyping(page);
  await addBlock(page, 'divider');

  const linkedNotes = [
    ['Research/Probability Axioms.md', 'Probability Axioms', '# Probability axioms\n\nThe normalization axiom supports [[Bayes Rule Derivation|the Bayes derivation]]. #probability #foundations'],
    ['Research/Conjugate Priors.md', 'Conjugate Priors', '# Conjugate priors\n\nClosed-form updates are a direct application of [[Bayes Rule Derivation|Bayes’ rule]]. #bayes #probability'],
    ['Research/Thompson Sampling.md', 'Thompson Sampling', '# Thompson sampling\n\nPosterior sampling repeatedly applies [[Bayes Rule Derivation|the posterior update]]. #bayes #decisions'],
    ['Research/Marginal Likelihood.md', 'Marginal Likelihood', '# Marginal likelihood\n\nModel evidence is the denominator in [[Bayes Rule Derivation|Bayes’ rule]]. #probability #derivation'],
    ['Research/Variational Inference.md', 'Variational Inference', '# Variational inference\n\nApproximate inference begins where [[Bayes Rule Derivation|exact marginalization]] becomes expensive. #bayes #approximation'],
    ['Teaching/Bayesian Statistics.md', 'Bayesian Statistics', '# Bayesian statistics\n\nLecture 2 revisits [[Research/Bayes Rule Derivation|the complete derivation]] and [[Research/Probability Axioms|probability foundations]]. #bayes #teaching'],
    ['Projects/Experiment Notes.md', 'Experiment Notes', '# Experiment notes\n\nThe analysis plan follows [[Research/Bayes Rule Derivation|the posterior workflow]] and checks [[Research/Marginal Likelihood|model evidence]]. #probability #experiments']
  ];
  for (const [path, title, markdown] of linkedNotes) {
    await createNote(page, path, title, markdown);
  }
  await page.reload();
  await openNote(page, 'Research/Bayes Rule Derivation.md', 'Bayes’ Rule Derivation');
  await expect(page.locator('.search-status')).toBeHidden();
}

async function openNote(page, path, title) {
  await ensureWorkspaceOpen(page);
  const segments = path.split('/');
  for (let index = 1; index < segments.length; index += 1) {
    const folderPath = segments.slice(0, index).join('/');
    const folder = page.locator(`.explorer-folder[data-path="${folderPath}"]`);
    await expect(folder).toBeAttached();
    await ensureWorkspaceOpen(page);
    if (await folder.getAttribute('aria-expanded') === 'false') {
      await folder.click();
    }
  }
  const file = page.locator(`.file-entry[data-path="${path}"] .file-select-btn`);
  await expect(file).toBeVisible();
  await file.click();
  await expect(page.locator('.breadcrumb')).toHaveText(path);
  await expect(page.locator('.document-title-input')).toHaveValue(title);
}

function failOnBrowserErrors(page) {
  const errors = [];
  page.on('pageerror', error => errors.push(error.message));
  page.on('console', message => { if (message.type() === 'error') errors.push(message.text()); });
  return () => expect(errors, errors.join('\n')).toEqual([]);
}

function consumeExpectedBrowserErrors(page, pattern) {
  const expected = page.__browserErrors.filter(message => pattern.test(message));
  expect(expected.length, `expected browser error matching ${pattern}`).toBeGreaterThan(0);
  page.__browserErrors = page.__browserErrors.filter(message => !pattern.test(message));
}

test.beforeEach(async ({ page }) => resetWorkspace(page));
test.afterEach(async ({ page }) => {
  expect(page.__browserErrors, page.__browserErrors.join('\n')).toEqual([]);
});

test('a phone keyboard opening does not close the focused field', async ({ page }) => {
  await page.route('**/api/auth/me', route => route.fulfill({
    status: 401,
    contentType: 'application/json',
    body: JSON.stringify({ code: 401, error: 'Authentication required' })
  }));
  await page.goto('/');
  const email = page.locator('.auth-email');
  await email.click();
  const size = page.viewportSize();
  await page.setViewportSize({ width: size.width, height: Math.round(size.height / 2) });
  await page.keyboard.type('owner@example.test');
  await expect(email).toBeFocused();
  await expect(email).toHaveValue('owner@example.test');
  await page.setViewportSize(size);
  consumeExpectedBrowserErrors(page, /status of 401/);
});

test('creates, switches, and reopens notes', async ({ page }) => {
  const assertClean = failOnBrowserErrors(page);
  await createNote(page, 'A.md', 'Alpha', '# Alpha\n\nFirst note');
  await createNote(page, 'B.md', 'Beta', '# Beta\n\nSecond note');
  await openNote(page, 'A.md', 'Alpha');
  await expect(page.locator('.page-rendered h1')).toHaveText('Alpha');
  await expect.poll(() => pageMarkdown(page)).toBe('# Alpha\n\nFirst note');
  assertClean();
});

test('home lists every note, filters them, gathers open tasks, and opens notes', async ({ page }) => {
  await expect(page.locator('.home-empty')).toBeVisible();
  await createNote(page, 'Work/Plan.md', 'Launch plan', '- [ ] Book venue\n- [x] Draft invite\nWe ship in spring #launch');
  await createNote(page, 'Groceries.md', 'Groceries', '- [ ] Buy oranges');

  await ensureWorkspaceOpen(page);
  await page.locator('.sidebar-home-btn').click();
  await expect(page.locator('.sidebar-home-btn')).toHaveAttribute('aria-current', 'page');
  const cards = page.locator('.home-card');
  await expect(cards).toHaveCount(2);
  await expect(page.locator('.home-stats')).toContainText('2 notes');
  await expect(page.locator('.home-stats')).toContainText('2 open tasks');
  const plan = page.locator('.home-card[data-path="Work/Plan.md"]');
  await expect(plan.locator('.home-card-folder')).toHaveText('Work');
  await expect(plan.locator('.home-card-snippet')).toContainText('We ship in spring');
  await expect(plan).toContainText('1/2 done');
  await expect(page.locator('.home-task-text')).toHaveText(['Buy oranges', 'Book venue']);

  await page.locator('.home-filter').fill('orange');
  await expect(cards.filter({ visible: true })).toHaveCount(1);
  await expect(page.locator('.home-all-count')).toHaveText('1');
  await page.locator('.home-filter').fill('nothing matches this');
  await expect(page.locator('.home-no-match')).toBeVisible();
  await page.locator('.home-filter').fill('');

  await page.locator('[data-home-tag="launch"]').click();
  await expect(cards).toHaveCount(1);
  await page.locator('[data-home-tag=""]').click();
  await expect(cards).toHaveCount(2);

  await page.locator('[data-home-view="list"]').click();
  await expect(page.locator('.home-row')).toHaveCount(2);
  await page.locator('[data-home-sort="name"]').click();
  await expect(page.locator('.home-open')).toHaveText(['Groceries', 'Launch plan']);
  await page.reload();
  await expect(page.locator('.home-row')).toHaveCount(2);
  await page.locator('[data-home-view="cards"]').click();

  await page.locator('.home-task', { hasText: 'Book venue' }).click();
  await expect(page.locator('.breadcrumb')).toHaveText('Work/Plan.md');
  await expect(page.locator('.document-title-input')).toHaveValue('Launch plan');
  await expect(page.locator('.home')).toHaveCount(0);
});

test('persists edits through switching and reload', async ({ page }) => {
  await createNote(page, 'A.md', 'Alpha', '# Alpha');
  await createNote(page, 'B.md', 'Beta', '# Beta');
  await openNote(page, 'A.md', 'Alpha');
  await page.locator('.document-title-input').fill('Alpha persisted');
  await persisted(page, 'Alpha persisted');
  await (await openSource(page)).fill('# Durable body');
  await persisted(page, 'Durable body');
  await openNote(page, 'B.md', 'Beta');
  await page.reload();
  await openNote(page, 'A.md', 'Alpha persisted');
  await expect(await openSource(page)).toHaveValue('# Durable body');
});

test('Markdown persistence failure remains visible and retries normally', async ({ page }) => {
  await createNote(page, 'Failure.md', 'Failure recovery', '# Durable baseline');
  await page.evaluate(async () => {
    const module = await import('/pkg/inkstone_web.js');
    module.close_local_db();
  });

  await (await openSource(page)).fill('# Retry this Markdown');
  await expect(page.locator('.save-recovery')).toBeVisible();
  await expect(page.locator('.save-recovery')).toContainText('not saved locally');
  await expect(page.locator('.markdown-source-input')).toHaveValue('# Retry this Markdown');
  await expect(page.locator('.topbar-save-status')).toHaveText(/failed/i);
  await expect(page.getByRole('button', { name: 'Retry save' })).toBeVisible();

  await page.getByRole('button', { name: 'Retry save' }).click();
  await persisted(page, 'Retry this Markdown');
  await expect(page.locator('.save-recovery')).toHaveCount(0);
  await page.reload();
  await openNote(page, 'Failure.md', 'Failure recovery');
  await expect(await openSource(page)).toHaveValue('# Retry this Markdown');
  consumeExpectedBrowserErrors(page, /Failed to persist local update/);
});

test('page ink persistence failure is visible and recoverable', async ({ page }) => {
  await createNote(page, 'Ink Failure.md', 'Ink failure recovery', '# Ink failure\n\nAnnotate this line.');
  await page.evaluate(async () => {
    const module = await import('/pkg/inkstone_web.js');
    module.close_local_db();
  });

  await chooseInkTool(page);
  await drawOver(page, page.locator('.page-rendered p'));
  await expect(page.locator('.save-recovery')).toBeVisible();
  await expect(page.locator('.topbar-save-status')).toHaveText(/failed/i);

  await page.getByRole('button', { name: 'Retry save' }).click();
  await expect(page.locator('.save-recovery')).toHaveCount(0);
  await expect.poll(async () => strokeCount(await pageInk(page))).toBe(1);
  await page.reload();
  await openNote(page, 'Ink Failure.md', 'Ink failure recovery');
  await expect.poll(async () => strokeCount(await pageInk(page))).toBe(1);
  expect(anchorTargets(await pageInk(page))).toHaveLength(1);
  consumeExpectedBrowserErrors(page, /Failed to persist local update/);
});

test('blocks render until clicked, then edit as source and return on Escape', async ({ page }) => {
  await createNote(page, 'A.md', 'Alpha', '# Current content\n\nSecond paragraph.');
  await expect(page.locator('.page-source')).toHaveCount(0);
  await expect(page.locator('.page-surface')).not.toContainText('# Current content');
  // Each element is its own block, so only the clicked one turns into source.
  await expect(pageBlocks(page, 'markdown')).toHaveCount(2);

  await page.locator('.page-rendered p').click({ position: { x: 40, y: 10 } });
  const source = page.locator('.page-source');
  await expect(source).toBeFocused();
  await expect(source).toHaveValue('Second paragraph.');
  await expect(page.locator('.page-rendered h1')).toHaveText('Current content');
  // The caret lands where the click was.
  const caret = await source.evaluate(element => element.selectionStart);
  expect(caret).toBeGreaterThan(0);
  expect(caret).toBeLessThan('Second paragraph.'.length);
  await closeSource(page);

  await page.locator('.page-rendered h1').click();
  await expect(source).toHaveValue('# Current content');
  await source.fill('# Latest content');
  await persisted(page, 'Latest content');
  await closeSource(page);
  await expect(page.locator('.page-rendered h1')).toHaveText('Latest content');

  await page.locator('.document-title-input').fill('Latest title');
  await persisted(page, 'Latest title');
  await page.reload();
  await openNote(page, 'A.md', 'Latest title');
  await expect.poll(() => pageMarkdown(page)).toBe('# Latest content\n\nSecond paragraph.');
});

test('Enter starts a block, Backspace joins it back, lists continue, and arrows walk blocks', async ({ page }) => {
  await createNote(page, 'Flow.md', 'Flow', 'Hello world');
  const source = page.locator('.page-source');
  await (await openSource(page)).evaluate(element => element.setSelectionRange(5, 5));
  await page.keyboard.press('Enter');
  await expect(pageBlocks(page, 'markdown')).toHaveCount(2);
  await expect(source).toBeFocused();
  await expect(source).toHaveValue(' world');
  expect(await source.evaluate(element => element.selectionStart)).toBe(0);
  await expect(page.locator('.page-rendered p')).toHaveText('Hello');

  await page.keyboard.press('Backspace');
  await expect(pageBlocks(page, 'markdown')).toHaveCount(1);
  await expect(source).toHaveValue('Hello world');
  expect(await source.evaluate(element => element.selectionStart)).toBe(5);

  await source.evaluate(element => element.setSelectionRange(11, 11));
  await page.keyboard.press('Enter');
  await page.keyboard.type('- one');
  await page.keyboard.press('Enter');
  await page.keyboard.type('two');
  await expect(source).toHaveValue('- one\n- two');
  await page.keyboard.press('Enter');
  await page.keyboard.press('Enter');
  await expect(source).toHaveValue('');
  await page.keyboard.type('After the list');
  await expect(page.locator('.page-rendered li')).toHaveCount(2);

  await page.keyboard.press('ArrowUp');
  await expect(source).toHaveValue('- one\n- two');
  await page.keyboard.press('ArrowDown');
  await expect(source).toHaveValue('After the list');
  await persisted(page, 'After the list');
  await closeSource(page);
  await expect.poll(() => pageMarkdown(page)).toBe('Hello world\n\n- one\n- two\n\nAfter the list');
});

test('a new note starts in its title, and Enter and arrows move between title and page', async ({ page }) => {
  await ensureWorkspaceOpen(page);
  await page.locator('.explorer-new-btn').click();
  await page.locator('.explorer-create-input').fill('Fresh.md');
  await page.locator('.explorer-create-input').press('Enter');
  await expect(page.locator('.breadcrumb')).toHaveText('Fresh.md');
  const title = page.locator('.document-title-input');
  await expect(title).toBeFocused();
  await page.keyboard.type('Fresh thoughts');
  await page.keyboard.press('Enter');
  const source = page.locator('.page-source');
  await expect(source).toBeFocused();
  await page.keyboard.type('First line');
  await page.keyboard.press('ArrowUp');
  await expect(title).toBeFocused();
  await page.keyboard.press('ArrowDown');
  await expect(source).toBeFocused();
  await expect(source).toHaveValue('First line');
  await persisted(page, 'First line');
  await expect.poll(() => pageMarkdown(page)).toBe('First line');
  await expect(title).toHaveValue('Fresh thoughts');
});

test('a title typed the moment a note opens lands in that note, never the previous one', async ({ page }) => {
  await createNote(page, 'Previous.md', 'Previous title', 'Previous body');
  for (const name of ['Quick1', 'Quick2', 'Quick3']) {
    await ensureWorkspaceOpen(page);
    await page.locator('.explorer-new-btn').click();
    await page.locator('.explorer-create-input').fill(`${name}.md`);
    await page.locator('.explorer-create-input').press('Enter');
    await expect(page.locator('.breadcrumb')).toHaveText(`${name}.md`);
    // No waiting for the note to load: type as soon as the title takes input.
    await page.locator('.document-title-input').fill(`${name} title`);
    await expect(page.locator('.document-title-input')).toHaveValue(`${name} title`);
    await persisted(page, `${name} title`);
  }
  const titles = await page.evaluate(async () => {
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
    return values.map(value => JSON.stringify(value));
  });
  for (const title of ['Previous title', 'Quick1 title', 'Quick2 title', 'Quick3 title']) {
    expect(titles.filter(record => record.includes(title))).toHaveLength(1);
  }
  await openNote(page, 'Previous.md', 'Previous title');
  await expect(page.locator('.document-title-input')).toHaveValue('Previous title');
});

test('typing / opens a block menu that filters, follows the keyboard, and turns lines into blocks', async ({ page }) => {
  await createNote(page, 'Slash.md', 'Slash', 'Intro');
  const source = page.locator('.page-source');
  const menu = page.locator('#slash-menu');
  await (await openSource(page)).evaluate(element => element.setSelectionRange(5, 5));
  await page.keyboard.press('Enter');
  await page.keyboard.type('/');
  await expect(menu).toBeVisible();
  await expect(menu.locator('.slash-item').first()).toHaveAttribute('data-command', 'text');
  await page.keyboard.type('head');
  await expect(menu.locator('.slash-item')).toHaveCount(3);
  await page.keyboard.press('ArrowDown');
  await expect(menu.locator('.slash-item.selected')).toHaveAttribute('data-command', 'h2');
  await page.keyboard.press('Enter');
  await expect(menu).toBeHidden();
  await expect(source).toHaveValue('## ');
  await page.keyboard.type('Section');
  await expect(source).toHaveClass(/source-h2/);

  // Escape keeps the slash as text; "and/or" never opens the menu.
  await page.keyboard.press('Enter');
  await page.keyboard.type('and/or /');
  await expect(menu).toBeVisible();
  await page.keyboard.press('Escape');
  await expect(menu).toBeHidden();
  await expect(source).toBeFocused();
  await page.keyboard.type('x');
  await expect(menu).toBeHidden();
  await expect(source).toHaveValue('and/or /x');

  // Block commands insert real blocks; clicking an item works too.
  await page.keyboard.press('Enter');
  await page.keyboard.type('/divi');
  await menu.locator('.slash-item[data-command="divider"]').click();
  await expect(pageBlocks(page, 'divider')).toHaveCount(1);
  await expect(source).toBeFocused();
  await expect(source).toHaveValue('');
  await page.keyboard.type('/math');
  await page.keyboard.press('Enter');
  await expect(page.locator('.page-source[data-block-kind="math"]')).toBeFocused();
  await page.keyboard.type('E = mc^2');
  await persisted(page, 'E = mc^2');
  await closeSource(page);
  await expect.poll(() => pageMarkdown(page)).toBe('Intro\n\n## Section\n\nand/or /x');
  await expect(page.locator('.page-block[data-block-type="math"] math')).toHaveCount(1);
});

test('inline $math$ renders as MathML inside text', async ({ page }) => {
  await createNote(page, 'InlineMath.md', 'Inline math', 'Energy $E = mc^2$ costs $5 or $10.');
  await expect(page.locator('.page-rendered .math-inline math')).toHaveCount(1);
  await expect(page.locator('.page-rendered')).toContainText('costs $5 or $10.');
});

test('checklist boxes tick from the page and undo restores them', async ({ page }) => {
  await createNote(page, 'Todo.md', 'Todo', '- [ ] milk\n- [ ] eggs\n> - [x] quoted');
  const boxes = page.locator('.page-rendered input.task-toggle');
  await expect(boxes).toHaveCount(3);
  await boxes.nth(1).click();
  await expect.poll(() => pageMarkdown(page)).toBe('- [ ] milk\n- [x] eggs\n> - [x] quoted');
  await expect(page.locator('.page-source')).toHaveCount(0);
  await expect(boxes.nth(1)).toBeChecked();
  await boxes.nth(2).click();
  await expect.poll(() => pageMarkdown(page)).toBe('- [ ] milk\n- [x] eggs\n> - [ ] quoted');
  await page.locator('.page-surface').focus();
  await page.keyboard.press('ControlOrMeta+z');
  await expect.poll(() => pageMarkdown(page)).toBe('- [ ] milk\n- [x] eggs\n> - [x] quoted');
  await persisted(page, '- [x] eggs');
  await page.reload();
  await openNote(page, 'Todo.md', 'Todo');
  await expect(page.locator('.page-rendered input.task-toggle').nth(1)).toBeChecked();
});

test('appearance settings restyle the app and survive reload', async ({ page }) => {
  await ensureWorkspaceOpen(page);
  await page.locator('.settings-open-btn').click();
  await page.locator('[data-settings-category="Appearance"]').click();
  for (const [key, value] of [['accent', 'teal'], ['font', 'serif'], ['size', 'l'], ['width', 'wide'], ['paper', 'grid'], ['layout', 'flat']]) {
    await page.locator(`[data-pref-key="${key}"][data-pref-value="${value}"]`).click();
    await expect(page.locator('html')).toHaveAttribute(`data-${key}`, value);
    await expect(page.locator(`[data-pref-key="${key}"][data-pref-value="${value}"]`)).toHaveAttribute('aria-pressed', 'true');
  }
  await page.locator('[data-theme-choice="midnight"]').click();
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'midnight');
  await page.reload();
  await expect(page.locator('html')).toHaveAttribute('data-accent', 'teal');
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'midnight');
  await expect(page.locator('html')).toHaveAttribute('data-size', 'l');
  await ensureWorkspaceOpen(page);
  await page.locator('.settings-open-btn').click();
  await page.locator('[data-settings-category="Appearance"]').click();
  await page.locator('.pref-reset').click();
  await expect(page.locator('html')).toHaveAttribute('data-accent', 'indigo');
  await expect(page.locator('html')).toHaveAttribute('data-layout', 'panels');
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'system');
});

test('blocks reorder by dragging their grip or with Alt+Shift+arrows while typing', async ({ page }, testInfo) => {
  await createNote(page, 'Order.md', 'Order', 'Alpha\n\nBravo\n\nCharlie');
  await expect(pageBlocks(page, 'markdown')).toHaveCount(3);
  const charlie = pageBlocks(page, 'markdown').nth(2);
  await charlie.hover();
  const grip = charlie.locator('.page-grip');
  const box = await grip.boundingBox();
  const first = await pageBlocks(page, 'markdown').first().boundingBox();
  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  await page.mouse.down();
  await page.mouse.move(box.x + box.width / 2, first.y + 4, { steps: 8 });
  await expect(page.locator('.block-drop-line')).toBeVisible();
  await page.mouse.up();
  await expect(page.locator('.block-drop-line')).toHaveCount(0);
  await expect.poll(() => pageMarkdown(page)).toBe('Charlie\n\nAlpha\n\nBravo');

  // A press without a drag is not a move.
  await pageBlocks(page, 'markdown').first().hover();
  await pageBlocks(page, 'markdown').first().locator('.page-grip').click();
  await expect.poll(() => pageMarkdown(page)).toBe('Charlie\n\nAlpha\n\nBravo');

  if (testInfo.project.name !== 'chromium-mobile') {
    const source = await openSource(page, 'markdown', 1);
    await source.evaluate(element => element.setSelectionRange(2, 2));
    await page.keyboard.press('Alt+Shift+ArrowDown');
    await expect(page.locator('.page-source')).toBeFocused();
    await expect(page.locator('.page-source')).toHaveValue('Alpha');
    expect(await page.locator('.page-source').evaluate(element => element.selectionStart)).toBe(2);
    await expect.poll(() => pageMarkdown(page)).toBe('Charlie\n\nBravo\n\nAlpha');
    await page.keyboard.press('Alt+Shift+ArrowUp');
    await page.keyboard.press('Alt+Shift+ArrowUp');
    await expect.poll(() => pageMarkdown(page)).toBe('Alpha\n\nCharlie\n\nBravo');
    await closeSource(page);
  }
  await persisted(page, 'Bravo');
});

test('one undo history covers typing, splitting blocks, and ink', async ({ page }) => {
  await createNote(page, 'Undo.md', 'Undo', 'Alpha');
  const source = await openSource(page);
  await source.evaluate(element => element.setSelectionRange(5, 5));
  await page.keyboard.type(' beta');
  await page.waitForTimeout(1100);
  await page.keyboard.press('Enter');
  await page.waitForTimeout(100);
  await page.keyboard.type('Gamma');
  await expect.poll(() => pageMarkdown(page)).toBe('Alpha beta\n\nGamma');

  await page.keyboard.press('ControlOrMeta+z');
  await expect.poll(() => pageMarkdown(page)).toBe('Alpha beta\n\n');
  await page.keyboard.press('ControlOrMeta+z');
  await expect.poll(() => pageMarkdown(page)).toBe('Alpha beta');
  await expect(page.locator('.page-source')).toBeFocused();
  await expect(page.locator('.page-source')).toHaveValue('Alpha beta');
  await page.keyboard.press('ControlOrMeta+z');
  await expect.poll(() => pageMarkdown(page)).toBe('Alpha');
  await page.keyboard.press('ControlOrMeta+Shift+z');
  await page.keyboard.press('ControlOrMeta+Shift+z');
  await page.keyboard.press('ControlOrMeta+Shift+z');
  await expect.poll(() => pageMarkdown(page)).toBe('Alpha beta\n\nGamma');
  await closeSource(page);

  // A stroke is the next step; the dock and the keyboard share one history.
  await chooseInkTool(page);
  await drawOver(page, page.locator('.page-rendered p', { hasText: 'Gamma' }), { x: 80, dx: 100 });
  await expect.poll(async () => strokeCount(await pageInk(page))).toBe(1);
  await page.getByRole('button', { name: 'Undo', exact: true }).click();
  await expect.poll(async () => strokeCount(await pageInk(page))).toBe(0);
  await page.getByRole('button', { name: 'Undo', exact: true }).click();
  await expect.poll(() => pageMarkdown(page)).toBe('Alpha beta\n\n');
  await page.getByRole('button', { name: 'Redo', exact: true }).click();
  await page.getByRole('button', { name: 'Redo', exact: true }).click();
  await expect.poll(async () => strokeCount(await pageInk(page))).toBe(1);
  await expect.poll(() => pageMarkdown(page)).toBe('Alpha beta\n\nGamma');
  await persisted(page, 'Gamma');
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

test('keyboard journey reaches search, files, inspector tabs, and restored controls', async ({ page }, testInfo) => {
  await createNote(page, 'Keyboard/Alpha.md', 'Keyboard Alpha', '# Keyboard search target');
  await createNote(page, 'Keyboard/Beta.md', 'Keyboard Beta', '# Secondary note');

  if (testInfo.project.name === 'chromium-mobile') {
    await ensureWorkspaceOpen(page);
    await page.locator('.search-input').click();
  } else {
    await page.keyboard.press('Control+K');
  }
  await expect(page.locator('.search-input')).toBeFocused();
  await page.keyboard.type('Keyboard search target');
  await expect(page.locator('.search-result-item[data-path="Keyboard/Alpha.md"]')).toHaveAttribute('aria-selected', 'true');
  await page.keyboard.press('Enter');
  await expect(page.locator('.breadcrumb')).toHaveText('Keyboard/Alpha.md');

  await page.setViewportSize({ width: 900, height: 800 });
  const inspectorToggle = page.getByRole('button', { name: 'Inspector', exact: true });
  await inspectorToggle.focus();
  await page.keyboard.press('Enter');
  await expect(page.locator('.inspector')).toHaveClass(/open/);
  const firstTab = page.locator('.inspector-tab').first();
  await firstTab.focus();
  await page.keyboard.press('ArrowRight');
  await expect(page.locator('.inspector-tab[data-tab="Outline"]')).toBeFocused();
  await expect(page.locator('.inspector-tab[data-tab="Outline"]')).toHaveAttribute('aria-selected', 'true');
  await page.keyboard.press('End');
  await expect(page.locator('.inspector-tab[data-tab="Graph"]')).toBeFocused();
  await page.keyboard.press('Escape');
  await expect(page.locator('.inspector')).not.toHaveClass(/open/);
  await expect(inspectorToggle).toBeFocused();

  await ensureWorkspaceOpen(page);
  const newFile = page.getByRole('button', { name: 'New file', exact: true });
  await newFile.focus();
  await page.keyboard.press('Enter');
  await expect(page.locator('.explorer-create-input')).toBeFocused();
  await page.keyboard.press('Escape');
  await expect(newFile).toBeFocused();
});

test('failed save defers navigation and resumes after retry', async ({ page }) => {
  await createNote(page, 'Race/A.md', 'Race A', '# Baseline A');
  await createNote(page, 'Race/B.md', 'Race B', '# Baseline B');
  await openNote(page, 'Race/A.md', 'Race A');
  await page.evaluate(async () => {
    const module = await import('/pkg/inkstone_web.js');
    module.close_local_db();
  });

  await (await openSource(page)).fill('# Unsaved but retained A');
  await expect(page.locator('.save-recovery')).toBeVisible();
  await ensureWorkspaceOpen(page);
  await page.locator('.file-entry[data-path="Race/B.md"] .file-select-btn').click();
  await expect(page.locator('.breadcrumb')).toHaveText('Race/A.md');
  await expect(page.locator('.topbar-save-status')).toContainText('retry before leaving');

  await page.getByRole('button', { name: 'Retry save' }).click();
  await expect(page.locator('.breadcrumb')).toHaveText('Race/B.md');
  await page.reload();
  await openNote(page, 'Race/A.md', 'Race A');
  await expect(await openSource(page)).toHaveValue('# Unsaved but retained A');
  consumeExpectedBrowserErrors(page, /Failed to persist local update/);
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
  await createNote(page, 'A.md', 'Alpha', '# Alpha\n\nBody');
  await chooseInkTool(page);
  await drawOver(page, page.locator('.page-rendered p'));
  await expect.poll(async () => strokeCount(await pageInk(page))).toBe(1);
  await chooseTyping(page);
  await openSource(page);
  await closeSource(page);
  assertClean();
});

test('workspace fits the configured viewport', async ({ page }, testInfo) => {
  await createNote(page, 'A.md', 'Alpha', '# Alpha');
  await expect(page.locator('.app-container')).toBeVisible();
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);
  expect(overflow, `${testInfo.project.name} horizontal overflow`).toBeLessThanOrEqual(0);
  // The inspector is a panel the reader opens, at every width.
  await expect(page.locator('.inspector')).toBeHidden();
  await page.locator('.mode-bar .toggle-inspector-btn').click();
  await expect(page.locator('.inspector')).toHaveClass(/open/);
  await expect(page.locator('.inspector')).toBeVisible();
  await page.locator('.inspector-tab[data-tab="Tags"]').click();
  await page.locator('.inspector-close-btn').click();
  await expect(page.locator('.inspector')).not.toHaveClass(/open/);

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

test('workspace remains usable across the release viewport matrix', async ({ page }) => {
  await createNote(page, 'Viewport.md', 'Viewport proof', '# Responsive workspace');
  for (const viewport of [
    { width: 320, height: 568 },
    { width: 768, height: 1024 },
    { width: 1280, height: 720 },
    { width: 1440, height: 900 }
  ]) {
    await page.setViewportSize(viewport);
    await expect(page.locator('.document-title-input')).toBeVisible();
    const dimensions = await page.evaluate(() => ({
      documentOverflow: document.documentElement.scrollWidth - window.innerWidth,
      bodyOverflow: document.body.scrollWidth - window.innerWidth
    }));
    expect(dimensions.documentOverflow, `${viewport.width}x${viewport.height} document overflow`).toBeLessThanOrEqual(0);
    expect(dimensions.bodyOverflow, `${viewport.width}x${viewport.height} body overflow`).toBeLessThanOrEqual(0);
  }
});

test('100-note workspace remains searchable and navigable', async ({ page }) => {
  await createStressWorkspace(page);
  await page.reload();
  await ensureWorkspaceOpen(page);
  await page.locator('.search-input').fill('token-073');
  const result = page.locator('.search-result-item[data-path="Stress/Note 073.md"]');
  await expect(result).toBeVisible();
  await result.click();
  await expect(page.locator('.document-title-input')).toHaveValue('Stress Note 073');
  await expect(page.locator('.page-surface')).toContainText('token-073');
});

test('soak iteration preserves the latest edit through rapid navigation', async ({ page }) => {
  await createNote(page, 'Soak/A.md', 'Soak A', '# Initial A');
  await createNote(page, 'Soak/B.md', 'Soak B', '# Initial B');
  await openNote(page, 'Soak/A.md', 'Soak A');
  await (await openSource(page)).fill('# Latest A');
  await persisted(page, 'Latest A');
  await openNote(page, 'Soak/B.md', 'Soak B');
  await openNote(page, 'Soak/A.md', 'Soak A');
  await expect(await openSource(page)).toHaveValue('# Latest A');
});

test('creates and reloads Markdown, Math, writing space, and Divider blocks', async ({ page }) => {
  await createNote(page, 'Mixed.md', 'Mixed notebook', '# Markdown block');
  await addBlock(page, 'markdown');
  await expect(page.locator('.page-source')).toBeFocused();
  await page.locator('.page-source').fill('Second Markdown block');
  await persisted(page, 'Second Markdown block');
  await closeSource(page);
  await addBlock(page, 'math');
  await page.locator('.math-source-input').fill('P(A|B) = P(B|A)P(A) / P(B)');
  await persisted(page, 'P(A|B)');
  await closeSource(page);
  await addBlock(page, 'space');
  await addBlock(page, 'divider');

  const expected = ['markdown', 'markdown', 'math', 'space', 'divider'];
  await expect(pageBlocks(page)).toHaveCount(expected.length);
  await page.reload();
  await openNote(page, 'Mixed.md', 'Mixed notebook');
  const blocks = pageBlocks(page);
  await expect(blocks).toHaveCount(expected.length);
  for (const [index, type] of expected.entries()) {
    await expect(blocks.nth(index)).toHaveAttribute('data-block-type', type);
  }
  await expect(page.locator('.page-math mfrac, .page-math math')).not.toHaveCount(0);
  await expect(page.locator('.page-divider .document-divider')).toBeVisible();
  await expect(await openSource(page, 'math')).toHaveValue('P(A|B) = P(B|A)P(A) / P(B)');
});

test('the page is a semantic reading surface with no source until editing', async ({ page }) => {
  await preparePageNotebook(page);

  await expect(page.locator('.page-source')).toHaveCount(0);
  await expect(page.locator('.block-wrapper')).toHaveCount(0);
  await expect(page.locator('.mode-toggle-btn[data-mode="preview"]')).toHaveCount(0);
  await expect(page.getByRole('heading', { name: 'Posterior update', exact: true })).toBeVisible();
  await expect(page.locator('.page-markdown ul li')).toHaveCount(3);
  await expect(page.getByRole('link', { name: 'prior reference' })).toHaveAttribute('href', 'https://example.com/prior');
  await expect(page.locator('.page-markdown table')).toBeVisible();
  await expect(page.locator('.page-markdown blockquote')).toBeVisible();
  await expect(page.locator('.page-surface')).not.toContainText('# Posterior update');
  await expect(page.locator('.page-surface')).not.toContainText('- Prior probability');
  await expect(page.locator('.page-math math')).toBeVisible();
  await expect(page.locator('.page-math mfrac')).toBeVisible();
  await expect(page.locator('.page-divider .document-divider')).toBeVisible();
  await expect(page.locator('[data-sourcepos]').first()).toBeAttached();
});

test('ink written over text is anchored to it and follows it as text is added', async ({ page }) => {
  await createNote(page, 'Anchors.md', 'Anchored ink', '# Lecture\n\nFirst idea.\n\nThe derivative of momentum gives force.\n\nLast idea.');
  const target = page.locator('.page-rendered p', { hasText: 'The derivative of momentum' });
  await chooseInkTool(page);
  await expect(page.locator('#page-ink-canvas')).toHaveCSS('position', 'fixed');
  const emptyPixels = await canvasPixels(page);
  await drawOver(page, target, { x: -10, dx: 300 });
  await expect.poll(async () => strokeCount(await pageInk(page))).toBe(1);
  await expect.poll(() => canvasPixels(page)).toBeGreaterThan(emptyPixels);
  const [anchor] = anchorTargets(await pageInk(page));
  expect(anchor.kind).toBe('text');

  // A second nearby stroke joins the same group; a far one starts another.
  await drawOver(page, target, { x: 320, dx: 30 });
  await expect.poll(async () => strokeCount(await pageInk(page))).toBe(2);
  expect(anchorTargets(await pageInk(page))).toHaveLength(1);

  // Undo and redo go through the dock.
  await page.getByRole('button', { name: 'Undo', exact: true }).click();
  await expect.poll(async () => strokeCount(await pageInk(page))).toBe(1);
  await page.getByRole('button', { name: 'Redo', exact: true }).click();
  await expect.poll(async () => strokeCount(await pageInk(page))).toBe(2);

  // Typing above the annotated paragraph moves the ink with it.
  const { y: oldTop } = await target.boundingBox();
  const pixelsBefore = await page.locator('#page-ink-canvas').evaluate(canvas => canvas.toDataURL());
  await chooseTyping(page);
  const source = await openSource(page);
  await source.evaluate(element => element.setSelectionRange(0, 0));
  await page.keyboard.type('Inserted line one.\nInserted line two.\n');
  await closeSource(page);
  await expect.poll(async () => (await target.boundingBox())?.y ?? 0).toBeGreaterThan(oldTop + 40);
  await expect.poll(() => page.locator('#page-ink-canvas').evaluate(canvas => canvas.toDataURL())).not.toBe(pixelsBefore);
  expect(anchorTargets(await pageInk(page))).toEqual([anchor]);

  await page.reload();
  await openNote(page, 'Anchors.md', 'Anchored ink');
  await expect.poll(async () => strokeCount(await pageInk(page))).toBe(2);
  await expect.poll(() => canvasPixels(page)).toBeGreaterThan(0);
});

test('ink drawn across a wide page still fits on a phone', async ({ page }, testInfo) => {
  test.skip(testInfo.project.name !== 'chromium-desktop', 'starts from a laptop-sized page');
  await createNote(page, 'Wide.md', 'Wide ink', 'A line to write beside.');
  const target = page.locator('.page-rendered p', { hasText: 'A line to write beside' });
  await chooseInkTool(page);
  const surface = await page.locator('.page-surface').boundingBox();
  await drawOver(page, target, { x: 4, dx: surface.width - 12 });
  await expect.poll(async () => strokeCount(await pageInk(page))).toBe(1);
  await expect.poll(async () => (await inkSpan(page))?.right ?? 0).toBeGreaterThan(surface.x + surface.width - 40);

  await page.setViewportSize({ width: 390, height: 844 });
  await expect.poll(async () => {
    const narrow = await page.locator('.page-surface').boundingBox();
    const span = await inkSpan(page);
    return narrow !== null && span !== null && span.left >= narrow.x - 8 && span.right <= narrow.x + narrow.width + 8;
  }).toBe(true);
});

test('ink written in the margin beside a line stays in the margin', async ({ page }, testInfo) => {
  test.skip(testInfo.project.name !== 'chromium-desktop', 'needs room beside the column');
  await createNote(page, 'Margin.md', 'Margin ink', 'A line with a note beside it.');
  const target = page.locator('.page-rendered p', { hasText: 'A line with a note beside it' });
  await chooseInkTool(page);
  await drawOver(page, target, { x: -115, dx: 70 });
  await expect.poll(async () => strokeCount(await pageInk(page))).toBe(1);
  expect(anchorTargets(await pageInk(page))[0].kind).toBe('text');
  await page.waitForTimeout(800);
  const surface = await page.locator('.page-surface').boundingBox();
  const span = await inkSpan(page);
  expect(span).not.toBeNull();
  expect(span.right).toBeLessThan(surface.x - 20);
  expect(span.left).toBeGreaterThan(surface.x - 135);
});

test('writing far below the text keeps the page where it is scrolled', async ({ page }, testInfo) => {
  test.skip(testInfo.project.name !== 'chromium-desktop', 'one viewport is enough');
  await createNote(page, 'Below.md', 'Below the text', 'One short line.');
  const picture = await page.evaluate(async () => {
    const canvas = document.createElement('canvas');
    canvas.width = 600;
    canvas.height = 800;
    const context = canvas.getContext('2d');
    context.fillStyle = '#3a7bd5';
    context.fillRect(0, 0, 600, 800);
    const blob = await new Promise(resolve => canvas.toBlob(resolve, 'image/png'));
    return Array.from(new Uint8Array(await blob.arrayBuffer()));
  });
  const chooser = page.waitForEvent('filechooser');
  await page.locator('.add-block-btn[data-block-type="image"]').click();
  await (await chooser).setFiles({ name: 'tall.png', mimeType: 'image/png', buffer: Buffer.from(picture) });
  await expect(page.locator('.page-surface img[data-asset-path]')).toHaveCount(1);
  await expect.poll(() => page.locator('.page-surface img[data-asset-path]').evaluate(image => image.complete && image.naturalWidth > 0)).toBe(true);
  await chooseInkTool(page);
  const scroller = page.locator('.main-content');
  const viewport = await scroller.boundingBox();
  const stroke = async () => {
    const surface = await page.locator('.page-surface').boundingBox();
    const y = viewport.y + viewport.height - 120;
    await page.mouse.move(surface.x + 80, y);
    await page.mouse.down();
    await page.mouse.move(surface.x + 160, y + 20, { steps: 6 });
    await page.mouse.move(surface.x + 240, y - 10, { steps: 6 });
    await page.mouse.up();
  };
  for (let round = 1; round <= 4; round += 1) {
    await stroke();
    await expect.poll(async () => strokeCount(await pageInk(page))).toBe(round);
    await scroller.evaluate(element => element.scrollTo(0, element.scrollHeight));
    await page.waitForTimeout(200);
  }
  const top = await scroller.evaluate(element => element.scrollTop);
  expect(top).toBeGreaterThan(300);
  await stroke();
  await expect.poll(async () => strokeCount(await pageInk(page))).toBe(5);
  await saved(page);
  await chooseTyping(page);
  await chooseInkTool(page);
  await page.waitForTimeout(300);
  expect(Math.abs((await scroller.evaluate(element => element.scrollTop)) - top)).toBeLessThan(4);
});

test('typing below a tall picture never moves the page', async ({ page }, testInfo) => {
  test.skip(testInfo.project.name !== 'chromium-desktop', 'one viewport is enough');
  await createNote(page, 'Typing below.md', 'Typing below', 'First line.');
  const picture = await page.evaluate(async () => {
    const canvas = document.createElement('canvas');
    canvas.width = 600;
    canvas.height = 1400;
    canvas.getContext('2d').fillRect(0, 0, 600, 1400);
    const blob = await new Promise(resolve => canvas.toBlob(resolve, 'image/png'));
    return Array.from(new Uint8Array(await blob.arrayBuffer()));
  });
  const chooser = page.waitForEvent('filechooser');
  await page.locator('.add-block-btn[data-block-type="image"]').click();
  await (await chooser).setFiles({ name: 'tall.png', mimeType: 'image/png', buffer: Buffer.from(picture) });
  await expect.poll(() => page.locator('.page-surface img[data-asset-path]').evaluate(image => image.complete && image.naturalWidth > 0)).toBe(true);
  await page.locator('.page-tail').click({ position: { x: 20, y: 10 } });
  const source = page.locator('.page-source:focus');
  await expect(source).toHaveCount(1);
  await page.keyboard.type('line one', { delay: 30 });
  await page.keyboard.press('Enter');
  await page.waitForTimeout(500);
  await page.evaluate(() => {
    window.__scrollSamples = [];
    window.__scrollTimer = setInterval(() => window.__scrollSamples.push(document.querySelector('.main-content').scrollTop), 10);
  });
  await page.keyboard.type('hahahahaha hahahaha', { delay: 40 });
  await page.keyboard.press('Enter');
  await page.keyboard.type('another line here', { delay: 40 });
  await page.waitForTimeout(800);
  const samples = await page.evaluate(() => {
    clearInterval(window.__scrollTimer);
    return window.__scrollSamples;
  });
  expect(Math.min(...samples)).toBeGreaterThan(200);
  expect(Math.max(...samples) - Math.min(...samples)).toBeLessThan(80);
});

test('a circle drawn around a word stays on that word on a phone', async ({ page }, testInfo) => {
  test.skip(testInfo.project.name !== 'chromium-desktop', 'starts from a laptop-sized page');
  const paragraph = 'Alpha bravo charlie delta echo foxtrot golf hotel india juliet kilo lima mike november oscar papa quebec romeo sierra tango uniform victor whiskey xray yankee zulu and then a few more words.';
  await createNote(page, 'Circle.md', 'Circled word', `# Notes\n\nIntro line.\n\n${paragraph}`);
  await chooseInkTool(page);
  await page.locator('.page-rendered p', { hasText: 'Alpha bravo' }).evaluate(element => element.scrollIntoView({ block: 'center' }));
  await circleAround(page, await wordBox(page, 'quebec'));
  await expect.poll(async () => strokeCount(await pageInk(page))).toBe(1);
  expect(anchorTargets(await pageInk(page))[0].kind).toBe('text');

  await page.setViewportSize({ width: 390, height: 844 });
  await page.locator('.page-rendered p', { hasText: 'Alpha bravo' }).evaluate(element => element.scrollIntoView({ block: 'center' }));
  await expect.poll(async () => {
    const word = await wordBox(page, 'quebec');
    const ink = await inkBox(page);
    if (!word || !ink) return Infinity;
    const dx = (ink.left + ink.right) / 2 - (word.left + word.right) / 2;
    const dy = (ink.top + ink.bottom) / 2 - (word.top + word.bottom) / 2;
    return Math.hypot(dx, dy);
  }).toBeLessThan(12);
});

test('writing wider than the page column never shrinks earlier ink', async ({ page }, testInfo) => {
  test.skip(testInfo.project.name !== 'chromium-desktop', 'needs room beside the column');
  await createNote(page, 'Wide writing.md', 'Wide writing', 'Write, type / for blocks, or draw anywhere with a pen.');
  await chooseInkTool(page);
  const surface = await page.locator('.page-surface').boundingBox();
  const y = surface.y + surface.height + 40;
  const scribble = async (x, top, width) => {
    await page.mouse.move(x, top);
    await page.mouse.down();
    for (let step = 1; step <= 12; step += 1) await page.mouse.move(x + (width * step) / 12, top + (step % 2) * 14);
    await page.mouse.up();
  };
  const rowsWidth = (top, bottom) => page.locator('#page-ink-canvas').evaluate((canvas, [top, bottom]) => {
    const rect = canvas.getBoundingClientRect();
    const ratio = canvas.width / rect.width;
    const from = Math.max(0, Math.floor((top - rect.top) * ratio));
    const to = Math.min(canvas.height, Math.ceil((bottom - rect.top) * ratio));
    const data = canvas.getContext('2d').getImageData(0, from, canvas.width, to - from).data;
    let left = Infinity;
    let right = -Infinity;
    for (let index = 3; index < data.length; index += 4) {
      if (data[index] === 0) continue;
      const x = ((index - 3) / 4) % canvas.width;
      left = Math.min(left, x);
      right = Math.max(right, x);
    }
    return right < left ? 0 : (right - left) / ratio;
  }, [top, bottom]);
  await scribble(surface.x + 120, y, 80);
  await expect.poll(async () => strokeCount(await pageInk(page))).toBe(1);
  await expect.poll(() => rowsWidth(y - 10, y + 24)).toBeGreaterThan(70);
  const before = await rowsWidth(y - 10, y + 24);
  await scribble(surface.x - 140, y + 60, surface.width + 260);
  await expect.poll(async () => strokeCount(await pageInk(page))).toBe(2);
  await page.waitForTimeout(300);
  expect(await rowsWidth(y + 50, y + 84)).toBeGreaterThan(surface.width + 200);
  expect(Math.abs((await rowsWidth(y - 10, y + 24)) - before)).toBeLessThan(3);
});

test('a long scribble keeps drawing and is saved', async ({ page }, testInfo) => {
  test.skip(testInfo.project.name !== 'chromium-desktop', 'one viewport is enough');
  test.setTimeout(90_000);
  await createNote(page, 'Scribble.md', 'Scribble', 'Scribble here.');
  await chooseInkTool(page);
  const box = await page.locator('.page-rendered p').boundingBox();
  const x = box.x + 20;
  const y = box.y + box.height / 2;
  await page.mouse.move(x, y);
  await page.mouse.down();
  for (let step = 0; step < 80; step += 1) {
    await page.mouse.move(x + (step % 2) * 240, y + (step % 7) * 6);
  }
  await page.mouse.up();
  await expect.poll(async () => strokeCount(await pageInk(page))).toBeGreaterThan(1);
  await page.waitForTimeout(300);
  const scribbled = strokeCount(await pageInk(page));
  await drawOver(page, page.locator('.page-rendered p'), { x: 20, dx: 120 });
  await expect.poll(async () => strokeCount(await pageInk(page))).toBe(scribbled + 1);
});

test('a finger draws on a touch phone and two fingers scroll the page', async ({ browser }, testInfo) => {
  test.skip(testInfo.project.name !== 'chromium-desktop', 'uses its own touch context');
  const context = await browser.newContext({ viewport: { width: 412, height: 860 }, hasTouch: true, isMobile: true });
  const phone = await context.newPage();
  await resetWorkspace(phone);
  const lines = Array.from({ length: 40 }, (_, index) => `Line number ${index + 1} of a long note.`).join('\n\n');
  await createNote(phone, 'Touch.md', 'Touch ink', lines);
  await chooseInkTool(phone);
  const cdp = await context.newCDPSession(phone);
  const touch = (type, points) => cdp.send('Input.dispatchTouchEvent', {
    type,
    touchPoints: points.map(([x, y], id) => ({ x, y, id }))
  });
  const firstLine = phone.locator('.page-rendered p', { hasText: /^Line number 3 of/ });
  await firstLine.evaluate(element => element.scrollIntoView({ block: 'center' }));
  const box = await firstLine.boundingBox();
  const startX = box.x + 30;
  const startY = box.y + box.height / 2;
  await touch('touchStart', [[startX, startY]]);
  for (let step = 1; step <= 12; step += 1) await touch('touchMove', [[startX + step * 12, startY + (step % 2) * 8]]);
  await touch('touchEnd', []);
  await expect.poll(async () => strokeCount(await pageInk(phone))).toBe(1);

  const scroller = phone.locator('.main-content');
  const before = await scroller.evaluate(element => element.scrollTop);
  await touch('touchStart', [[150, 600], [250, 600]]);
  for (let step = 1; step <= 10; step += 1) await touch('touchMove', [[150, 600 - step * 30], [250, 600 - step * 30]]);
  await touch('touchEnd', []);
  await expect.poll(() => scroller.evaluate(element => element.scrollTop)).toBeGreaterThan(before + 200);
  expect(strokeCount(await pageInk(phone))).toBe(1);
  expect(phone.__browserErrors).toEqual([]);
  await context.close();
});

test('the style panel sets ink color and a variable size that persists', async ({ page }) => {
  await createNote(page, 'Style.md', 'Style', 'Write beside this line.');
  await chooseInkTool(page);
  await page.locator('.ink-dock-style').click();
  await expect(page.locator('.ink-panel')).toBeVisible();
  await page.locator('.ink-panel-color[data-color="124,58,237"]').click();
  await expect(page.locator('.ink-panel-color[data-color="124,58,237"]')).toHaveClass(/active/);
  await page.locator('.ink-size-slider').fill('12');
  await expect(page.locator('.ink-size-value')).toHaveText('12');
  await drawOver(page, page.locator('.page-rendered p'));
  await expect(page.locator('.ink-panel')).toHaveCount(0);
  await expect.poll(async () => strokeCount(await pageInk(page))).toBe(1);
  const json = (await pageInk(page)).stonepen_json;
  expect(json).toMatch(/"base_w":\s*12(\.0)?[,}]/);
  await page.reload();
  await openNote(page, 'Style.md', 'Style');
  await chooseInkTool(page);
  await page.locator('.ink-dock-style').click();
  await expect(page.locator('.ink-size-value')).toHaveText('12');
  await expect(page.locator('.ink-panel-color[data-color="124,58,237"]')).toHaveClass(/active/);
});

test('ink beside a line follows it while the block is still being typed in', async ({ page }) => {
  await createNote(page, 'LiveAnchors.md', 'Live anchors', '- alpha\n- beta\n- gamma');
  await chooseInkTool(page);
  await drawOver(page, page.locator('.page-rendered li', { hasText: 'gamma' }), { x: 80, dx: 120 });
  await expect.poll(async () => strokeCount(await pageInk(page))).toBe(1);
  expect(anchorTargets(await pageInk(page))[0].kind).toBe('text');
  await chooseTyping(page);
  const before = await inkTop(page);
  expect(before).toBeGreaterThan(0);

  const source = await openSource(page);
  await source.evaluate(element => element.setSelectionRange(7, 7));
  const opened = await inkTop(page);
  await page.keyboard.press('Enter');
  await page.keyboard.type('new item');
  await expect(source).toHaveValue('- alpha\n- new item\n- beta\n- gamma');
  // Still editing, and the ink has moved down one line with "gamma".
  await expect(source).toBeFocused();
  await expect.poll(() => inkTop(page)).toBeGreaterThan(opened + 15);
  await page.keyboard.press('Backspace');
  for (let index = 0; index < 'new item'.length + 2; index += 1) await page.keyboard.press('Backspace');
  await expect(source).toHaveValue('- alpha\n- beta\n- gamma');
  await expect.poll(() => inkTop(page)).toBeLessThan(opened + 5);
});

test('a mouse only types until an ink tool is chosen', async ({ page }) => {
  await createNote(page, 'Modes.md', 'Modes', '# Modes\n\nClick me to edit.');
  await drawOver(page, page.locator('.page-rendered p'));
  await expect(page.locator('.page-source')).toBeVisible();
  expect(strokeCount(await pageInk(page))).toBe(0);
  await closeSource(page);

  await chooseInkTool(page);
  await drawOver(page, page.locator('.page-rendered p'));
  await expect.poll(async () => strokeCount(await pageInk(page))).toBe(1);
  await expect(page.locator('.page-source')).toHaveCount(0);

  await chooseInkTool(page, 'eraser');
  await drawOver(page, page.locator('.page-rendered p'), { x: 0, dx: 260 });
  await expect.poll(async () => strokeCount(await pageInk(page))).toBe(0);
});

test('a tablet pen clicks and types in Type mode and only draws with an ink tool', async ({ page }, testInfo) => {
  test.skip(testInfo.project.name !== 'chromium-desktop', 'a desktop drawing tablet');
  await createNote(page, 'Tablet.md', 'Tablet', '# Tablet\n\nClick me with the pen.');
  const cdp = await page.context().newCDPSession(page);
  const pen = (type, x, y, buttons = 1) => cdp.send('Input.dispatchMouseEvent', {
    type, x, y, button: type === 'mouseMoved' && !buttons ? 'none' : 'left', buttons, clickCount: 1, pointerType: 'pen', force: 0.5
  });
  const penStroke = async target => {
    await target.evaluate(element => element.scrollIntoView({ block: 'center' }));
    const box = await target.boundingBox();
    const x = box.x + 20;
    const y = box.y + box.height / 2;
    await pen('mouseMoved', x, y, 0);
    await pen('mousePressed', x, y);
    for (let step = 1; step <= 10; step += 1) await pen('mouseMoved', x + step * 14, y + (step % 2) * 8);
    await pen('mouseReleased', x + 140, y);
  };

  await penStroke(page.locator('.page-rendered p', { hasText: 'Click me' }));
  await expect(page.locator('.page-source')).toBeVisible();
  expect(strokeCount(await pageInk(page))).toBe(0);
  await closeSource(page);

  const penButton = page.locator('.ink-dock [data-page-ink="tool"][data-tool="pen"]');
  const buttonBox = await penButton.boundingBox();
  const bx = buttonBox.x + buttonBox.width / 2;
  const by = buttonBox.y + buttonBox.height / 2;
  await pen('mouseMoved', bx, by, 0);
  await pen('mousePressed', bx, by);
  await pen('mouseReleased', bx, by);
  await expect(page.locator('.page-surface')).toHaveClass(/ink-mode/);
  await penStroke(page.locator('.page-rendered p', { hasText: 'Click me' }));
  await expect.poll(async () => strokeCount(await pageInk(page))).toBe(1);

  await chooseTyping(page);
  await penStroke(page.locator('.page-rendered p', { hasText: 'Click me' }));
  await expect(page.locator('.page-source')).toBeVisible();
  expect(strokeCount(await pageInk(page))).toBe(1);
});

test('pen and highlighter keep their own color and size', async ({ page }) => {
  await createNote(page, 'Brushes.md', 'Brushes', 'Some text.');
  const panel = page.locator('.ink-panel');
  const setBrush = async (tool, color, size) => {
    await chooseInkTool(page, tool);
    await page.locator(`.ink-dock [data-page-ink="tool"][data-tool="${tool}"]`).click();
    await expect(panel).toBeVisible();
    await panel.locator(`[data-color="${color}"]`).click();
    await panel.locator('.ink-size-slider').fill(String(size));
    await expect(panel.locator('.ink-size-value')).toHaveText(String(size));
  };
  const expectBrush = async (tool, color, size) => {
    await chooseInkTool(page, tool);
    await page.locator(`.ink-dock [data-page-ink="tool"][data-tool="${tool}"]`).click();
    await expect(panel.locator('.ink-size-value')).toHaveText(String(size));
    await expect(panel.locator(`[data-color="${color}"]`)).toHaveAttribute('aria-pressed', 'true');
  };
  await setBrush('pen', '37,99,235', 6);
  await expectBrush('highlighter', '234,179,8', 14);
  await setBrush('highlighter', '22,163,74', 20);
  await expectBrush('pen', '37,99,235', 6);
  await page.reload();
  await openNote(page, 'Brushes.md', 'Brushes');
  await expectBrush('highlighter', '22,163,74', 20);
  await expectBrush('pen', '37,99,235', 6);
});

test('images and PDF pages stay on screen while the page redraws', async ({ page }) => {
  await createNote(page, 'Steady.md', 'Steady', 'Above the picture.');
  const chooser = page.waitForEvent('filechooser');
  await page.locator('.add-block-btn[data-block-type="image"]').last().click();
  await (await chooser).setFiles({ name: 'paper.pdf', mimeType: 'application/pdf', buffer: makePdf(['Page one heading']) });
  await expect(page.locator('.pdf-pages .pdf-page img')).toHaveCount(1, { timeout: 15000 });
  await page.evaluate(async () => {
    const canvas = document.createElement('canvas');
    canvas.width = 300;
    canvas.height = 200;
    canvas.getContext('2d').fillRect(0, 0, 300, 200);
    const blob = await new Promise(resolve => canvas.toBlob(resolve, 'image/png'));
    const transfer = new DataTransfer();
    transfer.items.add(new File([blob], 'image.png', { type: 'image/png' }));
    window.dispatchEvent(new ClipboardEvent('paste', { clipboardData: transfer, bubbles: true }));
  });
  const picture = page.locator('.page-rendered img[data-asset-path]');
  await expect.poll(() => picture.first().evaluate(image => image.complete && image.naturalWidth === 300).catch(() => false)).toBe(true);
  await page.evaluate(() => {
    window.__blank = 0;
    window.__swaps = 0;
    window.__watching = true;
    const check = () => {
      for (const image of document.querySelectorAll('.page-surface img')) {
        if (!image.getAttribute('src') || (image.complete && image.naturalWidth === 0)) window.__blank += 1;
      }
      if (window.__watching) requestAnimationFrame(check);
    };
    requestAnimationFrame(check);
    new MutationObserver(records => { window.__swaps += records.filter(record => record.oldValue && record.target.isConnected).length; })
      .observe(document.body, { subtree: true, attributes: true, attributeOldValue: true, attributeFilter: ['src'] });
  });
  for (let i = 0; i < 3; i++) {
    await chooseInkTool(page);
    await chooseTyping(page);
  }
  await page.locator('.document-title-input').fill('Steady still');
  await page.waitForTimeout(500);
  const seen = await page.evaluate(() => { window.__watching = false; return { blank: window.__blank, swaps: window.__swaps }; });
  expect(seen).toEqual({ blank: 0, swaps: 0 });
});

test('images are pasted, dropped, or picked into a note and stay after reload', async ({ page }, testInfo) => {
  await createNote(page, 'Pictures.md', 'Pictures', 'First line.\n\nSecond line.');
  const makeFiles = names => page.evaluate(async list => {
    const canvas = document.createElement('canvas');
    canvas.width = 60;
    canvas.height = 40;
    const context = canvas.getContext('2d');
    context.fillStyle = '#d33';
    context.fillRect(0, 0, 60, 40);
    const blob = await new Promise(resolve => canvas.toBlob(resolve, 'image/png'));
    const transfer = new DataTransfer();
    for (const name of list) transfer.items.add(new File([blob], name, { type: 'image/png' }));
    window.__transfer = transfer;
  }, names);
  const images = page.locator('.page-rendered img[data-asset-path]');
  const loaded = async index => images.nth(index).evaluate(image => image.complete && image.naturalWidth === 60 && image.src.startsWith('blob:'));

  await makeFiles(['image.png']);
  await page.evaluate(() => window.dispatchEvent(new ClipboardEvent('paste', { clipboardData: window.__transfer, bubbles: true })));
  await expect(images).toHaveCount(1);
  await expect(images.first()).toHaveAttribute('data-asset-path', /^Assets\/Pasted image .*\.png$/);
  await expect.poll(() => loaded(0)).toBe(true);
  expect(await pageMarkdown(page)).toMatch(/Second line\.\n\n!\[Pasted image [^\]]*\]\(Assets\/Pasted%20image%20.*\.png\)/);

  await makeFiles(['diagram.png']);
  await page.locator('.page-rendered p', { hasText: 'First line' }).evaluate(target => {
    const init = { dataTransfer: window.__transfer, bubbles: true, cancelable: true };
    target.dispatchEvent(new DragEvent('dragover', init));
    target.dispatchEvent(new DragEvent('drop', init));
  });
  await expect(images).toHaveCount(2);
  await expect(images.first()).toHaveAttribute('data-asset-path', 'Assets/diagram.png');
  await expect.poll(() => loaded(0)).toBe(true);

  const chooser = page.waitForEvent('filechooser');
  await page.locator('.add-block-btn[data-block-type="image"]').click();
  const picked = await page.evaluate(async () => {
    const file = window.__transfer.files[0];
    return Array.from(new Uint8Array(await file.arrayBuffer()));
  });
  await (await chooser).setFiles({ name: 'diagram.png', mimeType: 'image/png', buffer: Buffer.from(picked) });
  await expect(images).toHaveCount(3);
  await expect(images.last()).toHaveAttribute('data-asset-path', 'Assets/diagram (2).png');

  await chooseInkTool(page);
  await drawOver(page, images.first(), { x: 5, dx: 50 });
  await expect.poll(async () => strokeCount(await pageInk(page))).toBe(1);

  await page.reload();
  await openNote(page, 'Pictures.md', 'Pictures');
  await expect(images).toHaveCount(3);
  for (const index of [0, 1, 2]) await expect.poll(() => loaded(index)).toBe(true);
  expect(page.__browserErrors).toEqual([]);
});

test('writing spaces hold free-form ink and deleting a block keeps its ink', async ({ page }) => {
  await createNote(page, 'Space.md', 'Space', '# Space\n\nAbove the sketch.');
  await addBlock(page, 'space');
  await addBlock(page, 'markdown');
  await page.locator('.page-source').fill('Below the sketch.');
  await persisted(page, 'Below the sketch.');
  await closeSource(page);

  await chooseInkTool(page);
  await drawOver(page, pageBlocks(page, 'space'), { x: 80, y: 0.5, dx: 200 });
  await expect.poll(async () => strokeCount(await pageInk(page))).toBe(1);
  const [target] = anchorTargets(await pageInk(page));
  expect(target.kind).toBe('block');

  await chooseTyping(page);
  page.once('dialog', dialog => dialog.accept());
  const space = pageBlocks(page, 'space');
  await space.hover();
  await space.locator('[data-block-action="delete"]').click();
  await expect(pageBlocks(page, 'space')).toHaveCount(0);
  await expect.poll(async () => anchorTargets(await pageInk(page))[0]?.block_id).not.toBe(target.block_id);
  expect(strokeCount(await pageInk(page))).toBe(1);
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
  await expect(page.locator('.page-rendered h2#details')).toHaveCount(1);
  await expect(page.locator('.page-rendered h2#details-2')).toHaveCount(1);
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

test('populated research notebook is durable and composed', async ({ page }, testInfo) => {
  test.setTimeout(90_000);
  await prepareResearchWorkspace(page);
  const isMobile = testInfo.project.name === 'chromium-mobile';
  await page.setViewportSize(isMobile ? { width: 420, height: 800 } : { width: 1440, height: 900 });

  await ensureWorkspaceOpen(page);
  await page.locator('.explorer-folder[data-path="Teaching"]').click();
  await expect(page.locator('.file-entry[data-path="Research/Probability Axioms.md"]')).toBeAttached();
  await expect(page.locator('.file-entry[data-path="Teaching/Bayesian Statistics.md"]')).toBeAttached();
  await expect(page.locator('.document-title-input')).toHaveValue('Bayes’ Rule Derivation');
  await expect.poll(async () => strokeCount(await pageInk(page))).toBe(4);
  const storedInk = (await pageInk(page)).stonepen_json;

  if (isMobile) {
    await ensureWorkspaceOpen(page);
    await expect(page.locator('.sidebar')).toBeVisible();
    await page.locator('.file-entry[data-path="Research/Bayes Rule Derivation.md"]').click();
    await expect(page.locator('.app-container')).not.toHaveClass(/sidebar-open/);
    const markdownEditor = await openSource(page);
    await markdownEditor.fill(`${await markdownEditor.inputValue()}\n\nMobile review confirms the derivation remains editable.`);
    await persisted(page, 'Mobile review confirms');
    await closeSource(page);
    await chooseInkTool(page);
    await drawOver(page, pageBlocks(page, 'space'), { x: 40, y: 0.85, dx: 120 });
    await expect.poll(async () => strokeCount(await pageInk(page))).toBe(5);
    await chooseTyping(page);
  }

  await page.locator('.main-content').evaluate(element => { element.scrollTop = 0; });
  await page.screenshot({ path: `/tmp/inkstone-rc-proof/page-${isMobile ? 'mobile' : 'desktop'}.png`, fullPage: true });
  await expect(page.getByRole('heading', { name: 'Posterior inference', exact: true })).toBeVisible();
  await expect(page.getByRole('heading', { name: 'Derivation', exact: true })).toBeVisible();
  await expect(page.locator('.page-markdown ol li')).toHaveCount(3);
  await expect(page.locator('.page-markdown ul li')).toHaveCount(3);
  await expect(page.locator('.page-markdown blockquote')).toBeVisible();
  await expect(page.locator('.page-markdown table')).toBeVisible();
  await expect(page.locator('.page-math')).toHaveCount(2);
  await expect(page.locator('.page-math mfrac')).toHaveCount(1);
  await expect(page.locator('.page-math msubsup').first()).toBeVisible();
  await expect(page.locator('.page-source')).toHaveCount(0);

  await ensureInspectorOpen(page);
  await page.locator('.inspector-tab[data-tab="Backlinks"]').click();
  await expect(page.locator('.inspector-section[data-section="backlinks"] .backlink-context')).toHaveCount(7);
  await expect(page.locator('.inspector-section')).toContainText('Research/Conjugate Priors.md');
  if (!isMobile) {
    await page.screenshot({ path: '/tmp/inkstone-rc-proof/preview-backlinks.png', fullPage: true });
  }

  await page.locator('.inspector-tab[data-tab="Graph"]').click();
  await expect(page.locator('.local-graph-node')).toHaveCount(8);
  await expect(page.locator('.local-graph-node.active')).toContainText('Bayes Rule Deriva');
  if (!isMobile) {
    await page.screenshot({ path: '/tmp/inkstone-rc-proof/preview-graph.png', fullPage: true });
  }
  await page.locator('.graph-node-hit[data-path="Research/Conjugate Priors.md"]').click();
  await expect(page.locator('.breadcrumb')).toHaveText('Research/Conjugate Priors.md');
  await openNote(page, 'Research/Bayes Rule Derivation.md', 'Bayes’ Rule Derivation');
  await expect(await openSource(page)).toHaveValue(/Posterior inference/);
  await closeSource(page);
  await expect(page.locator('.page-math')).toHaveCount(2);
  if (!isMobile) expect((await pageInk(page)).stonepen_json).toBe(storedInk);
});


test('a note exports as a printable page, a web page, and Markdown with its ink', async ({ page }) => {
  await createNote(page, 'Export.md', 'Exported ink', 'The derivative of momentum gives force.\n\n- [x] checked item\n\n$$\nE = mc^2\n$$');
  const target = page.locator('.page-rendered p', { hasText: 'The derivative of momentum' });
  await chooseInkTool(page);
  await drawOver(page, target, { x: 10, dx: 200 });
  await expect.poll(async () => strokeCount(await pageInk(page))).toBe(1);
  await chooseTyping(page);

  const menuButton = page.locator('.export-btn');
  await menuButton.click();
  const items = page.locator('.export-menu [role="menuitem"]');
  await expect(items).toHaveText([/PDF/, /Web page/, /Markdown/, /Whole vault/]);
  await page.keyboard.press('Escape');
  await expect(page.locator('.export-menu')).toHaveCount(0);

  await menuButton.click();
  const htmlDownload = page.waitForEvent('download');
  await page.locator('.export-menu [data-export-action="html"]').click();
  const html = await htmlDownload;
  expect(html.suggestedFilename()).toBe('Export.html');
  const htmlText = require('fs').readFileSync(await html.path(), 'utf8');
  expect(htmlText).toContain('<title>Exported ink</title>');
  expect(htmlText).toContain('The derivative of momentum gives force.');
  expect(htmlText).toContain('<path d="M ');
  expect(htmlText).not.toContain('contenteditable');
  expect(htmlText).not.toContain('<script type="module"');

  const viewer = await page.context().newPage();
  await viewer.setContent(htmlText);
  const paragraph = await viewer.locator('p', { hasText: 'The derivative of momentum' }).boundingBox();
  const inkPath = await viewer.locator('.export-ink path').first().boundingBox();
  expect(inkPath.y + inkPath.height).toBeGreaterThan(paragraph.y - 20);
  expect(inkPath.y).toBeLessThan(paragraph.y + paragraph.height + 20);
  expect(inkPath.x).toBeGreaterThan(paragraph.x - 20);
  await expect(viewer.locator('input[type="checkbox"]')).toBeChecked();
  await viewer.close();

  await menuButton.click();
  const zipDownload = page.waitForEvent('download');
  await page.locator('.export-menu [data-export-action="markdown"]').click();
  const zip = await zipDownload;
  expect(zip.suggestedFilename()).toBe('Export.zip');
  const entries = require('child_process')
    .execFileSync('python3', ['-I', '-c', 'import sys,zipfile;z=zipfile.ZipFile(sys.argv[1]);print("\\n".join(z.namelist()));print(z.read("Export.md").decode())', await zip.path()])
    .toString();
  expect(entries).toContain('_ink/Export-ink-1.svg');
  expect(entries).toContain('The derivative of momentum gives force.\n\n![Handwriting](_ink/Export-ink-1.svg)');
  expect(entries).toContain('$$\nE = mc^2\n$$');
  expect(entries).not.toContain('inkstone-page-ink');

  await page.evaluate(() => {
    window.__printed = 0;
    new MutationObserver(records => {
      for (const record of records) {
        for (const node of record.addedNodes) {
          if (node.classList?.contains('export-print-frame')) {
            node.addEventListener('load', () => { node.contentWindow.print = () => { window.__printed += 1; }; }, { capture: true });
          }
        }
      }
    }).observe(document.body, { childList: true });
  });
  await menuButton.click();
  await page.locator('.export-menu [data-export-action="pdf"]').click();
  await expect(page.locator('.export-print-frame')).toHaveCount(1);
  await expect.poll(() => page.evaluate(() => window.__printed)).toBe(1);
  const printed = await page.locator('.export-print-frame').evaluate(frame => frame.contentDocument.body.textContent);
  expect(printed).toContain('The derivative of momentum gives force.');
});

test('the app is installable and opens again while offline', async ({ page, context }) => {
  const manifest = await (await page.request.get('/manifest.webmanifest')).json();
  expect(manifest.display).toBe('standalone');
  for (const icon of manifest.icons) {
    expect((await page.request.get(icon.src)).status()).toBe(200);
  }
  await page.addInitScript(() => Object.defineProperty(Navigator.prototype, 'webdriver', { get: () => false }));
  await page.goto('/?local=1');
  await ensureWorkspaceOpen(page);
  await expect(page.locator('link[rel="manifest"]')).toHaveCount(1);
  await page.evaluate(() => navigator.serviceWorker.ready);
  await page.reload();
  await expect.poll(() => page.evaluate(() => Boolean(navigator.serviceWorker.controller))).toBe(true);
  await ensureWorkspaceOpen(page);
  await context.setOffline(true);
  await page.reload();
  await ensureWorkspaceOpen(page);
  await expect(page.locator('.explorer-new-btn')).toBeVisible();
  await context.setOffline(false);
  await page.evaluate(async () => {
    for (const registration of await navigator.serviceWorker.getRegistrations()) await registration.unregister();
  });
});

function makePdf(pages) {
  const objects = ['<< /Type /Catalog /Pages 2 0 R >>'];
  const kids = pages.map((_, index) => `${3 + index * 2} 0 R`).join(' ');
  objects.push(`<< /Type /Pages /Kids [${kids}] /Count ${pages.length} >>`);
  const font = 3 + pages.length * 2;
  pages.forEach((text, index) => {
    const stream = `BT /F1 28 Tf 72 700 Td (${text}) Tj ET 0.2 0.4 0.8 rg 72 300 468 120 re f`;
    objects.push(`<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 ${font} 0 R >> >> /Contents ${4 + index * 2} 0 R >>`);
    objects.push(`<< /Length ${stream.length} >>\nstream\n${stream}\nendstream`);
  });
  objects.push('<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>');
  let out = '%PDF-1.4\n';
  const offsets = [];
  objects.forEach((body, index) => {
    offsets.push(out.length);
    out += `${index + 1} 0 obj\n${body}\nendobj\n`;
  });
  const xref = out.length;
  out += `xref\n0 ${objects.length + 1}\n0000000000 65535 f \n${offsets.map(offset => `${String(offset).padStart(10, '0')} 00000 n \n`).join('')}`;
  out += `trailer\n<< /Size ${objects.length + 1} /Root 1 0 R >>\nstartxref\n${xref}\n%%EOF\n`;
  return Buffer.from(out, 'latin1');
}

test('a PDF opens as pages you can write on, and the writing stays on the page at any width', async ({ page }) => {
  await createNote(page, 'Reading.md', 'Reading', 'Before the paper.');
  const chooser = page.waitForEvent('filechooser');
  await page.locator('.add-block-btn[data-block-type="image"]').click();
  await (await chooser).setFiles({ name: 'lecture.pdf', mimeType: 'application/pdf', buffer: makePdf(['Page one heading', 'Page two heading']) });
  const pages = page.locator('.pdf-pages .pdf-page');
  await expect(pages).toHaveCount(2);
  const rendered = index => pages.nth(index).locator('img').evaluate(image => image.complete && image.naturalWidth > 1000).catch(() => false);
  await expect.poll(() => rendered(0), { timeout: 15000 }).toBe(true);
  await expect.poll(() => rendered(1), { timeout: 15000 }).toBe(true);
  expect(await pageMarkdown(page)).toContain('![lecture](Assets/lecture.pdf)');
  const ratio = await pages.nth(1).evaluate(element => element.getBoundingClientRect().height / element.getBoundingClientRect().width);
  expect(ratio).toBeCloseTo(792 / 612, 2);

  await chooseInkTool(page);
  await drawOver(page, pages.nth(1), { x: 60, y: 0.55, dx: 120 });
  await expect.poll(async () => strokeCount(await pageInk(page))).toBe(1);
  expect(anchorTargets(await pageInk(page))[0].kind).toBe('block');
  await chooseTyping(page);

  const inkOnPage = async () => {
    await page.locator('.export-btn').click();
    const download = page.waitForEvent('download');
    await page.locator('.export-menu [data-export-action="html"]').click();
    const html = require('fs').readFileSync(await (await download).path(), 'utf8');
    expect(html).toContain('data:image/jpeg;base64');
    const viewer = await page.context().newPage();
    await viewer.setViewportSize(page.viewportSize());
    await viewer.setContent(html);
    const sheet = await viewer.locator('.pdf-page').nth(1).boundingBox();
    const ink = await viewer.locator('.export-ink path').first().boundingBox();
    await viewer.close();
    return { x: (ink.x - sheet.x) / sheet.width, y: (ink.y - sheet.y) / sheet.height };
  };
  const wide = await inkOnPage();
  expect(wide.y).toBeGreaterThan(0.4);
  expect(wide.y).toBeLessThan(0.7);
  const size = page.viewportSize();
  await page.setViewportSize({ width: Math.round(size.width * 0.7), height: size.height });
  await expect.poll(async () => (await pages.nth(1).boundingBox())?.width ?? Infinity).toBeLessThan(size.width * 0.7);
  const narrow = await inkOnPage();
  expect(Math.abs(narrow.x - wide.x)).toBeLessThan(0.03);
  expect(Math.abs(narrow.y - wide.y)).toBeLessThan(0.03);
  await page.setViewportSize(size);

  await page.reload();
  await openNote(page, 'Reading.md', 'Reading');
  await expect(pages).toHaveCount(2);
  await expect.poll(() => rendered(1), { timeout: 15000 }).toBe(true);
  expect(page.__browserErrors).toEqual([]);
});
