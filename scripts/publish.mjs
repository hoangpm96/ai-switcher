#!/usr/bin/env node
// Publish a release of AI Account Switcher, the way Reqwise Figma is published: one command, from
// this Mac, to the store's own server (store.xbirds.org). Nothing goes through GitHub.
//
//   npm run publish-release              check everything and print the plan — changes nothing
//   npm run publish-release -- --yes     build, sign, notarize, upload, go live
//   npm run publish-release -- --status  what the server offers now
//   npm run publish-release -- --test --yes     try an update end to end without offering it to
//                                               anyone: builds the working tree (version as it is),
//                                               uploads only the update package + a manifest under
//                                               downloads/ai-switcher/test/. Point an installed app
//                                               at it with AI_SWITCHER_UPDATE_ENDPOINT (see below).
//   npm run publish-release -- --test --remove --yes   delete that test channel from the server
//
// What goes where (server reached as `ssh bahub`, nginx serves /var/www/xbirds-downloads at
// https://store.xbirds.org/downloads/):
//   AI-Account-Switcher-<v>-universal.dmg              the installer; the store page links it
//   ai-switcher/AI-Account-Switcher-<v>-universal.app.tar.gz   the in-app update package
//   ai-switcher/latest.json                            what installed apps read (last, so they never
//                                                      see a version whose files are missing)
//   /var/lib/reqwise-website/downloads/ai-switcher.json   version + installer for the store page
// Only .dmg (and other installer types) count in Quản trị › Lượt tải; the manifest and the update
// package do not.
//
// Trust: the .app and .dmg are signed with the Developer ID certificate and notarized by Apple (the
// keychain profile Reqwise Figma uses). The update package is also signed with the updater key
// (~/.config/ai-switcher/updater.key, never leaves this Mac); the app refuses a package whose
// signature does not match the public key built into it (tauri.conf.json plugins.updater.pubkey).
//
// Settings: ~/.config/ai-switcher/release.env, else ~/.config/reqwise/release.env (KEY=value lines):
//   APPLE_IDENTITY=Developer ID Application: <Name> (<TEAM>)   APPLE_KEYCHAIN_PROFILE=<notarytool profile>
// Optional env: AI_SWITCHER_UPDATER_KEY (key path), RELEASE_SSH (host, default bahub).
//
// Try an update by hand after `--test --yes` (quit the app first):
//   open --env AI_SWITCHER_UPDATE_ENDPOINT=https://store.xbirds.org/downloads/ai-switcher/test/latest.json \
//     "/Applications/AI Account Switcher.app"

