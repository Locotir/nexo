## What changed

## How it was tested
- [ ] `nexo/tools/pre-push-check.sh` (fmt, locked build/tests/clippy on the local host)
- [ ] `nexo/tools/pre-push-check.sh --full` (also MSRV and dependency/advisory policy)
- [ ] Inspect native Linux, macOS and Windows CI jobs separately; inspect non-gating advisories too
- Validation limits / checks not run:
