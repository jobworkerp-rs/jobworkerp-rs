#!/bin/bash
# Workflow Schema Update Automation Script
#
# This script automates the process of updating workflow.yaml schema and
# regenerating Rust types with typify.
#
# Usage:
#   ./scripts/update_workflow_schema.sh

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

cd "$PROJECT_ROOT"

echo "=== Workflow Schema Update ==="
echo

# Step 1: Generate JSON schema from YAML
echo "Step 1: Generate JSON schema from YAML..."
yq --output-format=json runner/schema/workflow.yaml > runner/schema/workflow.json
echo "✓ JSON schema generated: runner/schema/workflow.json"
echo

# Step 2: Generate Rust types with typify
echo "Step 2: Generate Rust types with typify..."
cd runner
cargo typify schema/workflow.json
cd ..
echo "✓ Rust types generated: runner/schema/workflow.rs"
echo

# Step 3: Extract header from existing workflow.rs
echo "Step 3: Merge generated code with manual header..."
HEADER_FILE="/tmp/workflow_header_$$.rs"
GENERATED_FILE="runner/schema/workflow.rs"
MERGED_FILE="/tmp/workflow_merged_$$.rs"
TARGET_FILE="app-wrapper/src/workflow/definition/workflow.rs"

# Extract header (first 13 lines with clippy allows and module declarations)
cat > "$HEADER_FILE" << 'EOF'
#![allow(clippy::redundant_closure_call)]
#![allow(clippy::needless_lifetimes)]
#![allow(clippy::match_single_binding)]
#![allow(clippy::clone_on_copy)]
#![allow(irrefutable_let_patterns)]
#![allow(clippy::derivable_impls)]
#![allow(clippy::large_enum_variant)]

pub mod errors;
pub mod supplement;
#[cfg(test)]
pub mod supplement_test;
pub mod tasks;

EOF

# Merge header with generated code
cat "$HEADER_FILE" "$GENERATED_FILE" > "$MERGED_FILE"
echo "✓ Header merged with generated code"
echo

# Step 4: Backup and replace
echo "Step 4: Backup existing workflow.rs and replace..."
if [ -f "$TARGET_FILE" ]; then
    BACKUP_FILE="${TARGET_FILE}.bak.$(date +%Y%m%d_%H%M%S)"
    cp "$TARGET_FILE" "$BACKUP_FILE"
    echo "✓ Backup created: $BACKUP_FILE"
fi

mv "$MERGED_FILE" "$TARGET_FILE"
echo "✓ workflow.rs updated"
echo

# Cleanup temporary files
rm -f "$HEADER_FILE"

# Step 5: Verify build
echo "Step 5: Verify build..."
if cargo check --package app-wrapper --quiet 2>&1 | grep -q "error"; then
    echo "✗ Build check failed. Please review errors above."
    exit 1
else
    echo "✓ Build check passed"
fi
echo

# Step 6: Verify RunScript presence
echo "Step 6: Verify RunScript in generated code..."
if grep -q "pub struct RunScript" "$TARGET_FILE" && \
   grep -q "Script(RunScript)" "$TARGET_FILE"; then
    echo "✓ RunScript struct found"
    echo "✓ RunTaskConfiguration::Script variant found"
else
    echo "✗ RunScript or Script variant not found in generated code"
    exit 1
fi
echo

echo "=== Schema update completed successfully! ==="
echo
echo "Summary:"
echo "  - JSON schema: runner/schema/workflow.json"
echo "  - Generated types: runner/schema/workflow.rs"
echo "  - Final workflow.rs: app-wrapper/src/workflow/definition/workflow.rs"
echo "  - Lines: $(wc -l < "$TARGET_FILE")"
echo

# Display RunTaskConfiguration enum
echo "RunTaskConfiguration variants:"
grep -A 5 "pub enum RunTaskConfiguration" "$TARGET_FILE" || true
