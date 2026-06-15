# How to Verify a Published Bridge Validator Image

Run check 1 on every release. Run check 2 when you want
to trust nothing but the source. For the full reasoning, see
[`VERIFICATION_DETAILS.md`](./VERIFICATION_DETAILS.md).

## 1. Digest check

> Checks that the `<VERSION>` tag on Docker Hub still points to the exact
> image CI recorded for this release — i.e. the tag has not been re-pointed
> since publication.

Take `RECORDED_DIGEST` from the GitHub Release body (the `digest:` line under
**Published image**), then compare it to what the registry serves now:

```bash
docker buildx imagetools inspect gnosischain/bridge-validator:<VERSION> \
  --format '{{.Manifest.Digest}}'
```

Output equals `RECORDED_DIGEST` → pass. Mismatch → the tag was re-pointed;
**stop and investigate.**

## 2. Full audit: rebuild from source

> Checks that the published image's application payload (`/app`: the `worker`
> binary + `migrations`) is byte-identical to what the source at the release
> commit builds — without trusting CI at all. Both published platform slices
> (`linux/amd64` and `linux/arm64`) are verified; the non-native one runs
> under emulation and is slow (often 30+ min for a Rust release build).

### Inputs you need (obtain from a trusted, out-of-band channel)

- `VERIFIER_SHA` — commit SHA of the audited `verify.sh` to run (the tool).
- `VERSION` — release tag to check, e.g. `v0.2.0` (the subject).
- `EXPECTED_SOURCE_COMMIT` — commit SHA `VERSION` must resolve to.

### Prerequisites

- `docker` (with `buildx`), `git`.
- Network to Docker Hub and `github.com`; a few GB free disk.
- Emulation for the non-native slice (Docker Desktop: Rosetta/QEMU), or set
  `PLATFORMS` to verify only your native slice.

### Steps

1. Download the tool, pinned to its commit:

   ```bash
   curl -fsSL \
     https://raw.githubusercontent.com/gnosischain/bridge-validator/<VERIFIER_SHA>/verify.sh \
     -o verify.sh
   ```

2. Run it against the release, asserting the expected source commit:

   ```bash
   bash verify.sh <VERSION> <EXPECTED_SOURCE_COMMIT>
   ```

   To verify a single platform only (e.g. native-only, no emulation):

   ```bash
   PLATFORMS=linux/arm64 bash verify.sh <VERSION> <EXPECTED_SOURCE_COMMIT>
   ```

### Result

- `✅ VERIFICATION PASSED` → image at `<VERSION>` was built from
  `<EXPECTED_SOURCE_COMMIT>`; `/app` is byte-identical on every verified slice.
- `❌ VERIFICATION FAILED` → differences under `/app`, or the tag does not
  resolve to `<EXPECTED_SOURCE_COMMIT>`. Do not deploy; investigate.

Note: only tags cut after the base images were pinned by digest in
`bridge_validator/Dockerfile` are expected to reproduce; older tags had
floating `FROM` lines (see
[`VERIFICATION_DETAILS.md`](./VERIFICATION_DETAILS.md)).
