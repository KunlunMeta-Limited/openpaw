//! `notebook_edit` e2e tests: contract paths (replace / insert / delete),
//! failure paths (cell index out of range), and workspace-escape rejection.
//! Each test uses its own tempdir for isolation.

use std::path::{Path, PathBuf};

use openpaw_tools::fs::notebook_edit::run;
use serde_json::json;

fn sample_notebook() -> serde_json::Value {
    json!({
        "cells": [
            {"cell_type": "code", "source": ["print('hello')\n"], "metadata": {}, "outputs": [], "execution_count": null},
            {"cell_type": "markdown", "source": "# Title", "metadata": {}}
        ],
        "metadata": {},
        "nbformat": 4,
        "nbformat_minor": 5
    })
}

/// Write a notebook into a fresh tempdir; returns (root, file path).
async fn setup(dir: &Path, name: &str, nb: &serde_json::Value) -> PathBuf {
    let path = dir.join(name);
    tokio::fs::write(&path, serde_json::to_string_pretty(nb).unwrap())
        .await
        .unwrap();
    path
}

async fn read_notebook(path: &Path) -> serde_json::Value {
    let raw = tokio::fs::read_to_string(path).await.unwrap();
    serde_json::from_str(&raw).unwrap()
}

// ── contract paths ───────────────────────────────────────────────────────

#[tokio::test]
async fn replace_overwrites_cell_source() {
    let dir = tempfile::tempdir().unwrap();
    let path = setup(dir.path(), "nb.ipynb", &sample_notebook()).await;

    let out = run(
        dir.path(),
        json!({
            "path": "nb.ipynb",
            "operation": "replace",
            "cell_index": 0,
            "source": "print('updated')"
        }),
    )
    .await
    .unwrap();
    assert!(!out.is_error, "{}", out.content);
    assert!(out.content.contains("cells=2"));

    let nb = read_notebook(&path).await;
    assert_eq!(nb["cells"][0]["source"], "print('updated')");
    // Untouched cells (multi-line array source form) pass through unchanged.
    assert_eq!(nb["cells"][1]["source"], "# Title");
}

#[tokio::test]
async fn insert_at_index_and_append_when_out_of_range() {
    let dir = tempfile::tempdir().unwrap();
    let path = setup(dir.path(), "nb.ipynb", &sample_notebook()).await;

    // Insert in the middle.
    let out = run(
        dir.path(),
        json!({
            "path": "nb.ipynb",
            "operation": "insert",
            "cell_index": 1,
            "cell_type": "markdown",
            "source": "## inserted"
        }),
    )
    .await
    .unwrap();
    assert!(!out.is_error, "{}", out.content);

    let nb = read_notebook(&path).await;
    let cells = nb["cells"].as_array().unwrap();
    assert_eq!(cells.len(), 3);
    assert_eq!(cells[1]["cell_type"], "markdown");
    assert_eq!(cells[1]["source"], "## inserted");
    assert_eq!(cells[2]["source"], "# Title");

    // Out-of-range index appends.
    let out = run(
        dir.path(),
        json!({
            "path": "nb.ipynb",
            "operation": "insert",
            "cell_index": 999,
            "cell_type": "code",
            "source": "print('new')"
        }),
    )
    .await
    .unwrap();
    assert!(!out.is_error, "{}", out.content);

    let nb = read_notebook(&path).await;
    let cells = nb["cells"].as_array().unwrap();
    assert_eq!(cells.len(), 4);
    assert_eq!(cells[3]["source"], "print('new')");
    assert_eq!(cells[3]["cell_type"], "code");
}

#[tokio::test]
async fn delete_removes_cell() {
    let dir = tempfile::tempdir().unwrap();
    let path = setup(dir.path(), "nb.ipynb", &sample_notebook()).await;

    let out = run(
        dir.path(),
        json!({
            "path": "nb.ipynb",
            "operation": "delete",
            "cell_index": 0
        }),
    )
    .await
    .unwrap();
    assert!(!out.is_error, "{}", out.content);

    let nb = read_notebook(&path).await;
    let cells = nb["cells"].as_array().unwrap();
    assert_eq!(cells.len(), 1);
    assert_eq!(cells[0]["source"], "# Title");
}

// ── failure paths ────────────────────────────────────────────────────────

#[tokio::test]
async fn replace_index_out_of_range_is_error_outcome() {
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path(), "nb.ipynb", &sample_notebook()).await;

    let out = run(
        dir.path(),
        json!({
            "path": "nb.ipynb",
            "operation": "replace",
            "cell_index": 99,
            "source": "x"
        }),
    )
    .await
    .unwrap();
    assert!(out.is_error);
    assert!(out.content.contains("out of range"), "got: {}", out.content);
}

#[tokio::test]
async fn delete_index_out_of_range_is_error_outcome() {
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path(), "nb.ipynb", &sample_notebook()).await;

    let out = run(
        dir.path(),
        json!({
            "path": "nb.ipynb",
            "operation": "delete",
            "cell_index": 7
        }),
    )
    .await
    .unwrap();
    assert!(out.is_error);
    assert!(out.content.contains("out of range"), "got: {}", out.content);
}

#[tokio::test]
async fn workspace_escape_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let outside_path = outside.path().join("outside.ipynb");
    tokio::fs::write(
        &outside_path,
        serde_json::to_string(&sample_notebook()).unwrap(),
    )
    .await
    .unwrap();

    let out = run(
        dir.path(),
        json!({
            "path": outside_path.to_string_lossy(),
            "operation": "replace",
            "cell_index": 0,
            "source": "escaped"
        }),
    )
    .await
    .unwrap();
    assert!(out.is_error);
    assert!(
        out.content.contains("escapes workspace"),
        "got: {}",
        out.content
    );

    // Also via `..` traversal.
    let out = run(
        dir.path(),
        json!({
            "path": "../traversed.ipynb",
            "operation": "delete",
            "cell_index": 0
        }),
    )
    .await
    .unwrap();
    assert!(out.is_error);
    assert!(
        out.content.contains("escapes workspace"),
        "got: {}",
        out.content
    );
}

#[tokio::test]
async fn non_ipynb_file_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    tokio::fs::write(dir.path().join("notes.txt"), "nope")
        .await
        .unwrap();

    let out = run(
        dir.path(),
        json!({
            "path": "notes.txt",
            "operation": "delete",
            "cell_index": 0
        }),
    )
    .await
    .unwrap();
    assert!(out.is_error);
    assert!(out.content.contains("not a .ipynb"), "got: {}", out.content);
}

#[tokio::test]
async fn unknown_operation_is_error_outcome() {
    let dir = tempfile::tempdir().unwrap();
    setup(dir.path(), "nb.ipynb", &sample_notebook()).await;

    let out = run(
        dir.path(),
        json!({
            "path": "nb.ipynb",
            "operation": "frobnicate",
            "cell_index": 0
        }),
    )
    .await
    .unwrap();
    assert!(out.is_error);
    assert!(
        out.content.contains("unknown operation"),
        "got: {}",
        out.content
    );
}
