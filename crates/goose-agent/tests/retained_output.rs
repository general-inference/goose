#![cfg(all(feature = "tool-output", not(target_arch = "wasm32")))]

use goose_agent::tool_output::write_large_text_to_file;

#[test]
fn retained_output_survives_the_writing_process() {
    let directory = tempfile::tempdir().unwrap();
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "retained_writer_child", "--ignored"])
        .env("GOOSE_TEST_OUTPUT_DIRECTORY", directory.path())
        .status()
        .unwrap();
    assert!(status.success());
    let paths: Vec<_> = std::fs::read_dir(directory.path()).unwrap().collect();
    assert_eq!(paths.len(), 1);
    let path = paths[0].as_ref().unwrap().path();
    assert_eq!(std::fs::read_to_string(path).unwrap(), "retained output");
}

#[test]
#[ignore = "invoked by retained_output_survives_the_writing_process"]
fn retained_writer_child() {
    let directory = std::env::var_os("GOOSE_TEST_OUTPUT_DIRECTORY").unwrap();
    write_large_text_to_file("retained output", Some(std::path::Path::new(&directory))).unwrap();
}