import { spawnSync } from "node:child_process";
import { createHash, createPublicKey, verify as verifySig } from "node:crypto";
import { existsSync, mkdirSync, readFileSync, statSync, writeFileSync, rmSync } from "node:fs";
import { homedir, tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const args = new Set(process.argv.slice(2));
const YES = args.has("--yes");
const TEST = args.has("--test");
const STATUS = args.has("--status");
const REMOVE = args.has("--remove");

const SSH = process.env.RELEASE_SSH || "bahub";
const DOWNLOADS_DIR = "/var/www/xbirds-downloads";
const STORE_JSON_DIR = "/var/lib/reqwise-website/downloads";
const PUBLIC = "https://store.xbirds.org/downloads";
const CHANNEL = TEST ? "ai-switcher/test" : "ai-switcher";
const MANIFEST_URL = `${PUBLIC}/${CHANNEL}/latest.json`;
const APP_NAME = "AI Account Switcher";
const TARGET = "universal-apple-darwin";
const bundleDir = path.join(root, "src-tauri/target", TARGET, "release/bundle");

// ---------------------------------------------------------------- helpers

const ok = (msg) => console.log(`  ✓ ${msg}`);
const bad = [];
const no = (msg, fix) => {
  bad.push(msg);
  console.log(`  ✗ ${msg}${fix ? `\n      → ${fix}` : ""}`);
};
const step = (msg) => console.log(`\n==> ${msg}`);
function fail(msg) {
  console.error(`\n✗ ${msg}`);
  process.exit(1);
}

function run(cmd, argv, opts = {}) {
  const r = spawnSync(cmd, argv, { cwd: root, stdio: "inherit", ...opts });
  if (r.status !== 0) fail(`${cmd} ${argv.join(" ")} failed (exit ${r.status})`);
}

function capture(cmd, argv, opts = {}) {
  const r = spawnSync(cmd, argv, { cwd: root, encoding: "utf8", ...opts });
  return { code: r.status, out: (r.stdout || "").trim(), err: (r.stderr || "").trim() };
}

const ssh = (command) =>
  capture("ssh", ["-o", "BatchMode=yes", "-o", "ConnectTimeout=15", SSH, command]);

function sshOrFail(command, what) {
  const r = ssh(command);
  if (r.code !== 0) fail(`${what}: ${r.err || r.out || `exit ${r.code}`}`);
  return r.out;
}

const sha256 = (file) => createHash("sha256").update(readFileSync(file)).digest("hex");
const mb = (bytes) => `${(bytes / 1024 / 1024).toFixed(1)} MB`;

function readEnvFile(file) {
  const out = {};
  if (!existsSync(file)) return out;
  for (const line of readFileSync(file, "utf8").split("\n")) {
    const m = /^\s*([A-Z0-9_]+)\s*=\s*(.*?)\s*$/.exec(line);
    if (m && !line.trim().startsWith("#")) out[m[1]] = m[2].replace(/^["']|["']$/g, "");
  }
  return out;
}

/**
 * The check the installed app makes before installing: a minisign signature (as written by
 * `tauri signer sign`, base64) over `data`, made by the key whose public half is `pubkeyB64` (as in
 * tauri.conf.json). Checks the signature and the signed trusted comment.
 */
export function verifyUpdateSignature(pubkeyB64, sigB64, data) {
  const pubLine = Buffer.from(pubkeyB64, "base64").toString("utf8").trim().split("\n").pop().trim();
  const pk = Buffer.from(pubLine, "base64"); // "Ed" + key id (8) + key (32)
  const lines = Buffer.from(sigB64, "base64").toString("utf8").split("\n");
  const sig = Buffer.from(lines[1].trim(), "base64"); // "ED"/"Ed" + key id (8) + signature (64)
  const trusted = lines[2].replace(/^trusted comment: /, "");
  const globalSig = Buffer.from(lines[3].trim(), "base64");
  if (pk.length !== 42 || sig.length !== 74) return false;
  if (!sig.subarray(2, 10).equals(pk.subarray(2, 10))) return false; // signed by another key
  const key = createPublicKey({
    key: Buffer.concat([Buffer.from("302a300506032b6570032100", "hex"), pk.subarray(10)]),
    format: "der",
    type: "spki",
  });
  const prehashed = sig.subarray(0, 2).toString() === "ED";
  const message = prehashed ? createHash("blake2b512").update(data).digest() : data;
  return (
    verifySig(null, message, key, sig.subarray(10)) &&
    verifySig(null, Buffer.concat([sig.subarray(10), Buffer.from(trusted, "utf8")]), key, globalSig)
  );
}

// ---------------------------------------------------------------- project facts

const conf = JSON.parse(readFileSync(path.join(root, "src-tauri/tauri.conf.json"), "utf8"));
const version = conf.version;
const pubkey = conf.plugins?.updater?.pubkey ?? "";
const endpoints = conf.plugins?.updater?.endpoints ?? [];
const releaseEnv = {
  ...readEnvFile(path.join(homedir(), ".config/reqwise/release.env")),
  ...readEnvFile(path.join(homedir(), ".config/ai-switcher/release.env")),
};
const identity = process.env.APPLE_IDENTITY || releaseEnv.APPLE_IDENTITY || "";
const notaryProfile = process.env.APPLE_KEYCHAIN_PROFILE || releaseEnv.APPLE_KEYCHAIN_PROFILE || "";
const teamId = /\(([A-Z0-9]{10})\)\s*$/.exec(identity)?.[1] ?? "";
const updaterKey =
  process.env.AI_SWITCHER_UPDATER_KEY || path.join(homedir(), ".config/ai-switcher/updater.key");

const dmgName = `AI-Account-Switcher-${version}-universal.dmg`;
const tarName = `AI-Account-Switcher-${version}-universal${TEST ? "-test" : ""}.app.tar.gz`;
const dmgUrl = `${PUBLIC}/${dmgName}`;
const tarUrl = `${PUBLIC}/${CHANNEL}/${tarName}`;

function releaseNotes() {
  const all = JSON.parse(readFileSync(path.join(root, "release-notes.json"), "utf8"));
  return all[version]?.app ?? null;
}

// ---------------------------------------------------------------- --status

if (STATUS) {
  step(`What ${SSH} offers`);
  const live = ssh(`cat ${DOWNLOADS_DIR}/ai-switcher/latest.json 2>/dev/null`);
  const store = ssh(`cat ${STORE_JSON_DIR}/ai-switcher.json 2>/dev/null`);
  const test = ssh(`cat ${DOWNLOADS_DIR}/ai-switcher/test/latest.json 2>/dev/null`);
  const show = (label, text) => {
    if (!text.out) return console.log(`  ${label}: none`);
    try {
      const j = JSON.parse(text.out);
      console.log(`  ${label}: ${j.version}${j.pub_date ? ` · ${j.pub_date}` : j.date ? ` · ${j.date}` : ""}`);
    } catch {
      console.log(`  ${label}: unreadable`);
    }
  };
  show("In-app updates (latest.json)", live);
  show("Store page (ai-switcher.json)", store);
  show("Test channel", test);
  console.log(`  This repo: ${version}`);
  process.exit(0);
}

// ---------------------------------------------------------------- --test --remove

if (TEST && REMOVE) {
  step(`Remove the test channel (${DOWNLOADS_DIR}/ai-switcher/test/)`);
  if (!YES) {
    console.log("  Nothing changed. Run again with --yes to delete it.");
    process.exit(0);
  }
  sshOrFail(`rm -rf ${DOWNLOADS_DIR}/ai-switcher/test`, "deleting the test channel");
  ok("test channel removed");
  process.exit(0);
}

// ---------------------------------------------------------------- checks

step(`Checks for ${TEST ? "a TEST update" : "release"} ${version}`);

const versions = {
  "package.json": JSON.parse(readFileSync(path.join(root, "package.json"), "utf8")).version,
  "package-lock.json": JSON.parse(readFileSync(path.join(root, "package-lock.json"), "utf8")).version,
  "src-tauri/Cargo.toml": /^version\s*=\s*"([^"]+)"/m.exec(
    readFileSync(path.join(root, "src-tauri/Cargo.toml"), "utf8"),
  )?.[1],
};
const mismatched = Object.entries(versions).filter(([, v]) => v !== version);
if (mismatched.length)
  no(
    `Version ${version} in tauri.conf.json, but ${mismatched.map(([f, v]) => `${f} ${v}`).join(", ")}`,
    "bump all of them together",
  );
