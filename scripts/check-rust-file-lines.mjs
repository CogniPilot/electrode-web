import { execFileSync } from 'node:child_process';
import { readFileSync } from 'node:fs';

const maxLines = Number.parseInt(process.env.ELECTRODE_MAX_RUST_FILE_LINES ?? '2000', 10);

if (!Number.isInteger(maxLines) || maxLines <= 0) {
  throw new Error('ELECTRODE_MAX_RUST_FILE_LINES must be a positive integer');
}

const trackedRustFiles = execFileSync('git', ['ls-files', '-z', '--', '*.rs'], {
  encoding: 'utf8'
})
  .split('\0')
  .filter(Boolean)
  .filter((file) => !file.includes('/generated/'));

const oversized = trackedRustFiles
  .map((file) => ({
    file,
    lines: readFileSync(file, 'utf8').split('\n').length
  }))
  .filter(({ lines }) => lines > maxLines)
  .sort((a, b) => b.lines - a.lines);

if (oversized.length > 0) {
  const details = oversized.map(({ file, lines }) => `  ${file}: ${lines}`).join('\n');
  throw new Error(`Rust source files exceed ${maxLines} lines:\n${details}`);
}

console.log(`Checked ${trackedRustFiles.length} Rust files; limit is ${maxLines} lines.`);
