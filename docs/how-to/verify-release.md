# Verify a release

Each GitHub release contains platform archives, `SHA256SUMS`, Sigstore bundles,
and GitHub build provenance.

## Verify the checksum

```sh
sha256sum --check SHA256SUMS
```

On macOS, use `shasum -a 256 -c SHA256SUMS`.

## Verify the keyless signature

`af self update` checks every update against its archive's Sigstore bundle
before replacing anything. The certificate must name exactly
`https://github.com/mlco2/agentic-footprint/.github/workflows/release.yml@refs/tags/<tag>`,
issued by `https://token.actions.githubusercontent.com`, and appear in the
Rekor transparency log. The signature is keyless: GitHub Actions obtains a
short-lived identity through OIDC, so the project keeps no long-lived signing
key in repository secrets.

To check a first download by hand, verify its provenance as shown below.

## Verify GitHub provenance

With the GitHub CLI:

```sh
gh attestation verify \
  agentic-footprint-vX.Y.Z-aarch64-apple-darwin.tar.gz \
  --repo mlco2/agentic-footprint
```

Verification proves which workflow and repository produced the artifact. It
does not replace reviewing the source tag or the project's security policy.
