# Fixtures

This directory is reserved for deterministic test-fixture generators and the labeled matching corpus.

- Keep generated folders disposable and reproducible from a seed.
- Cover nested paths, conflicts, Unicode names, unreadable files, and large sparse files as the corresponding engine features arrive.
- Keep labeled corpus pairs small and include both music and non-music cases.
- Do not commit large generated files or user data.

`matching-pairs.json` is the hand-labeled filename-matching seed corpus (70 pairs). The Rust evaluator expands it into 350 deterministic variants and checks candidate recall and conservative Likely precision in CI. See `docs/matching.md` for metrics, tier definitions, correlation limits, and the evaluation command.