else ok(`Version ${version} in every file`);

let notes = null;
try {
  notes = releaseNotes();
} catch (e) {
  no(`release-notes.json unreadable: ${e.message}`);
}
const notesOk =
  Array.isArray(notes) &&
  notes.length > 0 &&
  notes.every(
    (l) => l && typeof l.en === "string" && typeof l.vi === "string" && l.en && l.vi && l.en.length <= 90 && l.vi.length <= 90,
  );
if (notesOk) ok(`What's new: ${notes.length} line${notes.length > 1 ? "s" : ""} (en + vi)`);
else if (TEST) {
  notes = [{ en: `Test build ${version}`, vi: `Bản thử ${version}` }];
  ok("What's new: none for this version — a test placeholder is used");
} else
  no(
    `release-notes.json has no usable "${version}".app lines`,
    'add {"en", "vi"} lines, each at most 90 characters',
  );

if (!TEST) {
  const changelog = readFileSync(path.join(root, "CHANGELOG.md"), "utf8");
  if (changelog.includes(`## [${version}]`)) ok(`CHANGELOG.md has ${version}`);
  else no(`CHANGELOG.md has no "## [${version}]" section`);

  const dirty = capture("git", ["status", "--porcelain", "--untracked-files=no"]).out;
  if (dirty) no("Uncommitted changes in tracked files", "commit first: the release is built from the commit");
  else ok(`Committed (${capture("git", ["rev-parse", "--short", "HEAD"]).out})`);
}

if (endpoints.includes(`${PUBLIC}/ai-switcher/latest.json`)) ok("The app reads updates from store.xbirds.org");
else no(`tauri.conf.json plugins.updater.endpoints is not ${PUBLIC}/ai-switcher/latest.json`);

if (!identity || !teamId) no("No Apple identity", "set APPLE_IDENTITY in ~/.config/ai-switcher/release.env");
else if (!capture("security", ["find-identity", "-v", "-p", "codesigning"]).out.includes(identity))
  no(`Certificate "${identity}" is not in the keychain`);
