#!/usr/bin/env node
// The JavaScript CLI of tools/rag before spec 28.13 (stage 6) moved command parsing, configuration
// and display to Rust. Tests compare the Rust CLI with it.
import { projectCommands, runProjectCommand } from './project.mjs';

const args = process.argv.slice(2);
const json = args.slice(0, args.includes('--') ? args.indexOf('--') : args.length).includes('--json');
function fail(command, message) {
  if (json) console.log(JSON.stringify({ schema_version: 1, command, status: 'failed', run_id: null,
    data: null, warnings: [], errors: [{ code: 'INVALID_ARGUMENT', message }] }));
  else console.error(message);
  process.exitCode = 2;
}

let project;
if (args[0] === '--project') {
  [, project] = args.splice(0, 2);
  if (!project?.trim() || project.startsWith('--')) {
    fail(null, 'Expected a directory after --project');
  }
}

if (process.exitCode === undefined) {
  const [command, ...options] = args;
  if (command === undefined || command === '--help' || command === '-h') {
    console.log(`Usage:\n${Object.values(projectCommands).map((line) => `  ${line}`).join('\n')}`);
  } else if (Object.hasOwn(projectCommands, command)) {
    await runProjectCommand(command, options, project);
  } else {
    fail(command, `Unknown command: ${command}. See nest --help.`);
  }
}
