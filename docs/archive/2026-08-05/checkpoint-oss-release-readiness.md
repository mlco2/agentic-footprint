# OSS release-readiness checkpoint — 2026-08-05

## Status

The codebase, documentation, repository tree, and release automation have been
prepared for publication in a new GitHub repository. Historical Git rewriting
is intentionally out of scope because publication will start from a fresh
repository history.

## Completed foundation

- Zensical documentation organized as tutorials, how-to guides, explanations,
  references, contributor guides, decisions, and archived development records.
- Outdated debug-console handoff and combined Codex/OpenCode material archived.
- First-release public documentation limited to Claude Code and Codex.
- MIT license copied from CodeCarbon as requested.
- Security policy, contribution guide, and code of conduct added.
- IDE metadata, local Claude/Codex state, generated files, and obsolete ZIP
  material removed from the publication tree and covered by ignore rules.
- Automated tracked-file and credential-pattern hygiene check added.
- OpenCode excluded from the default CLI and setup help; available only through
  `experimental-opencode` builds.
- Rust toolchain pinned to 1.97.1 and CI configured to test default and
  experimental feature surfaces.
- Deterministic release packaging added for Linux x86-64, macOS x86-64, and
  macOS ARM64.
- Release workflow builds binaries twice from clean target state and requires
  byte-identical output before publishing.
- Release assets include SHA-256 checksums, keyless Sigstore bundles, and
  GitHub OIDC build provenance.
- Local Rust, Python, installer, hook, statusline, Clippy, formatting,
  documentation, archive reproducibility, and hygiene validation passed.

## Remaining publication tasks

### Repository identity

- [ ] Choose the final GitHub organization and repository name.
- [ ] Add Cargo `repository` and `homepage` metadata after the URL exists.
- [ ] Add `site_url`, `repo_url`, `repo_name`, and `edit_uri` to `mkdocs.yml`.
- [ ] Replace `<owner>/<repository>` placeholders in release-verification docs.
- [ ] Replace placeholder installer/release URLs with canonical GitHub URLs.

### Legal and governance

- [ ] Decide the Agentic Footprint copyright holder and initial copyright year.
- [ ] Decide whether CodeCarbon's copied copyright notices should remain alone
      or be accompanied by an Agentic Footprint notice.
- [ ] Identify the private security-report contact or rely explicitly on
      GitHub private vulnerability reporting.
- [ ] Confirm maintainers and review/merge authority for the initial release.

### GitHub repository configuration

- [ ] Create the fresh public repository from the sanitized tree.
- [ ] Enable private vulnerability reporting.
- [ ] Enable artifact attestations.
- [ ] Configure branch protection or repository rulesets for the default branch.
- [ ] Require the Rust, console, docs, and hygiene CI jobs before merge.
- [ ] Protect release tags and optionally require a release environment review.
- [ ] Enable Dependabot or Renovate.

### Documentation publication

- [ ] Choose GitHub Pages or another documentation host.
- [ ] Add a Zensical deployment workflow for the selected host.
- [ ] Configure a custom documentation domain if wanted.
- [ ] Add final project logo, favicon, social metadata, and screenshots.
- [ ] Review archived documents for any material that should stay private rather
      than merely remain outside the active navigation.

### Release rehearsal

- [ ] Decide whether the workflow should accept prerelease tags such as
      `v0.1.0-rc.1`; the current version checker expects exact Cargo versions.
- [ ] Push a release-candidate tag and exercise the hosted workflow end to end.
- [ ] Confirm all three runner labels remain available to the new repository.
- [ ] Verify produced binaries are byte-identical across the workflow's two
      clean builds.
- [ ] Download and validate `SHA256SUMS`.
- [ ] Verify a Sigstore bundle with `cosign verify-blob`.
- [ ] Verify GitHub provenance with `gh attestation verify`.
- [ ] Test `install.sh` against a real published archive and checksum.
- [ ] Run fresh-machine installation tests on macOS ARM64, macOS Intel, and
      Linux x86-64.

### Distribution scope

- [ ] Decide whether Windows is unsupported for v0.1 or requires a release
      artifact and service implementation.
- [ ] Decide whether workspace crates remain implementation details or are
      intended for crates.io publication.
- [ ] If publishing crates, add final repository metadata, packaging excludes,
      crate-specific READMEs, and `cargo package` checks.
- [ ] Decide whether to add Homebrew or another package-manager distribution
      after the direct GitHub release path is proven.

### Supply-chain hardening

- [ ] Add `cargo-deny` policy for advisories, licenses, banned dependencies,
      sources, and duplicate versions.
- [ ] Decide whether GitHub Actions must be pinned to immutable commit SHAs.
- [ ] Add an SBOM to release artifacts if required for the target users.
- [ ] Consider independent builders or a second CI provider for stronger
      reproducibility evidence beyond same-runner clean rebuilds.

## Release exit criteria

The first public release is ready when:

1. the final repository and documentation URLs are present everywhere;
2. legal ownership and maintainer responsibilities are explicit;
3. protected CI passes in the fresh public repository;
4. an RC tag produces signed, attested, checksum-verified artifacts for every
   supported platform;
5. a clean machine installs one of those artifacts and successfully completes
   `af service status`, `af setup --check`, and `af python doctor`;
6. Claude Code and Codex each complete a fresh measured session without manual
   collector wiring;
7. unsupported platforms and experimental integrations are clearly labelled.

## Intentionally deferred

- OpenCode remains experimental and outside the first release.
- Transactional setup rollback remains a separate design decision rather than
  a release-readiness change.
- Historical Git cleanup is unnecessary because publication will use a fresh
  repository history.
