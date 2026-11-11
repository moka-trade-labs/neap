# Upstream provenance

This directory is a vendored copy of the percolator risk engine.

| | |
|---|---|
| Upstream | https://github.com/dcccrypto/percolator |
| Commit | `64cd8f245211c4d5dc4fbc8cc5eaf231180ad05e` (2026-09-28) |
| Grand-upstream | https://github.com/aeyakovenko/percolator (`37cb14ee`, 2026-10-28) |
| License | Apache-2.0 (see `LICENSE`) |

Vendored instead of forked because the session could only write to
`moka-trade-labs/moka-research`. Every change made here after the import is a
separate commit on top of the import commit, so `git log -- percolator/engine`
is the full diff against upstream.
