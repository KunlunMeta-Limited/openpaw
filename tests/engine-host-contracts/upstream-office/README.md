# Fixed upstream test schemas

Repository: https://github.com/KunlunMeta-Limited/ai-office-react
Commit: b221a44ce22fe08956356b5002d17df59a6a4f50

The nine protocol TypeScript files and LICENSE are exact upstream bytes.
See docs/engine-host/contracts/sources.json for SHA-256 hashes and provenance.
They are fixtures only: no Host dependency, copied engine, renderer, or modified
compatibility implementation. This snapshot is necessary because the private
Office source cannot be downloaded anonymously by OpenPaw CI.

Do not edit these files manually. To verify linked documents and regenerate:
node prepare-office.mjs --office-source /absolute/path/to/ai-office-react

Change the pinned commit, review its protocol, and update hashes together before
a future schema upgrade. Default prepare verifies every stored source hash and
fails on missing/changed files, without cross-repository credentials or network.

Root .gitattributes disables checkout EOL conversion for this snapshot. Hashes
cover original bytes on Windows and Unix; do not normalize during verification.
