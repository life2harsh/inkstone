const { test, expect } = require('@playwright/test');

const destructiveAck = 'DELETE_TEST_DATA';
const liveE2EEnabled = process.env.INKSTONE_LIVE_E2E === '1';

if (liveE2EEnabled && process.env.INKSTONE_LIVE_E2E_DESTRUCTIVE_ACK !== destructiveAck) {
  throw new Error(
    `Refusing destructive live E2E without INKSTONE_LIVE_E2E_DESTRUCTIVE_ACK=${destructiveAck}`
  );
}

function requiredEnv(name) {
  const value = process.env[name];
  if (!value) throw new Error(`${name} is required for destructive live E2E`);
  return value;
}

async function loadedWebModuleUrl(page) {
  return page.evaluate(() => {
    const moduleUrl = performance.getEntriesByType('resource')
      .map(entry => entry.name)
      .find(url => /\/pkg\/[^/]+\/inkstone_web\.js$/.test(new URL(url).pathname));
    if (!moduleUrl) throw new Error('Could not find the fingerprinted Inkstone web module');
    return moduleUrl;
  });
}

test.use({ trace: 'off', screenshot: 'off', video: 'off' });

test.describe('destructive encrypted live sync (test deployments only)', () => {
  test.skip(!liveE2EEnabled, 'requires an explicitly acknowledged disposable test deployment');

  test('syncs encrypted edits between invited browser contexts', async ({ browser }) => {
    test.setTimeout(90_000);
    const ownerContext = await browser.newContext();
    const inviteeContext = await browser.newContext();
    const owner = await ownerContext.newPage();
    const invitee = await inviteeContext.newPage();
    const suffix = `${Date.now()}-${Math.random().toString(16).slice(2)}`;
    const ownerEmail = requiredEnv('INKSTONE_E2E_OWNER_EMAIL');
    const ownerPassword = requiredEnv('INKSTONE_E2E_OWNER_PASSWORD');
    const ownerVaultPassphrase = requiredEnv('INKSTONE_E2E_VAULT_PASSPHRASE');
    const inviteeEmail = `invitee-${suffix}@example.test`;
    const inviteePassword = `another correct horse battery staple ${suffix}`;
    const sharedPath = `Live E2E ${suffix}/Shared.md`;
    const renamedPath = `Live E2E ${suffix}/Renamed.md`;
    const collaboratorPath = `Live E2E ${suffix}/Collaborator.md`;

    await owner.goto('/?local=1');
    await owner.locator('.auth-email').fill(ownerEmail);
    await owner.locator('.auth-password').fill(ownerPassword);
    await owner.locator('.auth-vault-passphrase').fill(ownerVaultPassphrase);
    if (process.env.INKSTONE_E2E_BOOTSTRAP === '1') {
      await owner.locator('.auth-bootstrap-token').fill(requiredEnv('INKSTONE_E2E_BOOTSTRAP_TOKEN'));
      await owner.getByRole('button', { name: 'Create first account' }).click();
    } else {
      await owner.getByRole('button', { name: 'Sign in' }).click();
    }
    await expect(owner.locator('.explorer-new-btn')).toBeVisible();

    await owner.locator('.explorer-new-btn').click();
    await owner.locator('.explorer-create-input').fill(sharedPath);
    await owner.locator('.explorer-create-input').press('Enter');
    await expect(owner.locator('.phase-badge')).toHaveText('Live');
    await owner.locator('.document-title-input').fill('Shared encrypted note');
    await owner.locator('.markdown-source-input').fill('# Owner ciphertext');

    await owner.locator('.invite-email').fill(inviteeEmail);
    await expect(owner.locator('.invite-email')).toHaveValue(inviteeEmail);
    await owner.getByRole('button', { name: 'Invite editor' }).click();
    const shareUrl = await owner.locator('.invite-url').inputValue();
    expect(new URL(shareUrl).hash).toContain('key=');
    const invitedWorkspaceId = new URLSearchParams(new URL(shareUrl).hash.slice(1)).get('workspace');
    await invitee.goto(shareUrl);
    await invitee.locator('.auth-email').fill(inviteeEmail);
    await invitee.locator('.auth-password').fill(inviteePassword);
    await invitee.locator('.auth-vault-passphrase').fill(`vault ${suffix}`);
    await invitee.getByRole('button', { name: 'Register and accept' }).click();
    await invitee.locator(`.explorer-folder[data-path="Live E2E ${suffix}"]`).click();
    await expect(invitee.locator(`.file-entry[data-path="${sharedPath}"]`)).toBeVisible();
    await invitee.locator(`.file-entry[data-path="${sharedPath}"] .file-select-btn`).click();
    await expect(invitee.locator('.document-title-input')).toHaveValue('Shared encrypted note');
    await expect(invitee.locator('.phase-badge')).toHaveText('Live');

    await invitee.locator('.markdown-source-input').fill('# Invitee live edit');
    await expect(owner.locator('.markdown-source-input')).toHaveValue('# Invitee live edit');

    const webModuleUrl = await loadedWebModuleUrl(owner);
    await owner.evaluate(async moduleUrl => {
      const module = await import(moduleUrl);
      module.disconnect_sync_for_test();
    }, webModuleUrl);
    await expect(owner.locator('.phase-badge')).toHaveText('Offline');
    await invitee.locator('.markdown-source-input').fill('# Catch up after reconnect');
    await expect(owner.locator('.phase-badge')).toHaveText('Live', { timeout: 20_000 });
    await expect(owner.locator('.markdown-source-input')).toHaveValue('# Catch up after reconnect');

    await invitee.locator(`.file-entry[data-path="${sharedPath}"]`).hover();
    await invitee.getByRole('button', { name: 'Rename Shared.md' }).click();
    await invitee.locator('.rename-input').fill('Renamed.md');
    await invitee.locator('.rename-input').press('Enter');
    await expect(owner.locator(`.file-entry[data-path="${renamedPath}"]`)).toBeVisible({ timeout: 15_000 });

    await invitee.locator('.explorer-new-btn').click();
    await invitee.locator('.explorer-create-input').fill(collaboratorPath);
    await invitee.locator('.explorer-create-input').press('Enter');
    await expect(invitee.locator('.phase-badge')).toHaveText('Live', { timeout: 20_000 });
    await invitee.locator('.document-title-input').fill('Created by collaborator');
    await invitee.locator('.markdown-source-input').fill('# Discovered without reload');

    const discovered = owner.locator(`.file-entry[data-path="${collaboratorPath}"]`);
    await expect(discovered).toBeVisible({ timeout: 15_000 });
    await discovered.locator('.file-select-btn').click();
    await expect(owner.locator('.document-title-input')).toHaveValue('Created by collaborator');
    await expect(owner.locator('.markdown-source-input')).toHaveValue('# Discovered without reload');

    await owner.locator(`.file-entry[data-path="${renamedPath}"]`).hover();
    owner.once('dialog', dialog => dialog.accept());
    await owner.getByRole('button', { name: 'Delete Renamed.md' }).click();
    await expect(invitee.locator(`.file-entry[data-path="${renamedPath}"]`)).toHaveCount(0, { timeout: 25_000 });

    await invitee.getByRole('button', { name: 'Sign out' }).click();
    await expect(invitee.locator('.auth-email')).toBeVisible();
    await expect.poll(() => invitee.evaluate(workspaceId => new Promise((resolve, reject) => {
      const open = indexedDB.open('inkstone-workspace', 1);
      open.onerror = () => reject(open.error);
      open.onsuccess = () => {
        const db = open.result;
        const request = db.transaction('doc_map', 'readonly')
          .objectStore('doc_map')
          .get(`\u0000workspace_key:${workspaceId}`);
        request.onerror = () => reject(request.error);
        request.onsuccess = () => {
          resolve(request.result);
          db.close();
        };
      };
    }), invitedWorkspaceId)).toBeUndefined();
    await invitee.locator('.auth-email').fill(ownerEmail);
    await invitee.locator('.auth-password').fill(ownerPassword);
    await invitee.locator('.auth-vault-passphrase').fill(ownerVaultPassphrase);
    await invitee.getByRole('button', { name: 'Sign in' }).click();
    await expect(invitee.locator('.explorer-new-btn')).toBeVisible();

    await ownerContext.close();
    await inviteeContext.close();
  });
});