else ok(`Signing as ${identity}`);

if (!TEST) {
  if (!notaryProfile) no("No notarization profile", "set APPLE_KEYCHAIN_PROFILE in ~/.config/ai-switcher/release.env");
  else if (capture("xcrun", ["notarytool", "history", "--keychain-profile", notaryProfile]).code !== 0)
    no(`Notarization profile "${notaryProfile}" does not log in`, `xcrun notarytool store-credentials ${notaryProfile}`);
  else ok(`Notarizing with profile "${notaryProfile}"`);
}

if (!existsSync(updaterKey)) no(`Updater key missing: ${updaterKey}`, "restore it from your backup — never make a new one while apps are installed");
else if ((statSync(updaterKey).mode & 0o077) !== 0) no(`Updater key ${updaterKey} is readable by others`, `chmod 600 ${updaterKey}`);
else if (!pubkey) no("tauri.conf.json has no plugins.updater.pubkey");
else {
  // Prove the key and the app's public key are one pair: sign a scratch file, check it as the app would.
  const dir = path.join(tmpdir(), `ai-switcher-keycheck-${process.pid}`);
  mkdirSync(dir, { recursive: true });
  const probe = path.join(dir, "probe");
  writeFileSync(probe, `probe ${Date.now()}`);
  const signed = capture("npx", ["tauri", "signer", "sign", "-f", updaterKey, "-p", "", probe]);
  const pair =
    signed.code === 0 &&
    existsSync(`${probe}.sig`) &&
    verifyUpdateSignature(pubkey, readFileSync(`${probe}.sig`, "utf8").trim(), readFileSync(probe));
  rmSync(dir, { recursive: true, force: true });
  if (pair) ok("Updater key matches the public key built into the app");
  else no("Updater key does not match plugins.updater.pubkey", "the app would refuse every package signed with it");
}

const remote = ssh(
  `test -w ${DOWNLOADS_DIR} && test -w ${STORE_JSON_DIR} && echo writable; cat ${DOWNLOADS_DIR}/${CHANNEL}/latest.json 2>/dev/null; echo; test -e ${DOWNLOADS_DIR}/${dmgName} && echo DMG_EXISTS`,
);
if (remote.code !== 0 && !remote.out) no(`ssh ${SSH} failed: ${remote.err}`, `ssh ${SSH} must work without a password`);
else if (!remote.out.startsWith("writable")) no(`${DOWNLOADS_DIR} or ${STORE_JSON_DIR} is not writable on ${SSH}`);
else {
  ok(`Server ${SSH} reachable, folders writable`);
  const liveVersion = /"version"\s*:\s*"([^"]+)"/.exec(remote.out)?.[1];
  if (!TEST && remote.out.includes("DMG_EXISTS")) no(`${dmgName} is already on the server`, "bump the version");
  else if (!TEST && liveVersion === version) no(`The server already offers ${version}`, "bump the version");
  else ok(liveVersion ? `${TEST ? "Test channel" : "Server"} now offers ${liveVersion}` : `Nothing on the ${TEST ? "test channel" : "server"} yet`);
}

// ---------------------------------------------------------------- plan

step("Plan");
console.log(`  Version ${version}${TEST ? " — TEST channel only, nobody is offered it" : ""}`);
if (!TEST) {
  console.log(`  Installer  ${dmgUrl}`);
  console.log(`  Store page ${STORE_JSON_DIR}/ai-switcher.json → store.xbirds.org/ai-account-switcher shows ${version}`);
}
console.log(`  Update     ${tarUrl}`);
console.log(`  Manifest   ${MANIFEST_URL}${TEST ? "" : " (installed apps see the update from here)"}`);
console.log("  What's new:");
for (const line of notes ?? []) console.log(`    • ${line.vi}  /  ${line.en}`);

if (bad.length) fail(`${bad.length} check${bad.length > 1 ? "s" : ""} failed — nothing was changed.`);
if (!YES) {
  console.log("\nNothing changed. Run again with --yes to publish.");
  process.exit(0);
}

// ---------------------------------------------------------------- build

