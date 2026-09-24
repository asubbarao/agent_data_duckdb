.PHONY: clean clean_all test_build_script

PROJ_DIR := $(dir $(abspath $(lastword $(MAKEFILE_LIST))))

EXTENSION_NAME=agent_data

# Unstable API required by duckdb-rs
USE_UNSTABLE_C_API=1

# Target DuckDB version
DEFAULT_TARGET_DUCKDB_VERSION := v1.5.5
TARGET_DUCKDB_VERSION ?= __AGENT_DATA_AUTO__

all: configure debug

# Include makefiles from DuckDB
include extension-ci-tools/makefiles/c_api_extensions/base.Makefile
include extension-ci-tools/makefiles/c_api_extensions/rust.Makefile

# The Rust helper only selects Cargo targets for macOS. Distribution builds for
# the MinGW artifact must emit a GNU DLL rather than the Windows host target.
ifeq ($(DUCKDB_PLATFORM),windows_amd64_mingw)
  TARGET = x86_64-pc-windows-gnu
  TARGET_INFO = --target $(TARGET)
  TARGET_PATH = ./target/$(TARGET)
endif

ifeq ($(TARGET_DUCKDB_VERSION),__AGENT_DATA_AUTO__)
  EFFECTIVE_DUCKDB_GIT_VERSION = $(if $(DUCKDB_GIT_VERSION),$(DUCKDB_GIT_VERSION),$(shell cat configure/duckdb_git_version.txt 2>/dev/null))
  RESOLVE_DUCKDB_METADATA_VERSION = scripts/duckdb_metadata_version.py --duckdb-git-version "$(EFFECTIVE_DUCKDB_GIT_VERSION)" --default "$(DEFAULT_TARGET_DUCKDB_VERSION)"
  override TARGET_DUCKDB_VERSION = $(shell $(PYTHON_VENV_BIN) $(RESOLVE_DUCKDB_METADATA_VERSION) 2>/dev/null || $(PYTHON_BIN) $(RESOLVE_DUCKDB_METADATA_VERSION))
endif
check_target_duckdb_version:
	@test -n "$(TARGET_DUCKDB_VERSION)" || (echo "Could not resolve TARGET_DUCKDB_VERSION" >&2; exit 1)

configure: venv platform extension_version duckdb_git_version

# Artifact identity must follow the current checkout, including incremental builds.
extension_version:
	@$(VERSION_COMMAND)

build_extension_with_metadata_debug build_extension_with_metadata_release: extension_version

.PHONY: duckdb_git_version
duckdb_git_version:
	@mkdir -p configure
	@printf '%s\n' "$(DUCKDB_GIT_VERSION)" > configure/duckdb_git_version.txt

debug: build_extension_library_debug build_extension_with_metadata_debug

release: build_extension_library_release build_extension_with_metadata_release

build_extension_library_debug build_extension_library_release build_extension_with_metadata_debug build_extension_with_metadata_release: check_target_duckdb_version

test: test_debug
test_debug: test_build_script test_extension_debug
test_release: test_build_script test_extension_release

test_build_script:
	@test_dir="$$(mktemp -d)"; \
	trap 'rm -rf "$$test_dir"' EXIT; \
	rustc --edition=2021 --test build.rs -o "$$test_dir/build_script_tests"; \
	"$$test_dir/build_script_tests"

clean: clean_build clean_rust
clean_all: clean_configure clean
