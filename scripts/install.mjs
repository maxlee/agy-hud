// Safe installer & uninstaller for `~/.gemini/antigravity-cli/settings.json`.
// Preserves all existing settings, backs up before editing, and refuses to clobber
// another tool's statusLine unless `--replace` is passed.
import fs from 'node:fs';
import path from 'node:path';

const shellQuote = (s) => `'${String(s).replaceAll("'", `'\\''`)}'`;

function readJsonObjectOrEmpty(filePath) {
  if (!fs.existsSync(filePath)) return {};
  const text = fs.readFileSync(filePath, 'utf8').replace(/^\uFEFF/, '');
  if (!text.trim()) return {};
  const parsed = JSON.parse(text);
  if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) {
    throw new Error(`${filePath} does not contain a JSON object.`);
  }
  return parsed;
}

function buildStatusLineCommand(launcherPath, env = process.env, platform = process.platform) {
  if (platform === 'win32') {
    return `"${process.execPath}" "${launcherPath}"`;
  }
  return `${shellQuote(process.execPath)} ${shellQuote(launcherPath)}`;
}

function writeLauncher(launcherPath, hudScriptPath) {
  const content = `#!/usr/bin/env node
// Auto-generated launcher for agy-hud statusLine.
import { pathToFileURL } from 'node:url';
const target = ${JSON.stringify(hudScriptPath)};
await import(pathToFileURL(target).href);
`;
  fs.writeFileSync(launcherPath, content, { mode: 0o755 });
}

export function installStatusLine({ paths, pluginRoot, argv = [], env = process.env }) {
  const replace = argv.includes('--replace');
  const stack = argv.includes('--stack');
  const padIdx = argv.indexOf('--padding');
  const padding =
    padIdx !== -1 && argv[padIdx + 1] !== undefined
      ? Math.max(0, Number.parseInt(argv[padIdx + 1], 10) || 0)
      : 0;

  fs.mkdirSync(paths.cliDir, { recursive: true, mode: 0o700 });
  fs.mkdirSync(paths.hudHome, { recursive: true, mode: 0o700 });

  const settings = readJsonObjectOrEmpty(paths.cliSettingsPath);
  const currentCmd =
    typeof settings.statusLine?.command === 'string' ? settings.statusLine.command : '';
  const currentKind = !currentCmd
    ? 'none'
    : currentCmd.includes('agy-hud') || currentCmd.includes('hud.mjs')
      ? 'agy-hud'
      : 'other';

  if (currentKind === 'other' && !replace) {
    throw new Error(
      'settings.json already configures another statusLine script. Ask the user first, then re-run `install --replace` (a backup is created automatically).',
    );
  }

  const launcherPath = path.join(paths.hudHome, 'statusline.mjs');
  const hudScriptPath = path.join(pluginRoot, 'scripts', 'hud.mjs');
  writeLauncher(launcherPath, hudScriptPath);

  let backupPath = null;
  if (fs.existsSync(paths.cliSettingsPath)) {
    const stamp = new Date().toISOString().replace(/[-:]/g, '').replace(/\..+/, '');
    backupPath = `${paths.cliSettingsPath}.bak.${stamp}`;
    fs.copyFileSync(paths.cliSettingsPath, backupPath);
  }
  if (currentKind === 'other') {
    fs.writeFileSync(path.join(paths.hudHome, 'previous-statusline.json'), JSON.stringify(settings.statusLine, null, 2), {
      mode: 0o600,
    });
  }

  const nextStatusLine = {
    ...(currentKind === 'agy-hud' && typeof settings.statusLine === 'object' ? settings.statusLine : {}),
    type: 'command',
    command: buildStatusLineCommand(launcherPath, env),
    enabled: true,
    padding,
    stack_with_default: stack,
  };

  const targetFile = fs.existsSync(paths.cliSettingsPath)
    ? fs.realpathSync(paths.cliSettingsPath)
    : paths.cliSettingsPath;
  const mode = fs.existsSync(targetFile) ? fs.statSync(targetFile).mode & 0o777 : 0o600;
  const tmp = `${targetFile}.${process.pid}.tmp`;
  fs.writeFileSync(tmp, `${JSON.stringify({ ...settings, statusLine: nextStatusLine }, null, 2)}\n`, {
    mode,
  });
  fs.chmodSync(tmp, mode);
  fs.renameSync(tmp, targetFile);

  return {
    installed: true,
    settingsPath: paths.cliSettingsPath,
    launcherPath,
    command: nextStatusLine.command,
    replaced: currentKind,
    backupPath,
  };
}

export function uninstallStatusLine({ paths }) {
  if (!fs.existsSync(paths.cliSettingsPath)) {
    return { uninstalled: false, reason: 'settings.json does not exist.' };
  }
  const settings = readJsonObjectOrEmpty(paths.cliSettingsPath);
  const prevFile = path.join(paths.hudHome, 'previous-statusline.json');
  const nextSettings = { ...settings };

  let restoredPrevious = false;
  if (fs.existsSync(prevFile)) {
    try {
      nextSettings.statusLine = JSON.parse(fs.readFileSync(prevFile, 'utf8'));
      fs.rmSync(prevFile, { force: true });
      restoredPrevious = true;
    } catch {
      delete nextSettings.statusLine;
    }
  } else {
    delete nextSettings.statusLine;
  }

  const targetFile = fs.realpathSync(paths.cliSettingsPath);
  const mode = fs.statSync(targetFile).mode & 0o777;
  const tmp = `${targetFile}.${process.pid}.tmp`;
  fs.writeFileSync(tmp, `${JSON.stringify(nextSettings, null, 2)}\n`, { mode });
  fs.chmodSync(tmp, mode);
  fs.renameSync(tmp, targetFile);

  return {
    uninstalled: true,
    settingsPath: paths.cliSettingsPath,
    restoredPrevious,
  };
}
