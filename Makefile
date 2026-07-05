PYTHON ?= python3

.PHONY: ci ci-lint ci-rust ci-test ci-quality

ci: ci-lint ci-rust ci-test ci-quality

ci-lint:
	$(PYTHON) -m pip install --upgrade pip
	pip install pre-commit
	pip install -e ".[dev]"
	pre-commit run --all-files
	ruff check src tests
	black --check src tests
	mypy src

ci-rust:
	cd diffctx && DIFFCTX_YAML_CASES_LIMIT="20" cargo test --lib
	cd diffctx && cargo build --release
	cd diffctx && DIFFCTX_YAML_CASES_LIMIT="20" cargo test --release --test yaml_cases
	cd diffctx && ./target/release/diffctx-test || true

ci-test:
	$(PYTHON) -m pip install --upgrade pip
	pip install "maturin>=1.10,<1.11"
	PYO3_USE_ABI3_FORWARD_COMPATIBILITY="1" pip install -e ".[dev,full,mcp]" --no-build-isolation
	pytest -v --cov=src/diffctx --cov-report=xml \
		--cov-report=term-missing --cov-branch --junitxml=test-results.xml
	coverage report --fail-under=40

ci-quality:
	$(PYTHON) -m pip install --upgrade pip
	pip install -e ".[dev]"
	radon cc src/diffctx/ --min B --show-complexity --total-average
	radon mi src/diffctx/ --min B --show
	radon cc src/diffctx/ --min C --total-average || \
		(echo "High complexity detected" && exit 1)
	lint-imports