step(`Build ${version} (${TARGET}), signed`);
const buildEnv = { ...process.env, APPLE_SIGNING_IDENTITY: identity };
// Notarization is done below with the keychain profile, which Tauri does not support.
for (const k of ["APPLE_ID", "APPLE_PASSWORD", "APPLE_TEAM_ID", "APPLE_API_KEY", "APPLE_API_ISSUER", "APPLE_API_KEY_PATH"])
  delete buildEnv[k];
run("npm", ["run", "tauri", "build", "--", "--target", TARGET, "--bundles", TEST ? "app" : "app,dmg"], { env: buildEnv });

const appPath = path.join(bundleDir, "macos", `${APP_NAME}.app`);
const builtDmg = path.join(bundleDir, "dmg", `${APP_NAME}_${version}_universal.dmg`);
if (!existsSync(appPath)) fail(`No app at ${appPath}`);
if (!TEST && !existsSync(builtDmg)) fail(`No dmg at ${builtDmg}`);

const plistVersion = capture("/usr/libexec/PlistBuddy", ["-c", "Print :CFBundleShortVersionString", `${appPath}/Contents/Info.plist`]).out;
if (plistVersion !== version) fail(`The built app says version ${plistVersion}, expected ${version}`);
if (capture("codesign", ["--verify", "--deep", "--strict", appPath]).code !== 0) fail("codesign --verify failed on the app");
const sigInfo = capture("codesign", ["-dvv", appPath]).err;
if (!sigInfo.includes(`TeamIdentifier=${teamId}`) || !/flags=.*runtime/.test(sigInfo))
  fail("The app is not signed with the hardened runtime by the expected team");
ok(`App ${version} signed by team ${teamId}, hardened runtime`);

// ---------------------------------------------------------------- notarize

const out = path.join(root, "src-tauri/target/publish", `${version}${TEST ? "-test" : ""}`);
rmSync(out, { recursive: true, force: true });
mkdirSync(out, { recursive: true });
const dmg = path.join(out, dmgName);

if (!TEST) {
  step("Notarize with Apple (usually 2–10 minutes)");
  writeFileSync(dmg, readFileSync(builtDmg));
  const sub = capture("xcrun", ["notarytool", "submit", dmg, "--keychain-profile", notaryProfile, "--wait", "--output-format", "json"]);
  let result = {};
  try {
    result = JSON.parse(sub.out);
  } catch {
    fail(`notarytool said: ${sub.err || sub.out}`);
  }
  if (result.status !== "Accepted") {
    if (result.id) console.log(capture("xcrun", ["notarytool", "log", result.id, "--keychain-profile", notaryProfile]).out);
    fail(`Apple did not accept the build (${result.status ?? "no status"}). Nothing was uploaded.`);
  }
  ok(`Accepted (${result.id})`);
  run("xcrun", ["stapler", "staple", dmg]);
  run("xcrun", ["stapler", "staple", appPath]);
  const dmgCheck = capture("spctl", ["-a", "-t", "open", "--context", "context:primary-signature", "-vv", dmg]);
  const appCheck = capture("spctl", ["-a", "-t", "exec", "-vv", appPath]);
  if (!dmgCheck.err.includes("Notarized Developer ID") || !appCheck.err.includes("Notarized Developer ID"))
    fail(`Gatekeeper does not accept it:\n${dmgCheck.err}\n${appCheck.err}`);
  ok("Gatekeeper: dmg and app are Notarized Developer ID");
}

// ---------------------------------------------------------------- update package

step("Update package");
const tar = path.join(out, tarName);
run("tar", ["-czf", tar, "-C", path.join(bundleDir, "macos"), `${APP_NAME}.app`], {
  env: { ...process.env, COPYFILE_DISABLE: "1" },
});
run("npx", ["tauri", "signer", "sign", "-f", updaterKey, "-p", "", tar], { stdio: ["ignore", "ignore", "inherit"] });
const signature = readFileSync(`${tar}.sig`, "utf8").trim();
if (!verifyUpdateSignature(pubkey, signature, readFileSync(tar))) fail("The update package signature does not verify");
ok(`${tarName} (${mb(statSync(tar).size)}), signed with the updater key`);

// ---------------------------------------------------------------- upload

const pubDate = new Date().toISOString();
const manifest = {
  version,
  notes: JSON.stringify(notes),
  pub_date: pubDate,
  // One universal package serves both chips.
  platforms: {
    "darwin-aarch64": { signature, url: tarUrl },
    "darwin-x86_64": { signature, url: tarUrl },
  },
};
const manifestFile = path.join(out, "latest.json");
writeFileSync(manifestFile, `${JSON.stringify(manifest, null, 2)}\n`);

