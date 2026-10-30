# Publishing Better-Photocraft-Web

Nothing in this repository has been pushed anywhere. This file is the checklist for the day you
decide to publish it. **None of these commands has been run.**

## The state it is in now

- The only remote is `upstream` (the official PhotoCraft, fetch only). Its push URL is `DISABLED`.
- `.git/hooks/pre-push` refuses every push.
- There is no `origin`.
- Work branches `glass` and `features` (and their worktrees in `.worktrees/`) are merged into `main`.

## 1. Fill in your GitHub username

Replace `TODO-USERNAME` in these places (check with `git grep -n TODO-USERNAME`):

| File | What |
|---|---|
| `crates/ui-egui/src/links.rs` | `GITHUB` and `ISSUES`: Help › This Edition on GitHub, Report an Issue, the About links |
| `README.md` | the `git clone` line under "Get started" |
| `packaging/linux/nfpm.yaml` | `homepage` |
| `packaging/linux/ai.storyteller.photocraft.metainfo.xml.in` | `homepage`, `bugtracker`, `contact` |
| `packaging/windows/photocraft.wxs` | the Add/Remove Programs links |
| `PUBLISHING.md` | the commands below |

```sh
git grep -l TODO-USERNAME | xargs sed -i '' 's/TODO-USERNAME/<your-username>/g'   # macOS sed
cargo test -p photocraft-ui-egui --lib links                                      # the link tests
git commit -am "Set the repository URL"
```

## 2. Pre-publish checklist

- [ ] **Secrets scan.** `git log -p v0.7.0..main | grep -n -i -E 'api[_-]?key|secret|token|password|BEGIN (RSA|OPENSSH|PRIVATE)'`
      should show nothing of yours. (No keys were ever needed for this work; no `.env` exists.)
- [ ] **Personal data.** `git log --format='%an <%ae>' v0.7.0..main | sort -u`: the commits carry
      your git name and email. Change them before pushing if you want a different public identity
      (`git rebase -r v0.7.0 --exec 'git commit --amend --no-edit --reset-author'` after setting
      `user.name` / `user.email`).
- [ ] **Licence.** `LICENSE` is MIT with both copyright lines; `LICENSE-APACHE` is gone
      (`git ls-files | grep -i apache` prints nothing); `NOTICE` says upstream is dual-licensed and
      this edition uses the MIT option.
- [ ] **Brand assets removed.** `git ls-files docs/brand` prints nothing, and
      `git grep -n -i -E 'discord|artcraft-(logo|mark)' -- ':!*.tsv' ':!docs/' ':!PUBLISHING.md' ':!CHANGELOG.md'`
      shows no link or image (translation catalogs still hold upstream's unused strings; plain-text
      credit to "the ArtCraft team" is intended).
- [ ] **`ATTRIBUTION.md` complete**: its "Added by this edition" section lists the two AI models and
      the added Rust dependencies. `docs/ai-models.md` matches `crates/ml/src/registry.rs`
      (`cargo test -p photocraft-ml registry`).
- [ ] **No model, font or corpus file committed.** `git ls-files | grep -E '\.(onnx|data|safetensors|pth)$'` prints nothing.
- [ ] **The gate is green**: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
      `cargo xtask layers`, `cargo xtask wasm`, `cargo test --workspace`, `packaging/web/package.sh`.
- [ ] Read `docs/better-roadmap.md` once more: it says which items are unfinished or unverified.
      Don't describe those as working in the release notes.

## 3. Create the repository and push

```sh
cd Better-Photocraft-Web-claude

# Remove the two safety locks
rm .git/hooks/pre-push
git remote set-url --push upstream https://github.com/storytold/photocraft   # optional: you can't push there anyway

# Create the empty repository on GitHub (public or private; no README, no licence: the repo has them)
gh auth login
gh repo create TODO-USERNAME/Better-Photocraft-Web --public \
  --description "PhotoCraft 0.7.0 with a Liquid Glass theme, local AI background removal and web improvements" \
  --disable-wiki

# Add it as origin and push main only (upstream's history comes along: this is a fork by history)
git remote add origin https://github.com/TODO-USERNAME/Better-Photocraft-Web.git
git push -u origin main
```

Do **not** push the upstream tags as your own releases, and do not open issues or pull requests
on `storytold/photocraft` from this work unless you decide to contribute something back.

## 4. After the first push

- Upstream's GitHub Actions workflows (`.github/workflows/`) will start running in your
  repository. `release.yml` expects signing secrets you don't have: disable it (Actions › the
  workflow › Disable) or delete it, and keep `ci.yml` only if you want its checks.
- Repository settings worth turning on: secret scanning and push protection, Dependabot alerts,
  branch protection on `main`, a description, topics and the website URL.
- To host the web build on GitHub Pages: build with `packaging/web/package.sh`, unzip
  `dist/release/better-photocraft-web-<version>.zip` and publish its folder. It works under a
  sub-path. Read `packaging/web/README.md` for the headers and the model-download CSP.
- A release: tag `v0.7.0-better.1`, attach the zip, and copy the notes from `CHANGELOG.md`.
