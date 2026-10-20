# Host contract baseline (T0)

This package is test-only, outside the production Rust dependency graph.
It does not implement the Host or claim Office/OpenWork runtime acceptance.

Node 24.19.0:

    npm ci --ignore-scripts --no-audit --no-fund
    npm run prepare:office
    npm run check
    npm test

For a source audit, additionally run:

    node prepare-office.mjs --office-source /absolute/path/to/ai-office-react

The latter reads the pinned commit with git show, preserving uncommitted Office
changes. It verifies all linked source hashes and regenerates only the fixed
test schemas. Default CI validates the stored schemas without private repo
credentials. Never hand-edit upstream-office.

Office fixtures execute exact upstream Zod schemas, including refinements and
normalization; wire null/depth checks are independent. SDK fixtures compile
with satisfies against v1 types, with negative type cases for old mixed-version
examples. SDK method tests use its actual HTTP request builder; SSE uses a real
loopback fixture server because this SDK's SSE ignores config.fetch.

T4/T5/T6 must add real Office/browser acceptance. T7 must run the fixed SDK
against the real Host. Passing T0 fixtures is not evidence of those features.
