import { lstat, readdir, readFile, mkdir, copyFile, writeFile, realpath } from 'node:fs/promises';
import path from 'node:path';
import { createHash } from 'node:crypto';
import { fileURLToPath } from 'node:url';
const desktop = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
export async function inventory(root, relative = '') {
  const result = [];
  for (const name of (await readdir(path.join(root, relative))).sort()) {
    if (name.includes('\\') || name.includes('\0')) throw new Error('Unsafe asset name');
    const file = path.join(relative, name), metadata = await lstat(path.join(root, file));
    if (metadata.isSymbolicLink()) throw new Error('Symlink assets refused');
    if (metadata.isDirectory()) result.push(...await inventory(root, file));
    else if (metadata.isFile() && metadata.size <= 64 * 1024 * 1024) {
      const bytes = await readFile(path.join(root, file));
      result.push({file: file.split(path.sep).join('/'), bytes: bytes.length, sha256: createHash('sha256').update(bytes).digest('hex')});
    } else throw new Error('Asset type or budget refused');
    if (result.length > 10000) throw new Error('Asset count refused');
  }
  return result;
}
export async function prepareAssets(consoleRoot, output, nativeRoot = path.join(desktop, 'native')) {
  consoleRoot = await realpath(consoleRoot); nativeRoot = await realpath(nativeRoot); output = path.resolve(output);
  if (output === consoleRoot || output.startsWith(consoleRoot + path.sep) || consoleRoot.startsWith(output + path.sep)
    || output === nativeRoot || output.startsWith(nativeRoot + path.sep) || nativeRoot.startsWith(output + path.sep)) throw new Error('Asset input/output overlap');
  const consoleFiles = await inventory(consoleRoot), nativeFiles = await inventory(nativeRoot);
  if (!consoleFiles.some(v => v.file === 'index.html') || !consoleFiles.some(v => v.file.startsWith('assets/') && v.file.endsWith('.js'))) throw new Error('A complete Console build is required');
  if (consoleFiles.some(v => v.file === 'asset-receipt.json' || v.file === 'native' || v.file.startsWith('native/'))) throw new Error('Reserved companion path in Console');
  await mkdir(output, {recursive: false}); // never merge with an old build
  for (const [root, files, prefix] of [[consoleRoot,consoleFiles,''],[nativeRoot,nativeFiles,'native']]) {
    for (const record of files) {
      const destination = path.join(output, prefix, record.file); await mkdir(path.dirname(destination), {recursive:true});
      await copyFile(path.join(root,record.file), destination, 1);
      if (createHash('sha256').update(await readFile(destination)).digest('hex') !== record.sha256) throw new Error('Asset changed during copy');
    }
  }
  // A second inventory detects source changes during staging; no rewriting of
  // the React console, its router, authentication or inline boot code occurs.
  if (JSON.stringify(await inventory(consoleRoot)) !== JSON.stringify(consoleFiles)) throw new Error('Console changed during staging');
  const receipt = {schema:1, console_files:consoleFiles, native_files:nativeFiles, native_ipc_windows:['companion'], console_auth:'Gateway HttpOnly cookie and CSRF', acceptance:'not_run'};
  await writeFile(path.join(output,'asset-receipt.json'),JSON.stringify(receipt,null,2)+'\n',{flag:'wx'});
  return receipt;
}
if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try { if(process.argv.length!==4) throw new Error('Usage: node assets.mjs CONSOLE_DIST NEW_OUTPUT'); const receipt = await prepareAssets(process.argv[2],process.argv[3]); console.log(JSON.stringify({files:receipt.console_files.length,acceptance:receipt.acceptance})); }
  catch(error) {console.error(error.message);process.exitCode=1;}
}
