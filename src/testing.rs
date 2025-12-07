use std::path::PathBuf;

pub fn get_test_file_path<P: AsRef<str>>(name: P) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("data")
        .join(name.as_ref())
}

pub fn get_test_file_contents<P: AsRef<str>>(name: P) -> String {
    let path = get_test_file_path(name);
    std::fs::read_to_string(path).unwrap_or_else(|_| panic!("Failed to read test file"))
}
