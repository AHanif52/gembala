# Contributing to Gembala

Requirements (BRD) and technical notes (TECH) are maintained by the repo owner in a separate place that isn't public.
Numbers like FR-61, SEC-20, or "TECH section 17" in code and docs refer to them. What you need to know lives in the
issue: pick up an existing issue, or open one before starting a new feature so it gets an FR number and acceptance
criteria.

## Set up your machine

macOS (Apple Silicon), Rust (`brew install rustup`), Node. Code structure, test commands, and the gotchas we've
already hit are in [`docs/DEVELOPMENT.md`](docs/DEVELOPMENT.md).

Release builds are signed with a self-signed certificate named **Gembala Dev** (`ui/src-tauri/tauri.conf.json`).
Without it `npm run tauri build` fails. Create it once on your machine:

```
cd "$(mktemp -d)"
openssl req -x509 -newkey rsa:2048 -nodes -days 3650 -subj "/CN=Gembala Dev" \
  -addext "extendedKeyUsage=codeSigning" -addext "keyUsage=digitalSignature" \
  -keyout key.pem -out cert.pem
openssl pkcs12 -export -legacy -inkey key.pem -in cert.pem -out gembala-dev.p12 -passout pass:gembala
security import gembala-dev.p12 -k ~/Library/Keychains/login.keychain-db -P gembala -T /usr/bin/codesign
rm key.pem gembala-dev.p12
```

Every machine's certificate is different, so the app you build is only trusted by the daemon you built yourself
(SEC-20). Automated tests and `scripts/` use the debug build and don't need the certificate.

Always install npm packages with `npm install --ignore-scripts` (SEC-16). Don't add a dependency without discussing it
in the PR.

## Workflow

1. Branch from `main`: `fr-61-pick-account`, `fix-drag-workspace`, etc.
2. Before you push, everything must be green:
   ```
   cargo clippy --workspace --all-targets -- -D warnings
   cargo test -p gembala && cargo test -p ui
   cd ui && npm run check && npm test
   ```
3. Commit messages are in English: start with the FR/SEC number if there is one, then a short summary
   (`FR-61: claude-<account> in a shell pane`). Use your GitHub noreply email
   (`git config user.email <id>+<user>@users.noreply.github.com`).
4. Open a PR against `main`. UI changes: include a screenshot or recording. Behavior changes: name the FR item or
   criterion it satisfies and the test that covers it.
5. Findings that change a requirement or design: write them in the PR/issue; the owner updates the BRD/TECH.

## Language

- Commit messages and code comments are in English. This applies from 4 Nov 2026; earlier history is in Indonesian and
  is not rewritten.
- All `.md` files in this repo are in English. The only Indonesian document is `README.id.md`, the translation of
  `README.md`; when you change one, update the other.
- UI text is not covered by this rule: the app is localized (Indonesian and English).

## Security

Don't report security holes in a public issue; send them through
[GitHub private vulnerability reporting](https://github.com/AHanif52/gembala/security/advisories/new).
