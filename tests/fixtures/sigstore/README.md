# Sigstore fixture

A real, publicly signed artifact for `af self update`'s signature tests:
`fulcio_checksums.txt` and its keyless bundle from the
[sigstore/fulcio v1.9.0 release](https://github.com/sigstore/fulcio/releases/tag/v1.9.0)
(Apache-2.0), saved here as `fulcio_checksums.txt.sigstore.json` (published as
`fulcio_checksums.txt-keyless.sigstore.json`).

Signer identity: `keyless@projectsigstore.iam.gserviceaccount.com`, OIDC
issuer `https://accounts.google.com`, verified against the production Sigstore
trust root that `sigstore-verify` embeds. A bundle for an arbitrary test
binary cannot be minted offline, hence a real one.
