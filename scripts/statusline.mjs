#!/usr/bin/env node
// Direct statusLine entrypoint wrapper for ~/.gemini/antigravity-cli/settings.json
import { main } from './hud.mjs';

main().then((code) => {
  process.exitCode = code;
});
