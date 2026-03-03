import fs from 'node:fs';
import path from 'node:path';
import process from 'node:process';

const versionArg = process.argv[2];

if (!versionArg) {
  console.error('Usage: npm run version:sync -- <version>');
  process.exit(1);
}

const version = versionArg.startsWith('v') ? versionArg.slice(1) : versionArg;
const semverPattern =
  /^\d+\.\d+\.\d+(?:-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$/;

if (!semverPattern.test(version)) {
  console.error(`Invalid semver version: ${versionArg}`);
  process.exit(1);
}

const root = process.cwd();
const packageJsonPath = path.join(root, 'package.json');
const packageLockPath = path.join(root, 'package-lock.json');
const tauriConfPath = path.join(root, 'src-tauri', 'tauri.conf.json');
const cargoTomlPath = path.join(root, 'src-tauri', 'Cargo.toml');

const packageJson = JSON.parse(fs.readFileSync(packageJsonPath, 'utf8'));
packageJson.version = version;
fs.writeFileSync(packageJsonPath, `${JSON.stringify(packageJson, null, 2)}\n`);

const packageLock = JSON.parse(fs.readFileSync(packageLockPath, 'utf8'));
packageLock.version = version;
if (packageLock.packages?.['']) {
  packageLock.packages[''].version = version;
}
fs.writeFileSync(packageLockPath, `${JSON.stringify(packageLock, null, 2)}\n`);

const tauriConf = JSON.parse(fs.readFileSync(tauriConfPath, 'utf8'));
tauriConf.version = version;
fs.writeFileSync(tauriConfPath, `${JSON.stringify(tauriConf, null, 2)}\n`);

const cargoToml = fs.readFileSync(cargoTomlPath, 'utf8');
const cargoVersionPattern = /(\[package\][\s\S]*?\nversion\s*=\s*")[^"]+(")/;
if (!cargoVersionPattern.test(cargoToml)) {
  console.error('Could not update [package].version in src-tauri/Cargo.toml');
  process.exit(1);
}
const updatedCargoToml = cargoToml.replace(cargoVersionPattern, `$1${version}$2`);

fs.writeFileSync(cargoTomlPath, updatedCargoToml);

console.log(`Synchronized version to ${version} in package.json, package-lock.json, tauri.conf.json, and Cargo.toml.`);
