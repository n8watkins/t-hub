use super::*;

#[test]
fn search_files_searches_a_real_tree() {
    // Build a tiny fixture and search it end-to-end through dispatch.
    let mut root = std::env::temp_dir();
    root.push(format!("t-hub-control-files-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();
    std::fs::write(root.join("README.md"), "# hi").unwrap();

    let ctx = test_ctx("t");
    let v = dispatch(
        &ctx,
        "search_files",
        &json!({ "root": root.to_string_lossy(), "query": "main", "limit": 5 }),
    )
    .unwrap();
    let hits = v["hits"].as_array().unwrap();
    assert!(
        hits.iter().any(|h| h["relPath"] == "src/main.rs"),
        "expected src/main.rs in {hits:?}"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn open_file_reads_text_contents() {
    let mut root = std::env::temp_dir();
    root.push(format!("t-hub-control-open-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let file = root.join("notes.md");
    std::fs::write(&file, "# Title\n\nbody").unwrap();

    let ctx = test_ctx("t");
    let v = dispatch(
        &ctx,
        "open_file",
        &json!({ "path": file.to_string_lossy() }),
    )
    .unwrap();
    assert_eq!(v["ext"], "md");
    assert!(v["text"].as_str().unwrap().contains("# Title"));

    let _ = std::fs::remove_dir_all(&root);
}
