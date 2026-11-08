#!/usr/bin/env node
// Skill-local helper wrapper so `skills/hud/SKILL.md` can invoke `scripts/hud-cli.mjs` relatively.
import { main } from '../../../scripts/hud.mjs';

main().then((code) => {
  process.exitCode = code;
});