const storeFile = path.join(out, "ai-switcher.json");
if (!TEST) {
  const bytes = statSync(dmg).size;
  const minOs = capture("/usr/libexec/PlistBuddy", ["-c", "Print :LSMinimumSystemVersion", `${appPath}/Contents/Info.plist`]).out || undefined;
  const build = { os: "macos", file: dmgName, url: dmgUrl, bytes, sha256: sha256(dmg), minOs, signed: true };
  writeFileSync(
    storeFile,
    `${JSON.stringify(
      {
        product: "ai-account-switcher",
        version,
        date: pubDate.slice(0, 10),
        // The store lists one installer per chip; the universal .dmg is the same file for both.
        builds: [
          { ...build, arch: "arm64" },
          { ...build, arch: "x64" },
        ],
      },
      null,
      2,
    )}\n`,
  );
}

step(`Upload to ${SSH}`);
const stage = `${DOWNLOADS_DIR}/ai-switcher/.upload-${version}-${process.pid}`;
sshOrFail(`mkdir -p ${DOWNLOADS_DIR}/${CHANNEL} ${stage}`, "creating the upload folder");
const files = [tar, manifestFile, ...(TEST ? [] : [dmg, storeFile])];
run("scp", ["-q", "-o", "BatchMode=yes", ...files, `${SSH}:${stage}/`]);
const remoteSums = sshOrFail(`cd ${stage} && sha256sum ${files.map((f) => path.basename(f)).join(" ")}`, "checking the upload");
for (const f of files) {
  if (!remoteSums.includes(`${sha256(f)}  ${path.basename(f)}`)) fail(`${path.basename(f)} arrived damaged — nothing went live`);
}
ok("Uploaded, checksums match");

// Files first, the manifest last: an app never reads a version whose package is not there yet.
const moves = [
  `chmod 644 ${stage}/*`,
  `mv -f ${stage}/${tarName} ${DOWNLOADS_DIR}/${CHANNEL}/${tarName}`,
  ...(TEST
    ? []
    : [
        `mv -n ${stage}/${dmgName} ${DOWNLOADS_DIR}/${dmgName}`,
        `mv -f ${stage}/ai-switcher.json ${STORE_JSON_DIR}/ai-switcher.json`,
      ]),
  `mv -f ${stage}/latest.json ${DOWNLOADS_DIR}/${CHANNEL}/latest.json`,
  `rm -rf ${stage}`,
];
sshOrFail(moves.join(" && "), "putting the files in place");
ok(TEST ? "Test channel live" : `Live: installer, store page, in-app update ${version}`);

// ---------------------------------------------------------------- check from outside

step("Check over https, as a user's Mac would");
const fresh = { headers: { "cache-control": "no-cache" } };
const m = await (await fetch(MANIFEST_URL, fresh)).json();
if (m.version !== version) fail(`${MANIFEST_URL} says ${m.version}`);
const pkg = Buffer.from(await (await fetch(m.platforms["darwin-aarch64"].url, fresh)).arrayBuffer());
if (createHash("sha256").update(pkg).digest("hex") !== sha256(tar)) fail("The downloaded update package differs from the one built");
if (!verifyUpdateSignature(pubkey, m.platforms["darwin-aarch64"].signature, pkg)) fail("The downloaded update package fails the app's signature check");
ok(`Manifest ${version}; update package downloads and passes the app's signature check`);
if (!TEST) {
  // HEAD, not GET: a GET would count as a download in Quản trị › Lượt tải.
  const head = await fetch(dmgUrl, { method: "HEAD" });
  if (!head.ok || Number(head.headers.get("content-length")) !== statSync(dmg).size)
    fail(`${dmgUrl} answers ${head.status}, ${head.headers.get("content-length")} bytes`);
  ok(`Installer downloadable: ${dmgUrl} (${mb(statSync(dmg).size)})`);
}

console.log(
  TEST
    ? `\n✓ Test update ${version} is on the test channel. Quit the app, then:\n  open --env AI_SWITCHER_UPDATE_ENDPOINT=${MANIFEST_URL} "/Applications/${APP_NAME}.app"`
    : `\n✓ ${version} is out. Installed apps (0.13.0 and later) offer it within a few hours; the store page shows it now.`,
);
