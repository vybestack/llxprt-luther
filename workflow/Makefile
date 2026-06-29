# Local contributor entrypoint mirroring the .github/workflows/pr-quality.yml gates.
#
# Usage:
#   make ci-check   # run the full local CI mirror (fmt, clippy, complexity, coverage, test)
#   make qa         # delegate to `cargo qa` (xtask qa)
#   make fmt        # cargo fmt --all -- --check
#   make clippy     # cargo clippy with CLIPPY_CONF_DIR=.github/clippy
#   make complexity # cargo xtask complexity (lizard + file-size gates)
#   make coverage   # cargo coverage (llvm-cov gate)
#   make test       # cargo test --workspace --all-features --lib --tests
#   make ocr-review ARGS="--preview" # local OpenCodeReview wrapper

.PHONY: ci-check qa fmt clippy complexity coverage test ocr-review

CLIPPY_CONF_DIR := .github/clippy

ci-check: fmt clippy complexity test coverage

qa:
	cargo qa

fmt:
	cargo fmt --all -- --check

clippy:
	CLIPPY_CONF_DIR=$(CLIPPY_CONF_DIR) cargo clippy --workspace --all-targets --all-features -- -D warnings

complexity:
	cargo xtask complexity

coverage:
	cargo coverage

test:
	cargo test --workspace --all-features --lib --tests

ocr-review:
	cargo xtask ocr-review $(ARGS)
